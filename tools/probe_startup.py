"""复现冷启动 hung 时间线：源码版 + 真实库副本（不打扰正在运行的实例）。

说明：
- 用 .venv-build 的 pythonw.exe 启动（有第三方依赖）；它是 redirector，
  实体进程 PID 与 Popen 返回的不同 —— 因此用「排除已有 ExeTrace 窗口 PID」
  的方式动态识别测试实例。
- 窗口按 PID 过滤后采样 IsHungAppWindow + CPU。

用法：.venv-build\\Scripts\\python.exe tools\\probe_startup.py [探测秒数]
"""
from __future__ import annotations

import ctypes
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import psutil

ROOT = Path(__file__).resolve().parents[1]
SRC_DATA = Path(os.environ["LOCALAPPDATA"]) / "ExeTrace"
PYW = ROOT / ".venv-build" / "Scripts" / "pythonw.exe"

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.IsHungAppWindow.argtypes = [ctypes.c_void_p]
user32.IsHungAppWindow.restype = ctypes.c_bool
user32.GetWindowTextLengthW.argtypes = [ctypes.c_void_p]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowTextW.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_int]
user32.GetWindowThreadProcessId.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
user32.GetWindowThreadProcessId.restype = ctypes.c_ulong
user32.EnumWindows.restype = ctypes.c_bool


def exetrace_windows() -> list[tuple[int, int, str]]:
    out: list[tuple[int, int, str]] = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def _cb(hwnd, _l):  # noqa: ANN001
        n = user32.GetWindowTextLengthW(hwnd)
        if n > 0:
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(hwnd, buf, n + 1)
            if "exetrace" in buf.value.lower() and "codebuddy" not in buf.value.lower():
                p = ctypes.c_ulong(0)
                user32.GetWindowThreadProcessId(hwnd, ctypes.byref(p))
                out.append((int(hwnd), int(p.value), buf.value))
        return True

    user32.EnumWindows(_cb, 0)
    return out


def main() -> int:
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 90.0

    existing = {pid for _hwnd, pid, _t in exetrace_windows()}
    print(f"已有 ExeTrace 窗口的 pid（将排除）: {existing}", flush=True)

    dst = Path(tempfile.mkdtemp(prefix="exetrace_startup_"))
    for name in ("history.db", "history.db-wal", "history.db-shm"):
        s = SRC_DATA / name
        if s.exists():
            shutil.copy2(s, dst / name)
    print(f"repro dir: {dst}", flush=True)

    env = {
        **os.environ,
        "EXETRACE_DATA_DIR": str(dst),
        "EXETRACE_MUTEX_NAME": f"Local\\ExeTrace_Probe_{os.getpid()}",
    }
    proc = subprocess.Popen([str(PYW), str(ROOT / "src" / "main.py")], env=env, cwd=str(ROOT))
    print(f"launcher pid: {proc.pid}", flush=True)

    t0 = time.time()
    last_hung: dict[int, bool] = {}
    app_pid: int | None = None
    ps: psutil.Process | None = None
    first_window_t: float | None = None
    last_cpu_report = 0.0
    hung_events = 0

    while time.time() - t0 < seconds and proc.poll() is None:
        now = time.time() - t0
        for hwnd, pid, title in exetrace_windows():
            if pid in existing:
                continue
            if app_pid is None:
                app_pid = pid
                ps = psutil.Process(pid)
                print(f"[{now:6.1f}s] 测试实例窗口出现: pid={pid} '{title}'", flush=True)
            if first_window_t is None:
                first_window_t = now
            hung = bool(user32.IsHungAppWindow(hwnd))
            if last_hung.get(hwnd) != hung:
                last_hung[hwnd] = hung
                if hung:
                    hung_events += 1
                mark = "HUNG  >>>" if hung else "normal   "
                print(f"[{now:6.1f}s] {mark} hwnd={hwnd} '{title}'", flush=True)
        if ps is not None and now - last_cpu_report >= 5.0:
            last_cpu_report = now
            try:
                ct = ps.cpu_times()
                ws = ps.memory_info().rss / 1024 / 1024
                print(f"[{now:6.1f}s] cpu={ct.user + ct.system:6.1f}s ws={ws:6.1f}MB threads={ps.num_threads()}", flush=True)
            except psutil.Error:
                print(f"[{now:6.1f}s] (进程已退出)", flush=True)
                break
        time.sleep(0.2)

    print(f"\nhung 事件总数: {hung_events}", flush=True)
    if app_pid is not None:
        subprocess.run(["taskkill", "/PID", str(app_pid), "/T", "/F"], capture_output=True, check=False)
    subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True, check=False)

    log = dst / "exetrace.log"
    if log.exists():
        print("--- 应用日志 ---", flush=True)
        print(log.read_text(encoding="utf-8", errors="replace"), flush=True)
    else:
        print("（无应用日志——进程可能未启动成功）", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
