//! SVG files become editable vector nodes that render like the SVG.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 200" width="300" height="200">
  <defs>
    <clipPath id="c"><circle cx="50" cy="100" r="40"/></clipPath>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient>
  </defs>
  <g transform="translate(100 0)"><rect id="sq" x="0" y="50" width="100" height="100" fill="#3355ff" clip-path="url(#c)"/></g>
  <rect id="bar" x="210" y="20" width="80" height="40" fill="url(#g)"/>
  <path id="line" d="M 10 190 L 290 190" stroke="#00ff00" stroke-width="6"/>
</svg>"##;

#[test]
fn imports_shapes_clips_gradients_and_strokes() {
    let imp = ultrafast_render::svg::import(SVG.as_bytes(), None, Some([600.0, 400.0])).unwrap();
    assert_eq!(imp.size, [600.0, 400.0]);
    for id in ["sq", "bar", "line_stroke"] {
        assert!(
            imp.component.nodes.contains_key(id),
            "{id}: {:?}",
            imp.component.nodes.keys()
        );
    }
    let dir = std::env::temp_dir().join(format!("ultrafast_svg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("art.svg"), SVG).unwrap();
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":600,"height":400,"fps":30,"duration":1},"output":"out","nodes":{
          "bg":{"op":"solid","params":{"color":"#ffffff"}},
          "art":{"op":"svg","params":{"src":"art.svg","size":[600,400]}},
          "out":{"op":"over","inputs":{"fg":"art","bg":"bg"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(&dir);
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    let at = |x: usize, y: usize| {
        let i = (y * 600 + x) * 4;
        [px[i], px[i + 1], px[i + 2]]
    };
    // Inside the clip circle (center 300,200 at 2x): the square's blue.
    assert_eq!(at(300, 200), [0x33, 0x55, 0xff]);
    // Inside the square but outside the clip: background.
    assert_eq!(at(210, 110), [255, 255, 255]);
    // Gradient runs red to blue across the bar.
    let (l, rr) = (at(425, 80), at(575, 80));
    assert!(l[0] > 200 && l[2] < 60 && rr[2] > 200 && rr[0] < 60, "{l:?} {rr:?}");
    // The stroked line.
    assert_eq!(at(300, 380), [0, 255, 0]);
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
}
