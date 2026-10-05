"""把 video_assets 里的 mp4 抽关键帧拼成一张预览图（快速审核用）。

用法：.venv-build\\Scripts\\python.exe tools\\preview_assets.py
输出：video_assets/preview_grid.png
"""
from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
VA = ROOT / "video_assets"
FF = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"

SHOTS = [
    ("title_card.mp4", 1.0),
    ("title_card.mp4", 3.5),
    ("mem_chart.mp4", 2.5),
    ("mem_chart.mp4", 6.0),
    ("mem_chart.mp4", 9.5),
    ("mem_chart.mp4", 13.2),
    ("end_card.mp4", 1.6),
    ("end_card.mp4", 5.6),
]

TW, TH = 640, 360
COLS = 2
rows = (len(SHOTS) + COLS - 1) // COLS
sheet = Image.new("RGB", (COLS * TW, rows * TH), (20, 20, 26))
label_f = ImageFont.truetype(r"C:\Windows\Fonts\msyh.ttc", 22)

for i, (name, tsec) in enumerate(SHOTS):
    tmp = Path(tempfile.gettempdir()) / f"pv_{i}.png"
    subprocess.run(
        [FF, "-y", "-hide_banner", "-loglevel", "error", "-ss", str(tsec),
         "-i", str(VA / name), "-frames:v", "1", str(tmp)],
        check=True,
    )
    im = Image.open(tmp).convert("RGB").resize((TW, TH), Image.LANCZOS)
    x = (i % COLS) * TW
    y = (i // COLS) * TH
    sheet.paste(im, (x, y))
    d = ImageDraw.Draw(sheet)
    d.rectangle([x, y, x + 340, y + 32], fill=(0, 0, 0))
    d.text((x + 8, y + 4), f"{name} @{tsec}s", font=label_f, fill=(120, 220, 160))

out = VA / "preview_grid.png"
sheet.save(out)
print(out)
