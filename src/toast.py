"""窗口内浮层卡片：给「钉桌面成功」这类操作一个看得见的强反馈。

对齐 Rust 原生版（`exetrace-native/src/native_ui.rs` 的 `draw_toast` / `place_toast`）的
观感与口径：绿边圆角卡片 + 绿圆白勾 + 主/副两行文案，停在主窗口**右侧**那片空白区
（列表列之后没有数据的地方），约 3.4 秒后消失。

实现选择：tkinter 没有"子窗口浮层"设施，独立无边框 `Toplevel` 最稳；卡片用 `Canvas`
自绘（圆角靠 `create_polygon(smooth=True)`、勾用两条 `create_line`），不引 Pillow 生成图片。

**方位是异步补的**：Explorer 刷新桌面图标列表有延迟（见 `winutil.new_icon_zone` 的重试），
所以先立刻弹卡片、等方位到了再刷一次副文案 —— 不让"瞬间反馈"去等外部系统。
"""
from __future__ import annotations

import tkinter as tk

CARD_MS = 3400          # 停留时长（与原生版 TOAST_SECS 对齐）
CARD_W, CARD_H = 560, 92
MARGIN_R = 28           # 距主窗口右边缘
MARGIN_T = 104          # 距主窗口上边缘（工具条之下）

BG = "#f4fbf4"          # 极浅绿底
LINE = "#4caf50"        # 绿（与「桌面」列的勾同一套语义）
TITLE_FG = "#182818"
BODY_FG = "#606060"
FONT_FAMILY = "Microsoft YaHei UI"


def _place(root: tk.Misc, w: int, h: int) -> tuple[int, int]:
    """贴主窗口内容区**右上**（列表右侧那片没有数据的空白区）。"""
    root.update_idletasks()
    rw = root.winfo_width()
    x = root.winfo_rootx() + max(0, rw - w - MARGIN_R)
    y = root.winfo_rooty() + MARGIN_T
    return x, y


def show(root: tk.Misc, title: str, body: str, zone_getter=None) -> tk.Toplevel | None:
    """弹一张卡片。`zone_getter()` 若给出，会拿它的返回值补在副文案后面。

    返回卡片窗口（便于测试），异常时返回 None —— 反馈失败绝不影响主流程。
    """
    try:
        win = tk.Toplevel(root)
        win.overrideredirect(True)                 # 无标题栏/无边框
        try:
            win.attributes("-topmost", True)
        except tk.TclError:
            pass

        x, y = _place(root, CARD_W, CARD_H)
        win.geometry(f"{CARD_W}x{CARD_H}+{x}+{y}")
        win.configure(bg=BG)

        cv = tk.Canvas(win, width=CARD_W, height=CARD_H, bg=BG,
                       highlightthickness=2, highlightbackground=LINE)
        cv.pack(fill="both", expand=True)

        # 左侧：绿圆 + 白勾（画线而不是字体符号，免得字号/字形缺了变方框）
        d = 38
        cx, cy = 16 + d // 2, CARD_H // 2
        cv.create_oval(cx - d // 2, cy - d // 2, cx + d // 2, cy + d // 2,
                       fill=LINE, outline=LINE)
        q = d // 4
        cv.create_line(cx - q, cy, cx - q // 3, cy + q, cx + q + q // 3, cy - q,
                       fill="white", width=3, capstyle="round", joinstyle="round")

        tx = cx + d // 2 + 12
        cv.create_text(tx, int(CARD_H * 0.32), anchor="w", text=title,
                       fill=TITLE_FG, font=(FONT_FAMILY, 11, "bold"))
        body_id = cv.create_text(tx, int(CARD_H * 0.68), anchor="w", text=body,
                                 fill=BODY_FG, font=(FONT_FAMILY, 9))

        win.after(CARD_MS, win.destroy)

        if zone_getter is not None:
            def _fill_zone() -> None:
                try:
                    zone = zone_getter()
                except Exception:      # noqa: BLE001 - 方位拿不到就不补
                    zone = None
                if not zone:
                    return
                try:
                    cv.itemconfigure(body_id, text=f"{body} · 在桌面{zone}")
                except tk.TclError:
                    pass       # 卡片已销毁

            win.after(60, _fill_zone)

        return win
    except tk.TclError:
        return None
