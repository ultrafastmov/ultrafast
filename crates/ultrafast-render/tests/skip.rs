//! Skipping redundant work (still geometry, pass-through layers) changes
//! no pixel: films rendered in sequence match a renderer that draws
//! everything every time.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

fn compare(film: &str, every: usize) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/films");
    let doc = Document::from_json(&std::fs::read_to_string(format!("{dir}/{film}.json")).unwrap()).unwrap();
    let mut fast = Renderer::new(Device::new().unwrap()).unwrap();
    let mut slow = Renderer::new(Device::new().unwrap()).unwrap();
    slow.set_skip_redundant(false);
    for r in [&mut fast, &mut slow] {
        r.set_base_dir(std::path::Path::new(dir));
    }
    let n = doc.format.frame_count() as usize;
    for i in (0..n).step_by(every) {
        let t = doc.format.time_of(i as u64);
        let (a, _) = fast.render_rgba8(&doc, t).unwrap();
        let (b, _) = slow.render_rgba8(&doc, t).unwrap();
        let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        assert!(worst <= 4, "{film} frame {i}: differs by {worst}");
    }
}

#[test]
fn numbers_matches_a_full_render() {
    compare("numbers", 23);
}

#[test]
fn ledger_matches_a_full_render() {
    compare("ledger", 41);
}
