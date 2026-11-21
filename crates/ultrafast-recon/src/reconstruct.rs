//! One-command reconstruction: a finished video in, an editable ultrafast
//! document out.
//!
//! Per shot (from [`probe`](crate::probe)):
//! 1. text lines are recognized (Apple Vision) in the shot's middle frame
//!    and become `text` layers;
//! 2. each layer's font, size, baseline, tracking and color are matched
//!    ([`match_font`](crate::fonts::match_font));
//! 3. the background becomes a gradient, a still plate, or (for moving
//!    footage) the reference video itself ([`plate`](crate::plate::plate));
//! 4. every layer's presence is measured frame by frame against the
//!    background and its own coverage, and becomes a visibility window and
//!    opacity keyframes (fades), with layers that do not fit a still
//!    placement flagged as moving for `fit-motion`.
//!
//! Shots are stacked with timing windows, so each one is an independent,
//! editable group of nodes named `s<k>_...`.

use crate::fonts::{MatchOptions, match_font};
use crate::metrics::Frame;
use crate::plate::{PlateOptions, plate};
use crate::track::{Sample, Tracker};
use crate::{Reference, probe, simplify_eased};
use serde_json::{Map, Value, json};
use std::path::Path;
use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Param};
use ultrafast_render::ops::srgb_to_linear;
use ultrafast_render::{Mode, Renderer};

#[derive(Debug, Clone)]
pub struct ReconstructOptions {
    /// Recognized lines below this confidence are ignored.
    pub min_confidence: f32,
    /// Frames sampled per shot for plates.
    pub plate_samples: usize,
    /// Most frames examined per shot for layer presence.
    pub presence_frames: usize,
    /// A still plate must explain the shot's frames to this PSNR (dB);
    /// below it the background is the reference footage itself.
    pub still_threshold: f64,
}

impl Default for ReconstructOptions {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            plate_samples: 15,
            presence_frames: 120,
            still_threshold: 32.0,
        }
    }
}

/// Fonts whose best rendering matches the reference coverage worse than
/// this (dB) are taken as OCR noise.
const MIN_GLYPH_MATCH: f64 = 16.0;

/// Removes text layer `id` and its `over`, reconnecting what was under it.
fn drop_layer(doc: &Document, id: &str, top: &mut String) -> Result<Document, String> {
    let mut doc = doc.clone();
    let layer = id.replace("_text", "_layer");
    let under = doc.nodes[&layer].inputs["bg"].clone();
    for n in doc.nodes.values_mut() {
        for src in n.inputs.values_mut() {
            if *src == layer {
                *src = under.clone();
            }
        }
    }
    if doc.output == layer {
        doc.output = under.clone();
    }
    if *top == layer {
        *top = under;
    }
    doc.nodes.remove(&layer);
    doc.nodes.remove(id);
    fresh(&doc)
}

/// A line whose ink is centered in the frame was almost certainly set
/// centered: switch it to `align: center` at the frame's center, so edits
/// to its text stay centered. Returns whether it did.
fn center_if_centered(device: &Device, doc: &mut Document, base: &Path, id: &str, t: f64) -> Result<bool, String> {
    let w = doc.format.width as f64;
    let ink_center = |doc: &Document| -> Result<f64, String> {
        let mut r = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
        r.set_base_dir(base);
        r.set_mode(Mode::Exact);
        r.render_rgba8(doc, t).map_err(|e| e.to_string())?;
        let b = r.node_bounds(id).ok_or("no bounds")?;
        Ok((b[0] + b[2]) as f64 / 2.0)
    };
    let before = ink_center(doc)?;
    if (before - w / 2.0).abs() > 0.012 * w {
        return Ok(false);
    }
    let Some(Param::Vector(pos)) = doc.nodes[id].params.get("position").cloned() else {
        return Ok(false);
    };
    let mut d = doc.clone();
    let n = d.nodes.get_mut(id).unwrap();
    n.params.insert("align".into(), Param::Text("center".into()));
    n.params.insert("position".into(), Param::Vector(vec![w / 2.0, pos[1]]));
    let d = fresh(&d)?;
    // Keep the ink where it was: the advance box and the ink box differ by
    // the side bearings.
    let shift = before - ink_center(&d)?;
    let x = w / 2.0 + shift;
    let x = if (x - w / 2.0).abs() <= 1.0 {
        w / 2.0
    } else {
        (x * 100.0).round() / 100.0
    };
    let mut d = d;
    d.nodes
        .get_mut(id)
        .unwrap()
        .params
        .insert("position".into(), Param::Vector(vec![x, pos[1]]));
    *doc = fresh(&d)?;
    Ok(true)
}

/// Keyframes as JSON (times and values rounded for reading).
fn keys_json(keys: &[(f64, Vec<f64>, Option<ultrafast_graph::Ease>)]) -> Vec<Value> {
    keys.iter()
        .map(|(t, v, ease)| {
            let v: Vec<f64> = v.iter().map(|x| (x * 100.0).round() / 100.0).collect();
            let mut k = json!({ "t": (t * 1000.0).round() / 1000.0, "v": v });
            if let Some(e) = ease {
                k["ease"] = round_json(serde_json::to_value(e).unwrap());
            }
            k
        })
        .collect()
}

/// Rounds every number in `v` to 3 decimals (bezier handles read better).
fn round_json(v: Value) -> Value {
    match v {
        Value::Number(n) => json!((n.as_f64().unwrap_or(0.0) * 1000.0).round() / 1000.0),
        Value::Array(a) => Value::Array(a.into_iter().map(round_json).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, round_json(v))).collect()),
        v => v,
    }
}

fn fresh(doc: &Document) -> Result<Document, String> {
    Document::from_json(&doc.to_json()).map_err(|e| e.to_string())
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Renders `node` of `doc` alone at `t` (its own output, RGBA8).
fn render_node(device: &Device, doc: &Document, base: &Path, node: &str, t: f64) -> Result<Vec<u8>, String> {
    let mut d = doc.clone();
    d.output = node.to_string();
    let d = fresh(&d)?;
    let mut r = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    r.set_base_dir(base);
    r.set_mode(Mode::Exact);
    Ok(r.render_rgba8(&d, t).map_err(|e| e.to_string())?.0)
}

/// Builds an editable document recreating `video`; writes plates into
/// `out_dir` (the document's base directory).
pub fn reconstruct(
    device: &Device,
    video: &Path,
    out_dir: &Path,
    opts: &ReconstructOptions,
    mut log: impl FnMut(&str),
) -> Result<(Document, Value), String> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let video = std::fs::canonicalize(video).map_err(|e| format!("{}: {e}", video.display()))?;
    log("probing shots");
    let probed = probe(device, &video, None)?;
    let (w, h, fps, duration) = (
        probed["width"].as_u64().unwrap() as u32,
        probed["height"].as_u64().unwrap() as u32,
        probed["fps"].as_f64().unwrap(),
        probed["duration"].as_f64().unwrap(),
    );
    let shots: Vec<(f64, f64)> = probed["shots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["start"].as_f64().unwrap(), s["end"].as_f64().unwrap()))
        .collect();
    let mut reference = Reference::open(device, &video, None, None)?;

    // Skeleton: per shot a background and one layer per recognized line.
    let mut nodes = Map::new();
    let mut shot_texts: Vec<Vec<String>> = Vec::new();
    // Each shot's own top layer (its layers over its background).
    let mut shot_tops: Vec<String> = Vec::new();
    let mut prev_root: Option<String> = None;
    for (k, &(start, end)) in shots.iter().enumerate() {
        let k = k + 1;
        let mid = (start + end) / 2.0;
        log(&format!("shot {k}: {start:.2}..{end:.2}s, recognizing text"));
        let frame = reference.frame(mid)?;
        let boxes = ultrafast_media::vision::recognize_text(&video, mid).map_err(|e| e.to_string())?;
        let bg_id = format!("s{k}_bg");
        nodes.insert(
            bg_id.clone(),
            json!({ "op": "solid", "params": { "color": hex(frame.border_color()) } }),
        );
        let mut under = bg_id;
        let mut texts = Vec::new();
        for (i, b) in boxes.iter().filter(|b| b.confidence >= opts.min_confidence).enumerate() {
            let i = i + 1;
            let (fg, _) = frame.two_colors(b.rect);
            let id = format!("s{k}_text{i}");
            nodes.insert(
                id.clone(),
                json!({ "op": "text", "params": {
                    "text": b.text, "size": (b.rect[3] * 0.95).max(4.0).round(), "color": hex(fg),
                    "position": [b.rect[0].round(), (b.rect[1] + b.rect[3] / 2.0).round()], "valign": "middle"
                } }),
            );
            let layer = format!("s{k}_layer{i}");
            nodes.insert(
                layer.clone(),
                json!({ "op": "over", "inputs": { "fg": id, "bg": under } }),
            );
            under = layer;
            texts.push(id);
        }
        shot_tops.push(under.clone());
        // Each later shot goes over the earlier ones inside its window (an
        // inactive `over` passes its background through).
        let stacked = match prev_root {
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
        prev_root = Some(stacked);
        shot_texts.push(texts);
    }
    let mut doc = Document::from_json(
        &json!({
            "version": 1,
            "format": { "width": w, "height": h, "fps": fps, "duration": duration },
            "output": prev_root.unwrap(),
            "nodes": nodes,
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    doc.validate().map_err(|e| e.to_string())?;

    let mut shot_reports = Vec::new();
    for (k, &(start, end)) in shots.iter().enumerate() {
        let k1 = k + 1;
        let mid = (start + end) / 2.0;
        let mut layers = Vec::new();

        // Fonts.
        for id in &shot_texts[k] {
            log(&format!("shot {k1}: matching the font of {id}"));
            let result =
                match_font(device, &doc, out_dir, &video, id, mid, &MatchOptions::default()).and_then(|(d, report)| {
                    // No installed face drew it convincingly: usually OCR
                    // picking up texture, not typeset text.
                    let q = report["best"]["coverage_psnr"].as_f64().unwrap_or(0.0);
                    if q < MIN_GLYPH_MATCH {
                        Err(format!("no font matches its glyphs (best {q:.1} dB)"))
                    } else {
                        Ok((d, report))
                    }
                });
            match result {
                Ok((d, report)) => {
                    doc = fresh(&d)?;
                    let centered = center_if_centered(device, &mut doc, out_dir, id, mid)?;
                    layers.push(json!({ "node": id, "font": report["best"], "centered": centered }));
                }
                Err(e) => {
                    log(&format!("shot {k1}: {id}: {e}; dropping it"));
                    doc = drop_layer(&doc, id, &mut shot_tops[k])?;
                    layers.push(json!({ "node": id, "dropped": e }));
                }
            }
        }

        // Background.
        log(&format!("shot {k1}: building the background plate"));
        let bg_id = format!("s{k1}_bg");
        let image = out_dir.join(format!("s{k1}_plate.png"));
        let popts = PlateOptions {
            from: start,
            to: end,
            samples: opts.plate_samples,
            background: Some(bg_id.clone()),
            ..Default::default()
        };
        // Only this shot's layers mark what the plate must fill.
        let mut shot_doc = doc.clone();
        shot_doc.output = shot_tops[k].clone();
        let shot_doc = fresh(&shot_doc)?;
        let (plated, preport) = plate(device, &video, Some((&shot_doc, out_dir)), &image, &popts)?;
        let still = preport["frames_psnr"].as_f64().unwrap_or(0.0) >= opts.still_threshold;
        if still {
            let bg = plated.unwrap().nodes[&bg_id].clone();
            doc.nodes.insert(bg_id.clone(), bg);
            doc = fresh(&doc)?;
        } else {
            // Moving footage: the reference itself is the background layer.
            let node: ultrafast_graph::Node = serde_json::from_value(
                json!({ "op": "video", "params": { "src": video.to_string_lossy(), "fit": "fill" } }),
            )
            .unwrap();
            doc.nodes.insert(bg_id.clone(), node);
            doc = fresh(&doc)?;
        }
        let background = if still {
            preport["recommended"].clone()
        } else {
            json!("footage")
        };

        // Over footage, text that changes during the shot belongs to the
        // footage (signs, timecodes), not to an overlay: drop it.
        if !still {
            let probes: Vec<Frame> = (0..8)
                .map(|i| reference.frame(start + (end - start) * (i as f64 + 0.5) / 8.0))
                .collect::<Result<_, _>>()?;
            for id in shot_texts[k].clone() {
                if !doc.nodes.contains_key(&id) {
                    continue;
                }
                let alpha = render_node(device, &doc, out_dir, &id, mid)?;
                let ink: Vec<usize> = (0..alpha.len() / 4).filter(|&i| alpha[i * 4 + 3] > 128).collect();
                let mut change = 0.0f64;
                for f in &probes[1..] {
                    let d: f64 = ink
                        .iter()
                        .map(|&i| {
                            (0..3)
                                .map(|c| (f.rgba[i * 4 + c] as f64 - probes[0].rgba[i * 4 + c] as f64).abs())
                                .sum::<f64>()
                        })
                        .sum();
                    change = change.max(d / (ink.len().max(1) * 3) as f64);
                }
                if change > 10.0 {
                    log(&format!(
                        "shot {k1}: {id} changes during the shot; it is part of the footage"
                    ));
                    doc = drop_layer(&doc, &id, &mut shot_tops[k])?;
                    if let Some(l) = layers.iter_mut().find(|v| v["node"] == json!(id)) {
                        l["dropped"] = json!(format!(
                            "changes during the shot (mean change {change:.0}/255): part of the footage"
                        ));
                    }
                }
            }
        }

        // Each layer tracked over the shot against its background: per frame
        // an offset and a presence (opacity), which become position and
        // opacity keyframes where they change. (Footage shots keep their
        // layers static: the footage already carries what they do.)
        let texts: Vec<String> = shot_texts[k]
            .iter()
            .filter(|id| doc.nodes.contains_key(*id))
            .cloned()
            .collect();
        if !texts.is_empty() && still {
            log(&format!("shot {k1}: tracking layers"));
            let bg = render_node(device, &doc, out_dir, &bg_id, mid)?;
            let mut trackers: Vec<(String, Tracker)> = Vec::new();
            for id in &texts {
                let px = render_node(device, &doc, out_dir, id, mid)?;
                let fg = match doc.nodes[id].params.get("color") {
                    Some(Param::Text(s)) => ultrafast_render::ops::parse_hex(s).unwrap_or([1.0; 4]),
                    _ => [1.0; 4],
                };
                let fg = [0, 1, 2].map(|c| srgb_to_linear(fg[c]) as f32);
                trackers.push((id.clone(), Tracker::new(&px, &bg, fg, w as usize, h as usize)));
            }
            // Track outward from the matched frame, where each layer sits
            // exactly where it was rendered: forward to the end, then
            // backward to the start.
            let n_frames = ((end - start) * fps).round().max(1.0) as usize;
            let stride = n_frames.div_ceil(opts.presence_frames).max(1);
            let frames: Vec<usize> = (0..n_frames).step_by(stride).collect();
            let split = frames.partition_point(|&fi| start + (fi as f64 + 0.5) / fps < mid);
            let passes: [Vec<usize>; 2] = [
                frames[split..].to_vec(),
                frames[..split].iter().rev().copied().collect(),
            ];
            for order in passes {
                for (_, tr) in &mut trackers {
                    tr.seed([0.0, 0.0]);
                }
                for fi in order {
                    let t = start + (fi as f64 + 0.5) / fps;
                    let f = reference.frame(t)?;
                    for (_, tr) in &mut trackers {
                        tr.step(&f, t);
                    }
                }
            }
            for (_, tr) in &mut trackers {
                tr.finish();
            }
            for (id, tr) in &trackers {
                // Snap near-full and near-empty: color and coverage are
                // estimates, so a fully shown layer measures a bit under 1.
                let snap = |p: f64| {
                    if p > 0.92 {
                        1.0
                    } else if p < 0.05 {
                        0.0
                    } else {
                        (p * 1000.0).round() / 1000.0
                    }
                };
                let presence: Vec<(f64, Vec<f64>)> = tr.samples.iter().map(|s| (s.t, vec![snap(s.presence)])).collect();
                let visible: Vec<&Sample> = tr.samples.iter().filter(|s| s.presence >= 0.05).collect();
                let max_offset = visible
                    .iter()
                    .map(|s| s.offset[0].abs().max(s.offset[1].abs()))
                    .fold(0.0, f64::max);
                let residual = {
                    let mut r: Vec<f64> = visible.iter().map(|s| s.residual).collect();
                    r.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    r.get(r.len() / 2).copied().unwrap_or(0.0)
                };
                let moving = max_offset > 0.75;
                let mut entry =
                    json!({ "node": id, "residual": (residual * 1000.0).round() / 1000.0, "moving": moving });
                if !presence.iter().all(|s| s.1[0] >= 1.0) {
                    let keys = keys_json(&simplify_eased(&presence, 0.05));
                    let layer = id.replace("_text", "_layer");
                    if let Some(n) = doc.nodes.get_mut(&layer) {
                        n.params.insert(
                            "opacity".into(),
                            serde_json::from_value(json!({ "keys": keys })).map_err(|e| e.to_string())?,
                        );
                    }
                    entry["opacity_keys"] = json!(keys);
                }
                if moving && let Some(Param::Vector(base)) = doc.nodes[id].params.get("position").cloned() {
                    // Offsets of faint frames are noisy; the path comes
                    // from frames where the layer clearly shows.
                    let path: Vec<(f64, Vec<f64>)> = visible
                        .iter()
                        .filter(|s| s.presence >= 0.3)
                        .map(|s| (s.t, vec![base[0] + s.offset[0], base[1] + s.offset[1]]))
                        .collect();
                    let keys = keys_json(&simplify_eased(&path, 0.75));
                    doc.nodes.get_mut(id).unwrap().params.insert(
                        "position".into(),
                        serde_json::from_value(json!({ "keys": keys })).map_err(|e| e.to_string())?,
                    );
                    entry["position_keys"] = json!(keys);
                }
                if let Some(layer) = layers.iter_mut().find(|v| v["node"] == json!(id)) {
                    layer["presence"] = entry;
                }
            }
            doc = fresh(&doc)?;
        }
        shot_reports.push(json!({
            "shot": k1, "start": start, "end": end, "background": background,
            "plate": { "frames_psnr": preport["frames_psnr"], "models": preport["models"].as_array().map(|m| m.iter().take(3).map(|m| json!({ "model": m["model"], "psnr": m["psnr"] })).collect::<Vec<_>>()) },
            "layers": layers,
        }));
    }

    // Score the whole document against the reference.
    log("comparing against the reference");
    let cmp = crate::compare(
        device,
        &doc,
        out_dir,
        &video,
        &crate::CompareOptions {
            times: (0..((duration * fps).floor() as usize))
                .step_by(((duration * fps) as usize / 60).max(1))
                .map(|f| (f as f64 + 0.5) / fps)
                .collect(),
            out_dir: Some(out_dir.join("diffs")),
            worst: 3,
            offset: 0.0,
        },
    )?;
    Ok((
        doc,
        json!({
            "video": video,
            "shots": shot_reports,
            "mean_psnr": cmp["mean_psnr"],
            "min_psnr": cmp["min_psnr"],
            "worst": cmp["worst"],
        }),
    ))
}
