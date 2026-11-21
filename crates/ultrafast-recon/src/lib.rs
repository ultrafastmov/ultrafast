//! Reconstruction tooling: the loop an agent runs to recreate a finished
//! video as an editable ultrafast document.
//!
//! 1. [`probe`] a reference: format, shots (cuts and fades), key frames.
//! 2. Build a document, then [`compare`] it against the reference frame by
//!    frame (PSNR, SSIM, side-by-side diff images for the worst frames).
//! 3. [`fit`] numeric params (positions, sizes, colors, timings) to the
//!    reference by search: rendering is fast enough to evaluate hundreds of
//!    candidates per second.

// Engine code: wide kernel and pass signatures and indexed loops over
// parallel arrays are the clearest form here.
#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop)]

pub mod fonts;
pub mod layers;
pub mod metrics;
pub mod plate;
pub mod reconstruct;
pub mod track;

use metrics::Frame;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Edit, Format, Param};
use ultrafast_render::{Mode, Renderer};

/// Renders frames of a reference video at a chosen size, through the same
/// zero-copy decoder the `video` op uses.
pub struct Reference {
    renderer: Renderer,
    doc: Document,
    pub info: ultrafast_media::VideoInfo,
}

impl Reference {
    pub fn open(device: &Device, path: &Path, width: Option<u32>, height: Option<u32>) -> Result<Self, String> {
        let info = ultrafast_media::video_info(path).map_err(|e| e.to_string())?;
        let (w, h) = match (width, height) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, (w as f64 * info.height as f64 / info.width as f64).round() as u32),
            _ => (info.width, info.height),
        };
        let abs = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc = Document::from_json(
            &json!({
                "version": 1,
                "format": { "width": w, "height": h, "fps": info.fps, "duration": info.duration },
                "output": "v",
                "nodes": { "v": { "op": "video", "params": { "src": abs.to_string_lossy(), "fit": "fill" } } }
            })
            .to_string(),
        )
        .map_err(|e| e.to_string())?;
        let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
        renderer.set_mode(Mode::Exact);
        Ok(Self { renderer, doc, info })
    }

    pub fn format(&self) -> Format {
        self.doc.format
    }

    /// The reference frame showing at `t` seconds.
    pub fn frame(&mut self, t: f64) -> Result<Frame, String> {
        let (px, _) = self.renderer.render_rgba8(&self.doc, t).map_err(|e| e.to_string())?;
        Ok(Frame {
            width: self.doc.format.width,
            height: self.doc.format.height,
            rgba: px,
        })
    }
}

/// One detected shot.
#[derive(Debug, Clone)]
pub struct Shot {
    pub start: f64,
    pub end: f64,
    /// How the shot begins: "start", "cut" or "fade".
    pub entry: &'static str,
    /// Mean sRGB color of the shot's middle frame.
    pub mean_color: [u8; 3],
}

/// Analyses a reference video: format, shots, and a contact sheet of the
/// middle frame of each shot (written to `sheet` if given).
pub fn probe(device: &Device, path: &Path, sheet: Option<&Path>) -> Result<Value, String> {
    let mut r = Reference::open(device, path, Some(160), None)?;
    let info = r.info;
    let n = (info.duration * info.fps).floor().max(1.0) as usize;
    let mut prev: Option<Vec<f32>> = None;
    let mut prev_luma = 0.0;
    let mut cuts: Vec<(f64, &'static str)> = vec![(0.0, "start")];
    let mut lumas = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / info.fps;
        let f = r.frame(t)?;
        let hist = f.histogram();
        let luma = f.mean_luma();
        if let Some(p) = &prev {
            let d: f32 = p.iter().zip(&hist).map(|(a, b)| (a - b).abs()).sum::<f32>() / 2.0;
            let last_cut = cuts.last().unwrap().0;
            // A hard cut: most of the histogram moves between two frames.
            if d > 0.45 && t - last_cut > 0.25 {
                cuts.push((t, "cut"));
            }
            // A fade from black: luma climbs out of near-black.
            if prev_luma < 0.02 && luma >= 0.02 && t - last_cut > 0.25 {
                cuts.push((t, "fade"));
            }
        }
        prev = Some(hist);
        prev_luma = luma;
        lumas.push(luma);
    }
    let mut shots = Vec::new();
    for (k, &(start, entry)) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).map(|c| c.0).unwrap_or(info.duration);
        let mid = (start + end) / 2.0;
        let f = r.frame(mid)?;
        shots.push(Shot {
            start,
            end,
            entry,
            mean_color: f.mean_srgb(),
        });
    }
    let mut out = json!({
        "path": path,
        "width": info.width,
        "height": info.height,
        "fps": info.fps,
        "duration": info.duration,
        "frames": n,
        "shots": shots.iter().map(|s| json!({
            "start": s.start, "end": s.end, "entry": s.entry,
            "mean_color": format!("#{:02x}{:02x}{:02x}", s.mean_color[0], s.mean_color[1], s.mean_color[2]),
        })).collect::<Vec<_>>(),
    });
    if let Some(sheet) = sheet {
        let mut big = Reference::open(device, path, Some(480), None)?;
        let frames: Vec<Frame> = shots
            .iter()
            .map(|s| big.frame((s.start + s.end) / 2.0))
            .collect::<Result<_, _>>()?;
        metrics::contact_sheet(&frames, 4).write_png(sheet)?;
        out["sheet"] = json!(sheet);
    }
    Ok(out)
}

/// Recognizes text in the reference frame at `t` and estimates each line's
/// color and the background, returning the boxes and a draft document with
/// one `text` node per line over a solid background (ready for `fit`).
/// Apple Vision reads the text on macOS, Tesseract elsewhere.
pub fn ocr(device: &Device, path: &Path, t: f64) -> Result<(Value, Document), String> {
    let boxes = ultrafast_media::vision::recognize_text(path, t).map_err(|e| e.to_string())?;
    let mut r = Reference::open(device, path, None, None)?;
    let frame = r.frame(t)?;
    let info = r.info;
    let bg = frame.border_color();
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    let mut nodes = serde_json::Map::new();
    nodes.insert("bg".into(), json!({ "op": "solid", "params": { "color": hex(bg) } }));
    let mut lines = Vec::new();
    let mut under = "bg".to_string();
    for (i, b) in boxes.iter().enumerate() {
        let (fg, _) = frame.two_colors(b.rect);
        let id = format!("text{}", i + 1);
        // Vision boxes span roughly ascender to descender: about one em.
        let size = (b.rect[3] * 0.95).max(4.0);
        nodes.insert(
            id.clone(),
            json!({ "op": "text", "params": {
                "text": b.text, "size": size.round(), "color": hex(fg),
                "position": [b.rect[0].round(), (b.rect[1] + b.rect[3] / 2.0).round()],
                "valign": "middle"
            } }),
        );
        let layer = format!("layer{}", i + 1);
        nodes.insert(
            layer.clone(),
            json!({ "op": "over", "inputs": { "fg": id, "bg": under } }),
        );
        under = layer;
        lines.push(json!({
            "text": b.text, "confidence": b.confidence,
            "rect": b.rect.map(|v| (v * 10.0).round() / 10.0), "color": hex(fg), "node": id,
            "words": b.words.iter().map(|(t, r)| json!({ "text": t, "rect": r.map(|v| (v * 10.0).round() / 10.0) })).collect::<Vec<_>>(),
        }));
    }
    let doc_json = json!({
        "version": 1,
        "format": { "width": info.width, "height": info.height, "fps": info.fps, "duration": info.duration },
        "output": under,
        "nodes": nodes,
    });
    let doc = Document::from_json(&doc_json.to_string()).map_err(|e| e.to_string())?;
    Ok((json!({ "t": t, "background": hex(bg), "lines": lines }), doc))
}

/// Writes a matte video of the reference's foreground subjects (white =
/// subject), one Vision mask per frame, for use as a `luma` matte that
/// separates footage into subject and background layers.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
pub fn matte(device: &Device, path: &Path, out: &Path, mut progress: impl FnMut(u64, u64)) -> Result<Value, String> {
    let info = ultrafast_media::video_info(path).map_err(|e| e.to_string())?;
    let n = (info.duration * info.fps).floor().max(1.0) as u64;
    let frames: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let t = i as f64 / info.fps;
            progress(i + 1, n);
            match ultrafast_media::vision::subject_mask(path, t).map_err(|e| e.to_string())? {
                Some((w, h, m)) if w == info.width && h == info.height => Ok(m),
                Some((w, h, m)) => Ok(resize_gray(&m, w, h, info.width, info.height)),
                None => Ok(vec![0u8; (info.width * info.height) as usize]),
            }
        })
        .collect::<Result<_, String>>()?;
    // Encode through ultrafast itself: an image sequence driven by a custom
    // shader would be overkill; write PNGs and let the agent use them, or
    // encode with the hardware writer via a tiny document per frame.
    let dir = out.with_extension("frames");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (i, m) in frames.iter().enumerate() {
        let rgba: Vec<u8> = m.iter().flat_map(|&v| [v, v, v, 255]).collect();
        Frame {
            width: info.width,
            height: info.height,
            rgba,
        }
        .write_png(&dir.join(format!("{i:05}.png")))?;
    }
    let _ = device;
    Ok(json!({ "frames": frames.len(), "dir": dir }))
}

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn resize_gray(m: &[u8], w: u32, h: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut out = vec![0u8; (tw * th) as usize];
    for y in 0..th {
        for x in 0..tw {
            let sx = (x as u64 * w as u64 / tw as u64) as usize;
            let sy = (y as u64 * h as u64 / th as u64) as usize;
            out[(y * tw + x) as usize] = m[sy * w as usize + sx];
        }
    }
    out
}

/// Writes reference frames at the given times as PNGs into `dir`.
pub fn extract(device: &Device, path: &Path, times: &[f64], dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut r = Reference::open(device, path, None, None)?;
    times
        .iter()
        .map(|&t| {
            let p = dir.join(format!("frame-{t:08.3}.png"));
            r.frame(t)?.write_png(&p)?;
            Ok(p)
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct CompareOptions {
    /// Times to compare (seconds). Empty: every frame of the document.
    pub times: Vec<f64>,
    /// Write side-by-side diffs of the worst frames here.
    pub out_dir: Option<PathBuf>,
    pub worst: usize,
    /// Reference time offset: reference t = doc t + offset.
    pub offset: f64,
}

/// Renders `doc` and compares it with the reference frame by frame.
pub fn compare(
    device: &Device,
    doc: &Document,
    base_dir: &Path,
    reference: &Path,
    opts: &CompareOptions,
) -> Result<Value, String> {
    let fmt = doc.format;
    let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    renderer.set_base_dir(base_dir);
    renderer.set_mode(Mode::Exact);
    let mut r = Reference::open(device, reference, Some(fmt.width), Some(fmt.height))?;
    let times: Vec<f64> = if opts.times.is_empty() {
        (0..fmt.frame_count()).map(|f| fmt.time_of(f)).collect()
    } else {
        opts.times.clone()
    };
    let mut scores = Vec::new();
    for &t in &times {
        let (px, _) = renderer.render_rgba8(doc, t).map_err(|e| e.to_string())?;
        let ours = Frame {
            width: fmt.width,
            height: fmt.height,
            rgba: px,
        };
        let theirs = r.frame(t + opts.offset)?;
        scores.push((t, ours.psnr(&theirs), ours.ssim(&theirs), ours, theirs));
    }
    let n = scores.len().max(1) as f64;
    let mean_psnr = scores.iter().map(|s| s.1.min(100.0)).sum::<f64>() / n;
    let mean_ssim = scores.iter().map(|s| s.2).sum::<f64>() / n;
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| scores[a].1.partial_cmp(&scores[b].1).unwrap());
    let mut worst = Vec::new();
    for &i in order.iter().take(opts.worst) {
        let (t, psnr, ssim, ours, theirs) = &scores[i];
        let mut entry = json!({ "t": t, "psnr": psnr, "ssim": ssim });
        if let Some(dir) = &opts.out_dir {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let p = dir.join(format!("diff-{t:08.3}.png"));
            metrics::side_by_side(ours, theirs).write_png(&p)?;
            entry["image"] = json!(p);
        }
        worst.push(entry);
    }
    Ok(json!({
        "frames": scores.len(),
        "mean_psnr": mean_psnr,
        "mean_ssim": mean_ssim,
        "min_psnr": order.first().map(|&i| scores[i].1),
        "per_frame": scores.iter().map(|s| json!({ "t": s.0, "psnr": s.1, "ssim": s.2 })).collect::<Vec<_>>(),
        "worst": worst,
    }))
}

/// A numeric param to fit: `node.param`, optionally one component.
#[derive(Debug, Clone)]
pub struct FitParam {
    pub node: String,
    pub param: String,
    /// Search step per component (default: 5% of the value, at least 1).
    pub step: Option<f64>,
}

impl FitParam {
    /// Parses `node.param` or `node.param:step`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (path, step) = match s.split_once(':') {
            Some((p, st)) => (p, Some(st.parse::<f64>().map_err(|e| e.to_string())?)),
            None => (s, None),
        };
        let (node, param) = path.rsplit_once('.').ok_or(format!("`{s}` is not node.param"))?;
        Ok(Self {
            node: node.into(),
            param: param.into(),
            step,
        })
    }
}

/// Fits static numeric params of `doc` to the reference at `times` by
/// minimizing mean squared error with Nelder-Mead. Returns the improved
/// document and a report.
pub fn fit(
    device: &Device,
    doc: &Document,
    base_dir: &Path,
    reference: &Path,
    params: &[FitParam],
    times: &[f64],
    iterations: usize,
) -> Result<(Document, Value), String> {
    let fmt = doc.format;
    let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    renderer.set_base_dir(base_dir);
    renderer.set_mode(Mode::Exact);
    let mut r = Reference::open(device, reference, Some(fmt.width), Some(fmt.height))?;
    let refs: Vec<Frame> = times.iter().map(|&t| r.frame(t)).collect::<Result<_, _>>()?;
    // Reference frames live on the GPU; each evaluation reduces the error
    // there and reads back one partial sum per tile instead of the frame.
    let ref_tex: Vec<ultrafast_gpu::Texture> = refs.iter().map(|f| f.upload(device)).collect();

    // Flatten the params into one search vector.
    let mut layout = Vec::new();
    let mut x0 = Vec::new();
    let mut steps = Vec::new();
    for p in params {
        let node = doc.nodes.get(&p.node).ok_or(format!("no node `{}`", p.node))?;
        let v = match node.params.get(&p.param) {
            Some(Param::Number(n)) => vec![*n],
            Some(Param::Vector(v)) => v.clone(),
            Some(Param::Animated { .. }) => {
                return Err(format!("{}.{} is animated; fit static params", p.node, p.param));
            }
            Some(_) => return Err(format!("{}.{} is not numeric", p.node, p.param)),
            None => return Err(format!("{}.{} is not set; give it a starting value", p.node, p.param)),
        };
        layout.push((p.clone(), v.len()));
        for x in &v {
            steps.push(p.step.unwrap_or((x.abs() * 0.05).max(1.0)));
            x0.push(*x);
        }
    }

    let mut work = doc.clone();
    let mut evals = 0usize;
    let mut eval = |x: &[f64], work: &mut Document| -> Result<f64, String> {
        evals += 1;
        let mut edits = Vec::new();
        let mut i = 0;
        for (p, len) in &layout {
            let v = x[i..i + len].to_vec();
            i += len;
            let value = if *len == 1 {
                Param::Number(v[0])
            } else {
                Param::Vector(v)
            };
            edits.push(Edit::SetParam {
                node: p.node.clone(),
                param: p.param.clone(),
                value,
            });
        }
        work.apply(&edits).map_err(|e| e.to_string())?;
        let mut err = 0.0;
        for (k, &t) in times.iter().enumerate() {
            err += renderer.frame_mse(work, t, &ref_tex[k]).map_err(|e| e.to_string())?;
        }
        Ok(err / times.len().max(1) as f64)
    };

    let start_err = eval(&x0, &mut work)?;
    let (best, best_err) = nelder_mead(&x0, &steps, iterations, |x| eval(x, &mut work))?;
    eval(&best, &mut work)?;
    let mut result = doc.clone();
    let mut i = 0;
    let mut fitted = serde_json::Map::new();
    let mut edits = Vec::new();
    for (p, len) in &layout {
        let v = best[i..i + len].to_vec();
        i += len;
        fitted.insert(format!("{}.{}", p.node, p.param), json!(v));
        let value = if *len == 1 {
            Param::Number(v[0])
        } else {
            Param::Vector(v)
        };
        edits.push(Edit::SetParam {
            node: p.node.clone(),
            param: p.param.clone(),
            value,
        });
    }
    result.apply(&edits).map_err(|e| e.to_string())?;
    let psnr = |mse: f64| {
        if mse <= 0.0 {
            100.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    };
    Ok((
        result,
        json!({
            "evaluations": evals,
            "psnr_before": psnr(start_err),
            "psnr_after": psnr(best_err),
            "params": fitted,
        }),
    ))
}

/// Fits an animated param: solves the param value at every time in `times`
/// (each frame warm-started from the previous), then simplifies the
/// per-frame values into keyframes within `tolerance` (param units).
/// Returns the document with the param replaced by those keyframes.
pub fn fit_motion(
    device: &Device,
    doc: &Document,
    base_dir: &Path,
    reference: &Path,
    param: &FitParam,
    times: &[f64],
    iterations: usize,
    tolerance: f64,
) -> Result<(Document, Value), String> {
    let fmt = doc.format;
    let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    renderer.set_base_dir(base_dir);
    renderer.set_mode(Mode::Exact);
    let mut r = Reference::open(device, reference, Some(fmt.width), Some(fmt.height))?;
    let node = doc.nodes.get(&param.node).ok_or(format!("no node `{}`", param.node))?;
    let start = match node.params.get(&param.param) {
        Some(Param::Number(n)) => vec![*n],
        Some(Param::Vector(v)) => v.clone(),
        Some(p @ Param::Animated { .. }) => p.eval(times.first().copied().unwrap_or(0.0)).unwrap_or_default(),
        _ => return Err(format!("{}.{} needs a numeric starting value", param.node, param.param)),
    };
    let steps: Vec<f64> = start
        .iter()
        .map(|x| param.step.unwrap_or((x.abs() * 0.05).max(1.0)))
        .collect();
    let mut work = doc.clone();
    let mut x = start.clone();
    let mut samples: Vec<(f64, Vec<f64>)> = Vec::new();
    let mut evals = 0usize;
    let mut psnrs = Vec::new();
    for &t in times {
        let target = r.frame(t)?;
        let target_tex = target.upload(device);
        let mut objective = |v: &[f64]| -> Result<f64, String> {
            evals += 1;
            let value = if v.len() == 1 {
                Param::Number(v[0])
            } else {
                Param::Vector(v.to_vec())
            };
            work.apply(&[Edit::SetParam {
                node: param.node.clone(),
                param: param.param.clone(),
                value,
            }])
            .map_err(|e| e.to_string())?;
            renderer.frame_mse(&work, t, &target_tex).map_err(|e| e.to_string())
        };
        // Warm start from the previous frame's answer with a smaller step.
        // On the first frame, or when tracking is lost (error jumps), search
        // globally first: local search cannot find an element it does not
        // overlap.
        let small: Vec<f64> = steps.iter().map(|s| s * 0.5).collect();
        let positional = x.len() == 2
            && ["center", "position", "pos", "anchor"]
                .iter()
                .any(|n| param.param.contains(n));
        let frame = [fmt.width as f64, fmt.height as f64];
        let global = |x0: &[f64],
                      objective: &mut dyn FnMut(&[f64]) -> Result<f64, String>|
         -> Result<Vec<f64>, String> {
            let mut best = (x0.to_vec(), objective(x0)?);
            let candidates: Vec<Vec<f64>> = if positional {
                (0..9)
                    .flat_map(|j| {
                        (0..16)
                            .map(move |i| vec![(i as f64 + 0.5) / 16.0 * frame[0], (j as f64 + 0.5) / 9.0 * frame[1]])
                    })
                    .collect()
            } else {
                (0..x0.len())
                    .flat_map(|d| {
                        [-16.0, -8.0, -4.0, -2.0, 2.0, 4.0, 8.0, 16.0]
                            .into_iter()
                            .map(move |m| (d, m))
                    })
                    .map(|(d, m)| {
                        let mut c = x0.to_vec();
                        c[d] += m * steps[d];
                        c
                    })
                    .collect()
            };
            for c in candidates {
                let e = objective(&c)?;
                if e < best.1 {
                    best = (c, e);
                }
            }
            Ok(best.0)
        };
        let prev_err = psnrs.last().copied();
        if samples.is_empty() {
            x = global(&x, &mut objective)?;
        }
        let (mut best, mut err) = nelder_mead(
            &x,
            if samples.is_empty() { &steps } else { &small },
            iterations,
            &mut objective,
        )?;
        let psnr_of = |e: f64| {
            if e <= 0.0 {
                100.0
            } else {
                10.0 * (255.0f64 * 255.0 / e).log10()
            }
        };
        if prev_err.is_some_and(|p| psnr_of(err) < p - 6.0) {
            let restart = global(&best, &mut objective)?;
            let (b2, e2) = nelder_mead(&restart, &steps, iterations, &mut objective)?;
            if e2 < err {
                (best, err) = (b2, e2);
            }
        }
        psnrs.push(psnr_of(err));
        x = best.clone();
        samples.push((t, best));
    }
    let keys = simplify_eased(&samples, tolerance);
    let keys_json: Vec<Value> = keys
        .iter()
        .map(|(t, v, ease)| {
            let mut k = json!({ "t": t, "v": v });
            if let Some(e) = ease {
                k["ease"] = serde_json::to_value(e).unwrap();
            }
            k
        })
        .collect();
    let animated: Param = serde_json::from_value(json!({ "keys": keys_json })).map_err(|e| e.to_string())?;
    let mut result = doc.clone();
    result
        .apply(&[Edit::SetParam {
            node: param.node.clone(),
            param: param.param.clone(),
            value: animated,
        }])
        .map_err(|e| e.to_string())?;
    let n = psnrs.len().max(1) as f64;
    Ok((
        result,
        json!({
            "frames": samples.len(),
            "evaluations": evals,
            "keyframes": keys.len(),
            "mean_psnr": psnrs.iter().sum::<f64>() / n,
            "keys": keys_json,
        }),
    ))
}

/// Like [`simplify`], but each segment may carry an easing curve: the
/// segment is accepted when some ease (named curves, or a fitted cubic
/// bezier) keeps every sample within `tolerance`. A move with an ease in/out
/// becomes two keys instead of a staircase of linear ones.
fn simplify_eased(samples: &[(f64, Vec<f64>)], tolerance: f64) -> Vec<(f64, Vec<f64>, Option<ultrafast_graph::Ease>)> {
    let n = samples.len();
    if n < 2 {
        return samples.iter().map(|(t, v)| (*t, v.clone(), None)).collect();
    }
    let mut out = Vec::new();
    fn rec(s: &[(f64, Vec<f64>)], tol: f64, out: &mut Vec<(f64, Vec<f64>, Option<ultrafast_graph::Ease>)>) {
        let (first, last) = (&s[0], &s[s.len() - 1]);
        if s.len() <= 2 {
            out.push((first.0, first.1.clone(), None));
            return;
        }
        if let Some(e) = best_ease(s, tol) {
            out.push((first.0, first.1.clone(), e));
            return;
        }
        // Split at the sample farthest from the linear interpolation.
        let mut worst = (1, 0.0f64);
        for (i, (t, v)) in s.iter().enumerate().skip(1).take(s.len() - 2) {
            let u = (t - first.0) / (last.0 - first.0).max(1e-12);
            let d = v
                .iter()
                .enumerate()
                .map(|(k, x)| (x - (first.1[k] + (last.1[k] - first.1[k]) * u)).abs())
                .fold(0.0, f64::max);
            if d > worst.1 {
                worst = (i, d);
            }
        }
        rec(&s[..=worst.0], tol, out);
        rec(&s[worst.0..], tol, out);
    }
    rec(samples, tolerance, &mut out);
    let last = &samples[n - 1];
    out.push((last.0, last.1.clone(), None));
    out
}

/// The simplest ease that fits a segment within `tol`: Some(None) for
/// linear, Some(Some(ease)) for a curve, None if nothing fits.
fn best_ease(s: &[(f64, Vec<f64>)], tol: f64) -> Option<Option<ultrafast_graph::Ease>> {
    use ultrafast_graph::Ease;
    let (first, last) = (&s[0], &s[s.len() - 1]);
    let err = |e: Ease| -> f64 {
        s.iter()
            .map(|(t, v)| {
                let u = e.apply((t - first.0) / (last.0 - first.0).max(1e-12));
                v.iter()
                    .enumerate()
                    .map(|(k, x)| (x - (first.1[k] + (last.1[k] - first.1[k]) * u)).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max)
    };
    if err(Ease::Linear) <= tol {
        return Some(None);
    }
    for e in [
        Ease::InOutCubic,
        Ease::OutCubic,
        Ease::InCubic,
        Ease::InOutQuad,
        Ease::OutQuad,
        Ease::InQuad,
    ] {
        if err(e) <= tol {
            return Some(Some(e));
        }
    }
    // Fit a cubic bezier ease to the samples.
    let (b, e) = nelder_mead(&[0.42, 0.0, 0.58, 1.0], &[0.15, 0.15, 0.15, 0.15], 300, |x| {
        let c = |v: f64| v.clamp(0.0, 1.0);
        Ok(err(Ease::Bezier(c(x[0]), x[1], c(x[2]), x[3])))
    })
    .ok()?;
    (e <= tol).then(|| Some(Ease::Bezier(b[0].clamp(0.0, 1.0), b[1], b[2].clamp(0.0, 1.0), b[3])))
}

/// Ramer-Douglas-Peucker over (time, value) samples: keeps the fewest keys
/// whose linear interpolation stays within `tolerance` of every sample.
#[cfg(test)]
fn simplify(samples: &[(f64, Vec<f64>)], tolerance: f64) -> Vec<(f64, Vec<f64>)> {
    if samples.len() <= 2 {
        return samples.to_vec();
    }
    let (first, last) = (&samples[0], &samples[samples.len() - 1]);
    let mut worst = (0usize, 0.0f64);
    for (i, (t, v)) in samples.iter().enumerate().skip(1).take(samples.len() - 2) {
        let u = (t - first.0) / (last.0 - first.0).max(1e-12);
        let d = v
            .iter()
            .enumerate()
            .map(|(k, x)| (x - (first.1[k] + (last.1[k] - first.1[k]) * u)).abs())
            .fold(0.0, f64::max);
        if d > worst.1 {
            worst = (i, d);
        }
    }
    if worst.1 <= tolerance {
        return vec![first.clone(), last.clone()];
    }
    let mut left = simplify(&samples[..=worst.0], tolerance);
    let right = simplify(&samples[worst.0..], tolerance);
    left.pop();
    left.extend(right);
    left
}

/// Nelder-Mead minimization (gradient-free; robust for the few-parameter,
/// noisy objectives of image matching).
fn nelder_mead(
    x0: &[f64],
    steps: &[f64],
    iterations: usize,
    mut f: impl FnMut(&[f64]) -> Result<f64, String>,
) -> Result<(Vec<f64>, f64), String> {
    let n = x0.len();
    let mut simplex: Vec<(Vec<f64>, f64)> = Vec::with_capacity(n + 1);
    simplex.push((x0.to_vec(), f(x0)?));
    for i in 0..n {
        let mut x = x0.to_vec();
        x[i] += steps[i];
        let fx = f(&x)?;
        simplex.push((x, fx));
    }
    for _ in 0..iterations {
        simplex.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let best = simplex[0].1;
        let worst = simplex[n].1;
        if (worst - best).abs() < 1e-6 * (1.0 + best.abs()) {
            break;
        }
        let centroid: Vec<f64> = (0..n)
            .map(|j| simplex[..n].iter().map(|s| s.0[j]).sum::<f64>() / n as f64)
            .collect();
        let point = |c: f64| -> Vec<f64> {
            (0..n)
                .map(|j| centroid[j] + c * (simplex[n].0[j] - centroid[j]))
                .collect()
        };
        let xr = point(-1.0);
        let fr = f(&xr)?;
        if fr < simplex[0].1 {
            let xe = point(-2.0);
            let fe = f(&xe)?;
            simplex[n] = if fe < fr { (xe, fe) } else { (xr, fr) };
        } else if fr < simplex[n - 1].1 {
            simplex[n] = (xr, fr);
        } else {
            let xc = if fr < simplex[n].1 { point(-0.5) } else { point(0.5) };
            let fc = f(&xc)?;
            if fc < simplex[n].1.min(fr) {
                simplex[n] = (xc, fc);
            } else {
                // Shrink toward the best point.
                let b = simplex[0].0.clone();
                for s in simplex.iter_mut().skip(1) {
                    let x: Vec<f64> = (0..n).map(|j| b[j] + 0.5 * (s.0[j] - b[j])).collect();
                    *s = (x.clone(), f(&x)?);
                }
            }
        }
    }
    simplex.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    Ok(simplex.swap_remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nelder_mead_finds_minimum() {
        let (x, fx) = nelder_mead(&[0.0, 0.0], &[1.0, 1.0], 200, |x| {
            Ok((x[0] - 3.0).powi(2) + (x[1] + 2.0).powi(2))
        })
        .unwrap();
        assert!(
            (x[0] - 3.0).abs() < 1e-3 && (x[1] + 2.0).abs() < 1e-3 && fx < 1e-6,
            "{x:?}"
        );
    }

    #[test]
    fn simplify_keeps_corners() {
        let s: Vec<(f64, Vec<f64>)> = (0..=20)
            .map(|i| (i as f64, vec![if i <= 10 { i as f64 } else { 20.0 - i as f64 }]))
            .collect();
        let k = simplify(&s, 0.01);
        assert_eq!(k.iter().map(|k| k.0).collect::<Vec<_>>(), vec![0.0, 10.0, 20.0]);
    }

    #[test]
    fn fit_param_parses() {
        let p = FitParam::parse("intro/title.position:4").unwrap();
        assert_eq!(
            (p.node.as_str(), p.param.as_str(), p.step),
            ("intro/title", "position", Some(4.0))
        );
    }
}
