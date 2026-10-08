//! ExeTrace（Rust + Win32 原生控件版）入口。
//!
//! 用法：
//!   exetrace.exe              正常启动（GUI + 托盘 + 后台记录）
//!   exetrace.exe --minimized  启动后直接缩到托盘
//!   exetrace.exe --selftest   非 GUI 自检（报告落盘）

// 正式产物（release）不弹控制台窗口；debug 保留控制台便于排查。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod config;
mod fghook;
mod paths;
mod resolver;
mod scanner;
mod selftest;
mod shortcut;
mod store;
mod watcher;
mod winx;

use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let minimized = args.iter().any(|a| a == "--minimized");
    let no_tray = args.iter().any(|a| a == "--no-tray");

    if args.iter().any(|a| a == "--selftest") {
        return if selftest::run() { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }

    // CLI：直接创建桌面快捷方式（脚本/批量用；退出码 0 = 已创建且读回校验通过）
    if let Some(i) = args.iter().position(|a| a == "--make-shortcut") {
        let Some(path) = args.get(i + 1) else {
            log::error!("用法: ExeTrace.exe --make-shortcut <exe 或 文件夹> [--name 名称]");
            return ExitCode::from(2);
        };
        let name = args
            .iter()
            .position(|a| a == "--name")
            .and_then(|j| args.get(j + 1))
            .cloned()
            .unwrap_or_default();
        return cli_make_shortcut(path, &name);
    }

    init_logging();
    log::info!("启动 {} {} (pid={})", paths::APP_NAME, paths::APP_VERSION, std::process::id());

    if winx::acquire_single_instance().is_none() {
        // 已有实例：直接把它的窗口呼出来；连窗口都找不到才退回提示。
        if !winx::activate_existing_window() {
            winx::message_box(
                "ExeTrace 已经在运行了（图标在系统托盘的「隐藏的图标」里，可拖出来固定显示）。",
            );
        }
        return ExitCode::SUCCESS;
    }

    let store = match store::Store::open(&paths::db_path()) {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(e) => {
            winx::message_box(&format!("无法打开历史库：\n{e}"));
            return ExitCode::FAILURE;
        }
    };

    // 冷启动扫描（后台线程）
    {
        let st = store.clone();
        std::thread::spawn(move || {
            if let Ok(mut s) = st.lock() {
                scanner::scan_all(&mut s);
            }
        });
    }

    // 实时监控（把"刚刚记录"的事件投给 UI 做提示）
    let last_event: native_ui::LastEvent = Arc::new(Mutex::new(None));
    let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let le = last_event.clone();
    let watcher = watcher::Watcher::spawn(store.clone(), paused.clone(), move |exe, counted| {
        if counted {
            if let Ok(mut slot) = le.lock() {
                *slot = Some((exe.to_string(), counted));
            }
        }
    });

    let code = match native_ui::run(store.clone(), last_event, minimized, !no_tray, paused.clone()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("GUI 退出异常: {e}");
            ExitCode::FAILURE
        }
    };
    watcher.stop();
    log::info!("已退出");
    code
}

/// CLI：创建桌面快捷方式并读回校验。退出码：0 成功 / 3 解析失败 / 4 创建失败 / 5 校验不一致。
fn cli_make_shortcut(raw_path: &str, name: &str) -> ExitCode {
    use std::io::Write;
    init_logging();
    let res = resolver::resolve(raw_path);
    if !res.ok || res.target.is_none() {
        log::error!("CLI 解析失败: {}", res.message);
        let _ = writeln!(std::io::stdout(), "error: {}", res.message);
        return ExitCode::from(3);
    }
    let target = res.target.clone().unwrap_or_default();
    let lnk_name = if name.is_empty() {
        if res.default_name.is_empty() {
            store::file_stem(&target)
        } else {
            res.default_name.clone()
        }
    } else {
        name.to_string()
    };
    match shortcut::pin_to_desktop(&target, &lnk_name) {
        Ok((lnk, verified)) => {
            log::info!("CLI 创建: {lnk} -> {target} (verified={verified})");
            let _ = writeln!(std::io::stdout(), "created: {lnk}");
            if verified {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(5)
            }
        }
        Err(e) => {
            log::error!("CLI 创建失败: {e}");
            let _ = writeln!(std::io::stdout(), "error: {e}");
            ExitCode::from(4)
        }
    }
}

fn init_logging() {
    // 简易文件日志（windowed 产物没有控制台，日志是唯一的排查手段）
    let path = paths::log_path();
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let logger = SimpleLogger { file: Mutex::new(file) };
        let _ = log::set_boxed_logger(Box::new(logger));
        log::set_max_level(log::LevelFilter::Info);
    }
}

struct SimpleLogger {
    file: Mutex<std::fs::File>,
}

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("{} {:5} {}: {}\n", now, record.level(), record.target(), record.args());
        if let Ok(mut f) = self.file.lock() {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

mod native_ui;
