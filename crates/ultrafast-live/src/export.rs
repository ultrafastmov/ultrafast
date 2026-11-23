//! Export: a document to a video, an animated GIF, an image sequence or
//! sound alone, in the container, codecs, quality level, size and frame
//! rate its [`Settings`] ask for (see [`crate::output`]).
//!
//! - H.264 and HEVC without alpha go through the hardware encoder with no
//!   readback. On macOS / Metal every frame is rendered on the GPU straight
//!   into the encoder's pixel buffers, a few frames in flight. On Linux and
//!   Windows with NVIDIA every frame is rendered straight into NV12 buffers
//!   shared with CUDA that NVENC encodes; `ffmpeg` only muxes the
//!   compressed stream and the sound.
//! - Everything else (ProRes, AV1, VP9, transparency, GIF, PNG and EXR
//!   sequences), and H.264 / HEVC without those encoders, is read back and
//!   piped to `ffmpeg`, which uses NVENC where it opens. Every readback is
//!   counted in the transfer stats.
//! - Sound alone is mixed and encoded without rendering a frame.

#[cfg(any(all(target_os = "macos", not(feature = "vulkan")), target_os = "linux", windows))]
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;
use ultrafast_graph::Document;
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
use ultrafast_media::Writer;
use ultrafast_render::{Mode, Readback, Renderer};

use crate::ffmpeg::{self, Pixels};
#[cfg(any(all(target_os = "macos", not(feature = "vulkan")), target_os = "linux", windows))]
use crate::output::Rate;
use crate::output::{Container, Quality, Settings, Target, VideoCodec};

#[derive(Debug, Clone, Copy, Default)]
pub struct ExportOptions {
    /// What to write: format, codecs, quality, size, frame rate, sound.
    pub settings: Settings,
    /// Range to export in seconds (default: the whole document).
    pub start: f64,
    pub end: Option<f64>,
    /// Give up with [`DEADLINE_PASSED`] once this passes (a caller that
    /// stops waiting); checked before every frame.
    pub deadline: Option<Instant>,
}

impl ExportOptions {
    /// Default options with these settings.
    pub fn with(settings: Settings) -> Self {
        ExportOptions {
            settings,
            ..Default::default()
        }
    }
}

/// The error of an export stopped by [`ExportOptions::deadline`].
pub const DEADLINE_PASSED: &str = "deadline passed";

impl ExportOptions {
    fn check_deadline(&self) -> Result<(), String> {
        match self.deadline {
            Some(d) if Instant::now() >= d => Err(DEADLINE_PASSED.into()),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExportReport {
    /// Frames written (none for sound alone).
    pub frames: u64,
    pub seconds: f64,
    /// Frames rendered per second of wall time.
    pub fps: f64,
    pub readbacks: u64,
    pub timing: ExportTiming,
    /// What was written: container, codecs, size and frame rate.
    pub target: Target,
}

/// Where an export's wall time went, in seconds (summed over frames).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExportTiming {
    /// CPU encoding frames: planning, params, geometry, recording commands.
    pub render_cpu: f64,
    /// GPU execution of the frames (timestamp queries).
    pub gpu: f64,
    /// Waiting for frames the GPU had not finished.
    pub gpu_wait: f64,
    /// Handing frames to the encoder and taking its bitstream (on the
    /// encoder's own thread where it has one, overlapping rendering).
    pub encode: f64,
    /// Rendering blocked on an encoder thread that had fallen behind.
    pub encode_wait: f64,
    /// Waiting for decoded media and uploading decoded images (part of
    /// `render_cpu`).
    pub media_wait: f64,
    /// The images' part of `media_wait`.
    pub image_wait: f64,
    /// Bytes copied to the GPU: decoded video frames and images, data.
    pub upload_bytes: u64,
}

impl ExportTiming {
    /// One line: per-frame milliseconds of each part.
    pub fn summary(&self, frames: u64) -> String {
        let ms = |s: f64| s * 1000.0 / frames.max(1) as f64;
        format!(
            "per frame: cpu {:.1} ms (media {:.1} ms, of it images {:.1} ms), gpu {:.1} ms, waiting on gpu {:.1} ms, \
             encoder {:.1} ms (waiting on it {:.1} ms), uploads {:.2} MB",
            ms(self.render_cpu),
            ms(self.media_wait),
            ms(self.image_wait),
            ms(self.gpu),
            ms(self.gpu_wait),
            ms(self.encode),
            ms(self.encode_wait),
            self.upload_bytes as f64 / 1e6 / frames.max(1) as f64
        )
    }

    fn add(&mut self, o: &ExportTiming) {
        self.render_cpu += o.render_cpu;
        self.gpu += o.gpu;
        self.gpu_wait += o.gpu_wait;
        self.encode += o.encode;
        self.encode_wait += o.encode_wait;
        self.media_wait += o.media_wait;
        self.image_wait += o.image_wait;
        self.upload_bytes += o.upload_bytes;
    }
}

/// The output's frames: `total` of them at `fps`, the first at frame
/// `first` of the output's own clock. Times are seconds of the document,
/// so another frame rate plays the film at the same speed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub fps: f64,
    pub first: u64,
    pub total: u64,
}

impl Clock {
    pub fn new(fps: f64, start: f64, end: f64) -> Clock {
        let first = (start.max(0.0) * fps).round() as u64;
        let last = (end * fps).round() as u64;
        Clock {
            fps,
            first,
            total: last.saturating_sub(first),
        }
    }

    /// The document time of output frame `i`.
    pub fn time(&self, i: u64) -> f64 {
        (self.first + i) as f64 / self.fps
    }

    /// Where the frames start and end, in seconds.
    pub fn span(&self) -> (f64, f64) {
        (self.time(0), self.time(self.total))
    }
}

/// The mp4 video clock for a frame rate: (timescale, ticks per frame).
/// 30 fps is 1/30000 with 1000 ticks a frame, 29.97 is 1/30000 with 1001.
#[cfg_attr(all(target_os = "macos", not(feature = "vulkan")), allow(dead_code))]
fn mp4_clock(fps: f64) -> (u32, u32) {
    let ntsc = fps * 1.001;
    if (ntsc - ntsc.round()).abs() < 1e-3 && (fps - fps.round()).abs() > 1e-3 {
        ((ntsc.round() * 1000.0) as u32, 1001)
    } else {
        ((fps * 1000.0).round() as u32, 1000)
    }
}

/// A frame rate as ffmpeg takes it: `30000/1001` for NTSC rates.
fn rate_arg(fps: f64) -> String {
    let (scale, ticks) = mp4_clock(fps);
    if ticks == 1001 {
        format!("{scale}/1001")
    } else {
        format!("{fps}")
    }
}

/// Exports `doc` to `path` as `opts.settings` ask (the format from the
/// extension unless they name one).
pub fn export_video(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    opts: ExportOptions,
    progress: impl FnMut(u64, u64),
) -> Result<ExportReport, String> {
    let target = opts.settings.resolve(Some(path), &doc.format)?;
    export_target(renderer, doc, path, target, opts, progress)
}

/// Exports `doc` to `path` as an already resolved `target`.
pub fn export_target(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    target: Target,
    opts: ExportOptions,
    progress: impl FnMut(u64, u64),
) -> Result<ExportReport, String> {
    export_target_as(renderer, doc, path, target, opts, progress, Whole::File)
}

/// What an export writes: a whole file, or one run of a parallel export
/// that is joined to the others afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Whole {
    File,
    /// Joined as it is by ffmpeg's concat demuxer, which lines runs up by
    /// their decode times: no B-frames, so those are the frames' own times
    /// (whichever encoder, NVENC or ffmpeg's, wrote each run).
    Run,
}

fn export_target_as(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    target: Target,
    opts: ExportOptions,
    mut progress: impl FnMut(u64, u64),
    whole: Whole,
) -> Result<ExportReport, String> {
    let fmt = doc.format;
    let end = opts.end.unwrap_or(fmt.duration).min(fmt.duration);
    let clock = Clock::new(target.fps, opts.start, end);
    // The quality level's motion blur, on a copy of the document.
    let tuned;
    let mb = target.quality.motion_blur(fmt.motion_blur);
    let doc = if mb != fmt.motion_blur.filter(|m| m.samples > 1) {
        let mut d = doc.fork();
        d.format.motion_blur = mb;
        tuned = d;
        &tuned
    } else {
        doc
    };
    if target.video.is_none() {
        return export_audio(renderer.base_dir(), doc, path, target, clock);
    }
    // Another size draws the document at that density (text, paths, blurs
    // and 3D rendered for it, not a scaled image); the renderer goes back to
    // its own size afterwards.
    let own = renderer.output_size();
    let size = (!target.native_size(&fmt)).then_some((target.width, target.height));
    renderer.set_output_size(size);
    let result = export_frames(renderer, doc, path, target, clock, opts, &mut progress, whole);
    renderer.set_output_size(own);
    result
}

/// The picture of an export, routed to the encoder that takes it.
#[allow(clippy::too_many_arguments)]
fn export_frames(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    target: Target,
    clock: Clock,
    opts: ExportOptions,
    progress: &mut dyn FnMut(u64, u64),
    whole: Whole,
) -> Result<ExportReport, String> {
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = whole;
    #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
    if metal::takes(&target, &opts.settings) {
        return metal::export(renderer, doc, path, target, clock, opts, progress);
    }
    #[cfg(any(target_os = "linux", windows))]
    if nvenc::takes(&target) && std::env::var_os("ULTRAFAST_NVENC").is_none_or(|v| v != "0") {
        match nvenc::export(renderer, doc, path, target, clock, opts, progress, whole) {
            Ok(r) => return Ok(r),
            Err(nvenc::Error::Unavailable(e)) => {
                eprintln!("ultrafast: NVENC unavailable ({e}); exporting through ffmpeg with readbacks")
            }
            Err(nvenc::Error::Failed(e)) => return Err(e),
        }
    }
    export_ffmpeg(renderer, doc, path, target, clock, opts, progress)
}

/// The bitrate a hardware encoder without constant quality is given: about
/// 0.12 bits a pixel a frame for H.264 at standard quality, less for HEVC,
/// scaled by the quality level.
#[cfg_attr(
    not(any(all(target_os = "macos", not(feature = "vulkan")), target_os = "linux", windows)),
    allow(dead_code)
)]
fn hardware_bitrate(t: &Target) -> u64 {
    let bpp = match t.codec() {
        Some(VideoCodec::Hevc) => 0.08,
        _ => 0.12,
    };
    let scale = match t.quality {
        Quality::Draft => 0.4,
        Quality::Standard => 1.0,
        Quality::High => 1.6,
        Quality::Master => 2.5,
    };
    (t.width as f64 * t.height as f64 * t.fps * bpp * scale) as u64
}

/// The document's sound over `from..to` seconds as a temporary 32-bit
/// float WAV (None when it has none).
fn audio_wav(base: &Path, doc: &Document, (from, to): (f64, f64)) -> Result<Option<PathBuf>, String> {
    let rate = ultrafast_media::audio::RATE as f64;
    let a = (from * rate).round() as usize;
    let b = (to * rate).round() as usize;
    // Only what these frames play of each source gets decoded.
    let Some(mix) = crate::audio_mix::AudioMix::from_doc_range(doc, base, Some((a, b)))? else {
        return Ok(None);
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let wav = std::env::temp_dir().join(format!(
        "ultrafast-export-{}-{}.wav",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    write_wav_f32(&wav, &mix.render(a, b.saturating_sub(a))).map_err(|e| e.to_string())?;
    Ok(Some(wav))
}

/// Interleaved float samples as a WAV (IEEE float, the mix's rate and
/// channels): what every encoder reads without losing bits.
fn write_wav_f32(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let ch = ultrafast_media::audio::CHANNELS as u16;
    let rate = ultrafast_media::audio::RATE;
    let data = (samples.len() * 4) as u32;
    let mut f = Vec::with_capacity(44 + data as usize);
    f.extend(b"RIFF");
    f.extend((36 + data).to_le_bytes());
    f.extend(b"WAVEfmt ");
    f.extend(16u32.to_le_bytes());
    f.extend(3u16.to_le_bytes());
    f.extend(ch.to_le_bytes());
    f.extend(rate.to_le_bytes());
    f.extend((rate * ch as u32 * 4).to_le_bytes());
    f.extend((ch * 4).to_le_bytes());
    f.extend(32u16.to_le_bytes());
    f.extend(b"data");
    f.extend(data.to_le_bytes());
    for s in samples {
        f.extend(s.to_le_bytes());
    }
    std::fs::write(path, f)
}

/// Runs ffmpeg with `args`, naming it in the error.
fn run_ffmpeg(args: &[String], what: &str) -> Result<(), String> {
    let status = std::process::Command::new("ffmpeg")
        .args(args)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("ffmpeg is required to {what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg ({what}) failed ({status})"))
    }
}

/// Where a sequence's sound goes: `audio.wav` in its folder.
fn sidecar_path(path: &Path, t: &Target) -> PathBuf {
    let pattern = crate::output::sequence_pattern(path, t.container);
    Path::new(&pattern).parent().unwrap_or(Path::new(".")).join("audio.wav")
}

/// Writes the document's sound beside an image sequence (as editors take
/// it), when it has some and sound wasn't turned off.
fn write_sidecar(
    base: &Path,
    doc: &Document,
    path: &Path,
    t: &Target,
    clock: Clock,
    s: &Settings,
) -> Result<(), String> {
    if s.audio == Some(false) {
        return Ok(());
    }
    let Some(wav) = audio_wav(base, doc, clock.span())? else {
        return Ok(());
    };
    let out = sidecar_path(path, t);
    let args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-i"]
        .map(String::from)
        .into_iter()
        .chain([wav.to_string_lossy().into_owned()])
        .chain(["-c:a", "pcm_s24le"].map(String::from))
        .chain([out.to_string_lossy().into_owned()])
        .collect();
    let r = run_ffmpeg(&args, "write the sequence's sound");
    let _ = std::fs::remove_file(&wav);
    r
}

/// Sound alone: the mix, encoded; nothing rendered.
fn export_audio(base: &Path, doc: &Document, path: &Path, t: Target, clock: Clock) -> Result<ExportReport, String> {
    let start = Instant::now();
    let wav = audio_wav(base, doc, clock.span())?.ok_or("the document has no sound to export")?;
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-i"]
        .map(String::from)
        .to_vec();
    args.push(wav.to_string_lossy().into_owned());
    args.extend(ffmpeg::audio_args(&t));
    args.extend(ffmpeg::mux_args(&t, 0));
    args.push(path.to_string_lossy().into_owned());
    let r = run_ffmpeg(&args, "encode the sound");
    let _ = std::fs::remove_file(&wav);
    r?;
    Ok(ExportReport {
        frames: 0,
        seconds: start.elapsed().as_secs_f64(),
        fps: 0.0,
        readbacks: 0,
        timing: ExportTiming::default(),
        target: t,
    })
}

/// Frames read back and piped to ffmpeg: every codec, any platform.
fn export_ffmpeg(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    t: Target,
    clock: Clock,
    opts: ExportOptions,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<ExportReport, String> {
    use std::io::Write;
    let v = t.video.expect("a picture");
    let pixels = Pixels::for_target(&t);
    let readback = match pixels {
        Pixels::Rgba8 => Readback::Srgb8,
        Pixels::Rgba16fSrgb => Readback::SrgbHalf,
        Pixels::Rgba16fLinear => Readback::LinearHalf,
    };
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map_err(|e| format!("ffmpeg is required to export {}: {e}", t.codec_name()))?;
    let encoder = ffmpeg::video_encoder(v.codec, &ffmpeg::encoder_opens);
    let out = if t.container.is_sequence() {
        let pattern = crate::output::sequence_pattern(path, t.container);
        if let Some(dir) = Path::new(&pattern).parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        pattern
    } else {
        path.to_string_lossy().into_owned()
    };
    let wav = match t.audio {
        Some(_) => audio_wav(renderer.base_dir(), doc, clock.span())?,
        None => None,
    };
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        pixels.ffmpeg_name(),
        "-s",
        &format!("{}x{}", t.width, t.height),
        "-r",
        &rate_arg(t.fps),
        "-i",
        "-",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(wav) = &wav {
        args.extend(["-i".to_string(), wav.to_string_lossy().into_owned()]);
        args.extend(["-map", "0:v", "-map", "1:a"].map(String::from));
    }
    args.extend(ffmpeg::video_args(&t, encoder));
    if wav.is_some() {
        args.extend(ffmpeg::audio_args(&t));
    }
    args.extend(ffmpeg::mux_args(&t, clock.first));
    args.push(out);
    let mut child = std::process::Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start ffmpeg: {e}"))?;
    let prev_mode = renderer.mode();
    renderer.set_mode(Mode::Exact);
    renderer.set_sequential(true);
    let before = renderer.device().stats();
    let media0 = renderer.media_wait();
    let image0 = renderer.image_wait();
    let start = Instant::now();
    let mut stdin = child.stdin.take().unwrap();
    let mut timing = ExportTiming::default();
    let result = (|| -> Result<(), String> {
        for i in 0..clock.total {
            opts.check_deadline()?;
            let c0 = Instant::now();
            let (px, _) = renderer
                .render_readback(doc, clock.time(i), readback, !v.alpha)
                .map_err(|e| e.to_string())?;
            timing.render_cpu += c0.elapsed().as_secs_f64();
            let e0 = Instant::now();
            stdin.write_all(&px).map_err(|e| e.to_string())?;
            timing.encode += e0.elapsed().as_secs_f64();
            progress(i + 1, clock.total);
        }
        Ok(())
    })();
    drop(stdin);
    renderer.set_mode(prev_mode);
    renderer.set_sequential(false);
    let status = child.wait().map_err(|e| e.to_string())?;
    if let Some(wav) = &wav {
        let _ = std::fs::remove_file(wav);
    }
    // A dead ffmpeg shows up first as a broken pipe: name ffmpeg instead.
    match (result, status.success()) {
        (Err(e), true) => return Err(e),
        (Err(e), false) => return Err(format!("ffmpeg ({encoder}) failed ({status}): {e}")),
        (Ok(()), false) => return Err(format!("ffmpeg ({encoder}) failed ({status})")),
        (Ok(()), true) => {}
    }
    if t.container.is_sequence() {
        write_sidecar(renderer.base_dir(), doc, path, &t, clock, &opts.settings)?;
    }
    let seconds = start.elapsed().as_secs_f64();
    let stats = renderer.device().stats() - before;
    timing.media_wait = renderer.media_wait() - media0;
    timing.image_wait = renderer.image_wait() - image0;
    timing.upload_bytes = stats.upload_bytes;
    Ok(ExportReport {
        frames: clock.total,
        seconds,
        fps: clock.total as f64 / seconds.max(1e-9),
        readbacks: stats.readbacks,
        timing,
        target: t,
    })
}

/// Zero-readback export through AVFoundation (macOS, Metal).
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
mod metal {
    use super::*;

    /// Frames encoded on the GPU before the CPU waits for the oldest.
    const IN_FLIGHT: usize = 3;

    /// Video frames of audio the writer is fed ahead of the video.
    const AUDIO_LEAD: u64 = 12;

    /// What the hardware writer makes: H.264 or HEVC without alpha in MP4
    /// or MOV, with AAC sound. An explicit CRF goes to ffmpeg (the writer
    /// takes a bitrate).
    pub fn takes(t: &Target, s: &Settings) -> bool {
        let Some(v) = t.video else { return false };
        matches!(v.codec, VideoCodec::H264 | VideoCodec::Hevc)
            && !v.alpha
            && matches!(t.container, Container::Mp4 | Container::Mov)
            && s.crf.is_none()
            && t.audio.is_none_or(|a| a.codec == crate::output::AudioCodec::Aac)
    }

    pub fn export(
        renderer: &mut Renderer,
        doc: &Document,
        path: &Path,
        t: Target,
        clock: Clock,
        opts: ExportOptions,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<ExportReport, String> {
        let v = t.video.expect("a picture");
        let codec = match v.codec {
            VideoCodec::Hevc => ultrafast_media::Codec::Hevc,
            _ => ultrafast_media::Codec::H264,
        };
        let bitrate = match v.rate {
            Some(Rate::Bitrate(b)) => b,
            _ => hardware_bitrate(&t),
        };
        let total = clock.total;
        let prev_mode = renderer.mode();
        renderer.set_mode(Mode::Exact);
        renderer.set_sequential(true);
        let before = renderer.device().stats();
        let media0 = renderer.media_wait();
        let image0 = renderer.image_wait();
        let start = Instant::now();
        let mix = if t.audio.is_some() {
            // Only what these frames play of each source gets decoded.
            let r = ultrafast_media::audio::RATE as f64;
            let (a, b) = clock.span();
            let span = ((a * r).round() as usize, (b * r).round() as usize);
            crate::audio_mix::AudioMix::from_doc_range(doc, renderer.base_dir(), Some(span))?
        } else {
            None
        };
        let audio_bits = t.audio.and_then(|a| a.bitrate).unwrap_or(192_000) as u32;
        let mut writer = Writer::create(
            renderer.device(),
            path,
            t.width,
            t.height,
            t.fps,
            codec,
            Some(bitrate),
            mix.is_some().then_some(audio_bits),
        )
        .map_err(|e| e.to_string())?;
        // Audio for video frame `i`, appended right after it so the writer
        // can interleave.
        let rate = ultrafast_media::audio::RATE as f64;
        let audio_for = |i: u64| -> Option<Vec<f32>> {
            let mix = mix.as_ref()?;
            let a = (clock.time(i) * rate).round() as usize;
            let b = (clock.time(i + 1) * rate).round() as usize;
            Some(mix.render(a, b - a))
        };
        let mut inflight = VecDeque::new();
        let mut timing = ExportTiming::default();
        // The writer interleaves audio and video and stops taking one while
        // the other lags, so audio is kept a little ahead of the video.
        let mut audio_next = 0u64;
        let push = |writer: &mut Writer,
                    frame: &ultrafast_media::macos::MappedFrame,
                    idx: u64,
                    audio_next: &mut u64|
         -> Result<(), String> {
            let waited = Instant::now();
            loop {
                // If video has been refused a while, let audio run further
                // ahead.
                let lead = if waited.elapsed().as_millis() > 100 {
                    total
                } else {
                    AUDIO_LEAD
                };
                while mix.is_some() && *audio_next < total && *audio_next <= idx + lead && writer.audio_ready() {
                    if let Some(pcm) = audio_for(*audio_next) {
                        writer.append_audio(&pcm).map_err(|e| e.to_string())?;
                    }
                    *audio_next += 1;
                    if *audio_next == total {
                        writer.finish_audio();
                    }
                }
                if writer.video_ready() || mix.is_none() {
                    return writer.append(frame, idx).map_err(|e| e.to_string());
                }
                if waited.elapsed().as_secs() > 20 {
                    return Err(format!(
                        "the encoder stopped taking frames (video frame {idx}, audio frame {audio_next}, audio ready {}): {}",
                        writer.audio_ready(),
                        writer.failure().unwrap_or_default()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        };
        let result = (|| -> Result<(), String> {
            for i in 0..total {
                opts.check_deadline()?;
                let frame = writer.frame().map_err(|e| e.to_string())?;
                let c0 = Instant::now();
                let (sub, _) = renderer
                    .submit_frame_with(doc, clock.time(i), |o| {
                        let mut p = o.header.to_vec();
                        p.extend(o.origin);
                        // Opaque output: flatten over black.
                        p.push(1.0);
                        o.list
                            .dispatch(&o.kernels.utility("k_encode_srgb8"), &frame.texture, &[o.texture], &p);
                    })
                    .map_err(|e| e.to_string())?;
                timing.render_cpu += c0.elapsed().as_secs_f64();
                inflight.push_back((sub, frame, i));
                while inflight.len() >= IN_FLIGHT {
                    let (sub, frame, idx) = inflight.pop_front().unwrap();
                    let w0 = Instant::now();
                    timing.gpu += sub.wait().map_err(|e| e.to_string())?.gpu_ms / 1000.0;
                    timing.gpu_wait += w0.elapsed().as_secs_f64();
                    let e0 = Instant::now();
                    push(&mut writer, &frame, idx, &mut audio_next)?;
                    timing.encode += e0.elapsed().as_secs_f64();
                    progress(idx + 1, total);
                }
            }
            while let Some((sub, frame, idx)) = inflight.pop_front() {
                let w0 = Instant::now();
                timing.gpu += sub.wait().map_err(|e| e.to_string())?.gpu_ms / 1000.0;
                timing.gpu_wait += w0.elapsed().as_secs_f64();
                let e0 = Instant::now();
                push(&mut writer, &frame, idx, &mut audio_next)?;
                timing.encode += e0.elapsed().as_secs_f64();
                progress(idx + 1, total);
            }
            // The rest of the audio.
            while mix.is_some() && audio_next < total {
                if let Some(pcm) = audio_for(audio_next) {
                    writer.append_audio(&pcm).map_err(|e| e.to_string())?;
                }
                audio_next += 1;
            }
            Ok(())
        })();
        renderer.set_mode(prev_mode);
        renderer.set_sequential(false);
        result?;
        writer.finish().map_err(|e| e.to_string())?;
        let seconds = start.elapsed().as_secs_f64();
        let stats = renderer.device().stats() - before;
        timing.media_wait = renderer.media_wait() - media0;
        timing.image_wait = renderer.image_wait() - image0;
        timing.upload_bytes = stats.upload_bytes;
        Ok(ExportReport {
            frames: total,
            seconds,
            fps: total as f64 / seconds.max(1e-9),
            readbacks: stats.readbacks,
            timing,
            target: t,
        })
    }
}

/// Zero-readback export through NVENC (Linux and Windows, NVIDIA).
#[cfg(any(target_os = "linux", windows))]
mod nvenc {
    use super::*;
    use ultrafast_gpu::{Bindings, BufferBinding};
    use ultrafast_media::EncodeRate;
    use ultrafast_media::nvenc::Encoder;

    pub enum Error {
        /// No encoder could be opened (no NVIDIA driver, no NVENC, no
        /// external memory): the caller falls back.
        Unavailable(String),
        Failed(String),
    }

    /// What NVENC makes here: H.264 or HEVC without alpha, in MP4, MOV or
    /// Matroska.
    pub fn takes(t: &Target) -> bool {
        let Some(v) = t.video else { return false };
        matches!(v.codec, VideoCodec::H264 | VideoCodec::Hevc)
            && !v.alpha
            && matches!(t.container, Container::Mp4 | Container::Mov | Container::Mkv)
    }

    /// Frames rendered ahead of the encoder (one NV12 buffer each);
    /// ULTRAFAST_IN_FLIGHT overrides.
    fn in_flight() -> usize {
        std::env::var("ULTRAFAST_IN_FLIGHT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(1, 16)
    }
    /// Finished frames queued for the encoder thread.
    const QUEUED: usize = 2;
    /// The first decoding time in the transport stream (90 kHz): its clock
    /// reference runs a little ahead, and stays positive.
    const TS_START: u64 = 90_000;

    /// The quality level as NVENC's rate control: constant quality on
    /// libx264's CRF scale (see [`ffmpeg::nvenc_cq`]), capped at the
    /// level's bits per pixel, or the asked bitrate; P1, P4, P5 or P7;
    /// three B-frames and a key frame every 250 frames, as libx264 makes
    /// them; a full-resolution first pass for masters. Runs of a parallel
    /// export have no B-frames (see [`Whole::Run`]).
    fn rate(t: &Target, whole: Whole) -> EncodeRate {
        let codec = t.codec().unwrap_or(VideoCodec::H264);
        let (bitrate, quality) = match t.video.and_then(|v| v.rate) {
            Some(Rate::Bitrate(b)) => (b, None),
            Some(Rate::Crf(c)) => (hardware_bitrate(t), Some(ffmpeg::nvenc_cq(codec, c))),
            None => (hardware_bitrate(t), None),
        };
        EncodeRate {
            bitrate,
            quality,
            preset: [1, 4, 5, 7][t.quality as usize],
            multipass: t.quality == Quality::Master,
            b_frames: if whole == Whole::File { 3 } else { 0 },
            gop: ffmpeg::GOP,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn export(
        renderer: &mut Renderer,
        doc: &Document,
        path: &Path,
        t: Target,
        clock: Clock,
        opts: ExportOptions,
        progress: &mut dyn FnMut(u64, u64),
        whole: Whole,
    ) -> Result<ExportReport, Error> {
        let total = clock.total;
        let codec = match t.codec() {
            Some(VideoCodec::Hevc) => ultrafast_media::Codec::Hevc,
            _ => ultrafast_media::Codec::H264,
        };
        let in_flight = in_flight();
        let rate = rate(&t, whole);
        // Input buffers: frames on the GPU, queued, being handed over, and
        // held by the encoder until their bitstream is read.
        let slots = in_flight + QUEUED + 1 + ultrafast_media::nvenc::frames_held(&rate);
        let mut enc = Encoder::new(renderer.device(), t.width, t.height, t.fps, codec, rate, slots)
            .map_err(Error::Unavailable)?;
        let failed = |e: String| Error::Failed(e);
        // ffmpeg only muxes: the video stream is copied as NVENC wrote it,
        // in a transport stream that gives each frame its times (with
        // B-frames, decode order is not display order).
        let wav = if t.audio.is_some() {
            audio_wav(renderer.base_dir(), doc, clock.span()).map_err(failed)?
        } else {
            None
        };
        let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-f", "mpegts", "-i", "-"]
            .map(String::from)
            .to_vec();
        if let Some(wav) = &wav {
            args.extend(["-i".into(), wav.to_string_lossy().into_owned()]);
            // The mix is exactly as long as the frames (no -shortest: with
            // a raw stream copied in, it cut the audio short).
            args.extend(["-map", "0:v", "-map", "1:a"].map(String::from));
            args.extend(ffmpeg::audio_args(&t));
        }
        // Frame times on a 1/30000-style clock (what editors and stream
        // joins expect): the 90 kHz times below land on it exactly. (No
        // setts filter: given only a duration, ffmpeg 9 sets every
        // packet's PTS to its DTS, which undoes the reordering.)
        let (timescale, _) = mp4_clock(t.fps);
        args.extend(["-c:v", "copy"].map(String::from));
        // The stream says BT.709 (the encoder's VUI); newer ffmpeg doesn't
        // carry that from a copied stream to the container.
        args.extend(ffmpeg::BT709_TAGS.map(String::from));
        if t.container != Container::Mkv {
            args.extend(["-video_track_timescale".into(), timescale.to_string()]);
        }
        args.extend(ffmpeg::mux_args(&t, clock.first));
        if codec == ultrafast_media::Codec::Hevc && t.container != Container::Mkv {
            args.extend(["-tag:v", "hvc1"].map(String::from));
        }
        args.push(path.to_string_lossy().into_owned());
        let mut child = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| failed(format!("cannot start ffmpeg to mux: {e}")))?;
        let stdin = child.stdin.take().unwrap();
        let mut ts = crate::ts::TsWriter::new(
            std::io::BufWriter::with_capacity(1 << 20, stdin),
            match codec {
                ultrafast_media::Codec::H264 => crate::ts::Stream::H264,
                ultrafast_media::Codec::Hevc => crate::ts::Stream::Hevc,
            },
        );
        // Frame i is shown at i and decoded in turn, shifted by the frames
        // the encoder may reorder so decoding never comes after showing;
        // ffmpeg starts the file at the first frame's time.
        let step = 90_000.0 / t.fps;
        let reorder = enc.b_frames() as f64;
        let mut decoded = 0u64;
        let mut write = |p: ultrafast_media::nvenc::Packet| {
            let pts = ((p.frame as f64 + reorder) * step).round() as u64 + TS_START;
            let dts = (decoded as f64 * step).round() as u64 + TS_START;
            decoded += 1;
            ts.write(p.data, pts, dts, p.key).map_err(|e| format!("muxer: {e}"))
        };

        let prev_mode = renderer.mode();
        renderer.set_mode(Mode::Exact);
        renderer.set_sequential(true);
        let before = renderer.device().stats();
        let media0 = renderer.media_wait();
        let image0 = renderer.image_wait();
        let start = Instant::now();
        let kernel = renderer.kernels().utility("k_encode_nv12");
        let (pitch, rows) = (enc.pitch(), enc.height());
        let bufs: Vec<ultrafast_gpu::Buffer> = (0..slots).map(|i| enc.buffer(i).clone()).collect();
        let mut timing = ExportTiming::default();
        let encoded = std::sync::atomic::AtomicU64::new(0);
        // NVENC runs on its own thread: handing it frames and taking their
        // bitstreams overlaps rendering the next ones. This thread waits for
        // each frame's GPU work, then queues the frame's buffer for it.
        let result = std::thread::scope(|scope| -> Result<(), String> {
            let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(QUEUED);
            let (enc, write, encoded) = (&mut enc, &mut write, &encoded);
            let worker = scope.spawn(move || -> Result<f64, String> {
                let mut busy = 0.0;
                for slot in rx {
                    let e0 = Instant::now();
                    enc.encode(slot, &mut *write)?;
                    busy += e0.elapsed().as_secs_f64();
                    encoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                enc.finish(&mut *write)?;
                Ok(busy)
            });
            let mut inflight: VecDeque<(ultrafast_gpu::Submitted, usize)> = VecDeque::new();
            // False once the encoder thread has stopped on an error (which
            // its join reports).
            let hand_over = |inflight: &mut VecDeque<(ultrafast_gpu::Submitted, usize)>,
                             timing: &mut ExportTiming|
             -> Result<bool, String> {
                let (sub, slot) = inflight.pop_front().unwrap();
                let w0 = Instant::now();
                timing.gpu += sub.wait().map_err(|e| e.to_string())?.gpu_ms / 1000.0;
                timing.gpu_wait += w0.elapsed().as_secs_f64();
                let q0 = Instant::now();
                let sent = tx.send(slot).is_ok();
                timing.encode_wait += q0.elapsed().as_secs_f64();
                Ok(sent)
            };
            let rendered = (|| -> Result<(), String> {
                for i in 0..total {
                    opts.check_deadline()?;
                    while inflight.len() >= in_flight {
                        if !hand_over(&mut inflight, &mut timing)? {
                            return Ok(());
                        }
                    }
                    let slot = (i as usize) % slots;
                    let buf = &bufs[slot];
                    let c0 = Instant::now();
                    let (sub, _) = renderer
                        .submit_frame_with(doc, clock.time(i), |o| {
                            let mut p = o.header.to_vec();
                            p.extend(o.origin);
                            p.extend([1.0, pitch as f32, rows as f32]);
                            let buffers = [BufferBinding::Buffer(buf)];
                            o.list.dispatch_ex(
                                &kernel,
                                o.texture,
                                &[o.texture],
                                &p,
                                Bindings {
                                    buffers: &buffers,
                                    grid: Some((pitch / 4, rows / 2)),
                                    threadgroup: None,
                                },
                            );
                        })
                        .map_err(|e| e.to_string())?;
                    timing.render_cpu += c0.elapsed().as_secs_f64();
                    inflight.push_back((sub, slot));
                    progress(encoded.load(std::sync::atomic::Ordering::Relaxed), total);
                }
                while !inflight.is_empty() {
                    if !hand_over(&mut inflight, &mut timing)? {
                        return Ok(());
                    }
                }
                Ok(())
            })();
            drop(tx);
            let encoder = worker.join().map_err(|_| "encoder thread panicked".to_string())?;
            rendered?;
            timing.encode = encoder?;
            progress(total, total);
            Ok(())
        });
        renderer.set_mode(prev_mode);
        renderer.set_sequential(false);
        drop(write);
        // Closing the pipe ends the stream; ffmpeg then finishes the file.
        let flushed = ts.flush().map_err(|e| format!("muxer: {e}"));
        drop(ts);
        let status = child.wait().map_err(|e| failed(e.to_string()))?;
        if let Some(wav) = &wav {
            let _ = std::fs::remove_file(wav);
        }
        result.map_err(failed)?;
        if !status.success() {
            return Err(failed(format!("ffmpeg (mux) failed ({status})")));
        }
        flushed.map_err(failed)?;
        let seconds = start.elapsed().as_secs_f64();
        let stats = renderer.device().stats() - before;
        timing.media_wait = renderer.media_wait() - media0;
        timing.image_wait = renderer.image_wait() - image0;
        timing.upload_bytes = stats.upload_bytes;
        Ok(ExportReport {
            frames: total,
            seconds,
            fps: total as f64 / seconds.max(1e-9),
            readbacks: stats.readbacks,
            timing,
            target: t,
        })
    }
}

/// Exports with `workers` renderers on as many threads, each with its own
/// GPU queue on the same device, each rendering a contiguous run of frames
/// (a video's runs as streams of their own, starting on a key frame, then
/// joined without re-encoding and given the soundtrack; a sequence's
/// straight into its folder). Encoding a frame on the CPU is
/// single-threaded, so one renderer leaves the GPU idle between frames;
/// several fill it, without the context switches separate processes cost.
/// Simulations replay up to each run's first frame. A GIF (one palette
/// for the whole film) and sound alone use one renderer. Make `device`
/// with [`ultrafast_gpu::Device::with_lanes`] (`workers + 1`) so each
/// worker gets a queue of its own.
pub fn export_parallel(
    device: &ultrafast_gpu::Device,
    doc_path: &Path,
    doc: &Document,
    path: &Path,
    opts: ExportOptions,
    workers: usize,
    mut progress: impl FnMut(u64, u64),
) -> Result<ExportReport, String> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let target = opts.settings.resolve(Some(path), &doc.format)?;
    let fmt = doc.format;
    let end = opts.end.unwrap_or(fmt.duration).min(fmt.duration);
    let clock = Clock::new(target.fps, opts.start, end);
    let base = doc_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let workers = (workers as u64).clamp(1, clock.total.max(1));
    if workers == 1 || target.video.is_none() || target.container == Container::Gif {
        let mut r = Renderer::new(device.fork().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        r.set_base_dir(&base);
        return export_target(&mut r, doc, path, target, opts, progress);
    }
    let sequence = target.container.is_sequence();
    let parts_dir = path.with_extension("parts");
    if !sequence {
        std::fs::create_dir_all(&parts_dir).map_err(|e| e.to_string())?;
    }
    let budget = device.memory_bytes() / 3 / workers;
    // Forks share the device's counters: one delta counts every worker.
    let before = device.stats();
    let start = Instant::now();
    let done: Vec<Arc<AtomicU64>> = (0..workers).map(|_| Arc::new(AtomicU64::new(0))).collect();
    let mut threads = Vec::new();
    let mut parts = Vec::new();
    for w in 0..workers {
        let a = clock.first + clock.total * w / workers;
        let b = clock.first + clock.total * (w + 1) / workers;
        let part = if sequence {
            path.to_path_buf()
        } else {
            parts_dir.join(format!("part{w:02}.{}", target.container.name()))
        };
        parts.push(part.clone());
        let dev = device.fork().map_err(|e| e.to_string())?;
        let doc = doc.clone();
        let base = base.clone();
        let counter = done[w as usize].clone();
        let mut t = target;
        t.audio = None;
        let o = ExportOptions {
            start: a as f64 / clock.fps,
            end: Some(b as f64 / clock.fps),
            settings: Settings {
                audio: Some(false),
                ..opts.settings
            },
            ..opts
        };
        threads.push(std::thread::spawn(move || -> Result<ExportReport, String> {
            let mut r = Renderer::new(dev).map_err(|e| e.to_string())?;
            r.set_base_dir(&base);
            r.set_slot_budget(budget);
            export_target_as(
                &mut r,
                &doc,
                &part,
                t,
                o,
                |d, _| counter.store(d, Ordering::Relaxed),
                Whole::Run,
            )
        }));
    }
    while threads.iter().any(|t| !t.is_finished()) {
        std::thread::sleep(std::time::Duration::from_millis(250));
        progress(done.iter().map(|d| d.load(Ordering::Relaxed)).sum(), clock.total);
    }
    let mut timing = ExportTiming::default();
    for (w, t) in threads.into_iter().enumerate() {
        let rep = t
            .join()
            .map_err(|_| format!("worker {w} panicked"))?
            .map_err(|e| format!("worker {w}: {e}"))?;
        timing.add(&rep.timing);
    }
    let stats = device.stats() - before;
    let readbacks = stats.readbacks;
    timing.upload_bytes = stats.upload_bytes;
    progress(clock.total, clock.total);
    if sequence {
        write_sidecar(&base, doc, path, &target, clock, &opts.settings)?;
    } else {
        // Join the runs as they are and add the soundtrack.
        let list = parts_dir.join("parts.txt");
        let entries: String = parts
            .iter()
            .map(|p| format!("file '{}'\n", p.file_name().unwrap().to_string_lossy()))
            .collect();
        std::fs::write(&list, entries).map_err(|e| e.to_string())?;
        let wav = if target.audio.is_some() {
            audio_wav(&base, doc, clock.span())?
        } else {
            None
        };
        let mut args: Vec<String> = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ]
        .map(String::from)
        .to_vec();
        args.push(list.to_string_lossy().into_owned());
        if let Some(wav) = &wav {
            args.extend(["-i".to_string(), wav.to_string_lossy().into_owned()]);
            args.extend(["-map", "0:v", "-map", "1:a"].map(String::from));
            args.extend(ffmpeg::audio_args(&target));
        }
        args.extend(["-c:v", "copy"].map(String::from));
        if matches!(target.container, Container::Mp4 | Container::Mov) {
            args.extend(["-video_track_timescale".to_string(), mp4_clock(clock.fps).0.to_string()]);
        }
        args.extend(ffmpeg::mux_args(&target, clock.first));
        args.push(path.to_string_lossy().into_owned());
        let r = run_ffmpeg(&args, "join the runs");
        if let Some(wav) = &wav {
            let _ = std::fs::remove_file(wav);
        }
        r?;
        let _ = std::fs::remove_dir_all(&parts_dir);
    }
    let seconds = start.elapsed().as_secs_f64();
    Ok(ExportReport {
        frames: clock.total,
        seconds,
        fps: clock.total as f64 / seconds.max(1e-9),
        readbacks,
        timing,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        let c = Clock::new(30.0, 0.0, 2.0);
        assert_eq!((c.first, c.total), (0, 60));
        assert_eq!(c.span(), (0.0, 2.0));
        // Another rate plays the same seconds.
        let c = Clock::new(60.0, 0.5, 2.0);
        assert_eq!((c.first, c.total), (30, 90));
        assert_eq!(c.time(0), 0.5);
        assert_eq!(rate_arg(30.0), "30");
        assert_eq!(rate_arg(30000.0 / 1001.0), "30000/1001");
        assert_eq!(rate_arg(24000.0 / 1001.0), "24000/1001");
    }
}
