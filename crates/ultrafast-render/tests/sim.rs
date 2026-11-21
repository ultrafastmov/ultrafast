//! Simulations are pure functions of the document and the frame: a frame
//! reached by playing, by seeking forward, or by seeking back is the same.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

fn fluid_doc(motion_blur: bool) -> Document {
    let mb = if motion_blur {
        r#","motion_blur":{"samples":3,"shutter":0.5}"#
    } else {
        ""
    };
    Document::from_json(&format!(
        r##"{{"version":1,"format":{{"width":96,"height":64,"fps":30,"duration":3{mb}}},
        "output":"out","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#101010"}}}},
          "blob":{{"op":"shape","params":{{"kind":"ellipse","center":{{"keys":[{{"t":0,"v":[30,32]}},{{"t":2,"v":[70,30]}}]}},"size":[18,18],"color":"#ff6040"}}}},
          "ink":{{"op":"fluid","params":{{"turbulence":800,"turbulence_scale":30,"vorticity":8,"resolution":0.5,"dye_resolution":1}},"inputs":{{"dye":"blob","stir":"blob"}}}},
          "out":{{"op":"over","inputs":{{"fg":"ink","bg":"bg"}}}}}}}}"##
    ))
    .unwrap()
}

fn feedback_doc() -> Document {
    Document::from_json(
        r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":3},
        "output":"trail","nodes":{
          "dot":{"op":"shape","params":{"kind":"ellipse","center":{"keys":[{"t":0,"v":[8,32]},{"t":2,"v":[56,32]}]},"size":[6,6],"color":"#ffffff"}},
          "trail":{"op":"feedback","params":{"code":"float4 update(float2 pos, float px) { float4 p = prev(pos) * 0.9; float4 i = input(0, pos); return i + p * (1.0 - i.a); }"},"inputs":{"in0":"dot"}}}}"##,
    )
    .unwrap()
}

fn frame(r: &mut Renderer, d: &Document, f: u64) -> Vec<u8> {
    r.render_rgba8(d, d.format.time_of(f)).unwrap().0
}

fn max_diff(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).abs())
        .max()
        .unwrap()
}

fn check(d: &Document) {
    // Played frame by frame.
    let mut played = Renderer::new(Device::new().unwrap()).unwrap();
    let mut seq = Vec::new();
    for f in 0..=40 {
        seq.push(frame(&mut played, d, f));
    }
    // The simulation moves.
    assert!(max_diff(&seq[10], &seq[40]) > 20);
    // Seeking straight to a frame.
    let mut seek = Renderer::new(Device::new().unwrap()).unwrap();
    assert_eq!(max_diff(&frame(&mut seek, d, 40), &seq[40]), 0, "seek forward");
    // Seeking back (resumes from a checkpoint) and re-rendering a frame.
    assert_eq!(max_diff(&frame(&mut seek, d, 22), &seq[22]), 0, "seek back");
    assert_eq!(max_diff(&frame(&mut seek, d, 22), &seq[22]), 0, "same frame twice");
    assert_eq!(
        max_diff(&frame(&mut seek, d, 3), &seq[3]),
        0,
        "back before any checkpoint"
    );
    assert_eq!(max_diff(&frame(&mut seek, d, 31), &seq[31]), 0, "forward again");
}

#[test]
fn fluid_frames_do_not_depend_on_how_they_are_reached() {
    check(&fluid_doc(false));
}

#[test]
fn fluid_with_motion_blur_steps_once_per_frame() {
    check(&fluid_doc(true));
}

#[test]
fn feedback_frames_do_not_depend_on_how_they_are_reached() {
    let d = feedback_doc();
    check(&d);
    // The trail: the dot's past positions linger, fading.
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let px = frame(&mut r, &d, 60);
    let at = |x: usize| px[(32 * 64 + x) * 4 + 3];
    assert!(at(56) > 200, "dot {}", at(56));
    assert!(at(45) > 10 && at(45) < at(54), "trail {} {}", at(45), at(54));
}
