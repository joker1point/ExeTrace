"""诊断脚本：查看某个 ExeTrace 数据目录的内容（库 + 日志 + WAL 状态）。

用法：python tools/diag_db.py <data_dir>
"""
from __future__ import annotations

import sqlite3
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: python tools/diag_db.py <data_dir>")
        return 2
    d = Path(sys.argv[1])
    if not d.is_dir():
        print(f"目录不存在: {d}")
        return 2

    print("=== dir ===")
    for p in sorted(d.iterdir()):
        print(f"  {p.name}  {p.stat().st_size} bytes")

    log = d / "exetrace.log"
    if log.exists():
        print("=== log ===")
        print(log.read_text(encoding="utf-8", errors="replace"))

    db = d / "history.db"
    if db.exists():
        print("=== db ===")
        con = sqlite3.connect(str(db))
        print("total:", con.execute("select count(*) from apps").fetchone()[0])
        rows = con.execute(
            "select path, launch_count, source from apps where source='watch'"
        ).fetchall()
        print("watch rows:", rows)
        rows2 = con.execute(
            "select path, launch_count, source from apps where path like ?", ("%pythonw%",)
        ).fetchall()
        print("pythonw rows:", rows2)
        con.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
