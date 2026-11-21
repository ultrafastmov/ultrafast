//! What part of each media input a render uses, from the document alone
//! (before anything is fetched): for video, the source times its frames
//! show, by the renderer's own `video_time` at every frame (and around it
//! for motion blur); for sound, the span the mixer plays, by the mixer's
//! own arithmetic. Everything else is used whole.

use std::collections::HashMap;
use ultrafast_graph::{Document, Param, resolve_chain};

/// How a render uses one reference.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Source-time windows `(from, to)` in seconds.
    pub windows: Vec<(f64, f64)>,
    /// Read the whole file (an image, a font, a looping clip...).
    pub whole: bool,
}

/// Usage per param string (the references as the document spells them)
/// for the document's seconds `[from, to]`.
pub fn usage(doc: &Document, from: f64, to: f64) -> Result<HashMap<String, Usage>, String> {
    let flat;
    let (nodes, chains) = if doc.needs_flatten() {
        flat = doc.flatten().map_err(|e| e.to_string())?;
        (&flat.doc.nodes, Some(&flat.chains))
    } else {
        (&doc.nodes, None)
    };
    let fps = doc.format.fps.max(1.0);
    // Frame times, and half a frame either side when motion blur samples
    // the shutter.
    let blur = doc.format.motion_blur.is_some();
    let mut times = Vec::new();
    let mut k = 0u64;
    loop {
        let t = from + k as f64 / fps;
        if t > to + 1e-9 {
            break;
        }
        times.push(t);
        if blur {
            times.push(t - 0.5 / fps);
            times.push(t + 0.5 / fps);
        }
        k += 1;
    }
    times.push(to);
    let mut out: HashMap<String, Usage> = HashMap::new();
    let mut add = |src: &str, w: Option<(f64, f64)>| {
        let u = out.entry(src.to_string()).or_default();
        match w {
            Some(w) => u.windows.push(w),
            None => u.whole = true,
        }
    };
    for (id, node) in nodes {
        let text = |k: &str| match node.params.get(k) {
            Some(Param::Text(s)) => Some(s.as_str()),
            _ => None,
        };
        let chain = chains.and_then(|c| c.get(id)).cloned().unwrap_or_default();
        match node.op.as_str() {
            "video" | "clip_camera" => {
                let Some(src) = text("src") else { continue };
                if matches!(node.params.get("loop"), Some(Param::Bool(true))) {
                    add(src, None);
                    continue;
                }
                let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                for &t in &times {
                    let Some(local) = resolve_chain(&chain, t) else {
                        continue;
                    };
                    let tau = if node.op == "video" {
                        ultrafast_render::ops::video_time(node, local, f64::INFINITY)
                    } else {
                        // clip_camera: `start` plus local time.
                        let start = node
                            .params
                            .get("start")
                            .and_then(|p| p.eval(local))
                            .and_then(|v| v.first().copied())
                            .unwrap_or(0.0);
                        (start + local).max(0.0)
                    };
                    if tau.is_finite() {
                        lo = lo.min(tau);
                        hi = hi.max(tau);
                    }
                }
                if lo <= hi {
                    add(src, Some((lo, hi)));
                }
            }
            "audio" => {
                let Some(src) = text("src") else { continue };
                let rate = node
                    .params
                    .get("rate")
                    .and_then(|p| p.eval(0.0))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(1.0)
                    .clamp(0.01, 100.0);
                let r = ultrafast_media::audio::RATE as f64;
                let (a, b) = ((from * r).round() as usize, (to * r).round() as usize);
                if let Some(w) = ultrafast_live::audio_mix::played_span(node, &chain, rate, a, b) {
                    add(src, Some(w));
                }
            }
            _ => {
                for v in node.params.values() {
                    if let Param::Text(s) = v
                        && crate::refs::looks_like_ref(s)
                    {
                        add(s, None);
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_and_audio_windows() {
        let doc = Document::from_json(
            &serde_json::json!({
                "version": 1,
                "format": {"width": 64, "height": 64, "fps": 10, "duration": 4},
                "nodes": {
                    "v": {"op": "video", "params": {"src": "gs://b/long.mp4#1", "start": 3600}, "timing": {"in": 1, "offset": 1}},
                    "a": {"op": "audio", "params": {"src": "gs://b/long.mp4#1", "start": 3600, "from": 1}},
                    "i": {"op": "image", "params": {"src": "gs://b/logo.png#2"}},
                    "o": {"op": "over", "inputs": {"fg": "i", "bg": "v"}}
                },
                "output": "o"
            })
            .to_string(),
        )
        .unwrap();
        let u = usage(&doc, 0.0, 4.0).unwrap();
        let long = &u["gs://b/long.mp4#1"];
        assert!(!long.whole);
        // The video shows local 0..3 s from t=1 (source 3600..3603); the
        // sound plays from local 1 s (source 3600..3603) too.
        assert_eq!(long.windows.len(), 2);
        for &(a, b) in &long.windows {
            assert!((a - 3600.0).abs() < 1e-3 && (b - 3603.0).abs() < 1e-3, "{a} {b}");
        }
        assert!(u["gs://b/logo.png#2"].whole);
    }
}
