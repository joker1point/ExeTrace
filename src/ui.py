"""主界面：tkinter GUI。

线程模型：
- tkinter 只在主线程操作；
- watcher / 托盘 / 扫描线程通过 `post(fn)` 把回调投入 ui_queue，
  由主线程的定时器（每 POLL_MS）取出执行 —— 避免跨线程碰 Tcl 解释器。
- 列表刷新由 Store.rev 版本号驱动：只有数据真的变了才重建，
  重建时保留选中项与滚动位置，且绝不清空搜索框内容。
"""
from __future__ import annotations

import logging
import os
import queue
import time
import tkinter as tk
from tkinter import font as tkfont
from tkinter import messagebox, ttk

import paths
import shortcut
import winutil

log = logging.getLogger("exetrace.ui")

POLL_MS = 600
HIDDEN_POLL_MS = 2000     # 窗口隐藏/最小化时的泵间隔（后台常驻以省内存为先）
HIDDEN_TRIM_S = 120.0     # 隐藏状态下修剪工作集的周期（秒）
SEARCH_DEBOUNCE_MS = 260
ICON_SIZE = 24
ICON_BUDGET_S = 0.4    # 单轮图标加载的时间预算（秒）：跑满立即让出主线程
REBUILD_BUDGET_S = 0.35  # 单轮列表重建（删除/插入）的时间预算（秒）
ICON_MAX_ROWS = 500   # 只给前 N 行加载图标，滚动到更深的位置意义不大

# 全局字号放大系数（Tk 的 scaling 作用于所有正数字号字体）。
# 可用环境变量覆盖、免重新打包：EXETRACE_UI_SCALE=1.35（大）/ 1.05（小）
try:
    UI_SCALE = float(os.environ.get("EXETRACE_UI_SCALE") or 1.2)
except ValueError:
    UI_SCALE = 1.2

SORT_LABELS = {
    "智能（常用优先）": "smart",
    "最近打开": "recent",
    "打开次数": "count",
    "名称": "name",
}


def enable_dpi_awareness() -> None:
    """必须在创建任何 Tk 窗口之前调用；窗口一旦创建，系统会忽略该设置。"""
    try:
        import ctypes

        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except Exception:
            ctypes.windll.user32.SetProcessDPIAware()
    except Exception:
        pass


def _system_dpi() -> float:
    """主显示器 DPI。直接问系统 API：Tk 早期的 winfo_fpixels 会返回未落定的值
    （实测同一进程里先读 192、后读 230），拿它换算会让字号与尺寸不一致。"""
    try:
        import ctypes

        try:
            return float(ctypes.windll.user32.GetDpiForSystem())
        except Exception:
            hdc = ctypes.windll.user32.GetDC(0)
            try:
                return float(ctypes.windll.gdi32.GetDeviceCaps(hdc, 88))  # LOGPIXELSX
            finally:
                ctypes.windll.user32.ReleaseDC(0, hdc)
    except Exception:
        return 96.0


def apply_scaling(root: tk.Misc, scale: float = UI_SCALE) -> None:
    """按真实 DPI × 放大系数设置 Tk 全局缩放（数字越大字体越大）。"""
    try:
        root.tk.call("tk", "scaling", _system_dpi() / 72.0 * scale)
    except Exception:
        pass


def px(root: tk.Misc, logical: int) -> int:
    """逻辑像素 → 当前 DPI 下的物理像素。

    DPI 感知生效后，Tk 的所有像素参数（geometry / rowheight / 列宽 / 图标尺寸）
    都按物理像素解释；不换算的话在高分屏上窗口和行高会显得极小、字被裁切。
    """
    return int(round(logical * _system_dpi() / 96.0 * UI_SCALE))


def pick_font(root: tk.Misc) -> str:
    families = set(tkfont.families(root))
    for name in ("Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI", "Arial"):
        if name in families:
            return name
    return "TkDefaultFont"


def human_time(ts: float | None) -> str:
    if not ts:
        return "—"
    now = time.time()
    delta = now - ts
    if delta < 60:
        return "刚刚"
    if delta < 3600:
        return f"{int(delta // 60)} 分钟前"
    lt, nt = time.localtime(ts), time.localtime(now)
    if (lt.tm_year, lt.tm_yday) == (nt.tm_year, nt.tm_yday):
        return "今天 " + time.strftime("%H:%M", lt)
    yt = time.localtime(now - 86400)
    if (lt.tm_year, lt.tm_yday) == (yt.tm_year, yt.tm_yday):
        return "昨天 " + time.strftime("%H:%M", lt)
    if lt.tm_year == nt.tm_year:
        return time.strftime("%m-%d %H:%M", lt)
    return time.strftime("%Y-%m-%d", lt)


def human_duration(seconds: float | None) -> str:
    """累计使用时长的人类可读格式。"""
    s = float(seconds or 0)
    if s < 10:
        return "—"
    if s < 60:
        return f"{int(s)}s"
    if s < 3600:
        return f"{int(s // 60)}m"
    return f"{s / 3600:.1f}h"


class AppWindow:
    def __init__(self, root: tk.Tk, store, start_hidden: bool = False, tray_enabled: bool = True):
        self.root = root
        self.store = store
        self.watcher = None
        self.tray = None
        self.font_family = pick_font(root)
        self._icon_px = px(root, 20)

        self._ui_queue: queue.Queue = queue.Queue()
        self._last_rev = -1
        self._exists_cache: dict[str, bool] = {}
        self._icon_images: dict[str, "tk.PhotoImage | None"] = {}
        self._icon_queue: list = []
        self._icon_gen = 0
        self._rebuild: dict | None = None
        self._search_job = None
        self._closing = False
        self._scanning = True
        self._tray_hint_shown = False
        self._hotkey = None
        self._hidden = False        # 窗口当前是否隐藏/最小化（后台常驻态）
        self._last_trim = 0.0       # 上次工作集修剪时刻

        t0 = time.perf_counter()
        self._build_window()
        self._build_styles()
        self._build_widgets()
        self._setup_tray(tray_enabled)
        self._setup_hotkey()
        t1 = time.perf_counter()

        self.refresh(force=True)
        t2 = time.perf_counter()
        self.root.after(POLL_MS, self._pump)

        if start_hidden and self.tray is not None:
            self.root.withdraw()
        else:
            self._show_window()
        log.info(
            "启动序列: 构建 %.2fs, 首次填充 %.2fs, 合计 %.2fs",
            t1 - t0, t2 - t1, time.perf_counter() - t0,
        )

    # ------------------------------------------------------------ 跨线程入口

    def post(self, fn) -> None:
        """任何线程调用：把回调排入主线程队列。"""
        self._ui_queue.put(fn)

    # ------------------------------------------------------------ 窗口构建

    def _build_window(self) -> None:
        r = self.root
        r.title(paths.APP_TITLE)
        r.geometry(f"{px(r, 920)}x{px(r, 640)}")
        r.minsize(px(r, 720), px(r, 470))
        try:
            from PIL import ImageTk

            self._win_icon = ImageTk.PhotoImage(winutil.make_app_icon_image(64))
            r.iconphoto(True, self._win_icon)
        except Exception:
            pass
        r.protocol("WM_DELETE_WINDOW", self._on_close_request)

    def _build_styles(self) -> None:
        f = self.font_family
        style = ttk.Style(self.root)
        for theme in ("vista", "winnative", "clam"):
            if theme in style.theme_names():
                style.theme_use(theme)
                break
        style.configure(
            "Treeview",
            font=(f, 10),
            rowheight=px(self.root, 22),
            background="#FFFFFF",
            fieldbackground="#FFFFFF",
        )
        style.configure("Treeview.Heading", font=(f, 9, "bold"), padding=(6, 5))
        style.configure("TButton", font=(f, 9), padding=(10, 4))
        style.configure("TCheckbutton", font=(f, 9))
        style.configure("TCombobox", font=(f, 9))
        style.configure("Hint.TLabel", font=(f, 9), foreground="#6B7480")

    def _build_widgets(self) -> None:
        f = self.font_family
        r = self.root
        r.columnconfigure(0, weight=1)
        r.rowconfigure(1, weight=1)

        # --- 顶部：搜索 / 排序 / 统计
        header = ttk.Frame(r, padding=(14, 12, 14, 6))
        header.grid(row=0, column=0, sticky="ew")
        header.columnconfigure(0, weight=1)

        self.search_var = tk.StringVar()
        self.search_entry = ttk.Entry(header, textvariable=self.search_var, font=(f, 10))
        self.search_entry.grid(row=0, column=0, sticky="ew", padx=(0, 10), ipady=3)
        self._set_placeholder()
        self.search_entry.bind("<FocusIn>", self._clear_placeholder)
        self.search_entry.bind("<FocusOut>", self._set_placeholder)
        self.search_entry.bind("<Escape>", lambda _e: self._reset_search())
        self.search_var.trace_add("write", self._on_search_changed)

        self.sort_box = ttk.Combobox(header, state="readonly", width=10, font=(f, 9), values=list(SORT_LABELS))
        self.sort_box.current(0)
        self.sort_box.grid(row=0, column=1, padx=(0, 12))
        self.sort_box.bind("<<ComboboxSelected>>", lambda _e: self.refresh(force=True))

        self.stat_label = ttk.Label(header, text="", style="Hint.TLabel")
        self.stat_label.grid(row=0, column=2)

        # --- 中部：列表
        body = ttk.Frame(r, padding=(14, 4, 14, 0))
        body.grid(row=1, column=0, sticky="nsew")
        body.rowconfigure(0, weight=1)
        body.columnconfigure(0, weight=1)

        self.tree = ttk.Treeview(
            body,
            columns=("path", "cat", "count", "dur", "desk", "last"),
            show="tree headings",
            selectmode="browse",
        )
        self.tree.heading("#0", text="应用", anchor="w")
        self.tree.heading("path", text="位置", anchor="w")
        self.tree.heading("cat", text="分类", anchor="center")
        self.tree.heading("count", text="打开次数", anchor="center")
        self.tree.heading("dur", text="使用时长", anchor="center")
        self.tree.heading("desk", text="桌面", anchor="center")
        self.tree.heading("last", text="最后打开", anchor="center")
        self.tree.column("#0", width=px(r, 240), minwidth=px(r, 160), stretch=False)
        self.tree.column("path", width=px(r, 380), minwidth=px(r, 200), stretch=True)
        self.tree.column("cat", width=px(r, 76), minwidth=px(r, 58), stretch=False, anchor="center")
        self.tree.column("count", width=px(r, 82), minwidth=px(r, 62), stretch=False, anchor="center")
        self.tree.column("dur", width=px(r, 88), minwidth=px(r, 66), stretch=False, anchor="center")
        self.tree.column("desk", width=px(r, 52), minwidth=px(r, 44), stretch=False, anchor="center")
        self.tree.column("last", width=px(r, 132), minwidth=px(r, 108), stretch=False, anchor="center")
        self.tree.tag_configure("odd", background="#F5F8FC")
        self.tree.tag_configure("missing", foreground="#9AA3AF")
        self.tree.grid(row=0, column=0, sticky="nsew")

        vsb = ttk.Scrollbar(body, orient="vertical", command=self.tree.yview)
        vsb.grid(row=0, column=1, sticky="ns")
        self.tree.configure(yscrollcommand=vsb.set)

        self.empty_label = ttk.Label(
            body,
            text="还没有记录。\n打开任意一个软件后，它就会出现在这里。",
            style="Hint.TLabel",
            anchor="center",
            justify="center",
            font=(f, 10),
        )

        self.tree.bind("<Double-1>", lambda _e: self._launch_selected())
        self.tree.bind("<Return>", lambda _e: self._launch_selected())
        self.tree.bind("<Control-e>", lambda _e: self._reveal_selected())
        self.tree.bind("<Control-c>", lambda _e: self._copy_path())
        self.tree.bind("<Control-p>", lambda _e: self._create_shortcut())
        self.tree.bind("<Delete>", lambda _e: self._remove_selected())
        self.tree.bind("<F1>", lambda _e: self._show_help())
        self.tree.bind("<question>", lambda _e: self._show_help())
        self.tree.bind("<Button-3>", self._on_right_click)
        self.tree.bind("<<TreeviewSelect>>", lambda _e: self._on_selection_changed())

        self.menu = tk.Menu(r, tearoff=0, font=(f, 9))
        self.menu.add_command(label="启动", command=self._launch_selected)
        self.menu.add_command(label="创建桌面快捷方式", command=self._create_shortcut)
        self.menu.add_command(label="打开所在文件夹", command=self._reveal_selected)
        self.menu.add_separator()
        self.menu.add_command(label="复制完整路径", command=self._copy_path)
        self.menu.add_command(label="复制应用名", command=self._copy_name)
        self.menu.add_separator()
        self.menu.add_command(label="忽略此应用（不再记录）", command=self._ignore_selected)
        self.menu.add_command(label="从历史中移除", command=self._remove_selected)

        # --- 底部：操作
        footer = ttk.Frame(r, padding=(14, 8, 14, 4))
        footer.grid(row=2, column=0, sticky="ew")

        self.btn_launch = ttk.Button(footer, text="启动", command=self._launch_selected)
        self.btn_shortcut = ttk.Button(
            footer, text="创建桌面快捷方式", command=self._create_shortcut
        )
        self.btn_reveal = ttk.Button(footer, text="打开所在文件夹", command=self._reveal_selected)
        self.btn_copy = ttk.Button(footer, text="复制路径", command=self._copy_path)
        self.btn_remove = ttk.Button(footer, text="移除记录", command=self._remove_selected)
        self._action_buttons = [
            self.btn_launch,
            self.btn_shortcut,
            self.btn_reveal,
            self.btn_copy,
            self.btn_remove,
        ]
        for i, btn in enumerate(self._action_buttons):
            btn.grid(row=0, column=i, padx=(0, 8))

        self.btn_prune = ttk.Button(footer, text="清理失效记录", command=self._prune_missing)
        self.btn_prune.grid(row=0, column=4, padx=(16, 8))

        self.pause_var = tk.BooleanVar(value=False)
        self.pause_cb = ttk.Checkbutton(
            footer, text="暂停记录", variable=self.pause_var, command=self._on_pause_toggle
        )
        self.pause_cb.grid(row=0, column=6, sticky="e", padx=(0, 12))

        self.autostart_var = tk.BooleanVar(value=winutil.is_autostart_enabled())
        self.autostart_cb = ttk.Checkbutton(
            footer, text="开机自动启动", variable=self.autostart_var, command=self._on_autostart_toggle
        )
        self.autostart_cb.grid(row=0, column=7, sticky="e")
        footer.columnconfigure(5, weight=1)

        # --- 状态栏
        status = ttk.Frame(r, padding=(14, 2, 14, 8))
        status.grid(row=3, column=0, sticky="ew")
        status.columnconfigure(0, weight=1)
        self.status_var = tk.StringVar(value="正在扫描系统历史记录…")
        self.event_var = tk.StringVar(value="")
        self.tip_var = tk.StringVar(value="")
        ttk.Label(status, textvariable=self.status_var, style="Hint.TLabel").grid(row=0, column=0, sticky="w")
        ttk.Label(status, textvariable=self.event_var, style="Hint.TLabel").grid(row=0, column=1, sticky="e")
        ttk.Label(status, textvariable=self.tip_var, style="Hint.TLabel").grid(
            row=1, column=0, columnspan=2, sticky="w"
        )

        self._update_buttons()
        self._update_tip()

    # ------------------------------------------------------------ 托盘

    def _setup_tray(self, enabled: bool) -> None:
        if not enabled:
            return
        try:
            from tray import TrayIcon

            self.tray = TrayIcon(
                on_show=lambda: self.post(self._show_window),
                on_quit=lambda: self.post(self.quit),
                is_autostart=winutil.is_autostart_enabled,
                toggle_autostart=lambda: self.post(self._toggle_autostart_from_tray),
                is_paused=lambda: bool(self.watcher and self.watcher.paused),
                toggle_pause=lambda: self.post(self._toggle_pause_from_tray),
            )
            if not self.tray.start():
                self.tray = None
        except Exception as exc:
            log.warning("托盘不可用: %s", exc)
            self.tray = None

    def _setup_hotkey(self) -> None:
        """注册全局热键 Ctrl+Alt+E：任意时刻呼出 ExeTrace 主窗口。"""
        try:
            self._hotkey = winutil.GlobalHotkey(lambda: self.post(self._show_window))
            if self._hotkey.start():
                log.info("全局热键已注册（Ctrl+Alt+E 呼出）")
            else:
                log.info("全局热键不可用（%s）", self._hotkey.last_error)
                self._hotkey = None
        except Exception as exc:  # noqa: BLE001
            log.warning("热键注册失败: %s", exc)
            self._hotkey = None

    def _toggle_autostart_from_tray(self) -> None:
        enabled = winutil.set_autostart(not winutil.is_autostart_enabled())
        self.autostart_var.set(enabled)
        self._set_event("已开启开机自启" if enabled else "已关闭开机自启")

    def _toggle_pause_from_tray(self) -> None:
        if self.watcher is None:
            return
        if self.watcher.paused:
            self.watcher.resume()
        else:
            self.watcher.pause()
        self.pause_var.set(self.watcher.paused)
        self._set_event("已暂停记录（历史保留）" if self.watcher.paused else "已恢复记录")
        if self.tray is not None:
            self.tray.update_menu()

    # ------------------------------------------------------------ 搜索

    _PLACEHOLDER = "搜索应用名或路径…"

    def _set_placeholder(self, _event=None) -> None:
        if not self.search_var.get() and self.search_entry.get() == "":
            self._placeholder_on = True
            self.search_entry.insert(0, self._PLACEHOLDER)
            self.search_entry.configure(foreground="#9AA3AF")

    def _clear_placeholder(self, _event=None) -> None:
        if getattr(self, "_placeholder_on", False):
            self.search_entry.delete(0, "end")
            self.search_entry.configure(foreground="#1F2937")
            self._placeholder_on = False

    def _real_keyword(self) -> str:
        if getattr(self, "_placeholder_on", False):
            return ""
        return self.search_var.get().strip()

    def _reset_search(self) -> None:
        self._clear_placeholder()
        self.search_var.set("")
        self.refresh(force=True)

    def _on_search_changed(self, *_args) -> None:
        if getattr(self, "_placeholder_on", False):
            return
        if self._search_job is not None:
            try:
                self.root.after_cancel(self._search_job)
            except Exception:
                pass
        self._search_job = self.root.after(SEARCH_DEBOUNCE_MS, lambda: self.refresh(force=True))

    # ------------------------------------------------------------ 刷新

    def _sort_key(self) -> str:
        return SORT_LABELS.get(self.sort_box.get(), "smart")

    def _path_exists(self, p: str) -> bool:
        cached = self._exists_cache.get(p)
        if cached is None:
            cached = os.path.isfile(p)
            self._exists_cache[p] = cached
        return cached

    def _selected_path(self) -> str | None:
        sel = self.tree.selection()
        return sel[0] if sel else None

    def refresh(self, force: bool = False) -> None:
        if self._closing:
            return
        if self._scanning and not force:
            # 扫描期间库在持续写入，逐条重建既无意义又昂贵（每次都是已显示
            # 窗口上的全量删除+插入）；扫描完成时 on_scan_done 会强制刷一次。
            return
        rev = self.store.rev
        if not force and rev == self._last_rev:
            return
        self._last_rev = rev

        try:
            yview = self.tree.yview()[0]
        except Exception:
            yview = 0.0
        selected = self._selected_path()
        rows = self.store.query(self._real_keyword(), self._sort_key())

        self._icon_gen += 1
        self._rebuild = {
            "gen": self._icon_gen,
            "rows": rows,
            "phase": "del",
            "idx": 0,
            "yview": yview,
            "selected": selected,
            "t0": time.perf_counter(),
        }
        self._rebuild_step()

    def _rebuild_step(self) -> None:
        """分片重建列表：删除与插入都受时间预算限制，主线程始终能泵消息。

        2026-10-05 实测：已显示窗口上一次性删除+插入 317 行可达 3~7 秒
        （窗口未映射时同样操作仅 0.02s——Tk 只在映射状态下做真实布局）；
        与冷启动扫描并行时整轮耗时 7.25s，窗口被系统标记"未响应"。
        """
        st = self._rebuild
        if st is None or self._closing or st["gen"] != self._icon_gen:
            return
        t0 = time.perf_counter()
        if st["phase"] == "del":
            while time.perf_counter() - t0 < REBUILD_BUDGET_S:
                children = self.tree.get_children()
                if not children:
                    break
                self.tree.delete(*children[:40])
            if self.tree.get_children():
                self.root.after(8, self._rebuild_step)
                return
            st["phase"] = "ins"

        rows = st["rows"]
        idx = st["idx"]
        n = len(rows)
        while idx < n and time.perf_counter() - t0 < REBUILD_BUDGET_S:
            row_i = idx
            row = rows[idx]
            idx += 1
            exists = self._path_exists(row.path)
            label = " " + (row.name if exists else f"{row.name}（文件已丢失）")
            tags = []
            if row_i % 2:
                tags.append("odd")
            if not exists:
                tags.append("missing")
            self.tree.insert(
                "",
                "end",
                iid=row.path,
                text=label,
                values=(
                    row.path,
                    winutil.categorize(row.path),
                    row.launch_count or "—",
                    human_duration(row.total_seconds),
                    "✔" if row.pinned else "",
                    human_time(row.last_seen),
                ),
                tags=tuple(tags),
            )
        st["idx"] = idx
        if idx < n:
            self.root.after(8, self._rebuild_step)
            return

        # ---- 收尾：恢复选中/滚动 + 启动图标队列 + 统计 ----
        cost = time.perf_counter() - st["t0"]
        if cost > 1.0:
            log.info("列表重建 %d 行耗时 %.2fs（分片，全程可响应）", n, cost)
        self._rebuild = None
        selected = st["selected"]
        if selected and self.tree.exists(selected):
            self.tree.selection_set(selected)
            self.tree.focus(selected)
            self.tree.see(selected)
        elif rows:
            self.tree.yview_moveto(st["yview"])

        if rows:
            self.empty_label.place_forget()
        else:
            self.empty_label.place(relx=0.5, rely=0.5, anchor="center")

        self._icon_queue = rows[:ICON_MAX_ROWS]
        self.root.after(30, lambda: self._load_icons(self._icon_gen, 0))
        self._update_stats(n)
        self._update_buttons()
        self._update_tip()

    def _load_icons(self, gen: int, start: int) -> None:
        """分批加载图标：单轮只占主线程 ICON_BUDGET_S 秒，然后立即让出。

        固定 60 个/批曾造成真实故障（2026-10-05）：冷启动扫描线程并行时，
        单个图标的 SHGetFileInfoW 从 ~24ms 涨到 ~281ms（GIL + 磁盘竞争），
        首批 60 个连续同步跑 16.9s，窗口被系统标记"未响应"12s。
        时间预算制下无论竞争多严重，单轮最多阻塞 ICON_BUDGET_S 秒。
        """
        if self._closing or gen != self._icon_gen:
            return
        t0 = time.perf_counter()
        idx = start
        n = len(self._icon_queue)
        while idx < n and time.perf_counter() - t0 < ICON_BUDGET_S:
            row = self._icon_queue[idx]
            idx += 1
            if not self.tree.exists(row.path):
                continue
            img = self._get_icon(row.path)
            if img is not None:
                try:
                    self.tree.item(row.path, image=img)
                except Exception:
                    pass
        cost = time.perf_counter() - t0
        if cost > 1.0:
            log.info("图标批次 [%d:%d] 耗时 %.2fs", start, idx, cost)
        if idx < n:
            self.root.after(15, lambda: self._load_icons(gen, idx))

    def _get_icon(self, path: str):
        if path in self._icon_images:
            return self._icon_images[path]
        img = None
        if self._path_exists(path):
            png = winutil.extract_icon_png(path, self._icon_px)
            if png:
                try:
                    img = tk.PhotoImage(data=png)
                except Exception:
                    img = None
        self._icon_images[path] = img  # 保持引用，否则 Tk 会丢图
        return img

    def _update_stats(self, shown: int) -> None:
        total = self.store.total()
        today = self.store.count_today()
        if self._scanning:
            self.status_var.set("正在扫描系统历史记录…")
        else:
            text = f"后台记录中 · 已收录 {total} 个应用 · 今日打开 {today} 个"
            if self._real_keyword():
                text += f" · 匹配 {shown} 个"
            self.status_var.set(text)
        self.stat_label.configure(text=f"显示 {shown} / {total}")

    def _update_buttons(self) -> None:
        state = "normal" if self._selected_path() else "disabled"
        for btn in self._action_buttons:
            btn.configure(state=state)

    def _set_event(self, text: str) -> None:
        self.event_var.set(text)

    def _on_selection_changed(self) -> None:
        self._update_buttons()
        self._update_tip()

    def _update_tip(self) -> None:
        """底部提示条：明确 Enter 将执行什么（消除"神秘启动"）。"""
        path = self._selected_path()
        if not path:
            self.tip_var.set("双击或 Enter 启动选中项 · ? 查看全部快捷键")
            return
        self.tip_var.set(
            f"Enter → 启动：{os.path.basename(path)}    ·    "
            "Ctrl+E 定位 · Ctrl+C 复制路径 · Ctrl+P 钉到桌面 · Del 移除 · ? 帮助"
        )

    _HELP_TEXT = (
        "快捷键\n"
        "  双击 / Enter    启动选中项\n"
        "  Ctrl+E          打开所在文件夹\n"
        "  Ctrl+C          复制完整路径\n"
        "  Ctrl+P          创建桌面快捷方式\n"
        "  Delete          从历史中移除\n"
        "  F1 / ?          显示本帮助\n"
        "  右键            更多操作（含「忽略此应用」）\n\n"
        "说明\n"
        "  · 后台自动记录你打开的每个应用；关窗后仍在托盘记录（可暂停）\n"
        "  · 「桌面」列 ✔ 表示已为该应用创建过桌面快捷方式\n"
        "  · 「忽略此应用」后它不会再被记录；想恢复就删掉 config.json 里对应条目"
    )

    def _show_help(self) -> None:
        messagebox.showinfo("ExeTrace 帮助", self._HELP_TEXT)

    def _ignore_selected(self) -> None:
        path = self._require_selection()
        if not path:
            return
        name = os.path.basename(path)
        if not messagebox.askyesno(
            "ExeTrace",
            f"不再记录这个应用？\n\n{name}\n\n（已记录的历史会一并移除；"
            "想恢复：删掉 config.json 里对应条目。）",
        ):
            return
        cfg = paths.load_config()
        ignore = [str(x) for x in (cfg.get("ignore") or [])]
        if name.lower() not in [x.lower() for x in ignore]:
            ignore.append(name)
        cfg["ignore"] = ignore
        try:
            paths.save_config(cfg)
        except OSError as exc:
            messagebox.showerror("ExeTrace", f"写入配置失败：\n{exc}")
            return
        self.store.remove(path)
        self._set_event(f"已忽略：{name}")
        self.refresh(force=True)

    # ------------------------------------------------------------ 主循环

    def _window_hidden(self) -> bool:
        """窗口是否不可见（托盘隐藏 / 最小化）——后台常驻态。"""
        try:
            return self.root.state() in ("iconic", "withdrawn")
        except tk.TclError:
            return True

    def _trim_idle(self) -> None:
        """修剪工作集：把不活跃的页换出，把后台物理内存压到最小。

        实测（2026-10-05）：51 MB → <8 MB 立即生效；之后被轮询/泵反复
        碰热的页会缓慢回弹（约 16 MB），故隐藏状态下按周期再压。
        """
        if winutil.trim_working_set():
            self._last_trim = time.time()
            log.info("后台内存已压缩（工作集修剪）")

    def _pump(self) -> None:
        """主线程定时器：处理跨线程回调 + 按版本号刷新列表。

        窗口隐藏/最小化时降频（HIDDEN_POLL_MS）并完全跳过界面重建 ——
        回弹的后台内存几乎全部来自周期性活动反复碰热的代码页，让它们
        安静下来是「后台内存最小化」的前提。
        """
        if self._closing:
            return
        t0 = time.perf_counter()
        hidden = self._window_hidden()
        if hidden != self._hidden:
            self._hidden = hidden
            if hidden:
                self._last_rev = -1   # 呼出后强制重建一次
                self._trim_idle()     # 刚隐藏：立即压一次
            log.info("窗口状态: %s", "隐藏/最小化（后台降频）" if hidden else "可见（恢复正常）")
        elif hidden and time.time() - self._last_trim >= HIDDEN_TRIM_S:
            self._trim_idle()         # 周期回压（对抗被轮询碰热的回弹）
        while True:
            try:
                fn = self._ui_queue.get_nowait()
            except queue.Empty:
                break
            try:
                fn()
            except Exception as exc:
                log.warning("UI 回调异常: %s", exc)
                if not self._closing:
                    try:
                        self.root.after(HIDDEN_POLL_MS if hidden else POLL_MS, self._pump)
                    except Exception:
                        pass
                return
        try:
            if not hidden:
                self.refresh()
            self.root.after(HIDDEN_POLL_MS if hidden else POLL_MS, self._pump)
        except tk.TclError:
            pass
        cost = time.perf_counter() - t0
        if cost > 1.0:
            log.info("主循环轮次耗时 %.2fs（含列表重建）", cost)

    # ------------------------------------------------------------ 外部事件

    def on_watcher_record(self, exe: str, counted: bool) -> None:
        """watcher 线程回调：经队列转到主线程。"""
        if not counted:
            return
        name = os.path.splitext(os.path.basename(exe))[0]
        self.post(lambda: self._set_event(f"刚刚打开：{name}"))

    def on_scan_done(self, stats: dict) -> None:
        self._scanning = False
        self._last_rev = -1  # 强制下次刷新
        total = stats.get("total", 0)
        if stats.get("error"):
            self._set_event(f"历史扫描失败：{stats['error']}")
        else:
            self._set_event(f"历史扫描完成：收录 {total} 个应用")

    # ------------------------------------------------------------ 动作

    def _require_selection(self) -> str | None:
        path = self._selected_path()
        if not path:
            return None
        return path

    def _launch_selected(self) -> None:
        path = self._require_selection()
        if not path:
            return
        if not os.path.isfile(path):
            messagebox.showwarning(
                "ExeTrace", f"这个文件已经不在原来的位置了：\n\n{path}\n\n可以右键把它从历史中移除。"
            )
            return
        try:
            winutil.launch_path(path)
            self._set_event(f"已启动：{os.path.basename(path)}")
        except OSError as exc:
            messagebox.showerror("ExeTrace", f"启动失败：\n{exc}")

    def _reveal_selected(self) -> None:
        path = self._require_selection()
        if not path:
            return
        winutil.reveal_in_explorer(path)
        self._set_event("已在资源管理器中定位")

    def _copy_path(self) -> None:
        path = self._require_selection()
        if not path:
            return
        self.root.clipboard_clear()
        self.root.clipboard_append(path)
        self._set_event("路径已复制到剪贴板")

    def _copy_name(self) -> None:
        path = self._require_selection()
        if not path:
            return
        self.root.clipboard_clear()
        self.root.clipboard_append(os.path.basename(path))
        self._set_event("文件名已复制到剪贴板")

    def _create_shortcut(self) -> None:
        """把选中的应用钉到桌面（v3 核心：查看历史 → 一键创建快捷方式）。"""
        path = self._require_selection()
        if not path:
            return
        if not os.path.isfile(path):
            messagebox.showwarning(
                "ExeTrace", f"这个文件已经不在原来的位置了：\n\n{path}\n\n无法为它创建快捷方式。"
            )
            return
        try:
            desktop = shortcut.desktop_dir()
            os.makedirs(desktop, exist_ok=True)
            name = shortcut.default_shortcut_name(path)
            lnk_path = shortcut.unique_lnk_path(desktop, name)
            shortcut.create_shortcut(
                lnk_path,
                path,
                workdir=os.path.dirname(path),
                icon=path,
                description=os.path.basename(path),
            )
        except OSError as exc:
            log.exception("创建快捷方式失败")
            messagebox.showerror("ExeTrace", f"创建桌面快捷方式失败：\n{exc}")
            return

        # 立刻读回校验（文件真的可解析、目标一致）
        verified = False
        try:
            info = shortcut.read_shortcut(lnk_path)
            verified = os.path.normcase(info.get("target", "")) == os.path.normcase(path)
        except Exception as exc:
            log.warning("读回校验失败: %s", exc)

        self.store.mark_pinned(path)
        self.refresh(force=True)
        suffix = "（已校验）" if verified else ""
        self._set_event(f"已创建桌面快捷方式：{os.path.basename(lnk_path)}{suffix}")

    def _remove_selected(self) -> None:
        path = self._require_selection()
        if not path:
            return
        self.store.remove(path)
        self._exists_cache.pop(path, None)
        self._set_event("已从历史中移除")
        self.refresh(force=True)

    def _prune_missing(self) -> None:
        removed = self.store.prune_missing()
        for p in list(self._exists_cache):
            self._exists_cache[p] = os.path.isfile(p)
        self._set_event(f"已清理 {removed} 条失效记录" if removed else "没有失效记录")
        self.refresh(force=True)

    def _on_autostart_toggle(self) -> None:
        want = self.autostart_var.get()
        actual = winutil.set_autostart(want)
        if actual != want:
            self.autostart_var.set(actual)
            messagebox.showerror("ExeTrace", "设置开机自启失败（注册表写入被拒绝）。")
        else:
            self._set_event("已开启开机自启" if actual else "已关闭开机自启")

    def _on_pause_toggle(self) -> None:
        want = self.pause_var.get()
        if self.watcher is None:
            self.pause_var.set(False)
            return
        if want:
            self.watcher.pause()
            self._set_event("已暂停记录（历史保留）")
        else:
            self.watcher.resume()
            self._set_event("已恢复记录")
        if self.tray is not None:
            self.tray.update_menu()

    def _on_right_click(self, event) -> None:
        iid = self.tree.identify_row(event.y)
        if iid:
            self.tree.selection_set(iid)
            self.tree.focus(iid)
        self._update_buttons()
        try:
            self.menu.tk_popup(event.x_root, event.y_root)
        finally:
            self.menu.grab_release()

    # ------------------------------------------------------------ 显示/退出

    def _show_window(self) -> None:
        try:
            self.root.deiconify()
            self.root.lift()
            self.root.attributes("-topmost", True)
            self.root.after(400, lambda: self.root.attributes("-topmost", False))
            self.root.focus_force()
        except tk.TclError:
            pass

    def _on_close_request(self) -> None:
        if self.tray is not None:
            self.root.withdraw()
            if not self._tray_hint_shown:
                self._tray_hint_shown = True
                self.tray.notify("已最小化到托盘，仍在后台记录应用历史。\n右键托盘图标可退出。", "ExeTrace")
        else:
            self.quit()

    def quit(self) -> None:
        if self._closing:
            return
        self._closing = True
        if self.watcher is not None:
            try:
                self.watcher.stop()
            except Exception:
                pass
        if self._hotkey is not None:
            try:
                self._hotkey.stop()
            except Exception:
                pass
        if self.tray is not None:
            try:
                self.tray.stop()
            except Exception:
                pass
        try:
            self.root.destroy()
        except Exception:
            pass
