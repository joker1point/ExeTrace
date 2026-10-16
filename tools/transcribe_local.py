"""本地转录（funasr 离线）：VAD 分段 → SenseVoiceSmall 逐段识别 → 句级时间戳 JSON。

零下载（模型已缓存于 modelscope 缓存目录）。用 video-editor 的 venv 跑：
  ..\\video-editor\\.venv\\Scripts\\python.exe tools\\transcribe_local.py in.wav out.json
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from funasr import AutoModel

FF = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"
TAG_RE = re.compile(r"<\|[^|]*\|>")   # SenseVoice 输出的 <|zh|><|NEUTRAL|> 等标记


def clean(text: str) -> str:
    return TAG_RE.sub("", text).replace(" ", "").strip()


def main() -> int:
    wav = sys.argv[1]
    out_json = Path(sys.argv[2])

    vad = AutoModel(
        model="iic/speech_fsmn_vad_zh-cn-16k-common-pytorch",
        disable_update=True, device="cpu", disable_pbar=True,
    )
    asr = AutoModel(
        model="iic/SenseVoiceSmall",
        disable_update=True, device="cpu", disable_pbar=True, disable_log=True,
    )

    # 段上限 5s：字幕需要"句级"粒度，长段会让时间分配误差累积
    res = vad.generate(input=wav, cache={}, max_single_segment_time=5000)
    segs_ms = res[0]["value"]  # [[start_ms, end_ms], ...]
    print(f"VAD: {len(segs_ms)} 段")

    tmp = Path(tempfile.gettempdir()) / "tv_clip.wav"
    out = []
    for i, (s_ms, e_ms) in enumerate(segs_ms):
        t0, t1 = s_ms / 1000.0, e_ms / 1000.0
        tmp.unlink(missing_ok=True)
        subprocess.run(
            [FF, "-y", "-hide_banner", "-loglevel", "error",
             "-ss", str(t0), "-to", str(t1), "-i", wav, str(tmp)],
            check=True,
        )
        r = asr.generate(input=str(tmp), cache={}, language="zh", use_itn=True,
                         batch_size_s=300)
        text = clean(r[0].get("text", ""))
        if text:
            out.append({"start": round(t0, 3), "end": round(t1, 3), "text": text})
        if (i + 1) % 10 == 0:
            print(f"  ... {i + 1}/{len(segs_ms)}")

    out_json.write_text(json.dumps(out, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"{len(out)} 句 -> {out_json}")
    for s in out[:12]:
        print(f"  [{s['start']:7.2f} - {s['end']:7.2f}] {s['text']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
