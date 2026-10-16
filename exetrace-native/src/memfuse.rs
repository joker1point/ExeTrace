//! memfuse 联动：把这个应用加进 memfuse 的 `protect-list.txt`（白名单）。
//!
//! 动机（2026-10-16 用户口径）：内存紧张时有些应用**一开就被 memfuse 杀掉**，
//! 根本来不及打开白名单界面 —— 而 ExeTrace 的历史列表里它一直都在，所以在
//! 这里右键「加入 memfuse 白名单」是最快的路径。
//!
//! 脚本位置解析顺序：环境变量 `MEMFUSE_SCRIPT` → config.json 的 `memfuse_script`
//! → 两个常见位置（作者本机布局 `Documents\trae_projects\memfuse\`，以及
//! `Documents\memfuse\`）。
//!
//! 写名单一律走 memfuse 自己的 `-AddProtect`（幂等 / 去重 / 保留注释 / 先写
//! `.bak`）：那套逻辑在 memfuse 里，重复实现迟早跑偏。这里只负责"找到脚本、
//! 把进程名递过去"。

use std::path::{Path, PathBuf};

/// 进程名归一：memfuse 的白名单按「进程名小写、去 .exe」匹配。
pub fn proc_key(name: &str) -> String {
    let t = name.trim().to_lowercase();
    match t.strip_suffix(".exe") {
        Some(s) => s.to_string(),
        None => t,
    }
}

/// 从「名字或路径」里取出要写进白名单的进程名。
///
/// `C:\Program Files\Tencent\WeMeet\XnXExternal.exe` → `XnXExternal`；
/// `XnXExternal` → `XnXExternal`；`XnXExternal.exe` → `XnXExternal`。
pub fn name_from(value: &str) -> String {
    let raw = value.trim();
    if raw.is_empty() {
        return String::new();
    }
    let base = if raw.contains('\\') || raw.contains('/') {
        crate::store::file_stem(raw)
    } else {
        raw.to_string()
    };
    if base.to_lowercase().ends_with(".exe") {
        base[..base.len() - 4].to_string()
    } else {
        base
    }
}

/// 解析 memfuse 主脚本路径（找不到时 `None`；调用方可据此给出设置指引）。
pub fn resolve_script() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("MEMFUSE_SCRIPT") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    let configured = crate::config::current().memfuse_script;
    if !configured.trim().is_empty() {
        let p = PathBuf::from(configured.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    let candidates = [
        home.join("Documents")
            .join("trae_projects")
            .join("memfuse")
            .join("memory-guard.ps1"),
        home.join("Documents").join("memfuse").join("memory-guard.ps1"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// 脚本旁边的 `protect-list.txt`（memfuse 的默认约定：与脚本同目录）。
pub fn protect_file_for(script: &Path) -> PathBuf {
    script
        .parent()
        .map(|d| d.join("protect-list.txt"))
        .unwrap_or_else(|| PathBuf::from("protect-list.txt"))
}

/// 这个进程名是否已经在名单里（只读文件，不写）。
pub fn is_listed(script: &Path, name: &str) -> bool {
    let needle = proc_key(name);
    if needle.is_empty() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(protect_file_for(script)) else {
        return false;
    };
    text.lines().any(|line| {
        let entry = line.split('#').next().unwrap_or("").trim();
        !entry.is_empty() && proc_key(entry) == needle
    })
}

/// 调 `memory-guard.ps1 -AddProtect <name>`（无窗口；拿退出码 + 输出）。
///
/// 返回 `(成功, 输出全文)`。输出只用于失败排查：PowerShell 5.1 往管道写中文可能
/// 是 GBK，失败详情里出现乱码不影响判定 —— 判定只看退出码。
pub fn add_to_protect_list(script: &Path, name: &str) -> (bool, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"])
        .arg("-File")
        .arg(script)
        .arg("-AddProtect")
        .arg(name)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(o) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            (o.status.success(), text.trim().to_string())
        }
        Err(e) => (false, format!("无法启动 PowerShell（{e}）")),
    }
}
