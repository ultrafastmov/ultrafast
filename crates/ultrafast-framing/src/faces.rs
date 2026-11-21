//! Face tracks from the footage itself, for `clip_camera` documents that
//! come without a face worker's `faces`.
//!
//! The caller decodes the clip into grey frames at a steady rate (about
//! ten a second); this finds faces (SeetaFace's frontal detector, through
//! `rustface`), links them into tracks within each shot, estimates who is
//! talking from how much the mouth region changes against the rest of the
//! face, finds the cuts, and writes the payload [`crate::Faces::from_json`]
//! reads: `{sourceWidth, sourceHeight, fps, frames: [{frame, faces: [{id,
//! x, y, w, h, talk}]}], shots: [{startFrame, endFrame}]}` with boxes
//! normalized to the frame.

use serde_json::{Value, json};

/// The detector's model (BSD-2-Clause, see `models/LICENSE-SeetaFace`).
const MODEL: &[u8] = include_bytes!("../models/seeta_fd_frontal_v1.0.bin");

/// Bumped when the analysis changes, so cached results are redone.
pub const VERSION: u32 = 1;

/// A greyscale frame, row-major, one byte per pixel.
#[derive(Debug, Clone)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub px: Vec<u8>,
}

impl Gray {
    /// From sRGB RGBA8 pixels (Rec. 601 luma).
    pub fn from_rgba(width: usize, height: usize, rgba: &[u8]) -> Self {
        let px = rgba
            .chunks_exact(4)
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
            .collect();
        Self { width, height, px }
    }

    /// Mean absolute difference from `other` over a box in normalized
    /// coordinates (x0, y0, x1, y1).
    fn diff(&self, other: &Gray, b: [f64; 4]) -> Option<f64> {
        let (w, h) = (self.width as f64, self.height as f64);
        let x0 = (b[0] * w).floor().max(0.0) as usize;
        let y0 = (b[1] * h).floor().max(0.0) as usize;
        let x1 = ((b[2] * w).ceil() as usize).min(self.width);
        let y1 = ((b[3] * h).ceil() as usize).min(self.height);
        if x1 <= x0 + 1 || y1 <= y0 + 1 {
            return None;
        }
        let mut sum = 0u64;
        for y in y0..y1 {
            let row = y * self.width;
            for x in x0..x1 {
                sum += (self.px[row + x] as i32 - other.px[row + x] as i32).unsigned_abs() as u64;
            }
        }
        Some(sum as f64 / ((x1 - x0) * (y1 - y0)) as f64)
    }

    fn histogram(&self) -> [f32; 32] {
        let mut h = [0f32; 32];
        for &p in &self.px {
            h[(p >> 3) as usize] += 1.0;
        }
        let n = self.px.len().max(1) as f32;
        h.map(|v| v / n)
    }
}

/// A face found in one frame: normalized top-left, size and the
/// detector's score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub score: f64,
}

/// Finds faces in grey frames.
pub struct Detector {
    inner: Box<dyn rustface::Detector>,
}

impl Detector {
    pub fn new() -> Self {
        let model = rustface::read_model(std::io::Cursor::new(MODEL)).expect("the bundled face model reads");
        let mut inner = rustface::create_detector_with_model(model);
        inner.set_min_face_size(20);
        inner.set_score_thresh(2.0);
        inner.set_pyramid_scale_factor(0.8);
        inner.set_slide_window_step(4, 4);
        Self { inner }
    }

    pub fn detect(&mut self, img: &Gray) -> Vec<Detection> {
        let (w, h) = (img.width as f64, img.height as f64);
        self.inner
            .detect(&rustface::ImageData::new(&img.px, img.width as u32, img.height as u32))
            .into_iter()
            .map(|f| {
                let b = f.bbox();
                Detection {
                    x: b.x() as f64 / w,
                    y: b.y() as f64 / h,
                    w: b.width() as f64 / w,
                    h: b.height() as f64 / h,
                    score: f.score(),
                }
            })
            .collect()
    }
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

/// One decoded frame of the clip: source seconds and pixels.
pub struct Sample {
    pub t: f64,
    pub frame: Gray,
}

#[derive(Debug, Clone, Copy)]
struct Tracked {
    id: i64,
    b: Detection,
}

/// Faces, tracks, talk and shots for frames decoded at a steady rate from a
/// source of `fps` frames a second and `width` x `height` pixels: the face
/// worker's payload.
pub fn analyze(samples: &[Sample], fps: f64, width: f64, height: f64) -> Value {
    let n = samples.len();
    if n == 0 {
        return json!({ "sourceWidth": width, "sourceHeight": height, "fps": fps, "frames": [], "shots": [] });
    }
    let step = if n > 1 {
        ((samples[n - 1].t - samples[0].t) / (n - 1) as f64).max(1e-3)
    } else {
        0.1
    };

    // Cuts: most of the histogram moves between two samples.
    let hists: Vec<[f32; 32]> = samples.iter().map(|s| s.frame.histogram()).collect();
    let mut shot_of = vec![0usize; n];
    let mut cuts = vec![0usize];
    for i in 1..n {
        let d: f32 = hists[i]
            .iter()
            .zip(&hists[i - 1])
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / 2.0;
        let since = i - *cuts.last().unwrap();
        if d > 0.45 && since as f64 * step > 0.25 {
            cuts.push(i);
        }
        shot_of[i] = cuts.len() - 1;
    }

    // Detections on every other sample (and the first of each shot), on
    // all cores.
    let every = if step < 0.15 { 2 } else { 1 };
    let detect_at: Vec<usize> = (0..n)
        .filter(|&i| i % every == 0 || i + 1 == n || cuts.contains(&i))
        .collect();
    let threads = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let mut found: Vec<Option<Vec<Detection>>> = vec![None; n];
    std::thread::scope(|scope| {
        let chunks: Vec<&[usize]> = detect_at.chunks(detect_at.len().div_ceil(threads).max(1)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|idx| {
                scope.spawn(move || {
                    let mut det = Detector::new();
                    idx.iter()
                        .map(|&i| (i, det.detect(&samples[i].frame)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            for (i, d) in h.join().expect("face detection thread") {
                found[i] = Some(d);
            }
        }
    });

    // Tracks: each detection joins the nearest track of its shot seen in
    // the last second, or starts one.
    let mut next_id = 1i64;
    let mut live: Vec<(Tracked, usize)> = Vec::new(); // (last box, sample index)
    let mut at: Vec<Vec<Tracked>> = vec![Vec::new(); n];
    for &i in &detect_at {
        live.retain(|(_, j)| shot_of[*j] == shot_of[i] && (i - j) as f64 * step <= 1.0);
        let dets = found[i].take().unwrap_or_default();
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (di, d) in dets.iter().enumerate() {
            for (li, (t, _)) in live.iter().enumerate() {
                let dx = (d.x + d.w / 2.0) - (t.b.x + t.b.w / 2.0);
                let dy = ((d.y + d.h / 2.0) - (t.b.y + t.b.h / 2.0)) * height / width;
                let dist = (dx * dx + dy * dy).sqrt();
                let size = d.w.max(t.b.w);
                let ratio = d.w.max(t.b.w) / d.w.min(t.b.w).max(1e-6);
                if dist < 0.75 * size && ratio < 1.6 {
                    pairs.push((dist / size, di, li));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut used_d, mut used_l) = (vec![false; dets.len()], vec![false; live.len()]);
        for (_, di, li) in pairs {
            if used_d[di] || used_l[li] {
                continue;
            }
            used_d[di] = true;
            used_l[li] = true;
            live[li] = (
                Tracked {
                    id: live[li].0.id,
                    b: dets[di],
                },
                i,
            );
            at[i].push(live[li].0);
        }
        for (di, d) in dets.iter().enumerate() {
            if !used_d[di] {
                let t = Tracked { id: next_id, b: *d };
                next_id += 1;
                live.push((t, i));
                at[i].push(t);
            }
        }
    }

    // A track seen once is usually a false find (a poster, a pattern).
    let mut seen: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for t in at.iter().flatten() {
        *seen.entry(t.id).or_default() += 1;
    }
    let long = |id: i64| seen.get(&id).copied().unwrap_or(0) >= 2 || detect_at.len() < 3;

    // Boxes on every sample: between two finds of a track in one shot, less
    // than a second apart, the box moves in a straight line.
    let mut boxes: Vec<Vec<Tracked>> = vec![Vec::new(); n];
    let mut last: std::collections::HashMap<i64, (usize, Detection)> = std::collections::HashMap::new();
    for &i in &detect_at {
        for t in &at[i] {
            if !long(t.id) {
                continue;
            }
            if let Some(&(j, b0)) = last.get(&t.id)
                && shot_of[j] == shot_of[i]
                && (i - j) as f64 * step <= 1.0
            {
                for k in j + 1..i {
                    let f = (k - j) as f64 / (i - j) as f64;
                    let l = |a: f64, b: f64| a + (b - a) * f;
                    boxes[k].push(Tracked {
                        id: t.id,
                        b: Detection {
                            x: l(b0.x, t.b.x),
                            y: l(b0.y, t.b.y),
                            w: l(b0.w, t.b.w),
                            h: l(b0.h, t.b.h),
                            score: b0.score.min(t.b.score),
                        },
                    });
                }
            }
            boxes[i].push(*t);
            last.insert(t.id, (i, t.b));
        }
    }

    // Talk: how much the mouth changes from the previous sample, beyond
    // how much the upper face does (head motion, camera noise).
    let mut raw: Vec<Vec<f64>> = vec![Vec::new(); n];
    for i in 0..n {
        for t in &boxes[i] {
            let b = t.b;
            let moving = (i > 0 && shot_of[i - 1] == shot_of[i])
                .then(|| {
                    let mouth = [b.x + 0.22 * b.w, b.y + 0.66 * b.h, b.x + 0.78 * b.w, b.y + 0.98 * b.h];
                    let upper = [b.x + 0.15 * b.w, b.y + 0.12 * b.h, b.x + 0.85 * b.w, b.y + 0.45 * b.h];
                    let m = samples[i].frame.diff(&samples[i - 1].frame, mouth)?;
                    let u = samples[i].frame.diff(&samples[i - 1].frame, upper)?;
                    Some((m - u).max(0.0))
                })
                .flatten();
            raw[i].push(moving.unwrap_or(0.0));
        }
    }
    // Smoothed over about half a second each way, per face.
    let reach = (0.5 / step).round().max(1.0) as usize;
    let mut smooth: Vec<Vec<f64>> = vec![Vec::new(); n];
    for i in 0..n {
        for (k, t) in boxes[i].iter().enumerate() {
            let (mut sum, mut cnt) = (raw[i][k], 1.0);
            for j in i.saturating_sub(reach)..(i + reach + 1).min(n) {
                if j == i || shot_of[j] != shot_of[i] {
                    continue;
                }
                if let Some(p) = boxes[j].iter().position(|o| o.id == t.id) {
                    sum += raw[j][p];
                    cnt += 1.0;
                }
            }
            smooth[i].push(sum / cnt);
        }
    }
    let mut all: Vec<f64> = smooth.iter().flatten().copied().collect();
    all.sort_by(|a, b| a.total_cmp(b));
    let reference = all
        .get(((all.len() as f64) * 0.9) as usize)
        .copied()
        .unwrap_or(1.0)
        .max(1.5);

    let r4 = |v: f64| (v * 1e4).round() / 1e4;
    let frames: Vec<Value> = (0..n)
        .map(|i| {
            let faces: Vec<Value> = boxes[i]
                .iter()
                .zip(&smooth[i])
                .map(|(t, s)| {
                    json!({
                        "id": t.id,
                        "x": r4(t.b.x), "y": r4(t.b.y), "w": r4(t.b.w), "h": r4(t.b.h),
                        "talk": r4((s / reference).clamp(0.0, 1.0)),
                    })
                })
                .collect();
            json!({ "frame": (samples[i].t * fps).round() as i64, "faces": faces })
        })
        .collect();
    let shots: Vec<Value> = cuts
        .iter()
        .enumerate()
        .map(|(k, &i)| {
            let end = cuts
                .get(k + 1)
                .map(|&j| samples[j].t)
                .unwrap_or(samples[n - 1].t + step);
            json!({
                "startFrame": (samples[i].t * fps).round() as i64,
                "endFrame": (end * fps).round() as i64,
            })
        })
        .collect();
    json!({
        "sourceWidth": width,
        "sourceHeight": height,
        "fps": fps,
        "frames": frames,
        "shots": shots,
        "tracker": format!("ultrafast faces v{VERSION}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with a mid-grey disc "face" whose lower part flickers when
    /// `talking`; no detector needed to test tracking glue, so this checks
    /// talk and cuts on boxes the test supplies.
    fn frame(w: usize, h: usize, level: u8) -> Gray {
        Gray {
            width: w,
            height: h,
            px: vec![level; w * h],
        }
    }

    #[test]
    fn finds_cuts_and_writes_the_worker_payload() {
        // Two shots: dark then bright, with nothing to detect.
        let samples: Vec<Sample> = (0..20)
            .map(|i| Sample {
                t: i as f64 * 0.1,
                frame: frame(64, 36, if i < 10 { 20 } else { 230 }),
            })
            .collect();
        let v = analyze(&samples, 30.0, 1280.0, 720.0);
        let shots = v["shots"].as_array().unwrap();
        assert_eq!(shots.len(), 2, "{v}");
        assert_eq!(shots[1]["startFrame"], 30);
        assert_eq!(v["frames"].as_array().unwrap().len(), 20);
        // It reads back as face tracks.
        let f = crate::Faces::from_json(&v, 30.0, 1.35).unwrap();
        assert_eq!(f.shots.len(), 2);
        assert_eq!(f.frames.len(), 20);
    }

    #[test]
    fn the_bundled_model_loads() {
        let mut d = Detector::new();
        assert!(d.detect(&frame(160, 90, 128)).is_empty());
    }
}
