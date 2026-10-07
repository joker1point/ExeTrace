"""GUI 冒烟：打开「钉任意程序」对话框并自动关闭，验证不崩（改 UI 后可复跑：自动开/关对话框验证不崩）。"""
import sys
import tkinter as tk
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "src"))

import shortcut  # noqa: E402
import ui  # noqa: E402
from store import Store  # noqa: E402

ui.enable_dpi_awareness()
root = tk.Tk()
root.withdraw()
shortcut.com_initialize()
store = Store()
app = ui.AppWindow(root, store, start_hidden=True, tray_enabled=False)
root.after(300, lambda: app._pin_any_path())
root.after(1800, lambda: [w.destroy() for w in root.winfo_children() if isinstance(w, tk.Toplevel)])
root.after(2600, root.destroy)
root.mainloop()
store.close()
print("GUI smoke OK：对话框打开并关闭，无异常")
