//! Preview audio follows the document and drives the playback clock. Skips
//! when the machine has no audio output. The track plays at zero gain.
#![cfg(all(feature = "audio", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]

use std::time::{Duration, Instant};
use ultrafast_graph::Document;
use ultrafast_live::Session;
use ultrafast_live::preview_audio::PreviewAudio;

#[test]
fn audio_clock_tracks_wall_time() {
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/assets");
    let doc = Document::from_json(
        r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":3},"output":"s",
        "nodes":{"s":{"op":"solid","params":{"color":"#000000"}},
                 "a":{"op":"audio","params":{"src":"tone.m4a","gain":0}}}}"##,
    )
    .unwrap();
    let mut session = Session::new(doc, Some(assets.join("preview.json"))).unwrap();
    let mut audio = match PreviewAudio::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    session.seek(0.5);
    // Wait for the background mix.
    let start = Instant::now();
    while !audio.active() {
        audio.sync(&mut session);
        assert!(start.elapsed() < Duration::from_secs(5), "mix never became active");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (t0, w0) = (audio.sync(&mut session), Instant::now());
    assert!((t0 - 0.5).abs() < 0.1, "started at {t0}");
    let mut t1 = t0;
    while w0.elapsed() < Duration::from_millis(800) {
        std::thread::sleep(Duration::from_millis(5));
        t1 = audio.sync(&mut session);
    }
    let (dt, dw) = (t1 - t0, w0.elapsed().as_secs_f64());
    assert!((dt - dw).abs() < 0.06, "audio clock moved {dt:.3}s in {dw:.3}s");

    // A seek realigns the device.
    session.seek(2.0);
    let t = audio.sync(&mut session);
    assert!((t - 2.0).abs() < 0.06, "after seek: {t}");
}
