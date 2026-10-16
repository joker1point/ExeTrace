"""按指定时间点抽帧拼图（时间轴检查用）。

用法：.venv-build\\Scripts\\python.exe tools\\grab_frames.py <video> <t1> <t2> ...
输出：%TEMP%/<视频名>_timeline.png
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

FF = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"


def main() -> int:
    video = sys.argv[1]
    times = [float(x) for x in sys.argv[2:]]
    if not times:
        print("need time points")
        return 1
    TW, TH = 480, 270
    cols = min(4, len(times))
    rows = (len(times) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * TW, rows * TH), (20, 20, 26))
    label_f = ImageFont.truetype(r"C:\Windows\Fonts\msyh.ttc", 20)
    for i, t in enumerate(times):
        tmp = Path(tempfile.gettempdir()) / f"gf_{i}.png"
        tmp.unlink(missing_ok=True)
        subprocess.run(
            [FF, "-y", "-hide_banner", "-loglevel", "error", "-ss", str(t),
             "-i", video, "-frames:v", "1", str(tmp)],
            check=True,
        )
        im = Image.open(tmp).convert("RGB").resize((TW, TH), Image.LANCZOS)
        x, y = (i % cols) * TW, (i // cols) * TH
        sheet.paste(im, (x, y))
        d = ImageDraw.Draw(sheet)
        d.rectangle([x, y, x + 130, y + 28], fill=(0, 0, 0))
        d.text((x + 6, y + 4), f"@{t}s", font=label_f, fill=(120, 220, 160))
    out = Path(tempfile.gettempdir()) / (Path(video).stem + "_timeline.png")
    sheet.save(out)
    print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
