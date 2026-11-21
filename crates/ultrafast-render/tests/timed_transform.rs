//! A timed transform outside its window passes its source through: a path or
//! text absorbed into it keeps drawing, unmoved, at its own times.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

const FONT: &str = "../../examples/assets/fonts/Geist-VF.ttf";

fn pixel(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    let i = (y * width + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[test]
fn shapes_and_type_under_an_inactive_timed_transform_still_draw() {
    // A star (1-2 s) and a word (1-2 s) under a push-in timed 0-0.5 s.
    let doc = format!(
        r##"{{"version":1,"format":{{"width":480,"height":270,"fps":30,"duration":3}},"output":"push","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#000000"}}}},
          "star":{{"op":"path","timing":{{"in":1,"out":2,"offset":1}},"params":{{"kind":"star","center":[120,135],"radius":80,"inner_radius":50,"sides":14,"color":"#ff0000",
            "scale":{{"keys":[{{"t":0,"v":[0.5]}},{{"t":0.3,"v":[1]}}]}}}}}},
          "word":{{"op":"text","timing":{{"in":1,"out":2,"offset":1}},"params":{{"text":"HI","font":"{FONT}","size":120,"weight":800,"position":[360,135],"anchor":[0,0],"color":"#00ff00"}}}},
          "a":{{"op":"over","inputs":{{"fg":"star","bg":"bg"}},"timing":{{"in":1,"out":2,"offset":1}}}},
          "b":{{"op":"over","inputs":{{"fg":"word","bg":"a"}},"timing":{{"in":1,"out":2,"offset":1}}}},
          "push":{{"op":"transform","inputs":{{"source":"b"}},"timing":{{"in":0,"out":0.5,"offset":0}},
            "params":{{"anchor":[240,135],"position":[240,135],"scale":{{"keys":[{{"t":0,"v":[1]}},{{"t":0.5,"v":[1.2]}}]}}}}}}}}}}"##
    );
    let doc = Document::from_json(&doc).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let (rgba, _) = r.render_rgba8(&doc, 1.5).unwrap();
    let star = pixel(&rgba, 480, 120, 135);
    assert!(star[0] > 200 && star[1] < 60, "the star is missing at 1.5 s: {star:?}");
    let green = (300..470)
        .flat_map(|x| (60..210).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = pixel(&rgba, 480, x, y);
            p[1] > 200 && p[0] < 60
        })
        .count();
    assert!(green > 200, "the word is missing at 1.5 s ({green} green pixels)");
}
