//! `ultrafast import-svg --into` adds a component to a document and writes
//! the document back in the forms it was written in, as an edit does: keys
//! written short (`[t, v, ease]`) stay short, and an `over`'s layers stay a
//! list.

use serde_json::{Value, json};
use std::process::Command;

#[test]
fn import_svg_into_keeps_the_documents_forms() {
    let dir = std::env::temp_dir().join(format!("ultrafast-import-into-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("film.json");
    let film = json!({
        "version": 1,
        "format": {"width": 64, "height": 64, "fps": 30, "duration": 1},
        "output": "o",
        "nodes": {
            "bg": {"op": "solid", "params": {"color": "#101010"}},
            "dot": {"op": "shape", "params": {"size": [8, 8],
                    "center": {"keys": [[0, [8, 8], "out_cubic"], [1, [56, 56]]]}}},
            "o": {"op": "over", "inputs": ["bg", "dot"]}
        }
    });
    std::fs::write(&doc, film.to_string()).unwrap();
    let svg = dir.join("mark.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#ff0000"/></svg>"##,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ultrafast"))
        .args(["import-svg", svg.to_str().unwrap(), "--into", doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&doc).unwrap()).unwrap();
    assert!(v["components"]["mark"].is_object(), "{v}");
    assert_eq!(
        v["nodes"]["dot"]["params"]["center"]["keys"],
        json!([[0.0, [8.0, 8.0], "out_cubic"], [1.0, [56.0, 56.0]]])
    );
    assert_eq!(v["nodes"]["o"]["inputs"], json!(["bg", "dot"]));
    let _ = std::fs::remove_dir_all(&dir);
}
