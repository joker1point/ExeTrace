"""生成 assets/app.ico —— 绘制逻辑唯一来源是 src/winutil.make_app_icon_image。

用法（构建 venv 内）：python tools/gen_icon.py
"""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

import winutil  # noqa: E402

SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]


def main() -> int:
    img = winutil.make_app_icon_image(256)
    out = ROOT / "assets" / "app.ico"
    out.parent.mkdir(parents=True, exist_ok=True)
    img.save(out, format="ICO", sizes=SIZES)
    print(f"written {out} ({out.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
