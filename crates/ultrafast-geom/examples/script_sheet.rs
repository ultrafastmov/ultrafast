//! A sheet of text as Ultrafast lays it out, as SVG outlines: to look at how a
//! document's `meta.script_fonts` sets each writing system without a GPU.
//!
//!     script_sheet sheet.json > sheet.svg
//!
//! sheet.json: {"width", "height", "pins": [{"family", "weight", "italic",
//! "path"}], "script_fonts": [...], "items": [{"text", "font", "weight",
//! "size", "x", "y", "tracking"?, "color"?, "align"?}]}.

use serde_json::Value;
use std::path::{Path, PathBuf};
use ultrafast_geom::geom::{self, FontPin, Fonts, ScriptFont};
use ultrafast_graph::{Format, Node, Param};

fn main() {
    let path = std::env::args().nth(1).expect("usage: script_sheet sheet.json");
    let sheet: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut fonts = Fonts::new();
    let pins: Vec<FontPin> = sheet["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| FontPin {
            family: p["family"].as_str().unwrap().to_string(),
            weight: p["weight"].as_u64().unwrap() as u16,
            italic: p["italic"].as_bool().unwrap_or(false),
            path: PathBuf::from(p["path"].as_str().unwrap()),
        })
        .collect();
    fonts.pin(&pins).unwrap();
    let rows: Vec<ScriptFont> = serde_json::from_value(sheet["script_fonts"].clone()).unwrap();
    fonts.set_script_fonts(rows);
    let (w, h) = (sheet["width"].as_f64().unwrap(), sheet["height"].as_f64().unwrap());
    println!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><rect width="{w}" height="{h}" fill="{}"/>"#,
        sheet["background"].as_str().unwrap_or("#fff")
    );
    let fmt = Format {
        width: w as u32,
        height: h as u32,
        fps: 30.0,
        duration: 1.0,
        motion_blur: None,
    };
    for item in sheet["items"].as_array().unwrap() {
        let (x, y) = (item["x"].as_f64().unwrap(), item["y"].as_f64().unwrap());
        let color = item["color"].as_str().unwrap_or("#111");
        if item.get("label").and_then(Value::as_bool) == Some(true) {
            println!(
                r#"<text x="{x}" y="{y}" font-family="sans-serif" font-size="{}" fill="{color}">{}</text>"#,
                item["size"].as_f64().unwrap(),
                item["text"]
                    .as_str()
                    .unwrap()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
            );
            continue;
        }
        let mut node = Node {
            op: "text".into(),
            ..Default::default()
        };
        let p = &mut node.params;
        p.insert("text".into(), Param::Text(item["text"].as_str().unwrap().into()));
        p.insert("font".into(), Param::Text(item["font"].as_str().unwrap().into()));
        p.insert("weight".into(), Param::Number(item["weight"].as_f64().unwrap_or(400.0)));
        p.insert("size".into(), Param::Number(item["size"].as_f64().unwrap()));
        p.insert(
            "tracking".into(),
            Param::Number(item["tracking"].as_f64().unwrap_or(0.0)),
        );
        p.insert("valign".into(), Param::Text("baseline".into()));
        p.insert("markup".into(), Param::Bool(false));
        if let Some(a) = item["align"].as_str() {
            p.insert("align".into(), Param::Text(a.into()));
        }
        if let Some(wrap) = item["wrap"].as_f64() {
            p.insert("width".into(), Param::Number(wrap));
        }
        match geom::text_paths(
            "sheet",
            &node,
            &fmt,
            0.0,
            ultrafast_geom::Affine::IDENTITY,
            &mut fonts,
            Path::new("."),
        ) {
            Ok((paths, _)) => {
                let d: Vec<String> = paths.iter().map(|(path, _, _)| path.to_svg()).collect();
                println!(
                    r#"<path transform="translate({x} {y})" fill="{color}" d="{}"/>"#,
                    d.join(" ")
                );
            }
            Err(e) => {
                println!(r#"<text x="{x}" y="{y}" font-size="20" fill="red">{e}</text>"#);
                eprintln!("{e}");
            }
        }
        if let Ok(id) = fonts.lookup(
            item["font"].as_str().unwrap(),
            item["weight"].as_f64().unwrap_or(400.0) as u16,
        ) {
            let missing = fonts.missing_glyphs(id, item["text"].as_str().unwrap());
            if !missing.is_empty() {
                eprintln!("missing {:?} in {}", missing, item["text"]);
            }
        }
    }
    println!("</svg>");
}
