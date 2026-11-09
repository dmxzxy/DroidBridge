//! 程序化绘制的应用图标（圆角手机轮廓），供窗口图标与打包脚本生成 .ico / .icns / .png 使用。

/// 返回 n×n 的 RGBA 像素
pub fn rgba(n: u32) -> (Vec<u8>, u32) {
    let s = n as f32 / 64.0;
    let mut out = vec![0u8; (n * n * 4) as usize];
    // 圆角矩形的有符号距离
    let sd = |x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32| {
        let qx = (x - cx).abs() - (hw - r);
        let qy = (y - cy).abs() - (hh - r);
        let (ox, oy) = (qx.max(0.0), qy.max(0.0));
        (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
    };
    for py in 0..n {
        for px in 0..n {
            // 在 64×64 坐标系中计算，按像素尺寸做抗锯齿
            let (x, y) = ((px as f32 + 0.5) / s, (py as f32 + 0.5) / s);
            let aa = |d: f32| (0.5 - d * s).clamp(0.0, 1.0);
            let a_body = aa(sd(x, y, 32.0, 32.0, 19.0, 29.0, 7.0));
            let a_screen = aa(sd(x, y, 32.0, 29.0, 14.0, 21.0, 3.0));
            let a_bar = aa(sd(x, y, 32.0, 55.0, 4.0, 1.2, 1.2));
            let mix = |a: f32, b: f32| a * (1.0 - a_screen) + b * a_screen;
            let mut col = [mix(79.0, 20.0), mix(140.0, 24.0), mix(255.0, 32.0)];
            for c in col.iter_mut() {
                *c = *c * (1.0 - a_bar) + 255.0 * a_bar;
            }
            let i = ((py * n + px) * 4) as usize;
            out[i] = col[0] as u8;
            out[i + 1] = col[1] as u8;
            out[i + 2] = col[2] as u8;
            out[i + 3] = (a_body * 255.0) as u8;
        }
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
}
