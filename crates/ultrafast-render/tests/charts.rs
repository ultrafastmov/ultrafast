//! Chart blocks build from `values`: bars end proportional to their values,
//! a donut's segments cover the ring in their shares.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

fn render(block: &str, t: f64) -> Vec<u8> {
    let d = Document::from_json(&format!(
        r##"{{"version":1,"format":{{"width":800,"height":800,"fps":30,"duration":6}},"output":"o","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#000000"}}}},
          "c":{{"op":"block","params":{block}}},
          "o":{{"op":"over","inputs":{{"fg":"c","bg":"bg"}}}}}}}}"##
    ))
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, t).unwrap();
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
    px
}

/// Tallest run of `pred` pixels down column `x`.
fn column_run(px: &[u8], x: usize, pred: impl Fn([u8; 3]) -> bool) -> usize {
    let (mut best, mut run) = (0, 0);
    for y in 0..800 {
        let i = (y * 800 + x) * 4;
        if pred([px[i], px[i + 1], px[i + 2]]) {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

#[test]
fn bars_end_proportional_to_values() {
    let px = render(
        r##"{"kind":"bar_chart","values":[1,2],"color":"#00ff00","accent":"#ff0000","height":300,"position":[100,100],"width":600}"##,
        3.0,
    );
    // Two bars centered in 600 px from x = 100: find each one's tallest column.
    let green = (100..700)
        .map(|x| column_run(&px, x, |c| c[1] > 200 && c[0] < 60))
        .max()
        .unwrap();
    let red = (100..700)
        .map(|x| column_run(&px, x, |c| c[0] > 200 && c[1] < 60))
        .max()
        .unwrap();
    assert!((red as i32 - 300).abs() <= 3, "tall bar {red}");
    assert!((green as i32 - 150).abs() <= 3, "short bar {green}");
}

#[test]
fn donut_segments_take_their_shares() {
    let px = render(
        r##"{"kind":"donut","values":[3,1],"colors":"#ff0000,#0000ff","position":[100,100],"width":600,"height":600}"##,
        4.0,
    );
    let (mut red, mut blue) = (0usize, 0usize);
    for c in px.chunks_exact(4) {
        if c[0] > 200 && c[2] < 60 && c[1] < 60 {
            red += 1;
        }
        if c[2] > 200 && c[0] < 60 && c[1] < 60 {
            blue += 1;
        }
    }
    let share = red as f64 / (red + blue) as f64;
    assert!((share - 0.75).abs() < 0.03, "red share {share} ({red} / {blue})");
}
