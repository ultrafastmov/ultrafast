//! Decomposing an encoded clip into layers: a gradient card that the
//! palette would cut into bands comes back as one gradient through its
//! outline, and the whole clip matches its source closely.
#![cfg(all(target_os = "macos", not(feature = "vulkan")))]

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_live::export::{ExportOptions, export_video};
use ultrafast_recon::layers::{LayerOptions, decompose};
use ultrafast_render::Renderer;

#[test]
fn gradient_card_becomes_a_gradient_layer() {
    let dir = std::env::temp_dir().join(format!("ultrafast-layers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("ref.mp4");
    // The card slides in and holds for under half the clip, so the plate
    // is the ground.
    let src = r##"{"version":1,"format":{"width":640,"height":360,"fps":30,"duration":3.5},"output":"l1",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#0e0b14"}},
          "sky":{"op":"gradient","params":{"a":"#3a1f6b","b":"#ff8a5c","space":"srgb","start":[0,90],"end":[0,270]}},
          "card":{"op":"shape","params":{"kind":"rect","size":[360,180],"radius":18,"color":"#ffffff",
            "center":{"keys":[{"t":1.8,"v":[-220,180],"ease":"out_cubic"},{"t":2.3,"v":[320,180]}]}}},
          "painted":{"op":"mask","inputs":{"source":"sky","matte":"card"}},
          "l1":{"op":"over","inputs":{"fg":"painted","bg":"bg"}}}}"##;
    let reference = Document::from_json(src).unwrap();
    let device = Device::new().unwrap();
    let mut r = Renderer::new(device.clone()).unwrap();
    export_video(&mut r, &reference, &clip, ExportOptions::default(), |_, _| {}).unwrap();

    let out = dir.join("layers");
    let (doc, report) = decompose(&device, &clip, &out, &LayerOptions::default(), |_| {}).unwrap();
    let fills: Vec<_> = doc.nodes.iter().filter(|(_, n)| n.op == "gradient").collect();
    assert_eq!(
        fills.len(),
        1,
        "one gradient for the card: {:#?}",
        doc.nodes.keys().collect::<Vec<_>>()
    );
    let p = serde_json::to_value(&fills[0].1.params).unwrap();
    // Vertical, moving with the card (its last key is where it holds).
    let at_rest = |v: &serde_json::Value| -> [f64; 2] {
        let v = v.get("keys").and_then(|k| k.as_array()?.last()?.get("v")).unwrap_or(v);
        [v[0].as_f64().unwrap(), v[1].as_f64().unwrap()]
    };
    let (start, end) = (at_rest(&p["start"]), at_rest(&p["end"]));
    let (dx, dy) = ((end[0] - start[0]).abs(), (end[1] - start[1]).abs());
    assert!(dy > 100.0 && dx < dy * 0.1, "direction {start:?} -> {end:?}");
    assert!(report["mean_psnr"].as_f64().unwrap() > 34.0, "{report:#}");
}

#[test]
fn photos_become_images_and_footage_a_window() {
    let dir = std::env::temp_dir().join(format!("ultrafast-layers-raster-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let assets = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/assets");
    for f in ["test.jpg", "testsrc.mp4"] {
        std::fs::copy(format!("{assets}/{f}"), dir.join(f)).unwrap();
    }
    let src = r##"{"version":1,"format":{"width":1280,"height":720,"fps":30,"duration":6.0},"output":"l2",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#101318"}},
          "photo":{"op":"image","params":{"src":"test.jpg",
             "rect":{"keys":[{"t":0.4,"v":[-420,120,400,250],"ease":"out_cubic"},{"t":1.2,"v":[90,120,400,250]},
                             {"t":3.6,"v":[90,120,400,250],"ease":"in_cubic"},{"t":4.2,"v":[90,-300,400,250]}]}}},
          "clip":{"op":"video","params":{"src":"testsrc.mp4","rect":[640,330,560,315]},"timing":{"in":2.0,"out":5.4}},
          "l1":{"op":"over","inputs":{"fg":"photo","bg":"bg"}},
          "l2":{"op":"over","inputs":{"fg":"clip","bg":"l1"}}}}"##;
    let reference = Document::from_json(src).unwrap();
    let device = Device::new().unwrap();
    let mut r = Renderer::new(device.clone()).unwrap();
    r.set_base_dir(&dir);
    let clip = dir.join("ref.mp4");
    export_video(&mut r, &reference, &clip, ExportOptions::default(), |_, _| {}).unwrap();

    let opts = LayerOptions {
        text: false,
        ..Default::default()
    };
    let (doc, report) = decompose(&device, &clip, &dir.join("layers"), &opts, |_| {}).unwrap();
    let ops: Vec<&str> = doc.nodes.values().map(|n| n.op.as_str()).collect();
    // The plate, the photo as an image moved frame by frame, the clip as
    // the source seen through a window.
    assert_eq!(ops.iter().filter(|o| **o == "image").count(), 2, "{ops:?}");
    assert_eq!(ops.iter().filter(|o| **o == "video").count(), 1, "{ops:?}");
    assert!(report["mean_psnr"].as_f64().unwrap() > 38.0, "{report:#}");
}
