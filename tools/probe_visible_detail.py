"""逐窗口剖析（多轮覆盖忙期）：visible_window_pids 的阻塞来自哪个窗口/进程。

用法：.venv-build\\Scripts\\python.exe tools\\probe_visible_detail.py
"""
from __future__ import annotations

import ctypes
import os
import sys
import threading
import time
import tkinter as tk
from ctypes import wintypes
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

import winutil  # noqa: E402

user32 = winutil.user32
self_pid = os.getpid()
print("self pid =", self_pid, flush=True)

root = tk.Tk()
root.title("BlockProbe Detail Window")
root.geometry("240x120")

busy_flag = [False]
results: dict = {}
_done = threading.Event()


def probe() -> None:
    rounds = []
    for i in range(11):
        t0 = time.perf_counter()
        wins: list[tuple[int, int]] = []

        @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        def _cb(hwnd, _l):  # noqa: ANN001
            if user32.IsWindowVisible(hwnd):
                pid = wintypes.DWORD(0)
                user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                wins.append((int(hwnd), int(pid.value)))
            return True

        user32.EnumWindows(_cb, 0)
        enum_cost = time.perf_counter() - t0

        self_windows: list[tuple] = []
        slow: list[tuple] = []
        for hwnd, pid in wins:
            t1 = time.perf_counter()
            n = winutil._safe_title_length(hwnd)
            dt = time.perf_counter() - t1
            if pid == self_pid:
                self_windows.append((hwnd, round(dt, 3), n))
            if dt > 0.05:
                slow.append((hwnd, pid, round(dt, 3), pid == self_pid))
        rounds.append({
            "i": i,
            "busy": busy_flag[0],
            "enum": round(enum_cost, 3),
            "total": round(time.perf_counter() - t0, 3),
            "self": self_windows,
            "slow": slow,
        })
        time.sleep(0.4)
    results["rounds"] = rounds
    _done.set()


def busy_task() -> None:
    busy_flag[0] = True
    print(">>> 主线程忙 5 秒（不泵消息）", flush=True)
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < 5.0:
        pass
    busy_flag[0] = False
    print(f">>> 忙结束（{time.perf_counter() - t0:.2f}s）", flush=True)
    root.after(400, root.destroy)


th = threading.Thread(target=probe, daemon=True, name="probe")
th.start()
root.after(1200, busy_task)
root.after(25000, root.destroy)
root.mainloop()

_done.wait(timeout=10)
print()
print(f"{'i':>2} {'busy':>4} {'enum':>6} {'total':>7}  self_windows(hwnd,dt,len)   slow(hwnd,pid,dt,is_self)")
for r in results.get("rounds", []):
    print(f"{r['i']:>2} {str(r['busy']):>5} {r['enum']:>6} {r['total']:>7}  {r['self']}  {r['slow']}")
