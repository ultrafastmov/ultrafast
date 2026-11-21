//! Layer decomposition: a finished video split into an editable stack, for
//! motion graphics over a background that holds still within a shot.
//!
//! Per shot, the background plate is the temporal median of its frames.
//! Everything that differs from the plate is foreground; the union of the
//! foreground over the shot, joined across small gaps, splits it into
//! elements (a word, a bar, a chart line). Each element gets a small palette
//! of flat colors, and every frame is matted against the known plate: each
//! pixel is explained as one palette color at some coverage over the plate.
//! Each color's coverage is traced at its half level into vector outlines,
//! and the outlines become one `path` per element and color that morphs
//! through its shapes, keyed only on the frames where the shape changes,
//! with its opacity keyed when it fades. The document is the plate image
//! with those paths over it: every element moves, recolors and reshapes as
//! an ordinary node.

use crate::metrics::Frame;
use crate::{Reference, compare, probe};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::Path;
use ultrafast_gpu::Device;
use ultrafast_graph::Document;

#[derive(Debug, Clone)]
pub struct LayerOptions {
    /// Difference from the plate (0..255, any channel) that counts as
    /// foreground.
    pub threshold: f32,
    /// Foreground closer than this (px) belongs to the same element.
    pub join: usize,
    /// Elements smaller than this many pixels are noise.
    pub min_area: usize,
    /// Colors per element at most.
    pub max_colors: usize,
    /// Frames sampled for the plate.
    pub plate_samples: usize,
    /// Recognize type and make it text nodes (macOS).
    pub text: bool,
}

impl Default for LayerOptions {
    fn default() -> Self {
        Self {
            threshold: 14.0,
            join: 6,
            min_area: 30,
            max_colors: 6,
            plate_samples: 31,
            text: true,
        }
    }
}

/// The background of `frames`: the temporal median, except in 32 px
/// blocks where the median is much busier (edge energy) than the quietest
/// frame there: something covers that block most of the time, and the
/// median of its few quietest frames shows what is behind it.
fn quiet_plate(frames: &[Frame], keep: usize) -> Frame {
    let (w, h) = (frames[0].width as usize, frames[0].height as usize);
    let mut plate = median(frames);
    let luma = |f: &Frame, i: usize| {
        0.2126 * f.rgba[i * 4] as f32 + 0.7152 * f.rgba[i * 4 + 1] as f32 + 0.0722 * f.rgba[i * 4 + 2] as f32
    };
    let energy = |f: &Frame, bx: usize, by: usize, x1: usize, y1: usize| -> f32 {
        let mut e = 0.0;
        for y in by..y1 {
            for x in bx..x1 {
                let i = y * w + x;
                if x + 1 < x1 {
                    e += (luma(f, i) - luma(f, i + 1)).abs();
                }
                if y + 1 < y1 {
                    e += (luma(f, i) - luma(f, i + w)).abs();
                }
            }
        }
        e
    };
    const B: usize = 32;
    let mut v = Vec::with_capacity(keep);
    for by in (0..h).step_by(B) {
        for bx in (0..w).step_by(B) {
            let (x1, y1) = ((bx + B).min(w), (by + B).min(h));
            let mut es: Vec<(f32, usize)> = frames
                .iter()
                .enumerate()
                .map(|(k, f)| (energy(f, bx, by, x1, y1), k))
                .collect();
            es.sort_by(|a, b| a.0.total_cmp(&b.0));
            let floor = es[0].0;
            let here = energy(&plate, bx, by, x1, y1);
            let area = ((x1 - bx) * (y1 - by)) as f32;
            if here <= floor * 2.0 + area * 1.5 {
                continue;
            }
            let chosen: Vec<usize> = es
                .iter()
                .take(keep)
                .filter(|(e, _)| *e <= floor * 1.5 + area)
                .map(|(_, k)| *k)
                .collect();
            for y in by..y1 {
                for x in bx..x1 {
                    let i = y * w + x;
                    for c in 0..3 {
                        v.clear();
                        v.extend(chosen.iter().map(|&k| frames[k].rgba[i * 4 + c]));
                        v.sort_unstable();
                        plate.rgba[i * 4 + c] = v[v.len() / 2];
                    }
                }
            }
        }
    }
    // Blocks whose color stands out from the blocks around them (a flat
    // fill that covered the place most of the time) take the frames where
    // they match their surroundings.
    let (gw, gh) = (w.div_ceil(B), h.div_ceil(B));
    let mean = |f: &Frame, gx: usize, gy: usize| -> [f32; 3] {
        let (bx, by) = (gx * B, gy * B);
        let (x1, y1) = ((bx + B).min(w), (by + B).min(h));
        let mut m = [0f32; 3];
        for y in by..y1 {
            for x in bx..x1 {
                for c in 0..3 {
                    m[c] += f.rgba[(y * w + x) * 4 + c] as f32;
                }
            }
        }
        let n = ((x1 - bx) * (y1 - by)) as f32;
        m.map(|v| v / n)
    };
    for _ in 0..2 {
        let means: Vec<[f32; 3]> = (0..gw * gh).map(|g| mean(&plate, g % gw, g / gw)).collect();
        let mut fixes = Vec::new();
        for gy in 0..gh {
            for gx in 0..gw {
                let mut around: [Vec<f32>; 3] = Default::default();
                for ny in gy.saturating_sub(3)..(gy + 4).min(gh) {
                    for nx in gx.saturating_sub(3)..(gx + 4).min(gw) {
                        if (nx, ny) != (gx, gy) {
                            for c in 0..3 {
                                around[c].push(means[ny * gw + nx][c]);
                            }
                        }
                    }
                }
                let consensus: [f32; 3] = std::array::from_fn(|c| {
                    around[c].sort_by(f32::total_cmp);
                    around[c][around[c].len() / 2]
                });
                let off = |m: [f32; 3]| (0..3).map(|c| (m[c] - consensus[c]).abs()).fold(0f32, f32::max);
                if off(means[gy * gw + gx]) <= 10.0 {
                    continue;
                }
                let mut fit: Vec<(f32, usize)> = frames
                    .iter()
                    .enumerate()
                    .map(|(k, f)| (off(mean(f, gx, gy)), k))
                    .collect();
                fit.sort_by(|a, b| a.0.total_cmp(&b.0));
                let chosen: Vec<usize> = fit
                    .iter()
                    .take(keep)
                    .filter(|(o, _)| *o <= fit[0].0 + 4.0)
                    .map(|(_, k)| *k)
                    .collect();
                if fit[0].0 < off(means[gy * gw + gx]) {
                    fixes.push((gx, gy, chosen));
                }
            }
        }
        if fixes.is_empty() {
            break;
        }
        for (gx, gy, chosen) in fixes {
            let (bx, by) = (gx * B, gy * B);
            for y in by..(by + B).min(h) {
                for x in bx..(bx + B).min(w) {
                    let i = y * w + x;
                    for c in 0..3 {
                        v.clear();
                        v.extend(chosen.iter().map(|&k| frames[k].rgba[i * 4 + c]));
                        v.sort_unstable();
                        plate.rgba[i * 4 + c] = v[v.len() / 2];
                    }
                }
            }
        }
    }
    plate
}

fn median(frames: &[Frame]) -> Frame {
    let (w, h) = (frames[0].width, frames[0].height);
    let n = frames.len();
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    let mut v = vec![0u8; n];
    for i in 0..(w * h) as usize {
        for c in 0..3 {
            for (k, f) in frames.iter().enumerate() {
                v[k] = f.rgba[i * 4 + c];
            }
            v.sort_unstable();
            rgba[i * 4 + c] = v[n / 2];
        }
    }
    Frame {
        width: w,
        height: h,
        rgba,
    }
}

/// Per pixel difference from the plate, averaged over 3x3: grain and
/// compression noise average out, edges of real content do not.
fn diff_map(f: &Frame, p: &Frame) -> Vec<f32> {
    let (w, h) = (f.width as usize, f.height as usize);
    // Per pixel, then a 3x3 box: rows in parallel bands.
    let mut raw = vec![0f32; w * h];
    let bands = std::thread::available_parallelism().map_or(8, |n| n.get()).max(1);
    let rows = h.div_ceil(bands);
    std::thread::scope(|sc| {
        for (b, chunk) in raw.chunks_mut(rows * w).enumerate() {
            sc.spawn(move || {
                for (k, v) in chunk.iter_mut().enumerate() {
                    let i = b * rows * w + k;
                    let (a, q) = (&f.rgba[i * 4..i * 4 + 3], &p.rgba[i * 4..i * 4 + 3]);
                    *v = (a[0].abs_diff(q[0])).max(a[1].abs_diff(q[1])).max(a[2].abs_diff(q[2])) as f32;
                }
            });
        }
    });
    let mut tmp = vec![0f32; w * h];
    std::thread::scope(|sc| {
        let raw = &raw;
        for (b, chunk) in tmp.chunks_mut(rows * w).enumerate() {
            sc.spawn(move || {
                for (r, row) in chunk.chunks_mut(w).enumerate() {
                    let src = &raw[(b * rows + r) * w..(b * rows + r + 1) * w];
                    for x in 0..w {
                        let (a, z) = (x.saturating_sub(1), (x + 1).min(w - 1));
                        row[x] = src[a..=z].iter().sum::<f32>() / (z - a + 1) as f32;
                    }
                }
            });
        }
    });
    let mut out = vec![0f32; w * h];
    std::thread::scope(|sc| {
        let tmp = &tmp;
        for (b, chunk) in out.chunks_mut(rows * w).enumerate() {
            sc.spawn(move || {
                for (r, row) in chunk.chunks_mut(w).enumerate() {
                    let y = b * rows + r;
                    let (a, z) = (y.saturating_sub(1), (y + 1).min(h - 1));
                    let n = (z - a + 1) as f32;
                    for x in 0..w {
                        let mut s = 0.0;
                        for yy in a..=z {
                            s += tmp[yy * w + x];
                        }
                        row[x] = s / n;
                    }
                }
            });
        }
    });
    out
}

fn diff(f: &Frame, p: &Frame, i: usize) -> f32 {
    (0..3)
        .map(|c| (f.rgba[i * 4 + c] as f32 - p.rgba[i * 4 + c] as f32).abs())
        .fold(0.0, f32::max)
}

/// Connected components (8-neighbour) of `mask`: a label per pixel (0 =
/// none) and each label's bounds [x0, y0, x1, y1] (inclusive) and size.
fn components(mask: &[bool], w: usize, h: usize) -> (Vec<u32>, Vec<([usize; 4], usize)>) {
    let mut label = vec![0u32; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask[start] || label[start] != 0 {
            continue;
        }
        let id = out.len() as u32 + 1;
        let mut b = [start % w, start / w, start % w, start / w];
        let mut n = 0;
        label[start] = id;
        stack.push(start);
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = (i % w, i / w);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && label[j] == 0 {
                        label[j] = id;
                        stack.push(j);
                    }
                }
            }
        }
        out.push((b, n));
    }
    (label, out)
}

/// `mask` grown by `r` px (square), separably.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    dilate_xy(mask, w, h, r, r)
}

/// `mask` grown by `rx` px across and `ry` px up and down.
fn dilate_xy(mask: &[bool], w: usize, h: usize, rx: usize, ry: usize) -> Vec<bool> {
    let r = rx;
    // Rows: distance to the nearest set pixel either way.
    let mut tmp = vec![false; w * h];
    for y in 0..h {
        let (src, dst) = (&mask[y * w..(y + 1) * w], &mut tmp[y * w..(y + 1) * w]);
        let mut d = usize::MAX / 2;
        for x in 0..w {
            d = if src[x] { 0 } else { d + 1 };
            dst[x] = d <= r;
        }
        d = usize::MAX / 2;
        for x in (0..w).rev() {
            d = if src[x] { 0 } else { d + 1 };
            dst[x] |= d <= r;
        }
    }
    // Columns, a row at a time (each column's distance carried along).
    let mut out = vec![false; w * h];
    let mut d = vec![usize::MAX / 2; w];
    for y in 0..h {
        for x in 0..w {
            d[x] = if tmp[y * w + x] { 0 } else { d[x] + 1 };
            out[y * w + x] = d[x] <= ry;
        }
    }
    d.fill(usize::MAX / 2);
    for y in (0..h).rev() {
        for x in 0..w {
            d[x] = if tmp[y * w + x] { 0 } else { d[x] + 1 };
            out[y * w + x] |= d[x] <= ry;
        }
    }
    out
}

/// A few flat colors explaining `samples` (sRGB 0..255), by k-means with
/// the smallest k that fits them within `tol` on average.
fn palette(samples: &[[f32; 3]], max_k: usize, tol: f32) -> Vec<[f32; 3]> {
    if samples.is_empty() {
        return Vec::new();
    }
    let dist = |a: &[f32; 3], b: &[f32; 3]| (0..3).map(|c| (a[c] - b[c]).powi(2)).sum::<f32>();
    let mut best: Vec<[f32; 3]> = Vec::new();
    for k in 1..=max_k.max(1) {
        // Farthest-point seeding, then Lloyd iterations.
        let mut centers = vec![samples[0]];
        while centers.len() < k {
            let far = samples
                .iter()
                .max_by(|a, b| {
                    let da = centers.iter().map(|c| dist(a, c)).fold(f32::MAX, f32::min);
                    let db = centers.iter().map(|c| dist(b, c)).fold(f32::MAX, f32::min);
                    da.total_cmp(&db)
                })
                .unwrap();
            centers.push(*far);
        }
        for _ in 0..12 {
            let mut sum = vec![[0f32; 4]; k];
            for s in samples {
                let j = (0..k)
                    .min_by(|&a, &b| dist(s, &centers[a]).total_cmp(&dist(s, &centers[b])))
                    .unwrap();
                for c in 0..3 {
                    sum[j][c] += s[c];
                }
                sum[j][3] += 1.0;
            }
            for j in 0..k {
                if sum[j][3] > 0.0 {
                    centers[j] = [sum[j][0] / sum[j][3], sum[j][1] / sum[j][3], sum[j][2] / sum[j][3]];
                }
            }
        }
        // Enough colors once almost every sample is near one of them (a
        // color covering a small share still counts).
        let off = samples
            .iter()
            .filter(|s| centers.iter().map(|c| dist(s, c)).fold(f32::MAX, f32::min).sqrt() > tol * 2.0)
            .count();
        best = centers;
        if (off as f32) < samples.len() as f32 * 0.01 {
            break;
        }
    }
    best
}

/// The colors of element `id` (palette `pal` over `ground`) drawn over its
/// color `j`: the background's own color goes first, then largest (by
/// coverage over the shot) behind.
fn in_front(
    weight: &HashMap<(usize, usize), f64>,
    id: usize,
    j: usize,
    pal: &[Fill],
    ground: Option<&[f32; 3]>,
) -> Vec<usize> {
    let is_ground = |k: usize| {
        !pal[k].ramp()
            && ground.is_some_and(|g| (0..3).map(|c| (g[c] - pal[k].c[c]).powi(2)).sum::<f32>() < 14.0 * 14.0)
    };
    let key = |k: usize| (!is_ground(k), -weight.get(&(id, k)).copied().unwrap_or(0.0));
    (0..pal.len()).filter(|&k| k != j && key(k) > key(j)).collect()
}

/// Each color's coverage carried under the colors drawn over it (`front`
/// of a color: those on top of it) wherever it surrounds them, so a card
/// or a highlight behind type has no type-shaped holes (and no halos when
/// the type moves or is edited). A pixel is under-filled when, along its
/// row or its column, the color is reached on both sides before the plate.
fn fill_under(alphas: &mut [Vec<f32>], bw: usize, bh: usize, front: impl Fn(usize) -> Vec<usize>) {
    const REACH: usize = 48;
    for j in 0..alphas.len() {
        let over = front(j);
        if over.is_empty() {
            continue;
        }
        let cover: Vec<f32> = (0..bw * bh)
            .map(|i| over.iter().map(|&k| alphas[k][i]).fold(0.0f32, f32::max))
            .collect();
        let a = &alphas[j];
        // Reached along a direction before open background.
        let reach = |x: usize, y: usize, dx: i64, dy: i64| -> bool {
            let (mut cx, mut cy) = (x as i64, y as i64);
            for _ in 0..REACH {
                cx += dx;
                cy += dy;
                if cx < 0 || cy < 0 || cx >= bw as i64 || cy >= bh as i64 {
                    return false;
                }
                let i = cy as usize * bw + cx as usize;
                if a[i] >= 0.5 {
                    return true;
                }
                if cover[i] <= 0.02 {
                    return false;
                }
            }
            false
        };
        let mut fill = Vec::new();
        for y in 0..bh {
            for x in 0..bw {
                let i = y * bw + x;
                if cover[i] > 0.02
                    && a[i] < 1.0
                    && ((reach(x, y, -1, 0) && reach(x, y, 1, 0)) || (reach(x, y, 0, -1) && reach(x, y, 0, 1)))
                {
                    fill.push(i);
                }
            }
        }
        for i in fill {
            alphas[j][i] = 1.0;
        }
    }
}

/// An element painted from pixels instead of colors: an image of it at
/// its fullest frame (moved by its dominant color's motion), or, when its
/// content keeps changing, a window onto the source video.
struct Raster {
    sprite: Option<String>,
    /// Its box at that frame.
    at: [usize; 4],
    /// The color whose place in the stack it takes.
    dominant: usize,
    /// The image's offset per frame, where it is that image moved.
    places: Vec<Option<[f64; 2]>>,
}

/// Where image `img` (taken with its box at `at`) sits in frame `fr`, near
/// where the element's box `bb` says: its offset from `at`, when the image
/// moved there matches the frame closely.
fn place_sprite(img: &Frame, at: [usize; 4], bb: [usize; 4], fr: &Frame, w: usize, h: usize) -> Option<[f64; 2]> {
    let (iw, ih) = (img.width as usize, img.height as usize);
    // The box's free edges say where it went (an edge at the frame's
    // border may be cut).
    let gx = if bb[0] == 0 && at[0] != 0 {
        bb[2] as f64 - at[2] as f64
    } else {
        bb[0] as f64 - at[0] as f64
    };
    let gy = if bb[1] == 0 && at[1] != 0 {
        bb[3] as f64 - at[3] as f64
    } else {
        bb[1] as f64 - at[1] as f64
    };
    let step = ((iw * ih) as f64 / 40_000.0).sqrt().ceil().max(1.0) as usize;
    // The frame sampled between pixels (bilinear).
    let sample = |x: f64, y: f64, c: usize| -> Option<f64> {
        if x < 0.0 || y < 0.0 || x > (w - 1) as f64 || y > (h - 1) as f64 {
            return None;
        }
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let p = |x: usize, y: usize| fr.rgba[(y * w + x) * 4 + c] as f64;
        Some((p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx) * (1.0 - fy) + (p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx) * fy)
    };
    let err = |dx: f64, dy: f64| -> Option<f64> {
        let (mut e, mut n) = (0f64, 0usize);
        for y in (0..ih).step_by(step) {
            for x in (0..iw).step_by(step) {
                let o = (y * iw + x) * 4;
                if img.rgba[o + 3] < 250 {
                    continue;
                }
                let (fx, fy) = (at[0] as f64 + x as f64 + dx, at[1] as f64 + y as f64 + dy);
                let Some(v) = (0..3).map(|c| sample(fx, fy, c)).collect::<Option<Vec<f64>>>() else {
                    continue;
                };
                for c in 0..3 {
                    e += (img.rgba[o + c] as f64 - v[c]).powi(2);
                }
                n += 1;
            }
        }
        (n >= 50).then(|| (e / (3 * n) as f64).sqrt())
    };
    let mut best = (f64::MAX, 0f64, 0f64);
    for dy in -3..=3 {
        for dx in -3..=3 {
            let (x, y) = (gx.round() + dx as f64, gy.round() + dy as f64);
            if let Some(e) = err(x, y)
                && e < best.0
            {
                best = (e, x, y);
            }
        }
    }
    // Between pixels: halves, then quarters.
    for d in [0.5, 0.25] {
        let (bx, by) = (best.1, best.2);
        for (sx, sy) in [
            (-d, 0.0),
            (d, 0.0),
            (0.0, -d),
            (0.0, d),
            (-d, -d),
            (d, d),
            (-d, d),
            (d, -d),
        ] {
            if let Some(e) = err(bx + sx, by + sy)
                && e < best.0
            {
                best = (e, bx + sx, by + sy);
            }
        }
    }
    if std::env::var_os("ULTRAFAST_DEBUG_RASTER").is_some() {
        eprintln!(
            "sprite at {at:?} box {bb:?} guess ({gx},{gy}): best {:.1} at ({}, {})",
            best.0, best.1, best.2
        );
    }
    (best.0 < 12.0).then_some([best.1, best.2])
}

/// One element color: flat, or a linear ramp `c + v * t` with `t` the
/// distance along `n` from `o`, over `t0..t1`; in sRGB values (0..255),
/// or in linear light (scaled to 0..255) when `linear`.
#[derive(Clone, Copy, Debug)]
struct Fill {
    c: [f32; 3],
    v: [f32; 3],
    n: [f32; 2],
    o: [f32; 2],
    t: [f32; 2],
    linear: bool,
}

fn to_lin(v: f32) -> f32 {
    ultrafast_render::ops::srgb_to_linear((v / 255.0) as f64) as f32 * 255.0
}

fn to_srgb(v: f32) -> f32 {
    let x = (v / 255.0).clamp(0.0, 1.0);
    let s = if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    s * 255.0
}

impl Fill {
    fn flat(c: [f32; 3]) -> Fill {
        Fill {
            c,
            v: [0.0; 3],
            n: [1.0, 0.0],
            o: [0.0; 2],
            t: [0.0; 2],
            linear: false,
        }
    }

    fn ramp(&self) -> bool {
        self.v.iter().any(|v| v.abs() > 1e-6)
    }

    /// The color (sRGB values) at distance `t` along the ramp.
    fn along(&self, t: f32) -> [f32; 3] {
        let t = t.clamp(self.t[0], self.t[1]);
        std::array::from_fn(|k| {
            let v = self.c[k] + self.v[k] * t;
            if self.linear { to_srgb(v) } else { v }
        })
    }

    /// The color (sRGB values) at frame point (x, y).
    fn at(&self, x: f32, y: f32) -> [f32; 3] {
        if !self.ramp() {
            return self.c;
        }
        self.along((x - self.o[0]) * self.n[0] + (y - self.o[1]) * self.n[1])
    }

    /// Its mean color (sRGB values).
    fn mean(&self) -> [f32; 3] {
        if self.linear { self.c.map(to_srgb) } else { self.c }
    }
}

/// Flat colors `pal` of samples `sampled` (color and 6 px cell) where some
/// neighboring ones are one smooth ramp across space (a gradient the
/// palette cut into bands): those become linear fills.
fn ramps(pal: Vec<[f32; 3]>, sampled: &[([f32; 3], (u16, u16))]) -> (Vec<[f32; 3]>, Vec<Fill>) {
    if pal.len() < 2 || sampled.len() < 60 {
        return (pal, Vec::new());
    }
    let d2 = |a: &[f32; 3], b: &[f32; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>();
    let nearest = |c: &[f32; 3]| {
        (0..pal.len())
            .min_by(|&a, &b| d2(c, &pal[a]).total_cmp(&d2(c, &pal[b])))
            .unwrap()
    };
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); pal.len()];
    let mut cells: Vec<std::collections::HashSet<(u16, u16)>> = vec![Default::default(); pal.len()];
    for (i, (c, cell)) in sampled.iter().enumerate() {
        let j = nearest(c);
        members[j].push(i);
        cells[j].insert(*cell);
    }
    // A ramp over samples: direction from the color's spatial gradient,
    // then each channel along it; its error (rms) and fill.
    let fit_in = |idx: &[usize], linear: bool| -> Option<(f32, Fill)> {
        if idx.len() < 30 {
            return None;
        }
        let col = |i: usize, k: usize| -> f64 {
            let v = sampled[i].0[k];
            (if linear { to_lin(v) } else { v }) as f64
        };
        let n = idx.len() as f64;
        let pos = |i: usize| {
            let (cx, cy) = sampled[i].1;
            (cx as f64 * 6.0 + 3.0, cy as f64 * 6.0 + 3.0)
        };
        let (mut mx, mut my, mut mc) = (0.0, 0.0, [0.0f64; 3]);
        for &i in idx {
            let (x, y) = pos(i);
            mx += x / n;
            my += y / n;
            for k in 0..3 {
                mc[k] += col(i, k) / n;
            }
        }
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        let mut sxc = [0.0f64; 3];
        let mut syc = [0.0f64; 3];
        for &i in idx {
            let (x, y) = pos(i);
            let (dx, dy) = (x - mx, y - my);
            sxx += dx * dx;
            sxy += dx * dy;
            syy += dy * dy;
            for k in 0..3 {
                let dc = col(i, k) - mc[k];
                sxc[k] += dx * dc;
                syc[k] += dy * dc;
            }
        }
        let det = sxx * syy - sxy * sxy;
        if det.abs() < 1e-6 {
            return None;
        }
        // Per channel gradient (gx, gy); the ramp's direction is where they
        // agree most (the dominant eigenvector of G^T G).
        let g: Vec<(f64, f64)> = (0..3)
            .map(|k| ((syy * sxc[k] - sxy * syc[k]) / det, (sxx * syc[k] - sxy * sxc[k]) / det))
            .collect();
        let (a, b, d) = (
            g.iter().map(|v| v.0 * v.0).sum::<f64>(),
            g.iter().map(|v| v.0 * v.1).sum::<f64>(),
            g.iter().map(|v| v.1 * v.1).sum::<f64>(),
        );
        let ang = 0.5 * (2.0 * b).atan2(a - d);
        let (nx, ny) = (ang.cos(), ang.sin());
        let ts: Vec<f64> = idx
            .iter()
            .map(|&i| {
                let (x, y) = pos(i);
                (x - mx) * nx + (y - my) * ny
            })
            .collect();
        let tt: f64 = ts.iter().map(|t| t * t).sum();
        if tt < 1e-6 {
            return None;
        }
        let v: [f64; 3] =
            std::array::from_fn(|k| idx.iter().zip(&ts).map(|(&i, t)| (col(i, k) - mc[k]) * t).sum::<f64>() / tt);
        // Error in sRGB values, whatever the space.
        let mut err = 0.0;
        for (&i, t) in idx.iter().zip(&ts) {
            for k in 0..3 {
                let p = (mc[k] + v[k] * t) as f32;
                let p = if linear { to_srgb(p) } else { p };
                err += (sampled[i].0[k] - p).powi(2) as f64;
            }
        }
        let rms = (err / n / 3.0).sqrt() as f32;
        let (t0, t1) = ts.iter().fold((f64::MAX, f64::MIN), |a, t| (a.0.min(*t), a.1.max(*t)));
        Some((
            rms,
            Fill {
                c: mc.map(|v| v as f32),
                v: v.map(|v| v as f32),
                n: [nx as f32, ny as f32],
                o: [mx as f32, my as f32],
                t: [t0 as f32, t1 as f32],
                linear,
            },
        ))
    };
    // Gradients interpolate in sRGB (CSS, Figma) or in linear light.
    let fit = |idx: &[usize]| -> Option<(f32, Fill)> {
        match (fit_in(idx, false), fit_in(idx, true)) {
            (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
            (a, b) => a.or(b),
        }
    };
    // Neighbors in space and not far apart in color, merged while one ramp
    // fits them all closely.
    let touching = |a: usize, b: usize| {
        cells[a]
            .iter()
            .filter(|(x, y)| {
                (-1i32..=1).any(|dy| {
                    (-1i32..=1)
                        .any(|dx| cells[b].contains(&((*x as i32 + dx).max(0) as u16, (*y as i32 + dy).max(0) as u16)))
                })
            })
            .count()
            >= 3
    };
    let mut group: Vec<usize> = (0..pal.len()).collect();
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for a in 0..pal.len() {
        for b in a + 1..pal.len() {
            let d = d2(&pal[a], &pal[b]).sqrt();
            if d < 140.0 && touching(a, b) {
                pairs.push((d, a, b));
            }
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
    for (_, a, b) in pairs {
        let (ga, gb) = (group[a], group[b]);
        if ga == gb {
            continue;
        }
        let idx: Vec<usize> = (0..pal.len())
            .filter(|&j| group[j] == ga || group[j] == gb)
            .flat_map(|j| members[j].iter().copied())
            .collect();
        if std::env::var_os("ULTRAFAST_DEBUG_RAMPS").is_some() {
            eprintln!(
                "ramp? {} + {}: {:?}",
                hex(pal[a]),
                hex(pal[b]),
                fit(&idx).map(|(r, f)| (r, f.linear, hex(f.along(f.t[0])), hex(f.along(f.t[1]))))
            );
        }
        if let Some((rms, f)) = fit(&idx)
            && rms < 5.0
            && d2(&f.along(f.t[0]), &f.along(f.t[1])).sqrt() >= 16.0
        {
            for g in group.iter_mut() {
                if *g == gb {
                    *g = ga;
                }
            }
        }
    }
    let mut flats = Vec::new();
    let mut fills = Vec::new();
    let mut done = std::collections::HashSet::new();
    for j in 0..pal.len() {
        let g = group[j];
        if !done.insert(g) {
            continue;
        }
        let parts: Vec<usize> = (0..pal.len()).filter(|&k| group[k] == g).collect();
        // A gradient is cut into three bands or more, end to end well apart;
        // two neighboring colors are two colors.
        let idx: Vec<usize> = parts.iter().flat_map(|&k| members[k].iter().copied()).collect();
        match fit(&idx).filter(|(_, f)| parts.len() >= 3 && d2(&f.along(f.t[0]), &f.along(f.t[1])).sqrt() >= 40.0) {
            Some((_, f)) => fills.push(f),
            None => flats.extend(parts.iter().map(|&k| pal[k])),
        }
    }
    (flats, fills)
}

/// Flat colors `pal` of samples `sampled` (color and cell) without those
/// that are a stronger one of them partly over `bg` where that one is also
/// seen (a fade: the same place at a lower opacity), and with colors for
/// the samples the rest left unexplained (the palette's slots freed).
fn drop_fades(pal: Vec<[f32; 3]>, sampled: &[([f32; 3], (u16, u16))], bg: [f32; 3], max_k: usize) -> Vec<[f32; 3]> {
    let d2 = |a: &[f32; 3], b: &[f32; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>();
    let nearest = |c: &[f32; 3]| {
        (0..pal.len())
            .min_by(|&a, &b| d2(c, &pal[a]).total_cmp(&d2(c, &pal[b])))
            .unwrap()
    };
    let mut cells: Vec<std::collections::HashSet<(u16, u16)>> = vec![Default::default(); pal.len()];
    for (c, cell) in sampled {
        cells[nearest(c)].insert(*cell);
    }
    let mut order: Vec<usize> = (0..pal.len()).collect();
    order.sort_by(|&a, &b| d2(&pal[b], &bg).total_cmp(&d2(&pal[a], &bg)));
    // Flat colors are clean: a blend is close to the line from the color
    // to the background in every channel.
    let blend_of = |q: &[f32; 3], p: &[f32; 3], tol: f32| {
        let d: [f32; 3] = std::array::from_fn(|k| p[k] - bg[k]);
        let dd = d2(p, &bg);
        if dd < 400.0 {
            return false;
        }
        let a = (0..3).map(|k| (q[k] - bg[k]) * d[k]).sum::<f32>() / dd;
        let r: [f32; 3] = std::array::from_fn(|k| q[k] - bg[k] - a * d[k]);
        (0.03..=0.97).contains(&a) && r.iter().map(|v| v * v).sum::<f32>() < tol * tol
    };
    // Where a color is seen, give or take a few cells (a fade that moves
    // as it comes in).
    let near: Vec<std::collections::HashSet<(u16, u16)>> = cells
        .iter()
        .map(|cs| {
            let mut out = std::collections::HashSet::new();
            for &(x, y) in cs {
                for dy in -4i32..=4 {
                    for dx in -4i32..=4 {
                        out.insert(((x as i32 + dx).max(0) as u16, (y as i32 + dy).max(0) as u16));
                    }
                }
            }
            out
        })
        .collect();
    let mut keep: Vec<usize> = Vec::new();
    for q in order {
        let fade = keep.iter().any(|&p| {
            blend_of(&pal[q], &pal[p], 12.0)
                && cells[q].iter().filter(|c| near[p].contains(c)).count() * 2 >= cells[q].len()
        });
        if !fade {
            keep.push(q);
        }
    }
    let mut out: Vec<[f32; 3]> = keep.iter().map(|&i| pal[i]).collect();
    if out.len() < pal.len() {
        // Samples no color explains, nor any as a fade (a color the full
        // palette had no room for): colors of their own.
        let left: Vec<[f32; 3]> = sampled
            .iter()
            .map(|x| x.0)
            .filter(|c| out.iter().all(|p| d2(c, p).sqrt() > 20.0 && !blend_of(c, p, 20.0)))
            .collect();
        if left.len() * 100 >= sampled.len() && left.len() >= 30 {
            out.extend(palette(&left, max_k.saturating_sub(out.len()), 10.0));
        }
    }
    out
}

/// Edge colors `edge` without those that are another color (a solid one
/// in `solid`, or a stronger edge color) partly over `bg`: thin strokes and
/// edges never reach their full color.
fn drop_blends(solid: &[[f32; 3]], mut edge: Vec<[f32; 3]>, bg: [f32; 3]) -> Vec<[f32; 3]> {
    let d2 = |a: &[f32; 3], b: &[f32; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>();
    edge.sort_by(|a, b| d2(b, &bg).total_cmp(&d2(a, &bg)));
    let mut keep: Vec<[f32; 3]> = solid.to_vec();
    for q in edge {
        let blend = keep.iter().any(|p| {
            let d: [f32; 3] = std::array::from_fn(|k| p[k] - bg[k]);
            let dd = d2(p, &bg);
            if dd < 400.0 {
                return false;
            }
            let a = (0..3).map(|k| (q[k] - bg[k]) * d[k]).sum::<f32>() / dd;
            let r: [f32; 3] = std::array::from_fn(|k| q[k] - bg[k] - a * d[k]);
            let luma = 0.2126 * r[0] + 0.7152 * r[1] + 0.0722 * r[2];
            (0.05..=0.97).contains(&a) && luma.abs() < 16.0 && r.iter().map(|v| v * v).sum::<f32>() < 40.0 * 40.0
        });
        if !blend {
            keep.push(q);
        }
    }
    keep.split_off(solid.len())
}

/// Closed outlines where `a` (w x h, row-major) crosses `level`, by
/// marching squares on pixel centers, offset by (ox, oy).
fn outlines(a: &[f32], w: usize, h: usize, level: f32, ox: f64, oy: f64) -> Vec<Vec<(f64, f64)>> {
    // Pad with zeros so every outline closes.
    let (pw, ph) = (w + 2, h + 2);
    let v = |x: usize, y: usize| -> f32 {
        if x == 0 || y == 0 || x > w || y > h {
            0.0
        } else {
            a[(y - 1) * w + (x - 1)]
        }
    };
    // Edge ids: horizontal edge (x,y)-(x+1,y) and vertical (x,y)-(x,y+1).
    let hid = |x: usize, y: usize| (y * pw + x) * 2;
    let vid = |x: usize, y: usize| (y * pw + x) * 2 + 1;
    let point = |e: usize| -> (f64, f64) {
        let cell = e / 2;
        let (x, y) = (cell % pw, cell / pw);
        let (v0, v1, x1, y1) = if e.is_multiple_of(2) {
            (v(x, y), v(x + 1, y), x + 1, y)
        } else {
            (v(x, y), v(x, y + 1), x, y + 1)
        };
        let t = ((level - v0) / (v1 - v0)).clamp(0.0, 1.0) as f64;
        // Pixel centers: padded index 1 is pixel 0 at x = 0.5.
        let px = x as f64 + (x1 as f64 - x as f64) * t - 0.5;
        let py = y as f64 + (y1 as f64 - y as f64) * t - 0.5;
        (ox + px, oy + py)
    };
    let mut next: HashMap<usize, usize> = HashMap::new();
    for y in 0..ph - 1 {
        for x in 0..pw - 1 {
            let (a0, a1, a2, a3) = (v(x, y), v(x + 1, y), v(x + 1, y + 1), v(x, y + 1));
            let case =
                (a0 > level) as u8 | ((a1 > level) as u8) << 1 | ((a2 > level) as u8) << 2 | ((a3 > level) as u8) << 3;
            let (top, right, bottom, left) = (hid(x, y), vid(x + 1, y), hid(x, y + 1), vid(x, y));
            // Segments oriented with the inside on the right.
            let segs: &[(usize, usize)] = match case {
                0 | 15 => &[],
                1 => &[(left, top)],
                2 => &[(top, right)],
                3 => &[(left, right)],
                4 => &[(right, bottom)],
                5 => {
                    if (a0 + a1 + a2 + a3) / 4.0 > level {
                        &[(left, bottom), (right, top)]
                    } else {
                        &[(left, top), (right, bottom)]
                    }
                }
                6 => &[(top, bottom)],
                7 => &[(left, bottom)],
                8 => &[(bottom, left)],
                9 => &[(bottom, top)],
                10 => {
                    if (a0 + a1 + a2 + a3) / 4.0 > level {
                        &[(top, left), (bottom, right)]
                    } else {
                        &[(top, right), (bottom, left)]
                    }
                }
                11 => &[(bottom, right)],
                12 => &[(right, left)],
                13 => &[(right, top)],
                14 => &[(top, left)],
                _ => &[],
            };
            for &(s, e) in segs {
                next.insert(s, e);
            }
        }
    }
    let mut loops = Vec::new();
    while let Some(&start) = next.keys().next() {
        let mut pts = Vec::new();
        let mut cur = start;
        while let Some(n) = next.remove(&cur) {
            pts.push(point(cur));
            cur = n;
            if cur == start {
                break;
            }
        }
        if pts.len() >= 3 {
            loops.push(pts);
        }
    }
    loops
}

/// Ramer-Douglas-Peucker on a closed loop.
fn simplify(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    fn rdp(p: &[(f64, f64)], eps: f64, out: &mut Vec<(f64, f64)>) {
        let (a, b) = (p[0], p[p.len() - 1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-12);
        let (mut far, mut idx) = (0.0, 0);
        for (i, q) in p.iter().enumerate().take(p.len() - 1).skip(1) {
            let d = ((q.0 - a.0) * dy - (q.1 - a.1) * dx).abs() / len;
            if d > far {
                far = d;
                idx = i;
            }
        }
        if far > eps {
            rdp(&p[..=idx], eps, out);
            out.pop();
            rdp(&p[idx..], eps, out);
        } else {
            out.push(a);
            out.push(b);
        }
    }
    if pts.len() < 4 {
        return pts.to_vec();
    }
    // Split at the point farthest from the first, so both halves are open.
    let far = (1..pts.len())
        .max_by(|&i, &j| {
            let d = |k: usize| (pts[k].0 - pts[0].0).powi(2) + (pts[k].1 - pts[0].1).powi(2);
            d(i).total_cmp(&d(j))
        })
        .unwrap();
    let mut a = Vec::new();
    rdp(&pts[..=far], eps, &mut a);
    let mut rest: Vec<(f64, f64)> = pts[far..].to_vec();
    rest.push(pts[0]);
    let mut b = Vec::new();
    rdp(&rest, eps, &mut b);
    a.pop();
    b.pop();
    a.extend(b);
    a
}

fn area(p: &[(f64, f64)]) -> f64 {
    let mut s = 0.0;
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        s += a.0 * b.1 - b.0 * a.1;
    }
    s.abs() / 2.0
}

/// SVG path data with relative coordinates and short numbers.
fn compact(p: &kurbo::BezPath) -> String {
    use std::fmt::Write;
    let num = |v: f64| {
        let r = (v * 10.0).round() / 10.0;
        let s = format!("{r:.1}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s == "-0" { "0".to_string() } else { s }
    };
    let mut d = String::new();
    let mut cur = kurbo::Point::ZERO;
    let mut start = kurbo::Point::ZERO;
    let pair = |d: &mut String, p: kurbo::Point, from: kurbo::Point| {
        let (x, y) = (num(p.x - from.x), num(p.y - from.y));
        if !d.ends_with(|c: char| c.is_ascii_alphabetic()) && !x.starts_with('-') {
            d.push(' ');
        }
        d.push_str(&x);
        if !y.starts_with('-') {
            d.push(' ');
        }
        d.push_str(&y);
    };
    // Relative coordinates accumulate rounding: positions round to the
    // grid and deltas are taken between rounded points.
    let snap = |p: kurbo::Point| kurbo::Point::new((p.x * 10.0).round() / 10.0, (p.y * 10.0).round() / 10.0);
    for el in p.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                let p = snap(p);
                let _ = write!(d, "M{} {}", num(p.x), num(p.y));
                cur = p;
                start = p;
            }
            kurbo::PathEl::LineTo(p) => {
                let p = snap(p);
                d.push('l');
                pair(&mut d, p, cur);
                cur = p;
            }
            kurbo::PathEl::QuadTo(a, p) => {
                let (a, p) = (snap(a), snap(p));
                d.push('q');
                pair(&mut d, a, cur);
                pair(&mut d, p, cur);
                cur = p;
            }
            kurbo::PathEl::CurveTo(a, b, p) => {
                let (a, b, p) = (snap(a), snap(b), snap(p));
                d.push('c');
                pair(&mut d, a, cur);
                pair(&mut d, b, cur);
                pair(&mut d, p, cur);
                cur = p;
            }
            kurbo::PathEl::ClosePath => {
                d.push('z');
                cur = start;
            }
        }
    }
    d
}

fn hex(c: [f32; 3]) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        c[0].round().clamp(0.0, 255.0) as u8,
        c[1].round().clamp(0.0, 255.0) as u8,
        c[2].round().clamp(0.0, 255.0) as u8
    )
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// One element color's coverage in a frame: its box in the frame and its
/// coverage there, normalized by its opacity.
#[derive(Clone)]
struct Cover {
    b: [usize; 4],
    bw: usize,
    bh: usize,
    a: Vec<f32>,
    opacity: f32,
    mass: f64,
}

impl Cover {
    fn new(a: Vec<f32>, b: [usize; 4]) -> Option<Cover> {
        let (bw, bh) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
        let mut vals: Vec<f32> = a.iter().copied().filter(|v| *v > 0.05).collect();
        if vals.len() < 4 {
            return None;
        }
        // Fading: the shape at half its own peak, at that opacity.
        vals.sort_by(f32::total_cmp);
        let peak = vals[(vals.len() as f32 * 0.98) as usize].max(0.05);
        let opacity = if peak > 0.92 { 1.0 } else { peak };
        let a: Vec<f32> = a.iter().map(|v| (v / opacity).min(1.0)).collect();
        let mass = a.iter().map(|v| *v as f64).sum();
        Some(Cover {
            b,
            bw,
            bh,
            a,
            opacity,
            mass,
        })
    }

    /// Unchanged but for noise: grain and compression move edge coverage
    /// by about a tenth, so only clear changes on more than a few pixels
    /// count.
    fn same(&self, o: &Cover) -> bool {
        if self.b != o.b {
            return false;
        }
        let changed = self.a.iter().zip(&o.a).filter(|(x, y)| (*x - *y).abs() > 0.3).count();
        changed * 400 <= self.a.len().max(1) && changed < 24
    }

    /// Outline at half coverage, simplified within 0.3 px, as compact
    /// path data.
    fn trace(&self) -> Option<String> {
        let loops: Vec<Vec<(f64, f64)>> = outlines(&self.a, self.bw, self.bh, 0.5, self.b[0] as f64, self.b[1] as f64)
            .into_iter()
            .map(|l| simplify(&l, 0.3))
            .filter(|l| l.len() >= 3 && area(l) > 1.0)
            .collect();
        if loops.is_empty() {
            return None;
        }
        let mut path = kurbo::BezPath::new();
        for l in &loops {
            path.move_to(l[0]);
            for p in &l[1..] {
                path.line_to(*p);
            }
            path.close_path();
        }
        Some(compact(&path))
    }

    /// Coverage at frame point (x, y), between pixel centers.
    fn at(&self, x: f64, y: f64) -> f32 {
        let (u, v) = (x - self.b[0] as f64, y - self.b[1] as f64);
        if u < -1.0 || v < -1.0 || u > self.bw as f64 || v > self.bh as f64 {
            return 0.0;
        }
        let (x0, y0) = (u.floor(), v.floor());
        let (fx, fy) = ((u - x0) as f32, (v - y0) as f32);
        let px = |x: i64, y: i64| -> f32 {
            if x < 0 || y < 0 || x >= self.bw as i64 || y >= self.bh as i64 {
                0.0
            } else {
                self.a[y as usize * self.bw + x as usize]
            }
        };
        let (x0, y0) = (x0 as i64, y0 as i64);
        let top = px(x0, y0) * (1.0 - fx) + px(x0 + 1, y0) * fx;
        let bottom = px(x0, y0 + 1) * (1.0 - fx) + px(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// Bounds [x0, y0, x1, y1] (frame px, exclusive end) of clear coverage.
    fn visible(&self) -> Option<[f64; 4]> {
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for y in 0..self.bh {
            for x in 0..self.bw {
                if self.a[y * self.bw + x] > 0.3 {
                    let (fx, fy) = ((self.b[0] + x) as f64, (self.b[1] + y) as f64);
                    b = [b[0].min(fx), b[1].min(fy), b[2].max(fx + 1.0), b[3].max(fy + 1.0)];
                }
            }
        }
        (b[0] <= b[2]).then_some(b)
    }
}

/// `cur` as `refc` moved by (dx, dy), and clipped to a rectangle when part
/// of it is hidden (a mask reveal): the placement, when it fits closely.
fn fit_shift(refc: &Cover, cur: &Cover) -> Option<(f64, f64, Option<[f64; 4]>)> {
    let (rv, cv) = (refc.visible()?, cur.visible()?);
    let step = (((cur.bw * cur.bh) as f64 / 20_000.0).sqrt().ceil() as usize).max(1);
    let inside =
        |clip: &Option<[f64; 4]>, x: f64, y: f64| clip.is_none_or(|c| x >= c[0] && x < c[2] && y >= c[1] && y < c[3]);
    // Mean difference over where either has coverage, and whether the moved
    // reference puts coverage where the frame has none (outside its box).
    let score = |dx: f64, dy: f64, clip: &Option<[f64; 4]>| -> f32 {
        let (mut err, mut n) = (0f32, 0usize);
        for y in (0..cur.bh).step_by(step) {
            for x in (0..cur.bw).step_by(step) {
                let (fx, fy) = ((cur.b[0] + x) as f64, (cur.b[1] + y) as f64);
                let r = if inside(clip, fx + 0.5, fy + 0.5) {
                    refc.at(fx - dx, fy - dy)
                } else {
                    0.0
                };
                let c = cur.a[y * cur.bw + x];
                if r > 0.02 || c > 0.02 {
                    // Grain and compression move coverage a little anyway.
                    err += ((r - c).abs() - 0.25).max(0.0);
                    n += 1;
                }
            }
        }
        let mut stray = 0f32;
        for y in (0..refc.bh).step_by(step) {
            for x in (0..refc.bw).step_by(step) {
                let v = refc.a[y * refc.bw + x];
                if v < 0.3 {
                    continue;
                }
                let (fx, fy) = ((refc.b[0] + x) as f64 + dx, (refc.b[1] + y) as f64 + dy);
                let in_box = fx >= cur.b[0] as f64 - 1.0
                    && fy >= cur.b[1] as f64 - 1.0
                    && fx <= cur.b[2] as f64 + 1.0
                    && fy <= cur.b[3] as f64 + 1.0;
                if !in_box && inside(clip, fx + 0.5, fy + 0.5) {
                    stray += v;
                }
            }
        }
        if n == 0 {
            f32::MAX
        } else {
            err / n as f32 + stray / (n as f32).max(1.0)
        }
    };
    // Candidates: the visible edges that are not clipped line up.
    let mut best = (f32::MAX, 0.0, 0.0, None);
    for dx in [cv[0] - rv[0], cv[2] - rv[2]] {
        for dy in [cv[1] - rv[1], cv[3] - rv[3]] {
            let moved = [rv[0] + dx, rv[1] + dy, rv[2] + dx, rv[3] + dy];
            let cut =
                moved[0] < cv[0] - 1.0 || moved[1] < cv[1] - 1.0 || moved[2] > cv[2] + 1.0 || moved[3] > cv[3] + 1.0;
            let clip = cut.then_some([cv[0] - 0.5, cv[1] - 0.5, cv[2] + 0.5, cv[3] + 0.5]);
            let e = score(dx, dy, &clip);
            if e < best.0 {
                best = (e, dx, dy, clip);
            }
        }
    }
    // Subpixel: motion rarely lands on whole pixels.
    let (_, bx, by, clip) = best;
    for sx in [-0.5, -0.25, 0.0, 0.25, 0.5] {
        for sy in [-0.5, -0.25, 0.0, 0.25, 0.5] {
            let e = score(bx + sx, by + sy, &clip);
            if e < best.0 {
                best = (e, bx + sx, by + sy, clip);
            }
        }
    }
    if std::env::var_os("ULTRAFAST_DEBUG_FIT").is_some() {
        eprintln!(
            "fit ref {:?} cur {:?} rv {rv:?} cv {cv:?}: err {:.3} at ({:.2}, {:.2}) clip {:?}",
            refc.b,
            cur.b,
            best.0,
            best.1,
            best.2,
            best.3.is_some()
        );
    }
    (best.0 < 0.03).then_some((best.1, best.2, best.3))
}

/// `cur` as `refc` scaled by `s` about the center of its visible box and
/// moved by (dx, dy): the placement, when it fits closely and the scale is
/// clearly not 1 (that is a move).
fn fit_scaled(refc: &Cover, cur: &Cover) -> Option<(f64, f64, f64)> {
    let (rv, cv) = (refc.visible()?, cur.visible()?);
    let (rw, rh) = (rv[2] - rv[0], rv[3] - rv[1]);
    let (cw, ch) = (cv[2] - cv[0], cv[3] - cv[1]);
    if rw < 4.0 || rh < 4.0 || cw < 2.0 || ch < 2.0 {
        return None;
    }
    let s0 = ((cw / rw) + (ch / rh)) / 2.0;
    // Not the same proportions: not a uniform scale.
    if !(0.03..=4.0).contains(&s0) || (cw / rw - ch / rh).abs() > 0.15 * s0 + 2.0 / rw.min(rh) {
        return None;
    }
    let c = [(rv[0] + rv[2]) / 2.0, (rv[1] + rv[3]) / 2.0];
    let d0 = [(cv[0] + cv[2]) / 2.0 - c[0], (cv[1] + cv[3]) / 2.0 - c[1]];
    let step = (((cur.bw * cur.bh) as f64 / 20_000.0).sqrt().ceil() as usize).max(1);
    let score = |dx: f64, dy: f64, s: f64| -> f32 {
        let (mut err, mut n) = (0f32, 0usize);
        for y in (0..cur.bh).step_by(step) {
            for x in (0..cur.bw).step_by(step) {
                let (fx, fy) = ((cur.b[0] + x) as f64 + 0.5, (cur.b[1] + y) as f64 + 0.5);
                let r = refc.at(c[0] + (fx - c[0] - dx) / s - 0.5, c[1] + (fy - c[1] - dy) / s - 0.5);
                let v = cur.a[y * cur.bw + x];
                if r > 0.02 || v > 0.02 {
                    err += ((r - v).abs() - 0.25).max(0.0);
                    n += 1;
                }
            }
        }
        if n == 0 { f32::MAX } else { err / n as f32 }
    };
    let mut best = (f32::MAX, d0[0], d0[1], s0);
    for k in [-0.03, -0.015, 0.0, 0.015, 0.03] {
        let s = s0 * (1.0 + k);
        let e = score(d0[0], d0[1], s);
        if e < best.0 {
            best = (e, d0[0], d0[1], s);
        }
    }
    for d in [0.5, 0.25] {
        let (_, bx, by, bs) = best;
        for (sx, sy) in [(-d, 0.0), (d, 0.0), (0.0, -d), (0.0, d)] {
            let e = score(bx + sx, by + sy, bs);
            if e < best.0 {
                best = (e, bx + sx, by + sy, bs);
            }
        }
    }
    (best.0 < 0.03 && (best.3 - 1.0).abs() > 0.01).then_some((best.1, best.2, best.3))
}

/// Where an element color sits in one frame.
#[derive(Clone, Copy, PartialEq)]
struct Placement {
    shape: usize,
    /// The shape was traced at this frame (no earlier outline fits it).
    own: bool,
    opacity: f32,
    dx: f64,
    dy: f64,
    clip: Option<[f64; 4]>,
    /// Scale about the reference's center (1 when only moved).
    scale: f64,
}

/// One element color over a shot.
struct Track {
    color: [f32; 3],
    /// Outlines: the reference first, then frames that are not it moved.
    shapes: Vec<String>,
    frames: Vec<Option<Placement>>,
    /// Coverage over the shot (for ordering).
    weight: f64,
    reference: Cover,
    last: Option<(Cover, Placement)>,
    /// The latest traced shapes, which later frames may be moved copies of.
    keys: Vec<(usize, Cover)>,
    /// Coverage of each traced shape that later frames are moved copies of.
    covers: HashMap<usize, Cover>,
    /// The color is the background's own: it covers what the plate holds
    /// where the background shows (drawn first, right over the plate).
    ground: bool,
    /// A linear ramp instead of the flat `color` (its mean).
    fill: Option<Fill>,
}

impl Track {
    /// Coverage of shape `si` as traced, if frames are fitted to it.
    #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
    fn cover(&self, si: usize) -> Option<&Cover> {
        if si == 0 {
            Some(&self.reference)
        } else {
            self.covers.get(&si)
        }
    }
}

/// Each palette color's coverage, in box `b`, of an element (its pixels
/// are those labeled `members`) in frame `fr` over `plate`. A pixel's
/// color comes from the opaque shape it borders: a gray could be
/// half-covered black.
#[allow(clippy::too_many_arguments)]
fn matte(
    fr: &Frame,
    plate: &Frame,
    w: usize,
    labels: &[u32],
    mask: &[bool],
    members: &[u32],
    b: [usize; 4],
    pal: &[Fill],
    threshold: f32,
) -> Vec<Vec<f32>> {
    let (bw, bh) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
    let mut fit = vec![(f32::MAX, 0f32); bw * bh * pal.len()];
    let mut fg = vec![false; bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            let i = (b[1] + y) * w + b[0] + x;
            // The element and its antialiased rim (one pixel beyond).
            let rim = mask[i]
                || (x > 0 && mask[i - 1])
                || (x + 1 < bw && mask[i + 1])
                || (y > 0 && mask[i - w])
                || (y + 1 < bh && mask[i + w]);
            if !members.contains(&labels[i]) || !rim || diff(fr, plate, i) <= threshold * 0.5 {
                continue;
            }
            fg[y * bw + x] = true;
            let fpx = [
                fr.rgba[i * 4] as f32,
                fr.rgba[i * 4 + 1] as f32,
                fr.rgba[i * 4 + 2] as f32,
            ];
            let bpx = [
                plate.rgba[i * 4] as f32,
                plate.rgba[i * 4 + 1] as f32,
                plate.rgba[i * 4 + 2] as f32,
            ];
            for (j, f) in pal.iter().enumerate() {
                let c = f.at((b[0] + x) as f32, (b[1] + y) as f32);
                let d = [c[0] - bpx[0], c[1] - bpx[1], c[2] - bpx[2]];
                let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                if dd < 100.0 {
                    continue;
                }
                let a = (((fpx[0] - bpx[0]) * d[0] + (fpx[1] - bpx[1]) * d[1] + (fpx[2] - bpx[2]) * d[2]) / dd)
                    .clamp(0.0, 1.0);
                let r = (0..3).map(|c| (fpx[c] - bpx[c] - a * d[c]).powi(2)).sum::<f32>();
                fit[(y * bw + x) * pal.len() + j] = (r, a);
            }
        }
    }
    let mut core = vec![usize::MAX; bw * bh];
    for (q, c) in core.iter_mut().enumerate() {
        if !fg[q] {
            continue;
        }
        let mut best = (f32::MAX, usize::MAX);
        for j in 0..pal.len() {
            let (r, a) = fit[q * pal.len() + j];
            if a > 0.9 && r < 600.0 && r < best.0 {
                best = (r, j);
            }
        }
        *c = best.1;
    }
    let mut alpha = vec![vec![0f32; bw * bh]; pal.len()];
    for y in 0..bh {
        for x in 0..bw {
            let q = y * bw + x;
            if !fg[q] {
                continue;
            }
            let mut near = [false; 8];
            let mut any = false;
            for yy in y.saturating_sub(2)..(y + 3).min(bh) {
                for xx in x.saturating_sub(2)..(x + 3).min(bw) {
                    let c = core[yy * bw + xx];
                    if c < 8 {
                        near[c] = true;
                        any = true;
                    }
                }
            }
            let mut best = (f32::MAX, usize::MAX, 0f32);
            for j in 0..pal.len() {
                if any && !(j < 8 && near[j]) {
                    continue;
                }
                let (r, a) = fit[q * pal.len() + j];
                // Without a core nearby, prefer the opaque reading.
                let cost = if any { r } else { r + 400.0 * (1.0 - a) * (1.0 - a) };
                if cost < best.0 {
                    best = (cost, j, a);
                }
            }
            if best.1 < pal.len() && best.2 > 0.02 {
                alpha[best.1][q] = best.2;
            }
        }
    }
    alpha
}

/// A line of type read from the video, with the motion of its elements.
struct TextLine {
    /// Text node params at the reference frame.
    params: Value,
    color: [f32; 3],
    frames: Vec<Option<Placement>>,
    /// How well the matched font covers the reference (dB).
    fit: f64,
}

/// Ink box (frame px) of coverage `c` placed by `p`, within the columns
/// `cut` (none there: none).
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn ink_in(c: &Cover, p: Placement, cut: [f64; 2]) -> Option<[f64; 4]> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for y in 0..c.bh {
        for x in 0..c.bw {
            if c.a[y * c.bw + x] <= 0.3 {
                continue;
            }
            let (fx, fy) = ((c.b[0] + x) as f64 + p.dx, (c.b[1] + y) as f64 + p.dy);
            if fx + 0.5 < cut[0] || fx + 0.5 >= cut[1] {
                continue;
            }
            b = [b[0].min(fx), b[1].min(fy), b[2].max(fx + 1.0), b[3].max(fy + 1.0)];
        }
    }
    (b[0] <= b[2]).then_some(b)
}

/// `frame` with the elements `own` (each a reference coverage and where it
/// sits) taken out: their pixels near `ink` filled in from around them, a
/// ring at a time.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn without(frame: &Frame, own: &[(&Cover, Placement)], ink: [f64; 4]) -> Vec<u8> {
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    let pad = 3.0;
    let x0 = ((ink[0] - pad).floor() as i64).clamp(0, fw - 1);
    let y0 = ((ink[1] - pad).floor() as i64).clamp(0, fh - 1);
    let x1 = ((ink[2] + pad).ceil() as i64).clamp(x0 + 1, fw);
    let y1 = ((ink[3] + pad).ceil() as i64).clamp(y0 + 1, fh);
    let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    // Covered pixels and a fringe (antialiasing, codec bleed).
    let mut hole = vec![false; rw * rh];
    for y in 0..rh {
        for x in 0..rw {
            let (fx, fy) = ((x0 + x as i64) as f64, (y0 + y as i64) as f64);
            hole[y * rw + x] = own.iter().any(|(c, p)| c.at(fx - p.dx, fy - p.dy) > 0.02);
        }
    }
    // Two pixels: compression bleeds a dark glyph's color into the next one.
    let hole = dilate(&hole, rw, rh, 2);
    let mut px = frame.rgba.clone();
    let mut open = hole;
    loop {
        let mut fill = Vec::new();
        for y in 0..rh {
            for x in 0..rw {
                if !open[y * rw + x] {
                    continue;
                }
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for (dx, dy) in [
                    (-1i64, 0i64),
                    (1, 0),
                    (0, -1),
                    (0, 1),
                    (-1, -1),
                    (1, -1),
                    (-1, 1),
                    (1, 1),
                ] {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= rw as i64 || ny >= rh as i64 || open[ny as usize * rw + nx as usize] {
                        continue;
                    }
                    let k = (((y0 + ny) * fw + x0 + nx) * 4) as usize;
                    for c in 0..3 {
                        sum[c] += px[k + c] as u32;
                    }
                    n += 1;
                }
                if n > 0 {
                    fill.push((x, y, sum.map(|v| ((v + n / 2) / n) as u8)));
                }
            }
        }
        if fill.is_empty() {
            break;
        }
        for (x, y, c) in fill {
            let k = (((y0 + y as i64) * fw + x0 + x as i64) * 4) as usize;
            px[k..k + 3].copy_from_slice(&c);
            open[y * rw + x] = false;
        }
    }
    px
}

/// Places a track at frame `fi` from its coverage there: unchanged from
/// the frame before, its reference (or a recently traced shape) moved and
/// clipped, or else its own new shape. Returns (moved, own) counts.
fn fit_frame(t: &mut Track, c: Option<Cover>, fi: usize) -> (usize, usize) {
    let Some(c) = c else {
        t.last = None;
        return (0, 0);
    };
    // Unchanged since the previous frame: the same placement.
    if let Some((lc, lp)) = &t.last
        && fi > 0
        && t.frames[fi - 1].is_some()
        && lc.same(&c)
    {
        let mut p = *lp;
        p.opacity = c.opacity;
        if p.own {
            p.own = false;
            if let Some((_, kc)) = t.keys.iter().find(|(si, _)| *si == p.shape) {
                t.covers.entry(p.shape).or_insert_with(|| kc.clone());
            }
        }
        t.frames[fi] = Some(p);
        return (0, 0);
    }
    // The reference moved, or else a recent traced shape moved (content
    // other than the reference's, holding).
    // Or the reference scaled about its center (a pop in or out).
    let fitted = fit_shift(&t.reference, &c)
        .map(|(dx, dy, clip)| (0, (dx, dy, clip, 1.0)))
        .or_else(|| fit_scaled(&t.reference, &c).map(|(dx, dy, s)| (0, (dx, dy, None, s))))
        .or_else(|| {
            t.keys
                .iter()
                .rev()
                .find_map(|(si, kc)| fit_shift(kc, &c).map(|(dx, dy, clip)| (*si, (dx, dy, clip, 1.0))))
        });
    let (place, counts) = match fitted {
        Some((si, (dx, dy, clip, scale))) => {
            if si != 0
                && !t.covers.contains_key(&si)
                && let Some((_, kc)) = t.keys.iter().find(|(k, _)| *k == si)
            {
                t.covers.insert(si, kc.clone());
            }
            (
                Placement {
                    shape: si,
                    own: false,
                    opacity: c.opacity,
                    dx,
                    dy,
                    clip,
                    scale,
                },
                (1, 0),
            )
        }
        None => {
            let Some(d) = c.trace() else {
                t.last = None;
                return (0, 0);
            };
            let si = match t.shapes.iter().position(|x| *x == d) {
                Some(si) => si,
                None => {
                    t.shapes.push(d);
                    t.shapes.len() - 1
                }
            };
            if si != 0 {
                t.keys.retain(|(k, _)| *k != si);
                t.keys.push((si, c.clone()));
                if t.keys.len() > 3 {
                    t.keys.remove(0);
                }
            }
            (
                Placement {
                    shape: si,
                    own: true,
                    opacity: c.opacity,
                    dx: 0.0,
                    dy: 0.0,
                    clip: None,
                    scale: 1.0,
                },
                (0, 1),
            )
        }
    };
    t.frames[fi] = Some(place);
    t.last = Some((c, place));
    counts
}

/// `f` over `items` on every core, results in order (work is taken one
/// item at a time, so uneven items balance).
fn par_map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    par_map_n(items, usize::MAX, f)
}

/// `par_map` on at most `most` threads (GPU work does not scale past a few).
fn par_map_n<T: Send, R: Send>(items: Vec<T>, most: usize, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    if items.len() < 2 || most < 2 {
        return items.into_iter().map(f).collect();
    }
    let n = items.len();
    let slots: Vec<std::sync::Mutex<(Option<T>, Option<R>)>> = items
        .into_iter()
        .map(|t| std::sync::Mutex::new((Some(t), None)))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(8, |c| c.get())
        .min(n)
        .min(most);
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let item = slots[i].lock().unwrap().0.take().unwrap();
                    let r = f(item);
                    slots[i].lock().unwrap().1 = Some(r);
                }
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().1.unwrap()).collect()
}

/// Frames (indices into `ground`, each frame's border color) where the
/// background becomes another color and stays it: the middle of each
/// change.
fn grounds_change(ground: &[[u8; 3]]) -> Vec<usize> {
    const HOLD: usize = 8;
    let near = |a: [u8; 3], b: [u8; 3]| (0..3).all(|c| a[c].abs_diff(b[c]) <= 12);
    let mut out = Vec::new();
    let mut held = ground.first().copied();
    let mut i = 0;
    while i < ground.len() {
        let Some(g) = held else { break };
        if near(ground[i], g) {
            i += 1;
            continue;
        }
        // Where the next steady color begins.
        let Some(j) = (i..ground.len().saturating_sub(HOLD - 1))
            .find(|&j| (j..j + HOLD).all(|k| near(ground[k], ground[j])) && !near(ground[j], g))
        else {
            break;
        };
        if (0..3).any(|c| ground[j][c].abs_diff(g[c]) > 40) {
            out.push((i + j) / 2);
        }
        held = Some(ground[j]);
        i = j + HOLD;
    }
    out
}

/// The run of frames around `i` that a track is present for.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn span(t: &Track, i: usize) -> std::ops::Range<usize> {
    let mut a = i;
    while a > 0 && t.frames[a - 1].is_some() {
        a -= 1;
    }
    let mut b = i + 1;
    while b < t.frames.len() && t.frames[b].is_some() {
        b += 1;
    }
    a..b
}

/// Whether a track is one outline over the span it is present for at `i`
/// (that outline moved, revealed or faded on most of its frames).
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn steady(t: &Track, i: usize) -> bool {
    let Some(p) = t.frames[i] else { return false };
    if p.own || t.cover(p.shape).is_none() {
        return false;
    }
    let s = span(t, i);
    let same = t.frames[s.clone()]
        .iter()
        .filter(|q| q.is_some_and(|q| !q.own && q.shape == p.shape && q.scale == 1.0))
        .count();
    same * 5 >= s.len() * 3
}

/// A text line's entrance: when its elements show before the frame `rest`
/// where it holds still, the glyph animators (a rise and a fade, staggered
/// glyph by glyph) that best render those frames are fitted and written into
/// `line` (its frames from the entrance on held at rest). Returns where the
/// entrance starts.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
#[allow(clippy::too_many_arguments)]
fn fit_entrance(
    device: &Device,
    reference: &mut Reference,
    plate: &Frame,
    context: &[Value],
    line: &mut TextLine,
    own: &[(usize, usize)],
    tracks: &HashMap<(usize, usize), Track>,
    rest: usize,
    (w, h, fps, f0): (usize, usize, f64, usize),
    log: &mut impl FnMut(&str),
) -> Result<Option<usize>, String> {
    // The frames before `rest` where any of its elements shows.
    let mut e0 = rest;
    while e0 > 0 && rest - e0 < 90 && own.iter().any(|k| tracks[k].frames[e0 - 1].is_some()) {
        e0 -= 1;
    }
    if rest - e0 < 4 {
        return Ok(None);
    }
    // Its ink at rest and the region the entrance moves through.
    let mut ink = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for k in own {
        let t = &tracks[k];
        let Some(p) = t.frames[rest] else { continue };
        let Some(v) = t.cover(p.shape).and_then(|c| c.visible()) else {
            continue;
        };
        ink = [
            ink[0].min(v[0] + p.dx),
            ink[1].min(v[1] + p.dy),
            ink[2].max(v[2] + p.dx),
            ink[3].max(v[3] + p.dy),
        ];
    }
    if ink[0] >= ink[2] {
        return Ok(None);
    }
    let ih = ink[3] - ink[1];
    let rx0 = ((ink[0] - ih * 0.3).floor().max(0.0)) as usize;
    let ry0 = ((ink[1] - ih * 0.6).floor().max(0.0)) as usize;
    let rx1 = ((ink[2] + ih * 0.3).ceil() as usize).min(w);
    let ry1 = ((ink[3] + ih * 0.8).ceil() as usize).min(h);
    let (rw, rh) = (rx1 - rx0, ry1 - ry0);
    if rw < 4 || rh < 4 {
        return Ok(None);
    }
    // What the frames show there: coverage of the line's color over the
    // plate (linear light).
    let lin = |v: u8| ultrafast_render::ops::srgb_to_linear(v as f64 / 255.0) as f32;
    let fg: [f32; 3] = line.color.map(|c| lin(c.round().clamp(0.0, 255.0) as u8));
    let times: Vec<f64> = (e0..rest).map(|i| (f0 + i) as f64 / fps).collect();
    let mut targets: Vec<Vec<f32>> = Vec::new();
    for &t in &times {
        let fr = reference.frame(t)?;
        let mut cov = vec![0f32; rw * rh];
        for y in 0..rh {
            for x in 0..rw {
                let i = ((ry0 + y) * w + rx0 + x) * 4;
                let b: [f32; 3] = std::array::from_fn(|c| lin(plate.rgba[i + c]));
                let c: [f32; 3] = std::array::from_fn(|c| lin(fr.rgba[i + c]));
                let d: [f32; 3] = std::array::from_fn(|k| fg[k] - b[k]);
                let dd: f32 = d.iter().map(|v| v * v).sum();
                if dd > 1e-4 {
                    cov[y * rw + x] = ((0..3).map(|k| (c[k] - b[k]) * d[k]).sum::<f32>() / dd).clamp(0.0, 1.0);
                }
            }
        }
        targets.push(cov);
    }
    // The line alone, white, in that region.
    let mut text = line.params.clone();
    let pos = [
        text["position"][0].as_f64().unwrap_or(0.0),
        text["position"][1].as_f64().unwrap_or(0.0),
    ];
    text["position"] = json!([pos[0] - rx0 as f64, pos[1] - ry0 as f64]);
    text["color"] = json!("#ffffff");
    let mut renderer = ultrafast_render::Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    renderer.set_mode(ultrafast_render::Mode::Exact);
    let glyphs = text["text"]
        .as_str()
        .map_or(1, |t| t.chars().filter(|c| !c.is_whitespace()).count().max(1));
    // Lines already read near it, as they move (their glyphs pass through
    // this one's region as they come in).
    let at_y = |p: &Value| -> f64 {
        let v = &p["position"];
        v.get("keys")
            .and_then(|k| k[0]["v"][1].as_f64())
            .or_else(|| v[1].as_f64())
            .unwrap_or(0.0)
    };
    let (y, size) = (at_y(&line.params), line.params["size"].as_f64().unwrap_or(64.0));
    let others: Vec<Value> = context
        .iter()
        .filter(|c| (at_y(c) - y).abs() < size * 2.5)
        .map(|c| {
            let mut c = c.clone();
            let pos = [
                c["position"][0].as_f64().unwrap_or(0.0),
                c["position"][1].as_f64().unwrap_or(0.0),
            ];
            c["position"] = json!([pos[0] - rx0 as f64, pos[1] - ry0 as f64]);
            c["color"] = json!("#ffffff");
            c
        })
        .collect();
    let with_others = |p: Value| -> Result<Document, String> {
        let mut nodes = serde_json::Map::new();
        nodes.insert("t".into(), json!({ "op": "text", "params": p }));
        let mut under = "t".to_string();
        for (k, c) in others.iter().enumerate() {
            let mut c = c.clone();
            let window = json!({ "in": c["__in"].clone(), "out": c["__out"].clone() });
            if let Some(m) = c.as_object_mut() {
                m.remove("__in");
                m.remove("__out");
            }
            nodes.insert(format!("c{k}"), json!({ "op": "text", "params": c, "timing": window }));
            nodes.insert(
                format!("o{k}"),
                json!({ "op": "over", "inputs": { "fg": format!("c{k}"), "bg": under } }),
            );
            under = format!("o{k}");
        }
        Document::from_json(
            &json!({
                "version": 1,
                "format": { "width": rw, "height": rh, "fps": fps, "duration": times.last().copied().unwrap_or(0.0) + 1.0 },
                "output": under,
                "nodes": nodes
            })
            .to_string(),
        )
        .map_err(|e| e.to_string())
    };
    let doc_of = |x: &[f64], ease: &str| -> Result<Document, String> {
        let mut p = text.clone();
        let (t0, dur, dy0, fade, stagger) = (x[0], x[1].max(0.05), x[2], x[3].max(0.02), x[4].max(0.0));
        p["glyph_offset"] =
            json!({ "keys": [{ "t": t0, "v": [0.0, dy0], "ease": ease }, { "t": t0 + dur, "v": [0.0, 0.0] }] });
        p["glyph_opacity"] = json!({ "keys": [{ "t": t0, "v": 0.0 }, { "t": t0 + fade, "v": 1.0 }] });
        p["stagger"] = json!(stagger);
        with_others(p)
    };
    let mut error = |doc: &Document| -> Result<f64, String> {
        let mut e = 0.0f64;
        for (t, target) in times.iter().zip(&targets) {
            let (px, _) = renderer.render_rgba8(doc, *t).map_err(|e| e.to_string())?;
            for (k, want) in target.iter().enumerate() {
                e += ((px[k * 4 + 3] as f32 / 255.0 - want) as f64).powi(2);
            }
        }
        Ok(e / (targets.len() * rw * rh).max(1) as f64)
    };
    // Held at rest the whole time: what the entrance must beat.
    let still = {
        let mut p = text.clone();
        p["glyph_opacity"] = json!(1.0);
        error(&with_others(p)?)?
    };
    let span = times.last().unwrap() - times[0];
    let x0 = [
        times[0] - 0.5 / fps,
        (span * 0.5).clamp(0.2, 1.0),
        ih * 0.4,
        (span * 0.4).clamp(0.1, 0.6),
        (span * 0.5 / glyphs as f64).min(0.05),
    ];
    let steps = [1.0 / fps, 0.1, ih * 0.2, 0.08, 0.004];
    let mut best: Option<(f64, Vec<f64>, &str)> = None;
    for ease in ["out_cubic", "out_expo"] {
        let (x, e) = crate::nelder_mead(&x0, &steps, 120, |x| error(&doc_of(x, ease)?))?;
        if best.as_ref().is_none_or(|b| e < b.0) {
            best = Some((e, x, ease));
        }
    }
    let Some((e, x, ease)) = best else { return Ok(None) };
    if std::env::var_os("ULTRAFAST_DEBUG_TYPE").is_some() {
        eprintln!(
            "DEBUG entrance {:?}: fit {e:.5} vs still {still:.5} over {} frames: {x:?}",
            line.params["text"],
            times.len()
        );
    }
    // Close, and far better than holding still (a masked rise or a blur
    // the model cannot make stays outlines).
    if !(e < 0.004 && e < still * 0.15) {
        return Ok(None);
    }
    let (t0, dur, dy0, fade, stagger) = (x[0], x[1].max(0.05), x[2], x[3].max(0.02), x[4].max(0.0));
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    line.params["glyph_offset"] =
        json!({ "keys": [{ "t": r3(t0), "v": [0.0, r3(dy0)], "ease": ease }, { "t": r3(t0 + dur), "v": [0.0, 0.0] }] });
    line.params["glyph_opacity"] = json!({ "keys": [{ "t": r3(t0), "v": 0.0 }, { "t": r3(t0 + fade), "v": 1.0 }] });
    line.params["stagger"] = json!(r3(stagger));
    let at_rest = line.frames[rest];
    for i in e0..rest {
        line.frames[i] = at_rest.map(|p| Placement { opacity: 1.0, ..p });
    }
    log(&format!(
        "  \"{}\": comes in glyph by glyph ({:.0} px rise over {:.2} s, {:.3} s apart)",
        line.params["text"].as_str().unwrap_or(""),
        dy0,
        dur,
        stagger
    ));
    Ok(Some(e0))
}

/// Lines of type: OCR at frames where text-like elements are whole; the
/// elements inside a line that move as one (each one outline moved or
/// revealed over the span it shows for) become matched-font text lines
/// carrying that motion, and leave `tracks` for that span. A line in more
/// than one style (an italic phrase) or with parts entering in turn
/// becomes one text line per run.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
#[allow(clippy::too_many_arguments)]
fn read_type(
    device: &Device,
    video: &Path,
    out_dir: &Path,
    plate_file: &str,
    plate: &Frame,
    (w, h, fps, f0): (usize, usize, f64, usize),
    tracks: &mut HashMap<(usize, usize), Track>,
    log: &mut impl FnMut(&str),
) -> Result<Vec<TextLine>, String> {
    let n = tracks.values().next().map_or(0, |t| t.frames.len());
    let debug = std::env::var_os("ULTRAFAST_DEBUG_TYPE").is_some();
    // Where to read: each span's middle frame where the element is whole.
    let mut frames: Vec<usize> = Vec::new();
    for t in tracks.values() {
        let mut i = 0;
        while i < n {
            if t.frames[i].is_none() {
                i += 1;
                continue;
            }
            let s = span(t, i);
            let whole: Vec<usize> = s
                .clone()
                .filter(|&j| t.frames[j].is_some_and(|p| !p.own && p.clip.is_none() && p.opacity >= 0.99))
                .collect();
            if let Some(&m) = whole.get(whole.len() / 2)
                && steady(t, m)
            {
                frames.push(m);
            }
            i = s.end;
        }
    }
    frames.sort_unstable();
    frames.dedup();
    // Debug: the elements at one frame, boxed by state (green steady and
    // whole, yellow fitted but not steady, red its own shape).
    if let Some(at) = std::env::var("ULTRAFAST_DEBUG_TRACKS_AT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        let mut fr = Reference::open(device, video, Some(w as u32), Some(h as u32))?.frame((f0 + at) as f64 / fps)?;
        for (k, t) in tracks.iter() {
            let Some(p) = t.frames[at] else { continue };
            let (col, b) = match t.cover(p.shape).and_then(|c| c.visible()) {
                Some(v) if !p.own => {
                    let col = if steady(t, at) && p.clip.is_none() {
                        [0u8, 200, 0]
                    } else {
                        [230, 180, 0]
                    };
                    (col, [v[0] + p.dx, v[1] + p.dy, v[2] + p.dx, v[3] + p.dy])
                }
                _ => ([220, 0, 0], {
                    // An own shape: its box from the shape's path.
                    let path = kurbo::BezPath::from_svg(&t.shapes[p.shape]).ok();
                    let bb = path.map(|q| kurbo::Shape::bounding_box(&q)).unwrap_or_default();
                    [bb.x0, bb.y0, bb.x1, bb.y1]
                }),
            };
            let (x0, y0, x1, y1) = (
                b[0].max(0.0) as usize,
                b[1].max(0.0) as usize,
                (b[2] as usize).min(w - 1),
                (b[3] as usize).min(h - 1),
            );
            for x in x0..=x1 {
                for y in [y0, y1] {
                    fr.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&col);
                }
            }
            for y in y0..=y1 {
                for x in [x0, x1] {
                    fr.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&col);
                }
            }
            let s = span(t, at);
            eprintln!(
                "TRACK {k:?} {:?} box {:.0},{:.0}-{:.0},{:.0} shape {} own {} clip {} span {}..{} steady {} color {:?}",
                col,
                b[0],
                b[1],
                b[2],
                b[3],
                p.shape,
                p.own,
                p.clip.is_some(),
                s.start,
                s.end,
                steady(t, at),
                t.color
            );
        }
        fr.write_png(&out_dir.join(format!("tracks-{at}.png")))?;
    }
    let mut out = Vec::new();
    // Lines that found no font, by text and place.
    let mut failed: std::collections::HashSet<(String, i64, i64)> = Default::default();
    // What each element had read from it: the stretch and the frames.
    let mut claims: HashMap<(usize, usize), Vec<([f64; 2], std::ops::Range<usize>)>> = HashMap::new();
    let mut reference = Reference::open(device, video, Some(w as u32), Some(h as u32))?;
    for fi in frames {
        // Elements whole at this frame, and their boxes.
        let whole: HashMap<(usize, usize), [f64; 4]> = tracks
            .iter()
            .filter(|(_, t)| t.frames[fi].is_some_and(|p| p.clip.is_none()) && steady(t, fi))
            .filter_map(|(k, t)| {
                let p = t.frames[fi]?;
                let v = t.cover(p.shape)?.visible()?;
                Some((*k, [v[0] + p.dx, v[1] + p.dy, v[2] + p.dx, v[3] + p.dy]))
            })
            .collect();
        if whole.is_empty() {
            continue;
        }
        let t = (f0 + fi) as f64 / fps + 0.25 / fps;
        let boxes = ultrafast_media::vision::recognize_text(video, t).map_err(|e| e.to_string())?;
        let frame = reference.frame(t)?;
        // Each line read on its own core; what they claim is settled in
        // reading order after.
        type Read = (
            Vec<(Vec<(usize, usize)>, [f64; 2], std::ops::Range<usize>, TextLine, String)>,
            Vec<String>,
            Vec<(String, i64, i64)>,
        );
        let candidates: Vec<&ultrafast_media::vision::TextBox> = boxes
            .iter()
            .filter(|b| b.confidence >= 0.5 && !b.text.trim().is_empty())
            .collect();
        let tracks_ro: &HashMap<(usize, usize), Track> = tracks;
        let failed_ro = &failed;
        let reads: Vec<Read> = par_map_n(candidates, 6, |b| {
            let tracks = tracks_ro;
            let failed = failed_ro;
            let mut lines = Vec::new();
            let mut logs: Vec<String> = Vec::new();
            let mut newly_failed: Vec<(String, i64, i64)> = Vec::new();
            let r = b.rect;
            let (x0, y0, x1, y1) = (
                r[0] - r[3] * 0.3,
                r[1] - r[3] * 0.3,
                r[0] + r[2] + r[3] * 0.3,
                r[1] + r[3] * 1.3,
            );
            // Elements inside the line not yet read.
            let mut members: Vec<(usize, usize)> = whole
                .iter()
                .filter(|(_, m)| m[0] >= x0 && m[2] <= x1 && m[1] >= y0 && m[3] <= y1)
                .map(|(k, _)| *k)
                .collect();
            if members.is_empty() {
                return (lines, logs, newly_failed);
            }
            members.sort_by(|a, b| whole[a][0].total_cmp(&whole[b][0]));
            let placed = |k: &(usize, usize)| whole[k];
            // Members that move as one (one color, one motion) around this
            // frame form groups, over the window of frames where they do
            // (glyphs entering in turn join once they hold still). A frame
            // that is not a member's outline at `fi` moved says nothing.
            let window = |a: &(usize, usize), k: &(usize, usize)| -> Option<std::ops::Range<usize>> {
                let (ta, tk) = (&tracks[a], &tracks[k]);
                if !(0..3).all(|c| (tk.color[c] - ta.color[c]).abs() < 24.0) {
                    return None;
                }
                let (pa, pk) = (ta.frames[fi].unwrap(), tk.frames[fi].unwrap());
                let fits = |p: &Option<Placement>, at: &Placement| {
                    p.filter(|p| !p.own && p.shape == at.shape && p.scale == 1.0)
                };
                let agree = |i: usize| match (fits(&ta.frames[i], &pa), fits(&tk.frames[i], &pk)) {
                    (Some(x), Some(y)) => {
                        (x.dx - pa.dx - (y.dx - pk.dx)).abs() < 0.6 && (x.dy - pa.dy - (y.dy - pk.dy)).abs() < 0.6
                    }
                    // One there and the other not at all: only while
                    // entering (masked or fading).
                    (Some(x), None) if tk.frames[i].is_none() => x.clip.is_some() || x.opacity < 0.5,
                    (None, Some(y)) if ta.frames[i].is_none() => y.clip.is_some() || y.opacity < 0.5,
                    (None, None) => ta.frames[i].is_some() || tk.frames[i].is_some(),
                    _ => true,
                };
                let (sa, sk) = (span(ta, fi), span(tk, fi));
                let (lo, hi) = (sa.start.min(sk.start), sa.end.max(sk.end));
                let mut a0 = fi;
                while a0 > lo && agree(a0 - 1) {
                    a0 -= 1;
                }
                let mut a1 = fi + 1;
                while a1 < hi && agree(a1) {
                    a1 += 1;
                }
                // Most of the shorter one's life, and not a passing moment.
                let need = (sa.len().min(sk.len()) as f64 * 0.6).max(8.0) as usize;
                (a1 - a0 >= need).then_some(a0..a1)
            };
            let mut groups: Vec<(Vec<(usize, usize)>, std::ops::Range<usize>)> = Vec::new();
            for k in &members {
                let mut joined = false;
                for (g, w) in groups.iter_mut() {
                    if let Some(x) = window(&g[0], k) {
                        g.push(*k);
                        *w = w.start.max(x.start)..w.end.min(x.end);
                        joined = true;
                        break;
                    }
                }
                if !joined {
                    groups.push((vec![*k], span(&tracks[k], fi)));
                }
            }
            // Each word belongs to the group with (nearly) all the ink in its
            // span; words split between groups stay shapes.
            let words: Vec<(String, [f64; 4])> = if b.words.is_empty() {
                vec![(b.text.trim().to_string(), r)]
            } else {
                b.words.clone()
            };
            let owner: Vec<Option<usize>> = words
                .iter()
                .map(|(_, wr)| {
                    // Only colors that stand out from what is behind the word
                    // as much as its type does (a highlight band under it
                    // does not own it).
                    let (cx, cy) = (
                        ((wr[0] + wr[2] / 2.0) as usize).min(w - 1),
                        ((wr[1] + wr[3] / 2.0) as usize).min(h - 1),
                    );
                    let behind = &plate.rgba[(cy * w + cx) * 4..(cy * w + cx) * 4 + 3];
                    let contrast: Vec<f32> = groups
                        .iter()
                        .map(|(g, _)| {
                            let c = tracks[&g[0]].color;
                            (0..3).map(|k| (c[k] - behind[k] as f32).powi(2)).sum::<f32>().sqrt()
                        })
                        .collect();
                    let strongest = contrast.iter().copied().fold(0f32, f32::max);
                    let share: Vec<f64> = groups
                        .iter()
                        .zip(&contrast)
                        .map(|((g, _), c)| {
                            if *c < strongest * 0.5 {
                                return 0.0;
                            }
                            g.iter()
                                .map(|k| {
                                    let m = placed(k);
                                    (m[2].min(wr[0] + wr[2]) - m[0].max(wr[0])).max(0.0) * (m[3] - m[1])
                                })
                                .sum()
                        })
                        .collect();
                    let total: f64 = share.iter().sum();
                    let (gi, best) = share.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
                    (*best > 0.0 && *best >= 0.85 * total).then_some(gi)
                })
                .collect();
            if debug {
                eprintln!(
                    "DEBUG {fi} {:?}: {} groups; {:?}",
                    b.text,
                    groups.len(),
                    words
                        .iter()
                        .zip(&owner)
                        .map(|(w, o)| format!("{}:{o:?}", w.0))
                        .collect::<Vec<_>>()
                );
            }
            // Runs: neighboring words of one group.
            let mut runs: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
            for (i, o) in owner.iter().enumerate() {
                let Some(g) = *o else { continue };
                match runs.last_mut() {
                    Some((lg, range)) if *lg == g && range.end == i => range.end = i + 1,
                    _ => runs.push((g, i..i + 1)),
                }
            }
            type Found = (Document, f64, Value, Vec<(usize, usize)>, [f64; 2]);
            // Matches the words in `span` (their ink among `pool`): the font
            // over the frame with those words' own ink filled in from around
            // it; `only` keeps one face (a line's font, for its parts).
            let attempt = |pool: &[(usize, usize)],
                           span: std::ops::Range<usize>,
                           only: Option<(String, u16, bool)>|
             -> Option<Found> {
                let text = words[span.clone()]
                    .iter()
                    .map(|w| w.0.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !text.chars().any(char::is_alphanumeric) {
                    return None;
                }
                let wr = words[span.clone()]
                    .iter()
                    .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, (_, r)| {
                        [
                            a[0].min(r[0]),
                            a[1].min(r[1]),
                            a[2].max(r[0] + r[2]),
                            a[3].max(r[1] + r[3]),
                        ]
                    });
                // The words' stretch: halfway to the words on either side
                // (an element running across words of two styles is read
                // in parts).
                let cut = [
                    if span.start > 0 {
                        let (p, q) = (words[span.start - 1].1, words[span.start].1);
                        (p[0] + p[2] + q[0]) / 2.0
                    } else {
                        f64::MIN
                    },
                    if span.end < words.len() {
                        let (p, q) = (words[span.end - 1].1, words[span.end].1);
                        (p[0] + p[2] + q[0]) / 2.0
                    } else {
                        f64::MAX
                    },
                ];
                let mut ink = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                let own: Vec<(usize, usize)> = pool
                    .iter()
                    .filter(|k| {
                        let t = &tracks[*k];
                        let p = t.frames[fi].unwrap();
                        let Some(b) = t.cover(p.shape).and_then(|c| ink_in(c, p, cut)) else {
                            return false;
                        };
                        ink = [ink[0].min(b[0]), ink[1].min(b[1]), ink[2].max(b[2]), ink[3].max(b[3])];
                        true
                    })
                    .copied()
                    .collect();
                if own.is_empty() {
                    return None;
                }
                // The words' ink must be (nearly) all there: part of a word
                // in another element would be drawn twice.
                if ink[2] - ink[0] < 0.8 * (wr[2] - wr[0]) {
                    if debug {
                        eprintln!(
                            "DEBUG   {text:?}: ink {:.0}-{:.0} short of words {:.0}-{:.0}",
                            ink[0], ink[2], wr[0], wr[2]
                        );
                    }
                    return None;
                }
                let covers: Vec<(&Cover, Placement)> = own
                    .iter()
                    .map(|k| {
                        let p = tracks[k].frames[fi].unwrap();
                        (tracks[k].cover(p.shape).unwrap(), p)
                    })
                    .collect();
                let bg = without(&frame, &covers, ink);
                if std::env::var("ULTRAFAST_DEBUG_BG").is_ok_and(|v| v == text) {
                    let f = Frame {
                        width: w as u32,
                        height: h as u32,
                        rgba: bg.clone(),
                    };
                    let _ = f.write_png(&out_dir.join(format!("bg-{fi}.png")));
                    eprintln!("DEBUG bg for {text:?} at {fi} (t {t:.3}): ink {ink:?}");
                }
                let color = tracks[&own[0]].color;
                // Ink of the line's color left after taking the run's
                // elements out: part of a glyph in another element.
                let left = {
                    let (x0, y0) = (ink[0].max(0.0) as usize, ink[1].max(0.0) as usize);
                    let (x1, y1) = ((ink[2] as usize).min(w), (ink[3] as usize).min(h));
                    let mut n = 0usize;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let k = (y * w + x) * 4;
                            if (0..3).all(|c| (bg[k + c] as f32 - color[c]).abs() < 40.0) {
                                n += 1;
                            }
                        }
                    }
                    n
                };
                let mass: f64 = covers.iter().map(|(c, _)| c.mass).sum();
                if debug {
                    eprintln!("DEBUG   {text:?}: {left} px of its color left beside {mass:.0} px of its elements");
                }
                let opts = crate::fonts::MatchOptions {
                    background: Some(std::sync::Arc::new(bg)),
                    ink: Some(ink),
                    only,
                    ..Default::default()
                };
                let draft = Document::from_json(
                    &json!({
                        "version": 1,
                        "format": { "width": w, "height": h, "fps": fps, "duration": t + 1.0 },
                        "output": "o",
                        "nodes": {
                            "plate": { "op": "image", "params": { "src": plate_file, "fit": "fill" } },
                            "txt": { "op": "text", "params": { "text": text, "size": ((wr[3] - wr[1]) * 0.95).max(4.0),
                                     "color": hex(color), "position": [wr[0], (wr[1] + wr[3]) / 2.0], "valign": "middle" } },
                            "o": { "op": "over", "inputs": { "fg": "txt", "bg": "plate" } }
                        }
                    })
                    .to_string(),
                )
                .ok()?;
                let (mut matched, mut report) =
                    crate::fonts::match_font(device, &draft, out_dir, video, "txt", t, &opts).ok()?;
                let mut fit = report["best"]["coverage_psnr"].as_f64().unwrap_or(0.0);
                // OCR reads dashes and dots loosely: each look-alike set in
                // the face found, kept where it fits better.
                const LOOKALIKE: [&[char]; 1] = [&['-', '–', '—', '−', '·', '•', '∙']];
                for set in LOOKALIKE {
                    let Some(&from) = set.iter().find(|c| text.contains(**c)) else {
                        continue;
                    };
                    let face = (
                        report["best"]["font"].as_str().unwrap_or("").to_string(),
                        report["best"]["weight"].as_u64().unwrap_or(400) as u16,
                        report["best"]["style"] == "italic",
                    );
                    for &to in set.iter().filter(|c| **c != from) {
                        let alt = text.replace(from, &to.to_string());
                        let mut d = draft.clone();
                        if d.apply(&[ultrafast_graph::Edit::SetParam {
                            node: "txt".into(),
                            param: "text".into(),
                            value: ultrafast_graph::Param::Text(alt),
                        }])
                        .is_err()
                        {
                            continue;
                        }
                        let o = crate::fonts::MatchOptions {
                            only: Some(face.clone()),
                            ..opts.clone()
                        };
                        if let Ok((m, r)) = crate::fonts::match_font(device, &d, out_dir, video, "txt", t, &o) {
                            let f = r["best"]["coverage_psnr"].as_f64().unwrap_or(0.0);
                            if f > fit + 0.5 {
                                (matched, report, fit) = (m, r, f);
                            }
                        }
                    }
                }
                if debug {
                    eprintln!(
                        "DEBUG   {text:?} ({} members, only {:?}): {} {} {} at {fit:.1}; then {}",
                        own.len(),
                        opts.only,
                        report["best"]["font"],
                        report["best"]["weight"],
                        report["best"]["style"],
                        report["runners_up"]
                            .as_array()
                            .map(|v| v
                                .iter()
                                .take(4)
                                .map(|r| format!("{} {} {}", r["font"], r["weight"], r["coverage_psnr"]))
                                .collect::<Vec<_>>()
                                .join(", "))
                            .unwrap_or_default()
                    );
                }
                // Short words fit many faces; they need a closer fit. A loose
                // fit (hairlines blur in compression) counts when no other
                // family comes near it.
                let short = text.chars().filter(|c| c.is_alphanumeric()).count() <= 3;
                let family = report["best"]["family"].clone();
                let other = report["runners_up"]
                    .as_array()
                    .and_then(|v| v.iter().find(|r| r["family"] != family))
                    .and_then(|r| r["coverage_psnr"].as_f64())
                    .unwrap_or(0.0);
                let sure = fit >= 26.0
                    || (!short && fit >= 22.0 && fit - other >= 3.0)
                    || (short && fit >= 24.0 && fit - other >= 3.0);
                Some((matched, fit, report, own, cut)).filter(|_| sure)
            };
            let face_of = |x: &Found| -> Option<(String, u16, bool)> {
                let b = &x.2["best"];
                Some((
                    b["font"].as_str()?.to_string(),
                    b["weight"].as_u64()? as u16,
                    b["style"] == "italic",
                ))
            };
            let key = |span: &std::ops::Range<usize>| {
                let r = words[span.start].1;
                (
                    words[span.clone()]
                        .iter()
                        .map(|w| w.0.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    (r[0] / 8.0).round() as i64,
                    (r[1] / 8.0).round() as i64,
                )
            };
            let mut accepted: Vec<(Found, std::ops::Range<usize>)> = Vec::new();
            // A line of one color whose words all belong to groups: its font
            // from the whole line, then each run placed in that font (runs
            // move separately when words enter in turn).
            let all = 0..words.len();
            let whole_line = owner.iter().all(Option::is_some)
                && runs.len() > 1
                && runs.iter().all(|(g, _)| {
                    (0..3).all(|c| (tracks[&groups[*g].0[0]].color[c] - tracks[&groups[0].0[0]].color[c]).abs() < 24.0)
                })
                && !(failed.contains(&key(&all)) || newly_failed.contains(&key(&all)));
            let line_face = if whole_line {
                // The elements of the groups that own words (not a band
                // behind them).
                let pool: Vec<(usize, usize)> = runs.iter().flat_map(|(g, _)| groups[*g].0.iter().copied()).collect();
                let found = attempt(&pool, all.clone(), None);
                if found.is_none() {
                    newly_failed.push(key(&all));
                }
                found.as_ref().and_then(face_of)
            } else {
                None
            };
            for (g, span) in runs {
                if failed.contains(&key(&span)) || newly_failed.contains(&key(&span)) {
                    continue;
                }
                let held = groups[g].1.clone();
                if let Some(face) = &line_face
                    && let Some(x) = attempt(&groups[g].0, span.clone(), Some(face.clone()))
                {
                    accepted.push((x, held));
                    continue;
                }
                let whole = attempt(&groups[g].0, span.clone(), None);
                // A close fit stands; a loose one (or none) may be two
                // styles in one run (an italic phrase, another family): the
                // split where both sides fit best, if clearly better.
                let loose = whole.as_ref().is_none_or(|x| x.1 < 26.0);
                // A side too short to name its own face may take one this
                // line already has (an "a" in the roman of the words before).
                let faces_here: Vec<(String, u16, bool)> = accepted
                    .iter()
                    .filter_map(|(x, _)| face_of(x))
                    .chain(line_face.clone())
                    .collect();
                let side = |r: std::ops::Range<usize>| {
                    attempt(&groups[g].0, r.clone(), None).or_else(|| {
                        faces_here
                            .iter()
                            .filter_map(|f| attempt(&groups[g].0, r.clone(), Some(f.clone())))
                            .max_by(|a, b| a.1.total_cmp(&b.1))
                    })
                };
                let split = if loose && (2..=10).contains(&span.len()) {
                    (span.start + 1..span.end)
                        .filter_map(|k| {
                            let a = side(span.start..k)?;
                            let b = side(k..span.end)?;
                            Some((a.1.min(b.1), a, b))
                        })
                        .max_by(|a, b| a.0.total_cmp(&b.0))
                } else {
                    None
                };
                match (whole, split) {
                    (Some(x), Some(sp)) if sp.0 < x.1 + 3.0 => accepted.push((x, held)),
                    (_, Some((_, a, b))) => {
                        accepted.push((a, held.clone()));
                        accepted.push((b, held));
                    }
                    (Some(x), None) => accepted.push((x, held)),
                    (None, None) => {
                        logs.push(format!("  \"{}\": no font", key(&span).0));
                        newly_failed.push(key(&span));
                    }
                }
            }
            for ((matched, fit, report, own, cut), held) in accepted {
                let msg = format!(
                    "  \"{}\": {} {} {} at {fit:.1} dB",
                    report["text"].as_str().unwrap_or(""),
                    report["best"]["font"],
                    report["best"]["weight"],
                    report["best"]["style"]
                );
                let color = tracks[&own[0]].color;
                let Ok(mut params) = serde_json::to_value(&matched.nodes["txt"].params) else {
                    continue;
                };
                params["markup"] = json!(false);
                // Over the members' spans: placements relative to this frame
                // from whichever member is its outline moved; a mask is the
                // one any member is clipped by. Frames where none is take the
                // nearest that was.
                // The window where the members move as one.
                let range = held.clone();
                // A member's clip is the visible part of its own box: the
                // line's is around every member's visible part.
                let boxes: Vec<[f64; 4]> = own
                    .iter()
                    .map(|k| {
                        let at = tracks[k].frames[fi].unwrap();
                        let v = placed(k);
                        [v[0] - at.dx, v[1] - at.dy, v[2] - at.dx, v[3] - at.dy]
                    })
                    .collect();
                let at_frame = |i: usize| -> Option<Placement> {
                    let mut out: Option<Placement> = None;
                    let mut clipped = false;
                    let mut seen = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                    for (k, b) in own.iter().zip(&boxes) {
                        let tr = &tracks[k];
                        let at = tr.frames[fi].unwrap();
                        let Some(p) = tr.frames[i].filter(|p| !p.own && p.shape == at.shape && p.scale == 1.0) else {
                            continue;
                        };
                        let v = match p.clip {
                            Some(c) => {
                                clipped = true;
                                c
                            }
                            None => [b[0] + p.dx, b[1] + p.dy, b[2] + p.dx, b[3] + p.dy],
                        };
                        seen = [
                            seen[0].min(v[0]),
                            seen[1].min(v[1]),
                            seen[2].max(v[2]),
                            seen[3].max(v[3]),
                        ];
                        if out.is_none() {
                            out = Some(Placement {
                                dx: p.dx - at.dx,
                                dy: p.dy - at.dy,
                                ..p
                            });
                        }
                    }
                    out.map(|p| Placement {
                        clip: clipped.then_some(seen),
                        ..p
                    })
                };
                let present = |i: usize| -> Option<f32> {
                    own.iter()
                        .filter_map(|k| tracks[k].frames[i])
                        .map(|p| p.opacity)
                        .reduce(f32::max)
                };
                let moved: Vec<Option<Placement>> = (0..n)
                    .map(|i| if range.contains(&i) { at_frame(i) } else { None })
                    .collect();
                let fitted: Vec<usize> = (0..n).filter(|&i| moved[i].is_some()).collect();
                let frames_rel: Vec<Option<Placement>> = (0..n)
                    .map(|i| {
                        if !range.contains(&i) {
                            return None;
                        }
                        let opacity = present(i)?;
                        let near = match moved[i] {
                            Some(p) => p,
                            None => moved[*fitted.iter().min_by_key(|&&j| (j as i64 - i as i64).abs())?]?,
                        };
                        Some(Placement { opacity, ..near })
                    })
                    .collect();
                lines.push((
                    own,
                    cut,
                    held,
                    TextLine {
                        params,
                        color,
                        frames: frames_rel,
                        fit,
                    },
                    msg,
                ));
            }
            (lines, logs, newly_failed)
        });
        type Claimed = (Vec<(usize, usize)>, [f64; 2], std::ops::Range<usize>, TextLine);
        let mut found: Vec<Claimed> = Vec::new();
        for (lines, logs, newly_failed) in reads {
            for l in logs {
                log(&l);
            }
            failed.extend(newly_failed);
            for (own, cut, held, line, msg) in lines {
                // An element read twice over the same stretch is one line too
                // many; over different stretches it is words of two styles.
                let clash =
                    |(o, c, _, _): &Claimed| own.iter().any(|k| o.contains(k)) && c[0] < cut[1] && cut[0] < c[1];
                // Or read at an earlier frame over the same time.
                let before = own.iter().any(|k| {
                    claims.get(k).is_some_and(|v| {
                        v.iter().any(|(c, win)| {
                            c[0] < cut[1] && cut[0] < c[1] && win.start < held.end && held.start < win.end
                        })
                    })
                });
                if before || found.iter().any(clash) {
                    continue;
                }
                log(&msg);
                found.push((own, cut, held, line));
            }
        }
        // The read elements leave their tracks for the window the text
        // covers (their entrances and exits outside it stay shapes), once
        // their whole width is read.
        let mut windows: Vec<((usize, usize), std::ops::Range<usize>)> = Vec::new();
        let mut stretches: HashMap<(usize, usize), Vec<[f64; 2]>> = HashMap::new();
        for (own, cut, held, mut line) in found {
            // Glyphs that came in one after another before the line held
            // still: the text animators that do it, fitted (with the lines
            // read so far moving as they do).
            let mut from = held.start;
            // (Each shown only over the frames it shows.)
            let context: Vec<Value> = out
                .iter()
                .filter_map(|l: &TextLine| {
                    let a = l.frames.iter().position(Option::is_some)?;
                    let b = l.frames.iter().rposition(Option::is_some)?;
                    let mut p = l.params.clone();
                    p["__in"] = json!((f0 + a) as f64 / fps);
                    p["__out"] = json!((f0 + b + 1) as f64 / fps);
                    Some(p)
                })
                .collect();
            if let Some(e0) = fit_entrance(
                device,
                &mut reference,
                plate,
                &context,
                &mut line,
                &own,
                tracks,
                held.start,
                (w, h, fps, f0),
                log,
            )? {
                from = e0;
            }
            for k in &own {
                windows.push((*k, from..held.end));
                stretches.entry(*k).or_default().push(cut);
                claims.entry(*k).or_default().push((cut, from..held.end));
            }
            out.push(line);
        }
        for (k, window) in windows {
            let t = &tracks[&k];
            let Some(p) = t.frames[fi].or_else(|| t.frames.iter().flatten().next().copied()) else {
                continue;
            };
            let Some(v) = t.cover(p.shape).and_then(|c| c.visible()) else {
                continue;
            };
            let (x0, x1) = (v[0] + p.dx, v[2] + p.dx);
            // The read stretches, joined, must span the element.
            let mut cs = stretches[&k].clone();
            cs.sort_by(|a, b| a[0].total_cmp(&b[0]));
            let mut reach = x0;
            for c in &cs {
                if c[0] <= reach + 1.0 {
                    reach = reach.max(c[1]);
                }
            }
            if reach < x1 - 1.0 {
                continue;
            }
            let t = tracks.get_mut(&k).unwrap();
            for i in window {
                t.frames[i] = None;
            }
        }
    }
    tracks.retain(|_, t| t.frames.iter().any(Option::is_some));
    Ok(out)
}

/// Decomposes `video` into a plate and keyed vector layers per shot;
/// writes the plates and `layers.json` into `out_dir`.
pub fn decompose(
    device: &Device,
    video: &Path,
    out_dir: &Path,
    opts: &LayerOptions,
    mut log: impl FnMut(&str),
) -> Result<(Document, Value), String> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let video = std::fs::canonicalize(video).map_err(|e| format!("{}: {e}", video.display()))?;
    log("probing shots");
    let probed = probe(device, &video, None)?;
    let (w, h, fps, duration) = (
        probed["width"].as_u64().unwrap() as usize,
        probed["height"].as_u64().unwrap() as usize,
        probed["fps"].as_f64().unwrap(),
        probed["duration"].as_f64().unwrap(),
    );
    let cuts: Vec<(f64, f64)> = probed["shots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["start"].as_f64().unwrap(), s["end"].as_f64().unwrap()))
        .collect();
    let mut reference = Reference::open(device, &video, None, None)?;
    // A shot whose background changes (a wipe to a new color) is one plate
    // per background.
    let mut shots: Vec<(f64, f64)> = Vec::new();
    for &(start, end) in &cuts {
        let (f0, f1) = (
            (start * fps).round() as usize,
            ((end * fps).round() as usize).max((start * fps).round() as usize + 1),
        );
        let mut ground = Vec::with_capacity(f1 - f0);
        for f in f0..f1 {
            ground.push(reference.frame(f as f64 / fps)?.border_color());
        }
        let mut from = f0;
        for at in grounds_change(&ground) {
            shots.push((from as f64 / fps, (f0 + at) as f64 / fps));
            from = f0 + at;
        }
        shots.push((from as f64 / fps, end));
    }
    let mut nodes = Map::new();
    let mut report_shots = Vec::new();
    let mut prev: Option<String> = None;
    for (si, &(start, end)) in shots.iter().enumerate() {
        let k = si + 1;
        let f0 = (start * fps).round() as usize;
        let f1 = ((end * fps).round() as usize).max(f0 + 1);
        let n = f1 - f0;
        log(&format!("shot {k}: {start:.2}..{end:.2}s ({n} frames): plate"));
        let samples: Vec<Frame> = (0..opts.plate_samples.min(n))
            .map(|i| {
                reference.frame((f0 as f64 + (i as f64 + 0.5) * n as f64 / opts.plate_samples.min(n) as f64) / fps)
            })
            .collect::<Result<_, _>>()?;
        let plate = quiet_plate(&samples, 5);
        drop(samples);
        let plate_file = format!("s{k}_plate.png");
        plate.write_png(&out_dir.join(&plate_file))?;

        // Pass 1: foreground components frame by frame, linked into tracks
        // where they overlap from one frame to the next (an element is one
        // track over its lifetime), with color samples along the way.
        log(&format!("shot {k}: tracking elements"));
        let seg_t = std::cell::Cell::new([0f64; 3]);
        let segment = |fr: &Frame| -> (Vec<u32>, Vec<([usize; 4], usize)>, Vec<bool>) {
            let t0 = std::time::Instant::now();
            let d = diff_map(fr, &plate);
            let mask: Vec<bool> = d.iter().map(|v| *v > opts.threshold).collect();
            let t1 = std::time::Instant::now();
            // Joined across more than down: letters join into words, lines
            // of type stay apart.
            let joined = dilate_xy(&mask, w, h, opts.join, opts.join.div_ceil(3));
            let t2 = std::time::Instant::now();
            let (labels, comps) = components(&joined, w, h);
            let mut a = seg_t.get();
            a[0] += (t1 - t0).as_secs_f64();
            a[1] += (t2 - t1).as_secs_f64();
            a[2] += t2.elapsed().as_secs_f64();
            seg_t.set(a);
            (labels, comps, mask)
        };
        let mut parent: Vec<usize> = Vec::new();
        fn root(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        let mut base: Vec<usize> = Vec::with_capacity(n);
        let mut raw_fg: Vec<usize> = Vec::new();
        // Flat colors and where they were seen (6 px cells).
        let mut raw_samples: Vec<Vec<([f32; 3], (u16, u16))>> = Vec::new();
        let mut raw_edges: Vec<Vec<([f32; 3], [f32; 3])>> = Vec::new();
        let mut prev_labels: Vec<u32> = Vec::new();
        let mut prev_sizes: Vec<usize> = Vec::new();
        let mut prev_base = 0usize;
        let (mut t_decode, mut t_segment) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for f in f0..f1 {
            let t_a = std::time::Instant::now();
            let fr = reference.frame(f as f64 / fps)?;
            t_decode += t_a.elapsed();
            let t_b = std::time::Instant::now();
            let (labels, comps, mask) = segment(&fr);
            t_segment += t_b.elapsed();
            let b0 = parent.len();
            base.push(b0);
            for (c, (bb, _)) in comps.iter().enumerate() {
                parent.push(b0 + c);
                // Foreground pixels and a spread of strong ones for colors.
                let (bw, bh) = (bb[2] - bb[0] + 1, bb[3] - bb[1] + 1);
                let step = (((bw * bh) as f64 / 400.0).sqrt().ceil() as usize).max(1);
                let mut fg = 0;
                let mut samples = Vec::new();
                let mut edges = Vec::new();
                for y in bb[1]..=bb[3] {
                    for x in bb[0]..=bb[2] {
                        let i = y * w + x;
                        if labels[i] as usize != c + 1 || !mask[i] {
                            continue;
                        }
                        fg += 1;
                        // Colors come from flat interiors (two pixels from any
                        // change): an edge pixel, a thin stroke or a motion
                        // smear is a blend, not a color of its own.
                        let flat = || {
                            x > 1 && y > 1 && x + 2 < w && y + 2 < h && {
                                (y - 2..=y + 2).all(|yy| {
                                    (x - 2..=x + 2).all(|xx| {
                                        let j = yy * w + xx;
                                        (0..3)
                                            .all(|c| (fr.rgba[j * 4 + c] as i32 - fr.rgba[i * 4 + c] as i32).abs() < 12)
                                    })
                                })
                            }
                        };
                        // Flat insides count from a smaller difference (a
                        // faint tint is still a color); edges only when strong.
                        let d = if x % step == 0 && y % step == 0 {
                            diff(&fr, &plate, i)
                        } else {
                            0.0
                        };
                        if d > opts.threshold {
                            let c = [
                                fr.rgba[i * 4] as f32,
                                fr.rgba[i * 4 + 1] as f32,
                                fr.rgba[i * 4 + 2] as f32,
                            ];
                            if flat() {
                                samples.push((c, ((x / 6) as u16, (y / 6) as u16)));
                            } else if d > opts.threshold * 3.0 {
                                let b = [
                                    plate.rgba[i * 4] as f32,
                                    plate.rgba[i * 4 + 1] as f32,
                                    plate.rgba[i * 4 + 2] as f32,
                                ];
                                edges.push((c, b));
                            }
                        }
                    }
                }
                raw_fg.push(fg);
                raw_samples.push(samples);
                raw_edges.push(edges);
            }
            if !prev_labels.is_empty() {
                // The same element from one frame to the next overlaps a lot;
                // one element handing over to another touches only briefly.
                let mut overlap: HashMap<(u32, u32), usize> = HashMap::new();
                for i in 0..w * h {
                    if labels[i] > 0 && prev_labels[i] > 0 {
                        *overlap.entry((labels[i], prev_labels[i])).or_default() += 1;
                    }
                }
                for ((c, pc), n) in overlap {
                    let small = comps[c as usize - 1].1.min(prev_sizes[pc as usize - 1]);
                    if (n as f64) < 0.25 * small as f64 {
                        continue;
                    }
                    let (a, b) = (
                        root(&mut parent, b0 + c as usize - 1),
                        root(&mut parent, prev_base + pc as usize - 1),
                    );
                    if a != b {
                        parent[a] = b;
                    }
                }
            }
            prev_sizes = comps.iter().map(|c| c.1).collect();
            prev_labels = labels;
            prev_base = b0;
        }
        if std::env::var_os("ULTRAFAST_DEBUG_TIME").is_some() {
            eprintln!(
                "pass 1: decode {t_decode:?}, segment {t_segment:?} (diff, dilate, components: {:?})",
                seg_t.get()
            );
        }
        // An element whose content is replaced in place (one chart giving
        // way to the next where they overlap, linking them) is cut where it
        // nearly vanishes between two fuller spans: each part gets its own
        // colors and shapes.
        {
            let total = parent.len();
            let frame_of: Vec<usize> = (0..n)
                .flat_map(|fi| {
                    let end = if fi + 1 < n { base[fi + 1] } else { total };
                    std::iter::repeat_n(fi, end - base[fi])
                })
                .collect();
            let roots: Vec<usize> = (0..total).map(|r| root(&mut parent, r)).collect();
            let mut area: HashMap<usize, Vec<f64>> = HashMap::new();
            for r in 0..total {
                area.entry(roots[r]).or_insert_with(|| vec![0.0; n])[frame_of[r]] += raw_fg[r] as f64;
            }
            const WIN: usize = 30;
            let mut cuts: HashMap<usize, Vec<usize>> = HashMap::new();
            for (t, a) in &area {
                let low = |f: usize| {
                    let before = a[f.saturating_sub(WIN)..f].iter().copied().fold(0.0, f64::max);
                    let after = a[f + 1..(f + 1 + WIN).min(n)].iter().copied().fold(0.0, f64::max);
                    a[f] < 0.35 * before && a[f] < 0.35 * after
                };
                let mut c = Vec::new();
                let mut f = 1;
                while f + 1 < n {
                    if !low(f) {
                        f += 1;
                        continue;
                    }
                    // The lowest point of the dip.
                    let mut best = f;
                    while f + 1 < n && low(f) {
                        if a[f] < a[best] {
                            best = f;
                        }
                        f += 1;
                    }
                    c.push(best);
                }
                if !c.is_empty() {
                    cuts.insert(*t, c);
                }
            }
            let mut first: HashMap<(usize, usize), usize> = HashMap::new();
            for r in 0..total {
                let t = roots[r];
                let Some(c) = cuts.get(&t) else { continue };
                let part = c.iter().filter(|&&f| frame_of[r] >= f).count();
                parent[r] = *first.entry((t, part)).or_insert(r);
            }
            if !cuts.is_empty() {
                log(&format!(
                    "shot {k}: {} elements cut where their content changes ({} cuts)",
                    cuts.len(),
                    cuts.values().map(Vec::len).sum::<usize>()
                ));
            }
        }
        // Tracks: their size at their largest and their colors.
        let total = parent.len();
        let mut peak: HashMap<usize, usize> = HashMap::new();
        let mut track_samples: HashMap<usize, Vec<([f32; 3], (u16, u16))>> = HashMap::new();
        let mut track_edges: HashMap<usize, Vec<([f32; 3], [f32; 3])>> = HashMap::new();
        for r in 0..total {
            let t = root(&mut parent, r);
            let e = peak.entry(t).or_default();
            *e = (*e).max(raw_fg[r]);
            track_samples
                .entry(t)
                .or_default()
                .extend(raw_samples[r].iter().copied());
            track_edges.entry(t).or_default().extend(raw_edges[r].iter().copied());
        }
        let keep: std::collections::HashSet<usize> = peak
            .iter()
            .filter(|(_, v)| **v >= opts.min_area)
            .map(|(k, _)| *k)
            .collect();
        let mut grounds: HashMap<usize, [f32; 3]> = HashMap::new();
        let palettes: HashMap<usize, Vec<Fill>> = keep
            .iter()
            .map(|&t| {
                let flat = &track_samples[&t];
                let edges = &track_edges[&t];
                let step = (flat.len() / 6000).max(1);
                let sampled: Vec<([f32; 3], (u16, u16))> = flat.iter().step_by(step).copied().collect();
                let s: Vec<[f32; 3]> = sampled.iter().map(|x| x.0).collect();
                // What the element is over (the plate at its edges).
                let bg: [f32; 3] = if edges.is_empty() {
                    [0.0; 3]
                } else {
                    std::array::from_fn(|k| {
                        let mut v: Vec<f32> = edges.iter().map(|e| e.1[k]).collect();
                        v.sort_by(f32::total_cmp);
                        v[v.len() / 2]
                    })
                };
                let mut max_colors = opts.max_colors;
                let mut pal = if s.len() >= 30 {
                    palette(&s, max_colors, 10.0)
                } else {
                    Vec::new()
                };
                // A big composite (a panel, a card) whose colors do not fit
                // gets twice as many.
                let off = s
                    .iter()
                    .filter(|c| {
                        pal.iter()
                            .all(|p| (0..3).map(|k| (p[k] - c[k]).powi(2)).sum::<f32>() > 400.0)
                    })
                    .count();
                if peak[&t] > 50_000 && off * 20 > s.len() {
                    max_colors *= 2;
                    pal = palette(&s, max_colors, 10.0);
                }
                // Gradients the palette cut into bands become ramps.
                let (mut pal, fills) = ramps(pal, &sampled);
                if !edges.is_empty() && pal.len() > 1 {
                    pal = drop_fades(pal, &sampled, bg, max_colors);
                }
                // Thin things (hairlines, small type) have no flat inside:
                // edge pixels no palette color over the plate explains add
                // their own colors.
                let unexplained: Vec<[f32; 3]> = edges
                    .iter()
                    .filter(|(c, b)| {
                        // Ramps explain edges in any of their colors.
                        let ramp_colors: Vec<[f32; 3]> = fills
                            .iter()
                            .flat_map(|f| (0..=4).map(move |i| f.along(f.t[0] + (f.t[1] - f.t[0]) * i as f32 / 4.0)))
                            .collect();
                        !pal.iter().chain(&ramp_colors).any(|p| {
                            let d = [p[0] - b[0], p[1] - b[1], p[2] - b[2]];
                            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                            if dd < 1.0 {
                                return false;
                            }
                            let a = (((c[0] - b[0]) * d[0] + (c[1] - b[1]) * d[1] + (c[2] - b[2]) * d[2]) / dd)
                                .clamp(0.0, 1.0);
                            // Codecs keep color at half resolution: an edge's
                            // brightness is a blend, its hue only roughly.
                            let r: [f32; 3] = std::array::from_fn(|k| c[k] - b[k] - a * d[k]);
                            let luma = 0.2126 * r[0] + 0.7152 * r[1] + 0.0722 * r[2];
                            luma.abs() < 20.0 && r.iter().map(|v| v * v).sum::<f32>() < 70.0 * 70.0
                        })
                    })
                    .map(|(c, _)| *c)
                    .collect();
                if unexplained.len() >= 12 && pal.len() < max_colors {
                    let step = (unexplained.len() / 4000).max(1);
                    let u: Vec<[f32; 3]> = unexplained.iter().step_by(step).copied().collect();
                    // Edges are blends: the most saturated end of each
                    // cluster is nearest the true color.
                    let more = palette(&u, max_colors - pal.len(), 14.0);
                    pal.extend(drop_blends(&pal, more, bg));
                }
                // Colors this close are one color with noise, grain or
                // compression on it.
                let mut merged: Vec<[f32; 3]> = Vec::new();
                for c in pal {
                    if !merged
                        .iter()
                        .any(|m| (0..3).map(|k| (m[k] - c[k]).powi(2)).sum::<f32>() < 26.0 * 26.0)
                    {
                        merged.push(c);
                    }
                }
                let mut merged: Vec<Fill> = merged.into_iter().map(Fill::flat).collect();
                merged.extend(fills);
                (t, merged, bg)
            })
            .map(|(t, merged, bg)| {
                grounds.insert(t, bg);
                (t, merged)
            })
            .collect();
        drop(track_samples);
        log(&format!("shot {k}: {} elements; tracing", keep.len()));
        if std::env::var_os("ULTRAFAST_DEBUG_LAYERS").is_some() {
            for (t, pal) in &palettes {
                let p: Vec<String> = pal
                    .iter()
                    .map(|f| {
                        if f.ramp() {
                            format!(
                                "{}..{}{}",
                                hex(f.along(f.t[0])),
                                hex(f.along(f.t[1])),
                                if f.linear { " (linear)" } else { "" }
                            )
                        } else {
                            hex(f.c)
                        }
                    })
                    .collect();
                eprintln!("track {t}: peak {} px, palette {p:?}", peak[t]);
            }
        }

        // Pass 2: every element color's coverage per frame, to find the
        // frame where each is most fully shown (its reference shape).
        let by_track = |parent: &mut Vec<usize>, base: usize, comps: &[([usize; 4], usize)]| {
            let mut out: HashMap<usize, (Vec<u32>, [usize; 4])> = HashMap::new();
            for (c, (bb, _)) in comps.iter().enumerate() {
                let t = root(parent, base + c);
                if !keep.contains(&t) || palettes[&t].is_empty() {
                    continue;
                }
                let e = out.entry(t).or_insert((Vec::new(), *bb));
                e.0.push(c as u32 + 1);
                e.1 = [
                    e.1[0].min(bb[0]),
                    e.1[1].min(bb[1]),
                    e.1[2].max(bb[2]),
                    e.1[3].max(bb[3]),
                ];
            }
            out
        };
        let mut peak_at: HashMap<(usize, usize), (f64, usize)> = HashMap::new();
        let mut weight: HashMap<(usize, usize), f64> = HashMap::new();
        for (fi, f) in (f0..f1).enumerate() {
            let fr = reference.frame(f as f64 / fps)?;
            let (labels, comps, mask) = segment(&fr);
            let elements: Vec<(usize, (Vec<u32>, [usize; 4]))> =
                by_track(&mut parent, base[fi], &comps).into_iter().collect();
            let masses = par_map(elements, |(id, (members, b))| {
                let alphas = matte(
                    &fr,
                    &plate,
                    w,
                    &labels,
                    &mask,
                    &members,
                    b,
                    &palettes[&id],
                    opts.threshold,
                );
                let m: Vec<Option<f64>> = alphas.into_iter().map(|a| Cover::new(a, b).map(|c| c.mass)).collect();
                (id, m)
            });
            for (id, m) in masses {
                for (j, mass) in m.into_iter().enumerate() {
                    if let Some(mass) = mass {
                        *weight.entry((id, j)).or_default() += mass;
                        let e = peak_at.entry((id, j)).or_insert((0.0, fi));
                        if mass > e.0 {
                            *e = (mass, fi);
                        }
                    }
                }
            }
        }

        // Pass 3: the reference shapes, traced once.
        log(&format!("shot {k}: reference shapes"));
        let mut tracks: HashMap<(usize, usize), Track> = HashMap::new();
        let mut peak_frames: Vec<usize> = peak_at.values().map(|v| v.1).collect();
        peak_frames.sort_unstable();
        peak_frames.dedup();
        for &fi in &peak_frames {
            let fr = reference.frame((f0 + fi) as f64 / fps)?;
            let (labels, comps, mask) = segment(&fr);
            for (id, (members, b)) in by_track(&mut parent, base[fi], &comps) {
                let pal = &palettes[&id];
                let mut alphas = matte(&fr, &plate, w, &labels, &mask, &members, b, pal, opts.threshold);
                fill_under(&mut alphas, b[2] - b[0] + 1, b[3] - b[1] + 1, |j| {
                    in_front(&weight, id, j, pal, grounds.get(&id))
                });
                for (j, a) in alphas.into_iter().enumerate() {
                    if peak_at.get(&(id, j)).map(|p| p.1) != Some(fi) {
                        continue;
                    }
                    let Some(c) = Cover::new(a, b) else { continue };
                    let Some(d) = c.trace() else { continue };
                    tracks.insert(
                        (id, j),
                        Track {
                            color: pal[j].mean(),
                            ground: !pal[j].ramp()
                                && grounds.get(&id).is_some_and(|g| {
                                    (0..3).map(|c| (g[c] - pal[j].c[c]).powi(2)).sum::<f32>() < 14.0 * 14.0
                                }),
                            fill: pal[j].ramp().then_some(pal[j]),
                            shapes: vec![d],
                            frames: vec![None; n],
                            weight: weight[&(id, j)],
                            reference: c,
                            last: None,
                            keys: Vec::new(),
                            covers: HashMap::new(),
                        },
                    );
                }
            }
        }

        // Pass 4: every frame as the reference moved (and clipped where a
        // mask hides part of it), or as its own shape when it is not.
        log(&format!("shot {k}: fitting motion"));
        let (mut moved, mut own) = (0usize, 0usize);
        // Each element's box per frame.
        let mut elem_boxes: HashMap<usize, Vec<Option<[usize; 4]>>> = HashMap::new();
        for (fi, f) in (f0..f1).enumerate() {
            let fr = reference.frame(f as f64 / fps)?;
            let (labels, comps, mask) = segment(&fr);
            // Each element with its tracks, on every core.
            let work: Vec<(usize, Vec<u32>, [usize; 4], Vec<(usize, Track)>)> = by_track(&mut parent, base[fi], &comps)
                .into_iter()
                .map(|(id, (members, b))| {
                    let ts = (0..palettes[&id].len())
                        .filter_map(|j| tracks.remove(&(id, j)).map(|t| (j, t)))
                        .collect();
                    (id, members, b, ts)
                })
                .collect();
            let done = par_map(work, |(id, members, b, mut ts)| {
                let pal = &palettes[&id];
                let mut alphas = matte(&fr, &plate, w, &labels, &mask, &members, b, pal, opts.threshold);
                fill_under(&mut alphas, b[2] - b[0] + 1, b[3] - b[1] + 1, |j| {
                    in_front(&weight, id, j, pal, grounds.get(&id))
                });
                let mut counts = (0, 0);
                for (j, a) in alphas.into_iter().enumerate() {
                    if let Some((_, t)) = ts.iter_mut().find(|(k, _)| *k == j) {
                        let (m, o) = fit_frame(t, Cover::new(a, b), fi);
                        counts = (counts.0 + m, counts.1 + o);
                    }
                }
                (id, ts, counts, b)
            });
            for (id, ts, (m, o), b) in done {
                moved += m;
                own += o;
                for (j, t) in ts {
                    tracks.insert((id, j), t);
                }
                elem_boxes.entry(id).or_insert_with(|| vec![None; n])[fi] = Some(b);
            }
        }
        log(&format!(
            "shot {k}: {moved} frames as motion, {own} as their own shapes"
        ));
        // Type: lines OCR reads whose elements move as one outline become
        // text nodes carrying that motion.
        #[allow(unused_mut)]
        let mut lines: Vec<TextLine> = Vec::new();
        #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
        if opts.text {
            log(&format!("shot {k}: reading type"));
            lines = read_type(
                device,
                &video,
                out_dir,
                &plate_file,
                &plate,
                (w, h, fps, f0),
                &mut tracks,
                &mut log,
            )?;
            log(&format!("shot {k}: {} lines as text", lines.len()));
        }

        // Elements the palette cannot paint (photos, illustrations, footage):
        // judged at their dominant color's fullest frame by how far the
        // painted colors are from the frame.
        let mut dominant: HashMap<usize, (usize, f64)> = HashMap::new();
        for ((id, j), t) in &tracks {
            let e = dominant.entry(*id).or_insert((*j, f64::MIN));
            if t.weight > e.1 {
                *e = (*j, t.weight);
            }
        }
        // Each element at its largest (whole, away from the frame's edges
        // when it can be).
        let mut at_peak: HashMap<usize, Vec<usize>> = HashMap::new();
        for id in dominant.keys() {
            let Some(boxes) = elem_boxes.get(id) else { continue };
            // Holding still (the same box the frames around), so not
            // caught mid-move between pixels.
            let score = |i: usize, b: &[usize; 4]| {
                let inside = b[0] > 0 && b[1] > 0 && b[2] + 1 < w && b[3] + 1 < h;
                let still = i > 0 && i + 1 < n && boxes[i - 1] == Some(*b) && boxes[i + 1] == Some(*b);
                ((b[2] - b[0] + 1) * (b[3] - b[1] + 1)) as f64
                    * if inside { 1.0 } else { 0.5 }
                    * if still { 1.0 } else { 0.7 }
            };
            if let Some((fi, _)) = boxes
                .iter()
                .enumerate()
                .filter_map(|(i, b)| b.map(|b| (i, score(i, &b))))
                .max_by(|a, b| a.1.total_cmp(&b.1))
            {
                at_peak.entry(fi).or_default().push(*id);
            }
        }
        let mut rasters: HashMap<usize, Raster> = HashMap::new();
        let mut candidates = Vec::new();
        for (pf, ids) in at_peak {
            let fr = reference.frame((f0 + pf) as f64 / fps)?;
            let (labels, comps, mask) = segment(&fr);
            let elements = by_track(&mut parent, base[pf], &comps);
            for id in ids {
                let Some((members, b)) = elements.get(&id) else {
                    continue;
                };
                let pal = &palettes[&id];
                let alphas = matte(&fr, &plate, w, &labels, &mask, members, *b, pal, opts.threshold);
                let (bw, bh) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
                let (mut err, mut n) = (0f64, 0usize);
                for y in 0..bh {
                    for x in 0..bw {
                        let i = (b[1] + y) * w + b[0] + x;
                        if !mask[i] || !members.contains(&labels[i]) {
                            continue;
                        }
                        let q = y * bw + x;
                        let a: f32 = alphas.iter().map(|a| a[q]).sum::<f32>().min(1.0);
                        for c in 0..3 {
                            let mut v = plate.rgba[i * 4 + c] as f32 * (1.0 - a);
                            for (j, f) in pal.iter().enumerate() {
                                v += alphas[j][q] * f.at((b[0] + x) as f32, (b[1] + y) as f32)[c];
                            }
                            err += ((fr.rgba[i * 4 + c] as f32 - v) as f64).powi(2);
                        }
                        n += 1;
                    }
                }
                let rms = (err / (3 * n.max(1)) as f64).sqrt();
                if n < 400 || rms <= 15.0 {
                    continue;
                }
                let (j, _) = dominant[&id];
                // The picture over the plate at that frame, unblended.
                let sprite_img = {
                    let mut img = Frame {
                        width: bw as u32,
                        height: bh as u32,
                        rgba: vec![0; bw * bh * 4],
                    };
                    for y in 0..bh {
                        for x in 0..bw {
                            let i = (b[1] + y) * w + b[0] + x;
                            let inside = mask[i] && members.contains(&labels[i]);
                            let a = if inside {
                                1.0
                            } else {
                                (diff(&fr, &plate, i) / (3.0 * opts.threshold)).min(1.0)
                            };
                            if a <= 0.02 || !(inside || members.contains(&labels[i])) {
                                continue;
                            }
                            let o = (y * bw + x) * 4;
                            for c in 0..3 {
                                let v = (fr.rgba[i * 4 + c] as f32 - (1.0 - a) * plate.rgba[i * 4 + c] as f32) / a;
                                img.rgba[o + c] = v.round().clamp(0.0, 255.0) as u8;
                            }
                            img.rgba[o + 3] = (a * 255.0).round() as u8;
                        }
                    }
                    img
                };
                candidates.push((id, j, *b, sprite_img, rms));
            }
        }
        // A still picture that moves is an image moved frame by frame (if
        // it is that picture moved on most frames it shows); content that
        // keeps changing is a window onto the source.
        for (id, j, b, img, rms) in candidates {
            let boxes = &elem_boxes[&id];
            let mut places: Vec<Option<[f64; 2]>> = vec![None; n];
            let (mut shown, mut held) = (0, 0);
            for i in 0..n {
                let Some(bb) = boxes[i] else { continue };
                shown += 1;
                let fr = reference.frame((f0 + i) as f64 / fps)?;
                if let Some(d) = place_sprite(&img, b, bb, &fr, w, h) {
                    places[i] = Some(d);
                    held += 1;
                }
            }
            let footage = held * 10 < shown * 8;
            let sprite = if footage {
                None
            } else {
                let file = format!("s{k}_r{id}.png");
                img.write_png(&out_dir.join(&file))?;
                Some(file)
            };
            log(&format!(
                "shot {k}: an element the palette paints {rms:.0} off becomes {}",
                if footage {
                    "a window onto the source"
                } else {
                    "an image"
                }
            ));
            rasters.insert(
                id,
                Raster {
                    sprite,
                    at: b,
                    dominant: j,
                    places,
                },
            );
        }

        // Assemble: the plate, then each element's colors, largest behind.
        let plate_id = format!("s{k}_plate");
        nodes.insert(
            plate_id.clone(),
            json!({ "op": "image", "params": { "src": plate_file, "fit": "fill" } }),
        );
        let mut order: Vec<(&(usize, usize), &Track)> = tracks.iter().filter(|(_, t)| !t.shapes.is_empty()).collect();
        // Background-colored layers first, then largest behind.
        order.sort_by(|a, b| b.1.ground.cmp(&a.1.ground).then(b.1.weight.total_cmp(&a.1.weight)));
        // Elements numbered in order of appearance.
        let mut firsts: Vec<(usize, usize)> = Vec::new();
        for ((eid, _), t) in &order {
            let f = t.frames.iter().position(Option::is_some).unwrap_or(0);
            match firsts.iter_mut().find(|(e, _)| e == eid) {
                Some(x) => x.1 = x.1.min(f),
                None => firsts.push((*eid, f)),
            }
        }
        firsts.sort_by_key(|x| x.1);
        let number = |eid: usize| firsts.iter().position(|x| x.0 == eid).unwrap_or(0) + 1;
        let mut under = plate_id;
        let mut layers = Vec::new();
        let (fw, fh) = (w as f64, h as f64);
        for (li, ((eid, ci), t)) in order.iter().enumerate() {
            let id = format!("s{k}_e{}c{}", number(*eid), ci + 1);
            let secs = |i: usize| (f0 + i) as f64 / fps;
            let Some(first) = t.frames.iter().position(Option::is_some) else {
                continue;
            };
            if let Some(r) = rasters.get(eid) {
                if *ci != r.dominant {
                    continue;
                }
                let id = format!("s{k}_e{}", number(*eid));
                // Present while any of its colors is.
                let all: Vec<&Track> = tracks.iter().filter(|((e, _), _)| e == eid).map(|(_, t)| t).collect();
                let present = |i: usize| all.iter().any(|t| t.frames[i].is_some());
                let Some(a) = (0..n).find(|&i| present(i)) else {
                    continue;
                };
                let z = (0..n).rfind(|&i| present(i)).unwrap_or(a);
                let window = json!({ "in": secs(a), "out": secs(z + 1) });
                let r_at = [
                    r.at[0] as f64,
                    r.at[1] as f64,
                    (r.at[2] - r.at[0] + 1) as f64,
                    (r.at[3] - r.at[1] + 1) as f64,
                ];
                let fg = match &r.sprite {
                    Some(file) => {
                        // The image where it was found; frames where it was
                        // not take the nearest, shown only while present.
                        let found: Vec<usize> = (0..n).filter(|&i| r.places[i].is_some()).collect();
                        let (mut rect, mut alpha, crops) = (Vec::new(), Vec::new(), Vec::<Value>::new());
                        let (mut lr, mut la) = (None::<[f64; 2]>, -1.0f32);
                        let clipped = false;
                        for i in a..=z {
                            let near = found.iter().min_by_key(|&&j| (j as i64 - i as i64).abs()).copied();
                            let Some(d) = near.and_then(|j| r.places[j]) else {
                                continue;
                            };
                            let op: f32 = if present(i) { 1.0 } else { 0.0 };
                            if lr != Some(d) {
                                let moving = r.places[i].is_some()
                                    && r.places.get(i + 1).copied().flatten().is_some_and(|q| q != d);
                                rect.push(json!({ "t": secs(i), "v": [round1(r_at[0] + d[0]), round1(r_at[1] + d[1]), r_at[2], r_at[3]],
                                                  "ease": if moving { "linear" } else { "hold" } }));
                                lr = Some(d);
                            }
                            if (op - la).abs() > 0.01 {
                                alpha.push(json!({ "t": secs(i), "v": op, "ease": "hold" }));
                                la = op;
                            }
                        }
                        let mut params = json!({ "src": file, "rect": if rect.len() > 1 { json!({ "keys": rect }) } else { rect.first().map_or(json!(r_at), |k| k["v"].clone()) } });
                        if alpha.len() > 1 || la < 0.999 {
                            params["opacity"] = json!({ "keys": alpha });
                        }
                        nodes.insert(id.clone(), json!({ "op": "image", "params": params, "timing": window }));
                        if clipped {
                            let m = format!("{id}_mask");
                            nodes.insert(m.clone(), json!({ "op": "crop", "params": { "rect": { "keys": crops }, "softness": 0.0 }, "inputs": { "source": id }, "timing": window }));
                            m
                        } else {
                            id.clone()
                        }
                    }
                    None => {
                        // The source itself, through the element's box.
                        let boxes = &elem_boxes[eid];
                        let mut crops = Vec::new();
                        let mut last = None;
                        for i in a..=z {
                            let bb = boxes[i]
                                .map(|b| {
                                    [
                                        b[0] as f64,
                                        b[1] as f64,
                                        (b[2] - b[0] + 1) as f64,
                                        (b[3] - b[1] + 1) as f64,
                                    ]
                                })
                                .unwrap_or([0.0; 4]);
                            if last != Some(bb) {
                                crops.push(json!({ "t": secs(i), "v": bb, "ease": "hold" }));
                                last = Some(bb);
                            }
                        }
                        nodes.insert(id.clone(), json!({ "op": "video", "params": { "src": video.to_string_lossy(), "rect": [0, 0, w, h], "start": 0 }, "timing": window }));
                        let m = format!("{id}_window");
                        nodes.insert(m.clone(), json!({ "op": "crop", "params": { "rect": { "keys": crops }, "softness": 0.0 }, "inputs": { "source": id }, "timing": window }));
                        m
                    }
                };
                let over = format!("s{k}_l{}", li + 1);
                nodes.insert(
                    over.clone(),
                    json!({ "op": "over", "params": { "space": "srgb" }, "inputs": { "fg": fg, "bg": under } }),
                );
                under = over;
                layers.push(json!({ "node": id, "kind": if r.sprite.is_some() { "image" } else { "footage" } }));
                continue;
            }
            let last = t.frames.iter().rposition(Option::is_some).unwrap_or(first);
            // Only the shapes some frame shows (read type takes frames away).
            let mut kept: Vec<usize> = t.frames.iter().flatten().map(|p| p.shape).collect();
            kept.sort_unstable();
            kept.dedup();
            let frames: Vec<Option<Placement>> = t
                .frames
                .iter()
                .map(|p| {
                    p.map(|p| Placement {
                        shape: kept.binary_search(&p.shape).unwrap(),
                        ..p
                    })
                })
                .collect();
            let shapes: Vec<&String> = kept.iter().map(|&si| &t.shapes[si]).collect();
            // Keys where something changes; absent frames are transparent.
            let (mut morph, mut alpha, mut pos, mut rect) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let (mut ls, mut la, mut lp, mut lr) = (usize::MAX, -1.0f32, (f64::NAN, 0.0), None::<[f64; 4]>);
            let (mut sc, mut lsc) = (Vec::new(), f64::NAN);
            let mut clipped = false;
            for i in first..=last {
                let lsc_held = if lsc.is_nan() { 1.0 } else { lsc };
                let p = frames[i].unwrap_or(Placement {
                    shape: if ls == usize::MAX { 0 } else { ls },
                    own: false,
                    opacity: 0.0,
                    dx: if lp.0.is_nan() { 0.0 } else { lp.0 },
                    dy: lp.1,
                    clip: lr,
                    scale: lsc_held,
                });
                if p.shape != ls {
                    morph.push(json!({ "t": secs(i), "v": [p.shape as f64], "ease": "hold" }));
                    ls = p.shape;
                }
                if (p.opacity - la).abs() > 0.01 {
                    alpha.push(json!({ "t": secs(i), "v": [t.color[0] / 255.0, t.color[1] / 255.0, t.color[2] / 255.0, p.opacity], "ease": "hold" }));
                    la = p.opacity;
                }
                // Moving on into the next frame (the same outline, placed
                // anew): interpolate, so motion blur sees the motion.
                let next = frames
                    .get(i + 1)
                    .copied()
                    .flatten()
                    .filter(|q| q.shape == p.shape && frames[i].is_some());
                let glide = |a: f64, b: f64| if (a - b).abs() > 0.01 { "linear" } else { "hold" };
                if lp.0.is_nan() || (p.dx - lp.0).abs() > 0.01 || (p.dy - lp.1).abs() > 0.01 {
                    let ease = next.map_or("hold", |q| {
                        if glide(q.dx, p.dx) == "linear" || glide(q.dy, p.dy) == "linear" {
                            "linear"
                        } else {
                            "hold"
                        }
                    });
                    pos.push(json!({ "t": secs(i), "v": [round1(p.dx), round1(p.dy)], "ease": ease }));
                    lp = (p.dx, p.dy);
                }
                if lsc.is_nan() || (p.scale - lsc).abs() > 0.001 {
                    let ease = next.map_or("hold", |q| glide(q.scale * 100.0, p.scale * 100.0));
                    sc.push(json!({ "t": secs(i), "v": (p.scale * 1000.0).round() / 1000.0, "ease": ease }));
                    lsc = p.scale;
                }
                if p.clip != lr || i == first {
                    let r = p.clip.unwrap_or([0.0, 0.0, fw, fh]);
                    rect.push(json!({ "t": secs(i), "v": [round1(r[0]), round1(r[1]), round1(r[2] - r[0]), round1(r[3] - r[1])], "ease": "hold" }));
                    clipped |= p.clip.is_some();
                    lr = p.clip;
                }
            }
            let mut params = json!({ "d": shapes[0], "fill_rule": "evenodd" });
            if shapes.len() > 1 {
                params["to"] = json!(shapes[1..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | "));
                params["morph"] = json!({ "keys": morph });
            }
            // A ramp paints a gradient through the outline (white, carrying
            // the opacity), moving with it.
            if t.fill.is_some() {
                for k in alpha.iter_mut() {
                    k["v"] = json!([1.0, 1.0, 1.0, k["v"][3]]);
                }
            }
            params["color"] = if alpha.len() == 1 && la >= 0.999 {
                json!(if t.fill.is_some() {
                    "#ffffff".to_string()
                } else {
                    hex(t.color)
                })
            } else {
                json!({ "keys": alpha })
            };
            // Scaled frames: scale about the reference's center (the anchor),
            // which the position carries.
            let scaled = sc.iter().any(|k| k["v"].as_f64() != Some(1.0));
            if scaled && let Some(v) = t.reference.visible() {
                let c = [(v[0] + v[2]) / 2.0, (v[1] + v[3]) / 2.0];
                params["anchor"] = json!([round1(c[0]), round1(c[1])]);
                params["scale"] = json!({ "keys": sc });
                let keys: Vec<Value> = pos
                    .iter()
                    .map(|k| {
                        let d = [k["v"][0].as_f64().unwrap_or(0.0), k["v"][1].as_f64().unwrap_or(0.0)];
                        json!({ "t": k["t"], "v": [round1(c[0] + d[0]), round1(c[1] + d[1])], "ease": k["ease"].clone() })
                    })
                    .collect();
                params["position"] = json!({ "keys": keys });
            } else if pos.len() > 1 || pos.first().is_some_and(|k| k["v"] != json!([0.0, 0.0])) {
                params["position"] = json!({ "keys": pos });
            }
            let window = json!({ "in": secs(first), "out": secs(last + 1) });
            nodes.insert(id.clone(), json!({ "op": "path", "params": params, "timing": window }));
            let mut fg = id.clone();
            if let Some(f) = t.fill {
                let end = |t: f32| [f.o[0] + f.n[0] * t, f.o[1] + f.n[1] * t];
                let color = |t: f32| hex(f.along(t));
                let (a, b) = (end(f.t[0]), end(f.t[1]));
                let moved = |p: [f32; 2]| -> Value {
                    if pos.len() > 1 {
                        json!({ "keys": pos.iter().map(|k| {
                            let d = [k["v"][0].as_f64().unwrap_or(0.0), k["v"][1].as_f64().unwrap_or(0.0)];
                            json!({ "t": k["t"], "v": [round1(p[0] as f64 + d[0]), round1(p[1] as f64 + d[1])], "ease": k["ease"].clone() })
                        }).collect::<Vec<_>>() })
                    } else {
                        let d = pos.first().map_or([0.0, 0.0], |k| {
                            [k["v"][0].as_f64().unwrap_or(0.0), k["v"][1].as_f64().unwrap_or(0.0)]
                        });
                        json!([round1(p[0] as f64 + d[0]), round1(p[1] as f64 + d[1])])
                    }
                };
                let grad = format!("{id}_fill");
                nodes.insert(
                    grad.clone(),
                    json!({ "op": "gradient", "params": { "kind": "linear", "space": if f.linear { "linear" } else { "srgb" },
                            "a": color(f.t[0]), "b": color(f.t[1]), "start": moved(a), "end": moved(b) },
                            "timing": window }),
                );
                fg = format!("{id}_paint");
                nodes.insert(
                    fg.clone(),
                    json!({ "op": "mask", "inputs": { "source": grad, "matte": id }, "timing": window }),
                );
            }
            if clipped {
                let src = fg.clone();
                fg = format!("{id}_mask");
                nodes.insert(
                    fg.clone(),
                    json!({ "op": "crop", "params": { "rect": { "keys": rect }, "softness": 0.0 },
                            "inputs": { "source": src }, "timing": window }),
                );
            }
            let over = format!("s{k}_l{}", li + 1);
            nodes.insert(
                over.clone(),
                json!({ "op": "over", "params": { "space": "srgb" }, "inputs": { "fg": fg, "bg": under } }),
            );
            under = over;
            layers.push(json!({ "node": id, "color": hex(t.color), "shapes": shapes.len(), "clipped": clipped }));
        }
        for (li, line) in lines.iter().enumerate() {
            let id = format!("s{k}_text{}", li + 1);
            let secs = |i: usize| (f0 + i) as f64 / fps;
            let Some(first) = line.frames.iter().position(Option::is_some) else {
                continue;
            };
            let last = line.frames.iter().rposition(Option::is_some).unwrap_or(first);
            let mut params = line.params.clone();
            let base = [
                params["position"][0].as_f64().unwrap_or(0.0),
                params["position"][1].as_f64().unwrap_or(0.0),
            ];
            let (mut alpha, mut pos, mut rect) = (Vec::new(), Vec::new(), Vec::new());
            let (mut la, mut lp, mut lr) = (-1.0f32, (f64::NAN, 0.0), None::<[f64; 4]>);
            let mut clipped = false;
            for i in first..=last {
                let p = line.frames[i].unwrap_or(Placement {
                    shape: 0,
                    own: false,
                    opacity: 0.0,
                    dx: if lp.0.is_nan() { 0.0 } else { lp.0 },
                    dy: lp.1,
                    clip: lr,
                    scale: 1.0,
                });
                if (p.opacity - la).abs() > 0.01 {
                    alpha.push(json!({ "t": secs(i), "v": [line.color[0] / 255.0, line.color[1] / 255.0, line.color[2] / 255.0, p.opacity], "ease": "hold" }));
                    la = p.opacity;
                }
                if lp.0.is_nan() || (p.dx - lp.0).abs() > 0.01 || (p.dy - lp.1).abs() > 0.01 {
                    let moving = line.frames[i].is_some()
                        && line
                            .frames
                            .get(i + 1)
                            .copied()
                            .flatten()
                            .is_some_and(|q| (q.dx - p.dx).abs() > 0.01 || (q.dy - p.dy).abs() > 0.01);
                    pos.push(
                        json!({ "t": secs(i), "v": [round1(base[0] + p.dx), round1(base[1] + p.dy)],
                                     "ease": if moving { "linear" } else { "hold" } }),
                    );
                    lp = (p.dx, p.dy);
                }
                if p.clip != lr || i == first {
                    let r = p.clip.unwrap_or([0.0, 0.0, fw, fh]);
                    rect.push(json!({ "t": secs(i), "v": [round1(r[0]), round1(r[1]), round1(r[2] - r[0]), round1(r[3] - r[1])], "ease": "hold" }));
                    clipped |= p.clip.is_some();
                    lr = p.clip;
                }
            }
            if alpha.len() > 1 || la < 0.999 {
                params["color"] = json!({ "keys": alpha });
            }
            if pos.len() > 1 {
                params["position"] = json!({ "keys": pos });
            }
            let window = json!({ "in": secs(first), "out": secs(last + 1) });
            nodes.insert(id.clone(), json!({ "op": "text", "params": params, "timing": window }));
            let mut fg = id.clone();
            if clipped {
                fg = format!("{id}_mask");
                nodes.insert(
                    fg.clone(),
                    json!({ "op": "crop", "params": { "rect": { "keys": rect }, "softness": 0.0 },
                            "inputs": { "source": id }, "timing": window }),
                );
            }
            let over = format!("s{k}_t{}", li + 1);
            nodes.insert(
                over.clone(),
                json!({ "op": "over", "params": { "space": "srgb" }, "inputs": { "fg": fg, "bg": under } }),
            );
            under = over;
            layers
                .push(json!({ "node": id, "text": line.params["text"], "font": line.params["font"], "fit": line.fit }));
        }
        let top = match prev {
            Some(p) => {
                let id = format!("s{k}");
                nodes.insert(
                    id.clone(),
                    json!({ "op": "over", "inputs": { "fg": under, "bg": p }, "timing": { "in": start, "out": end } }),
                );
                id
            }
            None => under,
        };
        prev = Some(top);
        report_shots.push(json!({ "shot": k, "start": start, "end": end, "plate": plate_file,
                                  "elements": keep.len(), "layers": layers }));
    }
    let mut doc = Document::from_json(
        &json!({
            "version": 1,
            "format": { "width": w, "height": h, "fps": fps, "duration": duration },
            "output": prev.ok_or("no shots")?,
            "nodes": nodes,
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    // The source's motion blur: the shutter that renders sampled frames
    // closest to it (none when sharp motion matches best).
    log("judging motion blur");
    let n = (duration * fps).round() as u64;
    let probe_times: Vec<f64> = (0..16)
        .map(|i| ((i as u64 * n) / 16) as f64 / fps + 0.5 / fps)
        .collect();
    let judge = |d: &Document| -> Result<f64, String> {
        let r = compare(
            device,
            d,
            out_dir,
            &video,
            &crate::CompareOptions {
                times: probe_times.clone(),
                offset: 0.0,
                worst: 0,
                out_dir: None,
            },
        )?;
        Ok(r["mean_psnr"].as_f64().unwrap_or(0.0))
    };
    let mut best = (judge(&doc)?, None);
    for shutter in [0.5, 1.0] {
        let mut d = doc.clone();
        d.format.motion_blur = Some(ultrafast_graph::MotionBlur {
            samples: 8,
            shutter,
            max_samples: None,
        });
        let p = judge(&d)?;
        if p > best.0 + 0.3 {
            best = (p, Some(shutter));
        }
    }
    if let Some(shutter) = best.1 {
        log(&format!("motion blur: a {:.0} degree shutter", shutter * 360.0));
        doc.format.motion_blur = Some(ultrafast_graph::MotionBlur {
            samples: 8,
            shutter,
            max_samples: None,
        });
    }
    let out = out_dir.join("layers.json");
    std::fs::write(&out, doc.to_json()).map_err(|e| e.to_string())?;
    log("comparing with the source");
    let times: Vec<f64> = (0..24)
        .map(|i| ((i as u64 * n) / 24) as f64 / fps + 0.5 / fps)
        .collect();
    let score = compare(
        device,
        &doc,
        out_dir,
        &video,
        &crate::CompareOptions {
            times,
            offset: 0.0,
            worst: 3,
            out_dir: Some(out_dir.join("diffs")),
        },
    )?;
    let report = json!({
        "video": video, "document": out, "shots": report_shots,
        "mean_psnr": score["mean_psnr"], "min_psnr": score["min_psnr"], "worst": score["worst"],
    });
    Ok((doc, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traces_a_square_with_subpixel_edges() {
        // A 10x10 square at (5.5..15.5) with half-covered edge pixels.
        let (w, h) = (24, 24);
        let mut a = vec![0f32; w * h];
        for y in 6..16 {
            for x in 6..16 {
                a[y * w + x] = 1.0;
            }
        }
        for i in 6..16 {
            a[5 * w + i] = 0.5;
            a[16 * w + i] = 0.5;
            a[i * w + 5] = 0.5;
            a[i * w + 16] = 0.5;
        }
        let loops: Vec<_> = outlines(&a, w, h, 0.5, 0.0, 0.0)
            .into_iter()
            .map(|l| simplify(&l, 0.3))
            .collect();
        assert_eq!(loops.len(), 1);
        let ar = area(&loops[0]);
        assert!((ar - 121.0).abs() < 8.0, "area {ar}");
    }

    #[test]
    fn palette_finds_the_colors() {
        let mut s = vec![[250.0, 20.0, 30.0]; 50];
        s.extend(vec![[20.0, 40.0, 240.0]; 50]);
        let p = palette(&s, 4, 10.0);
        assert_eq!(p.len(), 2);
    }
}
