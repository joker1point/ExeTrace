"""一次性清理：复现过程在真实库留下的 pythonw watch 噪音（含一致性备份）。

用法：.venv-build\\Scripts\\python.exe tools\\clean_pythonw_noise.py
"""
from __future__ import annotations

import os
import sqlite3
import time
from pathlib import Path

data = Path(os.environ["LOCALAPPDATA"]) / "ExeTrace"
db = data / "history.db"
backup = data / f"history.db.bak-{time.strftime('%Y%m%d-%H%M%S')}"

src = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
dst = sqlite3.connect(str(backup))
src.backup(dst)
src.close()
dst.close()
print("backup:", backup)

con = sqlite3.connect(str(db))
try:
    before = con.execute(
        "SELECT path, launch_count, source FROM apps WHERE path LIKE '%pythonw%'"
    ).fetchall()
    print("before:")
    for r in before:
        print("  ", r)
    con.execute("DELETE FROM apps WHERE source = 'watch' AND path LIKE '%pythonw.exe'")
    con.commit()
    after = con.execute(
        "SELECT path, source FROM apps WHERE path LIKE '%pythonw%'"
    ).fetchall()
    print("after:")
    for r in after:
        print("  ", r)
finally:
    con.close()
