//! ExeTrace（Rust + Win32 原生控件版）入口。
//!
//! 用法：
//!   exetrace.exe              正常启动（GUI + 托盘 + 后台记录）
//!   exetrace.exe --minimized  启动后直接缩到托盘
//!   exetrace.exe --selftest   非 GUI 自检（报告落盘）
//!
//! 「交给 Agent」接口（2026-10-16，实现见 `agent.rs`）：
//!   exetrace.exe --export [输出路径]                     全量数据 JSON（apps + projects + htmls，schema v2）
//!   exetrace.exe --find <关键词>                         查找（stdout JSON；0=有结果 1=无结果 2=出错）
//!   exetrace.exe --launch <路径>                         打开应用 / 文件夹
//!   exetrace.exe --make-shortcut <路径> [--name 名称]    钉到桌面（读回校验）
//!   exetrace.exe --agent-guide [输出路径]                生成接口说明（默认 <数据目录>\agent-guide.md）
//!   exetrace.exe --whitelist <名字或 exe 路径>           加入 memfuse 白名单（protect-list.txt）

// 正式产物（release）不弹控制台窗口；debug 保留控制台便于排查。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod appicon;
mod config;
mod fghook;
mod memfuse;
mod paths;
mod projects;
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
    // 屏幕外 + 不激活：给截图 / 演示采集用（用户无感）
    let offscreen = args.iter().any(|a| a == "--offscreen");
    // 跳过冷启动回填：让隔离库里只有调用方放进去的数据
    let no_scan = args.iter().any(|a| a == "--no-scan");
    // --view all|app|project|html：启动即定位到某个视图（截图 / 演示 / 快捷方式用）
    let initial_view = args
        .iter()
        .position(|a| a == "--view")
        .and_then(|i| args.get(i + 1))
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "app" | "应用" => 1usize,
            "project" | "项目" => 2,
            "html" | "网页" => 3,
            _ => 0,
        })
        .unwrap_or(0);

    if args.iter().any(|a| a == "--selftest") {
        return if selftest::run() { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }

    // CLI：导出数据（JSON）—— 「personal agent 采集友好」的最小数据接口（2026-10-15）。
    if args.iter().any(|a| a == "--export") {
        return cli_export(&args);
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

    // CLI（「交给 Agent」接口，2026-10-16）：查找 / 打开 / 交接说明。
    if let Some(i) = args.iter().position(|a| a == "--find") {
        match args.get(i + 1).filter(|s| !s.starts_with("--")) {
            Some(kw) => return cli_find(kw),
            None => {
                eprintln!("用法: ExeTrace.exe --find <关键词>");
                return ExitCode::from(2);
            }
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--launch") {
        match args.get(i + 1).filter(|s| !s.starts_with("--")) {
            Some(p) => return cli_launch(p),
            None => {
                eprintln!("用法: ExeTrace.exe --launch <路径>");
                return ExitCode::from(2);
            }
        }
    }
    // CLI：加入 memfuse 白名单（2026-10-16）—— 右键那项同一个动作的脚本入口。
    if let Some(i) = args.iter().position(|a| a == "--whitelist") {
        match args.get(i + 1).filter(|s| !s.starts_with("--")) {
            Some(v) => return cli_whitelist(v),
            None => {
                eprintln!("用法: ExeTrace.exe --whitelist <进程名 或 exe 路径>");
                return ExitCode::from(2);
            }
        }
    }
    if args.iter().any(|a| a == "--agent-guide") {
        return cli_agent_guide(&args);
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

    // `--no-scan`：跳过两轮冷启动扫描。演示 / 截图 / 回归用 —— 保证库里只有
    // 调用方预先放进去的数据，不会把本机真实使用痕迹灌进隔离库（2026-10-17）。
    if !no_scan {
        // 冷启动扫描（后台线程）：注册表历史
        {
            let st = store.clone();
            std::thread::spawn(move || {
                if let Ok(mut s) = st.lock() {
                    scanner::scan_all(&mut s);
                }
            });
        }

        // 冷启动扫描（后台线程）：本机 git 项目（2026-10-15，「管理项目入口」的地基）
        {
            let st = store.clone();
            std::thread::spawn(move || {
                if let Ok(mut s) = st.lock() {
                    projects::scan_all(&mut s);
                }
            });
        }
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

    let code = match native_ui::run(
        store.clone(),
        last_event,
        minimized,
        !no_tray,
        paused.clone(),
        offscreen,
        initial_view,
    ) {
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

/// CLI：导出库内容为 JSON（默认 `<data_dir>\export.json`，可传一个输出路径）。
/// schema `exetrace.export` v2：`apps[]` 与 `projects[]` 分开（见 `agent.rs`）。
fn cli_export(args: &[String]) -> ExitCode {
    use std::io::Write;
    let i = args.iter().position(|a| a == "--export").unwrap_or(0);
    let out = args
        .get(i + 1)
        .filter(|s| !s.starts_with("--"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths::data_dir().join("export.json"));
    let store = match store::Store::open(&paths::db_path()) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: 无法打开历史库: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = agent::export_doc(&store);
    let text = match serde_json::to_string_pretty(&doc) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: 序列化失败: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::write(&out, text) {
        let _ = writeln!(std::io::stderr(), "error: 写入失败: {e}");
        return ExitCode::from(2);
    }
    let _ = writeln!(
        std::io::stdout(),
        "exported: {} (apps={}, projects={})",
        out.display(),
        doc["counts"]["apps"],
        doc["counts"]["projects"]
    );
    ExitCode::SUCCESS
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

/// CLI：关键词查找（应用 + 项目；stdout 输出 JSON）。
/// 退出码：0 = 有结果 / 1 = 无结果 / 2 = 出错。
fn cli_find(keyword: &str) -> ExitCode {
    use std::io::Write;
    let store = match store::Store::open(&paths::db_path()) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: 无法打开历史库: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = agent::find_doc(&store, keyword, 20);
    let count = doc["count"].as_i64().unwrap_or(0);
    match serde_json::to_string_pretty(&doc) {
        Ok(text) => {
            let _ = writeln!(std::io::stdout(), "{text}");
            if count > 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: 序列化失败: {e}");
            ExitCode::from(2)
        }
    }
}

/// CLI：打开应用 / 文件夹（`--launch <路径>`）。退出码：0 = 已启动 / 1 = 失败或路径不存在。
fn cli_launch(path: &str) -> ExitCode {
    use std::io::Write;
    if !std::path::Path::new(path).exists() {
        let _ = writeln!(std::io::stderr(), "error: 路径不存在: {path}");
        return ExitCode::from(1);
    }
    if winx::launch_path(path) {
        ExitCode::SUCCESS
    } else {
        let _ = writeln!(std::io::stderr(), "error: 启动失败: {path}");
        ExitCode::from(1)
    }
}

/// CLI：把进程名加进 memfuse 的 `protect-list.txt`（`--whitelist <名字或 exe 路径>`）。
///
/// 退出码：0 = 已加入或本来就在名单里 / 1 = 失败（找不到 memfuse、调用失败）/ 2 = 用法错。
fn cli_whitelist(value: &str) -> ExitCode {
    use std::io::Write;
    let name = memfuse::name_from(value);
    if name.is_empty() {
        let _ = writeln!(std::io::stderr(), "error: 进程名为空");
        return ExitCode::from(2);
    }
    let Some(script) = memfuse::resolve_script() else {
        let _ = writeln!(
            std::io::stderr(),
            "error: 没找到 memfuse 的 memory-guard.ps1（设 config.json 的 memfuse_script，或环境变量 MEMFUSE_SCRIPT）"
        );
        return ExitCode::from(1);
    };
    if memfuse::is_listed(&script, &name) {
        let _ = writeln!(std::io::stdout(), "already: {name}");
        return ExitCode::SUCCESS;
    }
    let (ok, detail) = memfuse::add_to_protect_list(&script, &name);
    if ok {
        let _ = writeln!(std::io::stdout(), "added: {name}");
        ExitCode::SUCCESS
    } else {
        let _ = writeln!(std::io::stderr(), "error: 加入失败: {detail}");
        ExitCode::from(1)
    }
}

/// CLI：生成「交给 Agent」的接口说明（Markdown；默认 `<数据目录>\agent-guide.md`）。
/// 全文同时打印到 stdout —— 用户直接复制转发给任意 AI Agent 即可。
fn cli_agent_guide(args: &[String]) -> ExitCode {
    use std::io::Write;
    let i = args.iter().position(|a| a == "--agent-guide").unwrap_or(0);
    let out = args
        .get(i + 1)
        .filter(|s| !s.starts_with("--"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(agent::guide_path);
    let text = agent::guide_text();
    if let Err(e) = std::fs::write(&out, &text) {
        let _ = writeln!(std::io::stderr(), "error: 写入失败: {e}");
        return ExitCode::from(2);
    }
    let _ = writeln!(std::io::stdout(), "guide: {}", out.display());
    let _ = writeln!(std::io::stdout(), "{text}");
    ExitCode::SUCCESS
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
