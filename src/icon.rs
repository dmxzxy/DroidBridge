//! 程序化绘制的应用图标，供窗口、托盘图标与打包脚本生成 .ico / .icns / .png 使用。
//!
//! 图形只有三段曲线：一部手机的圆角轮廓，和它右侧逐渐变细的两道“回声”
//! （同样形状、逐级放大的轮廓的右半边），表示手机正被连接、画面正在传出去。
//! 应用图标为黑色圆角方块 + 白色线条；托盘图标（macOS 菜单栏）只用线条。

use std::f32::consts::PI;

type Pt = (f32, f32);

/// 圆角矩形轮廓上的采样点（起点在左边中点，绕一圈）
fn rrect(cx: f32, cy: f32, hw: f32, hh: f32, r: f32, n: usize) -> Vec<Pt> {
    let (sx, sy) = (hw - r, hh - r);
    let q = PI * r / 2.0;
    let per = 4.0 * sx + 4.0 * sy + 4.0 * q;
    // 从右边中点开始按弧长依次是：右边下半、右下角、下边、左下角、左边、左上角、上边、右上角、右边上半
    let segs = [sy, q, 2.0 * sx, q, 2.0 * sy, q, 2.0 * sx, q, sy];
    (0..=n)
        .map(|i| {
            let mut t = ((i as f32 / n as f32 + 0.5) % 1.0) * per;
            for (k, &len) in segs.iter().enumerate() {
                if t <= len + 1e-4 || k == segs.len() - 1 {
                    let f = if len > 0.0 { (t / len).min(1.0) } else { 0.0 };
                    let corner = |ox: f32, oy: f32, a0: f32| {
                        let a = a0 + f * PI / 2.0;
                        (cx + ox + r * a.cos(), cy + oy + r * a.sin())
                    };
                    return match k {
                        0 => (cx + hw, cy + f * sy),
                        1 => corner(sx, sy, 0.0),
                        2 => (cx + sx - f * 2.0 * sx, cy + hh),
                        3 => corner(-sx, sy, PI / 2.0),
                        4 => (cx - hw, cy + sy - f * 2.0 * sy),
                        5 => corner(-sx, -sy, PI),
                        6 => (cx - sx + f * 2.0 * sx, cy - hh),
                        7 => corner(sx, -sy, 1.5 * PI),
                        _ => (cx + hw, cy - sy + f * sy),
                    };
                }
                t -= len;
            }
            (cx + hw, cy)
        })
        .collect()
}

/// 三段曲线（64×64 坐标系）：(折线, 线宽)
/// bold：线宽倍数（托盘图标尺寸小，线条加粗）
fn glyph(bold: f32) -> Vec<(Vec<Pt>, f32)> {
    let (cx, cy) = (23.5, 32.0);
    let (hw, hh, r) = (7.0, 14.5, 4.5);
    let gap = 9.0;
    let phone = rrect(cx, cy, hw, hh, r, 400);
    let echo = |k: f32, cut: f32, n: usize| {
        let (w, h) = (hw + k * gap, hh + k * gap * 0.5);
        rrect(cx, cy, w, h, r + k * gap * 0.9, n).into_iter().filter(|p| p.0 > cx + w * cut).collect::<Vec<_>>()
    };
    vec![(phone, 4.8 * bold), (echo(1.0, 0.55, 600), 4.0 * bold), (echo(2.0, 0.65, 800), 3.3 * bold)]
}

fn seg_dist(p: Pt, a: Pt, b: Pt) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (p.0 - a.0, p.1 - a.1);
    let l = vx * vx + vy * vy;
    let t = if l > 0.0 { ((wx * vx + wy * vy) / l).clamp(0.0, 1.0) } else { 0.0 };
    let (dx, dy) = (wx - t * vx, wy - t * vy);
    (dx * dx + dy * dy).sqrt()
}

/// 线条覆盖率（0..1），n×n；画面对应 64×64 坐标系中以 (ox, oy) 为左上角、边长 size 的区域
fn coverage(n: u32, ox: f32, oy: f32, size: f32, bold: f32) -> Vec<f32> {
    let s = n as f32 / size;
    let curves = glyph(bold);
    let mut out = vec![0f32; (n * n) as usize];
    for (pts, w) in &curves {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in pts {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        let m = w / 2.0 + 1.0;
        let px0 = (((x0 - m - ox) * s).floor().max(0.0)) as u32;
        let py0 = (((y0 - m - oy) * s).floor().max(0.0)) as u32;
        let px1 = ((((x1 + m - ox) * s).ceil().max(0.0)) as u32).min(n);
        let py1 = ((((y1 + m - oy) * s).ceil().max(0.0)) as u32).min(n);
        for py in py0..py1 {
            for px in px0..px1 {
                let p = ((px as f32 + 0.5) / s + ox, (py as f32 + 0.5) / s + oy);
                let d = pts.windows(2).map(|ab| seg_dist(p, ab[0], ab[1])).fold(f32::MAX, f32::min) - w / 2.0;
                let a = (0.5 - d * s).clamp(0.0, 1.0);
                let o = &mut out[(py * n + px) as usize];
                *o = o.max(a);
            }
        }
    }
    out
}

/// 应用图标：黑色圆角方块 + 白色线条，返回 n×n 的 RGBA 像素
pub fn rgba(n: u32) -> (Vec<u8>, u32) {
    rgba_colored(n, 18.0, 250.0)
}

/// 反色的应用图标（浅色方块 + 黑色线条），用在深色的任务栏 / 面板上
pub fn rgba_inverted(n: u32) -> (Vec<u8>, u32) {
    rgba_colored(n, 242.0, 18.0)
}

fn rgba_colored(n: u32, bg: f32, fg: f32) -> (Vec<u8>, u32) {
    let s = n as f32 / 64.0;
    let g = coverage(n, 0.0, 0.0, 64.0, 1.0);
    let mut out = vec![0u8; (n * n * 4) as usize];
    // 方块留 2 个单位边距（Windows / Linux 上与其他图标大小协调）；圆角约为边长的 22%
    let (half, rad) = (30.0f32, 13.5f32);
    for py in 0..n {
        for px in 0..n {
            let (x, y) = ((px as f32 + 0.5) / s, (py as f32 + 0.5) / s);
            let qx = ((x - 32.0).abs() - (half - rad)).max(0.0);
            let qy = ((y - 32.0).abs() - (half - rad)).max(0.0);
            let d = (qx * qx + qy * qy).sqrt() - rad;
            let tile = (0.5 - d * s).clamp(0.0, 1.0);
            let a = g[(py * n + px) as usize];
            let c = (bg * (1.0 - a) + fg * a) as u8;
            let i = ((py * n + px) * 4) as usize;
            out[i..i + 4].copy_from_slice(&[c, c, c, (tile * 255.0) as u8]);
        }
    }
    (out, n)
}

/// 托盘图标（只有线条，透明背景）：dark=true 为白色线条（深色任务栏），否则黑色。
/// macOS 菜单栏使用黑色版本作为模板图标，系统会按明暗模式自动着色。
pub fn tray_rgba(n: u32, dark: bool) -> (Vec<u8>, u32) {
    // 托盘里没有底板：放大线条，铺满图标区域
    let g = coverage(n, 8.0, 4.0, 56.0, 1.3);
    let c = if dark { 255 } else { 0 };
    let mut out = vec![0u8; (n * n * 4) as usize];
    for (i, a) in g.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&[c, c, c, (a * 255.0) as u8]);
    }
    (out, n)
}

/// 未压缩 PNG 编码（不依赖压缩库，体积较大但无妨）
pub fn png(n: u32) -> Vec<u8> {
    let (rgba, _) = rgba(n);
    let mut raw = Vec::with_capacity(rgba.len() + n as usize);
    for row in rgba.chunks((n * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib 存储块（不压缩）
    let mut z = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(u8::from(last));
        let len = chunk.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&n.to_be_bytes());
    ihdr.extend_from_slice(&n.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = 0xffff_ffffu32;
    for &x in kind.iter().chain(data) {
        crc ^= x as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}

/// Windows .ico（内嵌 PNG，含 16–256 多种尺寸）
pub fn ico() -> Vec<u8> {
    let sizes = [16u32, 32, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| png(s)).collect();
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, p) in sizes.iter().zip(&pngs) {
        let d = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[d, d, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += p.len() as u32;
    }
    for p in pngs {
        out.extend_from_slice(&p);
    }
    out
}

/// macOS .icns（内嵌 PNG：128、256、512）
pub fn icns() -> Vec<u8> {
    let entries: [(&[u8; 4], u32); 3] = [(b"ic07", 128), (b"ic08", 256), (b"ic09", 512)];
    let mut body = Vec::new();
    for (kind, size) in entries {
        let p = png(size);
        body.extend_from_slice(kind);
        body.extend_from_slice(&(p.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(&p);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn png_header() {
        let p = super::png(16);
        assert_eq!(&p[1..4], b"PNG");
        assert_eq!(&super::ico()[..4], &[0, 0, 1, 0]);
    }

    #[test]
    fn glyph_is_drawn() {
        // 三段曲线都在画面内且有足够的笔画
        let (px, n) = super::rgba(64);
        let white = px.chunks(4).filter(|c| c[0] > 200 && c[3] > 200).count();
        assert!(white > 300 && white < 1600, "{white}");
        // 四角透明（圆角方块）
        assert_eq!(px[3], 0);
        assert_eq!(px[((n * n - 1) * 4 + 3) as usize], 0);
        let (t, _) = super::tray_rgba(32, false);
        assert!(t.chunks(4).any(|c| c[3] > 200));
    }
}
