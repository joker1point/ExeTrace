"""测量库内全部路径的 isfile 耗时，定位慢路径（>0.5s）。

用法：python tools/probe_isfile.py
"""
from __future__ import annotations

import os
import sqlite3
import time
from pathlib import Path

db = Path(os.environ["LOCALAPPDATA"]) / "ExeTrace" / "history.db"
con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
paths = [r[0] for r in con.execute("SELECT path FROM apps ORDER BY path")]
con.close()
print(f"total {len(paths)} paths", flush=True)

slow: list[tuple[float, bool, str]] = []
t_all = time.perf_counter()
for p in paths:
    t0 = time.perf_counter()
    ex = os.path.isfile(p)
    dt = time.perf_counter() - t0
    if dt > 0.5:
        slow.append((dt, ex, p))
        print(f"SLOW {dt:6.2f}s exists={ex} {p}", flush=True)
total = time.perf_counter() - t_all
print(f"done: {len(paths)} checked in {total:.1f}s, {len(slow)} slow(>0.5s)", flush=True)
for dt, ex, p in sorted(slow, reverse=True)[:10]:
    print(f"  {dt:6.2f}s {p}", flush=True)
