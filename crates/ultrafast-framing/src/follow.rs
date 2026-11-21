//! `clip_camera`: a video framed on its speaker from face tracks (a face
//! worker's, or [`crate::faces`] run on the footage when none are given).
//! The camera (subject choice, anchored easing, cuts, scope zoom,
//! protect mode) comes from `ultrafast-framing` and is computed once per clip;
//! the node expands into ordinary video nodes whose `rect` is keyed per
//! frame, so everything downstream (effects, masks, motion blur, live edits
//! of the rest of the document) treats it like any other footage.

use crate::{CameraFrame, Clip, Config, Faces, Rect, camera_track, camera_track_subjects};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use ultrafast_graph::{Component, Document, Ease, Format, Key, Node, NodeId, Param, Timing};

/// Params the expanded component keeps (the rest drive the build).
pub const PARAMS: &[&str] = &["opacity"];

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "clip_camera")
}

fn text(node: &Node, k: &str) -> Option<String> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn num(node: &Node, k: &str) -> Option<f64> {
    node.params
        .get(k)
        .and_then(|p| p.eval(0.0))
        .and_then(|v| v.first().copied())
}

fn resolve(base: &Path, p: &str) -> std::path::PathBuf {
    if Path::new(p).is_absolute() {
        Path::new(p).to_path_buf()
    } else {
        base.join(p)
    }
}

/// Whether a `faces` param holds the tracks themselves (JSON text) rather
/// than naming a file.
pub fn faces_inline(s: &str) -> bool {
    matches!(s.trim_start().as_bytes().first(), Some(b'{' | b'['))
}

/// The face payload: the worker's `faces` object (or props with a `faces`
/// key), inline as JSON text or in a file.
fn load_faces(node: &Node, base: &Path) -> Result<Value, String> {
    let src = text(node, "faces").ok_or("`clip_camera` needs `faces` (the face worker's tracks, inline or a file)")?;
    let v: Value = if faces_inline(&src) {
        serde_json::from_str(&src).map_err(|e| format!("`faces`: {e}"))?
    } else {
        let p = resolve(base, &src);
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?
    };
    Ok(match v.get("faces") {
        Some(f) if f.is_object() => f.clone(),
        _ => v.get("props").and_then(|p| p.get("faces")).cloned().unwrap_or(v),
    })
}

/// The modification time of the files a node reads (part of its cache key).
pub fn inputs_time(node: &Node, base: &Path) -> Option<std::time::SystemTime> {
    let faces = text(node, "faces")?;
    // Inline tracks are part of the params, which the key already covers.
    if faces_inline(&faces) {
        return None;
    }
    let p = resolve(base, &faces);
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Expands a `clip_camera` node for a document of format `fmt`.
pub fn build(node: &Node, fmt: &Format, base: &Path) -> Result<Component, String> {
    let src = text(node, "src").ok_or("`clip_camera` needs `src` (the source video)")?;
    let faces_json = load_faces(node, base)?;
    let fps = fmt.fps;
    let start = num(node, "start").unwrap_or(0.0);
    let duration = num(node, "duration").unwrap_or(fmt.duration);
    let track = track(&faces_json, text(node, "tuning").as_deref(), start, duration, fmt)?;
    // The video starts on the clip's first whole frame, as the reference does.
    let media_start = (start * fps + 1e-6).floor().max(0.0) / fps;
    Ok(component(&src, media_start, &track, fps))
}

/// The camera, frame by frame, for a clip of the source (`start`,
/// `duration` seconds) from the face worker's `faces` and the camera
/// `tuning` (JSON).
fn track(
    faces: &Value,
    tuning: Option<&str>,
    start: f64,
    duration: f64,
    fmt: &Format,
) -> Result<Vec<CameraFrame>, String> {
    let (faces, clip, cfg) = inputs(faces, tuning, start, duration, fmt)?;
    Ok(camera_track(faces.as_ref(), &clip, &cfg))
}

/// The camera as `track` computes it, with the id of the face it follows on
/// each frame (`None` where it follows none): what an editor shows as the
/// person being followed.
pub fn subjects(
    faces: &Value,
    tuning: Option<&str>,
    start: f64,
    duration: f64,
    fmt: &Format,
) -> Result<Vec<(CameraFrame, Option<i64>)>, String> {
    let (faces, clip, cfg) = inputs(faces, tuning, start, duration, fmt)?;
    Ok(camera_track_subjects(faces.as_ref(), &clip, &cfg))
}

fn inputs(
    faces: &Value,
    tuning: Option<&str>,
    start: f64,
    duration: f64,
    fmt: &Format,
) -> Result<(Option<Faces>, Clip, Config), String> {
    let cfg = config(tuning)?;
    let fps = fmt.fps;
    let clip = Clip {
        start,
        end: start + duration,
        frames: crate::js_round(duration * fps).max(1.0) as usize,
        fps,
        width: fmt.width as f64,
        height: fmt.height as f64,
    };
    let faces = Faces::from_json(faces, fps, cfg.cinematic_zoom_max);
    Ok((faces, clip, cfg))
}

fn config(tuning: Option<&str>) -> Result<Config, String> {
    Ok(match tuning {
        Some(t) => Config::from_json(Some(
            &serde_json::from_str::<Value>(t).map_err(|e| format!("tuning: {e}"))?,
        )),
        None => Config::default(),
    })
}

/// The faces the camera shows, frame by frame from time 0 of the clip:
/// each tracked face's box in output pixels, projected through the camera
/// (for anything placed relative to the speaker), as
/// `[{"t", "faces": [{"id", "x", "y", "w", "h", "talk"}]}]`.
pub fn faces_in_frame(
    faces: &Value,
    tuning: Option<&str>,
    start: f64,
    duration: f64,
    fmt: &Format,
) -> Result<Value, String> {
    let track = track(faces, tuning, start, duration, fmt)?;
    let Some(tracks) = Faces::from_json(faces, fmt.fps, config(tuning)?.cinematic_zoom_max) else {
        return Ok(Value::Array(Vec::new()));
    };
    let r1 = |v: f64| (v * 10.0).round() / 10.0;
    let out = track
        .iter()
        .enumerate()
        .map(|(k, cam)| {
            let t = k as f64 / fmt.fps;
            let src = cam.layers.last().expect("a camera has a layer").source;
            // The tracked frame nearest the source time, within half a second.
            let frame = crate::js_round((start + t) * tracks.fps) as i64;
            let i = tracks.frames.partition_point(|f| f.frame < frame);
            let near = [i.checked_sub(1), Some(i)]
                .into_iter()
                .flatten()
                .filter_map(|j| tracks.frames.get(j))
                .min_by_key(|f| (f.frame - frame).abs())
                .filter(|f| ((f.frame - frame).abs() as f64) <= 0.5 * tracks.fps);
            let boxes: Vec<Value> = near
                .map(|f| {
                    f.faces
                        .iter()
                        .filter(|b| [b.x, b.y, b.w, b.h].iter().all(|v| v.is_finite()))
                        .map(|b| {
                            serde_json::json!({
                                "id": b.id,
                                "x": r1(src.x + b.x * src.width),
                                "y": r1(src.y + b.y * src.height),
                                "w": r1(b.w * src.width),
                                "h": r1(b.h * src.height),
                                "talk": b.talk,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({"t": (t * 1e4).round() / 1e4, "faces": boxes})
        })
        .collect();
    Ok(Value::Array(out))
}

/// Where the camera puts the source frame by frame (the framed clip's
/// `rect`, keyed per frame from time 0 of the clip): for anything drawn in
/// the source's own space that must move with the footage, a subject
/// matte for one.
pub fn camera_rect(
    faces: &Value,
    tuning: Option<&str>,
    start: f64,
    duration: f64,
    fmt: &Format,
) -> Result<Param, String> {
    let track = track(faces, tuning, start, duration, fmt)?;
    let rects: Vec<Vec<f64>> = track
        .iter()
        .map(|c| rect_vec(&c.layers.last().expect("a camera has a layer").source))
        .collect();
    Ok(if rects.windows(2).all(|w| w[0] == w[1]) {
        Param::Vector(rects[0].clone())
    } else {
        Param::Animated {
            keys: keys(&rects, fmt.fps),
            repeat: None,
        }
    })
}

fn rect_vec(r: &Rect) -> Vec<f64> {
    vec![r.x, r.y, r.width, r.height]
}

/// Keys that reproduce a per-frame series exactly at frame times: one where
/// the motion changes, a hold before a jump (cuts snap, never slide).
fn keys(values: &[Vec<f64>], fps: f64) -> Vec<Key> {
    const JUMP_PX: f64 = 12.0;
    let n = values.len();
    let diff = |a: usize, b: usize| -> f64 {
        values[a]
            .iter()
            .zip(&values[b])
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
    };
    let jump = |a: usize| a + 1 < n && diff(a, a + 1) > JUMP_PX;
    let mut out = Vec::new();
    for k in 0..n {
        let keep = k == 0
            || k + 1 == n
            || jump(k)
            || jump(k - 1)
            || values[k + 1]
                .iter()
                .zip(&values[k])
                .zip(&values[k - 1])
                .any(|((a, b), c)| (a - 2.0 * b + c).abs() > 1e-6);
        if keep {
            out.push(Key {
                t: k as f64 / fps,
                v: values[k].clone(),
                ease: if jump(k) { Ease::Hold } else { Ease::Linear },
            });
        }
    }
    out
}

fn video(src: &str, start: f64, rect: Param) -> Node {
    Node {
        op: "video".into(),
        params: [
            ("src".to_string(), Param::Text(src.into())),
            ("start".to_string(), Param::Number(start)),
            ("rect".to_string(), rect),
        ]
        .into(),
        ..Default::default()
    }
}

fn component(src: &str, start: f64, track: &[CameraFrame], fps: f64) -> Component {
    let mut nodes: BTreeMap<NodeId, Node> = BTreeMap::new();
    // The framed clip: in protect shots its rect is the contained frame
    // (the reference's foreground layer).
    let rects: Vec<Vec<f64>> = track
        .iter()
        .map(|c| rect_vec(&c.layers.last().expect("a camera has a layer").source))
        .collect();
    let rect = if rects.windows(2).all(|w| w[0] == w[1]) {
        Param::Vector(rects[0].clone())
    } else {
        Param::Animated {
            keys: keys(&rects, fps),
            repeat: None,
        }
    };
    nodes.insert("camera".into(), video(src, start, rect));
    // Protect shots: a blurred, dimmed cover of the same footage behind the
    // contained frame, one chain per run of protect frames so it costs
    // nothing elsewhere.
    let mut below: Option<NodeId> = None;
    let mut i = 0;
    let mut run = 0;
    while i < track.len() {
        if !track[i].protect {
            i += 1;
            continue;
        }
        let a = i;
        while i < track.len() && track[i].protect {
            i += 1;
        }
        let window = Some(Timing {
            in_point: Some(a as f64 / fps),
            out_point: Some(i as f64 / fps),
            ..Default::default()
        });
        let back = &track[a].layers[0];
        let id = |s: &str| format!("protect{run}_{s}");
        let mut v = video(src, start, Param::Vector(rect_vec(&back.source)));
        v.timing = window.clone();
        nodes.insert(id("video"), v);
        nodes.insert(
            id("blur"),
            Node {
                op: "blur".into(),
                params: [("radius".to_string(), Param::Number(back.blur_px))].into(),
                inputs: [("source".to_string(), id("video"))].into(),
                timing: window.clone(),
                ..Default::default()
            },
        );
        nodes.insert(
            id("tone"),
            Node {
                op: "adjust".into(),
                // CSS brightness() multiplies; in linear light that is exposure.
                params: [("exposure".to_string(), Param::Number(back.brightness.log2()))].into(),
                inputs: [("source".to_string(), id("blur"))].into(),
                timing: window.clone(),
                ..Default::default()
            },
        );
        nodes.insert(
            id("fade"),
            Node {
                op: "opacity".into(),
                params: [("opacity".to_string(), Param::Number(back.opacity))].into(),
                inputs: [("source".to_string(), id("tone"))].into(),
                timing: window,
                ..Default::default()
            },
        );
        let layer = id("fade");
        below = Some(match below {
            None => layer,
            Some(prev) => {
                let over = id("over");
                nodes.insert(
                    over.clone(),
                    Node {
                        op: "over".into(),
                        inputs: [("fg".to_string(), layer), ("bg".to_string(), prev)].into(),
                        ..Default::default()
                    },
                );
                over
            }
        });
        run += 1;
    }
    let top = match below {
        None => "camera".to_string(),
        Some(bg) => {
            nodes.insert(
                "framed".into(),
                Node {
                    op: "over".into(),
                    inputs: [("fg".to_string(), "camera".to_string()), ("bg".to_string(), bg)].into(),
                    ..Default::default()
                },
            );
            "framed".into()
        }
    };
    nodes.insert(
        "clip_camera_out".into(),
        Node {
            op: "opacity".into(),
            params: [("opacity".to_string(), Param::Text("$opacity".into()))].into(),
            inputs: [("source".to_string(), top)].into(),
            ..Default::default()
        },
    );
    Component {
        nodes,
        output: "clip_camera_out".into(),
        params: [("opacity".to_string(), Param::Number(1.0))].into(),
        inputs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_project_through_the_camera() {
        let frames: Vec<Value> = (0..60)
            .map(|k| serde_json::json!({"frame": k, "faces": [{"id": 1, "x": 0.45, "y": 0.3, "w": 0.1, "h": 0.2, "talk": 1.0}]}))
            .collect();
        let faces = serde_json::json!({"sourceWidth": 1920, "sourceHeight": 1080, "fps": 30, "frames": frames});
        let fmt = Format {
            width: 1080,
            height: 1920,
            fps: 30.0,
            duration: 1.0,
            motion_blur: None,
        };
        let out = faces_in_frame(&faces, None, 0.0, 1.0, &fmt).unwrap();
        let out = out.as_array().unwrap();
        assert_eq!(out.len(), 30);
        let rect = camera_rect(&faces, None, 0.0, 1.0, &fmt).unwrap().eval(0.0).unwrap();
        let f = &out[0]["faces"][0];
        // The box is the source box scaled into where the camera draws the
        // source, and the camera keeps the speaker in the frame.
        let x = f["x"].as_f64().unwrap();
        assert!((x - (rect[0] + 0.45 * rect[2])).abs() < 0.1, "{x} vs {rect:?}");
        assert!((f["w"].as_f64().unwrap() - 0.1 * rect[2]).abs() < 0.1);
        let cx = x + f["w"].as_f64().unwrap() / 2.0;
        assert!((0.0..1080.0).contains(&cx), "{cx}");
    }

    #[test]
    fn subjects_name_the_followed_face_without_moving_the_camera() {
        // Face 1 speaks for the first second, face 2 (across the room) for
        // the three after.
        let frames: Vec<Value> = (0..120)
            .map(|k| {
                let (a, b) = if k < 30 { (1.0, 0.0) } else { (0.0, 1.0) };
                serde_json::json!({"frame": k, "faces": [
                    {"id": 1, "x": 0.15, "y": 0.3, "w": 0.1, "h": 0.2, "talk": a},
                    {"id": 2, "x": 0.75, "y": 0.3, "w": 0.1, "h": 0.2, "talk": b}]})
            })
            .collect();
        let faces = serde_json::json!({"sourceWidth": 1920, "sourceHeight": 1080, "fps": 30, "frames": frames,
            "shots": [{"startFrame": 0, "endFrame": 120}]});
        let fmt = Format {
            width: 1080,
            height: 1920,
            fps: 30.0,
            duration: 4.0,
            motion_blur: None,
        };
        let with = subjects(&faces, None, 0.0, 4.0, &fmt).unwrap();
        let plain = track(&faces, None, 0.0, 4.0, &fmt).unwrap();
        assert_eq!(with.iter().map(|(c, _)| c.clone()).collect::<Vec<_>>(), plain);
        let named: Vec<Option<i64>> = with.iter().map(|(_, s)| *s).collect();
        assert_eq!(named[10], Some(1));
        assert_eq!(named[119], Some(2), "{named:?}");
        // The camera frames whoever it names.
        let r = with[119].0.layers[0].source;
        let cx = r.x + 0.8 * r.width;
        assert!((0.0..1080.0).contains(&cx), "{cx}");
    }

    #[test]
    fn keys_reproduce_frames_and_hold_at_jumps() {
        let fps = 30.0;
        // Hold, ease, then a cut.
        let mut v: Vec<Vec<f64>> = Vec::new();
        for k in 0..30 {
            v.push(vec![
                -900.0 + (k.max(10).min(20) - 10) as f64 * 2.0,
                0.0,
                3413.0,
                1920.0,
            ]);
        }
        for _ in 0..10 {
            v.push(vec![-1500.0, 0.0, 3413.0, 1920.0]);
        }
        let ks = keys(&v, fps);
        let p = Param::Animated { keys: ks, repeat: None };
        for (k, want) in v.iter().enumerate() {
            let got = p.eval(k as f64 / fps).unwrap();
            assert!((got[0] - want[0]).abs() < 1e-9, "frame {k}: {got:?} vs {want:?}");
        }
        // Between the last frame before the cut and the cut, the camera holds.
        let mid = p.eval(29.5 / fps).unwrap();
        assert_eq!(mid[0], v[29][0]);
    }
}
