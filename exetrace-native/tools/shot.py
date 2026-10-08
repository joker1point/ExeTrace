r"""窗口截图工具（PrintWindow 离屏渲染，不抢焦点、不改窗口状态）。

用法：
    python tools/shot.py "窗口标题子串" 输出.png

需要 Pillow（用 exetrace/.venv-build 的 python 跑即可）。
同时打印窗口标题的 repr —— 验收窗口标题时比 PowerShell 可靠（不受控制台编码影响）。
"""

import ctypes
import os
import sys
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

PW_RENDERFULLCONTENT = 2


class RECT(ctypes.Structure):
    _fields_ = [
        ("left", ctypes.c_long),
        ("top", ctypes.c_long),
        ("right", ctypes.c_long),
        ("bottom", ctypes.c_long),
    ]


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_uint32),
        ("biWidth", ctypes.c_int32),
        ("biHeight", ctypes.c_int32),
        ("biPlanes", ctypes.c_uint16),
        ("biBitCount", ctypes.c_uint16),
        ("biCompression", ctypes.c_uint32),
        ("biSizeImage", ctypes.c_uint32),
        ("biXPelsPerMeter", ctypes.c_int32),
        ("biYPelsPerMeter", ctypes.c_int32),
        ("biClrUsed", ctypes.c_uint32),
        ("biClrImportant", ctypes.c_uint32),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", ctypes.c_uint32 * 3)]


def list_windows(pid: int | None = None, visible_only: bool = True) -> list[tuple[int, bool, str]]:
    """顶层窗口 (hwnd, visible, title)；pid 指定时列出该进程的全部窗口（调试用）。"""
    found: list[tuple[int, bool, str]] = []

    def cb(hwnd, _lparam):
        wpid = ctypes.c_ulong(0)
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(wpid))
        if pid is not None and wpid.value != pid:
            return True
        vis = bool(user32.IsWindowVisible(hwnd))
        if visible_only and not vis:
            return True
        n = user32.GetWindowTextLengthW(hwnd)
        buf = ctypes.create_unicode_buffer(n + 1)
        if n > 0:
            user32.GetWindowTextW(hwnd, buf, n + 1)
        if visible_only and n <= 0:
            return True
        found.append((hwnd, vis, buf.value))
        return True

    user32.EnumWindows(WNDENUMPROC(cb), 0)
    return found


def capture(hwnd: int, out_path: str) -> tuple[int, int]:
    r = RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    w, h = r.right - r.left, r.bottom - r.top
    if w <= 0 or h <= 0:
        raise RuntimeError(f"窗口尺寸异常: {w}x{h}")

    hdc = user32.GetWindowDC(hwnd)
    memdc = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    gdi32.SelectObject(memdc, bmp)
    user32.PrintWindow(hwnd, memdc, PW_RENDERFULLCONTENT)

    bi = BITMAPINFO()
    bi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bi.bmiHeader.biWidth = w
    bi.bmiHeader.biHeight = -h  # 自顶向下
    bi.bmiHeader.biPlanes = 1
    bi.bmiHeader.biBitCount = 32
    bi.bmiHeader.biCompression = 0  # BI_RGB
    buf = ctypes.create_string_buffer(w * h * 4)
    gdi32.GetDIBits(memdc, bmp, 0, h, buf, ctypes.byref(bi), 0)

    from PIL import Image

    img = Image.frombuffer("RGBA", (w, h), buf, "raw", "BGRA", 0, 1)
    img.convert("RGB").save(out_path)

    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(memdc)
    user32.ReleaseDC(hwnd, hdc)
    return w, h


def main() -> int:
    if len(sys.argv) >= 3 and sys.argv[1] == "--pid":
        pid = int(sys.argv[2])
        print(f"PID {pid} 的全部顶层窗口（含不可见）：")
        for hwnd, vis, t in list_windows(pid, visible_only=False):
            print(f"  {hwnd:#x}  visible={vis}  {t!r}")
        return 0

    if len(sys.argv) < 3:
        print(__doc__)
        print("当前可见窗口：")
        for hwnd, _vis, t in list_windows():
            print(f"  {hwnd:#x}  {t!r}")
        return 2

    title_sub, out = sys.argv[1], os.path.abspath(sys.argv[2])
    hits = [(h, t) for h, _v, t in list_windows() if title_sub in t]
    if not hits:
        print(f"未找到标题包含 {title_sub!r} 的可见窗口。当前窗口：")
        for hwnd, _vis, t in list_windows():
            print(f"  {hwnd:#x}  {t!r}")
        return 1

    hwnd, t = hits[0]
    os.makedirs(os.path.dirname(out), exist_ok=True)
    w, h = capture(hwnd, out)
    print(f"OK: {out}  {w}x{h}\n标题={t!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
