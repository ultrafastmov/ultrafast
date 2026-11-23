//! ultrafast media I/O: hardware video decode and encode with zero-copy GPU
//! interop.
//!
//! - macOS: AVFoundation / VideoToolbox, IOSurface-backed pixel buffers
//!   mapped to Metal textures through CVMetalTextureCache.
//! - Elsewhere (Linux / NVIDIA, the Vulkan backend on macOS): `ffmpeg`
//!   processes writing planar YCbCr (or BGRA for sources with alpha)
//!   straight into GPU-visible buffers; the renderer converts, rotates and
//!   linearizes them in one pass. NVDEC with CUDA <-> Vulkan external
//!   memory is the planned zero-copy path.
//!
//! [`VideoSource`] wraps a decoder on its own thread with a small bounded
//! window of decoded frames around the playhead, so seeking or editing never
//! waits behind a long queue.

// Engine code: wide kernel and pass signatures and indexed loops over
// parallel arrays are the clearest form here.
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::should_implement_trait
)]

pub mod audio;
pub mod beats;
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
pub mod ffmpeg;
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod nvdec;
#[cfg(any(target_os = "linux", windows))]
pub mod nvenc;
#[cfg(any(target_os = "linux", windows))]
mod nvenc_sys;
mod source;
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
pub mod vision;
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
#[path = "vision_tesseract.rs"]
pub mod vision;
pub mod voiceover;

pub use source::{VideoFrame, VideoSource};

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
pub use macos::Writer;

/// Video codec for export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

/// How a hardware encoder spends bits and time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodeRate {
    /// Average bits per second; with `quality`, the most it may use.
    pub bitrate: u64,
    /// Constant quality on the codec's quantizer scale (lower is better),
    /// instead of an average bitrate.
    pub quality: Option<f64>,
    /// Speed: 1 (fastest) to 7 (best), NVENC's P1..P7.
    pub preset: u8,
    /// A full-resolution first pass.
    pub multipass: bool,
    /// B-frames between reference frames, as far as the encoder has them
    /// (0: none). With them, frames come out in decode order, each saying
    /// which frame it is.
    pub b_frames: u8,
    /// Frames from one key frame to the next (0: two seconds' worth).
    pub gop: u32,
}

impl EncodeRate {
    /// An average bitrate at the default speed, without B-frames.
    pub fn bitrate(bitrate: u64) -> Self {
        EncodeRate {
            bitrate,
            quality: None,
            preset: 4,
            multipass: false,
            b_frames: 0,
            gop: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    /// Display size: upright, after `rotation`.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Seconds.
    pub duration: f64,
    /// Clockwise rotation (0, 90, 180 or 270 degrees) that turns decoded
    /// frames upright (phone video records sideways with a display matrix).
    pub rotation: u32,
}

/// How a source's decoded frames are laid out in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// A GPU texture (BGRA8) from the platform decoder.
    Texture,
    /// BGRA8 bytes in a buffer, straight alpha.
    Bgra,
    /// 8-bit YCbCr 4:2:0 in a buffer: the Y plane, then Cb and Cr at half
    /// size (rounded up), tightly packed.
    I420,
    /// 8-bit YCbCr 4:2:0 as hardware decoders write it: the Y plane, then
    /// Cb and Cr interleaved at half size, rows the frame's width apart.
    Nv12,
}

/// What decoded frames hold: layout, coded size and YCbCr meaning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameFormat {
    pub layout: Layout,
    /// Size of the decoded frames, before rotation.
    pub coded: (u32, u32),
    /// Luma weights (Kr, Kb) of the YCbCr matrix (BT.709: 0.2126, 0.0722).
    pub kr: f32,
    pub kb: f32,
    /// Full-range samples (0..255) instead of video range (16..235).
    pub full_range: bool,
}

impl FrameFormat {
    /// Bytes of one decoded frame in a buffer (0 for textures).
    pub fn frame_bytes(&self) -> usize {
        let (w, h) = (self.coded.0 as usize, self.coded.1 as usize);
        match self.layout {
            Layout::Texture => 0,
            Layout::Bgra => w * h * 4,
            Layout::I420 | Layout::Nv12 => w * h + 2 * w.div_ceil(2) * h.div_ceil(2),
        }
    }

    /// Kernel parameters for converting a frame: layout, rotation, coded
    /// size, Kr, Kb, full range.
    pub fn params(&self, rotation: u32) -> [f32; 7] {
        let layout = match self.layout {
            Layout::Texture => 0.0,
            Layout::Bgra => 1.0,
            Layout::I420 => 2.0,
            Layout::Nv12 => 3.0,
        };
        [
            layout,
            rotation as f32,
            self.coded.0 as f32,
            self.coded.1 as f32,
            self.kr,
            self.kb,
            if self.full_range { 1.0 } else { 0.0 },
        ]
    }
}

/// Reads a video file's format without decoding it.
pub fn video_info(path: &std::path::Path) -> Result<VideoInfo, MediaError> {
    #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
    {
        Ok(macos::Reader::open(path)?.info)
    }
    #[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
    {
        ffmpeg::probe(path)
    }
}

/// Upright size of a video file or URL's first video stream (its display
/// matrix applied), read with `ffprobe` on every platform.
pub fn display_size(target: &std::ffi::OsStr) -> Option<(u32, u32)> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height:stream_side_data=rotation:stream_tags=rotate")
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(target)
        .output()
        .ok()?;
    let (mut w, mut h, mut cw) = (0u32, 0u32, 0.0f64);
    for (k, v) in String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
    {
        match k {
            "width" => w = v.trim().parse().ok()?,
            "height" => h = v.trim().parse().ok()?,
            "rotation" => cw = -v.trim().parse::<f64>().unwrap_or(0.0),
            "TAG:rotate" => cw = v.trim().parse().unwrap_or(0.0),
            _ => {}
        }
    }
    let quarter = ((cw / 90.0).round() as i64).rem_euclid(2) == 1;
    (w > 0 && h > 0).then_some(if quarter { (h, w) } else { (w, h) })
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MediaError {
    #[error("cannot open {0}")]
    Open(String),
    #[error("{0}")]
    Platform(String),
}
