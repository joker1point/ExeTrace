"""DPI 换算探针（诊断用）：确认缩放与关键像素尺寸的实际取值。

用法：python tools/px_probe.py <src_dir>
"""
from __future__ import annotations

import os
import sys
import tkinter as tk
import tkinter.font as tf

src = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "src")
sys.path.insert(0, src)

import ui  # noqa: E402

ui.enable_dpi_awareness()
root = tk.Tk()
ui.apply_scaling(root)
root.withdraw()

dpi = root.winfo_fpixels("1i")
scaling = float(root.tk.call("tk", "scaling"))
family = ui.pick_font(root)
linespace = tf.Font(root=root, family=family, size=10).metrics("linespace")
rowheight = ui.px(root, 22)

print(
    "[%s] dpi=%.0f  scaling=%.2f  font_size=10pt(视觉行高 %dpx)  rowheight=%dpx  icon=%dpx"
    % (os.path.basename(os.path.dirname(src)), dpi, scaling, linespace, rowheight, ui.px(root, 20))
)
root.destroy()
