//! YuNet, the face detector OpenCV ships (Wu, Peng and Yu, "YuNet: A Tiny
//! Millisecond-level Face Detector", 2023; MIT, `models/LICENSE-YuNet`), run
//! on the CPU: 51 thousand weights, about 0.2 GMAC for a 640 x 360 frame.
//! It finds faces turned to the side as well as faces looking at the
//! camera. `tools/yunet_weights.py` writes the weights from the published
//! ONNX file.
//!
//! Every layer is a plain loop that adds its terms in one order, with no
//! fused multiply-adds, so its sums don't depend on the machine's vector
//! width.

use std::sync::OnceLock;

const WEIGHTS: &[u8] = include_bytes!("../models/yunet_2023mar.bin");

/// Longest side, px, frames are scaled to before detection (smaller frames
/// are used as they are).
pub const SIZE: usize = 640;

/// A face: top-left, size (px of the frame given) and score (0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

/// Channels of rows.
#[derive(Debug, Clone, Default)]
struct Map {
    c: usize,
    h: usize,
    w: usize,
    d: Vec<f32>,
}

impl Map {
    fn zeros(c: usize, h: usize, w: usize) -> Self {
        Self {
            c,
            h,
            w,
            d: vec![0.0; c * h * w],
        }
    }

    fn plane(&self, c: usize) -> &[f32] {
        let n = self.h * self.w;
        &self.d[c * n..(c + 1) * n]
    }
}

/// A convolution's weights, `[out][in][k][k]`, and biases.
struct Conv {
    co: usize,
    ci: usize,
    w: Vec<f32>,
    b: Vec<f32>,
}

/// A pointwise convolution, then a depthwise 3 x 3 one.
struct Unit {
    pw: Conv,
    dw: Conv,
}

struct Net {
    /// The first layer, 3 x 3 with stride 2, its weights summed over the
    /// colour channels: a grey frame is the same in all three.
    stem: Conv,
    /// The backbone's units, then the neck's.
    units: Vec<Unit>,
    /// Class, box and objectness heads, each at strides 8, 16 and 32.
    heads: Vec<Unit>,
}

struct Reader<'a> {
    b: &'a [u8],
}

impl Reader<'_> {
    fn u32(&mut self) -> usize {
        let (v, rest) = self.b.split_at(4);
        self.b = rest;
        u32::from_le_bytes(v.try_into().unwrap()) as usize
    }

    fn tensor(&mut self, shape: &[usize]) -> Vec<f32> {
        let rank = self.u32();
        let dims: Vec<usize> = (0..rank).map(|_| self.u32()).collect();
        assert_eq!(dims, shape, "YuNet weights: unexpected shape");
        let n: usize = dims.iter().product();
        let (v, rest) = self.b.split_at(4 * n);
        self.b = rest;
        v.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
    }

    fn conv(&mut self, co: usize, ci: usize, k: usize) -> Conv {
        Conv {
            co,
            ci,
            w: self.tensor(&[co, ci, k, k]),
            b: self.tensor(&[co]),
        }
    }

    fn unit(&mut self, ci: usize, co: usize) -> Unit {
        Unit {
            pw: self.conv(co, ci, 1),
            dw: self.conv(co, 1, 3),
        }
    }
}

impl Net {
    fn load() -> Self {
        let mut r = Reader { b: WEIGHTS };
        assert_eq!(&r.b[..8], b"YUNET\0\0\x01", "YuNet weights: not a weights file");
        r.b = &r.b[8..];
        assert_eq!(r.u32(), 94, "YuNet weights: tensor count");
        let rgb = r.conv(16, 3, 3);
        let stem = Conv {
            co: 16,
            ci: 1,
            w: (0..16 * 9)
                .map(|i| {
                    let (o, k) = (i / 9, i % 9);
                    (0..3).map(|c| rgb.w[(o * 3 + c) * 9 + k]).sum()
                })
                .collect(),
            b: rgb.b,
        };
        // model0..model5, then the neck's three lateral units.
        let chans = [(16, 16), (16, 16), (16, 32), (32, 32), (32, 64)];
        let units = chans
            .into_iter()
            .chain(std::iter::repeat_n((64, 64), 9))
            .map(|(ci, co)| r.unit(ci, co))
            .collect();
        let heads = [1, 4, 1]
            .into_iter()
            .flat_map(|co| [co; 3])
            .map(|co| r.unit(64, co))
            .collect();
        assert!(r.b.is_empty(), "YuNet weights: trailing bytes");
        Self { stem, units, heads }
    }
}

fn net() -> &'static Net {
    static NET: OnceLock<Net> = OnceLock::new();
    NET.get_or_init(Net::load)
}

/// The 3 x 3, stride 2 first layer and its ReLU, on one channel.
fn stem(x: &Map, cv: &Conv) -> Map {
    let (h, w) = (x.h / 2, x.w / 2);
    let mut out = Map::zeros(cv.co, h, w);
    let src = x.plane(0);
    // Each input row split into its even and odd columns: output column
    // `j` reads input columns 2j - 1, 2j and 2j + 1, which are odd[j - 1],
    // even[j] and odd[j].
    let split: Vec<(Vec<f32>, Vec<f32>)> = (0..x.h)
        .map(|y| {
            let row = &src[y * x.w..(y + 1) * x.w];
            (
                row.iter().step_by(2).copied().collect(),
                row.iter().skip(1).step_by(2).copied().collect(),
            )
        })
        .collect();
    for o in 0..cv.co {
        let k = &cv.w[o * 9..o * 9 + 9];
        for y in 0..h {
            let row = &mut out.d[(o * h + y) * w..(o * h + y + 1) * w];
            row.fill(cv.b[o]);
            for ky in 0..3 {
                let Some(sy) = (2 * y + ky).checked_sub(1).filter(|&sy| sy < x.h) else {
                    continue;
                };
                let (even, odd) = (&split[sy].0, &split[sy].1);
                let (k0, k1, k2) = (k[ky * 3], k[ky * 3 + 1], k[ky * 3 + 2]);
                for (d, s) in row[1..].iter_mut().zip(odd) {
                    *d += k0 * s;
                }
                for (d, s) in row.iter_mut().zip(even) {
                    *d += k1 * s;
                }
                for (d, s) in row.iter_mut().zip(odd) {
                    *d += k2 * s;
                }
            }
            for v in row.iter_mut() {
                *v = v.max(0.0);
            }
        }
    }
    out
}

/// A 1 x 1 convolution: four outputs at a time over blocks of pixels, so
/// each input value is read once per four.
fn pointwise(x: &Map, cv: &Conv) -> Map {
    debug_assert_eq!(x.c, cv.ci);
    let n = x.h * x.w;
    let mut out = Map::zeros(cv.co, x.h, x.w);
    const BLOCK: usize = 512;
    for p0 in (0..n).step_by(BLOCK) {
        let p1 = (p0 + BLOCK).min(n);
        let len = p1 - p0;
        let mut o = 0;
        while o < cv.co {
            let group = (cv.co - o).min(4);
            let mut acc = [[0f32; BLOCK]; 4];
            for (g, a) in acc.iter_mut().enumerate().take(group) {
                a[..len].fill(cv.b[o + g]);
            }
            for i in 0..cv.ci {
                let src = &x.d[i * n + p0..i * n + p1];
                if group == 4 {
                    let w = [0, 1, 2, 3].map(|g| cv.w[(o + g) * cv.ci + i]);
                    let [a0, a1, a2, a3] = &mut acc;
                    for ((((d0, d1), d2), d3), s) in a0[..len]
                        .iter_mut()
                        .zip(&mut a1[..len])
                        .zip(&mut a2[..len])
                        .zip(&mut a3[..len])
                        .zip(src)
                    {
                        *d0 += w[0] * s;
                        *d1 += w[1] * s;
                        *d2 += w[2] * s;
                        *d3 += w[3] * s;
                    }
                } else {
                    for (g, a) in acc.iter_mut().enumerate().take(group) {
                        let wv = cv.w[(o + g) * cv.ci + i];
                        for (d, s) in a[..len].iter_mut().zip(src) {
                            *d += wv * s;
                        }
                    }
                }
            }
            for (g, a) in acc.iter().enumerate().take(group) {
                out.d[(o + g) * n + p0..(o + g) * n + p1].copy_from_slice(&a[..len]);
            }
            o += group;
        }
    }
    out
}

/// A depthwise 3 x 3 convolution with padding 1, then ReLU if asked.
fn depthwise(x: &Map, cv: &Conv, relu: bool) -> Map {
    debug_assert_eq!(x.c, cv.co);
    let (h, w) = (x.h, x.w);
    let mut out = Map::zeros(x.c, h, w);
    for c in 0..x.c {
        let src = x.plane(c);
        let k = &cv.w[c * 9..c * 9 + 9];
        for y in 0..h {
            let row = &mut out.d[(c * h + y) * w..(c * h + y + 1) * w];
            row.fill(cv.b[c]);
            for ky in 0..3 {
                let Some(sy) = (y + ky).checked_sub(1).filter(|&sy| sy < h) else {
                    continue;
                };
                let s = &src[sy * w..(sy + 1) * w];
                let (k0, k1, k2) = (k[ky * 3], k[ky * 3 + 1], k[ky * 3 + 2]);
                for (d, v) in row[1..].iter_mut().zip(s) {
                    *d += k0 * v;
                }
                for (d, v) in row.iter_mut().zip(s) {
                    *d += k1 * v;
                }
                for (d, v) in row.iter_mut().zip(&s[1..]) {
                    *d += k2 * v;
                }
            }
            if relu {
                for v in row.iter_mut() {
                    *v = v.max(0.0);
                }
            }
        }
    }
    out
}

fn unit(x: &Map, u: &Unit, relu: bool) -> Map {
    depthwise(&pointwise(x, &u.pw), &u.dw, relu)
}

/// 2 x 2 max pooling, stride 2.
fn pool(x: &Map) -> Map {
    let (h, w) = (x.h / 2, x.w / 2);
    let mut out = Map::zeros(x.c, h, w);
    for c in 0..x.c {
        let src = x.plane(c);
        for y in 0..h {
            let (r0, r1) = (&src[2 * y * x.w..], &src[(2 * y + 1) * x.w..]);
            let row = &mut out.d[(c * h + y) * w..(c * h + y + 1) * w];
            for (j, d) in row.iter_mut().enumerate() {
                *d = r0[2 * j].max(r0[2 * j + 1]).max(r1[2 * j]).max(r1[2 * j + 1]);
            }
        }
    }
    out
}

/// `big` plus `small` scaled up twice (nearest).
fn up_add(small: &Map, big: &Map) -> Map {
    let mut out = big.clone();
    for c in 0..big.c {
        let s = small.plane(c);
        for y in 0..big.h {
            let row = &mut out.d[(c * big.h + y) * big.w..(c * big.h + y + 1) * big.w];
            let sr = &s[(y / 2) * small.w..];
            for (x, d) in row.iter_mut().enumerate() {
                *d += sr[x / 2];
            }
        }
    }
    out
}

fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

fn iou(a: &Face, b: &Face) -> f32 {
    let ix = ((a.x + a.w).min(b.x + b.w) - a.x.max(b.x)).max(0.0);
    let iy = ((a.y + a.h).min(b.y + b.h) - a.y.max(b.y)).max(0.0);
    let i = ix * iy;
    let u = a.w * a.h + b.w * b.h - i;
    if u > 0.0 { i / u } else { 0.0 }
}

/// Faces in a grey image (`width` x `height`, row-major) scoring at least
/// `threshold`, boxes in its px, after non-maximum suppression (IoU 0.3, as
/// OpenCV's).
pub fn detect(px: &[u8], width: usize, height: usize, threshold: f32) -> Vec<Face> {
    let net = net();
    // Scaled so the longest side is at most SIZE, then padded with black
    // to multiples of 32 at the right and bottom.
    let s = (SIZE as f32 / width.max(height) as f32).min(1.0);
    let (dw, dh) = (
        ((width as f32 * s).round() as usize).max(1),
        ((height as f32 * s).round() as usize).max(1),
    );
    let (pw, ph) = (dw.div_ceil(32) * 32, dh.div_ceil(32) * 32);
    let mut img = Map::zeros(1, ph, pw);
    let (sx, sy) = (width as f32 / dw as f32, height as f32 / dh as f32);
    for y in 0..dh {
        // Bilinear, at pixel centres.
        let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (height - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(height - 1);
        for x in 0..dw {
            let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (width - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(width - 1);
            let p = |yy: usize, xx: usize| px[yy * width + xx] as f32;
            let top = p(y0, x0) + (p(y0, x1) - p(y0, x0)) * tx;
            let bottom = p(y1, x0) + (p(y1, x1) - p(y1, x0)) * tx;
            img.d[y * pw + x] = top + (bottom - top) * ty;
        }
    }

    let u = &net.units;
    let mut x = unit(&stem(&img, &net.stem), &u[0], true);
    x = pool(&x);
    for un in &u[1..5] {
        x = unit(&x, un, true);
    }
    x = pool(&x);
    // The backbone at strides 8, 16 and 32, then the neck from the top
    // down.
    let b8 = unit(&unit(&x, &u[5], true), &u[6], true);
    let b16 = unit(&unit(&pool(&b8), &u[7], true), &u[8], true);
    let b32 = unit(&unit(&pool(&b16), &u[9], true), &u[10], true);
    let p32 = unit(&b32, &u[11], true);
    let p16 = unit(&up_add(&p32, &b16), &u[12], true);
    let p8 = unit(&up_add(&p16, &b8), &u[13], true);

    let mut faces = Vec::new();
    for (level, (feat, stride)) in [(&p8, 8.0f32), (&p16, 16.0), (&p32, 32.0)].into_iter().enumerate() {
        let cls = unit(feat, &net.heads[level], false);
        let bbox = unit(feat, &net.heads[3 + level], false);
        let obj = unit(feat, &net.heads[6 + level], false);
        let (rows, cols) = (feat.h, feat.w);
        let n = rows * cols;
        for r in 0..rows {
            for c in 0..cols {
                let i = r * cols + c;
                let score = (sigmoid(cls.d[i]).clamp(0.0, 1.0) * sigmoid(obj.d[i]).clamp(0.0, 1.0)).sqrt();
                if score < threshold {
                    continue;
                }
                let cx = (c as f32 + bbox.d[i]) * stride;
                let cy = (r as f32 + bbox.d[n + i]) * stride;
                let w = bbox.d[2 * n + i].exp() * stride;
                let h = bbox.d[3 * n + i].exp() * stride;
                faces.push(Face {
                    x: (cx - w / 2.0) / s,
                    y: (cy - h / 2.0) / s,
                    w: w / s,
                    h: h / s,
                    score,
                });
            }
        }
    }
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Face> = Vec::new();
    for f in faces {
        if kept.iter().all(|k| iou(k, &f) <= 0.3) {
            kept.push(f);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weights_load_and_a_blank_frame_has_no_faces() {
        let n = net();
        assert_eq!(n.units.len(), 14);
        assert_eq!(n.heads.len(), 9);
        assert!(detect(&vec![128u8; 320 * 180], 320, 180, 0.5).is_empty());
    }

    #[test]
    fn finds_a_face_seen_from_the_side() {
        // A two-shot from NASA's interview "Fireside Chat with Artemis
        // Launch Director Charlie Blackwell-Thompson and Apollo 11's JoAnn
        // Morgan" (public domain) at 467.5 s, grey, 480 x 90: on the left
        // a face turned almost to profile, which SeetaFace's frontal
        // cascade never found in this shot, on the right one turned three
        // quarters.
        let faces = detect(include_bytes!("../testdata/two-shot.gray"), 480, 90, 0.6);
        let mut centres: Vec<f32> = faces.iter().map(|f| f.x + f.w / 2.0).collect();
        centres.sort_by(f32::total_cmp);
        assert_eq!(centres.len(), 2, "{faces:?}");
        assert!(
            (centres[0] - 67.0).abs() < 3.0 && (centres[1] - 385.0).abs() < 3.0,
            "{faces:?}"
        );
    }

    #[test]
    fn layers_add_in_order() {
        // Pointwise: bias, then each input times its weight.
        let x = Map {
            c: 2,
            h: 1,
            w: 3,
            d: vec![1.0, 2.0, 3.0, 10.0, 20.0, 30.0],
        };
        let cv = Conv {
            co: 5,
            ci: 2,
            w: vec![1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 2.0, -1.0, 0.5, 0.5],
            b: vec![0.0, 0.0, 1.0, 0.0, -1.0],
        };
        let out = pointwise(&x, &cv);
        assert_eq!(
            out.d,
            [
                1.0, 2.0, 3.0, 10.0, 20.0, 30.0, 12.0, 23.0, 34.0, -8.0, -16.0, -24.0, 4.5, 10.0, 15.5
            ]
        );
        // Depthwise 3 x 3 with zero padding: the centre tap alone copies,
        // the right tap shifts left.
        let x = Map {
            c: 1,
            h: 2,
            w: 3,
            d: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        };
        let mut k = vec![0.0; 9];
        k[4] = 1.0;
        let centre = Conv {
            co: 1,
            ci: 1,
            w: k.clone(),
            b: vec![0.0],
        };
        assert_eq!(depthwise(&x, &centre, false).d, x.d);
        k[4] = 0.0;
        k[5] = 1.0;
        let right = Conv {
            co: 1,
            ci: 1,
            w: k,
            b: vec![-2.5],
        };
        assert_eq!(depthwise(&x, &right, true).d, [0.0, 0.5, 0.0, 2.5, 3.5, 0.0]);
        // Pooling and the upsampled sum.
        let p = pool(&Map {
            c: 1,
            h: 2,
            w: 4,
            d: vec![1.0, 5.0, 2.0, 0.0, 3.0, 4.0, 8.0, 1.0],
        });
        assert_eq!(p.d, [5.0, 8.0]);
        let big = Map::zeros(1, 2, 4);
        assert_eq!(up_add(&p, &big).d, [5.0, 5.0, 8.0, 8.0, 5.0, 5.0, 8.0, 8.0]);
    }
}
