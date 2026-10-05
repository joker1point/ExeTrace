"""诊断 watcher 记录链路：主库旁证 + 靶子三条件 + 源码版 watcher 冒烟。

用法（构建 venv 内）：python tools/diag_watch.py
"""
from __future__ import annotations

import os
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

import psutil  # noqa: E402

import winutil  # noqa: E402
from store import Store  # noqa: E402
from watcher import ProcessWatcher  # noqa: E402

print("=== 主库旁证（真实使用下 watcher 是否工作）===")
main_db = Path(os.environ["LOCALAPPDATA"]) / "ExeTrace" / "history.db"
print("main db:", main_db, "exists:", main_db.exists())
if main_db.exists():
    try:
        con = sqlite3.connect(f"file:{main_db}?mode=ro", uri=True)
        try:
            n_watch = con.execute("SELECT count(*) FROM apps WHERE source='watch'").fetchone()[0]
            rows = con.execute(
                "SELECT path, launch_count, datetime(last_seen,'unixepoch','localtime') "
                "FROM apps WHERE source='watch' ORDER BY last_seen DESC LIMIT 5"
            ).fetchall()
            print("watch rows:", n_watch)
            for r in rows:
                print("  ", r)
        finally:
            con.close()
    except Exception as exc:  # noqa: BLE001
        print("read main db failed:", exc)

print()
print("=== 靶子三条件 ===")
PYTHONW = Path(sys.base_prefix) / "pythonw.exe"  # 与 e2e 的 target_exe 一致（无 venv redirector）
print("sys.executable =", sys.executable)
print("sys.base_prefix =", sys.base_prefix)
print("PYTHONW =", PYTHONW, "exists:", PYTHONW.exists())

td = Path(tempfile.mkdtemp(prefix="diag_watch_"))
tgt_src = td / "t.py"
tgt_src.write_text(
    "import tkinter as tk\n"
    "root = tk.Tk()\n"
    "root.title('DiagWatcher Target 4242')\n"
    "root.geometry('300x120')\n"
    "root.after(30000, root.destroy)\n"
    "root.mainloop()\n",
    encoding="utf-8",
)

# ---- 源码版 watcher 冒烟（先建基线，再放靶子）----
db = td / "t.db"
st = Store(str(db))
w = ProcessWatcher(st, interval=1.0)
w.start()
time.sleep(3)  # 让首轮基线落定
print("watcher primed; known =", len(w._known))

p = subprocess.Popen([str(PYTHONW), str(tgt_src)])
print("target pid:", p.pid)
try:
    deadline = time.time() + 25
    found = None
    while time.time() < deadline:
        time.sleep(1.5)
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        rows = con.execute("SELECT path, launch_count, source FROM apps").fetchall()
        con.close()
        if rows:
            print("rows:", rows)
        if any("pythonw" in (row[0] or "").lower() for row in rows):
            found = rows
            break

    # 逐条件输出（无论是否记录都打，便于对照）
    info = None
    for proc in psutil.process_iter(["pid", "exe", "ppid"]):
        try:
            if proc.info["pid"] == p.pid:
                info = dict(proc.info)
                break
        except psutil.Error:
            continue
    print("1) psutil info for target pid:", info)
    if info and info.get("exe"):
        print("   is_noise_exe(target exe) =", winutil.is_noise_exe(info["exe"]))
    vis = winutil.visible_window_pids()
    print("2) target pid in visible_window_pids:", p.pid in vis, "| visible total:", len(vis))
    print("3) pending now:", {k: v.get("exe") for k, v in w._pending.items()})
    print("RESULT:", "RECORDED" if found else "NOT RECORDED")
    print("watcher.recorded_count =", w.recorded_count, "| last_error =", w.last_error or "-")
finally:
    w.stop()
    st.close()
    subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True)
    print("(target killed)")
