"""为 README 生成 ExeTrace 的动图与界面截图。

**不占用鼠标与键盘**：不注入任何真实输入，而是同进程驱动 Tk（改搜索变量、设列表选中、
直接调用业务方法），再用 PrintWindow 逐帧抓窗口位图。画面里发生的一切都真的走产品代码路径
（真的过滤列表、真的创建桌面快捷方式、状态栏真的变）。

产物：
  docs/screenshots/exetrace-pin.gif      —— 搜索 → 选中 → 一键钉桌面（多步交互 → 规范要求 GIF）
  docs/screenshots/exetrace-pin-any.png  —— 「钉任意程序…」对话框（静态图）

隐私：用临时 EXETRACE_DATA_DIR + 清掉注册表回填、只写入几条通用程序；录完删临时目录。
要求：Windows + Python 3.10+（Pillow）；在项目根目录运行：python tools/make_readme_media.py
"""
from __future__ import annotations

import ctypes
import os
import shutil
import sys
import tempfile
import time
import tkinter as tk
from ctypes import wintypes
from pathlib import Path
from tkinter import ttk

try:
    from PIL import Image, ImageGrab
except ImportError:
    print("需要 Pillow：pip install pillow")
    sys.exit(1)

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "screenshots"
sys.path.insert(0, str(ROOT / "src"))

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)
except Exception:
    try:
        user32.SetProcessDPIAware()
    except Exception:
        pass

DIALOG_TITLE = "钉任意程序到桌面"
FRAME_MS = 90                      # 抓帧间隔（PrintWindow 本身 ~40ms）

SEED_CANDIDATES = [
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft VS Code\Code.exe",
    r"C:\Program Files\7-Zip\7zFM.exe",
    r"C:\Windows\System32\notepad.exe",
]


class RECT(ctypes.Structure):
    _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long),
                ("right", ctypes.c_long), ("bottom", ctypes.c_long)]


def grab_window(hwnd: int) -> Image.Image:
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


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    data_dir = Path(tempfile.mkdtemp(prefix="exetrace_media_"))
    os.environ["EXETRACE_DATA_DIR"] = str(data_dir)          # 必须在 Store() 之前

    import paths                                            # noqa: E402
    import shortcut                                         # noqa: E402
    import ui                                               # noqa: E402
    from store import Store                                 # noqa: E402

    ui.enable_dpi_awareness()
    root = tk.Tk()
    shortcut.com_initialize()
    store = Store()
    app = ui.AppWindow(root, store, start_hidden=False, tray_enabled=False)
    root.title(paths.APP_TITLE)

    state: dict = {"frames": [], "recording": False, "t0": time.time(), "phase": "boot"}
    desktop_before = {p.name for p in Path(shortcut.desktop_dir()).glob("*.lnk")}

    def say(msg: str) -> None:
        print(f"[{time.time() - state.get('t0', time.time()):5.1f}s] {msg}", flush=True)

    # ---------------------------------------------------------------- 抓帧
    # 注意：本机可能同时开着 native 版，它的窗口标题与 Tk 版一模一样 —— 不能用
    # FindWindowW 按标题找（会抓错窗口）。用 wm_frame() 精确定位自己的 TkTopLevel，
    # 并且直接抓屏幕区域（PrintWindow 对 Tk 的渲染有时只给部分内容）。
    def hwnd_of_root() -> int:
        try:
            return int(root.wm_frame(), 16)
        except Exception:
            return 0

    _diag_done = False

    def crop_screen(rect: "RECT") -> Image.Image:
        """抓全屏再裁剪；坐标系不一致时按比例换算，并用边界自检兜底。"""
        nonlocal _diag_done
        full = ImageGrab.grab(all_screens=True)
        sm_w, sm_h = user32.GetSystemMetrics(0), user32.GetSystemMetrics(1)
        sx = full.width / sm_w if sm_w else 1.0
        sy = full.height / sm_h if sm_h else 1.0
        box = (int(rect.left * sx), int(rect.top * sy),
               int(rect.right * sx), int(rect.bottom * sy))
        if box[2] > full.width or box[3] > full.height or box[0] < 0 or box[1] < 0:
            box = (max(0, rect.left), max(0, rect.top),
                   min(rect.right, full.width), min(rect.bottom, full.height))
        if not _diag_done:
            _diag_done = True
            say(f"坐标诊断：全屏={full.width}x{full.height} GetSystemMetrics={sm_w}x{sm_h} "
                f"scale={sx:.2f} rect=({rect.left},{rect.top},{rect.right},{rect.bottom})")
        return full.crop(box)

    def grab_my_window() -> Image.Image:
        hwnd = hwnd_of_root()
        if not hwnd:
            raise RuntimeError("wm_frame() 拿不到窗口句柄")
        rect = RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        return crop_screen(rect)

    frame_err: list[str] = []

    def capture_loop() -> None:
        if state["recording"]:
            try:
                state["frames"].append((grab_my_window(), time.time()))
            except Exception as exc:          # noqa: BLE001
                if len(frame_err) < 3:
                    frame_err.append(repr(exc))
            root.after(FRAME_MS, capture_loop)

    def start_recording() -> None:
        state["recording"] = True
        state["t0"] = time.time()
        say("开始录屏")
        capture_loop()

    def stop_recording() -> None:
        state["recording"] = False
        say(f"停止录制，共 {len(state['frames'])} 帧")

    # ---------------------------------------------------------------- 导出
    def export_gif(dest: Path, width: int = 900) -> None:
        """相同帧合并、时长按真实时间戳累加 —— 否则静止段会被压缩成"快进"。"""
        frames = state["frames"]            # [(Image, ts)]
        if len(frames) < 5:
            say(f"!! 帧数太少（{len(frames)}），跳过 GIF")
            return
        w0 = frames[0][0].width
        if w0 < width:                      # 别把小窗口放大
            width = w0

        uniq: list[Image.Image] = []
        durs: list[int] = []
        prev_raw: bytes | None = None
        prev_ts: float | None = None
        for im, ts in frames:
            im = im.resize((width, max(1, round(im.height * width / im.width))), Image.LANCZOS)
            raw = im.tobytes()
            dur = 120 if prev_ts is None else int((ts - prev_ts) * 1000)
            dur = max(40, min(1200, dur))
            if prev_raw is not None and raw == prev_raw:
                durs[-1] += dur             # 静止段：把时间加给上一帧
            else:
                uniq.append(im)
                durs.append(dur)
                prev_raw = raw
            prev_ts = ts
        uniq[0].save(dest, save_all=True, append_images=uniq[1:],
                     duration=durs, loop=0, optimize=True)
        say(f"GIF 已保存 {dest.name}  {len(frames)} 帧 → 去重 {len(uniq)} 帧 / "
            f"{sum(durs)/1000:.1f} 秒  {dest.stat().st_size/1e6:.2f} MB")

    # ---------------------------------------------------------------- 动作
    def clean_and_seed(attempt: int = 0) -> None:
        """等库建好（不依赖 _scanning —— 那套扫描线程属于 main.py，这里没启动），
        然后清空并只写入少量示例记录。"""
        db = paths.data_dir() / "history.db"
        try:
            import sqlite3
            conn = sqlite3.connect(str(db), timeout=5)
            try:
                n = conn.execute("SELECT COUNT(*) FROM apps").fetchone()[0]
                conn.execute("DELETE FROM apps")
                conn.commit()
            finally:
                conn.close()
        except Exception as exc:                    # noqa: BLE001
            if attempt < 20:
                say(f"等库就绪…（{attempt}）{exc}")
                root.after(700, lambda: clean_and_seed(attempt + 1))
            else:
                say(f"!! 数据库始终不可用：{exc}")
                finish()
            return
        seeds = [p for p in SEED_CANDIDATES if Path(p).exists()]
        st = Store(str(db))
        try:
            now = time.time()
            for i, p in enumerate(seeds):
                st.upsert_seed(p, launch_count=8 + i * 6, last_seen=now - i * 3600)
            st.add_usage({seeds[0]: 5400.0})
        finally:
            st.close()
        app.refresh(force=True)
        say(f"已清库 {n} 条、写入 {len(seeds)} 条示例记录")
        root.after(700, start_recording)

    def type_search(text: str) -> None:
        app.search_var.set(text)
        say(f"搜索：{text!r}")

    def clear_search() -> None:
        app.search_var.set("")
        say("清空搜索")

    def select_first() -> None:
        app.refresh(force=True)                 # 确保列表是最新的
        kids = app.tree.get_children()
        say(f"列表条目数：{len(kids)}")
        if not kids:
            say("!! 列表为空，无法选中")
            return
        app.tree.selection_set(kids[0])
        app.tree.focus(kids[0])
        app._on_selection_changed()
        name = app.tree.item(kids[0], "text")
        say(f"选中：{name}")

    def pin_it() -> None:
        say("调用「创建桌面快捷方式」")
        app._create_shortcut()

    def after_gif() -> None:
        export_gif(OUT / "exetrace-pin.gif")
        root.after(600, schedule_dialog)

    # ------------------------------------------------- 第 3 张静态图：对话框
    def schedule_dialog() -> None:
        app.search_var.set("")
        say("准备「钉任意程序」对话框截图")
        root.after(300, lambda: app._pin_any_path())      # 阻塞在 wait_window，但 Tk 继续跑
        root.after(1500, shoot_dialog)

    def shoot_dialog() -> None:
        dlg = user32.FindWindowW(None, DIALOG_TITLE)
        if not dlg:
            say("!! 没找到对话框窗口")
            finish()
            return
        # 填入一个真实存在的路径并触发「识别」（等价于用户敲路径 + 回车）
        try:
            target = next(p for p in SEED_CANDIDATES if Path(p).exists())
        except StopIteration:
            target = str(Path(sys.executable))
        filled = False

        def walk(widget) -> bool:
            nonlocal filled
            for c in widget.winfo_children():
                if isinstance(c, ttk.Entry):
                    try:
                        c.delete(0, "end")
                        c.insert(0, target)
                        c.event_generate("<Return>")     # 触发「识别」
                        filled = True
                        return True
                    except tk.TclError:
                        pass
                if walk(c):
                    return True
            return False

        for w in root.winfo_children():
            if isinstance(w, tk.Toplevel) and walk(w):
                break
        say(f"对话框已填路径并识别：{target}（{'成功' if filled else '未找到输入框'}）")
        root.after(900, lambda: grab_dialog_and_close(dlg))

    def grab_dialog_and_close(dlg: int) -> None:
        rect = RECT()
        user32.GetWindowRect(dlg, ctypes.byref(rect))
        img = crop_screen(rect)
        dest = OUT / "exetrace-pin-any.png"
        img.save(dest)
        say(f"对话框截图已保存 {dest.name}  {img.size[0]}x{img.size[1]}")
        for w in root.winfo_children():
            if isinstance(w, tk.Toplevel):
                w.destroy()
        root.after(400, finish)

    def finish() -> None:
        # 清理：录制过程中真实创建到桌面的快捷方式
        created = [p for p in Path(shortcut.desktop_dir()).glob("*.lnk")
                   if p.name not in desktop_before]
        for p in created:
            try:
                p.unlink()
                say(f"已删除录制产生的快捷方式：{p.name}")
            except OSError:
                pass
        if frame_err:
            say("抓帧错误：" + " | ".join(frame_err))
        state["recording"] = False
        root.after(200, root.destroy)

    # ---------------------------------------------------------------- 时间线
    # 窗口挪到右下角、不置顶（尽量不打扰）；不调用 focus_force
    root.update_idletasks()
    # 注意：Tk 的 geometry 用「逻辑像素」，而 GetWindowRect / GetSystemMetrics 在
    # DPI-aware 进程里是「物理像素」—— 混用会把窗口挪到屏幕外（实测踩过）。
    # 所以：只用 geometry 给个初始尺寸，位置与最终尺寸一律用 MoveWindow（物理像素）。
    wa_w = user32.GetSystemMetrics(0)
    wa_h = user32.GetSystemMetrics(1)
    w_px, h_px = 1800, 1180
    x = max(0, wa_w - w_px - 40)
    y = max(0, wa_h - h_px - 90)
    root.geometry("900x560")
    root.update()
    try:
        hwnd0 = int(root.wm_frame(), 16)
    except Exception:
        hwnd0 = 0
    if hwnd0:
        user32.MoveWindow(hwnd0, x, y, w_px, h_px, True)
        say(f"窗口已对齐：{w_px}x{h_px} 物理像素 @ ({x},{y})，屏幕 {wa_w}x{wa_h}")
    root.attributes("-topmost", True)          # 录制期间置顶，避免被别的窗口盖住

    root.after(1500, clean_and_seed)              # 等首轮建库
    # GIF 时间线（录制大约从 1.7s 开始，总长约 8 秒）
    root.after(2600, lambda: type_search("e"))
    root.after(3100, lambda: type_search("ed"))
    root.after(3600, lambda: type_search("edge"))
    root.after(5000, clear_search)
    root.after(6000, select_first)
    root.after(7000, pin_it)
    root.after(9500, stop_recording)
    root.after(9900, after_gif)

    # 兜底：无论如何 40 秒后收工
    root.after(40000, finish)

    root.mainloop()
    store.close()
    shutil.rmtree(data_dir, ignore_errors=True)
    print("完成，临时数据目录已清理")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
