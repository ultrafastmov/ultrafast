//! Threaded video source with a small bounded decode window around the
//! playhead. Playback reads ahead a few frames; a seek (or an edit that moves
//! the source time) discards the window instead of draining it.

use crate::{FrameFormat, MediaError, VideoInfo};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
use ultrafast_gpu::Buffer;
use ultrafast_gpu::Device;
#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
use ultrafast_gpu::Texture;

/// Frames decoded ahead of the playhead.
#[allow(dead_code)]
const WINDOW: usize = 4;
/// Jumps further than this ahead are seeks, not reads.
#[allow(dead_code)]
const MAX_READ_AHEAD_S: f64 = 1.0;

/// A decoded frame: a GPU texture from the platform decoder, or the
/// decoder's bytes in a GPU-visible buffer (laid out as the source's
/// [`FrameFormat`] says), sRGB-encoded either way.
pub struct VideoFrame {
    pub pts: f64,
    #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
    mapped: crate::macos::MappedFrame,
    /// The decoded picture in memory the GPU reads (read there straight
    /// from ffmpeg, or copied there on the GPU by NVDEC); back to the
    /// source's pool, if it has one, when the frame is dropped.
    #[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
    data: Option<Buffer>,
    #[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
    pool: Option<Arc<Mutex<Vec<Buffer>>>>,
}

impl VideoFrame {
    #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
    pub fn texture(&self) -> &Texture {
        &self.mapped.texture
    }

    #[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
    pub fn buffer(&self) -> &Buffer {
        self.data.as_ref().unwrap()
    }
}

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
impl Drop for VideoFrame {
    /// Nothing reads the frame any more (the renderer keeps frames until
    /// the GPU work reading them completes): its buffer takes the next one.
    fn drop(&mut self) {
        if let (Some(b), Some(pool)) = (self.data.take(), &self.pool) {
            let mut pool = pool.lock().unwrap();
            if pool.len() < POOL {
                pool.push(b);
            }
        }
    }
}

/// Frame buffers a source keeps for reuse: its window plus frames the
/// renderer still holds for in-flight GPU work.
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
const POOL: usize = 12;

struct State {
    target: f64,
    frames: VecDeque<Arc<VideoFrame>>,
    eof: bool,
    error: Option<String>,
    closed: bool,
}

pub struct VideoSource {
    shared: Arc<(Mutex<State>, Condvar)>,
    /// The decoder thread, joined on drop: one still decoding (and
    /// touching the GPU) while the process exits crashed the NVIDIA driver.
    worker: Option<std::thread::JoinHandle<()>>,
    pub info: VideoInfo,
    /// How decoded frames are laid out (convert with `format.params`).
    pub format: FrameFormat,
    pub path: PathBuf,
    /// Seconds callers spent blocked waiting for frames.
    waited: std::sync::atomic::AtomicU64,
}

impl VideoSource {
    pub fn open(device: &Device, path: &Path) -> Result<Self, MediaError> {
        #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
        {
            let reader = crate::macos::Reader::open(path)?;
            let interop = crate::macos::Interop::new(device)?;
            let info = reader.info;
            let format = reader.format;
            let shared = Arc::new((
                Mutex::new(State {
                    target: 0.0,
                    frames: VecDeque::new(),
                    eof: false,
                    error: None,
                    closed: false,
                }),
                Condvar::new(),
            ));
            let worker = shared.clone();
            let reader = crate::macos::SendBox(reader);
            let handle = std::thread::Builder::new()
                .name(format!(
                    "ultrafast-decode:{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ))
                .spawn(move || decode_loop(reader.0, interop, worker))
                .map_err(|e| MediaError::Platform(e.to_string()))?;
            Ok(Self {
                shared,
                worker: Some(handle),
                info,
                format,
                path: path.to_path_buf(),
                waited: Default::default(),
            })
        }
        #[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
        {
            let reader = Decoder::open(device, path)?;
            let (info, format) = reader.formats();
            let shared = Arc::new((
                Mutex::new(State {
                    target: 0.0,
                    frames: VecDeque::new(),
                    eof: false,
                    error: None,
                    closed: false,
                }),
                Condvar::new(),
            ));
            let worker = shared.clone();
            let device = device.clone();
            let handle = std::thread::Builder::new()
                .name(format!(
                    "ultrafast-decode:{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ))
                .spawn(move || decode_loop(reader, device, worker))
                .map_err(|e| MediaError::Platform(e.to_string()))?;
            Ok(Self {
                shared,
                worker: Some(handle),
                info,
                format,
                path: path.to_path_buf(),
                waited: Default::default(),
            })
        }
    }

    /// The frame showing at `t` seconds of the source. With `wait`, blocks
    /// until that exact frame is decoded; otherwise returns the nearest
    /// decoded frame (or None before the first one arrives).
    pub fn frame_at(&self, t: f64, wait: bool) -> Result<Option<Arc<VideoFrame>>, MediaError> {
        let t = t.clamp(0.0, (self.info.duration - 1e-6).max(0.0));
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock().unwrap();
        if st.target != t {
            st.target = t;
            cv.notify_all();
        }
        let t0 = std::time::Instant::now();
        let mut blocked = false;
        let r = loop {
            if let Some(e) = &st.error {
                break Err(MediaError::Platform(e.clone()));
            }
            let covering = covering(&st.frames, t, st.eof);
            if covering.is_some() || !wait {
                break Ok(covering.or_else(|| nearest(&st.frames, t)));
            }
            blocked = true;
            st = cv.wait_timeout(st, Duration::from_millis(50)).unwrap().0;
        };
        if blocked {
            self.waited
                .fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        r
    }

    /// Seconds callers have spent blocked in [`frame_at`](Self::frame_at)
    /// waiting for the decoder.
    pub fn waited(&self) -> f64 {
        self.waited.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
    }
}

impl Drop for VideoSource {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap().closed = true;
        cv.notify_all();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

/// The frame whose display interval contains `t`, if decoded.
fn covering(frames: &VecDeque<Arc<VideoFrame>>, t: f64, eof: bool) -> Option<Arc<VideoFrame>> {
    let eps = 1e-4;
    for (i, f) in frames.iter().enumerate() {
        if f.pts > t + eps {
            return None;
        }
        match frames.get(i + 1) {
            Some(next) if next.pts <= t + eps => continue,
            Some(_) => return Some(f.clone()),
            None => return if eof { Some(f.clone()) } else { None },
        }
    }
    None
}

fn nearest(frames: &VecDeque<Arc<VideoFrame>>, t: f64) -> Option<Arc<VideoFrame>> {
    frames
        .iter()
        .min_by(|a, b| (a.pts - t).abs().partial_cmp(&(b.pts - t).abs()).unwrap())
        .cloned()
}

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn decode_loop(mut reader: crate::macos::Reader, interop: crate::macos::Interop, shared: Arc<(Mutex<State>, Condvar)>) {
    let (lock, cv) = &*shared;
    let mut positioned = false;
    // How far before the target the last seek landed when it hit the end
    // of the file with nothing decoded (a seek into the final GOP).
    let mut backoff = 0.0f64;
    loop {
        let target;
        {
            let mut st = lock.lock().unwrap();
            if st.closed {
                return;
            }
            target = st.target;
            let eps = 1e-4;
            let behind = st.frames.front().is_some_and(|f| f.pts > target + eps);
            let far_ahead = st.frames.back().is_some_and(|f| f.pts + MAX_READ_AHEAD_S < target)
                || (st.eof && st.frames.back().is_some_and(|f| f.pts < target - 1.0));
            if !positioned || behind || far_ahead {
                // Seek: drop the window and restart at the target.
                st.frames.clear();
                st.eof = false;
                drop(st);
                backoff = 0.0;
                if let Err(e) = reader.seek(target) {
                    lock.lock().unwrap().error = Some(e.to_string());
                    cv.notify_all();
                    return;
                }
                positioned = true;
                continue;
            }
            // Drop frames fully before the playhead (keep the covering one).
            while st.frames.len() >= 2 && st.frames[1].pts <= target + eps {
                st.frames.pop_front();
            }
            let ahead = st.frames.iter().filter(|f| f.pts > target + eps).count();
            if st.eof || ahead >= WINDOW {
                let _ = cv.wait_timeout(st, Duration::from_millis(20)).unwrap();
                continue;
            }
        }
        // Decode one frame without holding the lock.
        match reader.next() {
            Ok(Some(d)) => match interop.map(d.buffer.0) {
                Ok(mapped) => {
                    let mut st = lock.lock().unwrap();
                    st.frames.push_back(Arc::new(VideoFrame { pts: d.pts, mapped }));
                    cv.notify_all();
                }
                Err(e) => {
                    lock.lock().unwrap().error = Some(e.to_string());
                    cv.notify_all();
                    return;
                }
            },
            Ok(None) => {
                let mut st = lock.lock().unwrap();
                if st.frames.is_empty() && backoff < 8.0 && target > 0.0 {
                    // A seek near the end can land past the last decodable
                    // frame; back off and decode forward so the final frame
                    // still arrives instead of waiting forever on nothing.
                    backoff = if backoff == 0.0 { 0.5 } else { backoff * 2.0 };
                    drop(st);
                    if let Err(e) = reader.seek((target - backoff).max(0.0)) {
                        lock.lock().unwrap().error = Some(e.to_string());
                        cv.notify_all();
                        return;
                    }
                    continue;
                }
                st.eof = true;
                cv.notify_all();
            }
            Err(e) => {
                lock.lock().unwrap().error = Some(e.to_string());
                cv.notify_all();
                return;
            }
        }
    }
}

/// A source's decoder: NVDEC where it takes the stream (Linux, H.264 in
/// MP4, 8-bit 4:2:0), else an ffmpeg process.
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
enum Decoder {
    Ffmpeg {
        reader: crate::ffmpeg::Reader,
        pool: Arc<Mutex<Vec<Buffer>>>,
    },
    #[cfg(target_os = "linux")]
    Nvdec(crate::nvdec::Reader),
}

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
impl Decoder {
    fn open(device: &Device, path: &Path) -> Result<Self, MediaError> {
        let stream = crate::ffmpeg::stream(path)?;
        #[cfg(target_os = "linux")]
        match crate::nvdec::Reader::open(path, device, &stream) {
            Ok(r) => return Ok(Decoder::Nvdec(r)),
            Err(e) => {
                if std::env::var_os("ULTRAFAST_DEBUG_MEDIA").is_some() {
                    eprintln!("ultrafast: {} decodes with ffmpeg ({e})", path.display());
                }
            }
        }
        let _ = device;
        Ok(Decoder::Ffmpeg {
            reader: crate::ffmpeg::Reader::with_stream(path, stream),
            pool: Arc::default(),
        })
    }

    fn formats(&self) -> (VideoInfo, FrameFormat) {
        match self {
            Decoder::Ffmpeg { reader, .. } => (reader.info, reader.format),
            #[cfg(target_os = "linux")]
            Decoder::Nvdec(r) => (r.info, r.format),
        }
    }

    fn seek(&mut self, t: f64) -> Result<(), MediaError> {
        match self {
            Decoder::Ffmpeg { reader, .. } => reader.seek(t),
            #[cfg(target_os = "linux")]
            Decoder::Nvdec(r) => r.seek(t),
        }
    }

    /// The next frame, or None at the end.
    fn next(&mut self, device: &Device) -> Result<Option<VideoFrame>, MediaError> {
        match self {
            Decoder::Ffmpeg { reader, pool } => {
                // Whole 32-bit words: kernels read the bytes as uints.
                let bytes = reader.format.frame_bytes();
                let buf = pool
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| device.create_buffer(bytes.div_ceil(4) * 4, None));
                // SAFETY: the buffer is new or back from a dropped frame, which
                // nothing (the GPU included) reads any more.
                let read = reader.next_into(unsafe { buf.mapped() })?;
                Ok(read.map(|pts| {
                    device.note_upload(bytes);
                    VideoFrame {
                        pts,
                        data: Some(buf),
                        pool: Some(pool.clone()),
                    }
                }))
            }
            #[cfg(target_os = "linux")]
            Decoder::Nvdec(r) => Ok(r.next()?.map(|(pts, buf)| VideoFrame {
                pts,
                data: Some(buf),
                pool: None,
            })),
        }
    }
}

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
fn decode_loop(mut reader: Decoder, device: Device, shared: Arc<(Mutex<State>, Condvar)>) {
    let (lock, cv) = &*shared;
    let mut positioned = false;
    // How far before the target the last seek landed when it hit the end
    // of the file with nothing decoded (a seek into the final GOP).
    let mut backoff = 0.0f64;
    loop {
        let target;
        {
            let mut st = lock.lock().unwrap();
            if st.closed {
                return;
            }
            target = st.target;
            let eps = 1e-4;
            let behind = st.frames.front().is_some_and(|f| f.pts > target + eps);
            let far_ahead = st.frames.back().is_some_and(|f| f.pts + MAX_READ_AHEAD_S < target)
                || (st.eof && st.frames.back().is_some_and(|f| f.pts < target - 1.0));
            if !positioned || behind || far_ahead {
                st.frames.clear();
                st.eof = false;
                drop(st);
                backoff = 0.0;
                if let Err(e) = reader.seek(target) {
                    lock.lock().unwrap().error = Some(e.to_string());
                    cv.notify_all();
                    return;
                }
                positioned = true;
                continue;
            }
            while st.frames.len() >= 2 && st.frames[1].pts <= target + eps {
                st.frames.pop_front();
            }
            let ahead = st.frames.iter().filter(|f| f.pts > target + eps).count();
            if st.eof || ahead >= WINDOW {
                let _ = cv.wait_timeout(st, Duration::from_millis(20)).unwrap();
                continue;
            }
        }
        match reader.next(&device) {
            Ok(Some(frame)) => {
                let mut st = lock.lock().unwrap();
                st.frames.push_back(Arc::new(frame));
                cv.notify_all();
            }
            Ok(None) => {
                let mut st = lock.lock().unwrap();
                if st.frames.is_empty() && backoff < 8.0 && target > 0.0 {
                    // A seek near the end can land past the last decodable
                    // frame; back off and decode forward so the final frame
                    // still arrives instead of waiting forever on nothing.
                    backoff = if backoff == 0.0 { 0.5 } else { backoff * 2.0 };
                    drop(st);
                    if let Err(e) = reader.seek((target - backoff).max(0.0)) {
                        lock.lock().unwrap().error = Some(e.to_string());
                        cv.notify_all();
                        return;
                    }
                    continue;
                }
                st.eof = true;
                cv.notify_all();
            }
            Err(e) => {
                lock.lock().unwrap().error = Some(e.to_string());
                cv.notify_all();
                return;
            }
        }
    }
}
