//! 冷启动扫描（对齐 Python 版 scanner.py）。
//!
//! 数据源（全部 HKCU，只读，无需管理员）：
//! 1. Explorer\FeatureUsage\AppLaunch   —— 路径 + 启动次数 + 最后启动时间
//! 2. Explorer\FeatureUsage\AppSwitched —— 用过（显示过窗口）的应用
//! 3. Shell\MuiCache                    —— 值名剥后缀即 exe 路径

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::store::{unix_now, Store};
use crate::winx::is_noise_exe;

const FEATURE_USAGE: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FeatureUsage";
const MUI_CACHE: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache";

const MIN_TS: f64 = 946_684_800.0; // 2000-01-01

#[derive(Debug, Default)]
pub struct ScanStats {
    pub feature_usage: usize,
    pub muicache: usize,
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

    stats.total = store.total();
    log::info!(
        "冷启动扫描完成: feature_usage={} muicache={} total={}",
        stats.feature_usage,
        stats.muicache,
        stats.total
    );
    stats
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
