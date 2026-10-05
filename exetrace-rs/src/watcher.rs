//! 实时监控线程（对齐 Python 版 watcher.py 的判定策略）。
//!
//! - 首轮快照只建基线（此刻在跑的进程不算"被打开"）；
//! - 新进程进入 pending，凭「出现可见窗口」或「父进程是 explorer」确认；
//! - PENDING_TTL 内未满足则丢弃（后台服务/更新器）；
//! - 同 exe 在 DEDUPE_WINDOW 秒内重复出现只刷新时间不涨计数。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::store::Store;
use crate::winx;

pub const PENDING_TTL: Duration = Duration::from_secs(120);
pub const DEDUPE_WINDOW: f64 = 300.0;
pub const INTERVAL: Duration = Duration::from_secs(2);

struct Pending {
    exe: String,
    deadline: Instant,
    from_explorer: bool,
}

pub struct Watcher {
    stop: Arc<AtomicBool>,
    recorded: Arc<AtomicU64>,
    last_error: Arc<Mutex<String>>,
}

impl Watcher {
    pub fn spawn<F>(store: Arc<Mutex<Store>>, on_record: F) -> Self
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
                run_loop(store, stop_c, rec_c, err_c, on_record);
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

    while !stop.load(Ordering::SeqCst) {
        let procs = winx::snapshot_processes();
        let now = Instant::now();

        if !primed {
            known = procs.iter().map(|p| p.pid).collect();
            primed = true;
            log::info!("监控基线建立：{} 个进程", known.len());
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
                if winx::is_noise_exe(Some(&exe)) {
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

            let visible = winx::visible_window_pids();
            let alive: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
            let mut confirmed: Vec<(u32, String)> = Vec::new();
            for (pid, item) in pending.iter() {
                if visible.contains(pid) || item.from_explorer {
                    confirmed.push((*pid, item.exe.clone()));
                } else if !alive.contains(pid) || now > item.deadline {
                    confirmed.push((*pid, String::new())); // 空串 = 丢弃
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
                "tick #{tick_no}: procs={} fresh={fresh} resolved={resolved} pending={} confirmed={confirmed_len}",
                procs.len(),
                pending.len()
            );

            known = alive;
        }

        std::thread::sleep(crate::watcher::INTERVAL);
    }
    log::info!("监控线程退出");
}
