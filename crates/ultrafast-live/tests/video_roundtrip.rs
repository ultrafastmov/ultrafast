//! Export a composition with the hardware encoder, decode it back through a
//! `video` node, and check frames land at the right times with the right
//! colors. The zero-readback path is the macOS Metal one; other backends
//! export through an ffmpeg pipe.
#![cfg(all(target_os = "macos", not(feature = "vulkan")))]

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_live::export::{ExportOptions, export_video};
use ultrafast_render::Renderer;

#[test]
fn export_then_decode_is_frame_accurate() {
    let dir = std::env::temp_dir().join(format!("ultrafast-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("steps.mp4");
    // Color steps every 10 frames: red, green, blue.
    let src = Document::from_json(
        r##"{"version":1,"format":{"width":320,"height":180,"fps":30,"duration":1},"output":"c",
        "nodes":{"c":{"op":"solid","params":{"color":{"keys":[
          {"t":0,"v":[1,0,0],"ease":"hold"},{"t":0.3333333,"v":[0,1,0],"ease":"hold"},{"t":0.6666666,"v":[0,0,1]}]}}}}}"##,
    )
    .unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let rep = export_video(&mut r, &src, &clip, ExportOptions::default(), |_, _| {}).unwrap();
    assert_eq!(rep.frames, 30);
    assert_eq!(rep.readbacks, 0, "export must not read frames back to the CPU");

    let reader = Document::from_json(
        r##"{"version":1,"format":{"width":320,"height":180,"fps":30,"duration":1},"output":"v",
        "nodes":{"v":{"op":"video","params":{"src":"steps.mp4","fit":"fill"}}}}"##,
    )
    .unwrap();
    r.set_base_dir(&dir);
    for (frame, expect) in [(2, [255, 0, 0]), (12, [0, 255, 0]), (25, [0, 0, 255])] {
        let (px, _) = r.render_rgba8(&reader, frame as f64 / 30.0).unwrap();
        let c = &px[(90 * 320 + 160) * 4..(90 * 320 + 160) * 4 + 3];
        for k in 0..3 {
            assert!(
                (c[k] as i32 - expect[k]).abs() <= 12,
                "frame {frame}: got {c:?}, want {expect:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
