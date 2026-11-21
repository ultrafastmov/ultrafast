//! The background plate of an encoded shot recovers the gradient behind a
//! text layer as an editable gradient node.
#![cfg(all(target_os = "macos", not(feature = "vulkan")))]

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_live::export::{ExportOptions, export_video};
use ultrafast_recon::plate::{PlateOptions, plate};
use ultrafast_render::Renderer;

#[test]
fn recovers_linear_gradient_behind_text() {
    let dir = std::env::temp_dir().join(format!("ultrafast-plate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("ref.mp4");
    let src = r##"{"version":1,"format":{"width":640,"height":360,"fps":30,"duration":0.5},"output":"l1",
        "nodes":{
          "bg":{"op":"gradient","params":{"a":"#14213d","b":"#2a4a7f","start":[0,0],"end":[640,360]}},
          "t":{"op":"text","params":{"text":"Plate","size":120,"position":[160,230],"valign":"baseline","color":"#ffffff"}},
          "l1":{"op":"over","inputs":{"fg":"t","bg":"bg"}}}}"##;
    let reference = Document::from_json(src).unwrap();
    let device = Device::new().unwrap();
    let mut r = Renderer::new(device.clone()).unwrap();
    export_video(&mut r, &reference, &clip, ExportOptions::default(), |_, _| {}).unwrap();

    // The draft knows the text but not the background.
    let draft = Document::from_json(&src.replace(
        r##""bg":{"op":"gradient","params":{"a":"#14213d","b":"#2a4a7f","start":[0,0],"end":[640,360]}}"##,
        r##""bg":{"op":"solid","params":{"color":"#000000"}}"##,
    ))
    .unwrap();
    let opts = PlateOptions {
        background: Some("bg".into()),
        ..Default::default()
    };
    let (doc, report) = plate(&device, &clip, Some((&draft, &dir)), &dir.join("plate.png"), &opts).unwrap();
    assert_eq!(report["recommended"], "linear_linear", "{report:#}");
    assert!(report["models"][0]["psnr"].as_f64().unwrap() > 45.0, "{report:#}");
    let bg = &doc.unwrap().nodes["bg"];
    assert_eq!(bg.op, "gradient");
    let p = serde_json::to_value(&bg.params).unwrap();
    for (k, want) in [("start", [0.0, 0.0]), ("end", [640.0, 360.0])] {
        let got = p[k].as_array().unwrap();
        for c in 0..2 {
            assert!((got[c].as_f64().unwrap() - want[c]).abs() < 3.0, "{k}: {got:?}");
        }
    }
}
