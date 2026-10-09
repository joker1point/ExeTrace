"""生成 B 站投稿封面（1146×717，16:10）。

素材：demo 录屏里的真实界面帧（默认满列表那帧，视觉饱满，呼应"全都记住了"）。
风格：深色渐变底 + 冷色光晕 + 左侧痛点文案 + 右侧真实界面截图（微倾、投影），
和仓库的 social-preview.png 同源配色（黑底 + 彩色点）。

用法:
    python tools/make_bili_cover.py                     # 默认输出 assets/bilibili-cover.png
    python tools/make_bili_cover.py --frame <png> --out <png>
"""
from __future__ import annotations

import argparse
import pathlib

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H = 1146, 717
FONT_BOLD = r"C:\Windows\Fonts\msyhbd.ttc"
FONT_REG = r"C:\Windows\Fonts\msyh.ttc"

# 关键文字都放在中间安全区内（B 站部分位置会按 16:9 裁切，上下各吃 ~5%）
TEXT_X = 72


def font(size: int, bold: bool = True) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(FONT_BOLD if bold else FONT_REG, size)


def gradient_bg(w: int, h: int, top: tuple, bottom: tuple) -> Image.Image:
    strip = Image.new("RGB", (1, h))
    for y in range(h):
        t = y / (h - 1)
        strip.putpixel((0, y), tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)))
    return strip.resize((w, h), Image.BILINEAR).convert("RGBA")


def glow(canvas: Image.Image, center: tuple, radius: int, color: tuple, alpha: int) -> None:
    """柔和的径向光晕。

    椭圆只占图层中间一半、模糊半径也小于到边缘的距离——否则模糊会把 alpha
    糊到图层边界被硬切，画面上会留下一条可见的直角分界线。
    """
    size = radius * 2
    layer = Image.new("RGBA", (size, size), color + (0,))
    a = Image.new("L", (size, size), 0)
    r = radius // 2
    c = size // 2
    ImageDraw.Draw(a).ellipse([c - r, c - r, c + r, c + r], fill=alpha)
    a = a.filter(ImageFilter.GaussianBlur(r * 0.55))
    layer.putalpha(a)
    canvas.alpha_composite(layer, (center[0] - radius, center[1] - radius))


def build_shot(frame_path: str, width: int, tilt: float = 2.0, radius: int = 14) -> Image.Image:
    """真实界面截图 → 圆角 + 细描边 + 微倾（返回 RGBA，含旋转后的透明边角）。"""
    shot = Image.open(frame_path).convert("RGB")
    h = round(shot.height * width / shot.width)
    shot = shot.resize((width, h), Image.LANCZOS)
    mask = Image.new("L", shot.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, width - 1, h - 1], radius, fill=255)
    shot = shot.convert("RGBA")
    shot.putalpha(mask)
    d = ImageDraw.Draw(shot)
    d.rounded_rectangle([1, 1, width - 2, h - 2], radius - 1, outline=(255, 255, 255, 70), width=2)
    return shot.rotate(tilt, resample=Image.BICUBIC, expand=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--frame", default="assets/cover-frame.png",
                    help="界面截图源（默认用 demo 录屏抽出的满列表帧）")
    ap.add_argument("--out", default="assets/bilibili-cover.png")
    args = ap.parse_args()

    canvas = gradient_bg(W, H, (7, 11, 20), (16, 25, 44))
    glow(canvas, (1080, -40), 470, (37, 99, 235), 78)    # 右上角（出画）：冷蓝
    glow(canvas, (30, 740), 420, (8, 145, 178), 58)      # 左下角（出画）：青
    glow(canvas, (700, 330), 340, (109, 40, 217), 26)    # 中部：紫，低强度过渡

    d = ImageDraw.Draw(canvas)

    # 品牌（左上角小标）
    logo = Image.open("assets/logo.png").convert("RGBA").resize((46, 46), Image.LANCZOS)
    canvas.alpha_composite(logo, (TEXT_X, 52))
    d.text((TEXT_X + 46 + 12, 52 + 7), "ExeTrace", font=font(30, True), fill=(226, 232, 240, 240))

    # 右侧真实界面（先投影，再贴图）
    shot = build_shot(args.frame, 700, tilt=2.0)
    shadow = Image.new("RGBA", shot.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 155), (0, 0), shot.split()[3])
    shadow = shadow.filter(ImageFilter.GaussianBlur(18))
    sx, sy = 490, 145
    canvas.alpha_composite(shadow, (sx + 10, sy + 20))
    canvas.alpha_composite(shot, (sx, sy))

    # 左侧文案（三层：钩子 → 痛点 → 主张）
    d.text((TEXT_X, 185), "AI 做的软件", font=font(32, True), fill=(56, 189, 248, 255))
    big = font(68, True)
    d.text((TEXT_X, 240), "关掉就再也", font=big, fill=(255, 255, 255, 255))
    d.text((TEXT_X, 240 + 86), "找不到了？", font=big, fill=(255, 255, 255, 255))
    sub = font(32, False)
    d.text((TEXT_X, 458), "我做了个工具，", font=sub, fill=(168, 183, 206, 255))
    d.text((TEXT_X, 458 + 46), "把它全记住了", font=sub, fill=(168, 183, 206, 255))
    d.text((TEXT_X, 582), "1.6MB · 托盘 2MB · 开源免费",
           font=font(24, False), fill=(126, 147, 176, 255))

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    canvas.convert("RGB").save(out, optimize=True)
    print(f"封面已生成: {out.resolve()}  {canvas.size[0]}x{canvas.size[1]}  "
          f"{out.stat().st_size / 1024:.0f} KB")


if __name__ == "__main__":
    main()
