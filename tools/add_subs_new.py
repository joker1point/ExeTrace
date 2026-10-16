"""给新视频（10月6日 (1).mp4）的后段补字幕。

背景：视频前段（0-97s）已有字幕；96s 后（"Okay."之后）无字幕。
字幕来源：SenseVoice 本地转录（tools/transcribe_local.py），人工清洗碎句/错字。
样式对齐已有字幕：底部居中、微软雅黑、白字细黑边、Fontsize≈190@8K。

用法：.venv-build\\Scripts\\python.exe tools\\add_subs_new.py [--preview T0 T1]
"""
from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

FF = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"
VIDEO_IN = Path(r"C:\Users\biren\Videos\10月6日 (1).mp4")
TMP = Path(r"C:\Users\biren\AppData\Local\Temp\newaud")
ASS = TMP / "tail_subs.ass"
# ffmpeg 4.3.1 对含空格/括号的输出路径会报 "Invalid argument" → 先写英文路径再拷贝
VIDEO_TMP = TMP / "out_sub.mp4"
VIDEO_OUT = Path(r"C:\Users\biren\Videos\10月6日 (1)_字幕版.mp4")

# (start, end, text) —— 清洗自 SenseVoice 转录（去碎句"哦""嗯"、修错字）
EVENTS: list[tuple[float, float, str]] = [
    (100.14, 103.07, "创建他们的快捷方式。比如说，"),
    (104.33, 107.79, "我想为我的一个脚本创建一个"),
    (107.97, 109.39, "快捷方式。"),
    (109.59, 113.25, "那你就可以点击这一个「创建快捷方式」。"),
    (113.52, 115.61, "然后它就成功。"),
    (116.62, 118.93, "你看我们"),
    (119.09, 122.49, "就可以点击这个脚本，它就会自动运行。"),
    (124.74, 126.79, "好，然后你可以关闭它，"),
    (126.79, 131.0, "让它最小化在托盘这里。"),
    (132.05, 133.64, "然后又把它点出来。"),
]

HEADER = """[Script Info]
ScriptType: v4.00+
PlayResX: 7680
PlayResY: 4320
WrapStyle: 2
ScaledBorderAndShadow: yes

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Microsoft YaHei,190,&H00FFFFFF,&H000000FF,&H96000000,&H00000000,0,0,0,0,100,100,0,0,1,4,2,2,60,60,260,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
"""


def fmt(t: float) -> str:
    h = int(t // 3600)
    m = int((t % 3600) // 60)
    s = t % 60
    return f"{h}:{m:02d}:{s:05.2f}"


def write_ass(path: Path, shift: float = 0.0) -> None:
    lines = [HEADER]
    for s, e, text in EVENTS:
        lines.append(f"Dialogue: 0,{fmt(s - shift)},{fmt(e - shift)},Default,,0,0,0,,{text}")
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    write_ass(ASS)
    print(f"ASS: {len(EVENTS)} 条 -> {ASS}")

    # 分段编码模式：--seg T0 T1 （0-70 段无 seek 用原 ASS；70+ 段输入 seek 用平移 ASS）
    if "--seg" in sys.argv:
        i = sys.argv.index("--seg")
        t0, t1 = float(sys.argv[i + 1]), float(sys.argv[i + 2])
        out = TMP / f"seg_{int(t0):03d}.mp4"
        seg_ass = TMP / f"seg_{int(t0):03d}.ass"
        if t0 > 0:
            write_ass(seg_ass, shift=t0)      # 平移 ASS 匹配 seek 后的滤镜时间轴
            src_args = ["-ss", str(t0), "-to", str(t1), "-i", str(VIDEO_IN)]
        else:
            seg_ass = ASS
            src_args = ["-t", str(t1), "-i", str(VIDEO_IN)]
        subprocess.run(
            [FF, "-y", "-hide_banner", "-loglevel", "warning",
             *src_args, "-vf", f"ass={seg_ass.name}",
             "-c:v", "libx264", "-preset", "veryfast", "-crf", "18",
             "-pix_fmt", "yuv420p", "-an", str(out)],
            check=True, cwd=str(TMP),
        )
        print(f"seg -> {out}")
        return 0

    # ass= 滤镜对 Windows 路径的冒号敏感：cwd 到 ASS 目录 + 用文件名
    # 注意：-ss 走输入 seek 时滤镜时间轴从 0 起算 → 预览需用"平移版 ASS"
    if "--preview" in sys.argv:
        i = sys.argv.index("--preview")
        t0, t1 = float(sys.argv[i + 1]), float(sys.argv[i + 2])
        prev_ass = TMP / "preview_tail.ass"
        write_ass(prev_ass, shift=t0)
        out = TMP / "preview.mp4"
        subprocess.run(
            [FF, "-y", "-hide_banner", "-loglevel", "warning",
             "-ss", str(t0), "-to", str(t1), "-i", str(VIDEO_IN),
             "-vf", f"ass={prev_ass.name}", "-c:v", "libx264", "-preset", "veryfast",
             "-crf", "20", "-pix_fmt", "yuv420p", "-c:a", "copy", str(out)],
            check=True, cwd=str(TMP),
        )
        print(f"preview -> {out}")
        return 0

    subprocess.run(
        [FF, "-y", "-hide_banner", "-loglevel", "warning",
         "-i", str(VIDEO_IN), "-vf", f"ass={ASS.name}",
         "-c:v", "libx264", "-preset", "medium", "-crf", "18",
         "-pix_fmt", "yuv420p", "-movflags", "+faststart",
         "-c:a", "copy", str(VIDEO_TMP)],
        check=True, cwd=str(ASS.parent),
    )
    shutil.copy2(VIDEO_TMP, VIDEO_OUT)
    print(f"out -> {VIDEO_OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
