//! Video export.
//!
//! - macOS / Metal: every frame is rendered on the GPU straight into the
//!   hardware encoder's pixel buffers (no readback), a few frames in flight.
//! - Linux / NVIDIA: every frame is rendered on the GPU straight into NV12
//!   buffers shared with CUDA that NVENC encodes (no readback); `ffmpeg`
//!   only muxes the compressed stream and the audio.
//! - Elsewhere, or without NVENC: frames are read back and piped to
//!   `ffmpeg` (using `h264_nvenc` / `hevc_nvenc` when ffmpeg has them).
//!   Every readback is counted in the transfer stats.

#[cfg(any(all(target_os = "macos", not(feature = "vulkan")), target_os = "linux", windows))]
use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;
use ultrafast_graph::Document;
use ultrafast_media::Codec;
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
use ultrafast_media::Writer;
use ultrafast_render::{Mode, Renderer};

#[derive(Debug, Clone, Copy)]
pub struct ExportOptions {
    pub codec: Codec,
    /// Bits per second; None picks a high-quality default for the format.
    pub bitrate: Option<u64>,
    /// Range to export in seconds (default: the whole document).
    pub start: f64,
    pub end: Option<f64>,
    /// Include the document's soundtrack.
    pub audio: bool,
    /// Give up with [`DEADLINE_PASSED`] once this passes (a caller that
    /// stops waiting); checked before every frame.
    pub deadline: Option<Instant>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            bitrate: None,
            start: 0.0,
            end: None,
            audio: true,
            deadline: None,
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
    pub frames: u64,
    pub seconds: f64,
    pub fps: f64,
    pub readbacks: u64,
    pub timing: ExportTiming,
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

/// Frames encoded on the GPU before the CPU waits for the oldest.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
const IN_FLIGHT: usize = 3;

/// Video frames of audio the writer is fed ahead of the video.
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
const AUDIO_LEAD: u64 = 12;

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
pub fn export_video(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    opts: ExportOptions,
    mut progress: impl FnMut(u64, u64),
) -> Result<ExportReport, String> {
    let fmt = doc.format;
    let end = opts.end.unwrap_or(fmt.duration).min(fmt.duration);
    let first = (opts.start * fmt.fps).round() as u64;
    let last = (end * fmt.fps).round() as u64;
    let total = last.saturating_sub(first);
    let prev_mode = renderer.mode();
    renderer.set_mode(Mode::Exact);
    renderer.set_sequential(true);
    let before = renderer.device().stats();
    let media0 = renderer.media_wait();
    let image0 = renderer.image_wait();
    let start = Instant::now();
    let mix = if opts.audio {
        // Only what these frames play of each source gets decoded.
        let r = ultrafast_media::audio::RATE as f64;
        let span = (
            (fmt.time_of(first) * r).round() as usize,
            (fmt.time_of(last) * r).round() as usize,
        );
        crate::audio_mix::AudioMix::from_doc_range(doc, renderer.base_dir(), Some(span))?
    } else {
        None
    };
    let mut writer = Writer::create(
        renderer.device(),
        path,
        fmt.width,
        fmt.height,
        fmt.fps,
        opts.codec,
        opts.bitrate,
        mix.is_some(),
    )
    .map_err(|e| e.to_string())?;
    // Audio for video frame `i`, appended right after it so the writer can
    // interleave.
    let rate = ultrafast_media::audio::RATE as f64;
    let audio_for = |i: u64| -> Option<Vec<f32>> {
        let mix = mix.as_ref()?;
        let a = (fmt.time_of(first + i) * rate).round() as usize;
        let b = (fmt.time_of(first + i + 1) * rate).round() as usize;
        Some(mix.render(a, b - a))
    };
    let mut inflight = VecDeque::new();
    let mut timing = ExportTiming::default();
    // The writer interleaves audio and video and stops taking one while the
    // other lags, so audio is kept a little ahead of the video frames.
    let mut audio_next = 0u64;
    let push = |writer: &mut Writer,
                frame: &ultrafast_media::macos::MappedFrame,
                idx: u64,
                audio_next: &mut u64|
     -> Result<(), String> {
        let waited = Instant::now();
        loop {
            // If video has been refused a while, let audio run further ahead.
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
            let t = fmt.time_of(first + i);
            let c0 = Instant::now();
            let (sub, _) = renderer
                .submit_frame_with(doc, t, |o| {
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
    })
}

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
#[cfg_attr(not(any(target_os = "linux", windows)), allow(unused_mut))]
pub fn export_video(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    opts: ExportOptions,
    mut progress: impl FnMut(u64, u64),
) -> Result<ExportReport, String> {
    #[cfg(any(target_os = "linux", windows))]
    if std::env::var_os("ULTRAFAST_NVENC").is_none_or(|v| v != "0") {
        match nvenc::export(renderer, doc, path, opts, &mut progress) {
            Ok(r) => return Ok(r),
            Err(nvenc::Error::Unavailable(e)) => {
                eprintln!("ultrafast: NVENC unavailable ({e}); exporting through ffmpeg with readbacks")
            }
            Err(nvenc::Error::Failed(e)) => return Err(e),
        }
    }
    export_ffmpeg(renderer, doc, path, opts, progress)
}

/// The document's sound for frames `first..end` as a temporary WAV file
/// (None without audio).
fn audio_wav_in(base: &Path, doc: &Document, first: u64, end: f64) -> Result<Option<std::path::PathBuf>, String> {
    let rate = ultrafast_media::audio::RATE as f64;
    let a = (doc.format.time_of(first) * rate).round() as usize;
    let b = (end * rate).round() as usize;
    // Only what these frames play of each source gets decoded.
    let Some(mix) = crate::audio_mix::AudioMix::from_doc_range(doc, base, Some((a, b)))? else {
        return Ok(None);
    };
    let wav = std::env::temp_dir().join(format!("ultrafast-export-{}.wav", std::process::id()));
    let pcm = ultrafast_media::audio::Pcm {
        samples: mix.render(a, b.saturating_sub(a)),
    };
    ultrafast_media::audio::write_wav(&wav, &pcm).map_err(|e| e.to_string())?;
    Ok(Some(wav))
}

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
fn audio_wav(renderer: &Renderer, doc: &Document, first: u64, end: f64) -> Result<Option<std::path::PathBuf>, String> {
    audio_wav_in(renderer.base_dir(), doc, first, end)
}

/// Whether ffmpeg can open `encoder` here (one tiny frame, checked once per
/// encoder per process).
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
fn ffmpeg_encoder_opens(encoder: &str) -> bool {
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

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
fn export_ffmpeg(
    renderer: &mut Renderer,
    doc: &Document,
    path: &Path,
    opts: ExportOptions,
    mut progress: impl FnMut(u64, u64),
) -> Result<ExportReport, String> {
    use std::io::Write;
    let fmt = doc.format;
    let end = opts.end.unwrap_or(fmt.duration).min(fmt.duration);
    let first = (opts.start * fmt.fps).round() as u64;
    let total = ((end * fmt.fps).round() as u64).saturating_sub(first);
    let encoders = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .map_err(|e| format!("ffmpeg is required for export on this backend: {e}"))?;
    let (hw, sw) = match opts.codec {
        Codec::H264 => ("h264_nvenc", "libx264"),
        Codec::Hevc => ("hevc_nvenc", "libx265"),
    };
    // Distro ffmpeg builds list NVENC even without an NVIDIA driver, so it
    // is used only once a one-frame encode shows it opens.
    let codec = if encoders.contains(hw) && ffmpeg_encoder_opens(hw) {
        hw
    } else {
        sw
    };
    let size = format!("{}x{}", fmt.width, fmt.height);
    let rate = format!("{}", fmt.fps);
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
        &size,
        "-r",
        &rate,
        "-i",
        "-",
        "-c:v",
        codec,
        "-pix_fmt",
        "yuv420p",
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(b) = opts.bitrate {
        args.extend(["-b:v".to_string(), b.to_string()]);
    }
    // Sound: the document's audio mixed to a temporary WAV, muxed as AAC.
    let wav = if opts.audio {
        audio_wav(renderer, doc, first, end)?
    } else {
        None
    };
    if let Some(wav) = &wav {
        let at = args.iter().position(|a| a == "-c:v").unwrap();
        args.splice(
            at..at,
            [
                "-i".to_string(),
                wav.to_string_lossy().into_owned(),
                "-map".into(),
                "0:v".into(),
                "-map".into(),
                "1:a".into(),
            ],
        );
        args.extend(["-c:a", "aac", "-b:a", "192k", "-shortest"].map(String::from));
    }
    args.push(path.to_string_lossy().into_owned());
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
        for i in 0..total {
            opts.check_deadline()?;
            let c0 = Instant::now();
            let (px, _) = renderer
                .render_rgba8(doc, fmt.time_of(first + i))
                .map_err(|e| e.to_string())?;
            timing.render_cpu += c0.elapsed().as_secs_f64();
            let e0 = Instant::now();
            stdin.write_all(&px).map_err(|e| e.to_string())?;
            timing.encode += e0.elapsed().as_secs_f64();
            progress(i + 1, total);
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
        (Err(e), false) => return Err(format!("ffmpeg failed ({status}): {e}")),
        (Ok(()), false) => return Err(format!("ffmpeg failed ({status})")),
        (Ok(()), true) => {}
    }
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
    })
}

/// Zero-readback export through NVENC (Linux and Windows, NVIDIA).
#[cfg(any(target_os = "linux", windows))]
mod nvenc {
    use super::*;
    use std::io::Write;
    use ultrafast_gpu::{Bindings, BufferBinding};
    use ultrafast_media::nvenc::Encoder;

    pub enum Error {
        /// No encoder could be opened (no NVIDIA driver, no NVENC, no
        /// external memory): the caller falls back.
        Unavailable(String),
        Failed(String),
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

    pub fn export(
        renderer: &mut Renderer,
        doc: &Document,
        path: &Path,
        opts: ExportOptions,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<ExportReport, Error> {
        let fmt = doc.format;
        let end = opts.end.unwrap_or(fmt.duration).min(fmt.duration);
        let first = (opts.start * fmt.fps).round() as u64;
        let total = ((end * fmt.fps).round() as u64).saturating_sub(first);
        let bitrate = opts.bitrate.unwrap_or_else(|| {
            let bpp = match opts.codec {
                Codec::H264 => 0.12,
                Codec::Hevc => 0.08,
            };
            (fmt.width as f64 * fmt.height as f64 * fmt.fps * bpp) as u64
        });
        let in_flight = in_flight();
        // Input buffers: frames on the GPU, queued, being handed over, and
        // held by the encoder until their bitstream is read.
        let slots = in_flight + QUEUED + 1 + ultrafast_media::nvenc::DEPTH;
        let mut enc = Encoder::new(
            renderer.device(),
            fmt.width,
            fmt.height,
            fmt.fps,
            opts.codec,
            bitrate,
            slots,
        )
        .map_err(Error::Unavailable)?;
        let failed = |e: String| Error::Failed(e);
        // ffmpeg only muxes: the video stream is copied as NVENC wrote it.
        let wav = if opts.audio {
            audio_wav(renderer, doc, first, end).map_err(failed)?
        } else {
            None
        };
        let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-f"]
            .map(String::from)
            .to_vec();
        args.push(match opts.codec {
            Codec::H264 => "h264".into(),
            Codec::Hevc => "hevc".into(),
        });
        args.extend(["-framerate".into(), format!("{}", fmt.fps), "-i".into(), "-".into()]);
        if let Some(wav) = &wav {
            args.extend(["-i".into(), wav.to_string_lossy().into_owned()]);
            // The mix is exactly as long as the frames (no -shortest: with a
            // raw stream copied in, it cut the audio short).
            args.extend(["-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "192k"].map(String::from));
        }
        // Exact frame timestamps on a 1/30000-style clock (what editors and
        // stream joins expect): the raw stream's own
        // 1.2 MHz guesses left the average rate a hair off 30/1. NVENC writes
        // no B-frames, so decode order is display order.
        let (timescale, ticks) = mp4_clock(fmt.fps);
        args.extend([
            "-c:v".into(),
            "copy".into(),
            "-bsf:v".into(),
            // In the input's own time base (TB, the raw stream's 1.2 MHz):
            // ffmpeg 8.1 no longer applies setts' time_base option, which
            // left 60 fps files playing at 1200.
            format!("setts=ts=N*{ticks}/({timescale}*TB):duration={ticks}/({timescale}*TB)"),
            "-video_track_timescale".into(),
            timescale.to_string(),
            "-movflags".into(),
            "+faststart".into(),
        ]);
        if opts.codec == Codec::Hevc {
            args.extend(["-tag:v", "hvc1"].map(String::from));
        }
        args.push(path.to_string_lossy().into_owned());
        let mut child = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| failed(format!("cannot start ffmpeg to mux: {e}")))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut write = |b: &[u8]| stdin.write_all(b).map_err(|e| format!("muxer: {e}"));

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
                        .submit_frame_with(doc, fmt.time_of(first + i), |o| {
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
        drop(stdin);
        let status = child.wait().map_err(|e| failed(e.to_string()))?;
        if let Some(wav) = &wav {
            let _ = std::fs::remove_file(wav);
        }
        result.map_err(failed)?;
        if !status.success() {
            return Err(failed(format!("ffmpeg (mux) failed ({status})")));
        }
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
        })
    }
}

/// Exports with `workers` renderers on as many threads, each with its own
/// GPU queue on the same device, each rendering a contiguous run of frames
/// as its own stream (starting on a key frame); then joins the runs without
/// re-encoding and adds the soundtrack. Encoding a frame on the CPU is
/// single-threaded, so one renderer leaves the GPU idle between frames;
/// several fill it, without the context switches separate processes cost.
/// Simulations replay up to each run's first frame.
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
    let fmt = doc.format;
    let end = opts.end.unwrap_or(fmt.duration).min(fmt.duration);
    let first = (opts.start * fmt.fps).round() as u64;
    let last = (end * fmt.fps).round() as u64;
    let total = last.saturating_sub(first);
    let workers = (workers as u64).clamp(1, total.max(1));
    let parts_dir = path.with_extension("parts");
    std::fs::create_dir_all(&parts_dir).map_err(|e| e.to_string())?;
    let base = doc_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let budget = device.memory_bytes() / 3 / workers;
    let start = Instant::now();
    let done: Vec<Arc<AtomicU64>> = (0..workers).map(|_| Arc::new(AtomicU64::new(0))).collect();
    let mut threads = Vec::new();
    let mut parts = Vec::new();
    for w in 0..workers {
        let a = first + total * w / workers;
        let b = first + total * (w + 1) / workers;
        let part = parts_dir.join(format!("part{w:02}.mp4"));
        parts.push(part.clone());
        let dev = device.fork().map_err(|e| e.to_string())?;
        let doc = doc.clone();
        let base = base.clone();
        let counter = done[w as usize].clone();
        let o = ExportOptions {
            start: a as f64 / fmt.fps,
            end: Some(b as f64 / fmt.fps),
            audio: false,
            ..opts
        };
        threads.push(std::thread::spawn(move || -> Result<ExportReport, String> {
            let mut r = Renderer::new(dev).map_err(|e| e.to_string())?;
            r.set_base_dir(&base);
            r.set_slot_budget(budget);
            export_video(&mut r, &doc, &part, o, |d, _| counter.store(d, Ordering::Relaxed))
        }));
    }
    while threads.iter().any(|t| !t.is_finished()) {
        std::thread::sleep(std::time::Duration::from_millis(250));
        progress(done.iter().map(|d| d.load(Ordering::Relaxed)).sum(), total);
    }
    let mut timing = ExportTiming::default();
    let mut readbacks = 0;
    for (w, t) in threads.into_iter().enumerate() {
        let rep = t
            .join()
            .map_err(|_| format!("worker {w} panicked"))?
            .map_err(|e| format!("worker {w}: {e}"))?;
        readbacks += rep.readbacks;
        timing.render_cpu += rep.timing.render_cpu;
        timing.gpu += rep.timing.gpu;
        timing.gpu_wait += rep.timing.gpu_wait;
        timing.encode += rep.timing.encode;
        timing.encode_wait += rep.timing.encode_wait;
        timing.media_wait += rep.timing.media_wait;
        timing.image_wait += rep.timing.image_wait;
        timing.upload_bytes += rep.timing.upload_bytes;
    }
    progress(total, total);
    // Join the runs as they are and add the soundtrack.
    let list = parts_dir.join("parts.txt");
    let entries: String = parts
        .iter()
        .map(|p| format!("file '{}'\n", p.file_name().unwrap().to_string_lossy()))
        .collect();
    std::fs::write(&list, entries).map_err(|e| e.to_string())?;
    let wav = if opts.audio {
        audio_wav_in(&base, doc, first, end)?
    } else {
        None
    };
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
    ])
    .arg(&list);
    if let Some(wav) = &wav {
        cmd.arg("-i").arg(wav);
        cmd.args(["-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "192k"]);
    }
    cmd.args(["-c:v", "copy", "-video_track_timescale"])
        .arg(mp4_clock(fmt.fps).0.to_string())
        .args(["-movflags", "+faststart"])
        .arg(path);
    let status = cmd.status().map_err(|e| format!("cannot start ffmpeg to join: {e}"))?;
    if let Some(wav) = &wav {
        let _ = std::fs::remove_file(wav);
    }
    if !status.success() {
        return Err(format!("ffmpeg (join) failed ({status})"));
    }
    let _ = std::fs::remove_dir_all(&parts_dir);
    let seconds = start.elapsed().as_secs_f64();
    Ok(ExportReport {
        frames: total,
        seconds,
        fps: total as f64 / seconds.max(1e-9),
        readbacks,
        timing,
    })
}
