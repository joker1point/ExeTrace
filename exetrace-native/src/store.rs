//! SQLite 存储层 —— 与 Python 版**共用同一 schema**，既有历史无缝衔接。

use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS apps (
    path          TEXT PRIMARY KEY COLLATE NOCASE,
    name          TEXT NOT NULL,
    launch_count  INTEGER NOT NULL DEFAULT 0,
    first_seen    REAL,
    last_seen     REAL,
    source        TEXT NOT NULL DEFAULT 'scan',
    origin        TEXT NOT NULL DEFAULT '',
    pinned        INTEGER NOT NULL DEFAULT 0,
    total_seconds REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_apps_last_seen ON apps(last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_apps_count     ON apps(launch_count DESC);
";

#[derive(Debug, Clone)]
pub struct AppRow {
    pub path: String,
    pub name: String,
    pub launch_count: i64,
    pub first_seen: Option<f64>,
    pub last_seen: Option<f64>,
    pub source: String,
    /// 项目档位（source='project' 时有效）：own / clone / local；应用行为空串。
    pub origin: String,
    pub pinned: bool,
    pub total_seconds: f64,
}

pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub struct Store {
    conn: Connection,
    rev: u64,
}

impl Store {
    pub fn open(db_path: &std::path::Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        Ok(Self { conn, rev: 0 })
    }

    /// 幂等迁移：老库（无 pinned / total_seconds）补列，重跑不炸（对齐 Python 版）。
    fn migrate(conn: &Connection) -> rusqlite::Result<()> {
        let mut cols: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare("PRAGMA table_info(apps)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for r in rows {
                cols.push(r?);
            }
        }
        if !cols.iter().any(|c| c == "pinned") {
            conn.execute("ALTER TABLE apps ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if !cols.iter().any(|c| c == "total_seconds") {
            conn.execute("ALTER TABLE apps ADD COLUMN total_seconds REAL NOT NULL DEFAULT 0", [])?;
        }
        if !cols.iter().any(|c| c == "origin") {
            conn.execute("ALTER TABLE apps ADD COLUMN origin TEXT NOT NULL DEFAULT ''", [])?;
        }
        Ok(())
    }

    /// 数据版本号：只有真的变化时 UI 才重建列表（对齐 Python 版的 rev 机制）。
    pub fn rev(&self) -> u64 {
        self.rev
    }

    fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    /// 记录一次「打开」。同一路径在 dedupe_window 秒内重复出现只刷新时间、不涨计数。
    /// 返回是否算作一次新的打开。
    pub fn record_launch(&mut self, exe_path: &str, dedupe_window: f64) -> rusqlite::Result<bool> {
        let now = unix_now();
        let name = file_stem(exe_path);
        let existing: Option<(i64, Option<f64>)> = self
            .conn
            .query_row(
                "SELECT launch_count, last_seen FROM apps WHERE path = ?1",
                params![exe_path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        match existing {
            None => {
                self.conn.execute(
                    "INSERT INTO apps(path, name, launch_count, first_seen, last_seen, source)
                     VALUES(?1, ?2, 1, ?3, ?3, 'watch')",
                    params![exe_path, name, now],
                )?;
                self.bump();
                Ok(true)
            }
            Some((_, Some(last))) if now - last < dedupe_window => {
                self.conn.execute(
                    "UPDATE apps SET last_seen = ?1 WHERE path = ?2",
                    params![now, exe_path],
                )?;
                self.bump();
                Ok(false)
            }
            Some(_) => {
                self.conn.execute(
                    "UPDATE apps SET launch_count = launch_count + 1, last_seen = ?1, source = 'watch'
                     WHERE path = ?2",
                    params![now, exe_path],
                )?;
                self.bump();
                Ok(true)
            }
        }
    }

    /// 冷启动扫描的种子记录：计数取最大值（不叠加），时间取较新者。
    pub fn upsert_seed(
        &mut self,
        exe_path: &str,
        launch_count: Option<i64>,
        last_seen: Option<f64>,
    ) -> rusqlite::Result<()> {
        let name = file_stem(exe_path);
        let cnt = launch_count.unwrap_or(0);
        self.conn.execute(
            "INSERT INTO apps(path, name, launch_count, first_seen, last_seen, source)
             VALUES(?1, ?2, ?3, NULL, ?4, 'scan')
             ON CONFLICT(path) DO UPDATE SET
                 name         = excluded.name,
                 launch_count = MAX(launch_count, excluded.launch_count),
                 last_seen    = MAX(COALESCE(last_seen, 0), COALESCE(excluded.last_seen, 0)),
                 source       = CASE WHEN apps.source = 'watch' THEN 'watch' ELSE 'scan' END",
            params![exe_path, name, cnt, last_seen],
        )?;
        self.bump();
        Ok(())
    }

    /// 项目扫描的种子记录（`source='project'`）：只更新元数据，**不动** `pinned` /
    /// 使用统计（重扫不惩罚用户钉过的项目）。批量扫描由调用方在结尾统一 `mark_changed`。
    pub fn upsert_project(
        &mut self,
        path: &str,
        name: &str,
        origin: &str,
        last_seen: Option<f64>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO apps(path, name, launch_count, first_seen, last_seen, source, origin)
             VALUES(?1, ?2, 0, NULL, ?3, 'project', ?4)
             ON CONFLICT(path) DO UPDATE SET
                 name      = excluded.name,
                 origin    = excluded.origin,
                 last_seen = COALESCE(excluded.last_seen, apps.last_seen),
                 source    = CASE WHEN apps.source = 'watch' THEN 'watch' ELSE 'project' END",
            params![path, name, last_seen, origin],
        )?;
        Ok(())
    }

    /// HTML 打开历史的种子记录（`source='html'`）：只更新元数据，不动首次时间。
    /// 数据源 = Windows「最近使用的项目」里的 .lnk（见 `scanner::scan_recent_html`）。
    pub fn upsert_html(
        &mut self,
        path: &str,
        name: &str,
        last_seen: Option<f64>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO apps(path, name, launch_count, first_seen, last_seen, source)
             VALUES(?1, ?2, 0, ?3, ?3, 'html')
             ON CONFLICT(path) DO UPDATE SET
                 name      = excluded.name,
                 last_seen = MAX(COALESCE(last_seen, 0), COALESCE(excluded.last_seen, 0)),
                 source    = CASE WHEN apps.source IN ('watch', 'html') THEN apps.source ELSE 'html' END",
            params![path, name, last_seen],
        )?;
        Ok(())
    }

    /// 批量写入后手动标脏一次（避免逐行 bump 造成扫描期间 UI 反复重建）。
    pub fn mark_changed(&mut self) {
        self.bump();
    }

    pub fn remove(&mut self, exe_path: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM apps WHERE path = ?1", params![exe_path])?;
        self.bump();
        Ok(())
    }

    pub fn prune_missing(&mut self) -> rusqlite::Result<usize> {
        let mut paths: Vec<String> = Vec::new();
        {
            let mut stmt = self.conn.prepare("SELECT path FROM apps")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for r in rows {
                paths.push(r?);
            }
        }
        let mut removed = 0usize;
        for p in paths {
            // 应用行 = 文件；项目行 = 目录。两者都不存在才算真的失效。
            let path = std::path::Path::new(&p);
            if !path.is_file() && !path.is_dir() {
                self.conn
                    .execute("DELETE FROM apps WHERE path = ?1", params![p])?;
                removed += 1;
            }
        }
        if removed > 0 {
            self.bump();
        }
        Ok(removed)
    }

    /// `view`：`all` / `app` / `project`（项目 = `source='project'`）/ `html`
    /// （浏览器打开过的本地网页，`source='html'`；2026-10-17）。
    /// `days > 0` 时**应用行与网页行**只保留「最后打开时间在最近 days 天内」的（无时间戳的
    /// 注册表回填会被过滤）；**项目行不受 days 过滤** —— 仓库多久没动都该能被找到。
    pub fn query(
        &self,
        keyword: &str,
        sort: &str,
        limit: i64,
        days: i64,
        view: &str,
    ) -> rusqlite::Result<Vec<AppRow>> {
        let kw = keyword.trim();
        // smart（frecency）在 Rust 侧排序：SQLite 默认没有 exp()；
        // 其它排序交给 SQL。SQL 侧统一加防御性 LIMIT（smart 取全量再截断）。
        // 项目视图：先按档位分组（own → clone → local），**档内再按所选排序** ——
        // 用户切「最近打开」时必须真的按时间排（2026-10-15 反馈修正）。
        // smart 在项目里没有意义（launch_count 全为 0），退化为时间排序。
        let (order, sql_limit) = if view == "project" {
            let sub = match sort {
                "count" => "launch_count DESC, COALESCE(last_seen, 0) DESC",
                "name" => "name COLLATE NOCASE, path COLLATE NOCASE",
                _ => "COALESCE(last_seen, 0) DESC, name COLLATE NOCASE",
            };
            (
                format!(
                    "ORDER BY CASE origin WHEN 'own' THEN 0 WHEN 'clone' THEN 1 ELSE 2 END, {sub}"
                ),
                limit,
            )
        } else {
            match sort {
                "count" => (
                    "ORDER BY launch_count DESC, COALESCE(last_seen, 0) DESC".to_string(),
                    limit,
                ),
                "name" => ("ORDER BY name COLLATE NOCASE, path COLLATE NOCASE".to_string(), limit),
                "smart" => (String::new(), limit.max(5000)),
                _ => (
                    "ORDER BY COALESCE(last_seen, 0) DESC, name COLLATE NOCASE".to_string(),
                    limit,
                ),
            }
        };
        let days = days.clamp(0, 3650);
        let cutoff = if days > 0 {
            unix_now() - days as f64 * 86400.0
        } else {
            0.0
        };
        let sql = format!(
            "SELECT path, name, launch_count, first_seen, last_seen, source, origin, pinned, total_seconds
             FROM apps
             WHERE (?1 = '' OR name LIKE ?2 ESCAPE '\\' OR path LIKE ?2 ESCAPE '\\')
               AND (?4 = 0 OR source = 'project' OR (last_seen IS NOT NULL AND last_seen >= ?5))
               AND (?6 = 'all'
                    OR (?6 = 'project' AND source = 'project')
                    OR (?6 = 'app' AND source NOT IN ('project', 'html'))
                    OR (?6 = 'html' AND source = 'html'))
             {order} LIMIT ?3"
        );
        let like = format!(
            "%{}%",
            kw.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![kw, like, sql_limit, days, cutoff, view], |r| {
            Ok(AppRow {
                path: r.get(0)?,
                name: r.get(1)?,
                launch_count: r.get(2)?,
                first_seen: r.get(3)?,
                last_seen: r.get(4)?,
                source: r.get(5)?,
                origin: r.get(6)?,
                pinned: r.get::<_, i64>(7)? != 0,
                total_seconds: r.get(8)?,
            })
        })?;
        let mut out: Vec<AppRow> = rows.collect::<rusqlite::Result<_>>()?;
        if sort == "smart" && view != "project" {
            let now = unix_now();
            out.sort_by(|a, b| {
                frecency(b, now)
                    .partial_cmp(&frecency(a, now))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            out.truncate(limit.max(0) as usize);
        }
        Ok(out)
    }

    /// 累计前台使用时长（对齐 Python 版 `add_usage`；路径不在库里则静默忽略）。
    pub fn add_usage(&mut self, seconds_by_path: &std::collections::HashMap<String, f64>) {
        let mut touched = false;
        for (path, secs) in seconds_by_path {
            if *secs <= 0.0 {
                continue;
            }
            if self
                .conn
                .execute(
                    "UPDATE apps SET total_seconds = total_seconds + ?1 WHERE path = ?2",
                    params![secs, path],
                )
                .is_ok()
            {
                touched = true;
            }
        }
        if touched {
            self.bump();
        }
    }

    /// 标记「已钉到桌面」（幂等；路径不在库中时无副作用）。
    pub fn mark_pinned(&mut self, exe_path: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE apps SET pinned = 1 WHERE path = ?1",
            params![exe_path],
        )?;
        self.bump();
        Ok(())
    }

    /// 「已收录」应用数（不含项目行与网页行；两者单独统计）。
    pub fn total(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM apps WHERE source NOT IN ('project', 'html')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// 已收录的项目数。
    pub fn total_projects(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM apps WHERE source = 'project'", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// 已收录的本地网页数（浏览器打开过、被「最近使用的项目」记录）。2026-10-17。
    pub fn total_html(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM apps WHERE source = 'html'", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn count_today(&self) -> i64 {
        let start = today_start_ts();
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM apps WHERE last_seen >= ?1 AND source != 'project'",
                params![start],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// 按启动次数排序取前 N（用于「Dashboard」式统计，暂未使用也保留接口）。
    pub fn top_by_count(&self, n: i64) -> rusqlite::Result<Vec<AppRow>> {
        self.query("", "count", n, 0, "app")
    }
}

/// frecency 评分（对齐 Python 版）：`launch_count * exp(-(ln2/30) * age_days)`。
fn frecency(row: &AppRow, now: f64) -> f64 {
    let ts = row.last_seen.or(row.first_seen).unwrap_or(0.0);
    if ts <= 0.0 {
        return 0.0;
    }
    let age_days = ((now - ts) / 86400.0).max(0.0);
    (row.launch_count as f64) * (-(std::f64::consts::LN_2 / 30.0) * age_days).exp()
}

pub fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn today_start_ts() -> f64 {
    use chrono::{Local, TimeZone};
    let now = Local::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| now.naive_local());
    Local
        .from_local_datetime(&start)
        .single()
        .map(|dt| dt.timestamp() as f64)
        .unwrap_or(0.0)
}
