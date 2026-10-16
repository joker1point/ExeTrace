//! 非 GUI 自检（对齐 Python 版 --selftest 的检查项）。
//!
//! windowed 产物没有控制台，报告同时落盘 `%LOCALAPPDATA%\ExeTrace\selftest.txt`。

use std::io::Write;

use crate::agent;
use crate::paths;
use crate::resolver;
use crate::scanner;
use crate::shortcut;
use crate::store::Store;
use crate::winx;

fn report(lines: &mut Vec<String>, ok: &mut bool, name: &str, passed: bool, detail: &str) {
    *ok = *ok && passed;
    let line = format!(
        "[{}] {} {}",
        if passed { "PASS" } else { "FAIL" },
        name,
        detail
    )
    .trim_end()
    .to_string();
    println!("{line}");
    lines.push(line);
}

pub fn run() -> bool {
    let mut lines: Vec<String> = Vec::new();
    let mut ok = true;
    let samples = sample_exes();

    let data_dir = paths::data_dir();
    report(&mut lines, &mut ok, "data_dir", data_dir.is_dir(), &data_dir.display().to_string());

    // 存储层：临时库走全流程
    let tmp_dir = std::env::temp_dir().join(format!("exetrace_rs_selftest_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let db = tmp_dir.join("t.db");
    match Store::open(&db) {
        Ok(mut st) => {
            report(&mut lines, &mut ok, "sample_exes", !samples.is_empty(), &format!("{} 个真实 exe", samples.len()));

            let past = crate::store::unix_now() - 3600.0;
            for (i, s) in samples.iter().take(3).enumerate() {
                let _ = st.upsert_seed(s, Some(i as i64 + 1), Some(past - i as f64 * 60.0));
            }
            report(&mut lines, &mut ok, "store_seed", st.total() == samples.len().min(3) as i64, &format!("total={}", st.total()));

            if let Some(first) = samples.first() {
                let c1 = st.record_launch(first, 300.0).unwrap_or(false);
                report(&mut lines, &mut ok, "store_record", c1, &format!("counted={c1}"));
                let c2 = st.record_launch(first, 300.0).unwrap_or(false);
                report(&mut lines, &mut ok, "store_dedupe", !c2, &format!("counted={c2}"));

                let rows = st.query("", "count", 50, 0, "all").unwrap_or_default();
                report(&mut lines, &mut ok, "store_query", !rows.is_empty(), &format!("rows={}", rows.len()));

                // v3.1：pinned 标记 + 使用时长累加 + smart 排序
                let _ = st.mark_pinned(first);
                let mut usage = std::collections::HashMap::new();
                usage.insert(first.clone(), 12.0);
                st.add_usage(&usage);
                let rows31 = st.query("", "name", 50, 0, "all").unwrap_or_default();
                let hit = rows31.iter().find(|r| r.path.eq_ignore_ascii_case(first));
                let ok31 = hit
                    .map(|r| r.pinned && (r.total_seconds - 12.0).abs() < 0.001)
                    .unwrap_or(false);
                report(&mut lines, &mut ok, "store_v31_columns", ok31, "pinned=1 / total_seconds=12");
                let smart = st.query("", "smart", 50, 0, "all").unwrap_or_default();
                report(&mut lines, &mut ok, "store_smart_sort", !smart.is_empty(), &format!("{} 行", smart.len()));

                let _ = st.remove(first);
                report(&mut lines, &mut ok, "store_remove", st.total() == samples.len().min(3) as i64 - 1, &format!("total={}", st.total()));
            }
            // 天数过滤：全新的「40 天前」记录在 days=30 下隐藏、days=0 下可见
            // （用不存在的假路径 + upsert_seed 的 MAX(last_seen) 语义避开既有行）
            {
                let old_path = r"C:\__selftest__\old_tool.exe";
                let old_ts = crate::store::unix_now() - 40.0 * 86400.0;
                let _ = st.upsert_seed(old_path, Some(1), Some(old_ts));
                let recent = st.query("", "count", 50, 30, "all").unwrap_or_default();
                let all = st.query("", "count", 50, 0, "all").unwrap_or_default();
                let hidden = !recent.iter().any(|r| r.path.eq_ignore_ascii_case(old_path));
                let shown = all.iter().any(|r| r.path.eq_ignore_ascii_case(old_path));
                report(
                    &mut lines,
                    &mut ok,
                    "store_days_filter",
                    hidden && shown,
                    "40 天前的记录：days=30 隐藏 / days=0 可见",
                );

                // 项目行（2026-10-15）：days 过滤豁免 + view 过滤 + origin 落库
                let proj = r"C:\__selftest__\a_project";
                let _ = st.upsert_project(proj, "a_project", "own", Some(old_ts));
                let recent2 = st.query("", "count", 50, 30, "all").unwrap_or_default();
                let proj_only = st.query("", "count", 50, 30, "project").unwrap_or_default();
                let apps_only = st.query("", "count", 50, 30, "app").unwrap_or_default();
                let p_hit = recent2
                    .iter()
                    .find(|r| r.path == proj)
                    .map(|r| r.origin == "own" && r.source == "project")
                    .unwrap_or(false);
                let proj_ok = recent2.iter().any(|r| r.path == proj)
                    && proj_only.iter().all(|r| r.source == "project")
                    && !apps_only.iter().any(|r| r.source == "project")
                    && p_hit;
                report(
                    &mut lines,
                    &mut ok,
                    "store_project_rows",
                    proj_ok,
                    "项目行：days 豁免 / view 过滤 / origin 落库",
                );

                // 「交给 Agent」接口（2026-10-16）：导出拆 apps/projects + find 命中/无果分支
                {
                    let doc = agent::export_doc(&st);
                    let apps = doc["apps"].as_array().cloned().unwrap_or_default();
                    let projs = doc["projects"].as_array().cloned().unwrap_or_default();
                    let split_ok = doc["schemaVersion"].as_i64() == Some(agent::EXPORT_SCHEMA_VERSION)
                        && !apps.is_empty()
                        && !apps.iter().any(|r| r["source"] == "project")
                        && projs.iter().any(|r| r["path"] == proj);
                    report(
                        &mut lines,
                        &mut ok,
                        "agent_export_split",
                        split_ok,
                        &format!("schema v{} · apps={} projects={}", agent::EXPORT_SCHEMA_VERSION, apps.len(), projs.len()),
                    );
                }
                {
                    let hit = agent::find_doc(&st, "a_project", 20);
                    let miss = agent::find_doc(&st, "__zz_no_such_tool__", 20);
                    let hit_ok = hit["projects"]
                        .as_array()
                        .map(|a| a.iter().any(|r| r["path"] == proj))
                        .unwrap_or(false);
                    let miss_ok = miss["count"].as_i64() == Some(0);
                    report(
                        &mut lines,
                        &mut ok,
                        "agent_find",
                        hit_ok && miss_ok,
                        "命中项目 / 无结果 count=0",
                    );
                }
            }
            let _ = st.prune_missing();
        }
        Err(e) => report(&mut lines, &mut ok, "store_open", false, &format!("{e}")),
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);

    // 注册表扫描（只读）
    let scan_db = std::env::temp_dir().join(format!("exetrace_rs_scan_{}.db", std::process::id()));
    match Store::open(&scan_db) {
        Ok(mut st) => {
            let stats = scanner::scan_all(&mut st);
            report(
                &mut lines,
                &mut ok,
                "registry_scan",
                stats.total > 0,
                &format!("收录 {} 个应用（feature_usage={} muicache={}）", stats.total, stats.feature_usage, stats.muicache),
            );
        }
        Err(e) => report(&mut lines, &mut ok, "registry_scan", false, &format!("{e}")),
    }
    let _ = std::fs::remove_file(&scan_db);

    // 平台能力
    let pids = winx::visible_window_pids();
    report(&mut lines, &mut ok, "window_enum", !pids.is_empty(), &format!("{} 个带窗口进程", pids.len()));

    let procs = winx::snapshot_processes();
    report(&mut lines, &mut ok, "process_snapshot", procs.len() > 10, &format!("{} 个进程", procs.len()));

    match winx::process_exe_path(std::process::id()) {
        Some(p) => report(
            &mut lines,
            &mut ok,
            "process_exe_path",
            p.to_lowercase().ends_with("exetrace.exe"),
            &p,
        ),
        None => report(&mut lines, &mut ok, "process_exe_path", false, "读取自身 exe 路径失败"),
    }

    // 「交给 Agent」交接文本：四条契约与调用注意必须在内（内容随本机路径生成）
    {
        let g = agent::guide_text();
        let guide_ok = g.contains("--export")
            && g.contains("--find")
            && g.contains("--launch")
            && g.contains("--make-shortcut");
        report(&mut lines, &mut ok, "agent_guide", guide_ok, &format!("{} 字符", g.chars().count()));
    }

    // 运行中判定（列表自动屏蔽运行中应用用）：拿自己的名字去查 → 必须能读出自己在跑的路径
    {
        let mut stems = std::collections::HashSet::new();
        stems.insert("exetrace".to_string());
        let (paths, _fallback) = winx::running_exe_lookup(&stems);
        report(
            &mut lines,
            &mut ok,
            "running_lookup",
            paths.iter().any(|p| p.ends_with("exetrace.exe")),
            &format!("{} 条运行中路径", paths.len()),
        );
    }

    // 项目扫描（2026-10-15）：本机应能扫到仓库；browse_url 必须剥掉 URL 里的凭据
    {
        let scan_db = std::env::temp_dir().join(format!("exetrace_rs_proj_{}.db", std::process::id()));
        match Store::open(&scan_db) {
            Ok(mut st) => {
                let stats = crate::projects::scan_all(&mut st);
                report(
                    &mut lines,
                    &mut ok,
                    "project_scan",
                    stats.found > 0,
                    &format!(
                        "发现 {} 个项目（自有 {} / 克隆 {} / 本地 {}）",
                        stats.found, stats.own, stats.clone, stats.local
                    ),
                );
            }
            Err(e) => report(&mut lines, &mut ok, "project_scan", false, &format!("{e}")),
        }
        let _ = std::fs::remove_file(&scan_db);

        // 凭据剥离：内嵌 token 的 remote 不得原样返回
        let fix = std::env::temp_dir().join(format!("exetrace_rs_gitfix_{}", std::process::id()));
        let _ = std::fs::create_dir_all(fix.join(".git"));
        let _ = std::fs::write(
            fix.join(".git").join("config"),
            "[remote \"origin\"]\n\turl = https://ghp_FAKEtokenFAKEtoken@github.com/joker1point/demo.git\n",
        );
        let url = crate::projects::browse_url(&fix.to_string_lossy()).unwrap_or_default();
        report(
            &mut lines,
            &mut ok,
            "project_url_strip",
            url == "https://github.com/joker1point/demo.git",
            &url,
        );
        let _ = std::fs::remove_dir_all(&fix);
    }

    report(
        &mut lines,
        &mut ok,
        "git_cli",
        crate::projects::git_available(),
        "git --version 可执行",
    );

    report(
        &mut lines,
        &mut ok,
        "noise_filter",
        winx::is_noise_exe(Some(r"C:\Windows\System32\svchost.exe")) && !winx::is_noise_exe(Some(r"C:\Program Files\7-Zip\7zFM.exe")),
        "系统进程过滤 / 用户应用保留",
    );

    let icon_src = samples.first().cloned().unwrap_or_default();
    if !icon_src.is_empty() {
        match winx::extract_icon_rgba(&icon_src, 24) {
            Some((w, h, buf)) => report(
                &mut lines,
                &mut ok,
                "icon_extract",
                buf.len() == w * h * 4 && buf.chunks(4).any(|p| p[3] > 0),
                &format!("{}x{} {} bytes", w, h, buf.len()),
            ),
            None => report(&mut lines, &mut ok, "icon_extract", false, "提取失败"),
        }
    }

    let desktop = winx::desktop_dir();
    report(&mut lines, &mut ok, "desktop_dir", std::path::Path::new(&desktop).is_dir(), &desktop);

    // v3.1：分类 / 时长格式 / 快捷方式创建读回 / 文件夹识别
    report(
        &mut lines,
        &mut ok,
        "categorize",
        winx::categorize(r"C:\Program Files\Microsoft VS Code\Code.exe") == "开发"
            && winx::categorize(r"C:\Program Files\Google\Chrome\chrome.exe") == "浏览器"
            && winx::categorize(r"C:\mystery-tool.exe") == "其他",
        "开发 / 浏览器 / 其他",
    );
    report(
        &mut lines,
        &mut ok,
        "human_duration",
        winx::human_duration(5.0) == "—"
            && winx::human_duration(45.0) == "45s"
            && winx::human_duration(120.0) == "2m"
            && winx::human_duration(5400.0) == "1.5h",
        "— / 45s / 2m / 1.5h",
    );

    {
        let tmp = std::env::temp_dir().join(format!("exetrace_rs_lnk_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let lnk_path = tmp.join("selftest.lnk");
        let target = samples.first().cloned().unwrap_or_default();
        let lnk_s = lnk_path.to_string_lossy().to_string();
        let created = !target.is_empty()
            && shortcut::create_shortcut(&lnk_s, &target, "", &target, "selftest").is_ok();
        let readback = created
            && shortcut::read_shortcut(&lnk_s)
                .map(|i| i.target.eq_ignore_ascii_case(&target))
                .unwrap_or(false);
        report(&mut lines, &mut ok, "shortcut_create", created, &lnk_s);
        report(&mut lines, &mut ok, "shortcut_readback", readback, &target);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    {
        let sample = samples.first().cloned().unwrap_or_default();
        let parent = std::path::Path::new(&sample)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let res = resolver::resolve(&parent);
        report(
            &mut lines,
            &mut ok,
            "resolver_folder",
            res.ok && res.target.is_some(),
            &format!("{} 个候选 → {}", res.candidates.len(), res.target.as_deref().unwrap_or("—")),
        );
    }

    report(
        &mut lines,
        &mut ok,
        "autostart_readonly",
        true,
        &format!("当前自启状态 = {}", winx::is_autostart_enabled()),
    );

    let singleton = winx::acquire_single_instance();
    report(&mut lines, &mut ok, "single_instance", singleton.is_some(), "互斥体获取");

    // watcher 冒烟（不依赖 GUI）：起监控 → 拉起一个带窗口的靶子 → 期望入库
    match find_pythonw() {
        Some(pythonw) => {
            let db = std::env::temp_dir().join(format!("exetrace_rs_watch_{}.db", std::process::id()));
            match Store::open(&db) {
                Ok(st) => {
                    let shared = std::sync::Arc::new(std::sync::Mutex::new(st));
                    let walrus = crate::watcher::Watcher::spawn(
                        shared.clone(),
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        |_e, _c| {},
                    );
                    std::thread::sleep(std::time::Duration::from_secs(3)); // 让基线建立
                    let script = std::env::temp_dir().join("exetrace_rs_watch_target.py");
                    let _ = std::fs::write(
                        &script,
                        "import tkinter as tk\nr = tk.Tk()\nr.title('ExeTrace RS watch smoke')\nr.geometry('320x120')\nr.after(20000, r.destroy)\nr.mainloop()\n",
                    );
                    let mut child = std::process::Command::new(&pythonw).arg(&script).spawn().ok();
                    let mut recorded = false;
                    for _ in 0..25 {
                        std::thread::sleep(std::time::Duration::from_millis(1000));
                        let total = shared.lock().map(|s| s.total()).unwrap_or(0);
                        if total > 0 {
                            recorded = true;
                            break;
                        }
                    }
                    if let Some(c) = child.as_mut() {
                        let _ = c.kill();
                    }
                    walrus.stop();
                    report(
                        &mut lines,
                        &mut ok,
                        "watcher_smoke",
                        recorded,
                        if recorded { "靶子被实时记录" } else { "25 秒内未记录到靶子" },
                    );
                    let _ = std::fs::remove_file(&db);
                    let _ = std::fs::remove_file(&script);
                }
                Err(e) => report(&mut lines, &mut ok, "watcher_smoke", false, &format!("临时库打不开: {e}")),
            }
        }
        None => report(&mut lines, &mut ok, "watcher_smoke", true, "跳过（未找到 pythonw.exe）"),
    }

    lines.push(format!("SELFTEST {}", if ok { "OK" } else { "FAILED" }));
    let report_path = paths::selftest_report_path();
    if let Ok(mut f) = std::fs::File::create(&report_path) {
        let _ = f.write_all(lines.join("\n").as_bytes());
    }
    println!("{}", lines.last().unwrap());
    ok
}

/// 找一个可用的 pythonw.exe 作为冒烟测试的「带窗口靶子」。
fn find_pythonw() -> Option<String> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let base = std::path::Path::new(&local).join("Programs").join("Python");
    if let Ok(rd) = std::fs::read_dir(&base) {
        for entry in rd.flatten() {
            let p = entry.path().join("pythonw.exe");
            if p.is_file() {
                return Some(p.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn sample_exes() -> Vec<String> {
    let mut out = Vec::new();
    let roots = [
        std::env::var("ProgramFiles").unwrap_or_default(),
        std::env::var("ProgramFiles(x86)").unwrap_or_default(),
        std::env::var("LOCALAPPDATA").unwrap_or_default(),
        std::env::var("WINDIR").unwrap_or_default(),
    ];
    for root in roots.iter().filter(|r| !r.is_empty()) {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        for entry in rd.flatten().take(40) {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) {
                out.push(p.to_string_lossy().to_string());
            } else if p.is_dir() {
                if let Ok(rd2) = std::fs::read_dir(&p) {
                    for e2 in rd2.flatten().take(20) {
                        let p2 = e2.path();
                        if p2.is_file() && p2.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) {
                            out.push(p2.to_string_lossy().to_string());
                        }
                    }
                }
            }
            if out.len() >= 6 {
                break;
            }
        }
        if out.len() >= 6 {
            break;
        }
    }
    out.truncate(6);
    out
}
