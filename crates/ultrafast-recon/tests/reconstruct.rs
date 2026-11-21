//! Whole-video reconstruction: two shots (gradient + fading title, solid +
//! centered lines) come back as editable layers with the right fonts,
//! backgrounds, alignment and fade.
#![cfg(all(target_os = "macos", not(feature = "vulkan")))]

use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Param};
use ultrafast_live::export::{ExportOptions, export_video};
use ultrafast_recon::reconstruct::{ReconstructOptions, reconstruct};
use ultrafast_render::Renderer;

#[test]
fn rebuilds_two_shots() {
    let dir = std::env::temp_dir().join(format!("ultrafast-recon-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("film.mp4");
    let film = Document::from_json(
        r##"{"version":1,"format":{"width":960,"height":540,"fps":30,"duration":3},"output":"s2",
        "nodes":{
          "a_bg":{"op":"gradient","params":{"a":"#14213d","b":"#2a4a7f","start":[0,0],"end":[960,540]}},
          "a_t":{"op":"text","params":{"text":"Quiet Harbour","font":"Georgia","weight":700,"size":80,
                 "position":[100,250],"valign":"baseline","color":"#fca311"}},
          "a_l":{"op":"over","inputs":{"fg":"a_t","bg":"a_bg"},
                 "params":{"opacity":{"keys":[{"t":0.2,"v":0,"ease":"in_out_cubic"},{"t":0.8,"v":1}]}}},
          "b_bg":{"op":"solid","params":{"color":"#f4f1de"}},
          "b_t":{"op":"text","params":{"text":"Chapter Two","font":"Avenir Next","weight":700,"size":90,
                 "position":[480,300],"align":"center","valign":"baseline","color":"#3d405b"}},
          "b_l":{"op":"over","inputs":{"fg":"b_t","bg":"b_bg"}},
          "s2":{"op":"over","inputs":{"fg":"b_l","bg":"a_l"},"timing":{"in":1.5,"out":3}}}}"##,
    )
    .unwrap();
    let device = Device::new().unwrap();
    let mut r = Renderer::new(device.clone()).unwrap();
    export_video(&mut r, &film, &clip, ExportOptions::default(), |_, _| {}).unwrap();

    let (doc, report) = reconstruct(&device, &clip, &dir.join("out"), &ReconstructOptions::default(), |_| {}).unwrap();
    assert_eq!(report["shots"].as_array().unwrap().len(), 2, "{report:#}");
    let p = |id: &str, k: &str| doc.nodes[id].params.get(k).cloned();
    assert_eq!(p("s1_text1", "font"), Some(Param::Text("Georgia".into())), "{report:#}");
    assert_eq!(
        p("s2_text1", "font"),
        Some(Param::Text("Avenir Next".into())),
        "{report:#}"
    );
    assert_eq!(p("s2_text1", "align"), Some(Param::Text("center".into())), "{report:#}");
    match p("s2_text1", "position") {
        Some(Param::Vector(v)) => assert!(v[0] == 480.0 && (v[1] - 300.0).abs() < 0.1, "{v:?}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(doc.nodes["s1_bg"].op, "gradient");
    assert_eq!(doc.nodes["s2_bg"].op, "solid");
    assert!(
        matches!(p("s1_layer1", "opacity"), Some(Param::Animated { .. })),
        "{report:#}"
    );
    assert!(report["mean_psnr"].as_f64().unwrap() > 35.0, "{report:#}");
}
