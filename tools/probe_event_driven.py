"""事件驱动专项验证：短暂前台的窗口（活 1.5s，可能落在两个轮询之间）也能被记录。

用源码版 app + 真实库副本（不打扰正在运行的实例）。

用法：.venv-build\\Scripts\\python.exe tools\\probe_event_driven.py
"""
from __future__ import annotations

import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC_DATA = Path(os.environ["LOCALAPPDATA"]) / "ExeTrace"
PYW = ROOT / ".venv-build" / "Scripts" / "pythonw.exe"

_TARGET = """
import ctypes
import os
import tkinter as tk
from pathlib import Path

Path({pidfile!r}).write_text(str(os.getpid()))

user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32
root = tk.Tk()
root.title({title!r})
root.geometry("320x120+140+140")
tk.Label(root, text="event-driven probe").pack(padx=20, pady=20)
root.update()
hwnd = int(root.wm_frame(), 16)
fg = user32.GetForegroundWindow()
fg_thread = user32.GetWindowThreadProcessId(fg, None)
my_thread = kernel32.GetCurrentThreadId()
user32.AttachThreadInput(fg_thread, my_thread, True)
try:
    user32.SetForegroundWindow(hwnd)
    user32.SetFocus(hwnd)
finally:
    user32.AttachThreadInput(fg_thread, my_thread, False)
root.after(1500, root.destroy)
root.mainloop()
"""


def main() -> int:
    dst = Path(tempfile.mkdtemp(prefix="exetrace_eventdrv_"))
    for name in ("history.db", "history.db-wal", "history.db-shm"):
        s = SRC_DATA / name
        if s.exists():
            shutil.copy2(s, dst / name)
    print("repro dir:", dst, flush=True)

    # 离线核对：靶子 exe 会不会被判为噪音（含用户忽略表）
    os.environ["EXETRACE_DATA_DIR"] = str(dst)
    sys.path.insert(0, str(ROOT / "src"))
    import winutil  # noqa: PLC0415
    target_exe_path = str(Path(sys.base_prefix) / "pythonw.exe")
    print("target exe =", target_exe_path, flush=True)
    print("is_noise_exe(target) =", winutil.is_noise_exe(target_exe_path), flush=True)
    print("user_ignores =", sorted(winutil._user_ignores()), flush=True)
    print("foreground_pid() =", winutil.foreground_pid(), flush=True)

    env = {
        **os.environ,
        "EXETRACE_DATA_DIR": str(dst),
        "EXETRACE_MUTEX_NAME": f"Local\\ExeTrace_EvtProbe_{os.getpid()}",
    }
    app = subprocess.Popen(
        [str(PYW), str(ROOT / "src" / "main.py"), "--minimized"],
        env=env, cwd=str(ROOT),
    )
    time.sleep(8)  # 等基线与扫描

    pid_file = dst / "evt_target.pid"
    src = dst / "evt_target.py"
    title = f"EvtDrv Target {os.getpid()}"
    src.write_text(_TARGET.format(pidfile=str(pid_file), title=title), encoding="utf-8")
    tgt = subprocess.Popen([str(Path(sys.base_prefix) / "pythonw.exe"), str(src)])

    deadline = time.time() + 20
    found = None
    while time.time() < deadline:
        time.sleep(1.0)
        db = dst / "history.db"
        if not db.exists():
            continue
        try:
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            try:
                rows = con.execute(
                    "SELECT path, launch_count FROM apps "
                    "WHERE path LIKE '%pythonw%' AND source='watch'"
                ).fetchall()
            finally:
                con.close()
        except sqlite3.Error:
            rows = []
        if rows:
            found = rows
            break
    print("watch rows:", found, flush=True)
    print("RESULT:", "RECORDED" if found else "NOT-RECORDED", flush=True)

    log = dst / "exetrace.log"
    if log.exists():
        print("--- 应用日志 ---", flush=True)
        print(log.read_text(encoding="utf-8", errors="replace"), flush=True)

    subprocess.run(["taskkill", "/PID", str(app.pid), "/T", "/F"], capture_output=True, check=False)
    if tgt.poll() is None:
        subprocess.run(["taskkill", "/PID", str(tgt.pid), "/T", "/F"], capture_output=True, check=False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
