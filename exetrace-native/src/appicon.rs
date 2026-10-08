//! 程序化绘制应用/托盘图标（零依赖）：墨黑圆角方块 + 左右两段断口弧 + 中心圆点。
//! 与 Python 版 exetrace/src/winutil.py 的绘制保持同一视觉语言（对照 video_assets/_logo/logo.png）。

const INK: [u8; 3] = [24, 24, 27];
const WHITE: [u8; 3] = [255, 255, 255];

pub fn make_icon_rgba(size: usize) -> (usize, usize, Vec<u8>) {
    let s = size.max(16);
    let mut buf = vec![0u8; s * s * 4];
    let f = s as f64;
    let c = f / 2.0;
    let radius = f * 0.2266;
    let ring_r = f * 0.336;
    let ring_w = f * 0.0586;
    let dot_r = f * 0.121;

    for y in 0..s {
        for x in 0..s {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            if !in_round_rect(px, py, 0.0, 0.0, f, f, radius) {
                continue;
            }
            let mut rgb = INK;

            // 到中心的极坐标（屏幕坐标 y 向下，atan2 的角随顺时针增大，与 PIL 的 arc 一致）
            let (dx, dy) = (px - c, py - c);
            let r = (dx * dx + dy * dy).sqrt();
            let mut ang = dy.atan2(dx).to_degrees();
            if ang < 0.0 {
                ang += 360.0;
            }

            // 左右两段断口弧：右侧 -62°..62°，左侧 118°..242°
            if (r - ring_r).abs() <= ring_w / 2.0
                && (ang <= 62.0 || ang >= 298.0 || (118.0..=242.0).contains(&ang))
            {
                rgb = WHITE;
            }
            // 中心圆点
            if r <= dot_r {
                rgb = WHITE;
            }

            let i = (y * s + x) * 4;
            buf[i] = rgb[0];
            buf[i + 1] = rgb[1];
            buf[i + 2] = rgb[2];
            buf[i + 3] = 255;
        }
    }
    (s, s, buf)
}

fn in_round_rect(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}
