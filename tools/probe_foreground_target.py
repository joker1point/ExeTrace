"""观测：e2e 同款靶子窗口能否成为前台（前台判据的可行性事实核查）。

用法：.venv-build\\Scripts\\python.exe tools\\probe_foreground_target.py
"""
from __future__ import annotations

import ctypes
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes
from pathlib import Path

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.GetForegroundWindow.restype = wintypes.HWND
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD

PYTHONW = Path(sys.executable).with_name("pythonw.exe")


def fg_pid() -> int | None:
    hwnd = user32.GetForegroundWindow()
    if not hwnd:
        return None
    pid = wintypes.DWORD(0)
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return int(pid.value) or None


def main() -> int:
    print("PYTHONW =", PYTHONW, flush=True)
    td = Path(tempfile.mkdtemp(prefix="fg_probe_"))
    src = td / "t.py"
    src.write_text(
        "import ctypes\n"
        "import tkinter as tk\n"
        "user32 = ctypes.windll.user32\n"
        "kernel32 = ctypes.windll.kernel32\n"
        "root = tk.Tk()\n"
        "root.title('FG Probe Target')\n"
        "root.geometry('340x140+120+120')\n"
        "tk.Label(root, text='probe').pack(padx=24, pady=24)\n"
        "root.update()\n"
        "hwnd = int(root.wm_frame(), 16)\n"
        "fg = user32.GetForegroundWindow()\n"
        "fg_thread = user32.GetWindowThreadProcessId(fg, None)\n"
        "my_thread = kernel32.GetCurrentThreadId()\n"
        "user32.AttachThreadInput(fg_thread, my_thread, True)\n"
        "try:\n"
        "    user32.SetForegroundWindow(hwnd)\n"
        "    user32.SetFocus(hwnd)\n"
        "finally:\n"
        "    user32.AttachThreadInput(fg_thread, my_thread, False)\n"
        "root.after(30000, root.destroy)\n"
        "root.mainloop()\n",
        encoding="utf-8",
    )
    p = subprocess.Popen([str(PYTHONW), str(src)])
    print("target pid =", p.pid, flush=True)
    t0 = time.time()
    seen = False
    while time.time() - t0 < 8:
        fg = fg_pid()
        mark = "  <<< TARGET 在前台" if fg == p.pid else ""
        print(f"[{time.time() - t0:5.2f}s] fg_pid={fg}{mark}", flush=True)
        if fg == p.pid:
            seen = True
        time.sleep(0.4)
    print(
        "RESULT:",
        "TARGET-REACHED-FOREGROUND" if seen else "TARGET-NEVER-FOREGROUND",
        flush=True,
    )
    subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True, check=False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
