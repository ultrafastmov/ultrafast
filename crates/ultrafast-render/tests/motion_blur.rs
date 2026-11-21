//! Motion blur samples only what moves, and looks the same as sampling
//! everything.

use serde_json::json;
use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

fn doc() -> Document {
    Document::from_json(
        r##"{"version":1,"format":{"width":160,"height":90,"fps":30,"duration":2,"motion_blur":{"samples":8,"shutter":0.5}},
        "output":"graded","nodes":{
          "bg":{"op":"solid","params":{"color":"#202028"}},
          "ball":{"op":"shape","params":{"kind":"ellipse","size":[20,20],"color":"#a04030",
             "center":{"keys":[{"t":0,"v":[20,45]},{"t":0.25,"v":[140,45]},{"t":2,"v":[140,45]}]}}},
          "label":{"op":"shape","params":{"kind":"rect","center":[80,15],"size":[60,10],"color":"#ffffff"}},
          "a":{"op":"over","inputs":{"fg":"ball","bg":"bg"}},
          "b":{"op":"over","inputs":{"fg":"label","bg":"a"}},
          "glow":{"op":"blur","params":{"radius":4},"inputs":{"source":"b"}},
          "lit":{"op":"blend","params":{"mode":"add","opacity":0.3},"inputs":{"fg":"glow","bg":"b"}},
          "graded":{"op":"adjust","params":{"exposure":0.0,"contrast":1.1},"inputs":{"source":"lit"}}}}"##,
    )
    .unwrap()
}

#[test]
fn samples_only_moving_layers_and_matches_full_sampling() {
    let d = doc();
    let mut fast = Renderer::new(Device::new().unwrap()).unwrap();
    let mut full = Renderer::new(Device::new().unwrap()).unwrap();
    full.set_full_motion_blur(true);
    for t in [0.125, 1.5] {
        let (a, ra) = fast.render_rgba8(&d, t).unwrap();
        let (b, _) = full.render_rgba8(&d, t).unwrap();
        let worst = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 3, "t={t}: differs by {worst}");
        if t < 1.0 {
            // The ball moves: sampled, but the glow and grade run once.
            assert_eq!(ra.mb_samples, 8);
            assert!(ra.mb_split < fast.plan().unwrap().groups.len() - 1, "{}", ra.mb_split);
        } else {
            // Nothing moves: one render.
            assert_eq!(ra.mb_samples, 1);
        }
    }
    // The ball is blurred along its path: soft well past its edge.
    let (px, _) = fast.render_rgba8(&d, 0.125).unwrap();
    let (bg, _) = fast.render_rgba8(&d, 1.5).unwrap();
    let red = |p: &[u8], x: usize| p[(45 * 160 + x) * 4] as i32;
    assert!(
        red(&px, 80) > red(&px, 93) && red(&px, 93) > red(&bg, 60) + 10,
        "{} {}",
        red(&px, 80),
        red(&px, 93)
    );
}

/// Up to the split, groups that read nothing moving render once a frame
/// (before the samples), and what they make outlasts the samples even where
/// a sampled group writes their slot in between: the same image as sampling
/// everything. Here the still backdrop takes over the moving box's slot
/// (freed once the box's blur chain has read it), and the sampled
/// composite reads the backdrop.
#[test]
fn still_groups_render_once_and_match_full_sampling() {
    // The still layers fade later on: not static, so they share slots.
    let fade = json!({"keys": [{"t": 0, "v": [1]}, {"t": 1, "v": [1]}, {"t": 2, "v": [0.5]}]});
    let path = |a: [i32; 2], b: [i32; 2]| json!({"keys": [{"t": 0, "v": a}, {"t": 0.5, "v": b}, {"t": 2, "v": b}]});
    let mut nodes = json!({
        "bg": {"op": "gradient", "params": {"kind": "linear", "a": "#203040", "b": "#402030", "start": [0, 0], "end": [160, 90]}},
        "p1": {"op": "shape", "params": {"kind": "rect", "center": [40, 30], "size": [50, 30], "color": "#3070c0", "opacity": fade}},
        "s1": {"op": "over", "inputs": {"fg": "p1", "bg": "bg"}},
        "p2": {"op": "shape", "params": {"kind": "ellipse", "center": [120, 60], "size": [40, 40], "color": "#c09030", "opacity": fade}},
        "s2": {"op": "blend", "params": {"mode": "screen"}, "inputs": {"fg": "p2", "bg": "s1"}},
        "edge": {"op": "blur", "params": {"radius": 4}, "inputs": {"source": "s2"}},
        "ball": {"op": "shape", "params": {"kind": "ellipse", "size": [16, 16], "color": "#e04030", "center": path([10, 45], [150, 45])}},
        "c1": {"op": "over", "inputs": {"fg": "ball", "bg": "s2"}},
        "box": {"op": "shape", "params": {"kind": "rect", "size": [12, 12], "color": "#40e080", "center": path([80, 5], [80, 85])}},
        "c3": {"op": "over", "inputs": {"fg": "h11", "bg": "c1"}},
        "c4": {"op": "blend", "params": {"mode": "add", "opacity": 0.5}, "inputs": {"fg": "edge", "bg": "c3"}}
    });
    for i in 1..12 {
        let source = if i == 1 {
            "box".to_string()
        } else {
            format!("h{}", i - 1)
        };
        nodes[format!("h{i}")] = json!({"op": "blur", "params": {"radius": 1 + i % 3}, "inputs": {"source": source}});
    }
    let d = Document::from_json(
        &json!({"version": 1, "output": "c4", "nodes": nodes,
            "format": {"width": 160, "height": 90, "fps": 30, "duration": 2, "motion_blur": {"samples": 4, "shutter": 0.5}}})
        .to_string(),
    )
    .unwrap();
    let mut fast = Renderer::new(Device::new().unwrap()).unwrap();
    let mut full = Renderer::new(Device::new().unwrap()).unwrap();
    full.set_full_motion_blur(true);
    for t in [0.1, 0.25, 0.4] {
        let (a, ra) = fast.render_rgba8(&d, t).unwrap();
        let (b, _) = full.render_rgba8(&d, t).unwrap();
        let worst = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "t={t}: differs by {worst}");
        assert_eq!(ra.mb_samples, 4);
        assert!(ra.mb_once > 0, "t={t}: nothing ran once");
    }
}

/// Each moving layer is blurred on its own: what lies between two moving
/// layers (a haze over the first, under the second) renders once a frame,
/// on the first layer's average, and the second is sampled over it.
#[test]
fn layers_between_moving_layers_render_once() {
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":160,"height":90,"fps":30,"duration":2,"motion_blur":{"samples":4,"shutter":0.5}},
        "output":"top","nodes":{
          "bg":{"op":"solid","params":{"color":"#202028"}},
          "ball":{"op":"shape","params":{"kind":"ellipse","size":[16,16],"color":"#e04030",
             "center":{"keys":[{"t":0,"v":[10,30]},{"t":0.5,"v":[150,30]},{"t":2,"v":[150,30]}]}}},
          "a":{"op":"over","inputs":{"fg":"ball","bg":"bg"}},
          "haze":{"op":"blur","params":{"radius":6},"inputs":{"source":"a"}},
          "bar":{"op":"shape","params":{"kind":"rect","size":[30,6],"color":"#40e080",
             "center":{"keys":[{"t":0,"v":[150,75]},{"t":0.5,"v":[10,75]},{"t":2,"v":[10,75]}]}}},
          "top":{"op":"over","inputs":{"fg":"bar","bg":"haze"}}}}"##,
    )
    .unwrap();
    let mut fast = Renderer::new(Device::new().unwrap()).unwrap();
    let mut full = Renderer::new(Device::new().unwrap()).unwrap();
    full.set_full_motion_blur(true);
    for t in [0.1, 0.25, 0.4] {
        let (a, ra) = fast.render_rgba8(&d, t).unwrap();
        let (b, _) = full.render_rgba8(&d, t).unwrap();
        let worst = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "t={t}: differs by {worst}");
        assert_eq!(ra.mb_samples, 4);
        assert!(ra.mb_once >= 1, "t={t}: the haze was sampled");
    }
}

/// With `max_samples`, fast motion takes more samples: a bar whipping
/// 100 px across the shutter blurs into an even streak instead of eight
/// separate copies, slow motion keeps the base count, and a small layer
/// growing fast is measured on its own bounds, not the whole frame's.
#[test]
fn fast_motion_takes_more_samples() {
    let doc = |adaptive: bool| {
        let mb = if adaptive {
            json!({"samples": 8, "shutter": 0.5, "max_samples": 64})
        } else {
            json!({"samples": 8, "shutter": 0.5})
        };
        let bar = |h: i32| json!({"op": "path", "params": {"d": format!("M -3 -{h} L 3 -{h} L 3 {h} L -3 {h} Z"), "color": "#ffffff"}});
        Document::from_json(
            &json!({"version": 1, "output": "top",
                "format": {"width": 320, "height": 90, "fps": 30, "duration": 3, "motion_blur": mb},
                "nodes": {
                    "bg": {"op": "solid", "params": {"color": "#000000"}},
                    "bar": bar(20),
                    // 6000 px/s until 0.6 s (100 px over the shutter), then held off the frame.
                    "whip": {"op": "transform", "params": {"anchor": [0, 0], "position": {"keys": [
                        {"t": 0, "v": [-1640, 40], "ease": "linear"}, {"t": 0.6, "v": [1960, 40]}]}},
                        "inputs": {"source": "bar"}},
                    "slow_bar": bar(6),
                    // 600 px/s throughout: 10 px over the shutter.
                    "slow": {"op": "transform", "params": {"anchor": [0, 0], "position": {"keys": [
                        {"t": 0, "v": [-1340, 76], "ease": "linear"}, {"t": 3, "v": [460, 76]}]}},
                        "inputs": {"source": "slow_bar"}},
                    "dot": {"op": "path", "params": {"d": "M -5 -5 L 5 -5 L 5 5 L -5 5 Z", "color": "#ff0000"}},
                    // Grows twentyfold in a tenth of a second about its centre.
                    "grow": {"op": "transform", "params": {"anchor": [0, 0], "position": [280, 12], "scale": {"keys": [
                        {"t": 0, "v": [0.1]}, {"t": 1.5, "v": [0.1], "ease": "linear"}, {"t": 1.6, "v": [2.0]}]}},
                        "inputs": {"source": "dot"}},
                    // A plate (not vector) growing the same way: measured on what it moves.
                    "fill": {"op": "solid", "params": {"color": "#00ff00"}},
                    "plate": {"op": "crop", "params": {"rect": [-5, -5, 10, 10]}, "inputs": {"source": "fill"}},
                    "grow_plate": {"op": "transform", "params": {"anchor": [0, 0], "position": [40, 12], "scale": {"keys": [
                        {"t": 0, "v": [0.1]}, {"t": 1.5, "v": [0.1], "ease": "linear"}, {"t": 1.6, "v": [2.0]}]}},
                        "inputs": {"source": "plate"}},
                    "a": {"op": "over", "inputs": {"fg": "whip", "bg": "bg"}},
                    "b": {"op": "over", "inputs": {"fg": "slow", "bg": "a"}},
                    "c": {"op": "over", "inputs": {"fg": "grow_plate", "bg": "b"}},
                    "top": {"op": "over", "inputs": {"fg": "grow", "bg": "c"}}}})
            .to_string(),
        )
        .unwrap()
    };
    let streak = |px: &[u8]| (115..=205).map(|x| px[(40 * 320 + x) * 4] as i32).collect::<Vec<_>>();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let d = doc(true);
    // Once to learn where things are drawn, then the measured frame.
    r.render_rgba8(&d, 0.3).unwrap();
    let (px, rep) = r.render_rgba8(&d, 0.3).unwrap();
    assert_eq!(rep.mb_samples, 50, "100 px over the shutter at 2 px a sample");
    let s = streak(&px);
    let (lo, hi) = (*s.iter().min().unwrap(), *s.iter().max().unwrap());
    assert!(lo >= 30 && hi <= lo * 2, "an even streak: {lo}..{hi} {s:?}");
    // The same frame at a fixed eight samples: copies with gaps between.
    let mut f = Renderer::new(Device::new().unwrap()).unwrap();
    let (px8, rep8) = f.render_rgba8(&doc(false), 0.3).unwrap();
    assert_eq!(rep8.mb_samples, 8);
    assert!(*streak(&px8).iter().min().unwrap() < 10, "{:?}", streak(&px8));
    // Slow motion keeps the base count.
    r.render_rgba8(&d, 2.5).unwrap();
    assert_eq!(r.render_rgba8(&d, 2.5).unwrap().1.mb_samples, 8);
    // The dot and the plate grow twentyfold in a tenth of a second:
    // measured on their own 10 px bounds that is a few px over the shutter,
    // though the frame's corners, far from their centres, would move tens.
    r.render_rgba8(&d, 1.55).unwrap();
    assert_eq!(r.render_rgba8(&d, 1.55).unwrap().1.mb_samples, 8);
}
