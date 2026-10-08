//! 前台窗口事件钩子（`SetWinEventHook` + `EVENT_SYSTEM_FOREGROUND`）。
//!
//! 对齐 Python 版 `winutil.ForegroundEventHook`：
//! - 独立线程装钩子并跑消息泵（`WINEVENT_OUTOFCONTEXT` 的事件投递到该线程队列）；
//! - 回调只做「取 pid + 0.5s 去重 + 投递」，**绝不做重活**（该线程必须持续泵消息）；
//! - 退出用 `PostThreadMessageW(WM_QUIT)` 打断消息泵，再 `UnhookWinEvent`；
//! - 不可用时上层自动退化为纯轮询。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{
    SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK, WINEVENTPROC,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetWindowThreadProcessId, PostThreadMessageW, TranslateMessage,
    EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT, WM_QUIT,
};

/// 同 pid 事件去重窗口（秒）：前台快速反复切换时避免重复结算。
const DEDUPE_SECS: f64 = 0.5;

/// 回调与钩子线程间共享的上下文（应用内单例，用全局静态承载）。
struct HookCtx {
    on_foreground: Box<dyn Fn(u32) + Send>,
    last_pid: u32,
    last_at: f64,
    count: Arc<AtomicU64>,
}

static HOOK_CTX: Mutex<Option<HookCtx>> = Mutex::new(None);

pub struct ForegroundEventHook {
    thread_id: u32,
    available: bool,
    last_error: String,
    count: Arc<AtomicU64>,
}

impl ForegroundEventHook {
    /// 启动钩子线程；`on_foreground` 在**钩子线程**被调用（只做内存更新）。
    pub fn start<F>(on_foreground: F) -> Self
    where
        F: Fn(u32) + Send + 'static,
    {
        let count = Arc::new(AtomicU64::new(0));
        match HOOK_CTX.lock() {
            Ok(mut guard) => {
                *guard = Some(HookCtx {
                    on_foreground: Box::new(on_foreground),
                    last_pid: 0,
                    last_at: 0.0,
                    count: count.clone(),
                });
            }
            Err(_) => {
                return Self {
                    thread_id: 0,
                    available: false,
                    last_error: "hook 上下文锁中毒".into(),
                    count,
                };
            }
        }

        let (ready_tx, ready_rx) = mpsc::sync_channel::<(bool, u32, String)>(1);
        let spawned = std::thread::Builder::new()
            .name("ExeTrace-FgHook".into())
            .spawn(move || unsafe {
                let tid = GetCurrentThreadId();
                let hook = SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    std::ptr::null_mut(),
                    Some(hook_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                );
                let ok = !hook.is_null();
                let err = if ok {
                    String::new()
                } else {
                    format!("SetWinEventHook 失败 gle={}", GetLastError())
                };
                let _ = ready_tx.send((ok, tid, err));
                if ok {
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    UnhookWinEvent(hook);
                }
            })
            .is_ok();

        if !spawned {
            return Self {
                thread_id: 0,
                available: false,
                last_error: "钩子线程创建失败".into(),
                count,
            };
        }

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok((ok, tid, err)) => Self {
                thread_id: tid,
                available: ok,
                last_error: err,
                count,
            },
            Err(_) => Self {
                thread_id: 0,
                available: false,
                last_error: "钩子线程就绪超时".into(),
                count,
            },
        }
    }

    pub fn available(&self) -> bool {
        self.available
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    pub fn event_count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// 退出：向钩子线程投 `WM_QUIT` 打断消息泵。
    pub fn stop(&self) {
        if self.thread_id != 0 {
            unsafe {
                PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
            }
        }
    }
}

unsafe extern "system" fn hook_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_null() {
        return;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == 0 {
        return;
    }
    let Ok(mut guard) = HOOK_CTX.lock() else { return };
    let Some(ctx) = guard.as_mut() else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    if ctx.last_pid == pid && now - ctx.last_at < DEDUPE_SECS {
        return;
    }
    ctx.last_pid = pid;
    ctx.last_at = now;
    ctx.count.fetch_add(1, Ordering::Relaxed);
    (ctx.on_foreground)(pid);
}
