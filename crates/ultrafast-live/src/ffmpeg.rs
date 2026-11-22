//! ffmpeg command lines for an export [`Target`]: the encoder, its rate
//! control and speed for the quality level, the pixel format, the GIF
//! palette, image sequences and the sound. Pure, so every format's
//! arguments are tested without a GPU; [`encoder_opens`] is the only probe.

use crate::output::{AudioCodec, Container, Quality, Rate, Target, VideoCodec};

/// BT.709 at limited range, which every YUV export is, set on the frames
/// after conversion. The encoder writes the frames' own tags: newer ffmpeg
/// (9.0) ignores `-color_primaries` and `-color_trc` when a filter has
/// run, so files came out untagged; 4.4 and 6.1 took the options.
pub const BT709_FRAMES: &str = "scale=out_color_matrix=bt709:out_range=tv,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709";

/// The same as stream and container tags, for encoders and for stream
/// copies (a raw stream copied in carries no frames to take tags from).
pub const BT709_TAGS: [&str; 8] = [
    "-colorspace",
    "bt709",
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-color_range",
    "tv",
];

/// How frames reach ffmpeg's stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixels {
    /// sRGB RGBA8; straight alpha, or flattened over black when opaque.
    Rgba8,
    /// sRGB-encoded half floats (10-bit codecs keep their precision).
    Rgba16fSrgb,
    /// Linear half floats with straight alpha (EXR, which stores alpha
    /// premultiplied: [`video_args`] premultiplies it).
    Rgba16fLinear,
}

impl Pixels {
    /// What a target is fed.
    pub fn for_target(t: &Target) -> Pixels {
        match t.codec() {
            Some(VideoCodec::Exr) => Pixels::Rgba16fLinear,
            Some(VideoCodec::Prores) => Pixels::Rgba16fSrgb,
            _ => Pixels::Rgba8,
        }
    }

    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Pixels::Rgba8 => "rgba",
            Pixels::Rgba16fSrgb | Pixels::Rgba16fLinear => "rgbaf16le",
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Pixels::Rgba8 => 4,
            _ => 8,
        }
    }
}

/// The ffmpeg encoder a video codec uses here: the NVIDIA one when ffmpeg
/// has it and it opens, else the software one.
pub fn video_encoder(codec: VideoCodec, opens: &dyn Fn(&str) -> bool) -> &'static str {
    match codec {
        VideoCodec::H264 if opens("h264_nvenc") => "h264_nvenc",
        VideoCodec::H264 => "libx264",
        VideoCodec::Hevc if opens("hevc_nvenc") => "hevc_nvenc",
        VideoCodec::Hevc => "libx265",
        VideoCodec::Av1 if opens("av1_nvenc") => "av1_nvenc",
        VideoCodec::Av1 if opens("libsvtav1") => "libsvtav1",
        VideoCodec::Av1 => "libaom-av1",
        VideoCodec::Vp9 => "libvpx-vp9",
        VideoCodec::Prores => "prores_ks",
        VideoCodec::Gif => "gif",
        VideoCodec::Png => "png",
        VideoCodec::Exr => "exr",
    }
}

fn by_quality<T: Copy>(q: Quality, v: [T; 4]) -> T {
    v[q as usize]
}

/// Arguments for the picture, after the inputs: filters, encoder, rate
/// control, pixel format and color tags. `encoder` from [`video_encoder`].
pub fn video_args(t: &Target, encoder: &str) -> Vec<String> {
    let v = t.video.expect("a picture");
    let q = t.quality;
    let mut a: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    let yuv = !matches!(v.codec, VideoCodec::Gif | VideoCodec::Png | VideoCodec::Exr);
    if yuv {
        // The BT.709 matrix explicitly (older ffmpeg converts with BT.601
        // whatever the output is tagged), and the frames tagged with it.
        push(&["-vf", BT709_FRAMES]);
    }
    match v.codec {
        VideoCodec::Gif => {
            let stats = by_quality(q, ["diff", "diff", "full", "single"]);
            let colors = by_quality(q, ["128", "256", "256", "256"]);
            let dither = by_quality(
                q,
                [
                    "bayer:bayer_scale=3",
                    "sierra2_4a",
                    "floyd_steinberg",
                    "floyd_steinberg",
                ],
            );
            let new = if stats == "single" { ":new=1" } else { "" };
            let (reserve, threshold) = if v.alpha {
                (":reserve_transparent=1", ":alpha_threshold=128")
            } else {
                (":reserve_transparent=0", "")
            };
            push(&[
                "-filter_complex",
                &format!(
                    "[0:v]split[a][b];[a]palettegen=max_colors={colors}:stats_mode={stats}{reserve}[p];[b][p]paletteuse=dither={dither}{new}{threshold}"
                ),
            ]);
            // ffmpeg: -1 plays once, 0 forever, n repeats n times.
            let lp = match t.loops {
                None => "0".to_string(),
                Some(0) => "-1".to_string(),
                Some(n) => n.to_string(),
            };
            push(&["-c:v", "gif", "-loop", &lp]);
        }
        VideoCodec::Png => {
            push(&["-c:v", "png", "-pix_fmt", if v.alpha { "rgba" } else { "rgb24" }]);
            if q == Quality::Draft {
                push(&["-compression_level", "1"]);
            }
        }
        VideoCodec::Exr => {
            if v.alpha {
                // EXR stores alpha premultiplied. ffmpeg before 8.1 writes
                // what it's given; 8.1 on would premultiply itself, and
                // skips that after this filter, which marks its output.
                push(&["-vf", "premultiply=inplace=1"]);
            }
            push(&[
                "-c:v",
                "exr",
                "-format",
                "half",
                "-compression",
                "zip1",
                "-pix_fmt",
                if v.alpha { "gbrapf32le" } else { "gbrpf32le" },
            ]);
        }
        VideoCodec::Prores => {
            let p = v.prores.expect("a ProRes profile");
            let pix = match (p.has_alpha(), v.alpha) {
                (true, true) => "yuva444p10le",
                (true, false) => "yuv444p10le",
                _ => "yuv422p10le",
            };
            push(&[
                "-c:v",
                "prores_ks",
                "-profile:v",
                &p.ffmpeg_profile().to_string(),
                "-vendor",
                "apl0",
                "-pix_fmt",
                pix,
            ]);
            if v.alpha {
                push(&["-alpha_bits", "16"]);
            }
        }
        VideoCodec::H264 | VideoCodec::Hevc | VideoCodec::Av1 | VideoCodec::Vp9 => {
            push(&["-c:v", encoder]);
            let nvenc = encoder.ends_with("_nvenc");
            match encoder {
                _ if nvenc => {
                    let preset = by_quality(q, ["p1", "p2", "p5", "p7"]);
                    push(&["-preset", preset, "-tune", "hq"]);
                    if q == Quality::Master {
                        push(&["-multipass", "fullres"]);
                    }
                }
                "libx264" | "libx265" => {
                    push(&["-preset", by_quality(q, ["veryfast", "medium", "slow", "slower"])]);
                    if encoder == "libx265" {
                        push(&["-x265-params", "log-level=error"]);
                    }
                }
                "libsvtav1" => push(&["-preset", by_quality(q, ["10", "7", "5", "3"])]),
                "libaom-av1" => push(&["-cpu-used", by_quality(q, ["8", "6", "4", "3"]), "-row-mt", "1"]),
                "libvpx-vp9" => {
                    let deadline = if q == Quality::Draft { "realtime" } else { "good" };
                    push(&[
                        "-deadline",
                        deadline,
                        "-cpu-used",
                        by_quality(q, ["8", "2", "1", "1"]),
                        "-row-mt",
                        "1",
                    ]);
                }
                _ => {}
            }
            match v.rate.expect("rate control") {
                Rate::Crf(c) => {
                    let c = format!("{c}");
                    if nvenc {
                        push(&["-rc", "vbr", "-cq", &c, "-b:v", "0"]);
                    } else if matches!(encoder, "libvpx-vp9" | "libaom-av1") {
                        // Constant quality needs the bitrate cap off.
                        push(&["-crf", &c, "-b:v", "0"]);
                    } else {
                        push(&["-crf", &c]);
                    }
                }
                Rate::Bitrate(b) => {
                    let (b, max, buf) = (b.to_string(), (b * 2).to_string(), (b * 4).to_string());
                    if nvenc {
                        push(&["-rc", "vbr"]);
                    }
                    push(&["-b:v", &b, "-maxrate", &max, "-bufsize", &buf]);
                }
            }
            let pix = if v.alpha { "yuva420p" } else { "yuv420p" };
            push(&["-pix_fmt", pix]);
            if v.alpha {
                // libvpx keeps the alpha plane only without alt-ref frames.
                push(&["-auto-alt-ref", "0", "-metadata:s:v:0", "alpha_mode=1"]);
            }
            if v.codec == VideoCodec::H264 {
                push(&["-profile:v", "high"]);
            }
            if v.codec == VideoCodec::Hevc && matches!(t.container, Container::Mp4 | Container::Mov) {
                // The tag Apple players need.
                push(&["-tag:v", "hvc1"]);
            }
        }
    }
    if yuv {
        push(&BT709_TAGS);
    }
    a
}

/// Arguments for the sound (none for silent outputs).
pub fn audio_args(t: &Target) -> Vec<String> {
    let Some(a) = t.audio else { return Vec::new() };
    let enc = match a.codec {
        AudioCodec::Aac => "aac",
        AudioCodec::Opus => "libopus",
        AudioCodec::Mp3 => "libmp3lame",
        AudioCodec::Flac => "flac",
        AudioCodec::Pcm => "pcm_s16le",
        AudioCodec::Pcm24 => "pcm_s24le",
    };
    let mut v = vec!["-c:a".to_string(), enc.to_string()];
    if let Some(b) = a.bitrate {
        v.extend(["-b:a".to_string(), b.to_string()]);
    }
    v
}

/// Arguments for the container, before the output path: the muxer and
/// what players need (`+faststart`, a sequence's first number).
pub fn mux_args(t: &Target, first_frame: u64) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    match t.container {
        Container::Mp4 | Container::Mov | Container::M4a => a.extend(["-movflags".into(), "+faststart".into()]),
        Container::Png | Container::Exr => a.extend([
            "-f".into(),
            "image2".into(),
            "-start_number".into(),
            first_frame.to_string(),
        ]),
        Container::Opus => a.extend(["-f".into(), "ogg".into()]),
        _ => {}
    }
    a
}

/// Whether ffmpeg can open `encoder` here (one tiny frame, checked once per
/// encoder per process). Distro builds list NVENC without an NVIDIA
/// driver, and software encoders are optional at build time.
pub fn encoder_opens(encoder: &str) -> bool {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<BTreeMap<String, bool>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some(&ok) = seen.lock().unwrap().get(encoder) {
        return ok;
    }
    let ok = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "quiet", "-nostdin", "-f", "lavfi", "-i"])
        .args([
            "color=size=256x256:duration=0.04",
            "-frames:v",
            "1",
            "-c:v",
            encoder,
            "-f",
            "null",
            "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    seen.lock().unwrap().insert(encoder.to_string(), ok);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Settings;
    use std::path::Path;
    use ultrafast_graph::Format;

    fn target(json: &str, path: &str) -> Target {
        let s: Settings = serde_json::from_str(json).unwrap();
        let f = Format {
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration: 2.0,
            motion_blur: None,
        };
        s.resolve(Some(Path::new(path)), &f).unwrap()
    }

    fn joined(t: &Target, enc: &str) -> String {
        video_args(t, enc).join(" ")
    }

    #[test]
    fn encoders_fall_back_to_software() {
        let none = |_: &str| false;
        let all = |_: &str| true;
        assert_eq!(video_encoder(VideoCodec::H264, &none), "libx264");
        assert_eq!(video_encoder(VideoCodec::H264, &all), "h264_nvenc");
        assert_eq!(video_encoder(VideoCodec::Av1, &none), "libaom-av1");
        assert_eq!(video_encoder(VideoCodec::Av1, &|e| e == "libsvtav1"), "libsvtav1");
    }

    #[test]
    fn quality_sets_rate_and_speed() {
        let t = target("{}", "f.mp4");
        let a = joined(&t, "libx264");
        assert!(
            a.contains("-preset medium") && a.contains("-crf 18") && a.contains("-pix_fmt yuv420p"),
            "{a}"
        );
        assert!(
            a.contains("out_color_matrix=bt709")
                && a.contains("setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709")
                && a.contains("-colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv"),
            "{a}"
        );
        let t = target(r#"{"quality": "master"}"#, "f.mp4");
        let a = joined(&t, "h264_nvenc");
        assert!(
            a.contains("-preset p7") && a.contains("-cq 12") && a.contains("-multipass fullres"),
            "{a}"
        );
        let t = target(r#"{"bitrate": "8M"}"#, "f.mp4");
        let a = joined(&t, "libx264");
        assert!(a.contains("-b:v 8000000 -maxrate 16000000"), "{a}");
        let t = target(r#"{"quality": "draft"}"#, "f.webm");
        let a = joined(&t, "libvpx-vp9");
        assert!(a.contains("-deadline realtime") && a.contains("-crf 42 -b:v 0"), "{a}");
        let t = target(r#"{"codec": "hevc"}"#, "f.mp4");
        assert!(joined(&t, "libx265").contains("-tag:v hvc1"));
    }

    #[test]
    fn alpha_formats() {
        let t = target(r#"{"alpha": true}"#, "f.webm");
        let a = joined(&t, "libvpx-vp9");
        assert!(a.contains("yuva420p") && a.contains("-auto-alt-ref 0"), "{a}");
        let t = target(r#"{"codec": "prores", "alpha": true}"#, "f.mov");
        let a = joined(&t, "prores_ks");
        assert!(a.contains("-profile:v 4") && a.contains("yuva444p10le"), "{a}");
        assert_eq!(Pixels::for_target(&t), Pixels::Rgba16fSrgb);
        let t = target(r#"{"codec": "prores", "prores_profile": "proxy"}"#, "f.mov");
        assert!(joined(&t, "prores_ks").contains("-profile:v 0 -vendor apl0 -pix_fmt yuv422p10le"));
        let t = target(r#"{"alpha": true}"#, "frames/f-{frame:05}.exr");
        let a = joined(&t, "exr");
        assert!(a.contains("gbrapf32le") && !a.contains("bt709"), "{a}");
        assert!(a.contains("-vf premultiply=inplace=1"), "{a}");
        let o = joined(&target("{}", "frames/f-{frame:05}.exr"), "exr");
        assert!(!o.contains("premultiply"), "{o}");
        assert_eq!(Pixels::for_target(&t), Pixels::Rgba16fLinear);
        assert_eq!(mux_args(&t, 30), ["-f", "image2", "-start_number", "30"]);
    }

    #[test]
    fn gifs() {
        let t = target("{}", "f.gif");
        let a = joined(&t, "gif");
        assert!(
            a.contains("palettegen=max_colors=256:stats_mode=diff") && a.contains("-loop 0"),
            "{a}"
        );
        let t = target(r#"{"loops": 0, "alpha": true}"#, "f.gif");
        let a = joined(&t, "gif");
        assert!(a.contains("-loop -1") && a.contains("alpha_threshold=128"), "{a}");
        let t = target(r#"{"quality": "master"}"#, "f.gif");
        assert!(joined(&t, "gif").contains("stats_mode=single") && joined(&t, "gif").contains("new=1"));
    }

    /// Every format's arguments, run through this machine's ffmpeg on a
    /// few generated frames (encoders it lacks are skipped).
    #[test]
    fn every_format_encodes() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("no ffmpeg: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ultrafast-ffmpeg-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            ("{}", "a.mp4"),
            (r#"{"codec": "hevc", "quality": "draft"}"#, "b.mp4"),
            (r#"{"codec": "av1"}"#, "c.mp4"),
            (r#"{"codec": "av1"}"#, "c.webm"),
            ("{}", "d.webm"),
            (r#"{"alpha": true}"#, "e.webm"),
            (r#"{"codec": "prores"}"#, "f.mov"),
            (r#"{"codec": "prores", "alpha": true}"#, "g.mov"),
            (r#"{"codec": "vp9"}"#, "h.mkv"),
            ("{}", "i.gif"),
            (r#"{"alpha": true, "loops": 2, "quality": "master"}"#, "j.gif"),
            (r#"{"alpha": true}"#, "k/{frame:03}.png"),
            (r#"{"alpha": true}"#, "l/{frame:03}.exr"),
            (r#"{"bitrate": "2M", "quality": "high"}"#, "m.mp4"),
        ];
        let (w, h, n) = (256u32, 144u32, 4u64);
        for (json, name) in cases {
            let mut t = target(json, name);
            t.width = w;
            t.height = h;
            let pixels = Pixels::for_target(&t);
            let encoder = video_encoder(t.codec().unwrap(), &encoder_opens);
            if !encoder_opens(encoder) {
                eprintln!("{name}: {encoder} doesn't open here: skipped");
                continue;
            }
            let out = dir.join(name);
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            let path = if t.container.is_sequence() {
                crate::output::sequence_pattern(&out, t.container)
            } else {
                out.to_string_lossy().into_owned()
            };
            let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt"]
                .map(String::from)
                .to_vec();
            args.extend([
                pixels.ffmpeg_name().into(),
                "-s".into(),
                format!("{w}x{h}"),
                "-r".into(),
                "30".into(),
                "-i".into(),
                "-".into(),
            ]);
            args.extend(video_args(&t, encoder));
            args.extend(mux_args(&t, 0));
            args.push(path);
            let mut child = Command::new("ffmpeg")
                .args(&args)
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            for f in 0..n {
                let mut frame = Vec::new();
                for y in 0..h {
                    for x in 0..w {
                        let rgba = [x as f32 / w as f32, y as f32 / h as f32, f as f32 / n as f32, 0.5];
                        match pixels {
                            Pixels::Rgba8 => frame.extend(rgba.map(|c| (c * 255.0) as u8)),
                            _ => rgba.iter().for_each(|&c| frame.extend(half(c).to_le_bytes())),
                        }
                    }
                }
                stdin.write_all(&frame).unwrap();
            }
            drop(stdin);
            let res = child.wait_with_output().unwrap();
            assert!(
                res.status.success(),
                "{name} ({json}): ffmpeg {}\n{}",
                args.join(" "),
                String::from_utf8_lossy(&res.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// f32 to IEEE half, for the test frames.
    fn half(v: f32) -> u16 {
        let b = v.to_bits();
        let sign = ((b >> 16) & 0x8000) as u16;
        let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
        let man = ((b >> 13) & 0x3ff) as u16;
        if v == 0.0 || exp <= 0 {
            sign
        } else {
            sign | ((exp as u16) << 10) | man
        }
    }

    #[test]
    fn sound() {
        assert_eq!(audio_args(&target("{}", "f.mp4")), ["-c:a", "aac", "-b:a", "192000"]);
        assert_eq!(
            audio_args(&target("{}", "f.webm")),
            ["-c:a", "libopus", "-b:a", "128000"]
        );
        assert_eq!(
            audio_args(&target(r#"{"codec": "prores"}"#, "f.mov")),
            ["-c:a", "pcm_s16le"]
        );
        assert!(audio_args(&target("{}", "f.gif")).is_empty());
        assert_eq!(mux_args(&target("{}", "f.opus"), 0), ["-f", "ogg"]);
    }
}
