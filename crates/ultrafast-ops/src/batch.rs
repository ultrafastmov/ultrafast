//! One document, many videos: each row of a data file (CSV, JSON or JSON
//! lines) sets some of the document's params, and each row becomes its own
//! file. `ultrafast batch <doc.json> --data rows.csv --out 'out/{name}.mp4'`
//! renders them all with one warm renderer.
//!
//! A column (or key) named `node.param` sets that param of that node, and
//! `format.width`, `format.height`, `format.fps` or `format.duration` the
//! format. A JSON row may also carry `edits`, a list of edits as
//! `ultrafast edit` takes them. Every other column is data for the output
//! name: `{name}` in the `--out` pattern is replaced by the row's `name`,
//! `{row}` by its number (from 1). A column named `out` names the file
//! itself.

use serde_json::{Map, Value};
use std::collections::HashMap;
use ultrafast_graph::{Document, Edit, Param};

pub type Row = Map<String, Value>;

/// Reads rows from a data file by its extension: `.csv` (a header row,
/// then one row per video), `.jsonl` / `.ndjson` (an object per line), or
/// `.json` (a list of objects, or `{"rows": [...]}`).
pub fn read_rows(path: &std::path::Path) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "csv" => parse_csv(&text),
        "jsonl" | "ndjson" => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| match serde_json::from_str(l) {
                Ok(Value::Object(m)) => Ok(m),
                Ok(_) => Err(format!("line {}: not an object", i + 1)),
                Err(e) => Err(format!("line {}: {e}", i + 1)),
            })
            .collect(),
        _ => {
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let rows = match v {
                Value::Object(mut m) if m.get("rows").is_some_and(Value::is_array) => m.remove("rows").unwrap(),
                v => v,
            };
            match rows {
                Value::Array(a) => a
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| match r {
                        Value::Object(m) => Ok(m),
                        _ => Err(format!("row {}: not an object", i + 1)),
                    })
                    .collect(),
                _ => Err(format!(
                    "{}: expected a list of rows (objects), or {{\"rows\": [...]}}",
                    path.display()
                )),
            }
        }
    }
}

/// CSV (RFC 4180: quoted fields may hold commas, quotes as `""` and line
/// breaks) as rows keyed by the header. Cells stay text; [`row_edits`]
/// reads them by the type of the param they set.
pub fn parse_csv(text: &str) -> Result<Vec<Row>, String> {
    let text = text.trim_start_matches('\u{feff}');
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut rec = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => rec.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                rec.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut rec));
                any = false;
            }
            c => field.push(c),
        }
    }
    if quoted {
        return Err("CSV: a quoted field is never closed".into());
    }
    if any {
        rec.push(field);
        records.push(rec);
    }
    records.retain(|r| !(r.len() == 1 && r[0].trim().is_empty()));
    let mut it = records.into_iter();
    let header: Vec<String> = it
        .next()
        .ok_or("CSV: no header row")?
        .into_iter()
        .map(|h| h.trim().to_string())
        .collect();
    it.enumerate()
        .map(|(i, r)| {
            if r.len() > header.len() {
                return Err(format!(
                    "CSV row {}: {} cells for {} columns",
                    i + 2,
                    r.len(),
                    header.len()
                ));
            }
            Ok(header.iter().cloned().zip(r.into_iter().map(Value::String)).collect())
        })
        .collect()
}

/// Param types by op and param name, from the vocabulary.
fn param_types() -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    for op in crate::schema::vocabulary().as_array().into_iter().flatten() {
        let entry = out.entry(op["op"].as_str().unwrap_or("").to_string()).or_default();
        for p in op["params"].as_array().into_iter().flatten() {
            entry.insert(
                p["name"].as_str().unwrap_or("").to_string(),
                p["type"].as_str().unwrap_or("").to_string(),
            );
        }
    }
    out
}

/// A cell's value as a param of type `ty` (from the vocabulary; empty when
/// unknown). Text cells of text params stay text even when they look like
/// numbers (a year, a member number); others read as JSON where they can.
fn cell_param(cell: &Value, ty: &str) -> Result<Param, String> {
    let textual = ty == "text" || ty.starts_with("enum(");
    let v = match cell {
        Value::String(s) if !textual => {
            let t = s.trim();
            match serde_json::from_str::<Value>(t) {
                Ok(v) => v,
                // `1920,1080` for a vector, as a spreadsheet writes it.
                Err(_) if t.contains(',') && t.split(',').all(|x| x.trim().parse::<f64>().is_ok()) => {
                    Value::Array(t.split(',').map(|x| x.trim().parse::<f64>().unwrap().into()).collect())
                }
                Err(_) => Value::String(s.clone()),
            }
        }
        Value::Number(n) if textual => Value::String(n.to_string()),
        v => v.clone(),
    };
    serde_json::from_value(v.clone()).map_err(|_| format!("{v} is not a param value"))
}

/// The edits a row makes to `doc`. Columns without a `.` are data, not
/// edits. Errors name the column and, for a node that doesn't exist, the
/// nearest that does.
pub fn row_edits(doc: &Document, row: &Row) -> Result<Vec<Edit>, String> {
    let types = param_types();
    let mut edits = Vec::new();
    let mut format = doc.format;
    let mut format_changed = false;
    for (key, cell) in row {
        if key == "edits" {
            let more: Vec<Edit> = serde_json::from_value(cell.clone()).map_err(|e| format!("`edits`: {e}"))?;
            edits.extend(more);
            continue;
        }
        let Some((node, param)) = key.rsplit_once('.') else {
            continue;
        };
        if cell.is_null() || cell.as_str().is_some_and(|s| s.is_empty()) {
            // An empty cell keeps the document's value.
            continue;
        }
        if node == "format" {
            let num = match cell {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
            .ok_or_else(|| format!("`{key}`: {cell} is not a number"))?;
            match param {
                "width" => format.width = num.round().max(1.0) as u32,
                "height" => format.height = num.round().max(1.0) as u32,
                "fps" => format.fps = num,
                "duration" => format.duration = num,
                _ => return Err(format!("`{key}`: format has width, height, fps and duration")),
            }
            format_changed = true;
            continue;
        }
        let Some(n) = doc.nodes.get(node) else {
            let near = doc
                .nodes
                .keys()
                .min_by_key(|k| crate::schema::edit_distance(k, node))
                .map(|k| format!(" (did you mean `{k}`?)"))
                .unwrap_or_default();
            return Err(format!("`{key}`: no node `{node}`{near}"));
        };
        let ty = types
            .get(&n.op)
            .and_then(|t| t.get(param))
            .cloned()
            .or_else(|| {
                // A component instance: its default's form says the type.
                doc.components.get(&n.op).and_then(|c| match c.params.get(param) {
                    Some(Param::Text(s)) if !s.starts_with('#') => Some("text".to_string()),
                    Some(_) => Some(String::new()),
                    None => None,
                })
            })
            .unwrap_or_default();
        let value = cell_param(cell, &ty).map_err(|e| format!("`{key}`: {e}"))?;
        edits.push(Edit::SetParam {
            node: node.to_string(),
            param: param.to_string(),
            value,
        });
    }
    if format_changed {
        edits.insert(0, Edit::SetFormat { format });
    }
    Ok(edits)
}

/// The document for one row. A param its node doesn't take is refused,
/// like a node that isn't there: the film would never show that cell.
pub fn apply_row(doc: &Document, row: &Row) -> Result<Document, String> {
    let edits = row_edits(doc, row)?;
    let mut d = doc.fork();
    d.apply(&edits).map_err(|e| e.to_string())?;
    for e in &edits {
        if let Edit::SetParam { node, param, .. } = e
            && let Some(n) = d.nodes.get(node)
            && let Some(why) = crate::schema::unknown_param(&d, &n.op, param)
        {
            return Err(format!("`{node}.{param}`: {why}"));
        }
    }
    Ok(d)
}

/// The output path for row `index` (from 0): the row's `out`, else the
/// pattern with `{column}` and `{row}` filled in. Characters a file name
/// can't hold become `-`.
pub fn output_name(pattern: &str, row: &Row, index: usize) -> String {
    if let Some(Value::String(s)) = row.get("out")
        && !s.is_empty()
    {
        return s.clone();
    }
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let key = &rest[open + 1..open + close];
        let v = if key == "row" {
            Some((index + 1).to_string())
        } else {
            row.get(key).map(|v| match v {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            })
        };
        match v {
            Some(v) => out.extend(v.chars().map(|c| {
                if c == '/' || c == '\\' || c == '\0' || c.is_control() {
                    '-'
                } else {
                    c
                }
            })),
            None => out.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::from_json(
            r##"{"version":1,"format":{"width":1080,"height":1920,"fps":30,"duration":4},"output":"o",
            "nodes":{"bg":{"op":"solid","params":{"color":"#000000"}},
                     "name":{"op":"text","params":{"text":"Maya","size":90}},
                     "o":{"op":"over","inputs":{"fg":"name","bg":"bg"}}}}"##,
        )
        .unwrap()
    }

    #[test]
    fn csv_rows_set_params_by_type() {
        let rows = parse_csv(
            "name,name.text,name.size,bg.color,name.position,format.width\n\
             tomas,\"Tomás, 2026\",120,#ff6b35,\"540,960\",1080\n\
             bea,2026,,,\"[100, 200]\",\n",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let d = apply_row(&doc(), &rows[0]).unwrap();
        assert_eq!(d.nodes["name"].params["text"], Param::Text("Tomás, 2026".into()));
        assert_eq!(d.nodes["name"].params["size"], Param::Number(120.0));
        assert_eq!(d.nodes["bg"].params["color"], Param::Text("#ff6b35".into()));
        assert_eq!(d.nodes["name"].params["position"], Param::Vector(vec![540.0, 960.0]));
        // A text param keeps digits as text; an empty cell keeps the value.
        let d = apply_row(&doc(), &rows[1]).unwrap();
        assert_eq!(d.nodes["name"].params["text"], Param::Text("2026".into()));
        assert_eq!(d.nodes["name"].params["size"], Param::Number(90.0));
        assert_eq!(output_name("out/{name}-{row}.mp4", &rows[1], 1), "out/bea-2.mp4");
    }

    #[test]
    fn json_rows_and_mistakes() {
        let row: Row = serde_json::from_str(r#"{"nmae.text": "x"}"#).unwrap();
        let e = apply_row(&doc(), &row).unwrap_err();
        assert!(e.contains("did you mean `name`"), "{e}");
        // A param the node doesn't take, as a column or in `edits`.
        let row: Row = serde_json::from_str(r#"{"name.txet": "x"}"#).unwrap();
        let e = apply_row(&doc(), &row).unwrap_err();
        assert!(
            e.contains("`name.txet`: `text` has no param `txet` (did you mean `text`?)"),
            "{e}"
        );
        let row: Row = serde_json::from_str(
            r#"{"edits": [{"edit": "set_param", "node": "bg", "param": "colour", "value": "x"}]}"#,
        )
        .unwrap();
        let e = apply_row(&doc(), &row).unwrap_err();
        assert!(e.contains("did you mean `color`"), "{e}");
        let row: Row = serde_json::from_str(
            r##"{"format.height": 1080, "edits": [{"edit": "set_param", "node": "bg", "param": "color", "value": "#ffffff"}]}"##,
        )
        .unwrap();
        let d = apply_row(&doc(), &row).unwrap();
        assert_eq!(d.format.height, 1080);
        assert_eq!(d.nodes["bg"].params["color"], Param::Text("#ffffff".into()));
        let row: Row = serde_json::from_str(r#"{"out": "a/b.mp4", "name": "z"}"#).unwrap();
        assert_eq!(output_name("{name}.mp4", &row, 0), "a/b.mp4");
        let row: Row = serde_json::from_str(r#"{"name": "a/b"}"#).unwrap();
        assert_eq!(output_name("{name}.mp4", &row, 0), "a-b.mp4");
    }
}
