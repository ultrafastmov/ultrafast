//! `ultrafast check` on a document whose render fails exits with 1, not a
//! crash: the render can fail while the plan's kernels are still compiling,
//! and the process must not exit under those compile threads (the GPU
//! driver tears down its state at exit while they still use it; glibc then
//! aborts on a corrupted heap, or the process segfaults).

use serde_json::{Map, Value, json};
use std::process::Command;

/// Many distinct fused kernels (each blur ends a group), then a blend whose
/// unknown mode fails the render once they are all queued (packing its
/// params, while they compile).
fn failing_doc() -> Value {
    let effects = [
        "wave",
        "twirl",
        "adjust",
        "levels",
        "invert",
        "tint",
        "threshold",
        "vignette",
        "grain",
        "aberration",
        "halftone",
        "glitch",
        "crt",
        "dot_matrix",
        "grade",
        "film_look",
        "engrave",
        "chroma_key",
        "light_leak",
        "lens_flare",
    ];
    let sources = ["noise", "checker", "gradient", "marble", "starfield"];
    let mut nodes = Map::new();
    let mut below: Option<String> = None;
    for (i, fx) in effects.iter().enumerate() {
        nodes.insert(format!("g{i}"), json!({"op": sources[i % sources.len()], "params": {}}));
        nodes.insert(
            format!("f{i}"),
            json!({"op": fx, "params": {}, "inputs": {"source": format!("g{i}")}}),
        );
        nodes.insert(
            format!("b{i}"),
            json!({"op": "blur", "params": {"radius": 2 + i}, "inputs": {"source": format!("f{i}")}}),
        );
        below = Some(match below {
            None => format!("b{i}"),
            Some(bg) => {
                nodes.insert(
                    format!("o{i}"),
                    json!({"op": "over", "params": {"opacity": 0.5}, "inputs": {"fg": format!("b{i}"), "bg": bg}}),
                );
                format!("o{i}")
            }
        });
    }
    nodes.insert("paper".into(), json!({"op": "solid", "params": {"color": "#ffffff"}}));
    nodes.insert(
        "out".into(),
        json!({"op": "blend", "params": {"mode": "sparkle"}, "inputs": {"fg": below, "bg": "paper"}}),
    );
    json!({
        "version": 1,
        "format": {"width": 64, "height": 64, "fps": 30, "duration": 1.0},
        "output": "out",
        "nodes": nodes
    })
}

#[test]
fn check_exits_with_1_when_the_render_fails_mid_compile() {
    let root = std::env::temp_dir().join(format!("ultrafast-check-exit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let doc = root.join("doc.json");
    std::fs::write(&doc, serde_json::to_string(&failing_doc()).unwrap()).unwrap();

    // A cold kernel cache keeps the compile threads busy when the render
    // fails; before the fix most runs crashed, so a few runs catch it.
    for run in 0..4 {
        let cache = root.join(format!("cache-{run}"));
        let out = Command::new(env!("CARGO_BIN_EXE_ultrafast"))
            .args(["check"])
            .arg(&doc)
            .env("ULTRAFAST_CACHE_DIR", &cache)
            .env("ULTRAFAST_PIPELINE_CACHE", "0")
            .env_remove("ULTRAFAST_SHARED_CACHE_DIR")
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("run {run}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
        assert_eq!(report["ok"], false, "run {run}: {report}");
        assert!(
            report["issues"].to_string().contains("unknown blend mode `sparkle`"),
            "run {run}: {report}"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "run {run}: {:?}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
