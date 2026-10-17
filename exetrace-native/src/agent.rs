//! 「交给 Agent」接口（2026-10-16）。
//!
//! 把 ExeTrace 变成任何 AI Agent 可直接使用的数据源 + 能力提供方：
//!
//! - `export_doc` / `find_doc`：机器可读 JSON（应用 + 本机 git 项目），Agent 读数据；
//! - `guide_text`：随本机真实路径生成的 Markdown 交接文本 —— 用户执行
//!   `ExeTrace.exe --agent-guide` 后把全文（或文件）转给任意 Agent，
//!   Agent 即按其中的契约调用 `--export` / `--find` / `--launch` / `--make-shortcut`。
//!
//! 只做组装，不做 IO（写文件与打印在 `main.rs` 的 CLI 里）。

use crate::paths;
use crate::store::{AppRow, Store};

pub const EXPORT_SCHEMA: &str = "exetrace.export";
pub const EXPORT_SCHEMA_VERSION: i64 = 2;
pub const FIND_SCHEMA: &str = "exetrace.find";

/// 单行 → JSON（应用与项目共用字段口径；`origin` 仅项目行非空）。
fn row_json(r: &AppRow) -> serde_json::Value {
    serde_json::json!({
        "name": r.name,
        "path": r.path,
        "source": r.source,
        "origin": r.origin,
        "launchCount": r.launch_count,
        "firstSeen": r.first_seen,
        "lastSeen": r.last_seen,
        "pinned": r.pinned,
        "totalSeconds": r.total_seconds,
    })
}

/// 全量导出：`apps[]` / `projects[]` / `htmls[]` 分开（schema v2 —— v1 的单列表混放项目，
/// Agent 还得自己按 source 分流，拆开后「我的项目有哪些」是一次过滤的事；
/// `htmls[]` 是 2026-10-17 新增的**向后兼容加字段**，旧消费方忽略即可）。
pub fn export_doc(st: &Store) -> serde_json::Value {
    let apps: Vec<serde_json::Value> = st
        .query("", "name", 100_000, 0, "app")
        .unwrap_or_default()
        .iter()
        .map(row_json)
        .collect();
    let projects: Vec<serde_json::Value> = st
        .query("", "name", 100_000, 0, "project")
        .unwrap_or_default()
        .iter()
        .map(row_json)
        .collect();
    let htmls: Vec<serde_json::Value> = st
        .query("", "name", 100_000, 0, "html")
        .unwrap_or_default()
        .iter()
        .map(row_json)
        .collect();
    serde_json::json!({
        "schema": EXPORT_SCHEMA,
        "schemaVersion": EXPORT_SCHEMA_VERSION,
        "app": paths::APP_NAME,
        "appVersion": paths::APP_VERSION,
        "exportedAt": crate::store::unix_now(),
        "counts": { "apps": apps.len(), "projects": projects.len(), "htmls": htmls.len() },
        "apps": apps,
        "projects": projects,
        "htmls": htmls,
    })
}

/// 关键词查找（应用 + 项目 + 网页一起找；按常用度 smart 排序，最多 `limit` 条）。
pub fn find_doc(st: &Store, keyword: &str, limit: i64) -> serde_json::Value {
    let rows = st.query(keyword, "smart", limit, 0, "all").unwrap_or_default();
    let apps: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| r.source != "project" && r.source != "html")
        .map(row_json)
        .collect();
    let projects: Vec<serde_json::Value> = rows.iter().filter(|r| r.source == "project").map(row_json).collect();
    let htmls: Vec<serde_json::Value> = rows.iter().filter(|r| r.source == "html").map(row_json).collect();
    serde_json::json!({
        "schema": FIND_SCHEMA,
        "schemaVersion": 1,
        "query": keyword,
        "count": apps.len() + projects.len() + htmls.len(),
        "apps": apps,
        "projects": projects,
        "htmls": htmls,
    })
}

pub fn guide_path() -> std::path::PathBuf {
    paths::data_dir().join("agent-guide.md")
}

/// 交接文本（Markdown）。内容随本机真实路径生成；契约改动时同步改这里。
pub fn guide_text() -> String {
    let exe = paths::exe_path();
    let exe = exe.display();
    let data = paths::data_dir();
    let data = data.display();
    let export = paths::data_dir().join("export.json");
    let export = export.display();
    let guide = guide_path();
    let guide = guide.display();
    format!(
        r#"# ExeTrace · 交给 Agent 的接口说明

> 本文件由 `ExeTrace.exe --agent-guide` 生成（ExeTrace {ver}）。
> 用法：把**全文**（或本文件）发给任意 AI Agent，它即可读取本机的应用 / 项目记录，
> 并按下面第二节的契约调用 ExeTrace 的能力。

## 把这段一起发给 Agent（开场白示例）

> 我的电脑装了 ExeTrace（应用历史定位器，接口见下）。
> 请用它帮我：<把你想做的事写在这，例如「找出我上周用过的截图工具并打开它」>

## 一、数据（读）

1. **全量导出**（应用 + 本机 git 项目 + 打开过的本地网页）：

   `"{exe}" --export "{export}"`

   - JSON，schema `exetrace.export` v2：`apps[]` = 应用、`projects[]` = 项目、
     `htmls[]` = 浏览器打开过的本地网页（2026-10-17 新增，旧消费方忽略即可）；
     `counts` 是三边条数
   - 每行字段：`name` 名称 · `path` 完整路径 · `source`（`scan`=注册表历史回填 /
     `watch`=实时记录 / `project`=本机 git 项目 / `html`=本地网页）· `origin`
     （项目档位 `own`=自有 / `clone`=克隆 / `local`=本地；其余为空）· `launchCount`
     打开次数 · `firstSeen`/`lastSeen` Unix 秒（可为 null）· `pinned` 是否已钉桌面 ·
     `totalSeconds` 累计前台使用时长
2. **关键词查找**（不想拉全量时用）：

   `"{exe}" --find <关键词>`

   - 结果打在 stdout（JSON，`exetrace.find` v1）：`count` / `apps[]` / `projects[]` /
     `htmls[]`，按常用度排序，最多 20 条
   - 退出码：`0` = 有结果，`1` = 无结果，`2` = 出错

## 二、能力（做）

`"{exe}" --launch "<完整路径>"` —— 打开应用 / 文件夹（退出码 `0` = 已启动）

`"{exe}" --make-shortcut "<exe 或 文件夹路径>" [--name "名称"]` —— 钉到桌面，创建后读回校验
（退出码 `0` = 通过；失败细分：`3` 路径解析失败 / `4` 创建失败 / `5` 读回不一致）

`"{exe}" --whitelist "<进程名 或 exe 路径>"` —— 加进 memfuse 的内存白名单
（`protect-list.txt`，内存紧张时这个应用不会被杀；退出码 `0` = 已加入或本来就在名单里，
`1` = 失败 / `2` = 用法错。需要本机装了 memfuse：脚本位置解析顺序为环境变量
`MEMFUSE_SCRIPT` → config.json 的 `memfuse_script` → 两个常见位置）

## 三、调用注意（Windows，实测行为）

- 本程序是 **GUI 子系统 exe**：PowerShell 里 `& exe ...` **不会等它跑完**
  （现象：命令立刻返回、产物文件还没生成）。请用 `cmd /c "..."` 调用，
  或 `Start-Process -Wait`；关键是**以产物文件为准**，stdout 摘要可能晚到。
- 读数据优先读产物文件（`--export` 的输出路径）；`--find` 的结果在 stdout。
- 数据是即时快照：想拿最新的先跑一次 `--export`；不跑读到的就是上一次的。

## 四、两个典型工作流

1. 「打开我上周用的那个看图工具」：`--find 看图` → 从 `apps[].path` 选一条 →
   `--launch "<该 path>"`
2. 「给 7-Zip 钉个桌面快捷方式」：`--find 7z` → `--make-shortcut "<apps[].path>"`

## 五、边界（如实说明）

- 数据全部在**本机**：历史库 `{data}\history.db`、最近导出 `{export}`；本接口不联网。
- `--export` / `--find` 是纯读取；`--launch` / `--make-shortcut` 是显式动作。
- 正在运行的应用会从 GUI 列表隐藏（「找回」语义），但**导出数据里照样有**
  （含 `lastSeen`）——不要据此判断「没装过 / 没记录」。
- 项目列表由 GUI 启动时扫描维护；`--export` 读到的是最近一次扫描的结果。
- 本文件的落盘位置：`{guide}`
"#,
        ver = paths::APP_VERSION,
        exe = exe,
        data = data,
        export = export,
        guide = guide,
    )
}
