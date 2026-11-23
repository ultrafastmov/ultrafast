//! `ultrafast batch --docs DIR`: each row's document, written to DIR, reads
//! the same files the template does, and keeps the template's `meta`.

use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

#[test]
fn row_documents_find_their_files_from_where_they_are_written() {
    let root = std::env::temp_dir().join(format!("ultrafast-batch-docs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let proj = root.join("proj");
    std::fs::create_dir_all(proj.join("people")).unwrap();
    for f in ["bg.jpg", "people/ana.jpg", "people/ben.jpg"] {
        std::fs::write(proj.join(f), b"").unwrap();
    }
    let template = json!({
        "version": 1,
        "format": {"width": 320, "height": 180, "fps": 30, "duration": 1.0},
        "output": "card",
        "nodes": {
            "bg": {"op": "image", "params": {"src": "bg.jpg"}},
            "photo": {"op": "image", "params": {"src": "people/ana.jpg", "fit": "contain"}},
            "card": {"op": "over", "inputs": {"fg": "photo", "bg": "bg"}}
        },
        "meta": {"fonts": [{"family": "Brand", "src": "/fonts/Brand.ttf"}]}
    });
    std::fs::write(proj.join("card.json"), serde_json::to_string_pretty(&template).unwrap()).unwrap();
    std::fs::write(
        proj.join("rows.csv"),
        "name,photo.src\nana,people/ana.jpg\nben,people/ben.jpg\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ultrafast"))
        .current_dir(&root)
        .args(["batch", "proj/card.json", "--data", "proj/rows.csv"])
        .args(["--out", "{name}.mp4", "--docs", "out/docs"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let docs = root.join("out/docs");
    for (name, photo) in [("ana", "people/ana.jpg"), ("ben", "people/ben.jpg")] {
        let text = std::fs::read_to_string(docs.join(format!("{name}.json"))).unwrap();
        assert!(text.starts_with("{\n  \"version\""), "{text}");
        let doc: Value = serde_json::from_str(&text).unwrap();
        let src = |node: &str| doc["nodes"][node]["params"]["src"].as_str().unwrap().to_string();
        assert_eq!(src("bg"), "../../proj/bg.jpg");
        assert_eq!(src("photo"), format!("../../proj/{photo}"));
        for node in ["bg", "photo"] {
            assert!(Path::new(&docs).join(src(node)).is_file(), "{name}: {}", src(node));
        }
        assert_eq!(doc["meta"], template["meta"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
