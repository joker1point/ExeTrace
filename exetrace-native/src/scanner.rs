//! 冷启动扫描（对齐 Python 版 scanner.py）。
//!
//! 数据源（全部 HKCU / 用户目录，只读，无需管理员）：
//! 1. Explorer\FeatureUsage\AppLaunch   —— 路径 + 启动次数 + 最后启动时间
//! 2. Explorer\FeatureUsage\AppSwitched —— 用过（显示过窗口）的应用
//! 3. Shell\MuiCache                    —— 值名剥后缀即 exe 路径
//! 4. `%APPDATA%\Microsoft\Windows\Recent\*.lnk` —— 打开过的**本地网页**
//!    （`.html/.htm/.xhtml`，2026-10-17 新增；读 lnk 得完整路径，时间取 lnk 的 mtime）

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::shortcut;
use crate::store::{file_stem, unix_now, Store};
use crate::winx::{self, is_noise_exe};

const FEATURE_USAGE: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FeatureUsage";
const MUI_CACHE: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache";

const MIN_TS: f64 = 946_684_800.0; // 2000-01-01

#[derive(Debug, Default)]
pub struct ScanStats {
    pub feature_usage: usize,
    pub muicache: usize,
    /// 「最近使用的项目」里命中 .html/.htm 的条目数（2026-10-17）。
    pub recent_html: usize,
    pub total: i64,
}

pub fn scan_all(store: &mut Store) -> ScanStats {
    let mut stats = ScanStats::default();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    for sub in ["AppLaunch", "AppSwitched"] {
        let path = format!("{FEATURE_USAGE}\\{sub}");
        let Ok(key) = hkcu.open_subkey(&path) else { continue };
        for item in key.enum_values() {
            let Ok((name, value)) = item else { continue };
            if !name.to_lowercase().ends_with(".exe") {
                continue; // UWP 的 AppUserModelID 形态，跳过
            }
            if !std::path::Path::new(&name).is_file() || is_noise_exe(Some(&name)) {
                continue;
            }
            let (count, last) = parse_feature_usage(&value.bytes);
            if store.upsert_seed(&name, count, last).is_ok() {
                stats.feature_usage += 1;
            }
        }
    }

    if let Ok(key) = hkcu.open_subkey(MUI_CACHE) {
        for item in key.enum_values() {
            let Ok((name, _)) = item else { continue };
            let cand = strip_mui_suffix(&name);
            if cand.to_lowercase().ends_with(".exe")
                && std::path::Path::new(&cand).is_file()
                && !is_noise_exe(Some(&cand))
                && store.upsert_seed(&cand, None, None).is_ok()
            {
                stats.muicache += 1;
            }
        }
    }

    stats.recent_html = scan_recent_html(store);

    stats.total = store.total();
    log::info!(
        "冷启动扫描完成: feature_usage={} muicache={} recent_html={} total={}",
        stats.feature_usage,
        stats.muicache,
        stats.recent_html,
        stats.total
    );
    stats
}

/// 扫 Windows「最近使用的项目」目录，把**浏览器打开过的本地网页**记进库
/// （`source='html'`）。2026-10-17 新增。
///
/// 为什么用 lnk 而不是 RecentDocs 注册表：注册表里只有文件名 + pidl，拿不到完整
/// 路径；而 Recent 目录的 `.lnk` 用现成的 `shortcut::read_shortcut` 就能读出目标。
pub fn scan_recent_html(store: &mut Store) -> usize {
    let dir = winx::recent_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        log::warn!("HTML 历史：读不到 Recent 目录 {dir}");
        return 0;
    };
    let mut n = 0usize;
    for entry in entries.flatten() {
        let p = entry.path();
        if !p
            .extension()
            .map(|e| e.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(info) = shortcut::read_shortcut(&p.to_string_lossy()) else {
            continue;
        };
        if !is_html_path(&info.target) || !std::path::Path::new(&info.target).is_file() {
            continue;
        }
        // 打开时间取 lnk 的 mtime —— Windows 在每次打开文件时会重写这个快捷方式。
        let ts = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .filter(|t| *t > MIN_TS);
        if store
            .upsert_html(&info.target, &file_stem(&info.target), ts)
            .is_ok()
        {
            n += 1;
        }
    }
    if n > 0 {
        store.mark_changed();
    }
    log::info!("HTML 历史扫描: {n} 条（{dir}）");
    n
}

fn is_html_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml")
}

fn parse_feature_usage(bytes: &[u8]) -> (Option<i64>, Option<f64>) {
    let mut count = None;
    let mut last = None;
    if bytes.len() >= 8 {
        let ft = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
        last = filetime_to_unix(ft);
    }
    if bytes.len() >= 12 {
        let c = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));
        count = Some(c as i64);
    }
    (count, last)
}

fn filetime_to_unix(ft: u64) -> Option<f64> {
    if ft == 0 {
        return None;
    }
    let ts = ft as f64 / 10_000_000.0 - 11_644_473_600.0;
    if ts > MIN_TS && ts < unix_now() + 86_400.0 {
        Some(ts)
    } else {
        None
    }
}

fn strip_mui_suffix(name: &str) -> String {
    const SUFFIXES: [&str; 4] = [
        ".FriendlyAppName",
        ".ApplicationCompany",
        ".ApplicationName",
        ".AppUserModelID",
    ];
    let lower = name.to_lowercase();
    for s in SUFFIXES {
        if lower.ends_with(&s.to_lowercase()) {
            return name[..name.len() - s.len()].trim().to_string();
        }
    }
    name.trim().to_string()
}
