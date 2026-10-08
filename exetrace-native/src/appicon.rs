//! 程序化绘制应用/托盘图标（零依赖）：蓝色渐变圆角底 + 白色光标箭头 + 记录线。
//! 与 Python 版 exetrace/src/winutil.py 的绘制保持同一视觉语言。

pub fn make_icon_rgba(size: usize) -> (usize, usize, Vec<u8>) {
    let s = size.max(16);
    let mut buf = vec![0u8; s * s * 4];
    let f = s as f64;
    for y in 0..s {
        for x in 0..s {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let pad = f * 0.045;
            let radius = f * 0.235;
            if !in_round_rect(px, py, pad, pad, f - pad, f - pad, radius) {
                continue;
            }
            let t = py / f;
            let mut rgb = [
                lerp(59.0, 29.0, t),
                lerp(130.0, 63.0, t),
                lerp(246.0, 168.0, t),
            ];

            // 光标箭头
            let arrow = [
                (0.30, 0.155), (0.30, 0.705), (0.425, 0.575), (0.535, 0.825),
                (0.640, 0.770), (0.525, 0.530), (0.670, 0.510),
            ];
            let poly: Vec<(f64, f64)> = arrow.iter().map(|(a, b)| (a * f, b * f)).collect();
            if in_polygon(px, py, &poly) {
                rgb = [255.0, 255.0, 255.0];
            }
            // 底部记录线
            if py > f * 0.875 && py < f * 0.925 && px > f * 0.27 && px < f * 0.73 {
                rgb = [235.0, 240.0, 255.0];
            }

            let i = (y * s + x) * 4;
            buf[i] = rgb[0] as u8;
            buf[i + 1] = rgb[1] as u8;
            buf[i + 2] = rgb[2] as u8;
            buf[i + 3] = 255;
        }
    }
    (s, s, buf)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn in_round_rect(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn in_polygon(px: f64, py: f64, poly: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}
