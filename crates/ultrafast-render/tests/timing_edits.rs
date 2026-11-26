//! A timing edit is a value edit: the renderer keeps its plan and plays the
//! film to the new timing. (Sessions that pick up an edited file, `uf mcp`
//! and the player, rendered a node at its old offset until a structural
//! edit replanned.)

use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Edit, Timing};
use ultrafast_render::Renderer;

#[test]
fn a_timing_edit_renders_like_the_edited_file_loaded_fresh() {
    // A box fading in over 0.5 s of its local time, from 1 s.
    let src = r##"{"version":1,"format":{"width":160,"height":90,"fps":30,"duration":3},"output":"out","nodes":{
      "bg":{"op":"solid","params":{"color":"#000000"}},
      "box":{"op":"shape","timing":{"in":1,"offset":1},"params":{"kind":"rect","center":[80,45],"size":[60,30],"color":"#ffffff",
        "opacity":{"keys":[{"t":0,"v":0},{"t":0.5,"v":1}]}}},
      "out":{"op":"over","inputs":{"fg":"box","bg":"bg"}}}}"##;
    let mut doc = Document::from_json(src).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let before = r.render_rgba8(&doc, 1.25).unwrap().0;
    // From 0.75 s instead: at 1.25 s it is fully in, not half.
    doc.apply(&[Edit::SetTiming {
        node: "box".into(),
        timing: Some(Timing {
            in_point: Some(0.75),
            offset: 0.75,
            ..Default::default()
        }),
    }])
    .unwrap();
    let edited = r.render_rgba8(&doc, 1.25).unwrap().0;
    let reloaded = Document::from_json(&doc.to_json()).unwrap();
    let fresh = Renderer::new(Device::new().unwrap())
        .unwrap()
        .render_rgba8(&reloaded, 1.25)
        .unwrap()
        .0;
    let centre = |px: &[u8]| px[(45 * 160 + 80) * 4];
    assert!(
        centre(&before) < 200,
        "half faded in before the edit: {}",
        centre(&before)
    );
    assert_eq!(centre(&fresh), 255);
    assert!(edited == fresh, "the edited timing renders as the file would load");
}
