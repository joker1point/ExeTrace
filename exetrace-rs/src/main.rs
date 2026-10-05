//! ExeTrace（Rust/egui 版）入口。
//!
//! 用法：
//!   exetrace.exe              正常启动（GUI + 托盘 + 后台记录）
//!   exetrace.exe --minimized  启动后直接缩到托盘
//!   exetrace.exe --selftest   非 GUI 自检（报告落盘）

mod appicon;
mod paths;
mod scanner;
mod selftest;
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

    init_logging();
    log::info!("启动 {} {} (pid={})", paths::APP_NAME, paths::APP_VERSION, std::process::id());

    if winx::acquire_single_instance().is_none() {
        winx::message_box("ExeTrace 已经在运行了。\n请在系统托盘里查看它。");
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
    let last_event: ui::LastEvent = Arc::new(Mutex::new(None));
    let le = last_event.clone();
    let watcher = watcher::Watcher::spawn(store.clone(), move |exe, counted| {
        if counted {
            if let Ok(mut slot) = le.lock() {
                *slot = Some((exe.to_string(), counted));
            }
        }
    });

    let code = match ui::run(store.clone(), last_event, minimized, !no_tray) {
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

mod ui;
