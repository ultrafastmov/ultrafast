//! Compares ultrafast's camera track with recordings of the reference camera.
//!   cargo run -p ultrafast-framing --release --example golden -- <corpus dir> <golden dir> [--verbose]
//! Corpus files hold `{props}`; golden files hold `{rows: [{f, protect,
//! zoom, layers: [{v: [x, y, w, h], s: [x, y, w, h]}]}]}` (3 decimals).
use serde_json::Value;
use std::path::Path;

fn r3(v: f64) -> f64 {
    (v * 1000.0 + 0.5).floor() / 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = Path::new(&args[0]);
    let golden = Path::new(&args[1]);
    let verbose = args.iter().any(|a| a == "--verbose");
    let only: Option<&String> = args.iter().skip(2).find(|a| !a.starts_with("--"));
    let mut names: Vec<_> = std::fs::read_dir(golden)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with(".json"))
        .filter(|n| only.is_none_or(|o| n.starts_with(o.as_str())))
        .collect();
    names.sort();
    let (mut files_ok, mut frames_total, mut frames_bad, mut worst) = (0, 0usize, 0usize, 0.0f64);
    let t0 = std::time::Instant::now();
    for name in &names {
        let props: Value = serde_json::from_str(&std::fs::read_to_string(corpus.join(name)).unwrap()).unwrap();
        let props = props.get("props").unwrap_or(&props);
        let gold: Value = serde_json::from_str(&std::fs::read_to_string(golden.join(name)).unwrap()).unwrap();
        let rows = gold["rows"].as_array().unwrap();
        let track = ultrafast_framing::camera_track_from_props(props);
        let mut bad = 0usize;
        let mut first: Option<String> = None;
        let mut file_worst = 0.0f64;
        for (i, row) in rows.iter().enumerate() {
            let Some(cam) = track.get(i) else {
                bad += 1;
                continue;
            };
            let protect = row["protect"].as_bool().unwrap();
            let zoom = row["zoom"].as_f64().unwrap();
            let layers = row["layers"].as_array().unwrap();
            let mut err = 0.0f64;
            let mut ok =
                cam.protect == protect && (r3(cam.zoom) - zoom).abs() < 1e-9 && cam.layers.len() == layers.len();
            if ok {
                for (l, g) in cam.layers.iter().zip(layers) {
                    let s = g["s"].as_array().unwrap();
                    let mine = [l.source.x, l.source.y, l.source.width, l.source.height];
                    for (m, gv) in mine.iter().zip(s) {
                        let d = (r3(*m) - gv.as_f64().unwrap()).abs();
                        err = err.max(d);
                    }
                }
                if err > 0.0015 {
                    ok = false;
                }
            }
            file_worst = file_worst.max(err);
            if !ok {
                bad += 1;
                if first.is_none() {
                    first = Some(format!(
                        "frame {i}: ultrafast protect={} zoom={:.3} x={:.3} | ref protect={protect} zoom={zoom} s={}",
                        cam.protect,
                        cam.zoom,
                        cam.layers.first().map(|l| l.source.x).unwrap_or(f64::NAN),
                        layers.first().map(|l| l["s"].to_string()).unwrap_or_default()
                    ));
                }
            }
        }
        frames_total += rows.len();
        frames_bad += bad;
        worst = worst.max(file_worst);
        if bad == 0 && track.len() == rows.len() {
            files_ok += 1;
            if verbose {
                println!("ok   {name} ({} frames)", rows.len());
            }
        } else {
            println!(
                "DIFF {name}: {bad}/{} frames differ (track {} frames); first: {}",
                rows.len(),
                track.len(),
                first.unwrap_or_default()
            );
        }
    }
    println!(
        "{files_ok}/{} files match exactly; {frames_bad}/{frames_total} frames differ; {:.1} s",
        names.len(),
        t0.elapsed().as_secs_f64()
    );
}
