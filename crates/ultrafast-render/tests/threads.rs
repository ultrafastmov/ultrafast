//! One device shared by renderers on many threads: every frame they draw
//! matches the same frame drawn alone (frames recorded at once each get
//! their own slice of the device's per-frame memory).

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

#[test]
fn renderers_on_many_threads_share_one_device() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/films");
    let doc = Document::from_json(&std::fs::read_to_string(format!("{dir}/ledger.json")).unwrap()).unwrap();
    let device = Device::new().unwrap();
    let times: Vec<f64> = (0..8).map(|k| 1.0 + k as f64 * 2.7).collect();
    let alone: Vec<Vec<u8>> = {
        let mut r = Renderer::new(device.clone()).unwrap();
        r.set_base_dir(std::path::Path::new(dir));
        times.iter().map(|&t| r.render_rgba8(&doc, t).unwrap().0).collect()
    };
    std::thread::scope(|s| {
        for k in 0..6 {
            let (device, doc, times, alone) = (device.clone(), &doc, &times, &alone);
            s.spawn(move || {
                let mut r = Renderer::new(device).unwrap();
                r.set_base_dir(std::path::Path::new(dir));
                for round in 0..3 {
                    for (i, &t) in times.iter().enumerate().skip(k % times.len()) {
                        let (px, _) = r.render_rgba8(doc, t).unwrap();
                        let worst = px.iter().zip(&alone[i]).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                        assert!(worst <= 2, "thread {k} round {round} t={t}: differs by {worst}");
                    }
                }
            });
        }
    });
}
