//! `stack` places children by their measured bounds.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

#[test]
fn stacks_children_by_measured_bounds() {
    let font = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/assets/fonts/Geist-VF.ttf");
    let d = Document::from_json(&format!(
        r##"{{"version":1,"format":{{"width":400,"height":400,"fps":30,"duration":1}},"output":"o","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#000000"}}}},
          "a":{{"op":"shape","params":{{"kind":"rect","center":[300,300],"size":[100,40],"color":"#ff0000"}}}},
          "t":{{"op":"text","params":{{"text":"Hi","font":"{font}","size":60,"color":"#ffffff"}}}},
          "b":{{"op":"shape","params":{{"kind":"rect","center":[0,0],"size":[60,30],"color":"#00ff00"}}}},
          "row":{{"op":"stack","params":{{"direction":"row","gap":10}},"inputs":{{"in0":"b","in1":"c"}}}},
          "c":{{"op":"shape","params":{{"kind":"rect","center":[0,0],"size":[20,30],"color":"#0000ff"}}}},
          "s":{{"op":"stack","params":{{"gap":20,"position":[200,50],"anchor":"top","align":"center"}},"inputs":{{"in0":"a","in1":"t","in2":"row"}}}},
          "o":{{"op":"over","inputs":{{"fg":"s","bg":"bg"}}}}}}}}"##
    ))
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
    let at = |x: usize, y: usize| {
        let i = (y * 400 + x) * 4;
        [px[i], px[i + 1], px[i + 2]]
    };
    // The red bar: centered on x = 200, top at y = 50.
    assert_eq!(at(200, 52), [255, 0, 0]);
    assert_eq!(at(152, 70), [255, 0, 0]);
    assert_eq!(at(200, 48), [0, 0, 0]);
    // The text sits 20 px below it; the row 20 px below the text, centered:
    // green 60 wide, 10 gap, blue 20 wide = 90, from x = 155.
    let text_rows: Vec<usize> = (90..260)
        .filter(|&y| (0..400).any(|x| at(x, y)[0] > 200 && at(x, y)[1] > 200))
        .collect();
    let (t0, t1) = (*text_rows.first().unwrap(), *text_rows.last().unwrap());
    assert!((t0 as i32 - 110).abs() <= 2, "text top {t0}");
    let row_y = t1 + 21 + 15;
    assert_eq!(at(156, row_y), [0, 255, 0], "row at {row_y}");
    assert_eq!(at(234, row_y), [0, 0, 255]);
    assert_eq!(at(220, row_y), [0, 0, 0]);
}

#[test]
fn stacks_inside_a_component_lay_out_like_top_level_ones() {
    let font = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/assets/fonts/Geist-VF.ttf");
    let parts = |label: &str| {
        format!(
            r##""a":{{"op":"shape","params":{{"kind":"rect","center":[300,300],"size":[100,40],"color":"#ff0000"}}}},
          "t":{{"op":"text","params":{{"text":"{label}","font":"{font}","size":60,"color":"#ffffff"}}}},
          "s":{{"op":"stack","params":{{"gap":20,"position":[200,50],"anchor":"top","align":"center"}},"inputs":{{"in0":"a","in1":"t"}}}}"##
        )
    };
    let top = format!(
        r##"{{"version":1,"format":{{"width":400,"height":400,"fps":30,"duration":1}},"output":"o","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#000000"}}}}, {},
          "o":{{"op":"over","inputs":{{"fg":"s","bg":"bg"}}}}}}}}"##,
        parts("Hi")
    );
    let nested = format!(
        r##"{{"version":1,"format":{{"width":400,"height":400,"fps":30,"duration":1}},"output":"o",
        "components":{{"card":{{"output":"s","params":{{"label":""}},"nodes":{{ {} }}}}}},
        "nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#000000"}}}},
          "k":{{"op":"card","params":{{"label":"Hi"}}}},
          "o":{{"op":"over","inputs":{{"fg":"k","bg":"bg"}}}}}}}}"##,
        parts("$label")
    );
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (a, _) = r.render_rgba8(&Document::from_json(&top).unwrap(), 0.0).unwrap();
    let (b, _) = r.render_rgba8(&Document::from_json(&nested).unwrap(), 0.0).unwrap();
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
    let diff = a
        .iter()
        .zip(&b)
        .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 2)
        .count();
    assert_eq!(diff, 0, "{diff} bytes differ");
}
