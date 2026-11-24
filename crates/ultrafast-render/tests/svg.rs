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

/// Renders `svg` (200 x 200) over white and returns the pixel at (x, y).
fn render_svg(svg: &str, name: &str) -> impl Fn(usize, usize) -> [u8; 3] {
    let dir = std::env::temp_dir().join(format!("ultrafast_svg_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("art.svg"), svg).unwrap();
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":200,"height":200,"fps":30,"duration":1},"output":"out","nodes":{
          "bg":{"op":"solid","params":{"color":"#ffffff"}},
          "art":{"op":"svg","params":{"src":"art.svg"}},
          "out":{"op":"over","inputs":{"fg":"art","bg":"bg"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(&dir);
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    move |x, y| {
        let i = (y * 200 + x) * 4;
        [px[i], px[i + 1], px[i + 2]]
    }
}

/// `mix-blend-mode` becomes a `blend`, against everything under it in the
/// SVG (a group with only a transform is no layer of its own), and
/// translucency composites in sRGB, as in a browser.
#[test]
fn blend_modes_and_translucency_composite_like_a_browser() {
    let at = render_svg(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200">
          <rect width="200" height="100" fill="#ffffff"/>
          <circle cx="60" cy="50" r="40" fill="#ff0000" opacity="0.5"/>
          <rect y="100" width="120" height="100" fill="#ff0000"/>
          <g transform="translate(80 100)"><rect width="120" height="100" fill="#00ff00" style="mix-blend-mode:multiply"/></g>
        </svg>"##,
        "blend",
    );
    // Red at 50% over the SVG's white: a browser's (255, 128, 128), not
    // linear light's (255, 188, 188).
    let pink = at(60, 50);
    assert!(
        pink[0] == 255 && (126..=130).contains(&pink[1]) && pink[1] == pink[2],
        "{pink:?}"
    );
    // Green multiplied over red is black where they overlap; green alone
    // (multiplied over the white page) stays green.
    assert_eq!(at(100, 150), [0, 0, 0]);
    assert_eq!(at(180, 150), [0, 255, 0]);
    assert_eq!(at(40, 150), [255, 0, 0]);
}
