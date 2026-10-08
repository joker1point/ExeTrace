//! 把「exe 路径或软件文件夹」解析成可创建快捷方式的目标（对齐 Python 版 resolver.py）。
//!
//! 文件夹识别规则（打分，高者胜）：同名 exe 优先 → 浅层优先 → 体积大优先；
//! 明显的噪音组件（setup/unins/helper/updater/…）直接排除。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub exe: String,
    pub size: u64,
    pub depth: usize,
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub ok: bool,
    pub message: String,
    pub target: Option<String>,
    pub workdir: Option<String>,
    pub icon_source: Option<String>,
    pub default_name: String,
    pub candidates: Vec<Candidate>,
}

/// 噪音组件关键词（文件名小写后contains 即排除）。
const NOISE: &[&str] = &[
    "setup", "install", "uninstall", "unins", "update", "updater", "helper", "crash", "report",
    "vcredist", "dotnetfx", "unpack", "repair", "readme", "license", "eula",
];

const MAX_DEPTH: usize = 2;
const MAX_CANDIDATES: usize = 5;

pub fn resolve(raw: &str) -> Resolution {
    let text = raw.trim().trim_matches('"');
    if text.is_empty() {
        return err("请输入 exe 或软件文件夹路径。");
    }
    let path = PathBuf::from(text);
    if path.is_file() {
        let is_exe = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if !is_exe {
            return err("请选择 .exe 文件，或软件所在的文件夹。");
        }
        let s = path.to_string_lossy().to_string();
        return Resolution {
            ok: true,
            message: "已选择程序文件".into(),
            workdir: path.parent().map(|p| p.to_string_lossy().to_string()),
            icon_source: Some(s.clone()),
            default_name: stem(&s),
            target: Some(s),
            candidates: Vec::new(),
        };
    }
    if !path.is_dir() {
        return err("路径不存在（请检查是否完整）。");
    }

    let folder_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut cands: Vec<Candidate> = Vec::new();
    collect(&path, &path, 0, &mut cands);
    if cands.is_empty() {
        return err("这个文件夹里没有找到 .exe（可试试点进子目录再选）。");
    }
    cands.sort_by(|a, b| score(b, &folder_name).cmp(&score(a, &folder_name)));
    cands.truncate(MAX_CANDIDATES);

    let best = cands[0].clone();
    Resolution {
        ok: true,
        message: format!("已识别 {} 个候选", cands.len()),
        workdir: Path::new(&best.exe)
            .parent()
            .map(|p| p.to_string_lossy().to_string()),
        icon_source: Some(best.exe.clone()),
        default_name: stem(&best.exe),
        target: Some(best.exe),
        candidates: cands,
    }
}

fn collect(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Candidate>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect(root, &p, depth + 1, out);
            continue;
        }
        let is_exe = p
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if !is_exe {
            continue;
        }
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if NOISE.iter().any(|n| name.contains(n)) {
            continue;
        }
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        out.push(Candidate {
            exe: p.to_string_lossy().to_string(),
            size,
            depth,
        });
    }
}

/// 打分：同名（与文件夹同名）> 浅层 > 体积大。
fn score(c: &Candidate, folder_name: &str) -> i64 {
    let mut s = 0i64;
    let stem_l = stem(&c.exe).to_lowercase();
    if !folder_name.is_empty() && stem_l == folder_name.to_lowercase() {
        s += 1000;
    }
    s -= (c.depth as i64) * 120;
    s += ((c.size / 1024) as i64).min(600);
    s
}

fn stem(p: &str) -> String {
    crate::store::file_stem(p)
}

fn err(message: &str) -> Resolution {
    Resolution {
        ok: false,
        message: message.to_string(),
        target: None,
        workdir: None,
        icon_source: None,
        default_name: String::new(),
        candidates: Vec::new(),
    }
}
