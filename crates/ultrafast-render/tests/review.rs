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

/// Two cards, "every" on pink and "cut" on `paper1` (its node JSON; a
/// `strip` of a `solid1` is there to move), the second cut in over the
/// first by `cut` (the output's node JSON).
fn cards(paper1: &str, cut: &str) -> String {
    format!(
        r##"{{"version":1,"format":{{"width":480,"height":270,"fps":30,"duration":3}},"output":"out","nodes":{{
          "paper0":{{"op":"solid","params":{{"color":"#ff5c7a"}}}},
          "word0":{{"op":"text","params":{{"text":"every","font":"{FONT}","size":80,"position":[240,135],"align":"center","valign":"middle","color":"#14121c"}}}},
          "card0":{{"op":"over","inputs":{{"fg":"word0","bg":"paper0"}}}},
          "solid1":{{"op":"solid","params":{{"color":"#f6f3ee"}}}},
          "strip":{{"op":"crop","inputs":{{"source":"solid1"}},"params":{{"rect":[0,0,260,270]}}}},
          "paper1":{paper1},
          "word1":{{"op":"text","params":{{"text":"cut","font":"{FONT}","size":80,"position":[240,135],"align":"center","valign":"middle","color":"#14121c"}}}},
          "card1":{{"op":"over","inputs":{{"fg":"word1","bg":"paper1"}}}},
          "out":{cut}}}}}"##
    )
}

/// The type `inspect` lists at `t`, and what the reviewer measures there.
fn on_screen(r: &mut Renderer, doc: &str, t: f64) -> (Vec<String>, Vec<String>) {
    let d = Document::from_json(doc).unwrap();
    let (_, probes) = r.probe_texts(&d, t).unwrap();
    r.render_rgba8(&d, t).unwrap();
    let mut listed: Vec<String> = r
        .visible(&d, t)
        .into_iter()
        .filter(|v| v.text.is_some())
        .map(|v| v.element.node)
        .collect();
    let mut probed: Vec<String> = probes.into_iter().map(|p| p.node).collect();
    listed.sort();
    probed.sort();
    (listed, probed)
}

/// Type under an opaque card cut in over it is not on screen: `inspect`
/// doesn't list it and the reviewer doesn't measure it (it once read the
/// card's own word through it as dim type). What shows through a card
/// (translucent, faded, blended, or not covering all of it) still counts.
#[test]
fn type_under_an_opaque_card_is_not_on_screen() {
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let solid = r##"{"op":"solid","params":{"color":"#f6f3ee"}}"##;
    let over = r#"{"op":"over","inputs":{"fg":"card1","bg":"card0"},"timing":{"in":1,"out":2}}"#;
    let one = |w: &str| (vec![w.to_string()], vec![w.to_string()]);
    let both = (
        vec!["word0".to_string(), "word1".to_string()],
        vec!["word0".to_string(), "word1".to_string()],
    );
    let doc = cards(solid, over);
    assert_eq!(on_screen(&mut r, &doc, 0.5), one("word0"));
    assert_eq!(on_screen(&mut r, &doc, 1.5), one("word1"));
    assert_eq!(on_screen(&mut r, &doc, 2.5), one("word0"));
    // Covered by a panel (a shape, a cropped solid slid over it).
    let panel = r##"{"op":"shape","params":{"size":[300,120],"center":[240,140],"color":"#f6f3ee"}}"##;
    assert_eq!(on_screen(&mut r, &cards(panel, over), 1.5), one("word1"));
    let slid = r#"{"op":"transform","inputs":{"source":"strip"},"params":{"position":[340,135]}}"#;
    assert_eq!(on_screen(&mut r, &cards(slid, over), 1.5), one("word1"));
    // Showing through.
    let cases = [
        cards(r##"{"op":"solid","params":{"color":"#f6f3eecc"}}"##, over),
        cards(
            solid,
            &over.replace(r#""timing""#, r#""params":{"opacity":0.9},"timing""#),
        ),
        cards(
            solid,
            &over.replace(r#""op":"over""#, r#""op":"blend","params":{"mode":"multiply"}"#),
        ),
        cards(&panel.replace("[300,120]", "[150,120]"), over),
        cards(&slid.replace("[340,135]", "[340,135],\"rotation\":10"), over),
    ];
    for doc in cases {
        assert_eq!(on_screen(&mut r, &doc, 1.5), both, "{doc}");
    }
}
