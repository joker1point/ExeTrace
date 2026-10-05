"""可行性验证：Python(ctypes) 用 SetWinEventHook 监听前台窗口变化。

为「事件驱动替代 2 秒轮询」的迁移评估三点：
  1. 事件回调是否稳定（连续切换窗口不漏事件）
  2. 回调里能否拿到前台进程 PID（接记录逻辑）
  3. 处理器空闲时的 CPU 成本

关键实现点：SetWinEventHook 与 GetMessage 消息泵必须在同一线程
（回调经该线程的消息队列投递）。

用法：.venv-build\\Scripts\\python.exe tools\\probe_wineventhook.py [秒数]
"""
from __future__ import annotations

import ctypes
import sys
import threading
import time
from ctypes import wintypes

import psutil

user32 = ctypes.WinDLL("user32", use_last_error=True)

EVENT_SYSTEM_FOREGROUND = 0x0003
WINEVENT_OUTOFCONTEXT = 0x0000

WinEventProc = ctypes.WINFUNCTYPE(
    None, wintypes.HANDLE, wintypes.DWORD, wintypes.HWND,
    wintypes.LONG, wintypes.LONG, wintypes.DWORD, wintypes.DWORD,
)

user32.SetWinEventHook.argtypes = [
    wintypes.DWORD, wintypes.DWORD, wintypes.HMODULE, WinEventProc,
    wintypes.DWORD, wintypes.DWORD, wintypes.DWORD,
]
user32.SetWinEventHook.restype = wintypes.HANDLE
user32.UnhookWinEvent.argtypes = [wintypes.HANDLE]
user32.UnhookWinEvent.restype = wintypes.BOOL
user32.GetMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND, wintypes.UINT, wintypes.UINT]
user32.GetMessageW.restype = ctypes.c_int
user32.TranslateMessage.argtypes = [ctypes.POINTER(wintypes.MSG)]
user32.DispatchMessageW.argtypes = [ctypes.POINTER(wintypes.MSG)]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD

events: list[tuple[float, int]] = []
t_start = time.time()
hook_handle = [None]
ready = threading.Event()


def _callback(_hook, _event, hwnd, _id_object, _id_child, _thread, _ts):  # noqa: ANN001
    if hwnd:
        pid = wintypes.DWORD(0)
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        events.append((time.time() - t_start, int(pid.value)))


def hook_thread() -> None:
    cb = WinEventProc(_callback)
    hook = user32.SetWinEventHook(
        EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND,
        None, cb, 0, 0, WINEVENT_OUTOFCONTEXT,
    )
    hook_handle[0] = hook
    ready.set()
    if not hook:
        return
    msg = wintypes.MSG()
    while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
        user32.TranslateMessage(ctypes.byref(msg))
        user32.DispatchMessageW(ctypes.byref(msg))


def main() -> int:
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 60.0
    th = threading.Thread(target=hook_thread, daemon=True, name="wineventhook")
    th.start()
    ready.wait(timeout=5)
    if not hook_handle[0]:
        print("SetWinEventHook FAILED, GetLastError =", ctypes.get_last_error(), flush=True)
        return 1
    print(f"hook 安装成功；监听 {seconds:.0f}s，请来回切换几个窗口…", flush=True)

    ps = psutil.Process()
    c0 = ps.cpu_times()
    time.sleep(seconds)
    c1 = ps.cpu_times()
    cpu = (c1.user - c0.user) + (c1.system - c0.system)

    print(f"\n事件数: {len(events)}")
    for dt, pid in events[:24]:
        print(f"  [{dt:6.2f}s] 前台 pid={pid}")
    print(f"窗口 {seconds:.0f}s 内进程 CPU 消耗 {cpu:.2f}s（≈{cpu / seconds * 100:.1f}% 单核）")
    if hook_handle[0]:
        user32.UnhookWinEvent(hook_handle[0])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
