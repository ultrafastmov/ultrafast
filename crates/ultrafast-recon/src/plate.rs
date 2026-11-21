//! Background plates: what is behind everything else in a shot.
//!
//! The plate is the per-pixel temporal median of frames sampled across the
//! shot (anything that moves drops out), with the pixels the draft's own
//! layers cover filled in from their surroundings (push-pull inpainting in
//! linear light). Parametric backgrounds (solid, linear and radial
//! gradients in either interpolation space) are fitted to the plate and
//! rendered by ultrafast to score them, so an agent can keep a gradient as an
//! editable node when it reproduces the plate, and fall back to the image
//! plate when it does not.

use crate::metrics::Frame;
use crate::{Reference, nelder_mead};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Node, Param};
use ultrafast_render::ops::srgb_to_linear;
use ultrafast_render::{Mode, Renderer};

#[derive(Debug, Clone)]
pub struct PlateOptions {
    pub from: f64,
    pub to: f64,
    /// Frames sampled across [from, to).
    pub samples: usize,
    /// Background node of `doc` to replace; the rest of the document marks
    /// the pixels the plate must fill in.
    pub background: Option<String>,
    /// Minimum PSNR (dB, against the plate) for recommending a gradient
    /// over the image plate.
    pub threshold: f64,
}

impl Default for PlateOptions {
    fn default() -> Self {
        Self {
            from: 0.0,
            to: f64::INFINITY,
            samples: 15,
            background: None,
            threshold: 38.0,
        }
    }
}

fn to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

fn hex(c: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// Per-pixel, per-channel median of the frames.
fn median(frames: &[Frame]) -> Frame {
    let (w, h) = (frames[0].width, frames[0].height);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    let mut v = Vec::with_capacity(frames.len());
    for i in 0..(w * h) as usize {
        for c in 0..3 {
            v.clear();
            v.extend(frames.iter().map(|f| f.rgba[i * 4 + c]));
            v.sort_unstable();
            rgba[i * 4 + c] = v[v.len() / 2];
        }
    }
    Frame {
        width: w,
        height: h,
        rgba,
    }
}

/// Fills pixels with `known == false` from their surroundings (push-pull:
/// average down a pyramid by known weight, then blend back up).
fn inpaint(lin: &mut [[f32; 3]], known: &[bool], w: usize, h: usize) {
    // Level 0: premultiplied color and weight.
    let mut levels: Vec<(Vec<[f32; 4]>, usize, usize)> = vec![(
        lin.iter()
            .zip(known)
            .map(|(c, &k)| {
                let a = k as u8 as f32;
                [c[0] * a, c[1] * a, c[2] * a, a]
            })
            .collect(),
        w,
        h,
    )];
    while {
        let (_, lw, lh) = levels.last().unwrap();
        *lw > 1 || *lh > 1
    } {
        let (src, lw, lh) = levels.last().unwrap();
        let (nw, nh) = (lw.div_ceil(2), lh.div_ceil(2));
        let mut dst = vec![[0.0f32; 4]; nw * nh];
        for y in 0..nh {
            for x in 0..nw {
                let mut s = [0.0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((2 * x + dx).min(lw - 1), (2 * y + dy).min(lh - 1));
                    let p = src[sy * lw + sx];
                    for k in 0..4 {
                        s[k] += p[k];
                    }
                }
                // Normalize color by weight; cap weight at 1.
                let wsum = s[3];
                dst[y * nw + x] = if wsum > 0.0 {
                    let a = wsum.min(1.0);
                    [s[0] / wsum * a, s[1] / wsum * a, s[2] / wsum * a, a]
                } else {
                    [0.0; 4]
                };
            }
        }
        levels.push((dst, nw, nh));
    }
    // Pull: fill each level's missing weight from the coarser level.
    for l in (0..levels.len() - 1).rev() {
        let (coarse, cw, ch) = {
            let (c, cw, ch) = &levels[l + 1];
            (c.clone(), *cw, *ch)
        };
        let (fine, fw, fh) = &mut levels[l];
        for y in 0..*fh {
            for x in 0..*fw {
                // Bilinear sample of the coarse level at this pixel's center.
                let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (cw - 1) as f32);
                let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (ch - 1) as f32);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                let mut c = [0.0f32; 4];
                for k in 0..4 {
                    let top = coarse[y0 * cw + x0][k] * (1.0 - tx) + coarse[y0 * cw + x1][k] * tx;
                    let bot = coarse[y1 * cw + x0][k] * (1.0 - tx) + coarse[y1 * cw + x1][k] * tx;
                    c[k] = top * (1.0 - ty) + bot * ty;
                }
                let p = &mut fine[y * *fw + x];
                let missing = 1.0 - p[3];
                if missing > 0.0 {
                    // The coarse color is premultiplied by its own weight.
                    let cc = if c[3] > 0.0 {
                        [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
                    } else {
                        [0.0; 3]
                    };
                    for k in 0..3 {
                        p[k] += cc[k] * missing;
                    }
                    p[3] = 1.0;
                }
            }
        }
    }
    for (i, c) in lin.iter_mut().enumerate() {
        if !known[i] {
            let p = levels[0].0[i];
            *c = [p[0], p[1], p[2]];
        }
    }
}

/// Least squares `v ≈ a (1 - t) + b t` per channel.
fn fit_two_stops(ts: &[f32], vals: &[[f32; 3]]) -> ([f64; 3], [f64; 3]) {
    // Normal equations in (a, b).
    let (mut s00, mut s01, mut s11) = (0.0f64, 0.0f64, 0.0f64);
    let mut r0 = [0.0f64; 3];
    let mut r1 = [0.0f64; 3];
    for (&t, v) in ts.iter().zip(vals) {
        let (u, t) = (1.0 - t as f64, t as f64);
        s00 += u * u;
        s01 += u * t;
        s11 += t * t;
        for c in 0..3 {
            r0[c] += u * v[c] as f64;
            r1[c] += t * v[c] as f64;
        }
    }
    let det = (s00 * s11 - s01 * s01).max(1e-12);
    let mut a = [0.0; 3];
    let mut b = [0.0; 3];
    for c in 0..3 {
        a[c] = (r0[c] * s11 - r1[c] * s01) / det;
        b[c] = (r1[c] * s00 - r0[c] * s01) / det;
    }
    (a, b)
}

/// Candidate background nodes fitted to the plate's known pixels.
fn models(plate: &Frame, known: &[bool]) -> Vec<(String, Node)> {
    let (w, h) = (plate.width as usize, plate.height as usize);
    // Fit on a subsampled grid: gradients are smooth.
    let step = ((w * h) as f64 / 40_000.0).sqrt().max(1.0) as usize;
    let mut pts: Vec<[f32; 2]> = Vec::new();
    let mut srgb: Vec<[f32; 3]> = Vec::new();
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = y * w + x;
            if !known[i] {
                continue;
            }
            pts.push([x as f32 + 0.5, y as f32 + 0.5]);
            srgb.push([0, 1, 2].map(|c| plate.rgba[i * 4 + c] as f32 / 255.0));
        }
    }
    let mut out = Vec::new();
    if pts.is_empty() {
        return out;
    }
    let n = pts.len() as f64;
    let mean = [0, 1, 2].map(|c| srgb.iter().map(|v| v[c] as f64).sum::<f64>() / n);
    let node =
        |op: &str, params: Value| -> Node { serde_json::from_value(json!({ "op": op, "params": params })).unwrap() };
    out.push(("solid".into(), node("solid", json!({ "color": hex(mean) }))));

    for space in ["srgb", "linear"] {
        let vals: Vec<[f32; 3]> = if space == "srgb" {
            srgb.clone()
        } else {
            srgb.iter()
                .map(|v| v.map(|x| srgb_to_linear(x as f64) as f32))
                .collect()
        };
        let to_hex = |c: [f64; 3]| {
            if space == "srgb" {
                hex(c)
            } else {
                hex(c.map(|x| to_srgb(x as f32) as f64))
            }
        };

        // Linear: per-channel affine fit, shared direction from the
        // channels' combined gradient (2x2 eigenproblem).
        let (mx, my) = (
            pts.iter().map(|p| p[0] as f64).sum::<f64>() / n,
            pts.iter().map(|p| p[1] as f64).sum::<f64>() / n,
        );
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        let mut sxv = [0.0f64; 3];
        let mut syv = [0.0f64; 3];
        let vmean = [0, 1, 2].map(|c| vals.iter().map(|v| v[c] as f64).sum::<f64>() / n);
        for (p, v) in pts.iter().zip(&vals) {
            let (dx, dy) = (p[0] as f64 - mx, p[1] as f64 - my);
            sxx += dx * dx;
            sxy += dx * dy;
            syy += dy * dy;
            for c in 0..3 {
                sxv[c] += dx * (v[c] as f64 - vmean[c]);
                syv[c] += dy * (v[c] as f64 - vmean[c]);
            }
        }
        let det = (sxx * syy - sxy * sxy).max(1e-12);
        let g: Vec<[f64; 2]> = (0..3)
            .map(|c| [(sxv[c] * syy - syv[c] * sxy) / det, (syv[c] * sxx - sxv[c] * sxy) / det])
            .collect();
        let (m00, m01, m11) = (
            g.iter().map(|v| v[0] * v[0]).sum::<f64>(),
            g.iter().map(|v| v[0] * v[1]).sum::<f64>(),
            g.iter().map(|v| v[1] * v[1]).sum::<f64>(),
        );
        // Largest eigenvector of [[m00, m01], [m01, m11]].
        let tr = m00 + m11;
        let l = tr / 2.0 + ((m00 - m11).powi(2) / 4.0 + m01 * m01).sqrt();
        let mut d = if m01.abs() > 1e-15 {
            [l - m11, m01]
        } else if m00 >= m11 {
            [1.0, 0.0]
        } else {
            [0.0, 1.0]
        };
        let dl = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-12);
        d = [d[0] / dl, d[1] / dl];
        // Stops at the frame's extreme projections, so t spans [0, 1].
        let corners = [[0.0, 0.0], [w as f64, 0.0], [0.0, h as f64], [w as f64, h as f64]];
        let proj = |p: [f64; 2]| p[0] * d[0] + p[1] * d[1];
        let (lo, hi) = corners
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), &c| (lo.min(proj(c)), hi.max(proj(c))));
        let span = (hi - lo).max(1e-9);
        let ts: Vec<f32> = pts
            .iter()
            .map(|p| ((proj([p[0] as f64, p[1] as f64]) - lo) / span) as f32)
            .collect();
        let (a, b) = fit_two_stops(&ts, &vals);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let c0 = proj([cx, cy]);
        let start = [cx + d[0] * (lo - c0), cy + d[1] * (lo - c0)];
        let end = [cx + d[0] * (hi - c0), cy + d[1] * (hi - c0)];
        let r1 = |v: f64| (v * 10.0).round() / 10.0;
        out.push((
            format!("linear_{space}"),
            node(
                "gradient",
                json!({ "kind": "linear", "a": to_hex(a), "b": to_hex(b),
                        "start": [r1(start[0]), r1(start[1])], "end": [r1(end[0]), r1(end[1])], "space": space }),
            ),
        ));

        // Radial: search center and radius; stops by least squares.
        let radial_err = |x: &[f64]| -> Result<f64, String> {
            let r = x[2].abs().max(1.0);
            let ts: Vec<f32> = pts
                .iter()
                .map(|p| (((p[0] as f64 - x[0]).hypot(p[1] as f64 - x[1])) / r).min(1.0) as f32)
                .collect();
            let (a, b) = fit_two_stops(&ts, &vals);
            let mut e = 0.0;
            for (t, v) in ts.iter().zip(&vals) {
                for c in 0..3 {
                    let m = a[c] + (b[c] - a[c]) * *t as f64;
                    e += (m - v[c] as f64).powi(2);
                }
            }
            Ok(e / n)
        };
        let diag = (w as f64).hypot(h as f64);
        let mut best = (vec![cx, cy, diag / 2.0], f64::MAX);
        // A few starts: the radial optimum is not convex.
        for start in [
            [cx, cy, diag / 2.0],
            [cx, cy, diag],
            [0.0, 0.0, diag],
            [w as f64, h as f64, diag],
        ] {
            let (x, e) = nelder_mead(&start, &[w as f64 / 8.0, h as f64 / 8.0, diag / 8.0], 150, radial_err).unwrap();
            if e < best.1 {
                best = (x, e);
            }
        }
        let x = best.0;
        let r = x[2].abs().max(1.0);
        let ts: Vec<f32> = pts
            .iter()
            .map(|p| (((p[0] as f64 - x[0]).hypot(p[1] as f64 - x[1])) / r).min(1.0) as f32)
            .collect();
        let (a, b) = fit_two_stops(&ts, &vals);
        out.push((
            format!("radial_{space}"),
            node(
                "gradient",
                json!({ "kind": "radial", "a": to_hex(a), "b": to_hex(b),
                        "center": [r1(x[0]), r1(x[1])], "radius": r1(r), "space": space }),
            ),
        ));
    }
    out
}

/// Builds the background plate of `reference` over `[from, to)` and fits
/// parametric backgrounds to it. Writes the plate to `image`. With a
/// document and its background node, the rest of the document marks the
/// pixels to fill, and a copy of the document with that node replaced by
/// the recommended background is returned.
pub fn plate(
    device: &Device,
    reference: &Path,
    doc: Option<(&Document, &Path)>,
    image: &Path,
    opts: &PlateOptions,
) -> Result<(Option<Document>, Value), String> {
    let mut r = Reference::open(device, reference, None, None)?;
    let info = r.info;
    let (from, to) = (opts.from.max(0.0), opts.to.min(info.duration));
    if to <= from {
        return Err(format!("empty range {from}..{to}"));
    }
    let n = opts.samples.max(1);
    let times: Vec<f64> = (0..n)
        .map(|i| from + (to - from) * (i as f64 + 0.5) / n as f64)
        .collect();
    let frames: Vec<Frame> = times.iter().map(|&t| r.frame(t)).collect::<Result<_, _>>()?;
    let (w, h) = (frames[0].width as usize, frames[0].height as usize);
    let med = median(&frames);

    // Pixels the document's other layers cover at any sampled time.
    let mut known = vec![true; w * h];
    if let (Some((doc, base)), Some(bg)) = (doc, &opts.background) {
        if !doc.nodes.contains_key(bg) {
            return Err(format!("no node `{bg}`"));
        }
        let mut fg_only = doc.clone();
        fg_only.nodes.insert(
            bg.clone(),
            serde_json::from_value(json!({ "op": "solid", "params": { "color": "#00000000" } })).unwrap(),
        );
        if (fg_only.format.width as usize, fg_only.format.height as usize) != (w, h) {
            return Err(format!(
                "document is {}x{}, reference is {w}x{h}",
                fg_only.format.width, fg_only.format.height
            ));
        }
        let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
        renderer.set_base_dir(base);
        renderer.set_mode(Mode::Exact);
        let mut cover = vec![false; w * h];
        for &t in &times {
            let (px, _) = renderer.render_rgba8(&fg_only, t).map_err(|e| e.to_string())?;
            for (i, p) in px.chunks_exact(4).enumerate() {
                cover[i] |= p[3] > 2;
            }
        }
        // Dilate by 2 px: encoders smear edges.
        for y in 0..h {
            for x in 0..w {
                if cover[y * w + x] {
                    for yy in y.saturating_sub(2)..(y + 3).min(h) {
                        for xx in x.saturating_sub(2)..(x + 3).min(w) {
                            known[yy * w + xx] = false;
                        }
                    }
                }
            }
        }
    }
    let holes = known.iter().filter(|k| !**k).count();
    // How well one still plate explains the sampled frames (low for moving
    // cameras and live footage, where the shot needs a video background).
    let frames_psnr = {
        let mut total = 0.0;
        for f in &frames {
            let (mut sum, mut count) = (0.0f64, 0usize);
            for i in (0..w * h).filter(|&i| known[i]) {
                for c in 0..3 {
                    sum += (f.rgba[i * 4 + c] as f64 - med.rgba[i * 4 + c] as f64).powi(2);
                }
                count += 3;
            }
            let mse = sum / count.max(1) as f64;
            total += if mse <= 0.0 {
                100.0
            } else {
                (10.0 * (255.0f64 * 255.0 / mse).log10()).min(100.0)
            };
        }
        total / frames.len() as f64
    };

    // The image plate: median with holes inpainted in linear light.
    let mut lin: Vec<[f32; 3]> = med
        .rgba
        .chunks_exact(4)
        .map(|p| [0, 1, 2].map(|c| srgb_to_linear(p[c] as f64 / 255.0) as f32))
        .collect();
    if holes > 0 && holes < w * h {
        inpaint(&mut lin, &known, w, h);
    }
    let mut filled = med.rgba.clone();
    for (i, c) in lin.iter().enumerate() {
        if !known[i] {
            for k in 0..3 {
                filled[i * 4 + k] = (to_srgb(c[k]) * 255.0).round() as u8;
            }
        }
    }
    let plate = Frame {
        width: w as u32,
        height: h as u32,
        rgba: filled,
    };
    plate.write_png(image)?;

    // Parametric candidates, scored by ultrafast's own render of them.
    let mut scored = Vec::new();
    let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    renderer.set_mode(Mode::Exact);
    for (name, node) in models(&med, &known) {
        let d = Document::from_json(
            &json!({
                "version": 1,
                "format": { "width": w, "height": h, "fps": info.fps, "duration": 1 },
                "output": "bg",
                "nodes": { "bg": node }
            })
            .to_string(),
        )
        .map_err(|e| e.to_string())?;
        d.validate().map_err(|e| e.to_string())?;
        let (px, _) = renderer.render_rgba8(&d, 0.0).map_err(|e| e.to_string())?;
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for i in 0..w * h {
            if known[i] {
                for c in 0..3 {
                    sum += (px[i * 4 + c] as f64 - med.rgba[i * 4 + c] as f64).powi(2);
                }
                count += 3;
            }
        }
        let mse = sum / count.max(1) as f64;
        let psnr = if mse <= 0.0 {
            100.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        };
        scored.push((name, node, psnr));
    }
    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());

    let image_node: Node = serde_json::from_value(json!({
        "op": "image",
        "params": { "src": image.to_string_lossy(), "fit": "fill" }
    }))
    .unwrap();
    let pick = scored.first().filter(|s| s.2 >= opts.threshold);
    let (choice, chosen) = match pick {
        Some((name, node, _)) => (name.clone(), node.clone()),
        None => ("image".to_string(), image_node.clone()),
    };
    let out_doc = match (doc, &opts.background) {
        (Some((doc, base)), Some(bg)) => {
            let mut d = doc.clone();
            let mut node = chosen.clone();
            if node.op == "image" {
                // Relative to the document when possible.
                let rel = pathdiff(image, base).unwrap_or_else(|| image.to_path_buf());
                node.params
                    .insert("src".into(), Param::Text(rel.to_string_lossy().into_owned()));
            }
            // Keep the old node's timing.
            node.timing = d.nodes[bg].timing;
            d.nodes.insert(bg.clone(), node);
            Some(d)
        }
        _ => None,
    };
    Ok((
        out_doc,
        json!({
            "image": image,
            "samples": times.len(),
            "range": [from, to],
            "holes": holes as f64 / (w * h) as f64,
            "frames_psnr": (frames_psnr * 100.0).round() / 100.0,
            "models": scored.iter().map(|(name, node, psnr)| json!({
                "model": name, "psnr": (psnr * 100.0).round() / 100.0, "node": node,
            })).collect::<Vec<_>>(),
            "recommended": choice,
            "node": chosen,
        }),
    ))
}

/// `path` relative to `base`, if `path` is inside it.
fn pathdiff(path: &Path, base: &Path) -> Option<PathBuf> {
    let p = std::fs::canonicalize(path).ok()?;
    let b = std::fs::canonicalize(base).ok()?;
    p.strip_prefix(&b).ok().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inpaint_fills_smoothly() {
        let (w, h) = (16, 8);
        let mut lin: Vec<[f32; 3]> = (0..w * h).map(|i| [(i % w) as f32 / w as f32, 0.5, 0.0]).collect();
        let truth = lin.clone();
        let known: Vec<bool> = (0..w * h).map(|i| !(5..9).contains(&(i % w))).collect();
        for (i, c) in lin.iter_mut().enumerate() {
            if !known[i] {
                *c = [9.0; 3];
            }
        }
        inpaint(&mut lin, &known, w, h);
        for i in 0..w * h {
            assert!(
                (lin[i][0] - truth[i][0]).abs() < 0.15,
                "{i}: {:?} vs {:?}",
                lin[i],
                truth[i]
            );
            assert!((lin[i][1] - 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn two_stops_exact() {
        let ts: Vec<f32> = (0..11).map(|i| i as f32 / 10.0).collect();
        let vals: Vec<[f32; 3]> = ts.iter().map(|t| [0.2 + 0.6 * t, 1.0 - t, 0.5]).collect();
        let (a, b) = fit_two_stops(&ts, &vals);
        assert!((a[0] - 0.2).abs() < 1e-5 && (b[0] - 0.8).abs() < 1e-5);
        assert!((a[1] - 1.0).abs() < 1e-5 && b[1].abs() < 1e-5);
    }
}
