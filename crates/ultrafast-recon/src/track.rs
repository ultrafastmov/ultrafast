//! Layer tracking: where a known layer (its rendered coverage and color)
//! sits in each reference frame, and how strongly it shows, over a known
//! background.
//!
//! Per frame the model is `ref = bg + p * A(x - d) * (fg - bg)` in linear
//! light, with offset `d` and presence `p` (opacity). `p` has a closed form
//! for any `d`; `d` is refined by Nelder-Mead from the previous frame's
//! offset, and found by a coarse global search (quarter resolution,
//! sum-of-squares over every placement) when the layer was lost.

use crate::metrics::Frame;
use crate::nelder_mead;
use ultrafast_render::ops::srgb_to_linear;

/// One frame's solution.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub t: f64,
    /// Offset from where the layer was rendered, pixels.
    pub offset: [f64; 2],
    /// Presence (opacity), 0..1.
    pub presence: f64,
    /// Unexplained energy over the layer's own, 0 = perfect.
    pub residual: f64,
}

pub struct Tracker {
    w: usize,
    h: usize,
    /// Layer coverage (full frame) and its ink box.
    alpha: Vec<f32>,
    bbox: [usize; 4],
    fg: [f32; 3],
    /// Background, linear.
    bg: Vec<[f32; 3]>,
    lut: [f32; 256],
    last: Option<[f64; 2]>,
    prev: Option<[f64; 2]>,
    pub samples: Vec<Sample>,
}

const Q: usize = 4;

impl Tracker {
    /// `layer` and `background` are RGBA8 renders of the layer alone and of
    /// the background alone; `fg` is the layer's color (linear).
    pub fn new(layer: &[u8], background: &[u8], fg: [f32; 3], w: usize, h: usize) -> Self {
        let lut: [f32; 256] = std::array::from_fn(|i| srgb_to_linear(i as f64 / 255.0) as f32);
        let alpha: Vec<f32> = layer.chunks_exact(4).map(|p| p[3] as f32 / 255.0).collect();
        let mut bbox = [w, h, 0, 0];
        for y in 0..h {
            for x in 0..w {
                if alpha[y * w + x] > 0.0 {
                    bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x + 1), bbox[3].max(y + 1)];
                }
            }
        }
        let bg = background
            .chunks_exact(4)
            .map(|p| [0, 1, 2].map(|c| lut[p[c] as usize]))
            .collect();
        Self {
            w,
            h,
            alpha,
            bbox,
            fg,
            bg,
            lut,
            last: None,
            prev: None,
            samples: Vec::new(),
        }
    }

    fn a_at(&self, x: f64, y: f64) -> f32 {
        // Bilinear, zero outside.
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = ((x - x0) as f32, (y - y0) as f32);
        let get = |xi: i64, yi: i64| -> f32 {
            if xi < 0 || yi < 0 || xi >= self.w as i64 || yi >= self.h as i64 {
                0.0
            } else {
                self.alpha[yi as usize * self.w + xi as usize]
            }
        };
        let (xi, yi) = (x0 as i64, y0 as i64);
        let top = get(xi, yi) * (1.0 - tx) + get(xi + 1, yi) * tx;
        let bot = get(xi, yi + 1) * (1.0 - tx) + get(xi + 1, yi + 1) * tx;
        top * (1.0 - ty) + bot * ty
    }

    /// (presence, residual energy, layer energy, explained energy) at
    /// offset `d`. Placements are compared by explained energy: residual
    /// alone would reward moving the layer onto empty background.
    fn solve(&self, frame: &Frame, d: [f64; 2]) -> (f64, f64, f64, f64) {
        let [bx0, by0, bx1, by1] = self.bbox;
        if bx1 <= bx0 {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let x0 = ((bx0 as f64 + d[0]).floor() as i64 - 1).max(0) as usize;
        let y0 = ((by0 as f64 + d[1]).floor() as i64 - 1).max(0) as usize;
        let x1 = ((bx1 as f64 + d[0]).ceil() as i64 + 1).clamp(0, self.w as i64) as usize;
        let y1 = ((by1 as f64 + d[1]).ceil() as i64 + 1).clamp(0, self.h as i64) as usize;
        let (mut num, mut den, mut rr) = (0.0f64, 0.0f64, 0.0f64);
        for y in y0..y1 {
            for x in x0..x1 {
                let a = self.a_at(x as f64 - d[0], y as f64 - d[1]);
                let i = y * self.w + x;
                let b = self.bg[i];
                for c in 0..3 {
                    let r = self.lut[frame.rgba[i * 4 + c] as usize] - b[c];
                    let e = a * (self.fg[c] - b[c]);
                    num += (e * r) as f64;
                    den += (e * e) as f64;
                    rr += (r * r) as f64;
                }
            }
        }
        if den < 1e-9 {
            return (0.0, rr, 0.0, 0.0);
        }
        let p = (num / den).clamp(0.0, 1.0);
        // |r - p e|^2 = rr - 2 p num + p^2 den
        let explained = 2.0 * p * num - p * p * den;
        let res = (rr - explained).max(0.0);
        (p, res, den, explained)
    }

    /// Best placement over the whole frame at quarter resolution, allowing
    /// the layer to hang off the frame by up to half.
    fn global(&self, frame: &Frame) -> Option<[f64; 2]> {
        let (sw, sh) = (self.w / Q, self.h / Q);
        let [bx0, by0, bx1, by1] = self.bbox;
        if bx1 <= bx0 || sw == 0 || sh == 0 {
            return None;
        }
        // How much each block looks like the layer's color over the
        // background: the projection of (R - B) on (F - B), damped by what
        // the projection leaves unexplained (other colors are not the layer).
        let mut t = vec![0.0f32; sw * sh];
        for sy in 0..sh {
            for sx in 0..sw {
                let (mut r, mut b) = ([0.0f32; 3], [0.0f32; 3]);
                for y in sy * Q..sy * Q + Q {
                    for x in sx * Q..sx * Q + Q {
                        let i = y * self.w + x;
                        for c in 0..3 {
                            r[c] += self.lut[frame.rgba[i * 4 + c] as usize];
                            b[c] += self.bg[i][c];
                        }
                    }
                }
                let n = (Q * Q) as f32;
                let (mut num, mut den) = (0.0f32, 0.0f32);
                let mut e = [0.0f32; 3];
                let mut dr = [0.0f32; 3];
                for c in 0..3 {
                    e[c] = self.fg[c] - b[c] / n;
                    dr[c] = r[c] / n - b[c] / n;
                    num += dr[c] * e[c];
                    den += e[c] * e[c];
                }
                if den <= 1e-6 {
                    continue;
                }
                let proj = num / den;
                let perp: f32 = (0..3).map(|c| (dr[c] - proj * e[c]).powi(2)).sum();
                t[sy * sw + sx] = proj.clamp(0.0, 1.0) * (-perp / (0.05 * den + 1e-4)).exp();
            }
        }
        // The layer at quarter resolution.
        let (ax0, ay0) = (bx0 / Q, by0 / Q);
        let (aw, ah) = ((bx1.div_ceil(Q) - ax0).max(1), (by1.div_ceil(Q) - ay0).max(1));
        let mut a = vec![0.0f32; aw * ah];
        for j in 0..ah {
            for i in 0..aw {
                let mut s = 0.0;
                for y in (ay0 + j) * Q..((ay0 + j) * Q + Q).min(self.h) {
                    for x in (ax0 + i) * Q..((ax0 + i) * Q + Q).min(self.w) {
                        s += self.alpha[y * self.w + x];
                    }
                }
                a[j * aw + i] = s / (Q * Q) as f32;
            }
        }
        let aa: f32 = a.iter().map(|v| v * v).sum();
        if aa <= 0.0 {
            return None;
        }
        // Integral image of T^2 for the window sums.
        let mut ii = vec![0.0f64; (sw + 1) * (sh + 1)];
        for y in 0..sh {
            for x in 0..sw {
                let v = (t[y * sw + x] * t[y * sw + x]) as f64;
                ii[(y + 1) * (sw + 1) + x + 1] =
                    v + ii[y * (sw + 1) + x + 1] + ii[(y + 1) * (sw + 1) + x] - ii[y * (sw + 1) + x];
            }
        }
        let rect_sum = |x0: usize, y0: usize, x1: usize, y1: usize| {
            ii[y1 * (sw + 1) + x1] - ii[y0 * (sw + 1) + x1] - ii[y1 * (sw + 1) + x0] + ii[y0 * (sw + 1) + x0]
        };
        // Normalized cross-correlation over the in-frame part: shape, not
        // amplitude (fades). At least half the layer must be in frame and a
        // fifth of it showing.
        let (aw_i, ah_i) = (aw as i64, ah as i64);
        let mut best: Option<(f64, i64, i64)> = None;
        for v in -(ah_i / 2)..=(sh as i64 - ah_i / 2) {
            for u in -(aw_i / 2)..=(sw as i64 - aw_i / 2) {
                let (j0, j1) = ((-v).max(0), (sh as i64 - v).min(ah_i));
                let (i0, i1) = ((-u).max(0), (sw as i64 - u).min(aw_i));
                if j1 <= j0 || i1 <= i0 {
                    continue;
                }
                let (mut at, mut a2) = (0.0f32, 0.0f32);
                for j in j0..j1 {
                    let ty = (v + j) as usize;
                    let row = &t[ty * sw + (u + i0) as usize..ty * sw + (u + i1) as usize];
                    let arow = &a[j as usize * aw + i0 as usize..j as usize * aw + i1 as usize];
                    for (x, y) in row.iter().zip(arow) {
                        at += x * y;
                        a2 += y * y;
                    }
                }
                if a2 < 0.5 * aa || at < 0.2 * a2 {
                    continue;
                }
                let win = rect_sum(
                    (u + i0) as usize,
                    (v + j0) as usize,
                    (u + i1) as usize,
                    (v + j1) as usize,
                );
                let ncc = at as f64 / (a2 as f64 * win).sqrt().max(1e-12);
                if best.is_none_or(|b| ncc > b.0) {
                    best = Some((ncc, u, v));
                }
            }
        }
        best.map(|(_, u, v)| [(u - ax0 as i64) as f64 * Q as f64, (v - ay0 as i64) as f64 * Q as f64])
    }

    fn refine(&self, frame: &Frame, start: [f64; 2], step: f64, iters: usize) -> ([f64; 2], f64, f64, f64) {
        let (x, _) = nelder_mead(&start, &[step, step], iters, |d| Ok(-self.solve(frame, [d[0], d[1]]).3)).unwrap();
        let d = [x[0], x[1]];
        let (p, res, den, _) = self.solve(frame, d);
        (d, p, res, den)
    }

    /// Fraction of the layer's ink box inside the frame at `offset`.
    fn in_frame(&self, offset: [f64; 2]) -> f64 {
        let [x0, y0, x1, y1] = self.bbox.map(|v| v as f64);
        let area = ((x1 - x0) * (y1 - y0)).max(1e-9);
        let ix = ((x1 + offset[0]).min(self.w as f64) - (x0 + offset[0]).max(0.0)).max(0.0);
        let iy = ((y1 + offset[1]).min(self.h as f64) - (y0 + offset[1]).max(0.0)).max(0.0);
        ix * iy / area
    }

    /// Orders the samples in time, removes single-frame presence glitches,
    /// and explains frames where the layer is missing because it is off
    /// frame (entering or leaving) by extrapolating its motion instead of
    /// fading it.
    pub fn finish(&mut self) {
        self.samples.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
        let n = self.samples.len();
        if n >= 3 {
            let p: Vec<f64> = self.samples.iter().map(|s| s.presence).collect();
            for i in 1..n - 1 {
                let mut w = [p[i - 1], p[i], p[i + 1]];
                w.sort_by(|a, b| a.partial_cmp(b).unwrap());
                self.samples[i].presence = w[1];
            }
        }
        let visible: Vec<usize> = (0..n).filter(|&i| self.samples[i].presence >= 0.05).collect();
        let (Some(&first), Some(&last)) = (visible.first(), visible.last()) else {
            return;
        };
        let velocity = |samples: &[Sample], a: usize, b: usize| {
            let (sa, sb) = (samples[a], samples[b]);
            let dt = (sb.t - sa.t).max(1e-9);
            [(sb.offset[0] - sa.offset[0]) / dt, (sb.offset[1] - sa.offset[1]) / dt]
        };
        // Entering: missing before `first`, and at `first` partly off frame.
        if first > 0 && visible.len() >= 2 && self.in_frame(self.samples[first].offset) < 0.97 {
            let v = velocity(&self.samples, first, visible[1]);
            let (t0, o0) = (self.samples[first].t, self.samples[first].offset);
            for i in 0..first {
                let s = &mut self.samples[i];
                s.offset = [o0[0] + v[0] * (s.t - t0), o0[1] + v[1] * (s.t - t0)];
                s.presence = 1.0;
                s.residual = 0.0;
            }
        }
        // Leaving: missing after `last`, and at `last` partly off frame.
        if last + 1 < n && visible.len() >= 2 && self.in_frame(self.samples[last].offset) < 0.97 {
            let v = velocity(&self.samples, visible[visible.len() - 2], last);
            let (t0, o0) = (self.samples[last].t, self.samples[last].offset);
            for i in last + 1..n {
                let s = &mut self.samples[i];
                s.offset = [o0[0] + v[0] * (s.t - t0), o0[1] + v[1] * (s.t - t0)];
                s.presence = 1.0;
                s.residual = 0.0;
            }
        }
    }

    /// Restarts tracking from a known offset (for tracking backwards from
    /// the frame the layer was matched in).
    pub fn seed(&mut self, offset: [f64; 2]) {
        self.last = Some(offset);
        self.prev = None;
    }

    /// Solves frame `t` (frames should come in time order, either way).
    pub fn step(&mut self, frame: &Frame, t: f64) -> Sample {
        let normalized = |p: f64, res: f64, den: f64| {
            if p * p * den > 1e-9 {
                res / (p * p * den)
            } else {
                f64::INFINITY
            }
        };
        let good = |p: f64, res: f64, den: f64| p >= 0.05 && normalized(p, res, den) <= 0.5;
        let last = self.last.unwrap_or([0.0, 0.0]);
        // Constant-velocity prediction, searched with a step that scales
        // with the speed.
        let velocity = match (self.last, self.prev) {
            (Some(l), Some(p)) => [l[0] - p[0], l[1] - p[1]],
            _ => [0.0, 0.0],
        };
        let predicted = [last[0] + velocity[0], last[1] + velocity[1]];
        let speed = velocity[0].hypot(velocity[1]);
        let (mut d, mut p, mut res, mut den) = self.refine(frame, predicted, (speed / 3.0).max(0.75), 40);
        if !good(p, res, den) && speed > 0.0 {
            let (d2, p2, res2, den2) = self.refine(frame, last, 0.75, 40);
            if self.solve(frame, d2).3 > self.solve(frame, d).3 {
                (d, p, res, den) = (d2, p2, res2, den2);
            }
        }
        // Lost (or never found): search the whole frame, and take what it
        // finds only if it really is the layer.
        if !good(p, res, den)
            && let Some(g) = self.global(frame)
        {
            let (d2, p2, res2, den2) = self.refine(frame, g, 2.0, 60);
            if p2 >= 0.3 && normalized(p2, res2, den2) <= 1.0 && self.solve(frame, d2).3 > self.solve(frame, d).3 {
                (d, p, res, den) = (d2, p2, res2, den2);
            }
        }
        let visible = p >= 0.05;
        if visible {
            self.prev = self.last;
            self.last = Some(d);
        } else {
            // Hold where it was last seen; the velocity no longer applies.
            d = last;
            self.prev = None;
        }
        let s = Sample {
            t,
            offset: [(d[0] * 100.0).round() / 100.0, (d[1] * 100.0).round() / 100.0],
            presence: if visible { p } else { 0.0 },
            residual: if visible {
                normalized(p, res, den).min(10.0)
            } else {
                0.0
            },
        };
        self.samples.push(s);
        s
    }
}
