"""ExeTrace 短片生成器（约 52 秒 / 1920x1080 / 30fps）。

把「真机操作 GIF」当演示素材，配上极简文字卡，全自动出一条短片：

    开场卡 → 痛点卡 → 六段功能演示（GIF + 一句说明）→ 结尾卡

版式参考 B 站开源项目宣传片的常见做法（参考片：BV1Z1Hj6wEAy「VibeStart」）：
  · 浅色（米白）背景，内容居中，说明文字做成底部白底胶囊标签；
  · 不放编号 / 大标题 / 强调色，靠界面本身出彩；
  · 开场与结尾用同款品牌卡。

转场与运动（v2，2026-10-09 按用户反馈「转场相对单调」改造；标准见 vault
《成片标准--封面节奏音效动图》§1.1）：
  · 段间 = xfade 左滑（位移 + 淡化，XFADE 秒），不再是各自淡出再淡入；
  · 静态卡 = zoompan 极缓慢推近（Ken Burns，KB_ZOOM），全程没有一帧死画面；
  · 演示段时长按「GIF 整数遍 + 交叉补偿」取，循环接缝仍落在段边界；
  · 转场音效铺在每次交叉的**中点**（assemble_xfade 返回的 marks）。

用法：
    .venv-build\\Scripts\\python.exe tools\\make_short_video.py                 # 浅色版（默认）
    .venv-build\\Scripts\\python.exe tools\\make_short_video.py dark            # 深色版
    .venv-build\\Scripts\\python.exe tools\\make_short_video.py light light_v2.mp4

产物：video_assets/short_<theme>_v1.mp4（中间段落在 video_assets/_short/）
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
VA = ROOT / "video_assets"
SHORT = VA / "_short"
GIF_DIR = ROOT / "exetrace-native" / "assets" / "demo"
ICON = ROOT / "assets" / "app.ico"
FFMPEG = r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe"

FONT_BOLD = r"C:\Windows\Fonts\msyhbd.ttc"
FONT_REG = r"C:\Windows\Fonts\msyh.ttc"

W, H, FPS = 1920, 1080, 30

# ---- v2 转场与运动（2026-10-09 用户反馈「转场相对单调」后新增；
#      标准见 vault《成片标准--封面节奏音效动图》§1.1）：
#   · 段间不再是纯 fade，而是 xfade 滑动转场（位移 + 淡化组合）
#   · 静态卡加极缓慢推近（Ken Burns）—— 全程没有一帧死画面
XFADE = 0.5        # 段间交叉时长（秒）
KB_ZOOM = 0.03     # 静态卡 Ken Burns 幅度：整段缓慢放大 3%
INTRO_SEC = 4.0    # 开场卡
PAIN_SEC = 6.0     # 痛点卡
END_SEC = 6.5      # 结尾卡

# ---------------------------------------------------------------- 两套配色
LIGHT = {
    "bg": (250, 249, 246),
    "fg": (24, 24, 27),
    "dim": (122, 120, 114),
    "line": (226, 224, 218),
    "capsule_bg": (255, 255, 255),
    "capsule_fg": (60, 58, 54),
    "cta_bg": (24, 24, 27),
    "cta_fg": (250, 249, 246),
}
DARK = {
    "bg": (13, 16, 23),
    "fg": (235, 238, 245),
    "dim": (128, 137, 155),
    "line": (58, 66, 86),
    "capsule_bg": (24, 28, 38),
    "capsule_fg": (200, 208, 222),
    "cta_bg": (74, 222, 128),
    "cta_fg": (13, 16, 23),
}

# 界面段：GIF 1400x925 → 1300x861，横向居中；浅色版往上收，给底部胶囊让位
GW, GH = 1300, 861
GX = (W - GW) // 2

# ---------------------------------------------------------------- 六段演示
# (GIF 名, 编号, 标题, 胶囊说明, 秒数)
DEMOS = [
    ("1-search", "01", "一秒找到", "输入关键词，几百个应用实时过滤", 8.0),
    ("2-context-menu", "02", "右键就够", "启动 / 打开所在文件夹 / 复制路径 / 钉桌面", 9.0),
    ("3-pin-desktop", "03", "一键钉回桌面", "选中 → 创建桌面快捷方式，自动读回校验", 10.0),
    ("4-launch", "04", "顺手启动", "双击列表项直接打开它", 8.0),
    ("5-tray-hotkey", "05", "随时呼出", "关窗只是缩到托盘；Ctrl+Alt+E 随时唤出", 9.0),
    ("6-help", "06", "不用记手册", "F1 打开帮助：快捷键表与配置格式都在里面", 7.0),
]

BRAND_DOTS = [
    (91, 141, 239), (124, 91, 239), (74, 222, 128),
    (250, 204, 21), (248, 113, 113), (56, 189, 248),
]


def font(bold: bool, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(FONT_BOLD if bold else FONT_REG, size)


def ffd(args: list[str]) -> None:
    subprocess.run([FFMPEG, "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def gif_duration(p: Path) -> float:
    """GIF 的真实时长（逐帧累加延迟——各帧延迟可能不一致）。"""
    im = Image.open(p)
    total = 0
    for i in range(im.n_frames):
        im.seek(i)
        total += im.info.get("duration", 40)
    return total / 1000


def fit_loops(gif_len: float, target: float, max_loops: int = 2) -> tuple[int, float]:
    """把段落时长对齐到 GIF 的**整数遍数**（默认最多 2 遍）。

    对齐的意义：循环接缝正好落在段落边界，观众不会看到"同一段操作
    播到一半又从头开始"；最多 2 遍则避免同一画面重复太多次。

    取整用 **floor 而不是 round**（2026-10-09 改）：GIF 修成原速后单体变长
    （4~8s），round 会把 8.5s 的目标凑成 2 遍、段落凭空翻倍膨胀；floor 让它
    保持 1 遍，段落长度自然贴合素材。
    """
    loops = max(1, min(max_loops, int(target / gif_len)))
    return loops, round(gif_len * loops, 2)


def centered(d: ImageDraw.ImageDraw, text: str, y: int, f: ImageFont.FreeTypeFont,
             fill: tuple, width: int = W) -> None:
    """把一行文字水平居中放在 (0..width) 区域内。"""
    box = d.textbbox((0, 0), text, font=f)
    d.text(((width - (box[2] - box[0])) // 2, y), text, font=f, fill=fill)


def draw_dots(d: ImageDraw.ImageDraw, cy: int, r: int = 7, gap: int = 26) -> None:
    total = len(BRAND_DOTS) * gap - (gap - 2 * r)
    x = (W - total) // 2
    for c in BRAND_DOTS:
        d.ellipse([x, cy - r, x + 2 * r, cy + r], fill=c)
        x += gap


def capsule(d: ImageDraw.ImageDraw, text: str, cy: int, pal: dict) -> None:
    """底部居中的白底圆角胶囊标签（参考片的说明文字样式）。"""
    f = font(False, 30)
    box = d.textbbox((0, 0), text, font=f)
    tw, th = box[2] - box[0], box[3] - box[1]
    pw, ph = tw + 84, 76
    x0, y0 = (W - pw) // 2, cy - ph // 2
    d.rounded_rectangle([x0, y0, x0 + pw, y0 + ph], radius=ph // 2,
                        fill=pal["capsule_bg"], outline=pal["line"], width=2)
    d.text((x0 + 42, y0 + (ph - th) // 2 - box[1]), text, font=f, fill=pal["capsule_fg"])


def icon_img(size: int) -> Image.Image | None:
    """品牌标优先用 video_assets/_logo/logo.png（新做的字标），否则退回应用图标。"""
    logo = ROOT / "video_assets" / "_logo" / "logo.png"
    if logo.exists():
        return Image.open(str(logo)).convert("RGBA").resize((size, size), Image.LANCZOS)
    if not ICON.exists():
        return None
    im = Image.open(str(ICON)).convert("RGBA")
    return im.resize((size, size), Image.LANCZOS)


def card_png(kind: str, pal: dict) -> Path:
    """开场 / 痛点 / 结尾卡（居中版式）。"""
    im = Image.new("RGB", (W, H), pal["bg"])
    d = ImageDraw.Draw(im)

    if kind == "intro":
        ic = icon_img(228)
        if ic:
            im.paste(ic, ((W - 228) // 2, 196), ic)
        centered(d, "ExeTrace", 456, font(True, 128), pal["fg"])
        centered(d, "应用历史定位器", 640, font(False, 42), pal["dim"])
        centered(d, "AI 做的软件，关掉也能找回来", 740, font(True, 38), pal["fg"])
        draw_dots(d, 860)

    elif kind == "pain":
        lines = [
            ("AI 让每个人都能做出自己的小软件", False, 46),
            ("但很多作者没做桌面快捷方式", False, 46),
            ("关掉界面，它就消失了", True, 52),
        ]
        y = 330
        for text, bold, size in lines:
            centered(d, text, y, font(bold, size), pal["fg"] if bold else pal["dim"])
            y += 104
        centered(d, "翻开始菜单、翻下载文件夹，就是找不到它在哪", y + 60,
                 font(True, 44), pal["fg"])

    else:  # end
        ic = icon_img(196)
        if ic:
            im.paste(ic, ((W - 196) // 2, 186), ic)
        centered(d, "ExeTrace", 408, font(True, 108), pal["fg"])
        centered(d, "开源免费 · 单文件 exe · 无需安装", 560, font(False, 36), pal["dim"])
        centered(d, "托盘常驻约 2 MB　|　窗口打开约 58 MB", 636, font(True, 40), pal["fg"])
        # 黑色胶囊 CTA（参考片结尾样式）
        f = font(False, 34)
        text = "github.com/joker1point/ExeTrace"
        box = d.textbbox((0, 0), text, font=f)
        pw, ph = (box[2] - box[0]) + 96, 84
        x0, y0 = (W - pw) // 2, 740
        d.rounded_rectangle([x0, y0, x0 + pw, y0 + ph], radius=ph // 2, fill=pal["cta_bg"])
        d.text((x0 + 48, y0 + (ph - (box[3] - box[1])) // 2 - box[1]), text, font=f,
               fill=pal["cta_fg"])
        draw_dots(d, 900)

    p = SHORT / f"bg_{kind}.png"
    im.save(p)
    return p


def demo_png(gif_name: str, num: str, title: str, sub: str, pal: dict, dark: bool) -> Path:
    """演示段背景：浅色 = 界面居中 + 底部胶囊；深色 = 左上标题 + 编号。"""
    im = Image.new("RGB", (W, H), pal["bg"])
    d = ImageDraw.Draw(im)
    if dark:
        gy = 205
        d.rectangle([0, 0, W, 6], fill=pal["line"])
        d.text((140, 74), num, font=font(True, 58), fill=pal["fg"])
        d.text((240, 74), title, font=font(True, 58), fill=pal["fg"])
        d.text((244, 158), sub, font=font(False, 28), fill=pal["dim"])
        d.rectangle([GX - 2, gy - 2, GX + GW + 1, gy + GH + 1], outline=pal["line"], width=3)
    else:
        gy = 78
        # 顶部一行小字（极轻，只用来说明这是第几个功能）
        centered(d, f"{num} / 06　{title}", 34, font(False, 30), pal["dim"])
        # 界面卡片：白底 + 细边 + 轻微外框
        d.rounded_rectangle([GX - 14, gy - 14, GX + GW + 14, gy + GH + 14],
                            radius=18, fill=(255, 255, 255), outline=pal["line"], width=2)
        capsule(d, sub, 1032, pal)
    p = SHORT / f"bg_{gif_name}.png"
    im.save(p)
    return p


def clip_static(name: str, png: Path, seconds: float,
                fade_in: float = 0.0, fade_out: float = 0.0) -> Path:
    """静态卡 → mp4。

    v2：加极缓慢推近（Ken Burns，整段放大 KB_ZOOM）—— "没有一帧死画面"；
    默认不再自带淡入淡出，段间过渡改由 assemble_xfade 的位移+淡化承担，
    fade_in / fade_out 只给全片首尾卡用。
    """
    out = SHORT / f"{name}.mp4"
    n = max(1, int(seconds * FPS))
    vf = (f"zoompan=z='1+{KB_ZOOM}*on/{n}':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':"
          f"d=1:s={W}x{H}:fps={FPS}")
    if fade_in > 0:
        vf += f",fade=t=in:st=0:d={fade_in}"
    if fade_out > 0:
        vf += f",fade=t=out:st={seconds - fade_out}:d={fade_out}"
    ffd([
        "-loop", "1", "-i", str(png),
        "-vf", f"{vf},format=yuv420p",
        "-t", str(seconds), "-r", str(FPS),
        "-c:v", "libx264", "-preset", "medium", "-crf", "18", str(out),
    ])
    return out


def clip_demo(gif: Path, bg: Path, seconds: float, gy: int,
              fade_in: float = 0.0, fade_out: float = 0.0) -> Path:
    """演示段 → mp4（背景卡 + GIF 叠层）。v2：段内淡入淡出默认关掉，交给 xfade。"""
    out = SHORT / f"seg_{gif.stem}.mp4"
    tail = ""
    if fade_in > 0:
        tail += f",fade=t=in:st=0:d={fade_in}"
    if fade_out > 0:
        tail += f",fade=t=out:st={seconds - fade_out}:d={fade_out}"
    ffd([
        "-loop", "1", "-i", str(bg),
        "-stream_loop", "-1", "-i", str(gif),
        "-filter_complex",
        f"[1:v]scale={GW}:{GH}:flags=lanczos,fps={FPS},format=rgba[g];"
        f"[0:v][g]overlay={GX}:{gy}:shortest=0[v];"
        f"[v]format=yuv420p{tail}",
        "-t", str(seconds), "-r", str(FPS),
        "-c:v", "libx264", "-preset", "medium", "-crf", "18", str(out),
    ])
    return out


def assemble_xfade(clips: list[Path], lenses: list[float], out: Path) -> list[float]:
    """用 xfade（左滑）把各段串成整片，返回每个交叉点的**中点时刻**（给转场音效用）。

    v2 取代原来的 concat + 段内 fade：段与段之间是「位移 + 淡化」的连续过渡，
    而不是各自淡出再淡入的换片。9 段的长滤镜链在 ffmpeg 4.3.1 上实测可用
    （2026-10-09 先两段验证：4s + 6s - 0.5s = 9.5s / 285 帧，完全符合预期）；
    若日后段数明显变多，再改成分治两两合并。
    """
    inputs: list[str] = []
    for c in clips:
        inputs += ["-i", str(c)]

    chain: list[str] = []
    prev = "[0:v]"
    cur_len = lenses[0]
    marks: list[float] = []
    for i in range(1, len(clips)):
        off = round(cur_len - XFADE, 3)
        tag = f"[v{i}]"
        chain.append(
            f"{prev}[{i}:v]xfade=transition=slideleft:duration={XFADE}:offset={off}{tag}"
        )
        marks.append(round(off + XFADE / 2, 3))     # 交叉中点 = 视觉上的"换段时刻"
        prev = tag
        cur_len = cur_len + lenses[i] - XFADE

    ffd([*inputs, "-filter_complex", ";".join(chain), "-map", prev,
         "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p", str(out)])
    return marks


def find_bgm(args: list[str]) -> Path | None:
    """外部 BGM：显式 `--bgm <file>` 优先，否则自动取 video_assets/bgm/ 下第一个音频。"""
    if "--bgm" in args:
        idx = args.index("--bgm")
        if idx + 1 < len(args):
            p = Path(args[idx + 1])
            return p if p.exists() else None
        return None
    d = ROOT / "video_assets" / "bgm"
    for ext in ("*.mp3", "*.wav", "*.m4a", "*.flac"):
        hit = sorted(d.glob(ext)) if d.exists() else []
        if hit:
            return hit[0]
    return None


def synth_audio(total: float, marks: list[float], out_wav: Path,
                bgm: Path | None = None, bgm_volume: float = 0.48) -> None:
    """合成音轨：BGM 铺底 + 每个段落边界一声 whoosh。

    bgm 给了就用它（mp3/wav 均可：ffmpeg 解码 → 裁到片长 → 淡入淡出 → 控电平），
    否则退回内置的低音 pad。whoosh 始终本地合成，绕开 ffmpeg 4.3 的滤镜版本差异。
    """
    import array
    import math
    import random
    import wave

    SR = 22050
    n = int(total * SR)
    buf = [0.0] * n
    two_pi = 2 * math.pi
    fin, fout = int(2.0 * SR), int(3.0 * SR)

    # 1) BGM 基底
    if bgm and bgm.exists():
        src = SHORT / "bgm_src.wav"
        ffd(["-i", str(bgm), "-t", f"{total:.2f}", "-ac", "1", "-ar", str(SR),
             "-af", (f"afade=t=in:st=0:d=2,"
                     f"afade=t=out:st={max(0.0, total - 3.0):.2f}:d=3,"
                     f"volume={bgm_volume}"),
             "-c:a", "pcm_s16le", str(src)])
        with wave.open(str(src), "rb") as w:
            frames = w.readframes(w.getnframes())
        arr = array.array("h")
        arr.frombytes(frames)
        for i in range(min(n, len(arr))):
            buf[i] = arr[i] / 32768.0
    else:
        for f, a in ((110.0, 0.52), (164.81, 0.32), (220.0, 0.16)):
            w = two_pi * f / SR
            for i in range(n):
                buf[i] += a * math.sin(w * i)
        for i in range(n):
            t = i / SR
            buf[i] *= (0.55 + 0.45 * math.sin(two_pi * 0.18 * t)) * 0.085
        for i in range(fin):
            buf[i] *= i / fin
        for i in range(fout):
            buf[n - 1 - i] *= i / fout

    # 2) 段落边界 whoosh
    rnd = random.Random(7)
    for m in marks:
        s = int(m * SR)
        prev = 0.0
        for k in range(int(0.45 * SR)):
            idx = s + k
            if idx >= n:
                break
            t = k / SR
            env = (min(1.0, t / 0.09) if t < 0.18
                   else max(0.0, 1.0 - (t - 0.18) / 0.27))
            white = rnd.uniform(-1.0, 1.0)
            prev = 0.94 * prev + 0.06 * white       # 一阶低通 → 偏粉噪
            buf[idx] += (white - prev) * 0.07 * env

    # 3) 写 16bit 立体声。合成 pad 没有底噪，可以放心归一化到目标峰值；
    #    外部音乐已经用 volume 控过电平，这里只做防削波。
    peak = max(1e-9, max(abs(v) for v in buf))
    scale = min(1.0, 0.95 / peak) if (bgm and bgm.exists()) else 0.70 / peak
    data = array.array("h", bytes(4 * n))
    for i, v in enumerate(buf):
        s = int(max(-1.0, min(1.0, v * scale)) * 32767)
        data[2 * i] = s
        data[2 * i + 1] = s
    with wave.open(str(out_wav), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(data.tobytes())


def attach_audio(video: Path, marks: list[float], total: float,
                 bgm: Path | None = None) -> None:
    """合成音轨并合并进成片（原地替换）。"""
    wav = SHORT / "audio.wav"
    synth_audio(total, marks, wav, bgm)
    tmp = SHORT / "with_audio.mp4"
    ffd(["-i", str(video), "-i", str(wav),
         "-map", "0:v", "-map", "1:a",
         "-c:v", "copy", "-c:a", "aac", "-b:a", "160k",
         "-shortest", str(tmp)])
    tmp.replace(video)


def main() -> int:
    args = [a for a in sys.argv[1:]]
    dark = "dark" in args
    theme = "dark" if dark else "light"
    pal = DARK if dark else LIGHT
    out_name = next((a for a in args if a.endswith(".mp4")), f"short_{theme}_v1.mp4")
    out = VA / out_name

    if SHORT.exists():
        shutil.rmtree(SHORT, ignore_errors=True)
    SHORT.mkdir(parents=True)

    missing = [g for g, *_ in DEMOS if not (GIF_DIR / f"{g}.gif").exists()]
    if missing:
        print(f"缺少 GIF：{missing}（先跑 exetrace-native/tools/demo_record.py --all）")
        return 2

    gy = 205 if dark else 78
    # v2：首尾卡自带淡入/淡出，中间段一律不带 —— 段间过渡交给 xfade 的「位移 + 淡化」
    clips: list[Path] = [clip_static("intro", card_png("intro", pal), INTRO_SEC, fade_in=0.5)]
    lenses: list[float] = [INTRO_SEC]
    clips.append(clip_static("pain", card_png("pain", pal), PAIN_SEC))
    lenses.append(PAIN_SEC)

    for gif_name, num, title, sub, target in DEMOS:
        gpath = GIF_DIR / f"{gif_name}.gif"
        glen = gif_duration(gpath)
        # 这段的尾部要与下一段交叉 XFADE 秒，目标时长补回来才不会被"吃掉"
        loops, secs = fit_loops(glen, target + XFADE)
        secs = max(secs, XFADE + 1.0)
        bg = demo_png(gif_name, num, title, sub, pal, dark)
        p = clip_demo(gpath, bg, secs, gy)
        clips.append(p)
        lenses.append(secs)
        print(f"  段 {num} {title} → {p.name}（GIF {glen:.2f}s × {loops} 遍 = {secs}s）")

    clips.append(clip_static("end", card_png("end", pal), END_SEC, fade_out=0.6))
    lenses.append(END_SEC)

    marks = assemble_xfade(clips, lenses, out)
    total = sum(lenses) - XFADE * (len(clips) - 1)

    # 音频：BGM 铺底 + 每次段间交叉的中点一声 whoosh（最后一个交叉留给结尾卡淡出）
    bgm = find_bgm(args)
    attach_audio(out, marks[:-1], total, bgm)
    if bgm:
        print(f"  BGM：{bgm.name}")

    print(f"\n成片（{theme} 主题）：{out}（约 {total:.0f} 秒，"
          f"{out.stat().st_size / 1048576:.1f} MB，含 BGM + 段间滑动转场 + 转场音效）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
