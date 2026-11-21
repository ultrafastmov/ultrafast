//! Mixes a document's `audio` nodes for export: each node plays its source
//! from `start` (seconds into the file) at local time `from`, for `duration`
//! seconds (0: to the end), at `rate` (time stretched, pitch kept), inside
//! its timing windows, scaled by `gain` (animatable, linear).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use ultrafast_graph::{Document, Node, Timing, resolve_chain};
use ultrafast_media::audio::{CHANNELS, Pcm, RATE};

struct Track {
    /// The source decoded at `rate` (time stretched), from `offset` on.
    pcm: Arc<Pcm>,
    rate: f64,
    node: Node,
    chain: Vec<Timing>,
    /// Frame of the whole time-stretched source that `pcm` starts at (0
    /// unless only a span was decoded).
    offset: i64,
}

pub struct AudioMix {
    tracks: Vec<Track>,
}

/// Mix block size: timing and gain are evaluated per block.
const BLOCK: usize = 256;

/// Seconds decoded either side of what a span plays.
const SPAN_MARGIN: f64 = 0.5;

impl AudioMix {
    /// Collects and decodes the document's audio. None if it has none.
    pub fn from_doc(doc: &Document, base_dir: &Path) -> Result<Option<Self>, String> {
        Self::from_doc_range(doc, base_dir, None)
    }

    /// Like [`from_doc`](Self::from_doc), decoding of each source only what
    /// output frames `[a, b)` (48 kHz) play: a minute of a long recording
    /// decodes that minute, not the recording. Sources an export plays from
    /// their start, and that are not much longer, decode whole as before.
    pub fn from_doc_range(
        doc: &Document,
        base_dir: &Path,
        range: Option<(usize, usize)>,
    ) -> Result<Option<Self>, String> {
        Self::collect(doc, base_dir, range, |path, rate, span| {
            match span {
                Some((start, end)) => ultrafast_media::audio::decode_span(path, rate, start, end),
                None => ultrafast_media::audio::decode_rate(path, rate),
            }
            .map(|p| p.map(Arc::new))
            .map_err(|e| e.to_string())
        })
    }

    /// Like [`from_doc`](Self::from_doc), with the caller supplying decoded
    /// sources at a playback rate (the live preview keeps them cached across
    /// edits).
    pub fn from_doc_with(
        doc: &Document,
        base_dir: &Path,
        mut decode: impl FnMut(&Path, f64) -> Result<Option<Arc<Pcm>>, String>,
    ) -> Result<Option<Self>, String> {
        Self::collect(doc, base_dir, None, |path, rate, _| decode(path, rate))
    }

    fn collect(
        doc: &Document,
        base_dir: &Path,
        range: Option<(usize, usize)>,
        mut decode: impl FnMut(&Path, f64, Option<(f64, f64)>) -> Result<Option<Arc<Pcm>>, String>,
    ) -> Result<Option<Self>, String> {
        let flat;
        let (nodes, chains) = if doc.needs_flatten() {
            flat = doc.flatten().map_err(|e| e.to_string())?;
            (&flat.doc.nodes, Some(&flat.chains))
        } else {
            (&doc.nodes, None)
        };
        // Each node's source, rate, timing chain and (with a range) the
        // source seconds it plays; the sources' spans merged per rate.
        let mut wanted = Vec::new();
        let mut spans: HashMap<String, (f64, f64)> = HashMap::new();
        for (id, node) in nodes {
            if node.op != "audio" {
                continue;
            }
            let Some(src) = ultrafast_render::ops::text(node, "src") else {
                return Err(format!("audio node `{id}` needs `src`"));
            };
            let rate = node
                .params
                .get("rate")
                .and_then(|p| p.eval(0.0))
                .and_then(|v| v.first().copied())
                .unwrap_or(1.0)
                .clamp(0.01, 100.0);
            let chain = chains.and_then(|c| c.get(id)).cloned().unwrap_or_default();
            let key = format!("{src}\u{1f}{rate}");
            if let Some((a, b)) = range {
                // Nothing of it plays: nothing to decode.
                let Some((s0, s1)) = played_span(node, &chain, rate, a, b) else {
                    continue;
                };
                let e = spans.entry(key.clone()).or_insert((s0, s1));
                *e = (e.0.min(s0), e.1.max(s1));
            }
            wanted.push((id, node, src, rate, chain, key));
        }
        let mut decoded: HashMap<String, (Arc<Pcm>, i64)> = HashMap::new();
        let mut silent: std::collections::HashSet<String> = Default::default();
        let mut tracks = Vec::new();
        for (id, node, src, rate, chain, key) in wanted {
            if silent.contains(&key) {
                continue;
            }
            let (pcm, offset) = match decoded.get(&key) {
                Some(p) => p.clone(),
                None => {
                    let path = ultrafast_render::media_path(base_dir, src);
                    let span = spans.get(&key).and_then(|&(s0, s1)| {
                        let (mut w0, w1) = ((s0 - SPAN_MARGIN).max(0.0), s1 + SPAN_MARGIN);
                        if w0 < 1.0 {
                            w0 = 0.0;
                        }
                        // From the start: a span only if it saves decoding
                        // much of the file (else whole, exactly as before).
                        let saves = w0 > 0.0 || ultrafast_media::audio::duration(&path).is_some_and(|d| d > w1 + 30.0);
                        saves.then_some((w0, w1))
                    });
                    // A source without sound (a silent clip) plays silence.
                    let Some(pcm) = decode(&path, rate, span).map_err(|e| format!("audio node `{id}`: {e}"))? else {
                        silent.insert(key);
                        continue;
                    };
                    let offset = span.map_or(0, |(w0, _)| (w0 / rate * RATE as f64).round() as i64);
                    decoded.insert(key, (pcm.clone(), offset));
                    (pcm, offset)
                }
            };
            tracks.push(Track {
                pcm,
                rate,
                node: node.clone(),
                chain,
                offset,
            });
        }
        Ok((!tracks.is_empty()).then_some(Self { tracks }))
    }

    /// Output frames `[from, from + frames)` (48 kHz) as interleaved stereo.
    pub fn render(&self, from: usize, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * CHANNELS];
        self.render_into(from, &mut out);
        out
    }

    /// Mixes output frames starting at `from` into `out` (interleaved stereo,
    /// overwritten). Does not allocate, so it can run on an audio thread.
    pub fn render_into(&self, from: usize, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / CHANNELS;
        for track in &self.tracks {
            let mut k = 0;
            while k < frames {
                let n = BLOCK.min(frames - k);
                let t = (from + k) as f64 / RATE as f64;
                if let Some(local) = resolve_chain(&track.chain, t) {
                    let num = |name: &str, d: f64| {
                        track
                            .node
                            .params
                            .get(name)
                            .and_then(|p| p.eval(local))
                            .and_then(|v| v.first().copied())
                            .unwrap_or(d)
                    };
                    let rate = RATE as f64;
                    let begin = num("from", 0.0);
                    let duration = num("duration", 0.0);
                    // This block's frames inside the sound's window (the
                    // first frame at or after an edge, float noise aside).
                    let frame_at = |s: f64| ((s * rate - 1e-6).ceil().max(0.0) as usize).min(n);
                    let mut lo = frame_at(begin - local);
                    let hi = if duration > 0.0 {
                        frame_at(begin + duration - local)
                    } else {
                        n
                    };
                    // Frame of the (time-stretched) source at the block's
                    // first frame; nothing plays before the source starts.
                    let pos = ((num("start", 0.0) / track.rate + local - begin) * rate).round() as i64 - track.offset;
                    lo = lo.max((-pos).max(0) as usize);
                    let src = &track.pcm;
                    if lo < hi && ((pos + lo as i64) as usize) < src.frames() {
                        let gain = num("gain", 1.0) as f32;
                        let s0 = (pos + lo as i64) as usize;
                        let len = (hi - lo).min(src.frames() - s0);
                        let dst = &mut out[(k + lo) * CHANNELS..(k + lo + len) * CHANNELS];
                        let from = &src.samples[s0 * CHANNELS..(s0 + len) * CHANNELS];
                        for (d, s) in dst.iter_mut().zip(from) {
                            *d += s * gain;
                        }
                    }
                }
                k += n;
            }
        }
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }
}

/// Source seconds `(start, end)` (before the time stretch) of an `audio`
/// node that output frames `[a, b)` (48 kHz) play, by the same arithmetic
/// as [`AudioMix::render_into`]; None if they play none of it.
pub fn played_span(node: &Node, chain: &[Timing], rate: f64, a: usize, b: usize) -> Option<(f64, f64)> {
    let r = RATE as f64;
    let (mut s0, mut s1) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut k = a;
    while k < b {
        let n = BLOCK.min(b - k);
        if let Some(local) = resolve_chain(chain, k as f64 / r) {
            let num = |name: &str, d: f64| {
                node.params
                    .get(name)
                    .and_then(|p| p.eval(local))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(d)
            };
            let begin = num("from", 0.0);
            let duration = num("duration", 0.0);
            let frame_at = |s: f64| ((s * r - 1e-6).ceil().max(0.0) as usize).min(n);
            let mut lo = frame_at(begin - local);
            let hi = if duration > 0.0 {
                frame_at(begin + duration - local)
            } else {
                n
            };
            let pos = ((num("start", 0.0) / rate + local - begin) * r).round() as i64;
            lo = lo.max((-pos).max(0) as usize);
            if lo < hi {
                s0 = s0.min((pos + lo as i64) as f64 / r * rate);
                s1 = s1.max((pos + hi as i64) as f64 / r * rate);
            }
        }
        k += n;
    }
    (s0 <= s1).then_some((s0, s1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 s source whose samples count up, mixed by one `audio` node.
    fn mix(params: serde_json::Value) -> Vec<f32> {
        let doc = Document::from_json(
            &serde_json::json!({
                "version": 1,
                "format": {"width": 16, "height": 16, "fps": 30, "duration": 2},
                "nodes": {"a": {"op": "audio", "params": params}, "s": {"op": "solid"}},
                "output": "s"
            })
            .to_string(),
        )
        .unwrap();
        let samples = (0..RATE as usize).flat_map(|i| [i as f32 / 1e6; CHANNELS]).collect();
        let pcm = Arc::new(Pcm { samples });
        let m = AudioMix::from_doc_with(&doc, Path::new("."), |_, _| Ok(Some(pcm.clone())))
            .unwrap()
            .unwrap();
        m.render(0, 2 * RATE as usize).chunks(CHANNELS).map(|c| c[0]).collect()
    }

    #[test]
    fn window_offset_and_gain() {
        // Starts at 0.5 s from 0.25 s into the source, plays 0.1 s, half gain.
        let out = mix(serde_json::json!({"src": "x", "from": 0.5, "start": 0.25, "duration": 0.1, "gain": 0.5}));
        let at = |s: f64| out[(s * RATE as f64) as usize];
        assert_eq!(at(0.499), 0.0);
        assert!((at(0.5) - 0.5 * 12_000.0 / 1e6).abs() < 1e-6);
        assert!((at(0.55) - 0.5 * 14_400.0 / 1e6).abs() < 1e-6);
        assert_eq!(at(0.6), 0.0);
        // Without a duration it plays to the source's end (0.75 s later).
        let out = mix(serde_json::json!({"src": "x", "from": 0.5, "start": 0.25}));
        assert!(out[(1.249 * RATE as f64) as usize] > 0.0);
        assert_eq!(out[(1.26 * RATE as f64) as usize], 0.0);
    }

    #[test]
    fn played_span_follows_timing_and_rate() {
        let node: Node = serde_json::from_value(serde_json::json!({
            "op": "audio", "params": {"src": "x", "start": 60, "from": 1, "duration": 2, "rate": 2}
        }))
        .unwrap();
        let r = RATE as usize;
        // Output 0..4 s plays local 1..3 s: source 60 + 2 * (0..2) s.
        let (s0, s1) = played_span(&node, &[], 2.0, 0, 4 * r).unwrap();
        assert!((s0 - 60.0).abs() < 1e-3 && (s1 - 64.0).abs() < 1e-3, "{s0} {s1}");
        // Shifted by a timing offset of 10 s: output 10.5..11.5 s is local 0.5..1.5.
        let chain = [Timing {
            offset: 10.0,
            ..Default::default()
        }];
        let (s0, s1) = played_span(&node, &chain, 2.0, 10 * r + r / 2, 11 * r + r / 2).unwrap();
        assert!((s0 - 60.0).abs() < 1e-3 && (s1 - 61.0).abs() < 1e-3, "{s0} {s1}");
        // Nothing plays before `from`.
        assert!(played_span(&node, &[], 2.0, 0, r / 2).is_none());
    }

    /// A chapter deep into a long recording decodes only its span, and mixes
    /// exactly what decoding the whole recording mixes.
    #[test]
    fn span_decode_matches_whole_decode() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipped: no ffmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ultrafast-audio-span-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 120 s of a ramp that never repeats within a second's error.
        let n = 120 * RATE as usize;
        let pcm = Pcm {
            samples: (0..n)
                .flat_map(|i| [((i % 40_000) as f32 / 40_000.0) - 0.5; CHANNELS])
                .collect(),
        };
        ultrafast_media::audio::write_wav(&dir.join("long.wav"), &pcm).unwrap();
        let doc = Document::from_json(
            &serde_json::json!({
                "version": 1,
                "format": {"width": 16, "height": 16, "fps": 30, "duration": 3},
                "nodes": {"a": {"op": "audio", "params": {"src": "long.wav", "start": 90.25, "from": 0.5}},
                          "s": {"op": "solid"}},
                "output": "s"
            })
            .to_string(),
        )
        .unwrap();
        let r = RATE as usize;
        let span = AudioMix::from_doc_range(&doc, &dir, Some((0, 3 * r))).unwrap().unwrap();
        assert!(span.tracks[0].offset > 0, "decoded a span");
        assert!(
            span.tracks[0].pcm.duration() < 5.0,
            "decoded {} s",
            span.tracks[0].pcm.duration()
        );
        let whole = AudioMix::from_doc(&doc, &dir).unwrap().unwrap();
        let (a, b) = (span.render(0, 3 * r), whole.render(0, 3 * r));
        // A source without sound decodes to nothing either way.
        let silent = dir.join("silent.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:size=16x16:rate=30",
                "-t",
                "2",
            ])
            .arg(&silent)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            assert!(
                ultrafast_media::audio::decode_span(&silent, 1.0, 0.5, 1.5)
                    .unwrap()
                    .is_none()
            );
        }
        let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "worst sample difference {worst}");
        assert!(a[r * CHANNELS] != 0.0, "plays after `from`");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
