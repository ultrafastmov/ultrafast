//! The reviewer finds what it should, measured on rendered frames.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;
use ultrafast_render::review::{ReviewOptions, review};

fn kinds(doc: &str) -> Vec<(String, String)> {
    let d = Document::from_json(doc).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let rv = review(&mut r, &d, &ReviewOptions::default()).unwrap();
    rv.issues.into_iter().map(|i| (i.kind.to_string(), i.node)).collect()
}

const FONT: &str = "../../examples/assets/fonts/Geist-VF.ttf";

#[test]
fn flags_low_contrast_clipping_brevity_and_flashing() {
    let doc = format!(
        r##"{{"version":1,"format":{{"width":480,"height":270,"fps":30,"duration":2}},"output":"out","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#808080"}}}},
          "faint":{{"op":"text","params":{{"text":"faint words here","font":"{FONT}","size":36,"position":[40,80],"color":"#a8a8a8"}}}},
          "wide":{{"op":"text","params":{{"text":"Overflowing","font":"{FONT}","size":90,"position":[300,150],"color":"#000000"}}}},
          "blink":{{"op":"text","params":{{"text":"read this whole sentence quickly","font":"{FONT}","size":24,"position":[40,230],"color":"#ffffff"}},"timing":{{"in":0.5,"out":0.8}}}},
          "strobe":{{"op":"solid","params":{{"color":"#ffffff"}}}},
          "pulse":{{"op":"opacity","params":{{"opacity":{{"oscillate":{{"frequency":6,"amplitude":0.5,"base":0.5,"wave":"square"}}}}}},"inputs":{{"source":"strobe"}}}},
          "a":{{"op":"over","inputs":{{"fg":"faint","bg":"bg"}}}},
          "b":{{"op":"over","inputs":{{"fg":"wide","bg":"a"}}}},
          "c":{{"op":"over","inputs":{{"fg":"blink","bg":"b"}}}},
          "out":{{"op":"over","inputs":{{"fg":"pulse","bg":"c"}},"timing":{{"in":1.0}}}}}}}}"##
    );
    let k = kinds(&doc);
    let has = |kind: &str, node: &str| k.iter().any(|(a, b)| a == kind && b == node);
    assert!(has("low_contrast", "faint"), "{k:?}");
    assert!(has("clipped", "wide"), "{k:?}");
    assert!(has("too_brief", "blink"), "{k:?}");
    assert!(k.iter().any(|(a, _)| a == "flashing"), "{k:?}");
    assert!(!has("low_contrast", "wide"), "{k:?}");
}

#[test]
fn clean_title_has_no_issues() {
    let doc = format!(
        r##"{{"version":1,"format":{{"width":480,"height":270,"fps":30,"duration":2}},"output":"out","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#101014"}}}},
          "title":{{"op":"text","params":{{"text":"Hello","font":"{FONT}","size":72,"position":[240,150],"align":"center","color":"#f4f1ea"}}}},
          "out":{{"op":"over","inputs":{{"fg":"title","bg":"bg"}}}}}}}}"##
    );
    assert_eq!(kinds(&doc), vec![]);
}
