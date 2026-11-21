//! Frame metrics on 8-bit sRGB frames (what viewers and encoders see).

use std::path::Path;

pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, sRGB-encoded, straight alpha.
    pub rgba: Vec<u8>,
}

impl Frame {
    /// Uploads the frame as an RGBA8 (sRGB-encoded) GPU texture.
    pub fn upload(&self, device: &ultrafast_gpu::Device) -> ultrafast_gpu::Texture {
        device.upload_texture(
            ultrafast_gpu::TextureDesc {
                width: self.width,
                height: self.height,
                format: ultrafast_gpu::PixelFormat::Rgba8Unorm,
                cpu_readable: false,
            },
            &self.rgba,
        )
    }

    fn pixels(&self) -> impl Iterator<Item = &[u8]> {
        self.rgba.chunks_exact(4)
    }

    /// Mean squared error over RGB, compositing both over black.
    pub fn mse(&self, o: &Frame) -> f64 {
        assert_eq!((self.width, self.height), (o.width, o.height), "frame sizes differ");
        let mut sum = 0u64;
        for (a, b) in self.pixels().zip(o.pixels()) {
            for c in 0..3 {
                let x = a[c] as i64 * a[3] as i64 / 255;
                let y = b[c] as i64 * b[3] as i64 / 255;
                sum += ((x - y) * (x - y)) as u64;
            }
        }
        sum as f64 / (self.width as f64 * self.height as f64 * 3.0)
    }

    pub fn psnr(&self, o: &Frame) -> f64 {
        let mse = self.mse(o);
        if mse <= 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0 * 255.0 / mse).log10()
        }
    }

    fn luma(&self) -> Vec<f64> {
        self.pixels()
            .map(|p| (0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64) * p[3] as f64 / 255.0)
            .collect()
    }

    /// Mean SSIM of luma over 8x8 blocks.
    pub fn ssim(&self, o: &Frame) -> f64 {
        let (w, h) = (self.width as usize, self.height as usize);
        let (a, b) = (self.luma(), o.luma());
        let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
        let mut total = 0.0;
        let mut count = 0usize;
        for by in (0..h.saturating_sub(7)).step_by(8) {
            for bx in (0..w.saturating_sub(7)).step_by(8) {
                let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for y in by..by + 8 {
                    for x in bx..bx + 8 {
                        let (p, q) = (a[y * w + x], b[y * w + x]);
                        sa += p;
                        sb += q;
                        saa += p * p;
                        sbb += q * q;
                        sab += p * q;
                    }
                }
                let n = 64.0;
                let (ma, mb) = (sa / n, sb / n);
                let (va, vb, cov) = (saa / n - ma * ma, sbb / n - mb * mb, sab / n - ma * mb);
                total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
                count += 1;
            }
        }
        if count == 0 { 1.0 } else { total / count as f64 }
    }

    /// Normalized RGB histogram, 16 bins per channel.
    pub fn histogram(&self) -> Vec<f32> {
        let mut h = vec![0f32; 48];
        for p in self.pixels() {
            for c in 0..3 {
                h[c * 16 + (p[c] as usize >> 4)] += 1.0;
            }
        }
        let total: f32 = h.iter().sum();
        h.iter_mut().for_each(|v| *v /= total.max(1e-9));
        h
    }

    pub fn mean_luma(&self) -> f64 {
        let l = self.luma();
        l.iter().sum::<f64>() / l.len().max(1) as f64 / 255.0
    }

    /// Median color of the outermost pixel ring (a background estimate).
    pub fn border_color(&self) -> [u8; 3] {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut px: Vec<[u8; 3]> = Vec::new();
        for x in 0..w {
            for y in [0, h - 1] {
                let i = (y * w + x) * 4;
                px.push([self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]);
            }
        }
        for y in 0..h {
            for x in [0, w - 1] {
                let i = (y * w + x) * 4;
                px.push([self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]);
            }
        }
        let med = |c: usize| {
            let mut v: Vec<u8> = px.iter().map(|p| p[c]).collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        [med(0), med(1), med(2)]
    }

    /// Two dominant colors inside `rect` (x, y, w, h) by 2-means: (minor,
    /// major). For a text box the minor cluster is the glyph color.
    pub fn two_colors(&self, rect: [f64; 4]) -> ([u8; 3], [u8; 3]) {
        let (w, h) = (self.width as i64, self.height as i64);
        let x0 = (rect[0] as i64).clamp(0, w - 1);
        let y0 = (rect[1] as i64).clamp(0, h - 1);
        let x1 = ((rect[0] + rect[2]) as i64).clamp(x0 + 1, w);
        let y1 = ((rect[1] + rect[3]) as i64).clamp(y0 + 1, h);
        let mut px: Vec<[f64; 3]> = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                px.push([self.rgba[i] as f64, self.rgba[i + 1] as f64, self.rgba[i + 2] as f64]);
            }
        }
        let lum = |p: &[f64; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        // Seed with the darkest and brightest pixels.
        let mut c = [
            *px.iter()
                .min_by(|a, b| lum(a).partial_cmp(&lum(b)).unwrap())
                .unwrap_or(&[0.0; 3]),
            *px.iter()
                .max_by(|a, b| lum(a).partial_cmp(&lum(b)).unwrap())
                .unwrap_or(&[255.0; 3]),
        ];
        let mut counts = [0usize; 2];
        for _ in 0..10 {
            let mut sums = [[0.0f64; 3]; 2];
            counts = [0, 0];
            for p in &px {
                let d = |q: &[f64; 3]| (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f64>();
                let k = if d(&c[0]) <= d(&c[1]) { 0 } else { 1 };
                counts[k] += 1;
                for j in 0..3 {
                    sums[k][j] += p[j];
                }
            }
            for k in 0..2 {
                if counts[k] > 0 {
                    c[k] = sums[k].map(|s| s / counts[k] as f64);
                }
            }
        }
        let to = |p: [f64; 3]| p.map(|v| v.round().clamp(0.0, 255.0) as u8);
        if counts[0] <= counts[1] {
            (to(c[0]), to(c[1]))
        } else {
            (to(c[1]), to(c[0]))
        }
    }

    pub fn mean_srgb(&self) -> [u8; 3] {
        let mut s = [0u64; 3];
        for p in self.pixels() {
            for c in 0..3 {
                s[c] += p[c] as u64;
            }
        }
        let n = (self.width as u64 * self.height as u64).max(1);
        [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8]
    }

    /// Reads a PNG (8-bit gray, RGB or RGBA) as an RGBA frame.
    pub fn read_png(path: &Path) -> Result<Frame, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut dec = png::Decoder::new(std::io::BufReader::new(file));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(|e| e.to_string())?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
        let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let px = &buf[..info.buffer_size()];
        let rgba: Vec<u8> = match info.color_type {
            png::ColorType::Rgba => px.to_vec(),
            png::ColorType::Rgb => px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => px.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
            png::ColorType::Grayscale => px.iter().flat_map(|&v| [v, v, v, 255]).collect(),
            other => return Err(format!("{}: unsupported PNG color type {other:?}", path.display())),
        };
        Ok(Frame {
            width: info.width,
            height: info.height,
            rgba,
        })
    }

    pub fn write_png(&self, path: &Path) -> Result<(), String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .and_then(|mut w| w.write_image_data(&self.rgba))
            .map_err(|e| e.to_string())
    }
}

/// [ours | reference | difference (amplified)] side by side.
pub fn side_by_side(ours: &Frame, theirs: &Frame) -> Frame {
    let (w, h) = (ours.width as usize, ours.height as usize);
    let mut out = vec![0u8; w * 3 * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let a = &ours.rgba[i..i + 4];
            let b = &theirs.rgba[i..i + 4];
            let row = y * w * 3;
            out[(row + x) * 4..(row + x) * 4 + 4].copy_from_slice(a);
            out[(row + w + x) * 4..(row + w + x) * 4 + 4].copy_from_slice(b);
            let d = (0..3)
                .map(|c| (a[c] as i32 - b[c] as i32).unsigned_abs())
                .max()
                .unwrap_or(0);
            let v = (d * 4).min(255) as u8;
            // Heat: black (equal) through red to yellow.
            let px = [v, v.saturating_sub(128).saturating_mul(2), 0, 255];
            out[(row + 2 * w + x) * 4..(row + 2 * w + x) * 4 + 4].copy_from_slice(&px);
        }
    }
    Frame {
        width: (w * 3) as u32,
        height: h as u32,
        rgba: out,
    }
}

/// Grid of equally sized frames.
pub fn contact_sheet(frames: &[Frame], columns: usize) -> Frame {
    let Some(first) = frames.first() else {
        return Frame {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        };
    };
    let (fw, fh) = (first.width as usize, first.height as usize);
    let cols = columns.min(frames.len()).max(1);
    let rows = frames.len().div_ceil(cols);
    let gap = 6;
    let (w, h) = (cols * fw + (cols + 1) * gap, rows * fh + (rows + 1) * gap);
    let mut out = vec![20u8; w * h * 4];
    out.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    for (i, f) in frames.iter().enumerate() {
        let (ox, oy) = (gap + (i % cols) * (fw + gap), gap + (i / cols) * (fh + gap));
        for y in 0..fh {
            let src = &f.rgba[y * fw * 4..(y + 1) * fw * 4];
            let d = ((oy + y) * w + ox) * 4;
            out[d..d + fw * 4].copy_from_slice(src);
        }
    }
    Frame {
        width: w as u32,
        height: h as u32,
        rgba: out,
    }
}
