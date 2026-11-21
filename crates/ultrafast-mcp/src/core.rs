//! What the tools do given a renderer and a document, shared by the local
//! server (a session per document file) and the remote one (the service's
//! renderer, with the document's inputs already resolved).

use crate::rpc::{json_text, text};
use serde_json::{Value, json};
use ultrafast_graph::{Document, Edit};
use ultrafast_render::{Mode, Renderer};

/// Images larger than this as PNG go back as JPEG, so a sheet of busy
/// footage stays small enough for any client.
const PNG_LIMIT: usize = 1_500_000;

/// A tool result with one image and a caption.
pub fn image(w: u32, h: u32, rgba: &[u8], caption: String) -> Result<Value, String> {
    let (data, mime) = encode(w, h, rgba)?;
    Ok(json!({ "content": [
        { "type": "image", "data": base64(&data), "mimeType": mime },
        { "type": "text", "text": caption }
    ] }))
}

/// PNG, or JPEG when the PNG would be large.
pub fn encode(w: u32, h: u32, rgba: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .and_then(|mut wr| wr.write_image_data(rgba))
            .map_err(|e| e.to_string())?;
    }
    if png.len() <= PNG_LIMIT {
        return Ok((png, "image/png"));
    }
    // JPEG has no alpha: composite over black as the frame would show.
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u32;
            [0, 1, 2].map(|c| (p[c] as u32 * a / 255) as u8)
        })
        .collect();
    let mut jpg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 88)
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok((jpg, "image/jpeg"))
}

pub fn base64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Box-filter downscale so the longest side is at most `max`.
pub fn shrink(w: u32, h: u32, rgba: &[u8], max: u32) -> (u32, u32, Vec<u8>) {
    let k = (w.max(h) as f64 / max as f64).ceil().max(1.0) as u32;
    if k == 1 {
        return (w, h, rgba.to_vec());
    }
    let (tw, th) = (w / k, h / k);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            let mut acc = [0u32; 4];
            for yy in 0..k {
                for xx in 0..k {
                    let i = (((y * k + yy) * w + x * k + xx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += rgba[i + c] as u32;
                    }
                }
            }
            let o = ((y * tw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / (k * k)) as u8;
            }
        }
    }
    (tw, th, out)
}

/// A document from a tool argument: a JSON object, or a string holding one
/// (some clients send objects as strings).
pub fn parse_document(v: Option<&Value>) -> Result<Document, String> {
    let v = v.ok_or("missing `document`")?;
    let src = match v {
        Value::String(s) => s.clone(),
        Value::Object(_) => v.to_string(),
        _ => return Err("`document` must be an ultrafast document (a JSON object)".into()),
    };
    Document::from_json(&src).map_err(|e| format!("not an ultrafast document: {e}"))
}

/// The document argument as JSON, checked to parse.
pub fn document_value(v: Option<&Value>) -> Result<Value, String> {
    let doc = parse_document(v)?;
    serde_json::from_str(&doc.to_json()).map_err(|e| e.to_string())
}

/// `ultrafast_ops`: Markdown of every op, or the JSON of the named ones.
pub fn ops(args: &Value) -> Value {
    let vocab = ultrafast_render::schema::vocabulary();
    match args.get("ops").and_then(Value::as_array) {
        Some(names) if !names.is_empty() => {
            let want: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            let found: Vec<&Value> = vocab
                .as_array()
                .into_iter()
                .flatten()
                .filter(|op| want.contains(&op["op"].as_str().unwrap_or("")))
                .collect();
            let missing: Vec<&&str> = want
                .iter()
                .filter(|n| !found.iter().any(|op| op["op"] == ***n))
                .collect();
            let mut out = json!({ "ops": found });
            if !missing.is_empty() {
                out["unknown"] = json!(missing);
            }
            json_text(&out)
        }
        _ => text(ops_markdown(&vocab)),
    }
}

/// One line per op and per param: a compact reference for an agent's
/// context (what `uf ops --markdown` prints, docs/OPS.md).
pub fn ops_markdown(vocab: &Value) -> String {
    let mut md =
        String::from("# ultrafast ops\n\nEvery op, its input ports and its params (type = default: what it does).\n");
    for op in vocab.as_array().into_iter().flatten() {
        let ports: Vec<&str> = op["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str())
            .collect();
        md += &format!(
            "\n## {}{}\n{}\n",
            op["op"].as_str().unwrap_or(""),
            if ports.is_empty() {
                String::new()
            } else {
                format!("({})", ports.join(", "))
            },
            op["summary"].as_str().unwrap_or("")
        );
        for p in op["params"].as_array().into_iter().flatten() {
            let default = p["default"].as_str().unwrap_or("");
            md += &format!(
                "- `{}`: {}{}{}\n",
                p["name"].as_str().unwrap_or(""),
                p["type"].as_str().unwrap_or(""),
                if default.is_empty() {
                    String::new()
                } else {
                    format!(" = {default}")
                },
                match p["doc"].as_str().unwrap_or("") {
                    "" => String::new(),
                    d => format!(" - {d}"),
                }
            );
        }
    }
    md
}

/// A template's document JSON.
pub fn template(args: &Value) -> Result<&'static str, String> {
    let name = args.get("template").and_then(Value::as_str).unwrap_or("");
    ultrafast_ops::templates::get(name).ok_or_else(|| {
        format!(
            "unknown template `{name}` ({})",
            ultrafast_ops::templates::NAMES.join(", ")
        )
    })
}

fn issues_json(doc: &Document) -> Vec<Value> {
    ultrafast_render::schema::check(doc)
        .into_iter()
        .map(|i| json!({ "node": i.node, "severity": i.severity, "message": i.message }))
        .collect()
}

/// Renders frame `t` exactly, leaving the renderer's mode as it was.
pub fn render_exact(r: &mut Renderer, doc: &Document, t: f64) -> Result<Vec<u8>, String> {
    let mode = r.mode();
    r.set_mode(Mode::Exact);
    let px = r.render_rgba8(doc, t).map(|(px, _)| px).map_err(|e| e.to_string());
    r.set_mode(mode);
    px
}

/// `ultrafast_check`: static problems, then what rendering the first frame
/// reports (shader compiles, missing files and fonts).
pub fn check(r: &mut Renderer, doc: &Document) -> Value {
    let mut issues = issues_json(doc);
    if let Err(e) = render_exact(r, doc, 0.0) {
        issues.push(json!({ "node": "", "severity": "error", "message": e }));
    }
    for d in r.diagnostics() {
        issues.push(json!({ "node": d.node, "severity": d.severity(), "message": d.message }));
    }
    json_text(&json!({ "issues": issues }))
}

/// `ultrafast_frame`.
pub fn frame(r: &mut Renderer, doc: &Document, args: &Value) -> Result<Value, String> {
    let t = args.get("t").and_then(Value::as_f64).unwrap_or(0.0);
    let max = args.get("max_size").and_then(Value::as_f64).unwrap_or(1024.0) as u32;
    let (w, h) = (doc.format.width, doc.format.height);
    let px = render_exact(r, doc, t)?;
    let (tw, th, small) = shrink(w, h, &px, max.clamp(64, 4096));
    image(tw, th, &small, format!("t={t}s, {w}x{h} (shown at {tw}x{th})"))
}

/// `ultrafast_sheet`.
pub fn sheet(r: &mut Renderer, doc: &Document, args: &Value) -> Result<Value, String> {
    let times: Vec<f64> = match args.get("times").and_then(Value::as_array) {
        Some(v) if !v.is_empty() => v.iter().filter_map(Value::as_f64).take(60).collect(),
        _ => {
            let n = args
                .get("count")
                .and_then(Value::as_f64)
                .unwrap_or(9.0)
                .clamp(1.0, 30.0) as usize;
            let d = doc.format.duration;
            (0..n).map(|i| d * (i as f64 + 0.5) / n as f64).collect()
        }
    };
    let fmt = doc.format;
    let cols = args
        .get("columns")
        .and_then(Value::as_f64)
        .map(|c| c as usize)
        .unwrap_or(if fmt.height > fmt.width { 5 } else { 3 })
        .clamp(1, times.len().max(1));
    let (w, h, rgba) = ultrafast_live::contact_sheet(r, doc, &times, cols)?;
    // Keep the sheet within what clients take as one image.
    let (w, h, rgba) = shrink(w, h, &rgba, 2400);
    let labels: Vec<String> = times.iter().map(|t| format!("{t:.2}")).collect();
    image(
        w,
        h,
        &rgba,
        format!("{} frames at {} s", times.len(), labels.join(", ")),
    )
}

/// `ultrafast_review`.
pub fn review(r: &mut Renderer, doc: &Document, args: &Value) -> Result<Value, String> {
    let opts = ultrafast_render::review::ReviewOptions {
        step: args.get("step").and_then(Value::as_f64).unwrap_or(0.1).max(0.02),
        safe: match args.get("safe").and_then(Value::as_str) {
            Some("social") => ultrafast_render::review::SafeArea::Social,
            _ => ultrafast_render::review::SafeArea::Margin(0.04),
        },
    };
    let mode = r.mode();
    r.set_mode(Mode::Exact);
    let rep = ultrafast_render::review::review(r, doc, &opts);
    r.set_mode(mode);
    let rep = rep.map_err(|e| e.to_string())?;
    Ok(json_text(&serde_json::to_value(&rep).map_err(|e| e.to_string())?))
}

/// `ultrafast_inspect`.
pub fn inspect(r: &mut Renderer, doc: &Document, args: &Value) -> Result<Value, String> {
    let t = args.get("t").and_then(Value::as_f64).unwrap_or(0.0);
    render_exact(r, doc, t)?;
    let items = r.visible(doc, t);
    Ok(json_text(&json!({ "t": t, "visible": items })))
}

/// `ultrafast_profile`.
pub fn profile(r: &mut Renderer, doc: &Document, args: &Value) -> Result<Value, String> {
    let frames = args
        .get("frames")
        .and_then(Value::as_f64)
        .unwrap_or(60.0)
        .clamp(1.0, 600.0) as u64;
    let from = args.get("from").and_then(Value::as_f64).unwrap_or(0.0);
    let rep = r.profile_frames(doc, frames, from).map_err(|e| e.to_string())?;
    Ok(json_text(&serde_json::to_value(&rep).map_err(|e| e.to_string())?))
}

/// `ultrafast_estimate`.
pub fn estimate(r: &mut Renderer, doc: &Document, args: &Value, name: &str) -> Result<Value, String> {
    let opts = ultrafast_live::estimate::Options {
        from: args.get("from").and_then(Value::as_f64),
        to: args.get("to").and_then(Value::as_f64),
        samples: args.get("samples").and_then(Value::as_u64).unwrap_or(32).clamp(1, 600) as u32,
        ..Default::default()
    };
    let est = ultrafast_live::estimate::estimate(r, doc, &opts)?;
    let mut out = json_text(&serde_json::to_value(&est).map_err(|e| e.to_string())?);
    out["content"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({ "type": "text", "text": est.text(name) }));
    Ok(out)
}

/// The rows of a batch: inline `rows`, or read from the `data` file
/// (relative paths resolve against `base`).
pub fn batch_rows(args: &Value, base: Option<&std::path::Path>) -> Result<Vec<ultrafast_ops::batch::Row>, String> {
    if let Some(rows) = args.get("rows").and_then(Value::as_array) {
        return rows
            .iter()
            .enumerate()
            .map(|(i, r)| r.as_object().cloned().ok_or(format!("row {} is not an object", i + 1)))
            .collect();
    }
    let data = args
        .get("data")
        .and_then(Value::as_str)
        .ok_or("give `rows` or a `data` file")?;
    let path = match base {
        Some(b) if std::path::Path::new(data).is_relative() => b.join(data),
        _ => std::path::PathBuf::from(data),
    };
    ultrafast_ops::batch::read_rows(&path)
}

/// Every row's document and output name, checked before anything renders.
pub fn batch_jobs(
    doc: &Document,
    rows: &[ultrafast_ops::batch::Row],
    pattern: &str,
) -> Result<Vec<(Document, String)>, String> {
    if rows.is_empty() {
        return Err("no rows".into());
    }
    let mut jobs: Vec<(Document, String)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let d = ultrafast_ops::batch::apply_row(doc, row).map_err(|e| format!("row {}: {e}", i + 1))?;
        let errors: Vec<String> = ultrafast_render::schema::check(&d)
            .into_iter()
            .filter(|i| i.severity == "error")
            .map(|i| format!("{}: {}", i.node, i.message))
            .collect();
        if !errors.is_empty() {
            return Err(format!("row {}: {}", i + 1, errors.join("; ")));
        }
        let out = ultrafast_ops::batch::output_name(pattern, row, i);
        if jobs.iter().any(|(_, o)| *o == out) {
            return Err(format!(
                "row {}: writes {out} again (put {{row}} or a column in the pattern)",
                i + 1
            ));
        }
        jobs.push((d, out));
    }
    Ok(jobs)
}

/// Applies an edit batch. Returns the edited document and the report the
/// tool sends (change kind and problems on touched nodes).
pub fn edit(mut doc: Document, args: &Value) -> Result<(Document, Value), String> {
    let edits: Vec<Edit> = serde_json::from_value(args.get("edits").cloned().unwrap_or(json!([])))
        .map_err(|e| format!("bad edits: {e}"))?;
    let change = doc.apply(&edits).map_err(|e| e.to_string())?;
    let report = json!({ "change": format!("{change:?}").to_lowercase(), "issues": issues_json(&doc) });
    Ok((doc, report))
}

/// `ultrafast_beats` on a decoded file.
pub fn beats(path: &std::path::Path, args: &Value) -> Result<Value, String> {
    let pcm = ultrafast_media::audio::decode(path)
        .map_err(|e| e.to_string())?
        .ok_or("no audio in that file")?;
    let rep = ultrafast_media::beats::analyze(&pcm, args.get("band").and_then(Value::as_str).unwrap_or("all"));
    Ok(text(serde_json::to_string(&rep).map_err(|e| e.to_string())?))
}

/// The `text` node `ultrafast_measure` and `ultrafast_outline` work on.
pub fn text_node(args: &Value) -> Result<ultrafast_graph::Node, String> {
    let params = args.get("params").cloned().ok_or("missing `params`")?;
    serde_json::from_value(json!({ "op": "text", "params": params })).map_err(|e| e.to_string())
}

/// `ultrafast_measure` or `ultrafast_outline` (`outline` true).
pub fn measure(r: &mut Renderer, args: &Value, outline: bool) -> Result<Value, String> {
    let node = text_node(args)?;
    if outline {
        Ok(text(r.text_outline(&node).map_err(|e| e.to_string())?))
    } else {
        let m = r.measure_text(&node).map_err(|e| e.to_string())?;
        Ok(json_text(&serde_json::to_value(&m).map_err(|e| e.to_string())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_images_stay_png_and_big_ones_become_jpeg() {
        let (_, mime) = encode(4, 4, &[200u8; 64]).unwrap();
        assert_eq!(mime, "image/png");
        // Noise does not compress: 1000x1000 RGBA of it is ~4 MB as PNG.
        let mut x = 1u32;
        let noise: Vec<u8> = (0..4_000_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8 | 1
            })
            .collect();
        let (jpg, mime) = encode(1000, 1000, &noise).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(jpg.len() < 2_000_000);
    }

    #[test]
    fn documents_parse_from_objects_and_strings() {
        let src = ultrafast_ops::templates::get("title").unwrap();
        let v: Value = serde_json::from_str(src).unwrap();
        assert!(parse_document(Some(&v)).is_ok());
        assert!(parse_document(Some(&Value::String(src.into()))).is_ok());
        assert!(parse_document(Some(&json!(3))).is_err());
        assert!(parse_document(None).is_err());
    }

    #[test]
    fn ops_lookup_names_unknown_ops() {
        let r = ops(&json!({ "ops": ["text", "nope"] }));
        let body: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["ops"][0]["op"], "text");
        assert_eq!(body["unknown"][0], "nope");
        let md = ops(&json!({}));
        assert!(md["content"][0]["text"].as_str().unwrap().contains("## text"));
    }

    #[test]
    fn batch_rows_become_checked_documents() {
        let doc = Document::from_json(ultrafast_ops::templates::get("title").unwrap()).unwrap();
        let args = json!({ "rows": [ { "name": "a", "title.text": "One" }, { "name": "b", "title.text": "Two" } ] });
        let rows = batch_rows(&args, None).unwrap();
        let jobs = batch_jobs(&doc, &rows, "out/{name}.mp4").unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[1].1, "out/b.mp4");
        assert!(jobs[1].0.to_json().contains("Two"));
        // Two rows writing one file, and a misspelled param, are refused.
        assert!(batch_jobs(&doc, &rows, "same.mp4").is_err());
        let bad = batch_rows(&json!({ "rows": [ { "no_such_node.text": "x" } ] }), None).unwrap();
        assert!(batch_jobs(&doc, &bad, "{row}.mp4").is_err());
    }

    #[test]
    fn edits_apply_to_a_document() {
        let doc = Document::from_json(ultrafast_ops::templates::get("title").unwrap()).unwrap();
        let add = json!({ "edits": [{ "edit": "add_layer", "id": "wash", "node": { "op": "solid", "params": { "color": "#ff0000" } } }] });
        let (doc, rep) = edit(doc, &add).unwrap_or_else(|e| panic!("{e}"));
        assert!(rep["change"].is_string());
        assert!(doc.to_json().contains("wash"));
        assert!(
            edit(
                doc,
                &json!({ "edits": [{ "edit": "remove_node", "id": "no-such-node" }] })
            )
            .is_err()
        );
    }
}
