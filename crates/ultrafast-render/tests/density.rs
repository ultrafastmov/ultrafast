//! Rendering at another size draws the document at that density: a frame
//! at twice the size, averaged back down 2x2, is the frame at the
//! document's own size (text, paths, blurs, particles, simulations and 3D
//! in their places, not shifted, cut to a quarter or scaled up), and going
//! back to the document's size renders as before.

use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_render::Renderer;

const W: usize = 192;
const H: usize = 108;

fn doc() -> Document {
    Document::from_json(&format!(
        r##"{{"version":1,"format":{{"width":{W},"height":{H},"fps":30,"duration":2}},"output":"out",
        "nodes":{{
          "bg":{{"op":"gradient","params":{{"a":"#203050","b":"#d08040","start":[0,0],"end":[{W},{H}]}}}},
          "title":{{"op":"text","params":{{"text":"Sharp","size":28,"color":"#ffffff","position":[8,6]}}}},
          "disc":{{"op":"shape","params":{{"kind":"circle","color":"#ff3366","center":[150,30],"size":[36,36]}}}},
          "soft":{{"op":"blur","params":{{"radius":5}},"inputs":{{"source":"disc"}}}},
          "dye":{{"op":"shape","params":{{"kind":"ellipse","color":"#40c0ff","center":{{"keys":[{{"t":0,"v":[30,80]}},{{"t":1,"v":[80,80]}}]}},"size":[14,14]}}}},
          "ink":{{"op":"fluid","params":{{"resolution":0.5,"dye_resolution":1}},"inputs":{{"dye":"dye","stir":"dye"}}}},
          "sparks":{{"op":"particles","params":{{"position":[120,90],"rate":40,"speed":[20,40],"size":[4,2],"gravity":[0,0]}}}},
          "box":{{"op":"mesh","params":{{"primitive":"cube","color":"#e0e0e0","rotation":[20,30,0],"scale":0.35,"position":[0.6,-0.1,0]}}}},
          "cam":{{"op":"camera","params":{{"position":[0,0,3]}}}},
          "scene":{{"op":"render3d","inputs":{{"camera":"cam","box":"box"}}}},
          "o1":{{"op":"over","inputs":{{"fg":"ink","bg":"bg"}}}},
          "o2":{{"op":"over","inputs":{{"fg":"soft","bg":"o1"}}}},
          "o3":{{"op":"over","inputs":{{"fg":"sparks","bg":"o2"}}}},
          "o4":{{"op":"over","inputs":{{"fg":"scene","bg":"o3"}}}},
          "out":{{"op":"over","inputs":{{"fg":"title","bg":"o4"}}}}}}}}"##
    ))
    .unwrap()
}

fn linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    255.0
        * if l <= 0.0031308 {
            l * 12.92
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        }
}

/// PSNR (8-bit sRGB) of `small` (W x H) against `big` (2W x 2H) averaged
/// 2x2 in linear light, and the worst 16x16 tile's.
fn psnr_2x(small: &[u8], big: &[u8]) -> (f64, f64) {
    let tiles_x = W / 16;
    let mut tile_err = vec![0.0; tiles_x * (H / 16)];
    let mut total = 0.0;
    for y in 0..H {
        for x in 0..W {
            for c in 0..3 {
                let mut l = 0.0;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    l += linear(big[((2 * y + dy) * 2 * W + 2 * x + dx) * 4 + c]) / 4.0;
                }
                let e = (srgb(l) - small[(y * W + x) * 4 + c] as f64).powi(2);
                total += e;
                if x / 16 < tiles_x && y / 16 < H / 16 {
                    tile_err[(y / 16) * tiles_x + x / 16] += e;
                }
            }
        }
    }
    let db = |mse: f64| 10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10();
    let worst = tile_err.iter().cloned().fold(0.0, f64::max) / (16.0 * 16.0 * 3.0);
    (db(total / (W * H * 3) as f64), db(worst))
}

#[test]
fn twice_the_size_is_the_same_frame_drawn_finer() {
    let d = doc();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    for t in [0.0, 0.5, 1.0] {
        r.set_output_size(None);
        assert_eq!(r.frame_size(&d), (W as u32, H as u32));
        let (small, _) = r.render_rgba8(&d, t).unwrap();
        r.set_output_size(Some((2 * W as u32, 2 * H as u32)));
        assert_eq!(r.frame_size(&d), (2 * W as u32, 2 * H as u32));
        let (big, _) = r.render_rgba8(&d, t).unwrap();
        assert_eq!(big.len(), small.len() * 4);
        let (all, worst) = psnr_2x(&small, &big);
        assert!(
            all > 35.0 && worst > 22.0,
            "t={t}: {all:.1} dB, worst tile {worst:.1} dB"
        );
    }
    // Back at its own size, the frame is what it was.
    r.set_output_size(None);
    let (again, _) = r.render_rgba8(&d, 1.0).unwrap();
    let mut fresh = Renderer::new(Device::new().unwrap()).unwrap();
    let (first, _) = fresh.render_rgba8(&d, 1.0).unwrap();
    let off = again.iter().zip(&first).filter(|(a, b)| a.abs_diff(**b) > 1).count();
    assert_eq!(off, 0, "back at its own size, {off} values changed");
}

#[test]
fn other_sizes_keep_the_frame_in_place() {
    // 1.5x and 2/3: every texel stays inside the frame, and a bright disc
    // lands where it does at the document's size.
    let d = doc();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    for (w, h) in [(288u32, 162u32), (128, 72)] {
        r.set_output_size(Some((w, h)));
        assert_eq!(r.frame_size(&d), (w, h));
        let (px, _) = r.render_rgba8(&d, 0.0).unwrap();
        assert_eq!(px.len(), (w * h * 4) as usize);
        let s = w as f64 / W as f64;
        let at = |x: f64, y: f64| {
            let (x, y) = ((x * s) as usize, (y * s) as usize);
            &px[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4]
        };
        // The disc's center (150, 30) is pink; the frame's bottom-right
        // corner is the gradient's warm end, not empty.
        let c = at(150.0, 30.0);
        assert!(c[0] > 200 && c[1] < 120, "{w}x{h}: disc center {c:?}");
        let e = at(W as f64 - 0.5, H as f64 - 0.5);
        assert!(e[3] == 255 && e[0] > 150, "{w}x{h}: corner {e:?}");
    }
}
