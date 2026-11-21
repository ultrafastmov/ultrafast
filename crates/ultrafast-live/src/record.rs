//! `ultrafast record`: plays a document while applying a script of edits at
//! given moments, and records what the player shows (timeline, the
//! outlines of what each edit touched, and a log line of the edit) to a
//! video. A way to show a film being edited live, by an agent or a person,
//! with the exact edits that made it.

use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use ultrafast_graph::{Document, Edit};
use ultrafast_render::{Mode, Renderer};

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    /// Seconds into the recording.
    pub at: f64,
    pub edits: Vec<Edit>,
    /// What the log line says (default: a summary of the edits).
    #[serde(default)]
    pub note: Option<String>,
}

/// Parses a script: a JSON array of steps, or one step per line.
pub fn parse_script(s: &str) -> Result<Vec<Step>, String> {
    let t = s.trim();
    let mut steps: Vec<Step> = if t.starts_with('[') {
        serde_json::from_str(t).map_err(|e| format!("script: {e}"))?
    } else {
        t.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(|e| format!("script: {e}")))
            .collect::<Result<_, _>>()?
    };
    steps.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok(steps)
}

fn short(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.chars().count() > 28 {
        format!("{}…", s.chars().take(27).collect::<String>())
    } else {
        s
    }
}

/// One line describing an edit batch.
pub fn summary(edits: &[Edit]) -> String {
    let one = |e: &Edit| match e {
        Edit::SetParam { node, param, value } => {
            format!(
                "{node}.{param} = {}",
                short(&serde_json::to_value(value).unwrap_or(Value::Null))
            )
        }
        Edit::RemoveParam { node, param } => format!("{node}.{param} removed"),
        Edit::AddNode { id, node } | Edit::AddLayer { id, node } => format!("+ {id} ({})", node.op),
        Edit::RemoveNode { id } => format!("- {id}"),
        Edit::Connect { node, port, source } => format!("{node}.{port} <- {source}"),
        Edit::Disconnect { node, port } => format!("{node}.{port} disconnected"),
        Edit::SetOutput { node } => format!("output = {node}"),
        Edit::SetFormat { .. } => "format changed".into(),
        Edit::SetTiming { node, .. } => format!("{node} timing"),
        Edit::SetComponent { name, .. } => format!("component {name}"),
    };
    let mut parts: Vec<String> = edits.iter().take(2).map(one).collect();
    if edits.len() > 2 {
        parts.push(format!("+{} more", edits.len() - 2));
    }
    parts.join("  ·  ")
}

fn touched(edits: &[Edit]) -> Vec<String> {
    edits
        .iter()
        .filter_map(|e| match e {
            Edit::SetParam { node, .. } | Edit::RemoveParam { node, .. } => Some(node.clone()),
            Edit::AddNode { id, .. } | Edit::AddLayer { id, .. } => Some(id.clone()),
            Edit::Connect { node, .. } | Edit::Disconnect { node, .. } | Edit::SetTiming { node, .. } => {
                Some(node.clone())
            }
            _ => None,
        })
        .collect()
}

pub struct RecordOptions {
    pub width: u32,
    pub height: u32,
    /// Recording length in seconds (the document loops under it).
    pub duration: f64,
    /// Draw the log line of each edit.
    pub log: bool,
}

/// Records `doc` played with `steps` applied into `out` (needs ffmpeg).
pub fn record(
    mut doc: Document,
    base_dir: &Path,
    steps: &[Step],
    out: &Path,
    opts: &RecordOptions,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let device = ultrafast_gpu::Device::new().map_err(|e| e.to_string())?;
    let mut r = Renderer::new(device).map_err(|e| e.to_string())?;
    r.set_base_dir(base_dir);
    r.set_mode(Mode::Exact);
    let fps = doc.format.fps;
    let dur = doc.format.duration.max(1e-6);
    let (w, h) = (opts.width & !1, opts.height & !1);
    if opts.log {
        // The log line is a node of the document itself, over everything.
        let size = (doc.format.width.min(doc.format.height) as f64 * 0.034).round();
        let node: ultrafast_graph::Node = serde_json::from_value(serde_json::json!({
            "op": "text",
            "params": { "text": " ", "font": "ultrafast:mono", "size": size, "weight": 500, "color": "#ffffff",
                        "position": [size * 2.0, size * 3.0], "opacity": 0.0 }
        }))
        .map_err(|e| e.to_string())?;
        doc.apply(&[Edit::AddLayer {
            id: "__edit_log".into(),
            node,
        }])
        .map_err(|e| e.to_string())?;
        let shadow: ultrafast_graph::Node = serde_json::from_value(serde_json::json!({
            "op": "drop_shadow", "params": { "color": "#000000", "opacity": 0.9, "offset": [0, 2], "radius": 6 },
            "inputs": { "source": "__edit_log" }
        }))
        .map_err(|e| e.to_string())?;
        doc.apply(&[
            Edit::AddNode {
                id: "__edit_log_shadow".into(),
                node: shadow,
            },
            Edit::Connect {
                node: "__edit_log_over".into(),
                port: "fg".into(),
                source: "__edit_log_shadow".into(),
            },
        ])
        .map_err(|e| e.to_string())?;
    }
    let mut ff = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{w}x{h}"))
        .arg("-r")
        .arg(format!("{fps}"))
        .args([
            "-i", "-", "-c:v", "libx264", "-preset", "medium", "-crf", "16", "-pix_fmt", "yuv420p",
        ])
        .arg(out)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg is needed to record: {e}"))?;
    let mut stdin = ff.stdin.take().unwrap();
    let total = (opts.duration * fps).round() as u64;
    let mut next = 0;
    let mut marks_src: Vec<(String, f64)> = Vec::new();
    for i in 0..total {
        let rt = i as f64 / fps;
        let t = rt % dur;
        while next < steps.len() && steps[next].at <= rt + 1e-9 {
            let s = &steps[next];
            doc.apply(&s.edits).map_err(|e| format!("step at {}s: {e}", s.at))?;
            for n in touched(&s.edits) {
                marks_src.retain(|(m, _)| *m != n);
                marks_src.push((n, rt));
            }
            if opts.log {
                let text = s.note.clone().unwrap_or_else(|| format!("edit  {}", summary(&s.edits)));
                let fade = serde_json::json!({ "keys": [
                    { "t": t, "v": [0.0] }, { "t": t + 0.15, "v": [1.0] },
                    { "t": t + 2.6, "v": [1.0] }, { "t": t + 3.1, "v": [0.0] } ] });
                doc.apply(&[
                    Edit::SetParam {
                        node: "__edit_log".into(),
                        param: "text".into(),
                        value: ultrafast_graph::Param::Text(text),
                    },
                    Edit::SetParam {
                        node: "__edit_log".into(),
                        param: "opacity".into(),
                        value: serde_json::from_value(fade).map_err(|e| e.to_string())?,
                    },
                ])
                .map_err(|e| e.to_string())?;
            }
            next += 1;
        }
        let (fw, fh) = (doc.format.width as f32, doc.format.height as f32);
        let marks: Vec<([f32; 4], f32)> = marks_src
            .iter()
            .filter(|(_, at)| rt - at < 1.4)
            .rev()
            .take(4)
            .map(|(n, at)| {
                let b = r.content_bounds(n).unwrap_or([0.0, 0.0, fw, fh]);
                (b, (1.0 - (rt - at) / 1.4) as f32)
            })
            .collect();
        let hud = [1.0, (t / dur) as f32, 0.0, 0.0, 0.0, 1.0];
        let px = r
            .render_presented(&doc, t, w, h, hud, &marks)
            .map_err(|e| e.to_string())?;
        stdin.write_all(&px).map_err(|e| e.to_string())?;
        progress(i + 1, total);
    }
    drop(stdin);
    let status = ff.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("ffmpeg failed ({status})"));
    }
    Ok(())
}
