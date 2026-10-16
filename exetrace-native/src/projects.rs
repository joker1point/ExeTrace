//! 本机「项目」扫描（2026-10-15）：把散落各处的 git 仓库变成列表里可管理的一等条目。
//!
//! - 根目录：`config.json` 的 `project_roots`（空 → 默认 `~/Documents` `~/WorkBuddy` `~/Desktop`）
//! - 判定：目录下含 `.git` → 项目；读 `.git/config` 的 remote 分档（own / clone / local）
//! - **凭据纪律**：扫描与入库都不保存 remote URL（可能内嵌 token）；「打开 GitHub 页」在动作
//!   时现读并剥掉 URL 里的 userinfo（`://user:token@` → `://`）。
//! - 时间：max(目录 mtime, `.git\index` mtime, `.git\HEAD` mtime) ≈「最近动过」。

use std::path::Path;

use crate::store::Store;

/// 扫描深度上限（相对每个根目录）。
pub const MAX_DEPTH: usize = 4;

/// 递归时跳过的目录名（重 / 无仓库价值）。
const SKIP_DIRS: &[&str] = &[
    "node_modules", ".venv", "venv", "__pycache__", "target", ".next", "dist", ".cache", ".idea",
    ".vscode", "obj", "bin",
];

#[derive(Debug, Default)]
pub struct ProjectStats {
    pub found: usize,
    pub own: usize,
    pub clone: usize,
    pub local: usize,
}

pub fn scan_all(store: &mut Store) -> ProjectStats {
    let mut stats = ProjectStats::default();
    for root in scan_roots() {
        let p = Path::new(&root);
        if p.is_dir() {
            walk(p, 0, store, &mut stats);
        }
    }
    if stats.found > 0 {
        store.mark_changed(); // 一次扫描只 bump 一次：批次结尾统一触发 UI 重建
    }
    log::info!(
        "项目扫描完成: found={} own={} clone={} local={}",
        stats.found,
        stats.own,
        stats.clone,
        stats.local
    );
    stats
}

/// 根目录列表：config 有就用 config，否则给内置默认（本机实测 58 个仓库的聚集地）。
pub fn scan_roots() -> Vec<String> {
    let cfg = crate::config::current();
    if !cfg.project_roots.is_empty() {
        return cfg.project_roots;
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    if home.is_empty() {
        return Vec::new();
    }
    ["Documents", "WorkBuddy", "Desktop"]
        .iter()
        .map(|s| format!("{home}\\{s}"))
        .collect()
}

fn walk(dir: &Path, depth: usize, store: &mut Store, stats: &mut ProjectStats) {
    if depth > MAX_DEPTH {
        return;
    }
    if dir.join(".git").is_dir() {
        if let Some((name, origin, last)) = describe(dir) {
            let path = dir.to_string_lossy().to_string();
            if store.upsert_project(&path, &name, origin, last).is_ok() {
                stats.found += 1;
                match origin {
                    "own" => stats.own += 1,
                    "clone" => stats.clone += 1,
                    _ => stats.local += 1,
                }
            }
        }
        // 找到仓库后继续向下递归：本机存在「仓库里套仓库」（vendored / 子项目），都是有价值的入口。
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(n) = p.file_name().and_then(|s| s.to_str()) else { continue };
        if n == ".git" || SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(n)) {
            continue;
        }
        walk(&p, depth + 1, store, stats);
    }
}

fn describe(dir: &Path) -> Option<(String, &'static str, Option<f64>)> {
    let name = dir.file_name()?.to_string_lossy().to_string();
    if name.is_empty() || name == "." {
        return None;
    }
    let origin = classify(&remote_url_of(dir).unwrap_or_default());
    Some((name, origin, latest_mtime(dir)))
}

/// 分档：自有（joker1point / 自建服务器）→ own；其他有 remote → clone；无 remote → local。
fn classify(url: &str) -> &'static str {
    let low = url.to_lowercase();
    if low.is_empty() {
        return "local";
    }
    if (low.contains("github.com") || low.contains("ssh.github.com")) && low.contains("joker1point") {
        return "own";
    }
    if low.contains("jdcloud") {
        return "own"; // 自建服务器（ssh://jdcloud/...）
    }
    "clone"
}

/// 读 `.git/config` 的 remote URL（优先 origin）。原始串 —— 可能带凭据，**不要**直接入库 / 展示。
fn remote_url_of(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".git").join("config")).ok()?;
    let mut current: Option<String> = None;
    let mut origin: Option<String> = None;
    let mut first: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[remote ") {
            current = t.split('"').nth(1).map(|s| s.to_string());
            continue;
        }
        if let Some(u) = t.strip_prefix("url") {
            let u = u.trim_start().trim_start_matches('=').trim();
            if u.is_empty() {
                continue;
            }
            if first.is_none() {
                first = Some(u.to_string());
            }
            if current.as_deref() == Some("origin") {
                origin = Some(u.to_string());
            }
        }
    }
    origin.or(first)
}

/// 「打开 GitHub 页」用：返回剥掉凭据、可浏览的 https URL（仅 github / gitee）。
pub fn browse_url(repo_dir: &str) -> Option<String> {
    let raw = remote_url_of(Path::new(repo_dir))?;
    let mut u = raw.trim().to_string();
    // 剥 userinfo：scheme://user[:token]@host → scheme://host
    if let Some(pos) = u.find("://") {
        let (scheme, rest) = u.split_at(pos + 3);
        if let Some(at) = rest.find('@') {
            let slash = rest.find('/').unwrap_or(rest.len());
            if at < slash {
                u = format!("{scheme}{}", &rest[at + 1..]);
            }
        }
    }
    // ssh 形态 → https
    let low = u.to_lowercase();
    if low.starts_with("git@") {
        // git@github.com:owner/repo(.git)
        if let Some(rest) = u.split(':').nth(1) {
            u = format!("https://github.com/{}", rest.trim_start_matches('/'));
        }
    } else if low.starts_with("ssh://") {
        // ssh://git@ssh.github.com:443/owner/repo(.git)
        if let Some(idx) = u.find("ssh.github.com") {
            let tail = &u[idx + "ssh.github.com".len()..];
            let path = tail.splitn(2, '/').nth(1).unwrap_or("");
            if !path.is_empty() {
                u = format!("https://github.com/{path}");
            }
        }
    }
    let low = u.to_lowercase();
    if low.starts_with("https://") && (low.contains("github.com") || low.contains("gitee.com")) {
        Some(u)
    } else {
        None
    }
}

/// 最近修改时间 ≈ 目录 / index / HEAD 三者 mtime 的最大值。
fn latest_mtime(dir: &Path) -> Option<f64> {
    let mut best: Option<f64> = None;
    for p in [
        dir.to_path_buf(),
        dir.join(".git").join("index"),
        dir.join(".git").join("HEAD"),
    ] {
        if let Ok(m) = std::fs::metadata(&p).and_then(|md| md.modified()) {
            if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                let t = d.as_secs_f64();
                best = Some(best.map_or(t, |b: f64| b.max(t)));
            }
        }
    }
    best
}

/// `git -C <dir> pull --ff-only`（无窗口；调用方放到后台线程里跑，别堵 UI）。
/// 返回 (成功, 输出全文)，含 stdout + stderr。
pub fn git_pull(repo_dir: &str) -> (bool, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["pull", "--ff-only"])
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
        Err(e) => (false, format!("无法启动 git（{e}）")),
    }
}

/// git CLI 是否可用（selftest 用）。
pub fn git_available() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("git")
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
