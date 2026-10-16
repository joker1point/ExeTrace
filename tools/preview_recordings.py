"""预览录屏素材：按时间均匀抽帧拼成大图（快速审核内容用）。

用法：.venv-build\\Scripts\\python.exe tools\\preview_recordings.py [-n 帧数] <video> [video2 ...]
输出：%TEMP%/<视频名>_preview.png（每个视频一张）
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

FF = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"
FFPROBE = r"C:\Users\biren\anaconda3\Library\bin\ffprobe.exe"


def duration(p: Path) -> float:
    r = subprocess.run(
        [FFPROBE, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(p)],
        capture_output=True, text=True,
    )
    return float(r.stdout.strip())


def shot_times(p: Path, n: int) -> list[float]:
    d = duration(p)
    return [d * (i + 0.5) / n for i in range(n)]


args = sys.argv[1:]
n_override = None
if "-n" in args:
    i = args.index("-n")
    n_override = int(args[i + 1])
    del args[i:i + 2]

TW, TH = 480, 270
COLS = 4
label_f = ImageFont.truetype(r"C:\Windows\Fonts\msyh.ttc", 18)

for path in args:
    p = Path(path)
    n = n_override or (6 if duration(p) < 100 else 8)
    cells: list[tuple[float, Path]] = []
    for t in shot_times(p, n):
        tmp = Path(tempfile.gettempdir()) / f"rec_{len(cells)}.png"
        tmp.unlink(missing_ok=True)  # 防残留帧冒充新结果
        subprocess.run(
            [FF, "-y", "-hide_banner", "-loglevel", "error",
             "-ss", f"{t:.2f}", "-i", str(p), "-frames:v", "1", str(tmp)],
            check=True,
        )
        cells.append((round(t, 1), tmp))

    rows = (len(cells) + COLS - 1) // COLS
    sheet = Image.new("RGB", (COLS * TW, rows * TH), (20, 20, 26))
    for i, (t, tmp) in enumerate(cells):
        im = Image.open(tmp).convert("RGB").resize((TW, TH), Image.LANCZOS)
        x, y = (i % COLS) * TW, (i // COLS) * TH
        sheet.paste(im, (x, y))
        d = ImageDraw.Draw(sheet)
        d.rectangle([x, y, x + 200, y + 26], fill=(0, 0, 0))
        d.text((x + 6, y + 3), f"@{t}s", font=label_f, fill=(120, 220, 160))

    out = Path(tempfile.gettempdir()) / f"{p.stem}_preview.png"
    sheet.save(out)
    print(out)
