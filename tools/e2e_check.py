"""端到端验收：以交付形态（dist\\ExeTrace.exe）跑一遍真实链路。

验证四件事：
  1. GUI 窗口真的出现（枚举顶层窗口标题）
  2. 实时监控能捕捉「用户打开了一个应用」（用测试 GUI 进程当靶子）
  3. 记录能落库并被查询
  4. 数据落在指定的数据目录（EXETRACE_DATA_DIR 覆盖生效），不污染真实历史库

用法（构建 venv 内）：python tools/e2e_check.py
退出码 0 = 全部通过。
"""
from __future__ import annotations

import ctypes
import hashlib
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "dist" / "ExeTrace.exe"
SRC_MODE = os.environ.get("EXETRACE_E2E_SRC") == "1"
SRC_PYTHONW = ROOT / ".venv-build" / "Scripts" / "pythonw.exe"
sys.path.insert(0, str(ROOT / "src"))


def cli_command(*args: str) -> list[str]:
    """被测应用的命令行；EXETRACE_E2E_SRC=1 时跑源码版（免打包快速迭代）。"""
    if SRC_MODE and SRC_PYTHONW.exists():
        return [str(SRC_PYTHONW), str(ROOT / "src" / "main.py"), *args]
    return [str(EXE), *args]

import shortcut  # noqa: E402  （桌面路径取真实 Known Folder）
PYTHONW = Path(sys.executable).with_name("pythonw.exe")

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.GetWindowTextLengthW.argtypes = [wintypes.HWND]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.EnumWindows.restype = wintypes.BOOL

_TEST_GUI = """
import ctypes
import os
import tkinter as tk
from pathlib import Path

Path({pidfile!r}).write_text(str(os.getpid()))

user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32

root = tk.Tk()
root.title({title!r})
root.geometry("340x140+120+120")
tk.Label(root, text="ExeTrace e2e target", font=("Segoe UI", 12)).pack(padx=24, pady=24)
root.update()

# 强制把窗口置为前台：产品对「新进程算不算被用户打开」的判据是
# 「拥有前台窗口 / 由 explorer 启动」；测试环境没有真实用户点击
# （点击本身才给进程前台资格），用 AttachThreadInput 借前台线程的
# 输入队列绕过前台锁 —— 已验证可让本窗口成为 GetForegroundWindow。
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


def kill(proc: subprocess.Popen) -> None:
    # /T：onefile 是双进程模型（引导进程 + 真正的子进程），必须杀整棵树
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True, check=False)


def kill_all_instances() -> None:
    """清理任何残留实例：残留进程会占住单实例互斥体，让新实例只弹「已在运行」框。"""
    subprocess.run(["taskkill", "/IM", "ExeTrace.exe", "/T", "/F"], capture_output=True, check=False)


def launch_via_explorer(target_exe: str, src: Path, workdir: Path) -> None:
    """模拟「从桌面图标双击启动」：创建 lnk 并交给 explorer.exe 打开。

    这样靶子进程的父进程就是 explorer.exe，命中产品的「由 explorer 启动」判据。
    （另一条「前台窗口」判据在无人值守的测试环境不可靠——Windows 前台锁只给
    当前前台进程链资格，AttachThreadInput 绕过并不稳定，2026-10-05 实测。）
    """
    lnk = workdir / "e2e_launch_target.lnk"
    if lnk.exists():
        lnk.unlink()
    shortcut.create_shortcut(
        str(lnk), target_exe, workdir=str(workdir),
        arguments=f'"{src}"', icon=target_exe, description="ExeTrace E2E shim",
    )
    subprocess.Popen(["explorer.exe", str(lnk)])


def wait_until(predicate, timeout: float, interval: float = 1.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    return None


def main() -> int:
    results: list[tuple[bool, str]] = []

    def check(name: str, passed: bool, detail: str = "") -> None:
        results.append((passed, name))
        print(f"[{'PASS' if passed else 'FAIL'}] {name} {detail}".rstrip())

    if not SRC_MODE and not EXE.exists():
        print(f"缺少交付物: {EXE}（先运行 build.ps1）")
        return 1

    data_dir = Path(tempfile.mkdtemp(prefix="exetrace_e2e_"))
    db_file = data_dir / "history.db"
    log_file = data_dir / "exetrace.log"
    env = {**os.environ, "EXETRACE_DATA_DIR": str(data_dir)}

    lnk_path: str | None = None
    pid_file = data_dir / "e2e_target.pid"
    app_proc = target_proc = None
    try:
        kill_all_instances()
        time.sleep(1.0)
        shortcut.com_initialize()  # lnk 启动 shim 需要 COM（幂等）

        # --minimized：主窗口即使显示也会 focus_force 抢前台，把"前台判据"
        # 的资格从靶子手里夺走（Windows 只允许前台进程链上的新窗口激活）。
        # 收起主窗口 = 不抢前台，且这正是开机自启的真实形态。
        app_proc = subprocess.Popen(cli_command("--minimized"), env=env)
        # 精确匹配主窗口标题：单实例提示框的标题只有 "ExeTrace"，不能用松匹配
        titles = wait_until(lambda: window_titles_containing("应用历史定位器"), timeout=40)
        check("gui_window", bool(titles), f"titles={titles}")

        check("data_dir_override", db_file.exists(), str(db_file))

        # 等监控的「首轮基线」落定后再启动靶子：基线之前就在运行的进程会被
        # 视为"本来就在跑"（这是产品的正确设计），早启动会造成采样时机假红。
        time.sleep(6)

        # 靶子：一个带可见窗口的测试应用（pythonw 不在系统目录，应被记录）
        # 经 lnk + explorer 启动 —— 父进程为 explorer.exe，命中产品的启动判据
        target_src = data_dir / "e2e_target.py"
        title = f"ExeTrace E2E Target {os.getpid()}"
        target_src.write_text(_TEST_GUI.format(title=title, pidfile=str(pid_file)), encoding="utf-8")
        launch_via_explorer(str(PYTHONW), target_src, data_dir)
        target_proc = None
        wait_until(lambda: pid_file.exists() and pid_file.read_text(encoding="utf-8").strip(), timeout=20)
        target_window = wait_until(lambda: window_titles_containing(title), timeout=20)
        check("target_window", bool(target_window), f"title={target_window}")

        # 精确匹配测试靶子的完整路径。两个坑：
        #   1) 注册表种子里可能已有别的 pythonw.exe，LIKE '%pythonw.exe' 会被种子抢答（假绿）；
        #   2) venv 的 python(w).exe 是 redirector，会重启基础解释器进程，
        #      所以进程表里看到的 exe 是 base_prefix 下的 pythonw.exe。
        target_exe = os.path.abspath(os.path.join(sys.base_prefix, "pythonw.exe"))

        def _recorded():
            if not db_file.exists():
                return None
            try:
                con = sqlite3.connect(f"file:{db_file}?mode=ro", uri=True)
                try:
                    row = con.execute(
                        "SELECT launch_count, last_seen FROM apps WHERE path = ?", (target_exe,)
                    ).fetchone()
                    hits = con.execute(
                        "SELECT path, launch_count FROM apps WHERE path LIKE '%pythonw%'"
                    ).fetchall()
                finally:
                    con.close()
            except sqlite3.Error:
                return None
            if row and (row[0] or 0) >= 1:
                return {"path": target_exe, "count": row[0], "pythonw_rows": hits}
            return None

        row = wait_until(_recorded, timeout=45)
        check("live_record", bool(row), f"row={row}")
        if not row and db_file.exists():
            try:
                con = sqlite3.connect(f"file:{db_file}?mode=ro", uri=True)
                try:
                    hits = con.execute(
                        "SELECT path, launch_count, source FROM apps WHERE path LIKE '%pythonw%'"
                    ).fetchall()
                    total = con.execute("SELECT count(*) FROM apps").fetchone()[0]
                finally:
                    con.close()
                print(f"  （诊断）pythonw 相关行: {hits}；apps 总行数: {total}")
            except sqlite3.Error as exc:
                print(f"  （诊断）读库失败: {exc}")

        # v3 核心功能：交付物自己在真实桌面创建快捷方式（结束后清理）
        desktop = shortcut.desktop_dir()
        lnk_name = "ExeTrace V3 E2E 自检"
        lnk_path = os.path.join(desktop, f"{lnk_name}.lnk")
        if os.path.exists(lnk_path):
            os.remove(lnk_path)
        res = subprocess.run(
            cli_command("--make-shortcut", target_exe, "--name", lnk_name),
            capture_output=True, timeout=60,
        )
        check("shortcut_exit_code", res.returncode == 0, f"exit={res.returncode}")
        check("shortcut_created", os.path.isfile(lnk_path), lnk_path)
        if os.path.isfile(lnk_path):
            with open(lnk_path, "rb") as fh:
                blob = fh.read()
            sha = hashlib.sha256(blob).hexdigest()[:12]
            head = blob[:4]
            check("shortcut_magic", head == b"\x4c\x00\x00\x00", f"header={head.hex()} sha={sha}")
            # 用项目的读回接口再核一次目标（独立于创建路径）
            try:
                info = shortcut.read_shortcut(lnk_path)
                same = os.path.normcase(info.get("target", "")) == os.path.normcase(target_exe)
                check("shortcut_readback", same, f"target={info.get('target')}")
            except OSError as exc:
                check("shortcut_readback", False, repr(exc))

        if results and all(p for p, _ in results):
            print(f"\nE2E OK — 临时数据目录: {data_dir}（可删除）")
            return 0

        print("\nE2E FAILED")
        if log_file.exists():
            print("--- exetrace.log (tail) ---")
            print("\n".join(log_file.read_text(encoding="utf-8", errors="replace").splitlines()[-30:]))
        return 1
    finally:
        if pid_file.exists():
            try:
                tpid = int(pid_file.read_text(encoding="utf-8").strip())
                subprocess.run(["taskkill", "/PID", str(tpid), "/T", "/F"], capture_output=True, check=False)
            except (OSError, ValueError):
                pass
        if app_proc is not None:
            kill(app_proc)
        if lnk_path and os.path.exists(lnk_path):
            try:
                os.remove(lnk_path)
                print(f"（已清理测试快捷方式：{lnk_path}）")
            except OSError as exc:
                print(f"（清理失败，请手动删除 {lnk_path}: {exc}）")


if __name__ == "__main__":
    raise SystemExit(main())
