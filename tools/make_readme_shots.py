"""为 README 生成 ExeTrace 真实界面截图。

隐私纪律：**不暴露本机真实应用历史**。做法是用 `EXETRACE_DATA_DIR` 指向一个临时数据目录，
在隔离的库里真实记录几个大家机器上都有的通用程序（记事本 / 画图 / 字符映射表），
截完图即删。截的是真窗口、真记录（列表里的程序确实是这次真的打开过的）。

用法：python tools/make_readme_shots.py
要求：Windows + Python 3.10+（Pillow）；需先构建 dist/ExeTrace.exe（或改 EXE 指向源码运行）。
"""
from __future__ import annotations

import ctypes
import os
import shutil
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    print("需要 Pillow：pip install pillow")
    sys.exit(1)

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "screenshots"
EXE = ROOT / "dist" / "ExeTrace.exe"
TITLE = "ExeTrace — 应用历史定位器"      # = src/paths.py 的 APP_TITLE
# 截图里的示例记录用「本机真实存在的常见程序」；缺失的会自动跳过
SEED_CANDIDATES = [
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft VS Code\Code.exe",
    r"C:\Program Files\7-Zip\7zFM.exe",
    r"C:\Windows\System32\notepad.exe",
]

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)
except Exception:
    try:
        user32.SetProcessDPIAware()
    except Exception:
        pass


class RECT(ctypes.Structure):
    _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long),
                ("right", ctypes.c_long), ("bottom", ctypes.c_long)]


def trim(img: Image.Image, tol: int = 14) -> Image.Image:
    """裁掉右侧与底部残边（保留标题栏）。"""
    w, h = img.size
    px = img.load()
    bg = px[3, 3][:3]

    def is_bg(c) -> bool:
        return all(abs(c[i] - bg[i]) <= tol for i in range(3))

    bottom, right = h, w
    for y in range(h - 1, 0, -1):
        if any(not is_bg(px[x, y][:3]) for x in range(0, w, 4)):
            bottom = min(h, y + 8)
            break
    for x in range(w - 1, 0, -1):
        if any(not is_bg(px[x, y][:3]) for y in range(0, h, 4)):
            right = min(w, x + 8)
            break
    return img.crop((0, 0, right, bottom))


def grab_window(hwnd: int) -> Image.Image:
    """抓窗口自身位图（PrintWindow）。"""
    rect = RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    w, h = rect.right - rect.left, rect.bottom - rect.top

    hdc = user32.GetWindowDC(hwnd)
    mem_dc = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    old = gdi32.SelectObject(mem_dc, bmp)
    user32.PrintWindow(hwnd, mem_dc, 2)

    class BMIH(ctypes.Structure):
        _fields_ = [("biSize", wintypes.DWORD), ("biWidth", ctypes.c_long),
                    ("biHeight", ctypes.c_long), ("biPlanes", wintypes.WORD),
                    ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                    ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", ctypes.c_long),
                    ("biYPelsPerMeter", ctypes.c_long), ("biClrUsed", wintypes.DWORD),
                    ("biClrImportant", wintypes.DWORD)]

    bmi = BMIH()
    bmi.biSize = ctypes.sizeof(bmi)
    bmi.biWidth = w
    bmi.biHeight = -h
    bmi.biPlanes = 1
    bmi.biBitCount = 32
    buf = ctypes.create_string_buffer(w * h * 4)
    gdi32.GetDIBits(mem_dc, bmp, 0, h, buf, ctypes.byref(bmi), 0)
    gdi32.SelectObject(mem_dc, old)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem_dc)
    user32.ReleaseDC(hwnd, hdc)
    return Image.frombuffer("RGB", (w, h), buf, "raw", "BGRX", 0, 1)


def find_window(title: str, timeout_s: float = 40.0) -> int | None:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        hwnd = user32.FindWindowW(None, title)
        if hwnd:
            return hwnd
        time.sleep(0.4)
    return None


def reset_db(db: Path, seeds: list[str]) -> tuple[int, int]:
    """清空历史，再写入几条「常见程序」的示例记录（路径都真实存在）。

    必须清空的原因：启动时的「注册表冷启动回填」会把**本机真实使用过的应用**
    全灌进库（313 条，含个人软件痕迹）——那不该出现在公开截图里；
    隔离数据目录只能隔离新记录，隔离不了回填。
    这些种子走产品自己的 Store.upsert_seed，schema 与真实记录完全一致。
    """
    import sqlite3

    sys.path.insert(0, str(ROOT / "src"))
    from store import Store            # noqa: PLC0415

    conn = sqlite3.connect(str(db), timeout=10)
    try:
        before = conn.execute("SELECT COUNT(*) FROM apps").fetchone()[0]
        for _ in range(5):
            try:
                conn.execute("DELETE FROM apps")
                conn.commit()
                break
            except sqlite3.OperationalError:
                time.sleep(1.0)
    finally:
        conn.close()

    st = Store(str(db))
    try:
        base = time.time()
        for i, p in enumerate(seeds):
            st.upsert_seed(p, launch_count=6 + i * 5, last_seen=base - i * 3600)
        st.add_usage({seeds[0]: 7200.0})
    finally:
        st.close()
    return before, len(seeds)


def main() -> int:
    if not EXE.exists():
        print(f"找不到 {EXE}；请先构建（build.ps1）或修改脚本里的 EXE 路径")
        return 1

    OUT.mkdir(parents=True, exist_ok=True)
    data_dir = Path(tempfile.mkdtemp(prefix="exetrace_shot_"))
    env = {**os.environ, "EXETRACE_DATA_DIR": str(data_dir)}
    proc = None
    try:
        print(f"隔离数据目录：{data_dir}")
        proc = subprocess.Popen([str(EXE)], env=env)

        hwnd = find_window(TITLE, 60)
        if not hwnd:
            print("主窗口未出现")
            return 2
        # 等冷启动注册表回填跑完（要清的就是它灌进来的那些）
        time.sleep(8)

        # 隐私 + 干净：清空历史，写入几条「本机真实存在」的常见程序示例记录
        db = data_dir / "history.db"
        if not db.exists():
            print(f"未找到 {db}，中止（库里可能仍含本机真实记录）")
            return 3
        seeds = [p for p in SEED_CANDIDATES if Path(p).exists()]
        if not seeds:
            print("没有可用的种子程序（路径都不存在），中止")
            return 4
        before, after = reset_db(db, seeds)
        print(f"  已清空回填记录 {before} 条，写入示例程序 {after} 条：")
        for p in seeds:
            print(f"    - {p}")

        # 强制 UI 重建（隐藏→呼出会强制刷新列表），否则界面还停在旧数据
        user32.ShowWindow(hwnd, 6)          # SW_MINIMIZE
        time.sleep(1.2)
        user32.ShowWindow(hwnd, 9)          # SW_RESTORE
        user32.SetForegroundWindow(hwnd)
        time.sleep(2.5)

        img = trim(grab_window(hwnd))
        dest = OUT / "exetrace-main.png"
        img.save(dest)
        print(f"[ok] {dest.name}  {img.size[0]}x{img.size[1]}")
        return 0
    finally:
        if proc:
            subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                           capture_output=True)
        time.sleep(1.0)
        shutil.rmtree(data_dir, ignore_errors=True)
        print("已清理隔离数据目录")


if __name__ == "__main__":
    raise SystemExit(main())
