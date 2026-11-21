//! Portable media I/O through `ffmpeg` / `ffprobe` processes, for platforms
//! without a native path yet (Linux / NVIDIA, and the Vulkan backend on
//! macOS). Frames arrive as planar YCbCr (BGRA for sources with alpha or
//! other layouts) read straight into GPU-visible buffers (counted as
//! uploads); decoders restart at the target on seeks.

use crate::audio::{CHANNELS, Pcm, RATE};
use crate::{FrameFormat, Layout, MediaError, VideoInfo};
use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

fn run(cmd: &mut Command) -> Result<String, MediaError> {
    let out = cmd
        .output()
        .map_err(|e| MediaError::Platform(format!("{}: {e} (install ffmpeg)", cmd.get_program().to_string_lossy())))?;
    if !out.status.success() {
        return Err(MediaError::Platform(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fields(s: &str) -> impl Iterator<Item = (&str, &str)> {
    s.lines().filter_map(|l| l.split_once('='))
}

/// The first video stream as the decoder sees it.
#[derive(Debug, Clone)]
pub struct Stream {
    pub info: VideoInfo,
    /// Coded size (before rotation).
    pub coded: (u32, u32),
    /// The frame rate as ffmpeg spells it (`30000/1001`).
    pub rate: String,
    pub pix_fmt: String,
    /// `color_space`, `color_transfer`, `color_primaries`, `color_range`.
    pub matrix: String,
    pub transfer: String,
    pub primaries: String,
    pub range: String,
}

impl Stream {
    /// HLG or PQ: tone mapped to SDR BT.709 on decode.
    pub fn hdr(&self) -> bool {
        matches!(self.transfer.as_str(), "arib-std-b67" | "smpte2084")
    }

    /// The YCbCr matrix as ffmpeg's `scale` filter names it: the tagged
    /// one, else BT.709 for HD sizes and BT.601 below (what players assume
    /// for untagged video).
    fn matrix_name(&self) -> &'static str {
        match self.matrix.as_str() {
            "bt709" => "bt709",
            "bt470bg" | "smpte170m" | "bt601" | "fcc" => "bt601",
            "bt2020nc" | "bt2020c" => "bt2020",
            "smpte240m" => "smpte240m",
            _ if self.coded.0.max(self.coded.1) >= 1280 || self.coded.0.min(self.coded.1) >= 720 => "bt709",
            _ => "bt601",
        }
    }

    /// The YCbCr matrix's luma weights (Kr, Kb).
    pub fn weights(&self) -> (f32, f32) {
        match self.matrix_name() {
            "bt709" => (0.2126, 0.0722),
            "bt2020" => (0.2627, 0.0593),
            "smpte240m" => (0.212, 0.087),
            _ => (0.299, 0.114),
        }
    }

    pub fn full_range(&self) -> bool {
        match self.range.as_str() {
            "pc" => true,
            "tv" => false,
            _ => self.pix_fmt.starts_with("yuvj"),
        }
    }
}

fn rate(v: &str) -> Option<f64> {
    let (a, b) = v.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    (b > 0.0 && a > 0.0).then(|| a / b)
}

/// Size, orientation, frame rate, duration, pixel format and color of the
/// first video stream.
pub fn stream(path: &Path) -> Result<Stream, MediaError> {
    let out = run(Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(
            "stream=width,height,avg_frame_rate,r_frame_rate,pix_fmt,color_space,color_transfer,\
             color_primaries,color_range:stream_side_data=rotation:stream_tags=rotate:format=duration",
        )
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path))
    .map_err(|e| MediaError::Open(format!("{}: {e}", path.display())))?;
    let mut s = Stream {
        info: VideoInfo {
            width: 0,
            height: 0,
            fps: 0.0,
            duration: 0.0,
            rotation: 0,
        },
        coded: (0, 0),
        rate: String::new(),
        pix_fmt: String::new(),
        matrix: String::new(),
        transfer: String::new(),
        primaries: String::new(),
        range: String::new(),
    };
    let mut r_rate = None;
    // Counterclockwise display-matrix rotation, or the legacy clockwise tag.
    let mut ccw: Option<f64> = None;
    let mut tag_cw: Option<f64> = None;
    for (k, v) in fields(&out) {
        match k {
            "width" => s.coded.0 = v.parse().unwrap_or(0),
            "height" => s.coded.1 = v.parse().unwrap_or(0),
            "avg_frame_rate" => {
                if let Some(r) = rate(v) {
                    s.info.fps = r;
                    s.rate = v.to_string();
                }
            }
            "r_frame_rate" => r_rate = rate(v).map(|r| (r, v.to_string())),
            "pix_fmt" => s.pix_fmt = v.to_string(),
            "color_space" => s.matrix = v.to_string(),
            "color_transfer" => s.transfer = v.to_string(),
            "color_primaries" => s.primaries = v.to_string(),
            "color_range" => s.range = v.to_string(),
            "rotation" => ccw = v.parse().ok(),
            "TAG:rotate" => tag_cw = v.parse().ok(),
            "duration" => s.info.duration = v.parse().unwrap_or(0.0),
            _ => {}
        }
    }
    if s.info.fps == 0.0 {
        (s.info.fps, s.rate) = r_rate.unwrap_or((30.0, "30".into()));
    }
    if s.coded.0 == 0 || s.coded.1 == 0 {
        return Err(MediaError::Open(format!("{}: no video stream", path.display())));
    }
    let cw = ccw.map(|r| -r).or(tag_cw).unwrap_or(0.0);
    s.info.rotation = (((cw / 90.0).round() as i64).rem_euclid(4) * 90) as u32;
    (s.info.width, s.info.height) = if s.info.rotation % 180 == 90 {
        (s.coded.1, s.coded.0)
    } else {
        s.coded
    };
    Ok(s)
}

/// Size, frame rate and duration of the first video stream.
pub fn probe(path: &Path) -> Result<VideoInfo, MediaError> {
    stream(path).map(|s| s.info)
}

/// Whether this ffmpeg has `zscale` (zimg), which HDR tone mapping needs.
fn has_zscale() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| {
        run(Command::new("ffmpeg").args(["-hide_banner", "-filters"]))
            .is_ok_and(|out| out.lines().any(|l| l.split_whitespace().nth(1) == Some("zscale")))
    })
}

/// Sequential frames from `start` on, written into caller-provided memory
/// (a GPU-visible buffer): planar YCbCr 4:2:0 as decoded when the source
/// is 8-bit 4:2:0, tone mapped to it from HDR, else BGRA converted with
/// the source's own matrix and range. Frames come out as coded (the
/// renderer turns them upright) and at a constant rate: variable-rate
/// sources repeat or drop frames to the timeline, so frame `n` shows at
/// `start + n / fps`.
pub struct Reader {
    path: std::path::PathBuf,
    pub info: VideoInfo,
    pub format: FrameFormat,
    stream: Stream,
    child: Option<(Child, ChildStdout)>,
    /// Index of the next frame, counted from `start`.
    next: u64,
    start: f64,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        Ok(Self::with_stream(path, stream(path)?))
    }

    /// A reader for a stream already probed.
    pub fn with_stream(path: &Path, stream: Stream) -> Self {
        let (kr, kb) = stream.weights();
        let hdr = stream.hdr() && has_zscale();
        if stream.hdr() && !hdr {
            eprintln!(
                "ultrafast: {} is HDR but this ffmpeg has no zscale; colors will look flat",
                path.display()
            );
        }
        let planar = hdr || matches!(stream.pix_fmt.as_str(), "yuv420p" | "yuvj420p");
        let format = if hdr {
            // Tone mapped to BT.709 video range.
            FrameFormat {
                layout: Layout::I420,
                coded: stream.coded,
                kr: 0.2126,
                kb: 0.0722,
                full_range: false,
            }
        } else {
            FrameFormat {
                layout: if planar { Layout::I420 } else { Layout::Bgra },
                coded: stream.coded,
                kr,
                kb,
                full_range: stream.full_range(),
            }
        };
        Self {
            path: path.to_path_buf(),
            info: stream.info,
            format,
            stream,
            child: None,
            next: 0,
            start: 0.0,
        }
    }

    /// The `-vf` chain and output pixel format.
    fn filters(&self) -> (Option<String>, &'static str) {
        let s = &self.stream;
        match self.format.layout {
            Layout::I420 if s.hdr() => {
                // Linear light, fixed 1000-nit peak (no per-frame pumping),
                // mobius curve.
                let primaries = if matches!(s.primaries.as_str(), "bt2020" | "bt709" | "smpte432") {
                    s.primaries.as_str()
                } else {
                    "bt2020"
                };
                let matrix = if matches!(s.matrix.as_str(), "bt2020nc" | "bt2020c" | "bt709") {
                    s.matrix.as_str()
                } else {
                    "bt2020nc"
                };
                let range = if s.range == "pc" { "full" } else { "limited" };
                (
                    Some(format!(
                        "zscale=tin={}:pin={primaries}:min={matrix}:rin={range}:t=linear:npl=100,\
                         format=gbrpf32le,zscale=p=bt709,tonemap=mobius:param=0.3:desat=2:peak=10,\
                         zscale=t=bt709:m=bt709:r=limited:dither=error_diffusion,format=yuv420p",
                        s.transfer
                    )),
                    "yuv420p",
                )
            }
            Layout::I420 => (None, if s.pix_fmt == "yuvj420p" { "yuvj420p" } else { "yuv420p" }),
            _ => (
                Some(format!(
                    "scale=in_color_matrix={}:in_range={},format=bgra",
                    s.matrix_name(),
                    if self.format.full_range { "pc" } else { "tv" }
                )),
                "bgra",
            ),
        }
    }

    /// Restarts decoding at `t` seconds (the frame showing at `t`).
    pub fn seek(&mut self, t: f64) -> Result<(), MediaError> {
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let frame = (t * self.info.fps + 1e-6).floor().max(0.0);
        self.start = frame / self.info.fps;
        self.next = 0;
        let (vf, pix_fmt) = self.filters();
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-noautorotate", "-ss"])
            .arg(format!("{:.6}", self.start))
            .arg("-i")
            .arg(&self.path)
            .args(["-map", "0:v:0"]);
        if let Some(vf) = vf {
            cmd.arg("-vf").arg(vf);
        }
        let mut child = cmd
            .args([
                "-fps_mode",
                "cfr",
                "-r",
                &self.stream.rate,
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt,
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| MediaError::Platform(format!("ffmpeg: {e} (install ffmpeg)")))?;
        let out = child.stdout.take().unwrap();
        #[cfg(target_os = "linux")]
        {
            // Frames are megabytes: a 1 MiB pipe (the default limit) takes
            // them in a few reads instead of dozens of 64 KiB ones.
            use std::os::fd::AsRawFd;
            unsafe extern "C" {
                fn fcntl(fd: i32, cmd: i32, ...) -> i32;
            }
            const F_SETPIPE_SZ: i32 = 1031;
            unsafe { fcntl(out.as_raw_fd(), F_SETPIPE_SZ, 1i32 << 20) };
        }
        self.child = Some((child, out));
        Ok(())
    }

    /// Reads the next frame into `dst` (at least `format.frame_bytes()`
    /// long) and returns its presentation time, or None at the end.
    pub fn next_into(&mut self, dst: &mut [u8]) -> Result<Option<f64>, MediaError> {
        if self.child.is_none() {
            self.seek(0.0)?;
        }
        let n = self.format.frame_bytes();
        let (_, out) = self.child.as_mut().unwrap();
        let mut got = 0;
        while got < n {
            match out.read(&mut dst[got..n]) {
                Ok(0) => return Ok(None),
                Ok(k) => got += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(MediaError::Platform(e.to_string())),
            }
        }
        let pts = self.start + self.next as f64 / self.info.fps;
        self.next += 1;
        Ok(Some(pts))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// The first audio track at 48 kHz stereo float, or None without one.
pub fn decode_audio(path: &Path) -> Result<Option<Pcm>, MediaError> {
    let streams = run(Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index"])
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path))?;
    if streams.trim().is_empty() {
        return Ok(None);
    }
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-map", "0:a:0", "-f", "f32le", "-ac"])
        .arg(CHANNELS.to_string())
        .arg("-ar")
        .arg(RATE.to_string())
        .arg("-")
        .output()
        .map_err(|e| MediaError::Platform(format!("ffmpeg: {e} (install ffmpeg)")))?;
    if !out.status.success() {
        return Err(MediaError::Platform(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    let samples = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(Some(Pcm { samples }))
}

pub use crate::audio::write_wav;
