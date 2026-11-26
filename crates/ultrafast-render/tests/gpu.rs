//! End-to-end checks on the real GPU.

use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Edit, Param};
use ultrafast_render::Renderer;

fn doc() -> Document {
    Document::from_json(
        r##"{"version":1,"format":{"width":16,"height":16,"fps":30,"duration":1},
        "output":"comp","nodes":{
          "bg":{"op":"solid","params":{"color":"#000000"}},
          "fg":{"op":"solid","params":{"color":[1,1,1,0.5]}},
          "comp":{"op":"over","inputs":{"fg":"fg","bg":"bg"}}}}"##,
    )
    .unwrap()
}

#[test]
fn half_white_over_black_is_linear_correct() {
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&doc(), 0.0).unwrap();
    // 50% linear white over black encodes to sRGB ~0.735 -> 188.
    assert!((px[0] as i32 - 188).abs() <= 1, "got {}", px[0]);
    assert_eq!(px[3], 255);
}

#[test]
fn param_edit_reuses_plan_and_reruns_only_changed_static_pass() {
    let mut d = doc();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (_, first) = r.render_rgba8(&d, 0.0).unwrap();
    assert!(first.replanned);
    // bg and fg are inlined into comp: one kernel.
    assert_eq!(first.kernels_run, 1);
    let (_, idle) = r.render_rgba8(&d, 0.0).unwrap();
    assert_eq!((idle.kernels_run, idle.groups_cached), (0, 1));

    d.apply(&[Edit::SetParam {
        node: "fg".into(),
        param: "color".into(),
        value: Param::Text("#ff0000".into()),
    }])
    .unwrap();
    let (px, edited) = r.render_rgba8(&d, 0.0).unwrap();
    assert!(!edited.replanned);
    assert_eq!(edited.kernels_run, 1);
    assert_eq!(&px[..4], &[255, 0, 0, 255]);
}

/// A shader whose big constant tables (a ramp and a vector table) pick
/// each column's color; the renderer reads them from the params.
fn table_shader(ramp: impl Fn(usize) -> usize) -> String {
    let ramp: Vec<String> = (0..256).map(|i| format!("{:.6}", ramp(i) as f64 / 255.0)).collect();
    let tints: Vec<String> = (0..64)
        .map(|i| format!("float2({:.6}, {:.6})", (63 - i) as f64 / 255.0, i as f64 / 255.0))
        .collect();
    format!(
        "// Columns read the tables.\nstatic const float RAMP[256] = {{\n{}\n}};\nstatic const float2 TINT[] = {{ {} }};\n\
         float4 shade(float2 pos, float px) {{\n  int x = int(pos.x);\n  return from_srgb(float4(RAMP[x * 4], TINT[x].x, TINT[x].y, 1.0));\n}}\n",
        ramp.join(",\n"),
        tints.join(", ")
    )
}

#[test]
fn shader_tables_are_read_from_params_and_edit_without_compiling() {
    let doc = |code: &str| {
        Document::from_json(
            &serde_json::json!({"version":1,"format":{"width":64,"height":2,"fps":30,"duration":1},
              "output":"s","nodes":{"s":{"op":"shader","params":{"code": code}}}})
            .to_string(),
        )
        .unwrap()
    };
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&doc(&table_shader(|i| i)), 0.0).unwrap();
    for x in [0usize, 1, 17, 63] {
        assert_eq!(
            &px[x * 4..x * 4 + 4],
            &[(x * 4) as u8, (63 - x) as u8, x as u8, 255],
            "column {x}"
        );
    }
    let key = |r: &Renderer| r.plan().unwrap().groups.last().unwrap().key.clone();
    let before = key(&r);
    // New values in the table: the same kernel, new colors.
    let (px, _) = r.render_rgba8(&doc(&table_shader(|i| 255 - i)), 0.0).unwrap();
    assert_eq!(key(&r), before);
    assert_eq!(px[17 * 4], 255 - 68);
}

#[test]
fn broken_agent_shader_reports_diagnostic_and_keeps_rendering() {
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":1},
        "output":"comp","nodes":{
          "bg":{"op":"solid","params":{"color":"#00ff00"}},
          "bad":{"op":"shader","params":{"code":"float4 shade(float2 pos, float px) { return oops; }"}},
          "b":{"op":"blur","inputs":{"source":"bad"}},
          "comp":{"op":"over","inputs":{"fg":"b","bg":"bg"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    assert_eq!(&px[..4], &[0, 255, 0, 255]);
    let diag = r.diagnostics();
    assert_eq!(diag.len(), 1, "{diag:?}");
    assert_eq!(diag[0].node, "bad");
    assert!(diag[0].message.contains("oops"), "{}", diag[0].message);
}

#[test]
fn fused_transform_moves_shape() {
    let src = r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":1},
        "output":"out","nodes":{
          "bg":{"op":"solid","params":{"color":"#202040"}},
          "s":{"op":"shape","params":{"kind":"rect","size":[30,20],"center":[32,32],"color":"#ff8000"}},
          "x":{"op":"transform","inputs":{"source":"s"},"params":{"position":[40,30],"anchor":[32,32]}},
          "out":{"op":"over","inputs":{"fg":"x","bg":"bg"}}}}"##;
    let d = Document::from_json(src).unwrap();
    let mut fused = Renderer::new(Device::new().unwrap()).unwrap();
    let (a, ra) = fused.render_rgba8(&d, 0.0).unwrap();
    assert_eq!(ra.groups_fallback, 0);
    // Pixel at the translated rect center is orange, the old center is not
    // covered at its left edge.
    let at = |px: &[u8], x: usize, y: usize| px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].to_vec();
    assert_eq!(at(&a, 40, 30), vec![255, 128, 0, 255]);
    assert_eq!(at(&a, 20, 30), vec![32, 32, 64, 255]);
}

#[test]
fn present_writes_letterboxed_bgra() {
    use ultrafast_gpu::{Bindings, PixelFormat, TextureDesc};
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":16,"height":8,"fps":30,"duration":1},
        "output":"s","nodes":{"s":{"op":"solid","params":{"color":"#ff0000"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let target = r.device().create_texture(TextureDesc {
        width: 16,
        height: 16,
        format: PixelFormat::Bgra8Unorm,
        cpu_readable: true,
    });
    let (sub, _) = r
        .submit_frame_with(&d, 0.0, |o| {
            let mut p = o.header.to_vec();
            p.extend(o.origin);
            // Frame is 2:1, drawable square: letterboxed to rows 4..12.
            p.extend([0.0, 4.0, 16.0, 8.0, 16.0, 16.0, 0.0, 0.0, 1.0]);
            o.list.dispatch_ex(
                &o.kernels.utility("k_present"),
                &target,
                &[o.texture],
                &p,
                Bindings {
                    grid: Some((16, 16)),
                    ..Default::default()
                },
            );
        })
        .unwrap();
    sub.wait().unwrap();
    let px = r.device().readback(&target);
    let at = |x: usize, y: usize| px[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4].to_vec();
    // BGRA memory order: red frame inside, blue backdrop outside.
    assert_eq!(at(8, 8), vec![0, 0, 255, 255]);
    assert_eq!(at(8, 1), vec![255, 0, 0, 255]);
}

#[test]
fn timing_windows_give_layer_semantics() {
    // The red layer is only visible in [1, 2); outside it the composite
    // passes the green background through (it does not go transparent).
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":3},"output":"c",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#00ff00"}},
          "fg":{"op":"solid","params":{"color":"#ff0000"}},
          "c":{"op":"over","inputs":{"fg":"fg","bg":"bg"},"timing":{"in":1,"out":2}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    for (t, want) in [
        (0.5, [0, 255, 0, 255]),
        (1.5, [255, 0, 0, 255]),
        (2.5, [0, 255, 0, 255]),
    ] {
        let (px, _) = r.render_rgba8(&d, t).unwrap();
        assert_eq!(&px[..4], &want, "t={t}");
    }
}

#[test]
fn render3d_draws_lit_mesh_over_transparent_background() {
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":1},"output":"s",
        "nodes":{
          "box":{"op":"mesh","params":{"primitive":"cube","color":"#ff0000","rotation":[20,30,0]}},
          "cam":{"op":"camera","params":{"position":[0,0,3]}},
          "s":{"op":"render3d","inputs":{"camera":"cam","box":"box"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    let at = |x: usize, y: usize| px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].to_vec();
    let center = at(32, 32);
    assert_eq!(center[3], 255, "mesh covers the center");
    assert!(
        center[0] > 100 && center[1] < 60 && center[2] < 60,
        "lit red: {center:?}"
    );
    assert_eq!(at(1, 1)[3], 0, "background stays transparent");
}

#[test]
fn render3d_outside_its_window_draws_nothing() {
    // A scene timed to start at 2 s, background included, must not cover
    // the layers under it before then.
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":16,"height":16,"fps":30,"duration":3},"output":"o",
        "nodes":{
          "ground":{"op":"solid","params":{"color":"#00ff00"}},
          "box":{"op":"mesh","params":{"primitive":"cube"}},
          "cam":{"op":"camera","params":{"position":[0,0,3]}},
          "s":{"op":"render3d","params":{"background":"#000000"},"inputs":{"camera":"cam","box":"box"},"timing":{"in":2}},
          "o":{"op":"over","inputs":{"fg":"s","bg":"ground"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 1.0).unwrap();
    assert_eq!(&px[..4], &[0, 255, 0, 255], "ground shows before the scene starts");
    let (px, _) = r.render_rgba8(&d, 2.5).unwrap();
    assert_eq!(&px[..4], &[0, 0, 0, 255], "scene background once it starts");
}

#[test]
fn film_look_and_engrave_finish_a_render() {
    // Both built-in looks run over an image; the engraving turns a smooth
    // ramp into ink and paper (dark lines on light paper, not a ramp).
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":128,"height":64,"fps":30,"duration":1},"output":"e",
        "nodes":{
          "ramp":{"op":"gradient","params":{"start":[0,0],"end":[128,0],"a":"#202020","b":"#e0e0e0"}},
          "f":{"op":"film_look","inputs":{"source":"ramp"}},
          "e":{"op":"engrave","params":{"backdrop":0},"inputs":{"source":"f"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    let row: Vec<u8> = (0..128).map(|x| px[(32 * 128 + x) * 4]).collect();
    let (lo, hi) = (*row.iter().min().unwrap(), *row.iter().max().unwrap());
    assert!(hi > 200 && lo < 90, "ink and paper both present: {lo}..{hi}");
}

#[test]
fn raymarch_renders_a_scene_written_as_a_distance_function() {
    // A steel ball over a transparent background: lit and opaque in the
    // middle, see-through at the corner (so it composites over 2D).
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":1},"output":"rm",
        "nodes":{"rm":{"op":"raymarch","params":{
          "scene":"float2 map(float3 p) { return float2(sdSphere(p, 1.0), 3.0); }",
          "orbit":[0,0,4],"background":"transparent","samples":1}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    let at = |x: usize, y: usize| px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].to_vec();
    assert_eq!(at(32, 32)[3], 255, "the ball covers the center");
    assert!(at(32, 32)[..3].iter().any(|&c| c > 20), "and is lit: {:?}", at(32, 32));
    assert_eq!(at(1, 1)[3], 0, "the background is transparent");
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
}

#[test]
fn raymarch_glass_shows_what_is_behind_it() {
    // A clear glass ball in front of a white wall: seen through, it is
    // light (the wall), not a dark solid.
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":1},"output":"rm",
        "nodes":{"rm":{"op":"raymarch","params":{
          "scene":"float2 map(float3 p) { return opU(float2(p.z + 3.0, 7.0), float2(sdSphere(p, 1.0), 9.0)); }",
          "orbit":[0,0,5],"light":[0,0,6],"light_intensity":60,"samples":1}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    let c = &px[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 3];
    assert!(c.iter().all(|&v| v > 90), "the wall shows through the ball: {c:?}");
}

#[test]
fn raymarch_extrudes_a_2d_layer_into_a_solid() {
    // A white disc drawn in ultrafast, softened, becomes a 3D puck seen head on:
    // solid where the disc was, see-through outside it.
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":96,"height":64,"fps":30,"duration":1},"output":"rm",
        "nodes":{
          "disc":{"op":"shape","params":{"kind":"circle","center":[48,32],"size":[40,40],"color":"#ffffff"}},
          "soft":{"op":"blur","params":{"radius":3},"inputs":{"source":"disc"}},
          "rm":{"op":"raymarch","inputs":{"in0":"soft"},"params":{
            "scene":"float2 map(float3 p) { return float2(extrude(max(layerDistance(0, p.xy, 20.0, 3.0), sdBox2(p.xy, float2(1.5, 1.5), 0.0)), p.z, 0.5, 0.05) * 0.7, 3.0); }",
            "orbit":[0,0,6],"fov":40,"background":"transparent","samples":1}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
    let a = |x: usize, y: usize| px[(y * 96 + x) * 4 + 3];
    assert_eq!(a(48, 32), 255, "the puck covers the center");
    assert_eq!(a(4, 4), 0, "outside it the background shows");
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
}

#[test]
fn a_timed_raymarch_shows_nothing_outside_its_window() {
    // The scene reads a red texture on in0; outside its window it must not
    // show that texture (a plain shader would pass its input through).
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":32,"height":32,"fps":30,"duration":3},"output":"o",
        "nodes":{
          "tex":{"op":"solid","params":{"color":"#ff0000"}},
          "bg":{"op":"solid","params":{"color":"#0000ff"}},
          "rm":{"op":"raymarch","inputs":{"in0":"tex"},"timing":{"in":1,"out":2},"params":{
            "scene":"float2 map(float3 p) { return float2(sdSphere(p, 0.5), 3.0); }",
            "orbit":[0,0,4],"background":"transparent","samples":1}},
          "o":{"op":"over","inputs":{"fg":"rm","bg":"bg"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.5).unwrap();
    assert_eq!(
        &px[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 4],
        &[0, 0, 255, 255],
        "before: the blue backdrop"
    );
    let (px, _) = r.render_rgba8(&d, 1.5).unwrap();
    assert_ne!(
        &px[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 3],
        &[0, 0, 255],
        "during: the ball"
    );
}

#[test]
fn a_timed_effect_component_passes_its_source_outside_its_window() {
    // A drop shadow timed to 1..2 s over a red square on blue: before and
    // after, exactly the square on blue (not the component's insides).
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":32,"height":32,"fps":30,"duration":3},"output":"o",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#0000ff"}},
          "sq":{"op":"shape","params":{"kind":"rect","center":[16,16],"size":[12,12],"color":"#ff0000"}},
          "sh":{"op":"drop_shadow","inputs":{"source":"sq"},"timing":{"in":1,"out":2}},
          "o":{"op":"over","inputs":{"fg":"sh","bg":"bg"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let px_at = |px: &[u8], x: usize, y: usize| px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].to_vec();
    for t in [0.5, 2.5] {
        let (px, _) = r.render_rgba8(&d, t).unwrap();
        assert_eq!(px_at(&px, 16, 16), vec![255, 0, 0, 255], "t={t}: the square");
        assert_eq!(px_at(&px, 2, 2), vec![0, 0, 255, 255], "t={t}: the backdrop");
    }
}

#[test]
fn an_inactive_blur_keeps_its_source_while_later_groups_reuse_slots() {
    // A blur outside its window passes its source on. Groups planned after
    // it (here a drop shadow's own passes) reuse texture slots; the frame
    // must still show the source around the shadowed square.
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":3},"output":"o",
        "nodes":{
          "a_bg":{"op":"solid","params":{"color":"#0000ff"}},
          "a_dot":{"op":"shape","params":{"kind":"circle","size":[4,4],"color":"#00ff00",
            "center":{"keys":[{"t":0,"v":[4,4]},{"t":3,"v":[4,5]}]}}},
          "a_d2":{"op":"shape","params":{"kind":"circle","center":[60,4],"size":[2,2],"color":"#00ff00"}},
          "a_d3":{"op":"shape","params":{"kind":"circle","center":[60,8],"size":[2,2],"color":"#00ff00"}},
          "a_d4":{"op":"shape","params":{"kind":"circle","center":[60,12],"size":[2,2],"color":"#00ff00"}},
          "a_f1":{"op":"over","inputs":{"fg":"a_dot","bg":"a_bg"}},
          "a_f2":{"op":"over","inputs":{"fg":"a_d2","bg":"a_f1"}},
          "a_f3":{"op":"over","inputs":{"fg":"a_d3","bg":"a_f2"}},
          "a_film":{"op":"over","inputs":{"fg":"a_d4","bg":"a_f3"}},
          "b_seam":{"op":"blur","params":{"radius":6},"inputs":{"source":"a_film"},"timing":{"in":2,"out":2.5}},
          "c_seam":{"op":"blur","params":{"radius":6},"inputs":{"source":"b_seam"},"timing":{"in":2.5,"out":2.8}},
          "d_seam":{"op":"blur","params":{"radius":6},"inputs":{"source":"c_seam"},"timing":{"in":2.8,"out":2.9}},
          "z_sq":{"op":"shape","params":{"kind":"rect","size":[8,8],"color":"#ff0000",
            "center":{"keys":[{"t":0,"v":[40,40]},{"t":3,"v":[40,41]}]}}},
          "z_sh":{"op":"drop_shadow","params":{"radius":3,"offset":[2,2]},"inputs":{"source":"z_sq"},"timing":{"in":0,"out":1}},
          "o":{"op":"over","inputs":{"fg":"z_sh","bg":"d_seam"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.5).unwrap();
    let at = |x: usize, y: usize| px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].to_vec();
    assert_eq!(at(4, 60), vec![0, 0, 255, 255], "the backdrop survives");
    assert_eq!(at(4, 4)[1], 255, "the dot survives");
    assert_eq!(at(40, 40), vec![255, 0, 0, 255], "the square");
}

#[test]
fn a_camera_over_a_composite_keeps_type_sharp() {
    // Zooming a whole composite 4x draws its type at 4x, not upscaled: the
    // frame matches the same type set at four times the size.
    let zoomed = Document::from_json(
        r##"{"version":1,"format":{"width":160,"height":80,"fps":30,"duration":1},"output":"cam",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#101010"}},
          "t":{"op":"text","params":{"text":"Ag","size":12,"position":[10,10],"color":"#ffffff"}},
          "o":{"op":"over","inputs":{"fg":"t","bg":"bg"}},
          "cam":{"op":"transform","params":{"anchor":[0,0],"position":[0,0],"scale":4},"inputs":{"source":"o"}}}}"##,
    )
    .unwrap();
    let direct = Document::from_json(
        r##"{"version":1,"format":{"width":160,"height":80,"fps":30,"duration":1},"output":"o",
        "nodes":{
          "bg":{"op":"solid","params":{"color":"#101010"}},
          "t":{"op":"text","params":{"text":"Ag","size":48,"position":[40,40],"color":"#ffffff"}},
          "o":{"op":"over","inputs":{"fg":"t","bg":"bg"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (a, _) = r.render_rgba8(&zoomed, 0.0).unwrap();
    let (b, _) = r.render_rgba8(&direct, 0.0).unwrap();
    let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
    assert!(worst <= 24, "zoomed type differs from type set large by up to {worst}");
}

#[test]
fn captions_inside_a_component_render_like_top_level_ones() {
    let font = "../../examples/assets/fonts/Geist-VF.ttf";
    let caps = |words: &str| {
        format!(
            r##"{{"op":"captions","params":{{"words":"{words}","font":"{font}","size":40,"position":[160,90],"style":"pop"}}}}"##
        )
    };
    let top = format!(
        r##"{{"version":1,"format":{{"width":320,"height":180,"fps":30,"duration":2}},"output":"out","nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#101010"}}}},
          "caps":{},
          "out":{{"op":"over","inputs":{{"fg":"caps","bg":"bg"}}}}}}}}"##,
        caps("0 1 hello\\n1 2 world")
    );
    let nested = format!(
        r##"{{"version":1,"format":{{"width":320,"height":180,"fps":30,"duration":2}},"output":"out",
        "components":{{"subs":{{"output":"c","params":{{"words":""}},"nodes":{{"c":{}}}}}}},
        "nodes":{{
          "bg":{{"op":"solid","params":{{"color":"#101010"}}}},
          "caps":{{"op":"subs","params":{{"words":"0 1 hello\n1 2 world"}}}},
          "out":{{"op":"over","inputs":{{"fg":"caps","bg":"bg"}}}}}}}}"##,
        caps("$words")
    );
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let (a, _) = r.render_rgba8(&Document::from_json(&top).unwrap(), 1.5).unwrap();
    let (b, _) = r.render_rgba8(&Document::from_json(&nested).unwrap(), 1.5).unwrap();
    assert!(a.chunks(4).any(|p| p[0] > 200), "the top-level captions drew nothing");
    let diff = a
        .iter()
        .zip(&b)
        .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 2)
        .count();
    assert_eq!(diff, 0, "{diff} bytes differ");
}

/// A stack of `n` layers over a dark background: shapes, with a text layer
/// every third one and a window on every fourth; `split` puts an identity
/// opacity under every other layer, which keeps the chain from looping.
fn layer_stack(n: usize, split: bool) -> Document {
    let font = "../../examples/assets/fonts/Geist-VF.ttf";
    let mut nodes = vec![r##""bg":{"op":"solid","params":{"color":"#101418"}}"##.to_string()];
    let mut below = "bg".to_string();
    for i in 0..n {
        let x = 30 + (i % 6) * 48;
        let y = 30 + (i / 6) * 48;
        let layer = if i % 5 == 2 {
            // Gradients of two kinds (one op, two functions).
            let kind = if i % 2 == 0 { "radial" } else { "linear" };
            format!(
                r##""l{i}":{{"op":"gradient","params":{{"kind":"{kind}","stops":"0:#00000000, 1:#2040ff80","center":[{x},{y}],"radius":60,"start":[0,0],"end":[0,180]}}}}"##
            )
        } else if i % 3 == 1 {
            format!(
                r##""l{i}":{{"op":"text","params":{{"text":"{i}","font":"{font}","size":28,"position":[{x},{y}],"color":"#ffcc00"}}}}"##
            )
        } else {
            format!(
                r##""l{i}":{{"op":"shape","params":{{"kind":"rect","center":[{x},{y}],"size":[40,30],"radius":6,"color":[{},{},0.8,0.7]}}}}"##,
                (i as f64 * 0.13) % 1.0,
                1.0 - (i as f64 * 0.07) % 1.0
            )
        };
        nodes.push(layer);
        // Windowed layers loop; one in five stays on and unrolled.
        let timing = match i % 5 {
            4 => "",
            3 => r##","timing":{"in":0.5,"out":0.8}"##,
            _ => r##","timing":{"in":0.0,"out":0.9}"##,
        };
        nodes.push(format!(
            r##""c{i}":{{"op":"over","inputs":{{"fg":"l{i}","bg":"{below}"}}{timing}}}"##
        ));
        below = format!("c{i}");
        if split && i % 2 == 0 {
            nodes.push(format!(
                r##""s{i}":{{"op":"opacity","params":{{"opacity":1}},"inputs":{{"source":"{below}"}}}}"##
            ));
            below = format!("s{i}");
        }
    }
    Document::from_json(&format!(
        r##"{{"version":1,"format":{{"width":320,"height":180,"fps":30,"duration":1}},"output":"{below}","nodes":{{{}}}}}"##,
        nodes.join(",")
    ))
    .unwrap()
}

#[test]
fn looped_layer_stacks_render_like_unrolled_ones() {
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    r.set_wait_for_fused(true);
    for t in [0.2, 0.6] {
        let (a, _) = r.render_rgba8(&layer_stack(14, false), t).unwrap();
        let (b, _) = r.render_rgba8(&layer_stack(14, true), t).unwrap();
        assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
        let diff = a
            .iter()
            .zip(&b)
            .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 2)
            .count();
        assert_eq!(diff, 0, "t={t}: {diff} bytes differ");
    }
}

/// Text over nothing, then blurred by zero: transparent pixels carry no
/// color (coverage a hair above zero once painted full color into them,
/// seen as streaks by anything reading RGB alone).
#[test]
fn transparent_pixels_carry_no_color() {
    let d = Document::from_json(
        r##"{"version":1,"format":{"width":320,"height":120,"fps":30,"duration":1},
        "output":"blur","nodes":{
          "t":{"op":"text","params":{"text":"Why Are People","font":"ultrafast:serif","size":48,"color":"#0f766e","position":[12,30]}},
          "blur":{"op":"blur","inputs":{"source":"t"},"params":{"radius":0}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let (px, _) = r.render_rgba8(&d, 0.5).unwrap();
    let drawn = px.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(drawn > 500, "the text is drawn ({drawn} pixels)");
    let painted = px.chunks_exact(4).filter(|p| p[3] == 0 && p[..3] != [0, 0, 0]).count();
    assert_eq!(painted, 0, "{painted} transparent pixels carry color");
}

/// Two image sequences sharing one `src` pattern, each with its own window,
/// offset and first frame, show their own frames (each node is its own
/// playhead), including on frame boundaries after a nonzero offset.
#[test]
fn same_pattern_sequences_keep_their_own_playheads() {
    let dir = std::env::temp_dir().join(format!("ultrafast-seq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Frame n is a flat gray of value 10 * n.
    for n in 0..40u8 {
        image::RgbaImage::from_pixel(4, 4, image::Rgba([n * 6, n * 6, n * 6, 255]))
            .save(dir.join(format!("f{n:03}.png")))
            .unwrap();
    }
    let doc = Document::from_json(
        r##"{"version":1,"format":{"width":4,"height":4,"fps":30,"duration":2},
        "output":"b_over","nodes":{
          "bg":{"op":"solid","params":{"color":"#ff0000"}},
          "a":{"op":"image","params":{"src":"f{frame:03}.png","fit":"fill"},"timing":{"in":0,"out":0.5}},
          "a_over":{"op":"over","inputs":{"fg":"a","bg":"bg"},"timing":{"in":0,"out":0.5}},
          "b":{"op":"image","params":{"src":"f{frame:03}.png","fit":"fill","first_frame":20},"timing":{"in":0.5,"out":1.0,"offset":0.5}},
          "b_over":{"op":"over","inputs":{"fg":"b","bg":"a_over"},"timing":{"in":0.5,"out":1.0,"offset":0.5}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(&dir);
    // (frame, expected source frame); red where neither node is on screen.
    for (f, want) in [
        (3u32, Some(3u8)),
        (14, Some(14)),
        (15, Some(20)),
        (16, Some(21)),
        (25, Some(30)),
        (29, Some(34)),
        (30, None),
    ] {
        let t = f as f64 / 30.0;
        // Exact mode waits for each file; render twice in case of a cold cache.
        let _ = r.render_rgba8(&doc, t).unwrap();
        let (px, _) = r.render_rgba8(&doc, t).unwrap();
        match want {
            Some(n) => assert_eq!((px[0], px[1], px[3]), (n * 6, n * 6, 255), "frame {f}"),
            None => assert_eq!((px[0], px[1]), (255, 0), "frame {f}"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A camera flying over more plates of footage than one pass can bind
/// shows every plate where the camera puts it. The plates sit far outside
/// the frame in their own coordinates; without moving the camera onto each
/// plate, the pass the planner splits off drew them unmoved and the frame
/// came out empty.
#[test]
fn a_camera_over_many_footage_plates_shows_them_all() {
    let dir = std::env::temp_dir().join(format!("ultrafast-plates-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let n = 20;
    for k in 0..n {
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10 + 12 * k as u8, 200, 255 - 12 * k as u8, 255]))
            .save(dir.join(format!("p{k}.png")))
            .unwrap();
    }
    // Plate k: 8x8 px at world (1000 + 10 k, 2000), placed by its own transform;
    // the camera brings world (1000, 2000) to the frame's corner.
    let mut nodes = vec![r##""bg":{"op":"solid","params":{"color":"#000000"}}"##.to_string()];
    let mut top = "bg".to_string();
    for k in 0..n {
        nodes.push(format!(
            r##""img{k}":{{"op":"image","params":{{"src":"p{k}.png","rect":[-4,-4,8,8]}}}},
            "at{k}":{{"op":"transform","params":{{"anchor":[0,0],"position":[{},2004]}},"inputs":{{"source":"img{k}"}}}},
            "o{k}":{{"op":"over","inputs":{{"fg":"at{k}","bg":"{top}"}}}}"##,
            1004 + 10 * k
        ));
        top = format!("o{k}");
    }
    let json = format!(
        r##"{{"version":1,"format":{{"width":200,"height":20,"fps":30,"duration":1}},"output":"cam","nodes":{{{},
        "cam":{{"op":"transform","params":{{"anchor":[1000,2000],"position":[0,0]}},"inputs":{{"source":"{top}"}}}}}}}}"##,
        nodes.join(",")
    );
    let doc = Document::from_json(&json).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(&dir);
    let _ = r.render_rgba8(&doc, 0.0).unwrap();
    let (px, _) = r.render_rgba8(&doc, 0.0).unwrap();
    for k in 0..n {
        let (x, y) = (4 + 10 * k, 4);
        let i = (y * 200 + x) * 4;
        assert_eq!(px[i + 1], 200, "plate {k} is on screen at x {x}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A picture in a box keeps its aspect ratio: by default it covers the box,
/// cropped evenly (nothing outside the box), `contain` shows all of it
/// inside, and only `fill` stretches it to the box.
#[test]
fn pictures_in_a_box_keep_their_aspect_ratio() {
    let dir = std::env::temp_dir().join(format!("ultrafast-aspect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 60x20: red, green and blue thirds.
    let img = image::RgbaImage::from_fn(60, 20, |x, _| match x / 20 {
        0 => image::Rgba([250, 0, 0, 255]),
        1 => image::Rgba([0, 250, 0, 255]),
        _ => image::Rgba([0, 0, 250, 255]),
    });
    img.save(dir.join("bands.png")).unwrap();
    let render = |fit: &str| {
        let doc = Document::from_json(&format!(
            r##"{{"version":1,"format":{{"width":60,"height":40,"fps":30,"duration":1}},"output":"o","nodes":{{
              "bg":{{"op":"solid","params":{{"color":"#000000"}}}},
              "a":{{"op":"image","params":{{"src":"bands.png","rect":[20,10,20,20]{fit}}}}},
              "o":{{"op":"over","inputs":{{"fg":"a","bg":"bg"}}}}}}}}"##
        ))
        .unwrap();
        let mut r = Renderer::new(Device::new().unwrap()).unwrap();
        r.set_base_dir(&dir);
        let _ = r.render_rgba8(&doc, 0.0).unwrap();
        let (px, _) = r.render_rgba8(&doc, 0.0).unwrap();
        move |x: usize, y: usize| [px[(y * 60 + x) * 4], px[(y * 60 + x) * 4 + 1], px[(y * 60 + x) * 4 + 2]]
    };
    // Cover (the default in a box): the middle third fills the 20x20 box, and
    // the red and blue thirds it crops never show outside it.
    let at = render("");
    for (x, y) in [(21, 11), (30, 20), (38, 28)] {
        let c = at(x, y);
        assert!(c[1] > 200 && c[0] < 40 && c[2] < 40, "cover at ({x},{y}) is {c:?}");
    }
    for (x, y) in [(15, 20), (45, 20), (30, 5), (30, 35)] {
        assert_eq!(at(x, y), [0, 0, 0], "nothing outside the box at ({x},{y})");
    }
    // Contain: all three thirds, 20x6.7 px, centred in the box.
    let at = render(r#","fit":"contain""#);
    assert!(
        at(22, 20)[0] > 200 && at(30, 20)[1] > 200 && at(38, 20)[2] > 200,
        "{:?}",
        [at(22, 20), at(30, 20), at(38, 20)]
    );
    assert_eq!(at(30, 12), [0, 0, 0], "contain leaves the rest of the box empty");
    // Fill stretches, as asked: the red third reaches the box's left edge.
    let at = render(r#","fit":"fill""#);
    assert!(
        at(21, 20)[0] > 200 && at(30, 12)[1] > 200,
        "{:?}",
        [at(21, 20), at(30, 12)]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An image rewritten on disk under the same name (a sheet rebuilt, a frame
/// re-rendered) is read again once the renderer is asked to refresh changed
/// media; until then the decoded copy stands, and unchanged files stay put.
#[test]
fn media_rewritten_on_disk_is_read_again() {
    let dir = std::env::temp_dir().join(format!("ultrafast-refresh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let save = |name: &str, rgb: [u8; 3], side: u32| {
        image::RgbaImage::from_pixel(side, side, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
            .save(dir.join(name))
            .unwrap();
    };
    save("a.png", [250, 10, 10], 2);
    save("b.png", [10, 250, 10], 2);
    let doc = Document::from_json(
        r##"{"version":1,"format":{"width":20,"height":10,"fps":30,"duration":1},"output":"o","nodes":{
          "bg":{"op":"solid","params":{"color":"#000000"}},
          "a":{"op":"image","params":{"src":"a.png","rect":[0,0,10,10]}},
          "b":{"op":"image","params":{"src":"b.png","rect":[10,0,10,10]}},
          "ab":{"op":"over","inputs":{"fg":"a","bg":"bg"}},
          "o":{"op":"over","inputs":{"fg":"b","bg":"ab"}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    r.set_base_dir(&dir);
    let px = |r: &mut Renderer| {
        let (p, _) = r.render_rgba8(&doc, 0.0).unwrap();
        (
            [p[(5 * 20 + 5) * 4], p[(5 * 20 + 5) * 4 + 1], p[(5 * 20 + 5) * 4 + 2]],
            [p[(5 * 20 + 15) * 4], p[(5 * 20 + 15) * 4 + 1], p[(5 * 20 + 15) * 4 + 2]],
        )
    };
    let (a0, b0) = px(&mut r);
    assert!(a0[0] > 200 && b0[1] > 200, "{a0:?} {b0:?}");
    assert_eq!(r.refresh_changed_media(), 0, "nothing changed yet");
    // Rewrite a.png as blue (a different size too, so its length changes).
    save("a.png", [10, 10, 250], 4);
    let (a1, _) = px(&mut r);
    assert!(a1[0] > 200, "the decoded copy stands until a refresh: {a1:?}");
    assert_eq!(r.refresh_changed_media(), 1, "only a.png changed");
    let (a2, b2) = px(&mut r);
    assert!(a2[2] > 200 && a2[0] < 60, "a.png read again: {a2:?}");
    assert!(b2[1] > 200, "b.png kept: {b2:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A renderer that compiles kernels on demand draws what one with every
/// tier-0 kernel built up front draws, fused and op by op (a group's ops'
/// tier-0 kernels then compile when it first falls back to them).
#[test]
fn on_demand_kernels_render_the_same() {
    let src = r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":1},
        "output":"out","nodes":{
          "bg":{"op":"gradient","params":{"a":"#202040","b":"#40a0ff","start":[0,0],"end":[0,64]}},
          "s":{"op":"shape","params":{"kind":"circle","size":[30,30],"center":[32,32],"color":"#ff8000"}},
          "o":{"op":"opacity","inputs":{"source":"s"},"params":{"opacity":0.6}},
          "x":{"op":"transform","inputs":{"source":"o"},"params":{"position":[40,30],"anchor":[32,32]}},
          "out":{"op":"over","inputs":{"fg":"x","bg":"bg"}}}}"##;
    let d = Document::from_json(src).unwrap();
    let mut eager = Renderer::new(Device::new().unwrap()).unwrap();
    let (want, _) = eager.render_rgba8(&d, 0.0).unwrap();
    let mut lazy = Renderer::new_on_demand(Device::new().unwrap()).unwrap();
    let (fused, _) = lazy.render_rgba8(&d, 0.0).unwrap();
    assert_eq!(fused, want, "fused");
    // Op by op: a fresh renderer that doesn't wait for fused kernels.
    let mut lazy = Renderer::new_on_demand(Device::new().unwrap()).unwrap();
    lazy.set_wait_for_fused(false);
    let (by_op, _) = lazy.render_rgba8(&d, 0.0).unwrap();
    let off = by_op
        .iter()
        .zip(&want)
        .filter(|(a, b)| (**a as i32 - **b as i32).abs() > 2)
        .count();
    assert_eq!(off, 0, "op by op: {off} bytes differ");
}

/// An exact render doesn't depend on how far kernel compiles got: a blur
/// read by a fused group is kept at reduced resolution only once that
/// group's kernel is ready, and an exact render waits for it to be, so the
/// first frame from a cold renderer matches the same frame rendered again
/// once every kernel is built.
#[test]
fn blur_reader_compiling_renders_the_same() {
    // The reader is a shader whose source is new to this run, so no
    // pipeline cache has it and it is still compiling as the blur is drawn.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let code = format!(
        "// {nonce}\nfloat4 shade(float2 pos, float px) {{ return input(0, pos + float2(sin(pos.y * 0.07) * 9.0, 0.0)) * 0.6; }}"
    );
    let doc = serde_json::json!({"version":1,"format":{"width":256,"height":256,"fps":30,"duration":1},
        "output":"out","nodes":{
          "bg":{"op":"gradient","params":{"a":"#201810","b":"#604030","start":[0,0],"end":[0,256]}},
          "s":{"op":"shape","params":{"kind":"circle","size":[90,140],"color":"#fff1e0",
               "center":{"keys":[{"t":0,"v":[128,150]},{"t":1,"v":[128,90]}]}}},
          "soft":{"op":"blur","inputs":{"source":"s"},"params":{"radius":18}},
          "w":{"op":"shader","inputs":{"in0":"soft"},"params":{"code":code}},
          "out":{"op":"over","inputs":{"fg":"w","bg":"bg"}}}});
    let d = Document::from_json(&doc.to_string()).unwrap();
    let mut r = Renderer::new_on_demand(Device::new().unwrap()).unwrap();
    let (cold, _) = r.render_rgba8(&d, 0.0).unwrap();
    // Another time first, so the blur is drawn again rather than reused.
    r.render_rgba8(&d, 0.5).unwrap();
    let (warm, _) = r.render_rgba8(&d, 0.0).unwrap();
    let off = cold.iter().zip(&warm).filter(|(a, b)| a != b).count();
    assert_eq!(off, 0, "{off} bytes differ between the cold and the warm frame");
}
