//! Font matching recovers family, weight, size, baseline and tracking of
//! text in an encoded video, starting from a draft in the wrong font.
#![cfg(all(target_os = "macos", not(feature = "vulkan")))]

use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Param};
use ultrafast_live::export::{ExportOptions, export_video};
use ultrafast_recon::fonts::{MatchOptions, match_font};
use ultrafast_render::Renderer;

#[test]
fn recovers_font_size_and_tracking() {
    let dir = std::env::temp_dir().join(format!("ultrafast-font-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("ref.mp4");
    let reference = Document::from_json(
        r##"{"version":1,"format":{"width":960,"height":360,"fps":30,"duration":0.5},"output":"l2",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#1d2b44"}},
          "a":{"op":"text","params":{"text":"Quiet Harbour","font":"Georgia","weight":700,"size":72,
               "position":[80,140],"valign":"baseline","color":"#fca311"}},
          "b":{"op":"text","params":{"text":"Field notes from the coast","font":"Futura","weight":500,"size":40,
               "position":[84,250],"valign":"baseline","color":"#e5e5e5","tracking":2}},
          "l1":{"op":"over","inputs":{"fg":"a","bg":"bg"}},
          "l2":{"op":"over","inputs":{"fg":"b","bg":"l1"}}}}"##,
    )
    .unwrap();
    let device = Device::new().unwrap();
    let mut r = Renderer::new(device.clone()).unwrap();
    export_video(&mut r, &reference, &clip, ExportOptions::default(), |_, _| {}).unwrap();

    // A draft as OCR would make it: right text, roughly placed, default font.
    let mut draft = Document::from_json(
        r##"{"version":1,"format":{"width":960,"height":360,"fps":30,"duration":0.5},"output":"l2",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#1d2b44"}},
          "a":{"op":"text","params":{"text":"Quiet Harbour","size":60,"position":[80,118],"valign":"middle","color":"#ffaa00"}},
          "b":{"op":"text","params":{"text":"Field notes from the coast","size":34,"position":[84,238],"valign":"middle","color":"#ffffff"}},
          "l1":{"op":"over","inputs":{"fg":"a","bg":"bg"}},
          "l2":{"op":"over","inputs":{"fg":"b","bg":"l1"}}}}"##,
    )
    .unwrap();
    for (node, family, weight, size, tracking) in
        [("a", "Georgia", 700.0, 72.0, 0.0), ("b", "Futura", 500.0, 40.0, 2.0)]
    {
        let (out, report) = match_font(&device, &draft, &dir, &clip, node, 0.25, &MatchOptions::default()).unwrap();
        let p = &out.nodes[node].params;
        assert_eq!(p.get("font"), Some(&Param::Text(family.into())), "{report:#}");
        assert_eq!(p.get("weight"), Some(&Param::Number(weight)), "{report:#}");
        let got = |k: &str| match p.get(k) {
            Some(Param::Number(v)) => *v,
            _ => 0.0,
        };
        assert!((got("size") - size).abs() < 0.5, "size {} {report:#}", got("size"));
        assert!(
            (got("tracking") - tracking).abs() < 0.3,
            "tracking {} {report:#}",
            got("tracking")
        );
        draft = out;
    }
}
