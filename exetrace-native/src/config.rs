//! `config.json`（对齐 Python 版 `winutil`）：忽略列表 + 分类覆盖。
//!
//! 位置：`%LOCALAPPDATA%\ExeTrace\config.json`（可用 `EXETRACE_DATA_DIR` 覆盖）。
//!
//! ```json
//! {
//!   "ignore": ["spam-tool.exe"],
//!   "categories": { "code.exe": "开发" }
//! }
//! ```
//!
//! - `ignore`：小写关键词；目标路径包含它、或文件名完全等于它 → 不记录；
//! - `categories`：exe 文件名（小写）→ 分类名，覆盖内置分类规则；
//! - 热重载：watcher 每轮 tick 调一次 `reload_if_changed()`（开销 = 一次 stat）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::SystemTime;

use crate::paths;

#[derive(Default, Clone)]
pub struct Config {
    pub ignore: Vec<String>,
    pub categories: HashMap<String, String>,
    pub mtime: Option<SystemTime>,
}

static CURRENT: RwLock<Option<Config>> = RwLock::new(None);

pub fn config_path() -> PathBuf {
    paths::data_dir().join("config.json")
}

fn parse(text: &str) -> Config {
    // 记事本 / PowerShell 保存的 UTF-8 可能带 BOM —— 先剥掉再解析
    // （PS 5.1 的 `Set-Content -Encoding UTF8` 一定会写 BOM）。
    let text = text.trim_start_matches('\u{feff}');
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Config::default();
    };
    let mut cfg = Config::default();
    if let Some(arr) = value.get("ignore").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                let t = s.trim().to_lowercase();
                if !t.is_empty() {
                    cfg.ignore.push(t);
                }
            }
        }
    }
    if let Some(map) = value.get("categories").and_then(|x| x.as_object()) {
        for (key, val) in map {
            if let Some(s) = val.as_str() {
                let name = s.trim();
                if !name.is_empty() {
                    cfg.categories.insert(key.trim().to_lowercase(), name.to_string());
                }
            }
        }
    }
    cfg
}

/// 强制重读磁盘配置并更新全局缓存。
pub fn load_force() -> Config {
    let path = config_path();
    let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(_) => Config::default(),
    };
    cfg.mtime = mtime;
    if let Ok(mut guard) = CURRENT.write() {
        *guard = Some(cfg.clone());
    }
    cfg
}

/// 取当前配置（首次调用自动加载）。
pub fn current() -> Config {
    if let Ok(guard) = CURRENT.read() {
        if let Some(c) = guard.as_ref() {
            return c.clone();
        }
    }
    load_force()
}

/// mtime 变化时才重载（watcher 每轮调用）。
pub fn reload_if_changed() {
    let path = config_path();
    let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
    let same = {
        match CURRENT.read() {
            Ok(guard) => guard.as_ref().map(|c| c.mtime) == Some(mtime) && guard.is_some(),
            Err(_) => false,
        }
    };
    if !same {
        load_force();
    }
}

/// 是否在忽略列表里（路径子串 or 文件名完全相等）。
pub fn should_ignore(exe_path: &str) -> bool {
    let cfg = current();
    if cfg.ignore.is_empty() {
        return false;
    }
    let hay = exe_path.to_lowercase();
    let base = std::path::Path::new(exe_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    cfg.ignore
        .iter()
        .any(|k| hay.contains(k.as_str()) || base == *k)
}

/// 分类覆盖（按 exe 文件名小写精确匹配）。
pub fn category_override(exe_path: &str) -> Option<String> {
    let cfg = current();
    if cfg.categories.is_empty() {
        return None;
    }
    let base = std::path::Path::new(exe_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    cfg.categories.get(&base).cloned()
}
