//! 运行时路径模型（与 Python 版 exetrace/src/paths.py 对齐）。
//!
//! 可写数据一律落 `%LOCALAPPDATA%\ExeTrace`，与 exe 所在位置解耦；
//! 支持 `EXETRACE_DATA_DIR` 覆盖（验收脚本用它隔离测试数据）。

use std::path::PathBuf;

pub const APP_NAME: &str = "ExeTrace";
pub const APP_TITLE: &str = "ExeTrace — 应用历史定位器";
pub const APP_VERSION: &str = "1.0.0";

pub fn data_dir() -> PathBuf {
    if let Ok(v) = std::env::var("EXETRACE_DATA_DIR") {
        if !v.trim().is_empty() {
            let p = PathBuf::from(v);
            let _ = std::fs::create_dir_all(&p);
            return p;
        }
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        format!("{home}\\AppData\\Local")
    });
    let p = PathBuf::from(base).join(APP_NAME);
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn db_path() -> PathBuf {
    data_dir().join("history.db")
}

pub fn log_path() -> PathBuf {
    data_dir().join("exetrace.log")
}

pub fn selftest_report_path() -> PathBuf {
    data_dir().join("selftest.txt")
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("exetrace.exe"))
}

/// 开机自启注册表里写入的命令行。
pub fn autostart_command() -> String {
    format!("\"{}\" --minimized", exe_path().display())
}
