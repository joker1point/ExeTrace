"""验证实验（带对照组）：主线程「泵消息」vs「忙不泵消息」时，
另一线程的 visible_window_pids() 是否被本进程自身窗口阻塞。

用法：.venv-build\\Scripts\\python.exe tools\\probe_visible_block.py
"""
from __future__ import annotations

import sys
import threading
import time
import tkinter as tk
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

import winutil  # noqa: E402

root = tk.Tk()
root.title("BlockProbe Main Window")
root.geometry("240x120")

results: list[tuple[int, float, int, str]] = []
_done = threading.Event()


def probe() -> None:
    for i in range(9):
        t0 = time.perf_counter()
        pids = winutil.visible_window_pids()
        dt = time.perf_counter() - t0
        phase = "busy" if busy_flag[0] else "idle"
        results.append((i, round(dt, 3), len(pids), phase))
        time.sleep(0.3)
    _done.set()


busy_flag = [False]  # 0=idle 1=busy


def busy_task() -> None:
    busy_flag[0] = True
    print(">>> 主线程忙 5 秒（不泵消息）", flush=True)
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < 5.0:
        pass
    busy_flag[0] = False
    print(f">>> 忙结束（{time.perf_counter() - t0:.2f}s）", flush=True)
    root.after(200, root.destroy)


th = threading.Thread(target=probe, daemon=True, name="probe")
th.start()

root.after(1200, busy_task)      # 先空闲 1.2s（对照组），再忙 5s
root.after(20000, root.destroy)  # 兜底退出
root.mainloop()

_done.wait(timeout=10)

print("index, seconds, visible_pids, phase")
for i, dt, n, phase in results:
    flag = "  <== 被阻塞" if dt > 1.0 else ""
    print(f"{i:>5}, {dt:>7}, {n:>3}, {phase}{flag}")
blocked = [r for r in results if r[1] > 1.0]
print("VERDICT:", "BLOCKED-CONFIRMED" if blocked else "NO-BLOCK")
