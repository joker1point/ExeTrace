"""为 docs/feature-demo.md 生成 **native 版**真实界面截图（带功能高亮标注）。

三条纪律：
1. **隐私**：用 `EXETRACE_DATA_DIR` 指向临时目录，库里只写「系统自带程序 + C:\\Users\\Public
   下的演示项目/网页」，绝不出现本机真实应用历史与个人路径。
2. **真实**：截的是真窗口、真记录（项目和 html 的路径都真实存在，程序自己的查询会过滤失效行）。
3. **图文对应**：每张图只描述它对应那一节的功能，高亮框直接框在**该功能所在的控件**上
   （视图下拉 / 分类列 / 列表），并配一行标签说明。

用法：python tools/make_native_shots.py
前提：Windows + Pillow；dist/ExeTrace.exe 已构建。会临时关掉正在运行的 ExeTrace，跑完自动拉回。
"""
from __future__ import annotations

import ctypes
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    print("需要 Pillow：pip install pillow")
    raise SystemExit(1)

ROOT = Path(__file__).resolve().parents[1]          # exetrace/
# 优先用 target 里的编译产物：dist 的可能正在被用户运行的实例占用，拷不进去
_TARGET = (ROOT / "exetrace-native" / "target" / "x86_64-pc-windows-gnu"
           / "release" / "exetrace.exe")
EXE = _TARGET if _TARGET.exists() else ROOT / "exetrace-native" / "dist" / "ExeTrace.exe"
OUT = ROOT / "exetrace-native" / "docs" / "screenshots"
PUBLIC = Path(r"C:\Users\Public\exetrace-demo")     # 中性路径：不含个人用户名

TITLE = "ExeTrace — 应用历史定位器"
CLS = "ExeTraceNativeWnd"

ID_LIST = 1003
ID_STATUS = 1004
ID_VIEW = 1006

LVM_GETCOLUMNWIDTH = 0x101D
LVM_GETITEMRECT = 0x100E
CB_SETCURSEL = 0x014E
WM_COMMAND = 0x0111
CBN_SELCHANGE = 1

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)
except Exception:
    try:
        user32.SetProcessDPIAware()
    except Exception:
        pass

RED = (222, 54, 54)


class RECT(ctypes.Structure):
    _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long),
                ("right", ctypes.c_long), ("bottom", ctypes.c_long)]


# ------------------------------------------------------------------ 抓图

def grab_window(hwnd: int) -> Image.Image:
    rect = RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    w, h = rect.right - rect.left, rect.bottom - rect.top
    if w <= 0 or h <= 0:
        raise RuntimeError(f"窗口尺寸 {w}x{h}（hwnd={hwnd} 可能已销毁 —— 实例提前退出了？）")
    hdc = user32.GetWindowDC(hwnd)
    mem = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    old = gdi32.SelectObject(mem, bmp)
    user32.PrintWindow(hwnd, mem, 2)

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
    gdi32.GetDIBits(mem, bmp, 0, h, buf, ctypes.byref(bmi), 0)
    gdi32.SelectObject(mem, old)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)
    return Image.frombuffer("RGB", (w, h), buf, "raw", "BGRX", 0, 1).copy()


def ctrl_rect(hwnd: int, ctrl_id: int) -> tuple[int, int, int, int] | None:
    child = user32.GetDlgItem(hwnd, ctrl_id)
    if not child:
        return None
    win, box = RECT(), RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(win))
    user32.GetWindowRect(child, ctypes.byref(box))
    return (box.left - win.left, box.top - win.top,
            box.right - win.left, box.bottom - win.top)


def column_span(hwnd: int, index: int) -> tuple[int, int]:
    """列表里第 index 列的 (起始偏移, 宽度)，用于精确框住「分类」列。"""
    lst = user32.GetDlgItem(hwnd, ID_LIST)
    if not lst:
        return (0, 0)
    offset = 0
    width = 0
    for i in range(index + 1):
        w = user32.SendMessageW(lst, LVM_GETCOLUMNWIDTH, i, 0)
        if i == index:
            width = w
        else:
            offset += w
    return (offset, width)


def rows_span(hwnd: int, count: int) -> tuple[int, int] | None:
    """列表前 count 行的屏幕 y 范围（含表头）—— 高亮只框有数据的区域，
    不要贯穿整个空白列表。owner-data 列表的 `LVM_GETITEMRECT` 可用
    （`LVM_HITTEST` 才是不可靠的那个，见 README 实现注记）。"""
    lst = user32.GetDlgItem(hwnd, ID_LIST)
    if not lst:
        return None
    box = RECT()
    user32.GetWindowRect(lst, ctypes.byref(box))
    top = bottom = None
    for i in range(count):
        rc = RECT()
        rc.left = 0                       # LVIR_BOUNDS
        ok = user32.SendMessageW(lst, LVM_GETITEMRECT, i, ctypes.byref(rc))
        if not ok:
            break
        if top is None:
            top = rc.top
        bottom = rc.bottom
    if top is None or bottom is None:
        return None
    return (box.top + top, box.top + bottom)


# ------------------------------------------------------------------ 高亮

def _font(size: int):
    for name in ("msyh.ttc", "msyhbd.ttc", "simhei.ttf"):
        p = Path(r"C:\Windows\Fonts") / name
        if p.exists():
            return ImageFont.truetype(str(p), size)
    return ImageFont.load_default()


def highlight(img: Image.Image, box: tuple[int, int, int, int], label: str,
              anchor: str = "top") -> Image.Image:
    """在 box 上画高亮框 + 标签胶囊；anchor 决定标签贴在上边还是下边。"""
    l, t, r, b = [int(v) for v in box]
    l, t = max(0, l), max(0, t)
    r, b = min(img.width, r), min(img.height, b)
    if r <= l or b <= t:
        return img

    base = img.convert("RGBA")
    layer = Image.new("RGBA", base.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    # 大范围框只描边：半透明填充会把整片内容染色，反而看不清
    area = (r - l) * (b - t)
    alpha = 0 if area > img.width * img.height * 0.3 else 34
    d.rounded_rectangle((l, t, r, b), radius=6, fill=RED + (alpha,), outline=RED + (255,), width=3)

    f = _font(26)
    tw = int(d.textlength(label, font=f))
    pad = 10
    bw, bh = tw + pad * 2, 38
    bx = min(max(0, l), max(0, img.width - bw))
    by = t - bh - 6 if anchor == "top" else b + 6
    by = min(max(0, by), img.height - bh)
    d.rounded_rectangle((bx, by, bx + bw, by + bh), radius=8, fill=(24, 24, 27, 235))
    d.text((bx + pad, by + 4), label, font=f, fill=(255, 255, 255, 255))

    return Image.alpha_composite(base, layer).convert("RGB")


def crop_bottom(img: Image.Image) -> Image.Image:
    """裁掉窗口底部因最小高度留下的空白（坐标从左上起，不影响高亮位置）。"""
    px = img.load()
    bg = px[img.width - 6, img.height - 6]
    for y in range(img.height - 1, 0, -1):
        row = [px[x, y] for x in range(4, img.width - 4, 6)]
        if any(abs(c[0] - bg[0]) + abs(c[1] - bg[1]) + abs(c[2] - bg[2]) > 18 for c in row):
            return img.crop((0, 0, img.width, min(img.height, y + 10)))
    return img


# ------------------------------------------------------------------ 演示数据

def prepare_assets() -> dict:
    """在 C:\\Users\\Public 下造三个演示 git 仓库 + 三个 html，路径都真实存在。"""
    if PUBLIC.exists():
        shutil.rmtree(PUBLIC, ignore_errors=True)
    repos = [
        ("demo-blog", "https://github.com/joker1point/demo-blog.git", "own"),
        ("awesome-lib", "https://github.com/other/awesome-lib.git", "clone"),
        ("notes-local", None, "local"),
    ]
    for name, remote, _tier in repos:
        d = PUBLIC / name
        d.mkdir(parents=True, exist_ok=True)
        (d / "README.md").write_text(f"# {name}\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q"], cwd=d, check=False)
        if remote:
            subprocess.run(["git", "remote", "add", "origin", remote], cwd=d, check=False)

    htmls = []
    for stem, title in (("年度总结报告", "年度总结"), ("架构图", "架构图"),
                        ("简历预览", "简历预览")):
        p = PUBLIC / f"{stem}.html"
        p.write_text(f"<!doctype html><meta charset='utf-8'><title>{title}</title>"
                     f"<h1>{title}</h1>", encoding="utf-8")
        htmls.append(p)
    return {"repos": repos, "htmls": htmls}


SCHEMA = """
CREATE TABLE IF NOT EXISTS apps (
    path          TEXT PRIMARY KEY COLLATE NOCASE,
    name          TEXT NOT NULL,
    launch_count  INTEGER NOT NULL DEFAULT 0,
    first_seen    REAL,
    last_seen     REAL,
    source        TEXT NOT NULL DEFAULT 'scan',
    origin        TEXT NOT NULL DEFAULT '',
    pinned        INTEGER NOT NULL DEFAULT 0,
    total_seconds REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_apps_last_seen ON apps(last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_apps_count     ON apps(launch_count DESC);
"""


def seed_db(db: Path, repos, htmls) -> None:
    """清空冷启动回填的真实记录，只留几条通用程序的示例数据（隐私纪律）。"""
    apps = [p for p in (
        r"C:\Windows\System32\notepad.exe",
        r"C:\Windows\System32\mspaint.exe",
        r"C:\Windows\System32\charmap.exe",
        r"C:\Windows\System32\write.exe",
        r"C:\Windows\System32\calc.exe",
        r"C:\Windows\System32\magnify.exe",
        r"C:\Windows\System32\osk.exe",
        r"C:\Windows\System32\Wordpad.exe",
    ) if Path(p).exists()]

    conn = sqlite3.connect(str(db), timeout=10)
    try:
        conn.executescript(SCHEMA)
        conn.execute("DELETE FROM apps")      # 清掉冷启动回填进来的本机真实记录
        now = time.time()
        for i, p in enumerate(apps):
            conn.execute(
                "INSERT OR REPLACE INTO apps(path,name,launch_count,first_seen,last_seen,"
                "source,origin,pinned,total_seconds) VALUES(?,?,?,?,?,'scan','',0,?)",
                (p, Path(p).stem, 12 - i * 2, now - 86400 * (i + 1), now - 3600 * (i + 1),
                 5400 - i * 600),
            )
        for i, (name, _remote, tier) in enumerate(repos):
            d = PUBLIC / name
            conn.execute(
                "INSERT OR REPLACE INTO apps(path,name,launch_count,first_seen,last_seen,"
                "source,origin,pinned,total_seconds) VALUES(?,?,0,NULL,?,'project',?,0,0)",
                (str(d), name, now - 7200 * (i + 1), tier),
            )
        for i, p in enumerate(htmls):
            conn.execute(
                "INSERT OR REPLACE INTO apps(path,name,launch_count,first_seen,last_seen,"
                "source,origin,pinned,total_seconds) VALUES(?,?,0,?,?,'html','',0,0)",
                (str(p), p.stem, now - 5400 * (i + 1), now - 5400 * (i + 1)),
            )
        conn.commit()
    finally:
        conn.close()


# ------------------------------------------------------------------ 窗口操作

ENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


def find_window_for_pid(pid: int, timeout_s: float = 40.0) -> int | None:
    """只认**我们自己启动的那个进程**的窗口 —— 用户正在用的实例标题/类名一模一样，
    按标题找会误抓到它（那样后面的高亮与截图全错）。"""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        found: list[int] = []

        def _cb(hwnd, _lparam):
            owner = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            if owner.value != pid:
                return True
            buf = ctypes.create_unicode_buffer(128)
            user32.GetClassNameW(hwnd, buf, 128)
            if buf.value == CLS:          # 只认主窗口类名，别抓到 tooltip 之类的子窗口
                found.append(hwnd)
            return True

        user32.EnumWindows(ENUMPROC(_cb), 0)
        if found:
            return found[0]
        time.sleep(0.4)
    return None


def set_view(hwnd: int, index: int) -> None:
    box = user32.GetDlgItem(hwnd, ID_VIEW)
    user32.SendMessageW(box, CB_SETCURSEL, index, 0)
    user32.SendMessageW(hwnd, WM_COMMAND, (CBN_SELCHANGE << 16) | ID_VIEW, box)
    time.sleep(1.4)


# ------------------------------------------------------------------ 主流程

def shot(pid: int, hwnd: int, name: str, marks: list[tuple[tuple, str]]) -> None:
    # 每次重新按 pid 定位：窗口若被重建，缓存的 hwnd 会失效（尺寸变 0）
    fresh = find_window_for_pid(pid, 3.0)
    if fresh:
        hwnd = fresh
    img = crop_bottom(grab_window(hwnd))
    for box, label in marks:
        img = highlight(img, box, label)
    OUT.mkdir(parents=True, exist_ok=True)
    dest = OUT / name
    img.save(dest)
    print(f"[ok] {dest.name}  {img.size[0]}x{img.size[1]}")


def main() -> int:
    if not EXE.exists():
        print(f"找不到 {EXE}；先跑 build.ps1")
        return 1

    assets = prepare_assets()
    print(f"演示素材：{PUBLIC}（{len(assets['repos'])} 个仓库 / {len(assets['htmls'])} 个网页）")

    # 隔离数据目录 → 独立互斥体，与用户正在用的实例并存；--offscreen 窗口落在屏幕外
    # 且不激活、--no-tray 不往托盘塞第二个图标。用户全程无感（2026-10-17 采集素材纪律）。
    data_dir = Path(tempfile.mkdtemp(prefix="exetrace_shot_"))
    db = data_dir / "history.db"
    seed_db(db, assets["repos"], assets["htmls"])      # 启动前建好干净库
    env = {**os.environ, "EXETRACE_DATA_DIR": str(data_dir)}
    stderr_log = data_dir / "stderr.log"
    stderr_fp = open(stderr_log, "wb")

    def capture(view_key: str, name: str, marks_fn, rows_hint: int) -> bool:
        """起一个隔离实例、**停在指定视图**、截一张图、退出。

        为什么不切视图：切视图会走 `refresh(reset_icons=true)` 重建整个列表，
        实测在「库由外部写入」的数据形态下会让 UI 线程崩掉；改用 `--view` 让窗口
        一起步就是目标视图，绕开那条路径。
        """
        proc = subprocess.Popen(
            [str(EXE), "--offscreen", "--no-tray", "--no-scan", "--view", view_key],
            env=env, stdout=stderr_fp, stderr=stderr_fp,
        )
        try:
            hwnd = find_window_for_pid(proc.pid)
            if not hwnd:
                print(f"[fail] {name}: 主窗口未出现")
                return False
            rect = RECT()
            deadline = time.time() + 10.0
            while time.time() < deadline:
                user32.GetWindowRect(hwnd, ctypes.byref(rect))
                if rect.left < -10000:
                    break
                time.sleep(0.3)
            if rect.left > -10000:
                print(f"[abort] 窗口没落在屏幕外（left={rect.left}）——不打扰你")
                return False
            time.sleep(2.2)                     # 等首屏绘制与图标队列稳定

            view = ctrl_rect(hwnd, ID_VIEW)
            lst = ctrl_rect(hwnd, ID_LIST)
            col_off, col_w = column_span(hwnd, 2)

            def cat_col(rows: int):
                span = rows_span(hwnd, rows)
                top, bottom = span if span else (lst[1], lst[1] + 160)
                return (lst[0] + col_off, top, lst[0] + col_off + col_w, bottom)

            img = crop_bottom(grab_window(hwnd))
            # 数据只有几行时列表下方会留一大片空白 —— 按「表头 + N 行」估算裁掉
            # （owner-data 列表的 LVM_GETITEMRECT 不可靠，用几何估算更稳）
            dpi = 96
            try:
                dpi = user32.GetDpiForWindow(hwnd) or 96
            except Exception:
                pass
            scale = max(1.0, dpi / 96.0)
            cut = lst[1] + int((24 + 22 * rows_hint) * scale)
            if 120 < cut < img.height:
                img = img.crop((0, 0, img.width, cut))
            for box, label in marks_fn(view, lst, cat_col):
                img = highlight(img, box, label)
            OUT.mkdir(parents=True, exist_ok=True)
            dest = OUT / name
            img.save(dest)
            print(f"[ok] {dest.name}  {img.size[0]}x{img.size[1]}")
            return True
        finally:
            subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True)
            time.sleep(0.8)

    ok_all = True
    try:
        ok_all &= capture("project", "projects-view.png",
                          lambda v, l, c: [(v, "视图 → 项目"),
                                           (c(3), "分类列：自有 / 克隆 / 本地")], 3)
        ok_all &= capture("all", "projects-mixed.png",
                          lambda v, l, c: [(v, "视图 → 全部"),
                                           (l, "应用 / 项目 / 网页 同列表")], 11)
        ok_all &= capture("html", "html-view.png",
                          lambda v, l, c: [(v, "视图 → HTML"),
                                           (c(3), "本地网页：分类列显示「网页」")], 3)
    finally:
        stderr_fp.close()
        if not ok_all and stderr_log.exists() and stderr_log.stat().st_size:
            print("--- 进程 stderr ---")
            print(stderr_log.read_text(encoding="utf-8", errors="replace")[:1500])
        shutil.rmtree(data_dir, ignore_errors=True)
        shutil.rmtree(PUBLIC, ignore_errors=True)
        print("已清理隔离数据目录与演示素材（用户自己的实例全程未受影响）")

    # 第 8 节「运行中隐藏」不自动出图：那需要真的启动一个会弹窗的程序，
    # 违反「采集素材让用户无感」的纪律 —— 宁可少一张图（2026-10-17）。
    return 0 if ok_all else 6


if __name__ == "__main__":
    sys.exit(main())
