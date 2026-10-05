"""历史库：SQLite 存储层（多线程安全，单连接 + 互斥锁）。

使用「rev」自增版本号驱动 UI 增量刷新：写入方 bump，UI 侧轮询 rev
变化才重建列表 —— 避免每 N 秒无条件刷新导致的选中丢失/滚动跳动。
"""
from __future__ import annotations

import math
import os
import sqlite3
import threading
import time
from dataclasses import dataclass
from typing import Iterable, Sequence

import paths

SCHEMA = """
CREATE TABLE IF NOT EXISTS apps (
    path         TEXT PRIMARY KEY COLLATE NOCASE,
    name         TEXT NOT NULL,
    launch_count INTEGER NOT NULL DEFAULT 0,
    first_seen   REAL,
    last_seen    REAL,
    source       TEXT NOT NULL DEFAULT 'scan',
    pinned       INTEGER NOT NULL DEFAULT 0,
    total_seconds REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_apps_last_seen ON apps(last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_apps_count     ON apps(launch_count DESC);
"""


@dataclass
class AppRow:
    path: str
    name: str
    launch_count: int
    first_seen: float | None
    last_seen: float | None
    source: str
    pinned: bool = False
    total_seconds: float = 0.0
    exists: bool = True


class Store:
    def __init__(self, db_file: str | None = None):
        self._file = db_file or str(paths.db_path())
        self._lock = threading.RLock()
        self._rev = 0
        self._conn = sqlite3.connect(self._file, check_same_thread=False)
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA synchronous=NORMAL")
        with self._lock, self._conn:
            self._conn.executescript(SCHEMA)
            self._migrate()

    def _migrate(self) -> None:
        """幂等迁移：老库补 pinned 列（ALTER 前先查列是否存在，重跑不炸）。"""
        cols = {row[1] for row in self._conn.execute("PRAGMA table_info(apps)")}
        if "pinned" not in cols:
            self._conn.execute("ALTER TABLE apps ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0")
        if "total_seconds" not in cols:
            self._conn.execute("ALTER TABLE apps ADD COLUMN total_seconds REAL NOT NULL DEFAULT 0")

    def mark_pinned(self, exe_path: str) -> None:
        """记录「已为其创建过桌面快捷方式」。"""
        with self._lock, self._conn:
            self._conn.execute(
                "UPDATE apps SET pinned = 1 WHERE path = ?", (os.path.abspath(exe_path),)
            )
        self._bump()

    def add_usage(self, seconds_by_path: dict[str, float]) -> None:
        """给若干路径累加使用时长（秒）。路径不在库中则忽略。"""
        with self._lock, self._conn:
            for path, secs in seconds_by_path.items():
                if secs > 0:
                    self._conn.execute(
                        "UPDATE apps SET total_seconds = total_seconds + ? WHERE path = ?",
                        (float(secs), path),
                    )
        self._bump()

    # ---------------------------------------------------------- 版本号

    @property
    def rev(self) -> int:
        return self._rev

    def _bump(self) -> None:
        self._rev += 1

    # ---------------------------------------------------------- 写入

    def record_launch(self, exe_path: str, source: str = "watch", dedupe_window: float = 300.0) -> bool:
        """记录一次"打开"。同一路径在 dedupe_window 秒内重复出现只刷新时间、不涨计数
        （应用常会派生多个同源进程，Chrome 这类会一次冒出十几个）。"""
        exe_path = os.path.abspath(exe_path)
        name = os.path.splitext(os.path.basename(exe_path))[0]
        now = time.time()
        with self._lock, self._conn:
            row = self._conn.execute(
                "SELECT launch_count, last_seen FROM apps WHERE path = ?", (exe_path,)
            ).fetchone()
            if row is None:
                self._conn.execute(
                    "INSERT INTO apps(path, name, launch_count, first_seen, last_seen, source) "
                    "VALUES(?, ?, 1, ?, ?, ?)",
                    (exe_path, name, now, now, source),
                )
                self._bump()
                return True
            last_seen = row[1]
            if last_seen and (now - last_seen) < dedupe_window:
                self._conn.execute("UPDATE apps SET last_seen = ? WHERE path = ?", (now, exe_path))
                self._bump()
                return False
            self._conn.execute(
                "UPDATE apps SET launch_count = launch_count + 1, last_seen = ?, source = ? WHERE path = ?",
                (now, source, exe_path),
            )
            self._bump()
            return True

    def upsert_seed(
        self,
        exe_path: str,
        launch_count: int | None = None,
        last_seen: float | None = None,
    ) -> None:
        """冷启动扫描得到的种子记录：计数取最大值（不叠加），时间取较新者。"""
        exe_path = os.path.abspath(exe_path)
        name = os.path.splitext(os.path.basename(exe_path))[0]
        cnt = int(launch_count) if launch_count else 0
        with self._lock, self._conn:
            self._conn.execute(
                """
                INSERT INTO apps(path, name, launch_count, first_seen, last_seen, source)
                VALUES(?, ?, ?, NULL, ?, 'scan')
                ON CONFLICT(path) DO UPDATE SET
                    name         = excluded.name,
                    launch_count = MAX(launch_count, excluded.launch_count),
                    last_seen    = MAX(COALESCE(last_seen, 0), COALESCE(excluded.last_seen, 0)),
                    source       = CASE WHEN apps.source = 'watch' THEN 'watch' ELSE 'scan' END
                """,
                (exe_path, name, cnt, last_seen),
            )
        self._bump()

    def remove(self, exe_path: str) -> None:
        with self._lock, self._conn:
            self._conn.execute("DELETE FROM apps WHERE path = ?", (os.path.abspath(exe_path),))
        self._bump()

    def prune_missing(self) -> int:
        """删除磁盘上已不存在的记录，返回删除条数。"""
        removed = 0
        with self._lock, self._conn:
            rows = self._conn.execute("SELECT path FROM apps").fetchall()
            for (p,) in rows:
                if not os.path.exists(p):
                    self._conn.execute("DELETE FROM apps WHERE path = ?", (p,))
                    removed += 1
        if removed:
            self._bump()
        return removed

    # ---------------------------------------------------------- 读取

    # frecency（"智能"排序）：频次 × 时间衰减，λ=ln2/30（半衰期 30 天，
    # 公式出处：Firefox urlbar 排序文档）。聚合数据下逐次访问不可考，
    # 用 launch_count 近似频次、last_seen 近似最近访问。
    _FRECENCY_LAMBDA = 0.6931471805599453 / 30.0

    def _frecency(self, row: AppRow, now: float) -> float:
        ts = row.last_seen or row.first_seen or 0.0
        if not ts:
            return 0.0
        age_days = max(0.0, (now - ts) / 86400.0)
        return float(row.launch_count or 0) * math.exp(-self._FRECENCY_LAMBDA * age_days)

    def query(self, keyword: str = "", sort: str = "recent", limit: int = 2000) -> list[AppRow]:
        where, args = "", []
        kw = keyword.strip()
        if kw:
            like = f"%{kw.replace('%', r'\%').replace('_', r'\_')}%"
            where = r"WHERE (name LIKE ? ESCAPE '\' OR path LIKE ? ESCAPE '\')"
            args = [like, like]
        if sort == "smart":
            # frecency 需逐行算分 → 放 Python 侧排序（量级 ~千行，成本可忽略）
            with self._lock:
                rows: Sequence[tuple] = self._conn.execute(
                    f"SELECT path, name, launch_count, first_seen, last_seen, source, pinned, total_seconds "
                    f"FROM apps {where}",
                    args,
                ).fetchall()
            now = time.time()
            apps = [AppRow(*r) for r in rows]
            apps.sort(key=lambda r: (-self._frecency(r, now), r.name.lower()))
            return apps[:limit]
        order = {
            "recent": "ORDER BY COALESCE(last_seen, 0) DESC, name COLLATE NOCASE",
            "count": "ORDER BY launch_count DESC, COALESCE(last_seen, 0) DESC",
            "name": "ORDER BY name COLLATE NOCASE, path COLLATE NOCASE",
        }.get(sort, "ORDER BY COALESCE(last_seen, 0) DESC")
        with self._lock:
            rows: Sequence[tuple] = self._conn.execute(
                f"SELECT path, name, launch_count, first_seen, last_seen, source, pinned, total_seconds "
                f"FROM apps {where} {order} LIMIT ?",
                (*args, limit),
            ).fetchall()
        return [AppRow(*r) for r in rows]

    def total(self) -> int:
        with self._lock:
            return int(self._conn.execute("SELECT COUNT(*) FROM apps").fetchone()[0])

    def count_today(self) -> int:
        import datetime as _dt

        start = _dt.datetime.now().replace(hour=0, minute=0, second=0, microsecond=0).timestamp()
        with self._lock:
            return int(
                self._conn.execute("SELECT COUNT(*) FROM apps WHERE last_seen >= ?", (start,)).fetchone()[0]
            )

    def all_paths(self) -> list[str]:
        with self._lock:
            return [r[0] for r in self._conn.execute("SELECT path FROM apps").fetchall()]

    def close(self) -> None:
        with self._lock:
            try:
                self._conn.close()
            except Exception:
                pass
