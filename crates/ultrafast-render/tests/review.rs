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

/// Type put in perspective by a `corner_pin` is measured where the pin
/// puts it: dark words pinned onto a light panel read fine, though where
/// their raster was drawn (before the pin) the frame is black.
#[test]
fn reads_type_where_a_corner_pin_puts_it() {
    let doc = format!(
        r##"{{"version":1,"format":{{"width":480,"height":270,"fps":30,"duration":1}},"output":"out","nodes":{{
          "dark":{{"op":"solid","params":{{"color":"#000000"}}}},
          "panel":{{"op":"shape","params":{{"kind":"rect","size":[180,270],"center":[390,135],"color":"#f4f1ea"}}}},
          "bg":{{"op":"over","inputs":{{"fg":"panel","bg":"dark"}}}},
          "label":{{"op":"text","params":{{"text":"Pinned","font":"{FONT}","size":40,"position":[30,70],"color":"#14141a"}}}},
          "pinned":{{"op":"corner_pin","inputs":{{"source":"label"}},"params":{{"source_rect":[20,30,160,60],"corners":[310,105, 470,98, 470,165, 310,158]}}}},
          "out":{{"op":"over","inputs":{{"fg":"pinned","bg":"bg"}}}}}}}}"##
    );
    let k = kinds(&doc);
    assert!(!k.iter().any(|(_, n)| n == "label"), "{k:?}");
    // The same words unpinned, where they were drawn: black on black.
    let unpinned = doc.replace(r#""fg":"pinned""#, r#""fg":"label""#);
    let k = kinds(&unpinned);
    assert!(k.iter().any(|(_, n)| n == "label"), "{k:?}");
}

/// A label faded out far up a deep stack of layers is not on screen, and
/// isn't reviewed as if it were (the walk to the output once gave up after
/// 48 levels and took whatever was below for visible).
#[test]
fn follows_type_up_deep_stacks() {
    let mut nodes = vec![
        r##""bg":{"op":"solid","params":{"color":"#202024"}}"##.to_string(),
        format!(
            r##""label":{{"op":"text","params":{{"text":"Hidden here","font":"{FONT}","size":40,"position":[40,150],"color":"#f4f1ea"}}}}"##
        ),
    ];
    let mut below = "label".to_string();
    for i in 0..60 {
        nodes.push(format!(
            r##""dot{i}":{{"op":"shape","params":{{"kind":"circle","size":[4,4],"center":[{x},20],"color":"#ffffff"}}}}"##,
            x = 100 + i * 5
        ));
        nodes.push(format!(
            r##""s{i}":{{"op":"over","inputs":{{"fg":"dot{i}","bg":"{below}"}}}}"##
        ));
        below = format!("s{i}");
    }
    nodes.push(format!(
        r##""gone":{{"op":"opacity","inputs":{{"source":"{below}"}},"params":{{"opacity":0}}}}"##
    ));
    nodes.push(r##""out":{"op":"over","inputs":{"fg":"gone","bg":"bg"}}"##.to_string());
    let doc = format!(
        r##"{{"version":1,"format":{{"width":480,"height":270,"fps":30,"duration":1}},"output":"out","nodes":{{{}}}}}"##,
        nodes.join(",")
    );
    let k = kinds(&doc);
    assert!(!k.iter().any(|(_, n)| n == "label"), "{k:?}");
}
