//! 实时监控线程（对齐 Python 版 v3.1 的判定与计时策略）。
//!
//! - 首轮快照只建基线（此刻在跑的进程不算「被打开」）；
//! - 新进程进入 pending，凭「PENDING_TTL 内成为过前台」或「父进程是 explorer」确认
//!   （旧的「任意可见窗口」判据会把输入法/崩溃报告器类弹窗组件记成用户打开的软件）；
//! - 前台事件钩子（SetWinEventHook）实时驱动 + 轮询现场查询兜底，hook 失效自动退化；
//! - 使用时长：前台切换结算停留段，每 30s 按 exe 聚合落库；
//! - 同 exe 在 DEDUPE_WINDOW 秒内重复出现只刷新时间不涨计数；
//! - `paused` 为真时只维护基线：不记录新进程、不计时。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::fghook::ForegroundEventHook;
use crate::store::{self, Store};
use crate::winx;

pub const PENDING_TTL: Duration = Duration::from_secs(120);
pub const DEDUPE_WINDOW: f64 = 300.0;
pub const INTERVAL: Duration = Duration::from_secs(2);

const FLUSH_USAGE_SECS: f64 = 30.0;
const MAX_SEGMENT_SECS: f64 = 86400.0;
const RECENT_MAX: usize = 64;

struct Pending {
    exe: String,
    deadline: Instant,
    from_explorer: bool,
}

/// 前台时间状态：钩子线程只写 `recent`/归属；轮询线程负责聚合落库。
struct TimeState {
    /// pid → 最近成为前台的 unix 秒（判据用，对齐 Python 的 `_fg_recent`）。
    recent: HashMap<u32, f64>,
    fg_pid: Option<u32>,
    fg_since: f64,
    /// pid → 尚未落库的停留秒数。
    accum: HashMap<u32, f64>,
    /// pid → exe 路径缓存（落库换算用）。
    pid_exe: HashMap<u32, String>,
}

impl TimeState {
    fn new(now: f64) -> Self {
        Self {
            recent: HashMap::new(),
            fg_pid: None,
            fg_since: now,
            accum: HashMap::new(),
            pid_exe: HashMap::new(),
        }
    }

    /// 结算上一段停留并切到新的前台。
    fn switch(&mut self, pid: u32, now: f64) {
        self.recent.insert(pid, now);
        if self.fg_pid != Some(pid) {
            if let Some(prev) = self.fg_pid {
                let delta = now - self.fg_since;
                if delta > 0.0 && delta < MAX_SEGMENT_SECS {
                    *self.accum.entry(prev).or_insert(0.0) += delta;
                }
            }
            self.fg_pid = Some(pid);
            self.fg_since = now;
        }
        if self.recent.len() > RECENT_MAX {
            let deadline = now - PENDING_TTL.as_secs_f64();
            self.recent.retain(|_, t| *t >= deadline);
        }
    }

    /// 把 accum（含当前段）按 exe 聚合并取走。
    fn take_usage(&mut self, now: f64) -> HashMap<String, f64> {
        if let Some(cur) = self.fg_pid {
            let delta = now - self.fg_since;
            if delta > 0.0 && delta < MAX_SEGMENT_SECS {
                *self.accum.entry(cur).or_insert(0.0) += delta;
                self.fg_since = now; // 已计入：重置起点防重复
            }
        }
        let mut by_path: HashMap<String, f64> = HashMap::new();
        for (pid, secs) in self.accum.drain() {
            let exe = self
                .pid_exe
                .entry(pid)
                .or_insert_with(|| winx::process_exe_path(pid).unwrap_or_default())
                .clone();
            if !exe.is_empty() {
                *by_path.entry(exe).or_insert(0.0) += secs;
            }
        }
        if self.pid_exe.len() > 512 {
            self.pid_exe.clear();
        }
        by_path
    }
}

pub struct Watcher {
    stop: Arc<AtomicBool>,
    recorded: Arc<AtomicU64>,
    last_error: Arc<Mutex<String>>,
}

impl Watcher {
    pub fn spawn<F>(store: Arc<Mutex<Store>>, paused: Arc<AtomicBool>, on_record: F) -> Self
    where
        F: Fn(&str, bool) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = Arc::new(AtomicU64::new(0));
        let last_error = Arc::new(Mutex::new(String::new()));

        let (stop_c, rec_c, err_c) = (stop.clone(), recorded.clone(), last_error.clone());
        std::thread::Builder::new()
            .name("ExeTrace-Watcher".into())
            .spawn(move || {
                run_loop(store, paused, stop_c, rec_c, err_c, on_record);
            })
            .expect("无法启动监控线程");

        Self { stop, recorded, last_error }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn recorded_count(&self) -> u64 {
        self.recorded.load(Ordering::Relaxed)
    }

    pub fn last_error(&self) -> String {
        self.last_error.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

fn run_loop<F>(
    store: Arc<Mutex<Store>>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    recorded: Arc<AtomicU64>,
    last_error: Arc<Mutex<String>>,
    on_record: F,
) where
    F: Fn(&str, bool),
{
    let mut known: HashSet<u32> = HashSet::new();
    let mut pending: HashMap<u32, Pending> = HashMap::new();
    let mut primed = false;
    let mut tick_no = 0u64;

    let time_state = Arc::new(Mutex::new(TimeState::new(store::unix_now())));
    let mut last_flush = store::unix_now();

    // 前台事件钩子：回调只更新内存状态（该线程必须持续泵消息）
    let hook = {
        let ts = time_state.clone();
        ForegroundEventHook::start(move |pid| {
            let now = store::unix_now();
            if let Ok(mut g) = ts.lock() {
                g.switch(pid, now);
            }
        })
    };
    if hook.available() {
        log::info!("前台事件 hook 已启用（+{}s 轮询兜底）", INTERVAL.as_secs());
    } else {
        log::warn!("前台事件 hook 不可用（{}），退化纯轮询", hook.last_error());
    }

    while !stop.load(Ordering::SeqCst) {
        crate::config::reload_if_changed(); // 配置热重载（mtime 变了才重读）
        let procs = winx::snapshot_processes();
        let now = Instant::now();
        let now_ts = store::unix_now();
        let is_paused = paused.load(Ordering::Relaxed);

        if !primed {
            known = procs.iter().map(|p| p.pid).collect();
            primed = true;
            log::info!("监控基线建立：{} 个进程", known.len());
        } else if is_paused {
            // 暂停：只维护基线，不登记新进程、不计时
            known = procs.iter().map(|p| p.pid).collect();
            pending.clear();
            if let Ok(mut g) = time_state.lock() {
                g.fg_pid = None; // 暂停期不计入任何应用
            }
        } else {
            let mut name_by_pid: HashMap<u32, String> = HashMap::new();
            for p in &procs {
                name_by_pid.insert(p.pid, p.name.to_lowercase());
            }

            let mut fresh = 0usize;
            let mut resolved = 0usize;
            for p in &procs {
                if known.contains(&p.pid) {
                    continue;
                }
                fresh += 1;
                let Some(exe) = winx::process_exe_path(p.pid) else { continue };
                resolved += 1;
                if winx::is_noise_exe(Some(&exe)) || crate::config::should_ignore(&exe) {
                    continue;
                }
                let from_explorer = name_by_pid
                    .get(&p.ppid)
                    .map(|n| n == "explorer.exe")
                    .unwrap_or(false);
                pending.insert(
                    p.pid,
                    Pending { exe, deadline: now + PENDING_TTL, from_explorer },
                );
            }

            // 轮询兜底：hook 失效/漏事件时现场补记前台
            if let Some(fg) = winx::foreground_pid() {
                if let Ok(mut g) = time_state.lock() {
                    g.switch(fg, now_ts);
                }
            }

            // 判据复核（对齐 v3.1）：PENDING_TTL 内成为过前台 or 由 explorer 启动
            let alive: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
            let mut confirmed: Vec<(u32, String)> = Vec::new();
            {
                let g = match time_state.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        std::thread::sleep(INTERVAL);
                        continue;
                    }
                };
                let cutoff = now_ts - PENDING_TTL.as_secs_f64();
                for (pid, item) in pending.iter() {
                    let was_fg = g.recent.get(pid).map(|t| *t >= cutoff).unwrap_or(false);
                    if was_fg || item.from_explorer {
                        confirmed.push((*pid, item.exe.clone()));
                    } else if !alive.contains(pid) || now > item.deadline {
                        confirmed.push((*pid, String::new())); // 空串 = 静默丢弃
                    }
                }
            }
            let confirmed_len = confirmed.len();
            for (pid, exe) in confirmed {
                pending.remove(&pid);
                if exe.is_empty() {
                    continue;
                }
                let counted = match store.lock() {
                    Ok(mut st) => st.record_launch(&exe, DEDUPE_WINDOW).unwrap_or(false),
                    Err(e) => {
                        if let Ok(mut le) = last_error.lock() {
                            *le = format!("store lock poisoned: {e}");
                        }
                        continue;
                    }
                };
                recorded.fetch_add(1, Ordering::Relaxed);
                log::info!("记录打开: {exe}{}", if counted { "" } else { " (窗口内去重)" });
                on_record(&exe, counted);
            }

            tick_no += 1;
            log::info!(
                "tick #{tick_no}: procs={} fresh={fresh} resolved={resolved} pending={} confirmed={confirmed_len} hook={}",
                procs.len(),
                pending.len(),
                hook.event_count()
            );

            known = alive;
        }

        // ---- 使用时长：每 30s 按 exe 聚合落库 ----
        if !is_paused && now_ts - last_flush >= FLUSH_USAGE_SECS {
            let by_path = match time_state.lock() {
                Ok(mut g) => g.take_usage(now_ts),
                Err(_) => HashMap::new(),
            };
            if !by_path.is_empty() {
                if let Ok(mut st) = store.lock() {
                    st.add_usage(&by_path);
                }
                log::info!("使用时长落库: {} 个应用", by_path.len());
            }
            last_flush = now_ts;
        }

        std::thread::sleep(INTERVAL);
    }

    hook.stop();
    log::info!("监控线程退出");
}
