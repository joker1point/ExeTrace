"""生成成片字幕（ASS）并烧录到 v5。

时间映射（原素材 → 成片）：每个已完成的 cut（静音裁剪 + 语义删除）把时间轴
缩短 (cut 时长 - 15ms 交叉淡化)；再叠加段偏移：
    成片 = 段偏移 + t - Σ_{cut 在 t 前}(cut 时长 - CROSSFADE_S)
段1 只保留成片段内 30.2s（尾部 OBS 弹窗被裁）；段2 无上限。

用法：.venv-build\\Scripts\\python.exe tools\\make_subtitles.py [--no-burn]
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VE = ROOT.parent / "video-editor"
SRC = VE / "_src"
VA = ROOT / "video_assets"
VIDEO_IN = VA / "final_v4.mp4"
ASS_OUT = VA / "subtitles.ass"
VIDEO_OUT = VA / "final_v5.mp4"
FFMPEG = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"

CROSSFADE_S = 0.015   # 与 video-editor 配置一致：每处 cut 交叉淡化 15ms
TITLE_S = 4.0         # 片头时长
SEG1_CAP = 30.2       # 段1 成片段内长度（已裁掉尾部 OBS 弹窗）
OFFSET1 = TITLE_S
OFFSET2 = TITLE_S + SEG1_CAP

# ASR 错字修正 / 语气词丢弃（None = 不显示）
FIXES: dict[str, str | None] = {
    "AIIcoding的发展。": "AI coding 的发展。",
    "给你一个set道引导文件去。": "给你一个 setup 引导文件去。",
    "那么这里呢我的第一个软件是可以输入入境，": "那么这里呢，我的第一个软件是可以输入路径，",
    "1X1。": "exe。",
    "呃，": None,
}


def load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def build_cuts(ctx: dict) -> list[tuple[float, float]]:
    """全部裁剪区间（秒、原时间轴）：pause_cuts + 语义删除的 atom 区间。"""
    cuts = [(c["start_ms"] / 1000.0, c["end_ms"] / 1000.0) for c in ctx["pause_cuts"]]
    atoms = {a["id"]: a for a in ctx["atoms"]}
    plan = load(Path(ctx["_work"]) / "semantic_plan.json")
    order = [a["id"] for a in ctx["atoms"]]
    for e in plan.get("edits", []):
        sid, eid = e.get("remove_start_id"), e.get("remove_end_id")
        if sid in order and eid in order:
            i0, i1 = order.index(sid), order.index(eid)
            for aid in order[i0 : i1 + 1]:
                a = atoms.get(aid)
                if a:
                    cuts.append((float(a["start"]), float(a["end"])))
    cuts.sort()
    return cuts


def removed_ids(ctx: dict) -> set[str]:
    plan = load(Path(ctx["_work"]) / "semantic_plan.json")
    order = [a["id"] for a in ctx["atoms"]]
    out: set[str] = set()
    for e in plan.get("edits", []):
        sid, eid = e.get("remove_start_id"), e.get("remove_end_id")
        if sid in order and eid in order:
            i0, i1 = order.index(sid), order.index(eid)
            out.update(order[i0 : i1 + 1])
    return out


def map_time(t: float, cuts: list[tuple[float, float]]) -> float:
    shift = 0.0
    for s, e in cuts:
        if e <= t:
            shift += (e - s) - CROSSFADE_S
        elif s < t < e:
            return s - shift
    return t - shift


def collect_events(ctx: dict, offset: float, cap: float | None) -> list[list]:
    cuts = build_cuts(ctx)
    removed = removed_ids(ctx)
    events: list[list] = []
    for a in ctx["atoms"]:
        if a["id"] in removed:
            continue
        s, e = map_time(float(a["start"]), cuts), map_time(float(a["end"]), cuts)
        if cap is not None:
            if s >= cap:
                continue
            e = min(e, cap)
            s = min(s, cap - 0.05)
        text = str(a["text"]).strip().replace("\n", " ")
        if text in FIXES:
            fixed = FIXES[text]
            if fixed is None:
                continue
            text = fixed
        if text and e > s:
            events.append([offset + s, offset + e, text])
    return events


def fmt(t: float) -> str:
    t = max(t, 0.0)
    h = int(t // 3600)
    m = int((t % 3600) // 60)
    s = t % 60
    return f"{h}:{m:02d}:{s:05.2f}"


HEADER = """[Script Info]
ScriptType: v4.00+
PlayResX: 1280
PlayResY: 720
WrapStyle: 2
ScaledBorderAndShadow: yes

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Microsoft YaHei,40,&H00FFFFFF,&H000000FF,&H00000000,&H66000000,0,0,0,0,100,100,0,0,1,2,1,2,30,30,28,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
"""


def main() -> int:
    works = [
        (SRC / ".seg1.mp4_work", OFFSET1, SEG1_CAP),
        (SRC / ".seg2.mp4_work", OFFSET2, None),
    ]
    events: list[list] = []
    for work, offset, cap in works:
        ctx = load(work / "semantic_context.json")
        ctx["_work"] = str(work)
        events += collect_events(ctx, offset, cap)
    events.sort(key=lambda x: x[0])
    # 修剪重叠：前一条的结束不晚于后一条的开始（原子时间偶有交叠）
    for i in range(len(events) - 1):
        if events[i + 1][0] < events[i][1]:
            events[i][1] = events[i + 1][0]
    lines = [HEADER]
    for s, e, text in events:
        if e - s < 0.25:
            e = s + 0.25
        end = min(e, s + 6.0)   # 单条字幕不超过 6 秒
        lines.append(f"Dialogue: 0,{fmt(s)},{fmt(end)},Default,,0,0,0,,{text}")
    ASS_OUT.write_text("\n".join(lines), encoding="utf-8")
    print(f"ASS: {len(events)} 条 -> {ASS_OUT}")
    for s, e, t in events[:6]:
        print(f"  [{s:6.2f} - {e:6.2f}] {t}")

    if "--no-burn" in sys.argv:
        return 0
    subprocess.run(
        [FFMPEG, "-y", "-hide_banner", "-loglevel", "warning",
         "-i", str(VIDEO_IN), "-vf", f"ass={ASS_OUT.name}",
         "-c:v", "libx264", "-preset", "medium", "-crf", "18",
         "-pix_fmt", "yuv420p", "-movflags", "+faststart",
         "-c:a", "copy", str(VIDEO_OUT)],
        check=True, cwd=str(VA),
    )
    print(f"out -> {VIDEO_OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
