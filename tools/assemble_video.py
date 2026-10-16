"""ExeTrace 宣传片成片组装：非录屏素材 + 两段粗剪录屏 → 最终成片。

结构（v1）：
    片头大字卡 → 段1（AI 小工具背景）→ 段2（找不到 / ExeTrace 找回）
    → 内存对比动画 → 片尾三连

规格：1280x720 / 30fps / H.264 CRF18 + AAC 160k
     录屏段保留原声（口播），无音频段自动补同长度静音轨。

用法：.venv-build\\Scripts\\python.exe tools\\assemble_video.py
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent      # .../exetrace
VA = ROOT / "video_assets"
VE = ROOT.parent / "video-editor"                  # 兄弟目录：粗剪工具
SRC = VE / "_src"
OUT = VA / (sys.argv[1] if len(sys.argv) > 1 else "final_v2.mp4")
PRIVACY = "privacy" in sys.argv[2:]   # 列表段隐私模糊（v4+）
FFMPEG = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"
FFPROBE = r"C:\Users\biren\anaconda3\Library\bin\ffprobe.exe"

# (文件, 是否带音频, 附加视频滤镜, 附加音频滤镜)
# seg1 裁到 30.2s：尾部 ~0.65s 是 OBS 弹窗 + OBS 界面误入镜头（2026-10-06 检查发现）
CLIPS: list[tuple[Path, bool, str | None, str | None]] = [
    (VA / "title_card.mp4", False, "scale=1280:720", None),
    (SRC / "seg1_edited.mp4", True,
     "trim=end=30.2,setpts=PTS-STARTPTS", "atrim=end=30.2,asetpts=PTS-STARTPTS"),
    (SRC / "seg2_edited.mp4", True, None, None),
    (VA / "mem_chart.mp4", False, "scale=1280:720", None),
    (VA / "end_card.mp4", False, "scale=1280:720", None),
]
SILENCE_S = [4.0, 12.0, 8.0]  # 与上面三个无音频段的时长对应

# 列表段隐私模糊参数（2026-10-06 对 v3 逐帧实测：列表窗口稳固在 x=108..988, y=40..676；
# 开头 ~2s 窗口从右侧滑入，需额外覆盖右侧区域）
_P_X, _P_Y, _P_W, _P_H = 108, 40, 880, 636
_P_T0, _P_T1 = 75.8, 96.6                    # 列表出现 → 段2 结束
_S_X, _S_Y, _S_W, _S_H = 600, 20, 680, 680   # 滑入瞬间：列表还在右侧
_S_T0, _S_T1 = 75.6, 77.6


def build() -> list[str]:
    args = [FFMPEG, "-y", "-hide_banner", "-loglevel", "warning"]
    for path, *_ in CLIPS:
        args += ["-i", str(path)]
    base = len(CLIPS)
    for s in SILENCE_S:
        args += ["-f", "lavfi", "-t", str(s), "-i", "anullsrc=r=44100:cl=stereo"]

    parts: list[str] = []
    for i, (_, _, vf, _af) in enumerate(CLIPS):
        v = f"[{i}:v]fps=30,setsar=1"
        if vf:
            v += f",{vf}"
        parts.append(v + f"[v{i}]")

    a_labels: list[str] = []
    si = 0
    for i, (_, has_audio, _vf, af) in enumerate(CLIPS):
        src = f"{i}:a" if has_audio else f"{base + si}:a"
        if not has_audio:
            si += 1
        chain = (
            f"[{src}]aresample=44100,"
            "aformat=sample_fmts=fltp:channel_layouts=stereo"
        )
        if af:
            chain += f",{af}"
        parts.append(chain + f"[a{i}]")
        a_labels.append(f"[a{i}]")

    seq = "".join(f"[v{i}]{a_labels[i]}" for i in range(len(CLIPS)))
    parts.append(f"{seq}concat=n={len(CLIPS)}:v=1:a=1[vc][aout]")
    if PRIVACY:
        # 列表段隐私模糊：主窗口矩形 + 滑入瞬间的右侧矩形
        parts.append("[vc]split=3[base][b1][b2]")
        parts.append(f"[b1]crop={_P_W}:{_P_H}:{_P_X}:{_P_Y},boxblur=14:2[bl1]")
        parts.append(
            f"[base][bl1]overlay={_P_X}:{_P_Y}"
            f":enable='between(t,{_P_T0},{_P_T1})'[t1]"
        )
        parts.append(f"[b2]crop={_S_W}:{_S_H}:{_S_X}:{_S_Y},boxblur=14:2[bl2]")
        parts.append(
            f"[t1][bl2]overlay={_S_X}:{_S_Y}"
            f":enable='between(t,{_S_T0},{_S_T1})'[vout]"
        )
    else:
        parts.append("[vc]null[vout]")

    args += [
        "-filter_complex", ";".join(parts),
        "-map", "[vout]", "-map", "[aout]",
        "-c:v", "libx264", "-preset", "medium", "-crf", "18",
        "-pix_fmt", "yuv420p", "-movflags", "+faststart",
        "-c:a", "aac", "-b:a", "160k",
        str(OUT),
    ]
    return args


def main() -> int:
    for path, *_ in CLIPS:
        if not path.exists():
            print(f"缺失素材: {path}")
            return 1
    cmd = build()
    print("assembling ...")
    subprocess.run(cmd, check=True)
    info = subprocess.run(
        [FFPROBE, "-v", "error", "-show_entries", "format=duration,size",
         "-of", "default=nw=1", str(OUT)],
        capture_output=True, text=True,
    )
    print(info.stdout.strip())
    print(f"out -> {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
