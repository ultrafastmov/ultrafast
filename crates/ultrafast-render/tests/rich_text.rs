//! Inline styling: spans take their own color, highlights sit behind the
//! type, and `em_draw` sweeps marks across lines.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

fn render(text: &str, extra: &str) -> Vec<u8> {
    let d = Document::from_json(&format!(
        r##"{{"version":1,"format":{{"width":600,"height":200,"fps":30,"duration":1}},"output":"o","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#000000"}}}},
          "t":{{"op":"text","params":{{"text":"{text}","font":"ultrafast:sans","size":80,"weight":800,
                 "color":"#ffffff","position":[20,40]{extra}}}}},
          "o":{{"op":"over","inputs":{{"fg":"t","bg":"bg"}}}}}}}}"##
    ))
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
    px
}

/// Counts of pixels in `x0..x1` that are mostly red, mostly white, and
/// yellow-ish (red and green high, blue low).
fn tally(px: &[u8], x0: usize, x1: usize) -> (usize, usize, usize) {
    let (mut red, mut white, mut yellow) = (0, 0, 0);
    for y in 0..200 {
        for x in x0..x1 {
            let i = (y * 600 + x) * 4;
            let (r, g, b) = (px[i], px[i + 1], px[i + 2]);
            if r > 200 && g < 60 && b < 60 {
                red += 1;
            }
            if r > 200 && g > 200 && b > 200 {
                white += 1;
            }
            if r > 200 && g > 160 && b < 90 {
                yellow += 1;
            }
        }
    }
    (red, white, yellow)
}

#[test]
fn spans_color_their_glyphs_and_highlights_sit_behind() {
    // "AB" white then "CD" red: the left half is white ink, the right red.
    let px = render("AB[CD]{color=#ff0000}", "");
    let (r_left, w_left, _) = tally(&px, 0, 130);
    let (r_right, w_right, _) = tally(&px, 130, 600);
    assert!(w_left > 500 && r_left == 0, "left: white {w_left}, red {r_left}");
    assert!(r_right > 500 && w_right < 20, "right: white {w_right}, red {r_right}");

    // A highlight is a filled box behind white type.
    let px = render("AB [CD]{highlight=#ffd000}", "");
    let (_, w, y) = tally(&px, 130, 600);
    assert!(y > 3000, "highlight box {y}");
    assert!(w > 400, "type over it {w}");
}

#[test]
fn emphasis_takes_em_params_and_draw_sweeps() {
    let px = render("AB *CD*", r##","em_color":"#ff0000""##);
    let (r, _, _) = tally(&px, 130, 600);
    assert!(r > 500, "emphasis red {r}");
    // Half drawn: the highlight covers about half its full width.
    let full = tally(&render("AB *CD*", r##","em_highlight":"#ffd000""##), 0, 600).2;
    let half = tally(
        &render("AB *CD*", r##","em_highlight":"#ffd000","em_draw":0.5"##),
        0,
        600,
    )
    .2;
    let ratio = half as f64 / full as f64;
    assert!((0.35..0.65).contains(&ratio), "half drawn {half}/{full}");
}
