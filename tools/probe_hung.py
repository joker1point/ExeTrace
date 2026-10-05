"""探测 ExeTrace 窗口 hung 状态时间线（只读，IsHungAppWindow + PID 区分）。

用法：python tools/probe_hung.py [探测秒数] [--pid N]
  --pid N   只报告该进程的窗口（隔离多个实例）
"""
from __future__ import annotations

import ctypes
import sys
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.IsHungAppWindow.argtypes = [ctypes.c_void_p]
user32.IsHungAppWindow.restype = ctypes.c_bool
user32.GetWindowTextLengthW.argtypes = [ctypes.c_void_p]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowTextW.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_int]
user32.GetWindowThreadProcessId.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
user32.GetWindowThreadProcessId.restype = ctypes.c_ulong
user32.EnumWindows.restype = ctypes.c_bool


def windows_matching(needle: str) -> list[tuple[int, int, str]]:
    out: list[tuple[int, int, str]] = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def _cb(hwnd, _l):  # noqa: ANN001
        n = user32.GetWindowTextLengthW(hwnd)
        if n > 0:
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(hwnd, buf, n + 1)
            if needle.lower() in buf.value.lower() and "codebuddy" not in buf.value.lower():
                pid = ctypes.c_ulong(0)
                user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                out.append((int(hwnd), int(pid.value), buf.value))
        return True

    user32.EnumWindows(_cb, 0)
    return out


def main() -> int:
    args = [a for a in sys.argv[1:]]
    only_pid = None
    if "--pid" in args:
        i = args.index("--pid")
        only_pid = int(args[i + 1])
        del args[i : i + 2]
    seconds = float(args[0]) if args else 60.0

    t0 = time.time()
    hung_total = 0
    samples = 0
    last_state: dict[int, bool] = {}
    while time.time() - t0 < seconds:
        for hwnd, pid, title in windows_matching("ExeTrace"):
            if only_pid is not None and pid != only_pid:
                continue
            samples += 1
            hung = bool(user32.IsHungAppWindow(hwnd))
            if last_state.get(hwnd) != hung:
                last_state[hwnd] = hung
                if hung:
                    hung_total += 1
                mark = "HUNG  >>>" if hung else "normal   "
                print(f"[{time.time() - t0:6.1f}s] {mark} pid={pid} hwnd={hwnd} '{title}'", flush=True)
        time.sleep(0.25)
    print(f"sampled {samples}, hung_events={hung_total}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
