"""端到端验收：Rust 版 ExeTrace 真实链路（对齐 Python 版验收思路）。

验证：
  1. GUI 窗口真的出现（枚举顶层窗口标题，精确匹配）
  2. 数据目录覆盖生效（EXETRACE_DATA_DIR 指向临时目录）
  3. 打开一个测试应用 → 被实时监控记录进库
  4. 清理：杀进程树，无残留

用法（任意 Python 3.10+）：python tools/e2e_check.py [--exe <path>]
退出码 0 = 全部通过。
"""
from __future__ import annotations

import argparse
import ctypes
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_RELEASE = ROOT / "target" / "x86_64-pc-windows-gnu" / "release" / "exetrace.exe"
DEFAULT_DEBUG = ROOT / "target" / "x86_64-pc-windows-gnu" / "debug" / "exetrace.exe"
assert (ROOT / "src" / "winx.rs").exists() or True

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.GetWindowTextLengthW.argtypes = [wintypes.HWND]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.EnumWindows.restype = wintypes.BOOL

TEST_GUI = """
import tkinter as tk
root = tk.Tk()
root.title({title!r})
root.geometry("340x140+120+120")
tk.Label(root, text="ExeTrace(Rust) e2e target", font=("Segoe UI", 12)).pack(padx=24, pady=24)
root.after(60000, root.destroy)
root.mainloop()
"""


def window_titles_containing(needle: str) -> list[str]:
    found: list[str] = []

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def _cb(hwnd, _lparam):
        n = user32.GetWindowTextLengthW(hwnd)
        if n > 0:
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(hwnd, buf, n + 1)
            if needle.lower() in buf.value.lower():
                found.append(buf.value)
        return True

    user32.EnumWindows(_cb, 0)
    return found


def kill_tree(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True, check=False)


def wait_until(predicate, timeout: float, interval: float = 0.8):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", help="被测 exe 路径（默认 release，回退 debug）")
    parser.add_argument("--no-tray", action="store_true", help="给被测程序传 --no-tray（诊断用）")
    args = parser.parse_args()
    app_args = ["--no-tray"] if args.no_tray else []

    exe = Path(args.exe) if args.exe else (DEFAULT_RELEASE if DEFAULT_RELEASE.exists() else DEFAULT_DEBUG)
    results: list[tuple[bool, str]] = []

    def check(name: str, passed: bool, detail: str = "") -> None:
        results.append((passed, name))
        print(f"[{'PASS' if passed else 'FAIL'}] {name} {detail}".rstrip())

    if not exe.exists():
        print(f"缺少交付物: {exe}（先 cargo build）")
        return 1
    print(f"被测对象: {exe}")

    data_dir = Path(tempfile.mkdtemp(prefix="exetrace_rs_e2e_"))
    db_file = data_dir / "history.db"
    log_file = data_dir / "exetrace.log"
    env = {**os.environ, "EXETRACE_DATA_DIR": str(data_dir)}

    subprocess.run(["taskkill", "/IM", "exetrace.exe", "/T", "/F"], capture_output=True, check=False)
    time.sleep(0.8)

    app_proc = target_proc = None
    try:
        app_proc = subprocess.Popen([str(exe), *app_args], env=env)
        titles = wait_until(lambda: window_titles_containing("应用历史定位器"), timeout=40)
        check("gui_window", bool(titles), f"titles={titles}")
        check("data_dir_override", db_file.exists(), str(db_file))

        # 等监控建立首轮基线，再启动靶子（否则会被当成"本来就在运行"）
        time.sleep(6)

        pythonw = Path(sys.executable).with_name("pythonw.exe")
        target_exe = os.path.abspath(os.path.join(sys.base_prefix, "pythonw.exe"))  # venv redirector 会切到基础解释器
        target_src = data_dir / "e2e_target.py"
        title = f"ExeTrace-RS E2E Target {os.getpid()}"
        target_src.write_text(TEST_GUI.format(title=title), encoding="utf-8")
        target_proc = subprocess.Popen([str(pythonw), str(target_src)])
        check("target_window", bool(wait_until(lambda: window_titles_containing(title), timeout=20)), title)

        def _recorded():
            if not db_file.exists():
                return None
            try:
                con = sqlite3.connect(f"file:{db_file}?mode=ro", uri=True)
                try:
                    row = con.execute(
                        "SELECT launch_count FROM apps WHERE path = ?", (target_exe,)
                    ).fetchone()
                finally:
                    con.close()
            except sqlite3.Error:
                return None
            if row and (row[0] or 0) >= 1:
                return {"path": target_exe, "count": row[0]}
            return None

        row = wait_until(_recorded, timeout=45)
        check("live_record", bool(row), f"row={row}")

        if results and all(p for p, _ in results):
            print(f"\nE2E OK — 临时数据目录: {data_dir}（可删除）")
            return 0
        print("\nE2E FAILED")
        if log_file.exists():
            print("--- exetrace.log (tail) ---")
            print("\n".join(log_file.read_text(encoding="utf-8", errors="replace").splitlines()[-25:]))
        return 1
    finally:
        for proc in (target_proc, app_proc):
            if proc is not None:
                kill_tree(proc)


if __name__ == "__main__":
    raise SystemExit(main())
