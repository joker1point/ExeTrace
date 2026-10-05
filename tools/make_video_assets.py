"""ExeTrace 宣传片「非录屏素材」生成器。

产出（1920x1080 / 30fps / H.264）：
  video_assets/title_card.mp4    大字卡「它到底装哪了？」          4s
  video_assets/mem_chart.mp4     内存对比条形图（卖点可视化）      14s
  video_assets/end_card.mp4      片尾卡（GitHub + 一键三连）       8s
  video_assets/cover.png         B站封面（1146x717）

用法：
    .venv-build\\Scripts\\python.exe tools\\make_video_assets.py
"""
from __future__ import annotations

import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "video_assets"
FRAMES = OUT / "_frames"
FFMPEG = Path(r"C:\Users\biren\anaconda3\Library\bin\ffmpeg.exe")
FONT_BOLD = r"C:\Windows\Fonts\msyhbd.ttc"
FONT_REG = r"C:\Windows\Fonts\msyh.ttc"

W, H, FPS = 1920, 1080, 30

BG = (13, 16, 23)
FG = (235, 238, 245)
DIM = (128, 137, 155)
ACCENT = (91, 141, 239)
ACCENT2 = (124, 91, 239)
GREEN = (74, 222, 128)
BAR = (52, 60, 78)
BAR_HI = (74, 222, 128)
RED = (248, 113, 113)
GRAY_BAR = (78, 86, 104)


def font(size: int, bold: bool = True) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(FONT_BOLD if bold else FONT_REG, size)


def ease_out(t: float) -> float:
    t = max(0.0, min(1.0, t))
    return 1 - (1 - t) ** 3


def ease_io(t: float) -> float:
    t = max(0.0, min(1.0, t))
    return 3 * t * t - 2 * t * t * t


def new_canvas() -> Image.Image:
    return Image.new("RGB", (W, H), BG)


def text_center(d: ImageDraw.ImageDraw, xy, s, f, fill):
    x, y = xy
    d.text((x, y), s, font=f, fill=fill, anchor="mm")


def fade(draw_fn, t: float, dur: float, start: float, end: float | None = None):
    """把 draw_fn 绘制到一层上，按时间做透明度淡入淡出（返回叠加图层）。"""
    out = t
    if out < start:
        a = 0.0
    elif end is not None and out > end:
        a = 0.0
    else:
        rise = ease_out((out - start) / dur) if dur > 0 else 1.0
        a = rise
        if end is not None and end - out < dur:
            fall = ease_out((end - out) / dur)
            a = min(rise, fall)
    layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    if a > 0.01:
        draw_fn(layer)
        alpha = layer.getchannel("A").point(lambda v: int(v * a))
        layer.putalpha(alpha)
    return layer


# --------------------------------------------------------------------- 大字卡


def gen_title_card() -> None:
    name = "title_card"
    dur = 4.0
    d_frame = FRAMES / name
    d_frame.mkdir(parents=True, exist_ok=True)

    f_main = font(104)
    n = int(dur * FPS)
    for i in range(n):
        t = i / FPS
        img = new_canvas()
        base = ImageDraw.Draw(img)

        def paint(layer: Image.Image, t=t):
            d = ImageDraw.Draw(layer)
            # 主字：轻微缩放 + 淡入
            a = ease_out(t / 0.7) if t < 0.7 else 1.0
            if t > 3.3:
                a = max(0.0, 1 - ease_out((t - 3.3) / 0.7))
            scale = 1.0 + 0.08 * (1 - ease_out(t / 0.7)) if t < 0.7 else 1.0
            if a > 0.01:
                size = int(104 * scale)
                fm = font(size)
                text_center(d, (W // 2, H // 2 - 30), "它到底装哪了？", fm,
                            (255, 255, 255))
                # 细线：从左展开
                if t > 0.35:
                    prog = ease_out((t - 0.35) / 0.5)
                    half = int(240 * prog)
                    y = H // 2 + 90
                    d.line([(W // 2 - half, y), (W // 2 + half, y)],
                           fill=ACCENT, width=5)
                # 小字
                if t > 0.9:
                    a2 = ease_out((t - 0.9) / 0.6)
                    if t > 3.3:
                        a2 = min(a2, max(0.0, 1 - ease_out((t - 3.3) / 0.7)))
                    if a2 > 0.01:
                        d.text((W // 2, H // 2 + 168),
                               "AI 做的小软件，关掉之后你还找得到吗",
                               font=font(38, bold=False), fill=DIM, anchor="mm")

        def painter(layer, t=t):
            paint(layer, t)

        if t < 0.7:
            a0 = ease_out(t / 0.7)
        elif t > 3.3:
            a0 = max(0.0, 1 - ease_out((t - 3.3) / 0.7))
        else:
            a0 = 1.0
        layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
        if a0 > 0.01:
            painter(layer)
            layer.putalpha(layer.getchannel("A").point(lambda v: int(v * a0)))
            img = Image.alpha_composite(img.convert("RGBA"), layer).convert("RGB")
        img.save(d_frame / f"f{i:04d}.png", compress_level=1)

    encode(name, dur)


# --------------------------------------------------------------------- 内存对比


ROWS = [
    ("微信", 226.0, GRAY_BAR, False),
    ("OneDrive", 15.6, GRAY_BAR, False),
    ("QuickClipboard", 13.1, GRAY_BAR, False),
    ("Everything", 0.5, GRAY_BAR, False),
    ("ExeTrace", 5.8, BAR_HI, True),
]


def gen_mem_chart() -> None:
    name = "mem_chart"
    dur = 14.0
    d_frame = FRAMES / name
    d_frame.mkdir(parents=True, exist_ok=True)

    f_title = font(56)
    f_sub = font(32, bold=False)
    f_name = font(40)
    f_val = font(44)
    f_note = font(28, bold=False)

    x_name_r = 470          # 名称右对齐位置
    x_bar = 520             # 条形起点
    max_w = 960             # 最大条宽（对应 sqrt(226)）
    row_h = 92
    top = 330

    # 数据延迟与时长
    starts = [2.0, 2.9, 3.8, 4.7, 6.2]
    grow = 1.0
    hi_t = 8.6              # ExeTrace 高亮开始
    note_t = 12.0

    n = int(dur * FPS)
    for i in range(n):
        t = i / FPS
        img = new_canvas().convert("RGBA")
        layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
        d = ImageDraw.Draw(layer)

        # 标题
        if t > 0.2:
            a = ease_out((t - 0.2) / 0.6)
            if t < 0.9:
                d.text((W // 2, 150), "常驻软件，后台占多少内存？",
                       font=f_title, fill=(255, 255, 255), anchor="mm")
                layer.putalpha(layer.getchannel("A"))
            else:
                d.text((W // 2, 150), "常驻软件，后台占多少内存？",
                       font=f_title, fill=(255, 255, 255), anchor="mm")
        if t > 0.9:
            d.text((W // 2, 214), "任务管理器实测 · 后台 / 最小化状态",
                   font=f_sub, fill=DIM, anchor="mm")

        # 条形
        for idx, (label, val, color, hi) in enumerate(ROWS):
            st = starts[idx]
            if t < st:
                continue
            prog = ease_out((t - st) / grow)
            norm = math.sqrt(val) / math.sqrt(226.0)
            w_now = int(max_w * norm * prog)
            y = top + idx * row_h
            # 名称
            col = GREEN if (hi and t > hi_t) else FG
            d.text((x_name_r, y + 26), label, font=f_name, fill=col, anchor="rm")
            # 条形底槽
            norm_full = math.sqrt(val) / math.sqrt(226.0)
            w_full = int(max_w * norm_full)
            d.rounded_rectangle([x_bar, y + 8, x_bar + w_full, y + 44],
                                radius=8, fill=(28, 33, 45))
            # 条形
            if w_now > 6:
                if hi and t > hi_t:
                    glow = 0.5 + 0.5 * math.sin((t - hi_t) * 4.5)
                    gcol = tuple(int(color[c] * (0.85 + 0.15 * glow)) for c in range(3))
                    d.rounded_rectangle([x_bar, y + 8, x_bar + w_now, y + 44],
                                        radius=8, fill=gcol)
                else:
                    d.rounded_rectangle([x_bar, y + 8, x_bar + w_now, y + 44],
                                        radius=8, fill=color)
            # 数值
            txt = f"{val:g} MB"
            if hi and t > hi_t:
                k = min(1.0, (t - hi_t) / 0.4)
                size = int(44 + 8 * k)
                d.text((x_bar + w_now + 28, y + 26), txt,
                       font=font(size), fill=GREEN, anchor="lm")
            else:
                if prog > 0.7:
                    a = (prog - 0.7) / 0.3
                    vcol = tuple(int(DIM[c] + (FG[c] - DIM[c]) * a) for c in range(3))
                    d.text((x_bar + w_now + 28, y + 26), txt,
                           font=f_val, fill=vcol, anchor="lm")

        # 高亮说明
        if t > hi_t + 0.5:
            a = ease_out((t - hi_t - 0.5) / 0.5)
            if a > 0.02:
                sub = Image.new("RGBA", (W, H), (0, 0, 0, 0))
                ds = ImageDraw.Draw(sub)
                ds.text((x_bar, top + 5 * row_h - 40),
                        "后台 5–8 MB：手机 App 级的常驻开销",
                        font=font(36), fill=GREEN)
                sub.putalpha(sub.getchannel("A").point(lambda v: int(v * a)))
                layer = Image.alpha_composite(layer, sub)
                d = ImageDraw.Draw(layer)

        # 底部注释
        if t > note_t:
            a = ease_out((t - note_t) / 0.6)
            if a > 0.02:
                sub = Image.new("RGBA", (W, H), (0, 0, 0, 0))
                ds = ImageDraw.Draw(sub)
                ds.text((W // 2, H - 78),
                        "口径：专用工作集（任务管理器「内存」列）· 本机实测",
                        font=f_note, fill=(96, 104, 122), anchor="mm")
                sub.putalpha(sub.getchannel("A").point(lambda v: int(v * a)))
                layer = Image.alpha_composite(layer, sub)
                d = ImageDraw.Draw(layer)

        # 全局淡入淡出
        if t < 0.4:
            f0 = ease_out(t / 0.4)
        elif t > dur - 0.4:
            f0 = max(0.0, 1 - ease_out((t - (dur - 0.4)) / 0.4))
        else:
            f0 = 1.0
        layer.putalpha(layer.getchannel("A").point(lambda v: int(v * f0)))
        img = Image.alpha_composite(img, layer).convert("RGB")
        img.save(d_frame / f"f{i:04d}.png", compress_level=1)

    encode(name, dur)


# --------------------------------------------------------------------- 片尾卡


def gen_end_card() -> None:
    name = "end_card"
    dur = 8.0
    d_frame = FRAMES / name
    d_frame.mkdir(parents=True, exist_ok=True)

    icon_path = ROOT / "assets" / "preview.png"
    icon = Image.open(icon_path).convert("RGBA") if icon_path.exists() else None

    f_name = font(64)
    f_sub = font(36, bold=False)
    f_gh = font(40)
    f_btn = font(34)
    f_cta = font(42)

    n = int(dur * FPS)
    for i in range(n):
        t = i / FPS
        img = new_canvas().convert("RGBA")
        layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
        d = ImageDraw.Draw(layer)

        # 图标弹入
        if icon is not None and t > 0.1:
            p = ease_out((t - 0.1) / 0.7)
            s = int(150 * (0.7 + 0.3 * p))
            ic = icon.resize((s, s), Image.LANCZOS)
            a = p
            ic.putalpha(ic.getchannel("A").point(lambda v: int(v * a)))
            layer.alpha_composite(ic, (W // 2 - s // 2, 150 + (150 - s) // 2))

        # 标题
        if t > 0.7:
            y = 360 - int(14 * (1 - ease_out((t - 0.7) / 0.6)))
            d.text((W // 2, y), "ExeTrace · 应用历史定位器",
                   font=f_name, fill=(255, 255, 255), anchor="mm")
        # 副题
        if t > 1.2:
            d.text((W // 2, 442), "开源 · 免费 · 单文件 exe · 数据全本地",
                   font=f_sub, fill=DIM, anchor="mm")
        # GitHub
        if t > 1.8:
            d.text((W // 2, 540), "GitHub：joker1point/ExeTrace",
                   font=f_gh, fill=ACCENT, anchor="mm")

        # 三连（彩色圆 + 符号 + 下方标签，符号化避免文字重复）
        btns = [("点赞", ACCENT, 2.6), ("投币", (86, 204, 242), 3.1), ("收藏", (245, 197, 66), 3.6)]
        x0 = W // 2 - 360
        for idx, (lbl, col, st) in enumerate(btns):
            if t > st:
                p = ease_out((t - st) / 0.45)
                cx = x0 + idx * 360
                r = int(52 * (0.6 + 0.4 * p))
                cy = 700 + int(18 * (1 - p))
                d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=col)
                if idx == 0:   # 点赞：上箭头（拇指意象）
                    d.polygon([(cx, cy - 22), (cx - 20, cy + 14), (cx + 20, cy + 14)],
                              fill=(13, 16, 23))
                elif idx == 1:  # 投币：圆环
                    d.ellipse([cx - 18, cy - 18, cx + 18, cy + 18],
                              outline=(13, 16, 23), width=8)
                else:          # 收藏：菱形（星形简化）
                    d.polygon([(cx, cy - 20), (cx + 18, cy), (cx, cy + 20), (cx - 18, cy)],
                              fill=(13, 16, 23))
                d.text((cx, cy + 92), lbl, font=f_btn, fill=FG, anchor="mm")

        # CTA
        if t > 4.4:
            p = ease_out((t - 4.4) / 0.7)
            y = 890 - int(16 * p)
            d.text((W // 2, y), "一键三连，支持一下", font=f_cta,
                   fill=(255, 255, 255), anchor="mm")

        # 淡入淡出
        if t < 0.3:
            f0 = ease_out(t / 0.3)
        elif t > dur - 0.5:
            f0 = max(0.0, 1 - ease_out((t - (dur - 0.5)) / 0.5))
        else:
            f0 = 1.0
        layer.putalpha(layer.getchannel("A").point(lambda v: int(v * f0)))
        img = Image.alpha_composite(img, layer).convert("RGB")
        img.save(d_frame / f"f{i:04d}.png", compress_level=1)

    encode(name, dur)


# --------------------------------------------------------------------- 封面


def gen_cover() -> None:
    cw, ch = 1146, 717
    img = Image.new("RGB", (cw, ch), BG).convert("RGBA")
    d = ImageDraw.Draw(img)

    # 顶部渐变色条
    bar_h = 10
    for x in range(cw):
        k = x / cw
        col = tuple(int(ACCENT[c] * (1 - k) + ACCENT2[c] * k) for c in range(3))
        d.line([(x, 0), (x, bar_h)], fill=col)

    icon_path = ROOT / "assets" / "preview.png"
    if icon_path.exists():
        ic = Image.open(icon_path).convert("RGBA").resize((120, 120), Image.LANCZOS)
        img.alpha_composite(ic, (72, 78))

    d.text((214, 100), "ExeTrace", font=font(52), fill=(255, 255, 255))
    d.text((216, 158), "应用历史定位器", font=font(30, bold=False), fill=DIM)

    d.text((72, 268), "关掉就找不到的软件", font=font(76), fill=(255, 255, 255))
    d.text((72, 360), "我帮你记住了", font=font(76), fill=GREEN)

    # 徽章
    badges = ["后台内存 5-8 MB", "一键钉回桌面", "开源免费 · 单文件"]
    x = 72
    for b in badges:
        f = font(30)
        tw = d.textlength(b, font=f)
        d.rounded_rectangle([x, 520, x + tw + 48, 584], radius=32,
                            fill=(28, 33, 45), outline=(52, 60, 78), width=2)
        d.text((x + 24, 552), b, font=f, fill=FG, anchor="lm")
        x += int(tw) + 72

    d.text((72, 640), "GitHub：joker1point/ExeTrace",
           font=font(28, bold=False), fill=DIM)

    out = OUT / "cover.png"
    img.convert("RGB").save(out)
    print(f"cover -> {out}")


# --------------------------------------------------------------------- 编码


def encode(name: str, dur: float) -> None:
    d_frame = FRAMES / name
    out = OUT / f"{name}.mp4"
    cmd = [
        str(FFMPEG), "-y", "-hide_banner", "-loglevel", "error",
        "-framerate", str(FPS), "-i", str(d_frame / "f%04d.png"),
        "-c:v", "libx264", "-preset", "medium", "-crf", "16",
        "-pix_fmt", "yuv420p", "-movflags", "+faststart",
        str(out),
    ]
    subprocess.run(cmd, check=True)
    size_kb = out.stat().st_size // 1024
    print(f"{name}.mp4 -> {out}  ({size_kb} KB, {dur:g}s)")


def main() -> int:
    if not FFMPEG.exists():
        print(f"ffmpeg not found: {FFMPEG}", file=sys.stderr)
        return 1
    OUT.mkdir(exist_ok=True)
    FRAMES.mkdir(exist_ok=True)

    only = sys.argv[1] if len(sys.argv) > 1 else "all"
    if only in ("all", "title"):
        gen_title_card()
    if only in ("all", "chart"):
        gen_mem_chart()
    if only in ("all", "end"):
        gen_end_card()
    if only in ("all", "cover"):
        gen_cover()

    if only == "all":
        shutil.rmtree(FRAMES, ignore_errors=True)
    print("done.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
