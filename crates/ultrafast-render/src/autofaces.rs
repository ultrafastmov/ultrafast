//! Face tracks for `clip_camera` from the footage itself, when a document
//! gives no `faces`: the clip is decoded at ten frames a second through
//! the same decoder the `video` op uses, and [`ultrafast_framing::faces`]
//! finds, tracks and scores the faces. Results are cached per file and
//! range in the user cache (`.../ultrafast/faces/`), so a clip is analysed
//! once.

use crate::{Mode, Renderer};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use ultrafast_framing::faces::{Gray, Sample, VERSION, analyze};
use ultrafast_gpu::Device;
use ultrafast_graph::Document;

/// Frames a second the clip is analysed at.
pub const RATE: f64 = 10.0;
/// Longest side of the analysed frames, px.
const SIZE: u32 = 960;

fn cache_path(path: &Path, from: f64, to: f64) -> Option<PathBuf> {
    let abs = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&abs).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let key = format!(
        "{}|{}|{mtime}|{from:.3}|{to:.3}|{VERSION}|{SIZE}",
        abs.display(),
        meta.len()
    );
    let dir = crate::fontfetch::cache_dir().parent()?.join("faces");
    Some(dir.join(format!("{}.json", &blake3::hash(key.as_bytes()).to_hex()[..32])))
}

/// Face tracks for source seconds `[from, to]` of `path` (the whole file
/// when `to` is past its end), as a face worker's payload. Cached.
pub fn track(device: &Device, path: &Path, from: f64, to: f64) -> Result<Value, String> {
    let cache = cache_path(path, from, to);
    if let Some(c) = &cache
        && let Ok(text) = std::fs::read_to_string(c)
        && let Ok(v) = serde_json::from_str::<Value>(&text)
    {
        return Ok(v);
    }
    let v = track_uncached(device, path, from, to)?;
    if let Some(c) = &cache {
        let _ = std::fs::create_dir_all(c.parent().unwrap());
        let tmp = c.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, c);
        }
    }
    Ok(v)
}

fn track_uncached(device: &Device, path: &Path, from: f64, to: f64) -> Result<Value, String> {
    let info = ultrafast_media::video_info(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let abs = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let s = (SIZE as f64 / info.width.max(info.height) as f64).min(1.0);
    let even = |v: f64| ((v * s / 2.0).round() as u32 * 2).max(2);
    let (w, h) = (even(info.width as f64), even(info.height as f64));
    let doc = Document::from_json(
        &json!({
            "version": 1,
            "format": { "width": w, "height": h, "fps": RATE, "duration": info.duration.max(0.1) },
            "output": "v",
            "nodes": { "v": { "op": "video", "params": { "src": abs.to_string_lossy(), "fit": "fill" } } }
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    let mut r = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    r.set_mode(Mode::Exact);
    r.set_sequential(true);
    let from = from.clamp(0.0, info.duration);
    let to = to.clamp(from, info.duration);
    let mut samples = Vec::new();
    let mut k = 0;
    loop {
        let t = from + k as f64 / RATE;
        // The last whole frame before the end.
        if t > to + 1e-9 || t > info.duration - 1.0 / info.fps.max(1.0) {
            break;
        }
        let (px, _) = r.render_rgba8(&doc, t).map_err(|e| e.to_string())?;
        samples.push(Sample {
            t,
            frame: Gray::from_rgba(w as usize, h as usize, &px),
        });
        k += 1;
    }
    Ok(analyze(&samples, info.fps, info.width as f64, info.height as f64))
}
