//! Every export format end to end: a small document with a transparent
//! background and a half-transparent square, exported as each format,
//! then probed and decoded with ffmpeg to check frame counts, frame rates,
//! sound and that transparency comes through (straight alpha where the
//! format keeps it, flattened over black where it doesn't). Skips what this
//! machine's ffmpeg can't encode or decode.

use std::path::{Path, PathBuf};
use std::process::Command;
use ultrafast_gpu::Device;
use ultrafast_graph::Document;
use ultrafast_live::export::{ExportOptions, export_video};
use ultrafast_live::output::Settings;
use ultrafast_render::Renderer;

const DOC: &str = r##"{
  "version": 1,
  "format": { "width": 320, "height": 180, "fps": 30, "duration": 0.5 },
  "output": "both",
  "nodes": {
    "disc": { "op": "shape", "params": { "kind": "circle", "color": "#ff7a2e", "size": [100, 100], "center": [100, 90] } },
    "half": { "op": "shape", "params": { "kind": "rect", "color": [0.2, 0.6, 1.0, 0.5], "size": [100, 100], "center": [220, 90] } },
    "both": { "op": "over", "inputs": { "fg": "half", "bg": "disc" } }
  }
}"##;

fn ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn encoder(name: &str) -> bool {
    ultrafast_live::ffmpeg::encoder_opens(name)
}

fn decoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!(" {name} ")))
}

/// Frames and frame rate of the first video stream.
fn probe(path: &Path) -> (u64, String) {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames,r_frame_rate", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    let mut it = s.trim().split(',');
    let rate = it.next().unwrap_or("").to_string();
    let frames = it.next().and_then(|f| f.parse().ok()).unwrap_or(0);
    (frames, rate)
}

/// The first video stream's colour tags as players read them (sorted
/// `key=value`s).
fn colour(path: &Path) -> String {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .args([
            "stream=color_range,color_space,color_transfer,color_primaries",
            "-of",
            "default=nw=1",
        ])
        .arg(path)
        .output()
        .unwrap();
    let mut tags: Vec<&str> = std::str::from_utf8(&o.stdout).unwrap().lines().collect();
    tags.sort();
    tags.join(" ")
}

/// What every YUV export is: BT.709 at limited range.
const BT709: &str = "color_primaries=bt709 color_range=tv color_space=bt709 color_transfer=bt709";

/// ProRes in a MOV: QuickTime's colour atom (nclc) has no range field and
/// ProRes is limited range by definition; ffmpeg 9 reads that as `tv`, 6.1
/// as `unknown`, from the same file.
fn prores_colour(path: &Path) -> String {
    colour(path).replace("color_range=unknown", "color_range=tv")
}

fn has_audio(path: &Path) -> bool {
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    !o.stdout.is_empty()
}

/// RGBA8 of the first frame at the transparent corner, the disc and the
/// half-transparent square.
fn pixels(path: &Path, decoder: &[&str]) -> [[u8; 4]; 3] {
    let o = Command::new("ffmpeg")
        .args(["-v", "error"])
        .args(decoder)
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .unwrap();
    let px = |x: usize, y: usize| {
        let i = (y * 320 + x) * 4;
        [o.stdout[i], o.stdout[i + 1], o.stdout[i + 2], o.stdout[i + 3]]
    };
    [px(5, 5), px(100, 90), px(220, 90)]
}

fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
}

fn export(r: &mut Renderer, doc: &Document, out: &Path, json: &str) -> ultrafast_live::export::ExportReport {
    let settings: Settings = serde_json::from_str(json).unwrap();
    export_video(r, doc, out, ExportOptions::with(settings), |_, _| {})
        .unwrap_or_else(|e| panic!("{} {json}: {e}", out.display()))
}

#[test]
fn every_format() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ultrafast-formats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = Document::from_json(DOC).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let at = |n: &str| -> PathBuf { dir.join(n) };
    // Straight-alpha expectations: transparent, the orange disc, the blue
    // square at half alpha; flattened over black, the square is darker.
    let clear = [0, 0, 0, 0];
    let disc = [255, 122, 46, 255];
    let half = [51, 153, 255, 128];
    let half_on_black = [35, 111, 188, 255];

    // H.264 MP4: 15 frames at 30 fps; and at 60 fps, 30 frames of the same
    // half second.
    let rep = export(&mut r, &doc, &at("a.mp4"), "{}");
    assert_eq!(rep.frames, 15);
    assert_eq!((rep.target.width, rep.target.height), (320, 180));
    let rep = export(&mut r, &doc, &at("b.mp4"), r#"{"fps": 60, "quality": "draft"}"#);
    assert_eq!(rep.frames, 30);
    assert_eq!(probe(&at("b.mp4")), (30, "60/1".into()));
    // Tagged as what it is, so players don't guess (FFmpeg-based ones guess
    // BT.601). Hardware (NVENC, VideoToolbox) or ffmpeg, whichever ran.
    assert_eq!(colour(&at("a.mp4")), BT709, "h264 colour tags");
    assert_eq!(colour(&at("b.mp4")), BT709, "h264 60 fps colour tags");

    // PNG sequence with alpha, and without (flattened).
    export(&mut r, &doc, &at("seq/"), r#"{"alpha": true}"#);
    let first = at("seq").join("00000.png");
    assert_eq!(std::fs::read_dir(at("seq")).unwrap().count(), 15);
    let p = pixels(&first, &[]);
    assert!(
        close(p[0], clear, 0) && close(p[1], disc, 1) && close(p[2], half, 2),
        "png alpha {p:?}"
    );
    export(&mut r, &doc, &at("flat/f-{frame:03}.png"), "{}");
    let p = pixels(&at("flat").join("f-000.png"), &[]);
    assert!(
        close(p[0], [0, 0, 0, 255], 0) && close(p[2], half_on_black, 2),
        "png flat {p:?}"
    );

    // ProRes 4444 with alpha (and 422 HQ without).
    if encoder("prores_ks") {
        export(&mut r, &doc, &at("p.mov"), r#"{"codec": "prores", "alpha": true}"#);
        assert_eq!(probe(&at("p.mov")).0, 15);
        let p = pixels(&at("p.mov"), &[]);
        assert!(
            close(p[0], clear, 1) && close(p[1], disc, 2) && close(p[2], half, 2),
            "prores {p:?}"
        );
        let rep = export(&mut r, &doc, &at("hq.mov"), r#"{"codec": "prores"}"#);
        assert_eq!(rep.target.codec_name(), "prores-hq");
        assert_eq!(prores_colour(&at("p.mov")), BT709, "prores 4444 colour tags");
        assert_eq!(prores_colour(&at("hq.mov")), BT709, "prores hq colour tags");
    }

    // VP9 WebM with alpha (libvpx decodes the alpha plane).
    if encoder("libvpx-vp9") && decoder("libvpx-vp9") {
        export(&mut r, &doc, &at("v.webm"), r#"{"alpha": true}"#);
        let p = pixels(&at("v.webm"), &["-c:v", "libvpx-vp9"]);
        assert!(
            close(p[0], clear, 3) && close(p[1], disc, 4) && close(p[2], half, 4),
            "webm {p:?}"
        );
        assert_eq!(colour(&at("v.webm")), BT709, "vp9 colour tags");
    }

    // GIF: binary transparency, played once.
    export(&mut r, &doc, &at("g.gif"), r#"{"alpha": true, "loops": 0}"#);
    let p = pixels(&at("g.gif"), &[]);
    assert!(p[0][3] == 0 && close(p[1], disc, 8), "gif {p:?}");

    // EXR: linear, premultiplied as the format stores it (the square's
    // blue 1.0 at half alpha reads 0.5).
    if encoder("exr") {
        export(&mut r, &doc, &at("e/{frame:02}.exr"), r#"{"alpha": true}"#);
        let o = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(at("e").join("00.exr"))
            .args(["-f", "rawvideo", "-pix_fmt", "gbrapf32le", "-"])
            .output()
            .unwrap();
        let n = 320 * 180;
        let plane = |c: usize| {
            let i = (c * n + 90 * 320 + 220) * 4;
            f32::from_le_bytes(o.stdout[i..i + 4].try_into().unwrap())
        };
        // gbrap: G, B, R, A planes.
        let (b, a) = (plane(1), plane(3));
        assert!(
            (b - 0.5).abs() < 0.01 && (a - 0.5).abs() < 0.01,
            "exr blue {b} alpha {a}"
        );
    }

    // AV1 where an encoder opens.
    if encoder("libsvtav1") || encoder("libaom-av1") || encoder("av1_nvenc") {
        let rep = export(&mut r, &doc, &at("c.mp4"), r#"{"codec": "av1"}"#);
        assert_eq!(rep.target.codec_name(), "av1");
        assert_eq!(colour(&at("c.mp4")), BT709, "av1 colour tags");
        assert!(std::fs::metadata(at("c.mp4")).unwrap().len() > 0);
    }

    // Sound: a tone, mixed into an MP4, alone as WAV, beside a sequence.
    let tone = Document::from_json(
        r##"{"version":1,"format":{"width":160,"height":90,"fps":30,"duration":0.5},"output":"bg",
        "nodes":{"bg":{"op":"solid","params":{"color":[0,0,0]}},
                 "beep":{"op":"audio","params":{"src":"assets/tone.m4a"}}}}"##,
    )
    .unwrap();
    r.set_base_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples"));
    {
        export(&mut r, &tone, &at("t.mp4"), "{}");
        assert!(has_audio(&at("t.mp4")));
        let rep = export(&mut r, &tone, &at("t.wav"), "{}");
        assert_eq!(rep.frames, 0);
        assert!(has_audio(&at("t.wav")));
        export(&mut r, &tone, &at("ts/"), "{}");
        assert!(at("ts").join("audio.wav").exists());
        export(&mut r, &tone, &at("silent.mp4"), r#"{"audio": false}"#);
        assert!(!has_audio(&at("silent.mp4")));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_combinations_fail_before_rendering() {
    let doc = Document::from_json(DOC).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let dir = std::env::temp_dir();
    for (json, name) in [
        (r#"{"alpha": true}"#, "x.mp4"),
        (r#"{"codec": "prores"}"#, "x.mp4"),
        (r#"{"size": "1080x1080"}"#, "x.mp4"),
        (r#"{"fps": 60}"#, "x.gif"),
    ] {
        let s: Settings = serde_json::from_str(json).unwrap();
        let out = dir.join(name);
        let e = export_video(&mut r, &doc, &out, ExportOptions::with(s), |_, _| {});
        assert!(e.is_err(), "{json} {name} should fail");
    }
}

/// Width and height of the first video stream, and its first frame as RGBA.
fn first_frame(path: &Path) -> (u32, u32, Vec<u8>) {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    let mut it = s.trim().split(',').map(|v| v.parse::<u32>().unwrap());
    let (w, h) = (it.next().unwrap(), it.next().unwrap());
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .unwrap();
    (w, h, o.stdout)
}

#[test]
fn other_sizes_draw_the_document_at_their_size() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ultrafast-sizes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = Document::from_json(DOC).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    let disc = [255, 122, 46, 255];
    let half_on_black = [35, 111, 188, 255];
    // Twice the size (the hardware route where there is one), half the size
    // through ffmpeg with alpha: the picture is where it is at 320x180.
    for (name, json, (w, h), alpha) in [
        ("big.mp4", r#"{"scale": 2}"#, (640, 360), false),
        ("small/", r#"{"size": "160x90", "alpha": true}"#, (160, 90), true),
    ] {
        let rep = export(&mut r, &doc, &dir.join(name), json);
        assert_eq!((rep.target.width, rep.target.height), (w, h), "{json}");
        let file = if alpha {
            dir.join(name).join("00000.png")
        } else {
            dir.join(name)
        };
        let (fw, fh, px) = first_frame(&file);
        assert_eq!((fw, fh), (w, h), "{json}");
        let s = w as f64 / 320.0;
        let at = |x: f64, y: f64| {
            let i = ((y * s) as usize * w as usize + (x * s) as usize) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        // H.264's 4:2:0 color moves a few levels; PNG is exact.
        let tol = if alpha { 3 } else { 20 };
        assert!(close(at(100.0, 90.0), disc, tol), "{json}: disc {:?}", at(100.0, 90.0));
        let square = if alpha { [51, 153, 255, 128] } else { half_on_black };
        let sq = at(220.0, 90.0);
        assert!(close(sq, square, tol), "{json}: square {sq:?}");
        let corner = if alpha { [0, 0, 0, 0] } else { [0, 0, 0, 255] };
        assert!(close(at(2.0, 2.0), corner, tol), "{json}: corner {:?}", at(2.0, 2.0));
    }
    // The renderer is back at the document's size.
    assert_eq!(r.output_size(), None);
    let rep = export(&mut r, &doc, &dir.join("own.mp4"), "{}");
    assert_eq!((rep.target.width, rep.target.height), (320, 180));
    assert_eq!(first_frame(&dir.join("own.mp4")).0, 320);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The grey level (0..255) of every frame as it decodes, and each frame's
/// time in seconds.
fn frame_levels(path: &Path) -> (Vec<f64>, Vec<f64>) {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", "scale=4:4,format=gray", "-f", "rawvideo", "-"])
        .output()
        .unwrap();
    let levels = o
        .stdout
        .chunks_exact(16)
        .map(|c| c.iter().map(|&v| v as f64).sum::<f64>() / 16.0)
        .collect();
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pts_time",
        ])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    // One line a frame; a frame with side data (libx264's first, its
    // settings) has more fields after the time.
    let times = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.split(',').next()?.trim().parse().ok())
        .collect();
    (levels, times)
}

/// Frames come out in the order they're shown, each at its own time, with
/// the encoder's B-frames (decoded in another order) and without: a grey
/// that brightens every frame, through whichever encoder runs here.
#[test]
fn frames_keep_their_order_and_times() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    const RAMP: &str = r##"{
      "version": 1,
      "format": { "width": 320, "height": 180, "fps": 30, "duration": 3 },
      "output": "grey",
      "nodes": {
        "grey": { "op": "solid", "params": { "color": { "keys": [
          { "t": 0, "v": [0, 0, 0, 1] }, { "t": 3, "v": [1, 1, 1, 1] } ] } } }
      }
    }"##;
    let dir = std::env::temp_dir().join(format!("ultrafast-order-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = Document::from_json(RAMP).unwrap();
    let mut r = Renderer::new(Device::new().unwrap()).unwrap();
    for (name, json, fps) in [
        ("h264.mp4", "{}", 30.0),
        ("ntsc.mp4", r#"{"fps": 29.97}"#, 30000.0 / 1001.0),
        ("hevc.mp4", r#"{"codec": "hevc"}"#, 30.0),
        ("h264.mkv", "{}", 30.0),
    ] {
        if name.starts_with("hevc") && !decoder("hevc") {
            continue;
        }
        let path = dir.join(name);
        let rep = export(&mut r, &doc, &path, json);
        let (levels, times) = frame_levels(&path);
        assert_eq!(levels.len() as u64, rep.frames, "{name}: frames decoded");
        for (i, w) in levels.windows(2).enumerate() {
            assert!(
                w[1] > w[0] - 0.5,
                "{name}: frame {} ({}) after frame {i} ({})",
                i + 1,
                w[1],
                w[0]
            );
        }
        // Matroska keeps milliseconds.
        let tol = if name.ends_with(".mkv") { 1e-3 } else { 1e-6 };
        assert_eq!(times.len(), levels.len(), "{name}: frame times");
        for (i, t) in times.iter().enumerate() {
            assert!((t - i as f64 / fps).abs() < tol, "{name}: frame {i} at {t}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
