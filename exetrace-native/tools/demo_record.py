r"""ExeTrace 功能演示录制器：真实操作 + 真实录屏 → GIF。

用法（在 exetrace-native 目录下执行）：
    python tools/demo_record.py --list              # 列出所有场景
    python tools/demo_record.py --scene 1-search    # 只录一个场景
    python tools/demo_record.py --all               # 全录
    python tools/demo_record.py --scene 1-search --gif-only   # 只重转 GIF（复用已有 mp4）

产物：
    assets/demo/raw/<scene>.mp4   中间录像（保留，便于只重调 GIF 参数）
    assets/demo/<scene>.gif       最终演示动图

原理（为什么是"真实使用演示"）：
  - 画面：ffmpeg `gdigrab` 录 exetrace 窗口的真实渲染结果，含真实鼠标指针；
  - 操作：`SendInput` 注入真实键鼠事件（点搜索框/输入/右键/双击/热键），
    走的是和用户手操完全相同的输入路径，不是摆拍截图；
  - 每场景先重启 exetrace，保证起点状态一致、可重复录制。

注意：录制期间会独占鼠标键盘（约 15-25 秒/场景），请勿同时操作电脑。
"""

from __future__ import annotations

import argparse
import atexit
import ctypes
import shutil
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path

from PIL import Image, ImageFilter

# ------------------------------------------------------------------ 基础路径

ROOT = Path(__file__).resolve().parent.parent          # exetrace-native/
EXE = ROOT / "dist" / "ExeTrace.exe"
RAW_DIR = ROOT / "assets" / "demo" / "raw"
GIF_DIR = ROOT / "assets" / "demo"
FFMPEG = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"

WINDOW_CLASS = "ExeTraceNativeWnd"

# ------------------------------------------------------------------ Win32 绑定

user32 = ctypes.WinDLL("user32", use_last_error=True)

# 让本进程 DPI 感知：GetWindowRect / SetCursorPos 都用物理像素，
# 与 gdigrab 录制的物理像素坐标系一致（本机 200% 缩放，逻辑≠物理会全错位）。
try:
    user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PER_MONITOR_AWARE_V2
except Exception:
    user32.SetProcessDPIAware()

ULONG_PTR = ctypes.c_ulonglong if ctypes.sizeof(ctypes.c_void_p) == 8 else ctypes.c_ulong


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", wintypes.WORD), ("wScan", wintypes.WORD),
                ("dwFlags", wintypes.DWORD), ("time", wintypes.DWORD),
                ("dwExtraInfo", ULONG_PTR)]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [("dx", wintypes.LONG), ("dy", wintypes.LONG),
                ("mouseData", wintypes.DWORD), ("dwFlags", wintypes.DWORD),
                ("time", wintypes.DWORD), ("dwExtraInfo", ULONG_PTR)]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [("uMsg", wintypes.DWORD), ("wParamL", wintypes.WORD), ("wParamH", wintypes.WORD)]


class _INPUT_UNION(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT), ("hi", HARDWAREINPUT)]


class INPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _INPUT_UNION)]


INPUT_KEYBOARD = 1
INPUT_MOUSE = 0
KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010

VK_MENU = 0x12        # Alt
VK_CONTROL = 0x11
VK_SHIFT = 0x10
VK_RETURN = 0x0D
VK_ESCAPE = 0x1B
VK_DELETE = 0x2E
VK_F1 = 0x70
VK_F4 = 0x73
VK_DOWN = 0x28
VK_TAB = 0x09

SM_CYMENU = 15   # 系统菜单项高度（本进程 DPI 感知，拿到的是物理像素）
SM_CYEDGE = 46   # 菜单边框厚度

# 会话光标临时放大（录 GIF 用：观众要看得清鼠标指哪儿）
IMAGE_CURSOR = 2
LR_LOADFROMFILE = 0x0010
OCR_NORMAL = 32512
OCR_HAND = 32649
SPI_SETCURSORS = 0x0057
SPIF_SENDCHANGE = 0x0002
CURSOR_DIR = Path(r"C:\Windows\Cursors")
CURSOR_PX = 72          # 放大后的光标边长（物理像素）

# ------------------------------------------------ 鼠标轨迹（后期把指针画进 GIF）
# gdigrab 的 -draw_mouse 在本机**不生效**（录出来的画面里根本没有指针），
# 所以由我们自己记录注入过的光标位置，转 GIF 时逐帧叠加一个放大版指针。
CURSOR_TRAIL: list = []   # [(相对秒, 屏幕x, 屏幕y), ...]
TRAIL_T0: float = 0.0


def _send(inp: INPUT) -> bool:
    return user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT)) == 1


def _scan(vk: int) -> int:
    """虚拟键码 → 硬件扫描码（真实键盘的按键两者都有）。"""
    return user32.MapVirtualKeyW(vk, 0) & 0xFF


def key_down(vk: int) -> None:
    # KEYBDINPUT 的字段顺序是 (wVk, wScan, dwFlags, ...)——虚拟键码必须放 wVk。
    # 扫描码也一并填上：只给 wVk 的"半截按键"会被菜单这类 modal 循环过滤掉。
    _send(INPUT(type=INPUT_KEYBOARD, u=_INPUT_UNION(ki=KEYBDINPUT(vk, _scan(vk), 0, 0, 0))))


def key_up(vk: int) -> None:
    _send(INPUT(type=INPUT_KEYBOARD,
                u=_INPUT_UNION(ki=KEYBDINPUT(vk, _scan(vk), KEYEVENTF_KEYUP, 0, 0))))


def key_vk(vk: int) -> None:
    # 按下时长要接近真实敲键（真实人手约 50-150ms）：0.02s 的"瞬时击键"
    # 会被菜单这类 modal 消息循环当成噪声吞掉。
    key_down(vk)
    time.sleep(0.08)
    key_up(vk)


def key_combo(*vks: int) -> None:
    for vk in vks:
        key_down(vk)
        time.sleep(0.05)
    time.sleep(0.05)
    for vk in reversed(vks):
        key_up(vk)
        time.sleep(0.03)
    time.sleep(0.03)


def type_char(ch: str) -> None:
    """打一个字符（KEYEVENTF_UNICODE，不依赖键盘布局）。"""
    code = ord(ch)
    _send(INPUT(type=INPUT_KEYBOARD,
                u=_INPUT_UNION(ki=KEYBDINPUT(0, code, KEYEVENTF_UNICODE, 0, 0))))
    time.sleep(0.01)
    _send(INPUT(type=INPUT_KEYBOARD,
                u=_INPUT_UNION(ki=KEYBDINPUT(0, code, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, 0, 0))))


def mouse_click(flags_down: int, flags_up: int) -> None:
    _send(INPUT(type=INPUT_MOUSE, u=_INPUT_UNION(mi=MOUSEINPUT(0, 0, 0, flags_down, 0, 0))))
    time.sleep(0.04)
    _send(INPUT(type=INPUT_MOUSE, u=_INPUT_UNION(mi=MOUSEINPUT(0, 0, 0, flags_up, 0, 0))))


HWND_TOPMOST = -1
HWND_NOTOPMOST = -2
SWP_NOSIZE = 0x0001
SWP_NOMOVE = 0x0002
SWP_SHOWWINDOW = 0x0040

RECORD_W = 2240   # 窗口物理尺寸（200% 缩放下的 1120 逻辑）
RECORD_H = 1480


def place_and_pin(hwnd: int) -> None:
    """把窗口钉在屏幕左上角并置顶。

    为什么必须置顶：gdigrab 录的是**屏幕合成结果**，如果录制期间有别的窗口
    盖在这个区域上（或被用户点到），录出来的就是别人的画面。置顶后即使
    录制区域内有其它程序，ExeTrace 也始终在最上层。
    """
    user32.SetWindowPos(hwnd, ctypes.c_void_p(HWND_TOPMOST), 0, 0, RECORD_W, RECORD_H,
                        SWP_SHOWWINDOW)
    time.sleep(0.4)


def unpin_window(hwnd: int) -> None:
    """取消置顶（录制结束必须还原，否则会打扰用户日常使用）。"""
    user32.SetWindowPos(hwnd, ctypes.c_void_p(HWND_NOTOPMOST), 0, 0, 0, 0,
                        SWP_NOMOVE | SWP_NOSIZE)


def find_window(timeout: float = 20.0) -> int:
    deadline = time.time() + timeout
    while time.time() < deadline:
        hwnd = user32.FindWindowW(WINDOW_CLASS, None)
        if hwnd:
            return hwnd
        time.sleep(0.2)
    raise RuntimeError("找不到 ExeTrace 主窗口（它启动了吗？）")


def find_child(parent: int, cls: str, nth: int = 0) -> int:
    """按类名找第 nth 个子窗口（FindWindowEx 从 0 开始数）。"""
    child = None
    prev = None
    for _ in range(nth + 1):
        child = user32.FindWindowExW(parent, prev, cls, None)
        if not child:
            return 0
        prev = child
    return child


def window_rect(hwnd: int) -> tuple[int, int, int, int]:
    r = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return (r.left, r.top, r.right - r.left, r.bottom - r.top)


# ------------------------------------------------------------------ UI 封装


class Ui:
    def __init__(self, hwnd: int) -> None:
        self.hwnd = hwnd
        self.edit = find_child(hwnd, "Edit")
        self.combo = find_child(hwnd, "ComboBox")
        self.list = find_child(hwnd, "SysListView32")
        self.buttons = [find_child(hwnd, "Button", i) for i in range(5)]
        self.status = find_child(hwnd, "msctls_statusbar32")

    # 控件几何 ----------------------------------------------------------
    def rect_of(self, hwnd: int) -> tuple[int, int, int, int]:
        return window_rect(hwnd)

    def center_of(self, hwnd: int) -> tuple[int, int]:
        x, y, w, h = window_rect(hwnd)
        return (x + w // 2, y + h // 2)

    def list_mid_point(self) -> tuple[int, int]:
        """列表垂直中点（一定落在某一行上，不依赖表头/行高）。"""
        x, y, w, h = window_rect(self.list)
        return (x + w // 2, y + h // 2)

    def list_first_row_point(self) -> tuple[int, int]:
        """第一行中心 —— 200% 缩放实测：表头 37px、行高 33px，故行 0 中心 ≈ +50。"""
        x, y, w, _h = window_rect(self.list)
        return (x + w // 2, y + 50)

    # 动作 --------------------------------------------------------------
    def ensure_front(self) -> None:
        """被别的窗口抢走前台时重新置前（否则点击会发到别人的窗口上）。"""
        if user32.GetForegroundWindow() != self.hwnd:
            self.activate()

    def move(self, x: int, y: int, pause: float = 0.25) -> None:
        user32.SetCursorPos(x, y)
        CURSOR_TRAIL.append((time.monotonic() - TRAIL_T0, x, y))
        time.sleep(pause)

    def click_at(self, x: int, y: int, pause: float = 0.35) -> None:
        self.ensure_front()
        self.move(x, y, 0.2)
        mouse_click(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        time.sleep(pause)

    def right_click_at(self, x: int, y: int, pause: float = 0.5) -> None:
        self.ensure_front()
        self.move(x, y, 0.2)
        mouse_click(MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
        time.sleep(pause)

    def double_click_at(self, x: int, y: int, pause: float = 0.4) -> None:
        self.ensure_front()
        self.move(x, y, 0.2)
        mouse_click(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        time.sleep(0.08)
        mouse_click(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        time.sleep(pause)

    def type_text(self, text: str, per_char: float = 0.13) -> None:
        self.ensure_front()
        for ch in text:
            type_char(ch)
            time.sleep(per_char)

    def activate(self) -> bool:
        """把主窗口切到前台（先模拟一下 Alt，绕开前台锁定）。"""
        key_vk(VK_MENU)
        time.sleep(0.05)
        if user32.IsIconic(self.hwnd):
            user32.ShowWindow(self.hwnd, 9)   # SW_RESTORE
        else:
            user32.ShowWindow(self.hwnd, 5)   # SW_SHOW
        user32.SetForegroundWindow(self.hwnd)
        time.sleep(0.4)
        return user32.GetForegroundWindow() == self.hwnd


# ------------------------------------------------------------------ 录制 / 转 GIF


def start_record(rect: tuple[int, int, int, int], out_path: Path, fps: int = 15):
    x, y, w, h = rect
    w -= w % 2   # libx264 + yuv420p 要求偶数尺寸
    h -= h % 2
    args = [
        FFMPEG, "-hide_banner", "-loglevel", "error", "-y",
        "-f", "gdigrab", "-framerate", str(fps),
        "-draw_mouse", "1",
        "-offset_x", str(x), "-offset_y", str(y),
        "-video_size", f"{w}x{h}",
        "-i", "desktop",
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p",
        str(out_path),
    ]
    return subprocess.Popen(args, stdin=subprocess.PIPE,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def stop_record(proc) -> None:
    try:
        if proc.stdin:
            proc.stdin.write(b"q")   # ffmpeg 优雅停止（写完尾部 moov）
            proc.stdin.flush()
    except Exception:
        proc.terminate()
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        proc.kill()


def load_cursor_png(size: int) -> Image.Image:
    """大号箭头指针（后期叠加用），带白描边——在任何背景上都醒目。"""
    base = Image.open(str(CURSOR_DIR / "aero_arrow_xl.cur")).convert("RGBA")
    base = base.resize((size, size), Image.LANCZOS)
    mask = base.split()[3]
    ring = mask.filter(ImageFilter.MaxFilter(5 if size > 48 else 3))
    white = Image.new("RGBA", base.size, (255, 255, 255, 0))
    white.putalpha(ring)
    out = Image.alpha_composite(Image.new("RGBA", base.size, (0, 0, 0, 0)), white)
    return Image.alpha_composite(out, base)


def _trail_pos(trail: list, t: float) -> tuple:
    """轨迹里 t 时刻的指针位置（取最近的前一个采样）。"""
    pos = (trail[0][1], trail[0][2])
    for ts, x, y in trail:
        if ts <= t:
            pos = (x, y)
        else:
            break
    return pos


def _to_gif_with_cursor(mp4: Path, gif: Path, width: int, fps: int,
                        trail: list, rect: tuple, head: float) -> None:
    """抽帧 → 逐帧叠加大号指针 → palette 合成 GIF（画质与常规路径一致）。"""
    work = mp4.parent / f"_frames_{mp4.stem}"
    if work.exists():
        shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    try:
        subprocess.run(
            [FFMPEG, "-hide_banner", "-loglevel", "error", "-y", "-i", str(mp4),
             "-vf", f"fps={fps},scale={width}:-1:flags=lanczos",
             str(work / "f%05d.png")], check=True)
        frames = sorted(work.glob("f*.png"))
        if not frames:
            raise RuntimeError("抽帧失败，无法叠加指针")
        k = width / rect[2]
        cursor = load_cursor_png(max(28, int(CURSOR_PX * k)))
        for i, fp in enumerate(frames):
            sx, sy = _trail_pos(trail, head + i / fps)
            im = Image.open(fp).convert("RGBA")
            px = max(0, min(int((sx - rect[0]) * k), im.width - cursor.width))
            py = max(0, min(int((sy - rect[1]) * k), im.height - cursor.height))
            im.alpha_composite(cursor, (px, py))   # 指针热点在左上角
            im.convert("RGB").save(fp)
        palette = work / "palette.png"
        subprocess.run(
            [FFMPEG, "-hide_banner", "-loglevel", "error", "-y",
             "-i", str(work / "f%05d.png"), "-vf", "palettegen=stats_mode=diff", str(palette)],
            check=True)
        subprocess.run(
            [FFMPEG, "-hide_banner", "-loglevel", "error", "-y",
             "-i", str(work / "f%05d.png"), "-i", str(palette),
             "-lavfi", "paletteuse=dither=bayer:bayer_scale=3", "-loop", "0", str(gif)],
            check=True)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def to_gif(mp4: Path, gif: Path, width: int = 1400, fps: int = 12,
           trail: list | None = None, rect: tuple | None = None, head: float = 0.7) -> None:
    """mp4 → GIF。

    给了 trail（鼠标轨迹）与 rect（录制区域）时，先把放大指针逐帧画进画面再合成
    —— 因为 gdigrab 的 draw_mouse 在本机不生效（录不到指针）。
    """
    if trail and rect:
        _to_gif_with_cursor(mp4, gif, width, fps, trail, rect, head)
        return
    palette = mp4.with_suffix(".palette.png")
    scale = f"scale={width}:-1:flags=lanczos"
    subprocess.run(
        [FFMPEG, "-hide_banner", "-loglevel", "error", "-y", "-i", str(mp4),
         "-vf", f"fps={fps},{scale},palettegen=stats_mode=diff", str(palette)],
        check=True,
    )
    subprocess.run(
        [FFMPEG, "-hide_banner", "-loglevel", "error", "-y", "-i", str(mp4),
         "-i", str(palette),
         "-lavfi", f"fps={fps},{scale}[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3",
         "-loop", "0", str(gif)],
        check=True,
    )
    palette.unlink(missing_ok=True)


# ------------------------------------------------------------------ 场景


def scene_1_search(ui: Ui) -> None:
    """1. 搜索实时过滤：输入即筛，Esc 复原。"""
    ui.click_at(*ui.center_of(ui.edit))
    ui.type_text("chr", per_char=0.16)
    time.sleep(1.0)
    ui.type_text("ome", per_char=0.16)
    time.sleep(1.1)
    key_vk(VK_ESCAPE)          # 清空搜索 → 恢复全列表
    time.sleep(0.9)


def scene_2_context_menu(ui: Ui) -> None:
    """2. 右键菜单 → 点击「复制完整路径」（真实鼠标移过去点，菜单键盘循环会吞注入键）。"""
    x, y = ui.list_mid_point()
    ui.right_click_at(x, y, pause=1.1)      # 菜单展开
    menu = user32.FindWindowW("#32768", None)   # 弹出菜单窗口（系统类名）
    if menu:
        mx, my, mw, _mh = window_rect(menu)
        item_h = user32.GetSystemMetrics(SM_CYMENU)
        edge = user32.GetSystemMetrics(SM_CYEDGE)
        # 第 3 项 =「复制完整路径」（0:启动 1:打开所在文件夹 2:复制完整路径）
        ui.move(mx + mw // 2, my + edge * 2 + item_h * 2 + item_h // 2, pause=0.8)
        time.sleep(0.5)
        mouse_click(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        time.sleep(1.6)                     # 状态栏出现「完整路径已复制到剪贴板」


def scene_3_pin_desktop(ui: Ui) -> None:
    """3. 一键钉桌面：选中一行 → 点按钮 → 状态栏反馈 + 「桌面」列打勾。"""
    ui.click_at(*ui.list_first_row_point(), pause=0.8)   # 真实鼠标点击选中第一行
    pin_btn = ui.buttons[3]                 # BUTTONS: 启动/打开所在文件夹/复制路径/创建桌面快捷方式/钉任意程序
    ui.click_at(*ui.center_of(pin_btn), pause=2.6)


def scene_4_launch(ui: Ui) -> None:
    """4. 搜索定位 → 双击启动（真实启动 7-Zip，随后脚本关掉它回到列表）。

    关闭不用 Alt+F4（等它成为前台太靠运气），改成按标题直接发 WM_CLOSE。
    """
    close_windows_titled("7-Zip")           # 防上一次残留
    ui.click_at(*ui.center_of(ui.edit))
    ui.type_text("7-zip", per_char=0.13)
    time.sleep(1.2)
    ui.double_click_at(*ui.list_first_row_point(), pause=1.0)   # 双击启动
    time.sleep(2.4)                          # 让它真的启动起来（新窗口出现本身即证明）
    close_windows_titled("7-Zip")            # 收尾清理，避免影响后续场景
    time.sleep(0.6)
    ui.activate()                            # 焦点回到 ExeTrace
    time.sleep(0.8)


def scene_5_tray_hotkey(ui: Ui) -> None:
    """5. 关窗缩托盘 → 全局热键 Ctrl+Alt+E 呼出并聚焦搜索框。"""
    key_combo(VK_MENU, VK_F4)               # 关窗 = 缩托盘
    time.sleep(1.8)                         # 画面里窗口消失
    key_combo(VK_CONTROL, VK_MENU, 0x45)    # Ctrl+Alt+E 全局呼出
    time.sleep(1.6)                         # 窗口回来 + 搜索框聚焦
    ui.type_text("edge", per_char=0.15)     # 焦点在搜索框：直接输入即生效
    time.sleep(1.2)
    key_vk(VK_ESCAPE)
    time.sleep(0.6)


def help_dialog_open() -> bool:
    """帮助面板是个 MessageBox：对话框类 #32770、标题恰为 APP_TITLE。"""
    return bool(user32.FindWindowW("#32770", "ExeTrace"))


def force_key(vk: int) -> None:
    """发一次真实按键；先强制释放修饰键。

    为什么需要：activate() 会先敲一下 Alt（抢前台用），Alt 状态若残留，
    后续 F1 会被系统当成 Alt+F1 —— 帮助面板就打不开了。
    """
    for mod in (VK_CONTROL, VK_MENU, VK_SHIFT):
        key_up(mod)
    time.sleep(0.06)
    key_vk(vk)


def scene_6_help(ui: Ui) -> None:
    """6. 帮助面板（功能说明 + 快捷键表）。

    入口走右键菜单的「帮助 (F1)」而不是敲 F1：实测本机上 F1 在应用层收不到
    （键盘注入整体正常——↓/文字都有效——唯独 F1 被系统或别的程序占用），
    而右键菜单是已验证可靠的路径，画面效果完全一致（帮助面板照样弹出）。
    """
    x, y = ui.list_mid_point()
    ui.right_click_at(x, y, pause=1.1)              # 菜单展开
    menu = user32.FindWindowW("#32768", None)
    if menu:
        mx, my, mw, mh = window_rect(menu)
        item_h = user32.GetSystemMetrics(SM_CYMENU)
        edge = user32.GetSystemMetrics(SM_CYEDGE)
        # 「帮助 (F1)」是菜单最后一项 → 从底部往上推算（不受分隔线影响）
        ui.move(mx + mw // 2, my + mh - edge - item_h // 2, pause=0.9)
        time.sleep(0.5)
        mouse_click(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
    deadline = time.time() + 2.5
    while time.time() < deadline and not help_dialog_open():
        time.sleep(0.2)
    time.sleep(2.2)                                 # 展示帮助内容
    dlg = user32.FindWindowW("#32770", "ExeTrace")
    if dlg:
        user32.PostMessageW(dlg, 0x0010, 0, 0)      # WM_CLOSE：比依赖回车键更稳
    time.sleep(1.0)


SCENES = {
    "1-search": scene_1_search,
    "2-context-menu": scene_2_context_menu,
    "3-pin-desktop": scene_3_pin_desktop,
    "4-launch": scene_4_launch,
    "5-tray-hotkey": scene_5_tray_hotkey,
    "6-help": scene_6_help,
}


# ------------------------------------------------------------------ 编排


def restart_exe() -> int:
    subprocess.run(["taskkill", "/IM", "ExeTrace.exe", "/F"],
                   capture_output=True, text=True)
    time.sleep(1.2)
    subprocess.Popen([str(EXE)], cwd=str(ROOT))
    hwnd = find_window()
    time.sleep(3.0)          # 等首启扫描 + 首屏填充完成
    return hwnd


def desktop_lnks() -> set:
    desktop = Path.home() / "Desktop"
    return {p.name for p in desktop.glob("*.lnk")}


def window_title(h: int) -> str:
    """取窗口标题。

    对**别的进程**的窗口不能用 GetWindowTextW —— 它只返回本进程的缓存标题，
    对其它进程常常拿到空串（这就是"找 7-Zip 时有时找不到"的原因）。
    改用 WM_GETTEXT：系统会为这条消息做跨进程字符串封送，配合超时可防卡死。
    """
    buf = ctypes.create_unicode_buffer(512)
    res = ctypes.c_size_t()
    ok = user32.SendMessageTimeoutW(
        h, 0x000D, 512, buf, 0x0002, 200, ctypes.byref(res)   # WM_GETTEXT, SMTO_ABORTIFHUNG
    )
    if ok:
        return buf.value
    user32.GetWindowTextW(h, buf, 512)
    return buf.value


def enum_titles() -> list:
    """当前所有顶层窗口的标题。"""
    titles = []

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def cb(h, _l):
        t = window_title(h)
        if t:
            titles.append(t)
        return True

    user32.EnumWindows(cb, 0)
    return titles


def find_titled(*keywords: str) -> list:
    """标题含任一关键字的顶层窗口标题。"""
    return [t for t in enum_titles()
            if any(k.lower() in t.lower() for k in keywords)]


def close_windows_titled(*keywords: str, tries: int = 3, gap: float = 0.7) -> int:
    """关闭标题含任一关键字的顶层窗口；重试若干次（窗口可能还在启动中）。"""
    total = 0
    for _ in range(tries):
        closed = 0

        @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        def cb(h, _l):
            nonlocal closed
            title = window_title(h)
            if title and any(k.lower() in title.lower() for k in keywords):
                user32.PostMessageW(h, 0x0010, 0, 0)   # WM_CLOSE
                closed += 1
            return True

        user32.EnumWindows(cb, 0)
        total += closed
        # 注意：closed==0 也继续重试——窗口可能还在创建中（标题此刻拿不到）
        time.sleep(gap)
    return total


def set_big_cursor() -> bool:
    """【备用】把会话的箭头/手型光标换成放大版。

    实测在本机**无效**：gdigrab 录出来的画面里根本没有光标（放大也没用），
    所以正式流程改成了"记录轨迹 + 转 GIF 时逐帧叠加指针"（见 CURSOR_TRAIL）。
    这个函数保留备用（有些环境下 gdigrab 是会画光标的）。
    只影响本次登录会话，用完 restore_cursor() 立即还原。
    """
    user32.LoadImageW.restype = ctypes.c_void_p
    user32.CopyImage.restype = ctypes.c_void_p
    ok = False
    for name, ocr in (("aero_arrow_xl.cur", OCR_NORMAL), ("aero_link_xl.cur", OCR_HAND)):
        src = CURSOR_DIR / name
        if not src.exists():
            continue
        h = user32.LoadImageW(None, str(src), IMAGE_CURSOR, 0, 0, LR_LOADFROMFILE)
        if not h:
            continue
        h2 = user32.CopyImage(ctypes.c_void_p(h), IMAGE_CURSOR, CURSOR_PX, CURSOR_PX, 0)
        if h2 and user32.SetSystemCursor(ctypes.c_void_p(h2), ocr):
            ok = True
    return ok


def restore_cursor() -> None:
    """还原系统光标方案（录制结束/异常退出都必须调用）。"""
    user32.SystemParametersInfoW(SPI_SETCURSORS, 0, None, SPIF_SENDCHANGE)


def run_scene(name: str, gif_only: bool = False) -> None:
    mp4 = RAW_DIR / f"{name}.mp4"
    gif = GIF_DIR / f"{name}.gif"
    RAW_DIR.mkdir(parents=True, exist_ok=True)
    GIF_DIR.mkdir(parents=True, exist_ok=True)

    if gif_only:
        if not mp4.exists():
            raise SystemExit(f"没有中间录像可复用：{mp4}")
        to_gif(mp4, gif)
        print(f"  GIF 已重转：{gif.relative_to(ROOT)}  {gif.stat().st_size/1024:.0f} KB")
        return

    print(f"[{name}] 重启 ExeTrace ...")
    hwnd = restart_exe()
    place_and_pin(hwnd)                     # 固定到左上角 + 置顶（防遮挡）
    ui = Ui(hwnd)
    if not ui.activate():
        print("  ⚠ 窗口未能切到前台，录出来可能是别的画面（继续，结束后可检查）")
    time.sleep(0.4)

    before_lnks = desktop_lnks()
    rect = window_rect(hwnd)
    print(f"  录制区域（物理像素）: {rect[0]},{rect[1]} {rect[2]}x{rect[3]}")

    global TRAIL_T0
    CURSOR_TRAIL.clear()
    TRAIL_T0 = time.monotonic()           # 轨迹时间基准（与录像起点对齐）
    proc = start_record(rect, mp4)
    time.sleep(0.7)                       # 录头：给一段稳定画面
    try:
        SCENES[name](ui)
        time.sleep(0.6)                   # 录尾
    finally:
        stop_record(proc)
        unpin_window(hwnd)                # 还原置顶状态

    # 清理该场景在桌面留下的快捷方式（3-pin-desktop 会创建）
    added = desktop_lnks() - before_lnks
    for lnk in added:
        try:
            (Path.home() / "Desktop" / lnk).unlink()
            print(f"  已清理演示产生的桌面快捷方式：{lnk}")
        except OSError:
            pass

    to_gif(mp4, gif, trail=list(CURSOR_TRAIL), rect=rect)
    size_kb = gif.stat().st_size / 1024
    print(f"  完成：{gif.relative_to(ROOT)}  {size_kb:.0f} KB")


def main() -> int:
    ap = argparse.ArgumentParser(description="ExeTrace 功能演示录制器")
    ap.add_argument("--scene", help="场景名（见 --list）")
    ap.add_argument("--all", action="store_true", help="录全部场景")
    ap.add_argument("--list", action="store_true", help="列出场景")
    ap.add_argument("--gif-only", action="store_true", help="只重转 GIF（复用已有 mp4）")
    args = ap.parse_args()

    if args.list:
        for name in SCENES:
            print(f"  {name:18s} {SCENES[name].__doc__.splitlines()[0].strip()}")
        return 0

    if not EXE.exists():
        print(f"找不到 {EXE}（先在项目根跑 build.ps1）", file=sys.stderr)
        return 2
    if not Path(FFMPEG).exists():
        print(f"找不到 ffmpeg: {FFMPEG}", file=sys.stderr)
        return 2

    targets = list(SCENES) if args.all else ([args.scene] if args.scene else [])
    if not targets:
        ap.print_help()
        return 2
    atexit.register(restore_cursor)       # 异常退出也保证光标还原

    for name in targets:
        if name not in SCENES:
            print(f"未知场景：{name}", file=sys.stderr)
            return 2
        run_scene(name, gif_only=args.gif_only)

    print("全部完成。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
