//! SQLite 存储层 —— 与 Python 版**共用同一 schema**，既有历史无缝衔接。

use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS apps (
    path         TEXT PRIMARY KEY COLLATE NOCASE,
    name         TEXT NOT NULL,
    launch_count INTEGER NOT NULL DEFAULT 0,
    first_seen   REAL,
    last_seen    REAL,
    source       TEXT NOT NULL DEFAULT 'scan'
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
        Ok(Self { conn, rev: 0 })
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
            if !std::path::Path::new(&p).is_file() {
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

    pub fn query(&self, keyword: &str, sort: &str, limit: i64) -> rusqlite::Result<Vec<AppRow>> {
        let kw = keyword.trim();
        let order = match sort {
            "count" => "ORDER BY launch_count DESC, COALESCE(last_seen, 0) DESC",
            "name" => "ORDER BY name COLLATE NOCASE, path COLLATE NOCASE",
            _ => "ORDER BY COALESCE(last_seen, 0) DESC, name COLLATE NOCASE",
        };
        let sql = format!(
            "SELECT path, name, launch_count, first_seen, last_seen, source FROM apps
             WHERE (?1 = '' OR name LIKE ?2 ESCAPE '\\' OR path LIKE ?2 ESCAPE '\\')
             {order} LIMIT ?3"
        );
        let like = format!(
            "%{}%",
            kw.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![kw, like, limit], |r| {
            Ok(AppRow {
                path: r.get(0)?,
                name: r.get(1)?,
                launch_count: r.get(2)?,
                first_seen: r.get(3)?,
                last_seen: r.get(4)?,
                source: r.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn total(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM apps", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn count_today(&self) -> i64 {
        let start = today_start_ts();
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM apps WHERE last_seen >= ?1",
                params![start],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// 按启动次数排序取前 N（用于「Dashboard」式统计，暂未使用也保留接口）。
    pub fn top_by_count(&self, n: i64) -> rusqlite::Result<Vec<AppRow>> {
        self.query("", "count", n)
    }
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
