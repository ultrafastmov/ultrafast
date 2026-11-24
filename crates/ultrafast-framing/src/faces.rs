//! Face tracks from the footage itself, for `clip_camera` documents that
//! come without a face worker's `faces`.
//!
//! The caller decodes the clip into grey frames at a steady rate (about
//! ten a second); this finds the cuts ([`cuts`], then [`cut_within`] on
//! the frames between two samples for the exact frame), finds faces
//! (SeetaFace's frontal detector, through `rustface`), links them into
//! tracks within each shot, estimates who is talking from how much the
//! lower face changes against the middle of the face, and writes the
//! payload [`crate::Faces::from_json`] reads: `{sourceWidth, sourceHeight,
//! fps, frames: [{frame, faces: [{id, x, y, w, h, talk}]}], shots:
//! [{startFrame, endFrame}]}` with boxes normalized to the frame.

use serde_json::{Value, json};

/// The detector's model (BSD-2-Clause, see `models/LICENSE-SeetaFace`).
const MODEL: &[u8] = include_bytes!("../models/seeta_fd_frontal_v1.0.bin");

/// Bumped when the analysis changes, so cached results are redone.
pub const VERSION: u32 = 2;

/// Cells across and down of the grid a frame's layout is averaged over.
const GRID_W: usize = 16;
const GRID_H: usize = 9;

/// How much two frames must differ ([`Look::change`]) for the step between
/// them to be a cut. Cameras in one room share their light, so a cut
/// between them moves only a quarter of the histogram (0.22 to 0.35 in a
/// three-camera interview, where nothing within a shot moved 0.07); the
/// grid term catches the new layout.
const CUT: f32 = 0.15;

/// The source frame shown at `t` seconds: the last one starting at or
/// before it (the numbering the payload's `frame`s and shots use).
pub fn frame_at(t: f64, fps: f64) -> i64 {
    ((t + 1e-4) * fps).floor() as i64
}

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

    /// The mean of each cell of a [`GRID_W`] x [`GRID_H`] grid, 0..1, row
    /// by row (`None` for cells of a frame too small to have pixels there).
    fn grid(&self) -> [Option<f32>; GRID_W * GRID_H] {
        let xs: Vec<usize> = (0..=GRID_W).map(|c| c * self.width / GRID_W).collect();
        let ys: Vec<usize> = (0..=GRID_H).map(|r| r * self.height / GRID_H).collect();
        let mut out = [None; GRID_W * GRID_H];
        for gy in 0..GRID_H {
            for gx in 0..GRID_W {
                let (x0, x1, y0, y1) = (xs[gx], xs[gx + 1], ys[gy], ys[gy + 1]);
                if x1 == x0 || y1 == y0 {
                    continue;
                }
                let sum: u64 = (y0..y1)
                    .map(|y| {
                        self.px[y * self.width + x0..y * self.width + x1]
                            .iter()
                            .map(|&p| p as u64)
                            .sum::<u64>()
                    })
                    .sum();
                out[gy * GRID_W + gx] = Some(sum as f32 / ((x1 - x0) * (y1 - y0)) as f32 / 255.0);
            }
        }
        out
    }
}

/// What a frame looks like at a glance, for telling shots apart: its luma
/// histogram and its layout (the mean of each cell of a coarse grid).
#[derive(Debug, Clone)]
pub struct Look {
    hist: [f32; 32],
    grid: [Option<f32>; GRID_W * GRID_H],
}

impl Look {
    pub fn of(frame: &Gray) -> Self {
        Self {
            hist: frame.histogram(),
            grid: frame.grid(),
        }
    }

    /// How different two frames look, from 0: how much of the histogram
    /// moved (0..1), or 1.5 x the mean change of the grid's cells, whichever
    /// is larger. A pan or a gesture moves the layout but little of the
    /// histogram; a cut between cameras in one room moves both.
    pub fn change(&self, other: &Look) -> f32 {
        let hist = self
            .hist
            .iter()
            .zip(&other.hist)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / 2.0;
        let (mut sum, mut cells) = (0f32, 0usize);
        for (a, b) in self.grid.iter().zip(&other.grid) {
            if let (Some(a), Some(b)) = (a, b) {
                sum += (a - b).abs();
                cells += 1;
            }
        }
        let grid = if cells > 0 { sum / cells as f32 } else { 0.0 };
        hist.max(1.5 * grid)
    }
}

/// The samples where a new shot starts (never the first): the step into
/// the sample changes the frame by more than [`CUT`], half again as much as
/// the steps either side (a pan, a gesture or a fade changes it over
/// several), and the frame stays changed (a flash, or something passing
/// close to the lens, comes back within 0.2 s). Shots last more than 0.25 s.
/// The cut lies between the sample and the one before; [`cut_within`] finds
/// its frame.
pub fn cuts(samples: &[Sample]) -> Vec<usize> {
    let n = samples.len();
    if n < 2 {
        return Vec::new();
    }
    let step = ((samples[n - 1].t - samples[0].t) / (n - 1) as f64).max(1e-3);
    let looks: Vec<Look> = samples.iter().map(|s| Look::of(&s.frame)).collect();
    // into[i]: the change from sample i - 1 to sample i.
    let into: Vec<f32> = (0..n)
        .map(|i| if i == 0 { 0.0 } else { looks[i].change(&looks[i - 1]) })
        .collect();
    let mut out: Vec<usize> = Vec::new();
    for i in 1..n {
        let d = into[i];
        if d <= CUT {
            continue;
        }
        let around = into[i - 1].max(into.get(i + 1).copied().unwrap_or(0.0));
        if d < 1.5 * around {
            continue;
        }
        if i + 2 < n && looks[i + 2].change(&looks[i - 1]) < 0.5 * d {
            continue;
        }
        if out.last().is_some_and(|&last| (i - last) as f64 * step <= 0.25) {
            continue;
        }
        out.push(i);
    }
    out
}

/// Where a cut found between two samples falls: `frames` are every source
/// frame from the sample before the cut to the sample after it, in order.
/// Returns the index of the new shot's first frame when one step between
/// neighbours holds the change (more than [`CUT`], at least half of the
/// change across all of them and twice any other step), and `None` when it
/// builds over several (a fade, a dissolve, a fast pan): no hard cut.
pub fn cut_within(frames: &[&Gray]) -> Option<usize> {
    match frames.len() {
        0 | 1 => return None,
        2 => return Some(1),
        _ => {}
    }
    let looks: Vec<Look> = frames.iter().map(|g| Look::of(g)).collect();
    let steps: Vec<f32> = looks.windows(2).map(|w| w[1].change(&w[0])).collect();
    let (k, top) = steps.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let next = steps
        .iter()
        .enumerate()
        .filter(|&(j, _)| j != k)
        .map(|(_, &s)| s)
        .fold(0f32, f32::max);
    let whole = looks[looks.len() - 1].change(&looks[0]);
    (top > CUT && top >= 0.5 * whole && top >= 2.0 * next).then_some(k + 1)
}

/// A cut: the first sample of the new shot and the source frame it starts
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub sample: usize,
    pub frame: i64,
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
/// worker's payload. Each shot starts on the sample after its cut; with
/// the frames between samples at hand, find the cut's frame with
/// [`cut_within`] and pass it to [`analyze_with`].
pub fn analyze(samples: &[Sample], fps: f64, width: f64, height: f64) -> Value {
    let cuts: Vec<Cut> = cuts(samples)
        .into_iter()
        .map(|i| Cut {
            sample: i,
            frame: frame_at(samples[i].t, fps),
        })
        .collect();
    analyze_with(samples, &cuts, fps, width, height)
}

/// [`analyze`] with the shots starting at `cuts` (in order, each on a
/// sample after the first, at a frame after the previous sample's and no
/// later than its own).
pub fn analyze_with(samples: &[Sample], cuts: &[Cut], fps: f64, width: f64, height: f64) -> Value {
    let n = samples.len();
    if n == 0 {
        return json!({ "sourceWidth": width, "sourceHeight": height, "fps": fps, "frames": [], "shots": [] });
    }
    let step = if n > 1 {
        ((samples[n - 1].t - samples[0].t) / (n - 1) as f64).max(1e-3)
    } else {
        0.1
    };

    // Shots: (first sample, first frame).
    let mut starts: Vec<(usize, i64)> = vec![(0, frame_at(samples[0].t, fps))];
    for c in cuts {
        let (last_sample, last_frame) = *starts.last().unwrap();
        if c.sample <= last_sample || c.sample >= n {
            continue;
        }
        let lo = frame_at(samples[c.sample - 1].t, fps) + 1;
        let hi = frame_at(samples[c.sample].t, fps).max(lo);
        let frame = c.frame.clamp(lo, hi);
        if frame > last_frame {
            starts.push((c.sample, frame));
        }
    }
    let mut shot_of = vec![0usize; n];
    let mut is_start = vec![false; n];
    for (k, &(s, _)) in starts.iter().enumerate() {
        is_start[s] = true;
        let end = starts.get(k + 1).map_or(n, |next| next.0);
        shot_of[s..end].fill(k);
    }

    // Detections on every other sample (and the first of each shot), on
    // all cores.
    let every = if step < 0.15 { 2 } else { 1 };
    let detect_at: Vec<usize> = (0..n)
        .filter(|&i| i % every == 0 || i + 1 == n || is_start[i])
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

    // Talk: how much the lower face changes from the previous sample, beyond
    // a quarter of how much the middle of the face does. The lower face
    // across the box's whole width, so a mouth in profile, at the box's
    // edge, counts; the middle without the sides, where hair and ears
    // swing. Only a quarter, because speakers move their heads and brows
    // as they talk: taking off all of it left a gesturing speaker quieter
    // than a listener who smiles. In the two-shots of a two-camera
    // interview the speaker scored higher in 97-98 % of samples this way,
    // 80-82 % that way.
    let mut raw: Vec<Vec<f64>> = vec![Vec::new(); n];
    for i in 0..n {
        for t in &boxes[i] {
            let b = t.b;
            let moving = (i > 0 && shot_of[i - 1] == shot_of[i])
                .then(|| {
                    let lower = [b.x, b.y + 0.6 * b.h, b.x + b.w, b.y + 1.05 * b.h];
                    let middle = [b.x + 0.3 * b.w, b.y + 0.2 * b.h, b.x + 0.7 * b.w, b.y + 0.45 * b.h];
                    let m = samples[i].frame.diff(&samples[i - 1].frame, lower)?;
                    let u = samples[i].frame.diff(&samples[i - 1].frame, middle)?;
                    Some((m - 0.25 * u).max(0.0))
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
            json!({ "frame": frame_at(samples[i].t, fps), "faces": faces })
        })
        .collect();
    let last_end = frame_at(samples[n - 1].t + step, fps).max(starts[starts.len() - 1].1 + 1);
    let shots: Vec<Value> = starts
        .iter()
        .enumerate()
        .map(|(k, &(_, start))| {
            let end = starts.get(k + 1).map_or(last_end, |next| next.1);
            json!({ "startFrame": start, "endFrame": end })
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

    /// A 160 x 90 frame from `f(x, y)`.
    fn draw(f: impl Fn(usize, usize) -> u8) -> Gray {
        let (w, h) = (160, 90);
        Gray {
            width: w,
            height: h,
            px: (0..w * h).map(|i| f(i % w, i / w)).collect(),
        }
    }

    fn at_ten_a_second(frames: Vec<Gray>) -> Vec<Sample> {
        frames
            .into_iter()
            .enumerate()
            .map(|(i, frame)| Sample {
                t: i as f64 * 0.1,
                frame,
            })
            .collect()
    }

    #[test]
    fn a_cut_between_cameras_in_one_room_is_found() {
        // The same light from the other side: an identical histogram, a
        // different layout. Within each shot the frame barely moves.
        let left = |shift: usize| draw(move |x, _| if x + shift < 80 { 60 } else { 180 });
        let right = |shift: usize| draw(move |x, _| if x + shift < 80 { 180 } else { 60 });
        let frames: Vec<Gray> = (0..12)
            .map(|i| if i < 6 { left(i % 2) } else { right(i % 2) })
            .collect();
        assert_eq!(Look::of(&left(0)).hist, Look::of(&right(0)).hist);
        assert_eq!(cuts(&at_ten_a_second(frames)), vec![6]);
    }

    #[test]
    fn a_pan_a_flash_and_a_fade_are_not_cuts() {
        // Bars sliding past a third of their period each sample.
        let pan: Vec<Gray> = (0..12)
            .map(|i| draw(move |x, _| if (x + i * 13) % 40 < 20 { 30 } else { 220 }))
            .collect();
        let pan = at_ten_a_second(pan);
        assert!(Look::of(&pan[1].frame).change(&Look::of(&pan[0].frame)) > CUT);
        assert_eq!(cuts(&pan), Vec::<usize>::new());
        // One bright sample.
        let flash = at_ten_a_second((0..12).map(|i| frame(160, 90, if i == 6 { 250 } else { 40 })).collect());
        assert_eq!(cuts(&flash), Vec::<usize>::new());
        // Up from black over half a second.
        let fade = at_ten_a_second((0..12).map(|i| frame(160, 90, (i.min(6) * 36) as u8)).collect());
        assert_eq!(cuts(&fade), Vec::<usize>::new());
    }

    #[test]
    fn a_cut_lands_on_its_frame_and_a_fade_is_none() {
        let a = draw(|x, _| if x < 80 { 60 } else { 180 });
        let a2 = draw(|x, _| if x < 81 { 60 } else { 180 });
        let b = draw(|x, _| if x < 80 { 180 } else { 60 });
        let b2 = draw(|x, _| if x < 79 { 180 } else { 60 });
        // The sample before, two frames between, the sample after: the new
        // shot starts on the third.
        assert_eq!(cut_within(&[&a, &a2, &b, &b2]), Some(2));
        assert_eq!(cut_within(&[&a, &b, &b2, &b]), Some(1));
        let ramp: Vec<Gray> = (0..4).map(|i| frame(160, 90, 40 + i as u8 * 40)).collect();
        assert_eq!(cut_within(&ramp.iter().collect::<Vec<_>>()), None);
        // With no frames between, the step is the cut.
        assert_eq!(cut_within(&[&a, &b]), Some(1));
    }

    #[test]
    fn shots_start_on_the_cut_frame() {
        let samples: Vec<Sample> = (0..20)
            .map(|i| Sample {
                t: i as f64 * 0.1,
                frame: frame(64, 36, if i < 10 { 20 } else { 230 }),
            })
            .collect();
        let shots = |cuts: &[Cut]| {
            let v = analyze_with(&samples, cuts, 30.0, 1280.0, 720.0);
            v["shots"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| (s["startFrame"].as_i64().unwrap(), s["endFrame"].as_i64().unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(shots(&[Cut { sample: 10, frame: 28 }]), vec![(0, 28), (28, 60)]);
        // Kept between the samples either side.
        assert_eq!(shots(&[Cut { sample: 10, frame: 5 }]), vec![(0, 28), (28, 60)]);
        assert_eq!(shots(&[]), vec![(0, 60)]);
    }
}
