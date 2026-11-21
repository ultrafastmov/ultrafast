//! Image resources: decoded off the render thread, uploaded once, converted
//! to the working space once, then shared by every node that references them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::SystemTime;
use ultrafast_gpu::{Device, PixelFormat, Texture, TextureDesc};

/// What a media file was when it was read (modified time and length): a
/// different stamp later means it was rewritten.
pub type FileStamp = (SystemTime, u64);

/// The file's stamp now (None for a URL or a missing file).
pub fn file_stamp(path: &Path) -> Option<FileStamp> {
    if path.to_string_lossy().contains("://") {
        return None;
    }
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

pub use ultrafast_geom::f32_to_f16;

pub enum ImageState {
    Loading(Receiver<Result<Decoded, String>>),
    /// Decoded and uploaded as RGBA8; waiting for the working-space pass.
    Uploaded(Texture),
    Ready(Texture),
    Failed(String),
}

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The RGBA8 pixels decoded straight into GPU-visible memory instead
    /// (then `rgba` is empty): the render thread copies nothing.
    pub staged: Option<ultrafast_gpu::Buffer>,
    /// Already in its final format (LUTs: RGBA16F data); no prep pass.
    pub final_format: Option<PixelFormat>,
}

/// Parses a `.cube` 3D LUT into an RGBA16F texture of N*N x N texels: blue
/// slices side by side, red along x within a slice, green along y.
pub fn parse_cube(text: &str) -> Result<Decoded, String> {
    let mut n = 0usize;
    let mut values: Vec<[f32; 3]> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") || line.starts_with("DOMAIN_") {
            continue;
        }
        let mut it = line.split_whitespace();
        match it.next().unwrap_or("") {
            "LUT_3D_SIZE" => n = it.next().and_then(|v| v.parse().ok()).ok_or("bad LUT_3D_SIZE")?,
            "LUT_1D_SIZE" => return Err("1D LUTs are not supported (use a 3D .cube)".into()),
            _ => values.push(parse3(line.split_whitespace()).ok_or_else(|| format!("bad LUT line `{line}`"))?),
        }
    }
    if n < 2 || values.len() != n * n * n {
        return Err(format!("LUT has {} entries, expected {n}^3", values.len()));
    }
    let (w, h) = (n * n, n);
    let mut rgba = Vec::with_capacity(w * h * 8);
    // .cube order: red fastest, then green, then blue.
    for g in 0..n {
        for b in 0..n {
            for r in 0..n {
                let v = values[r + g * n + b * n * n];
                for c in [v[0], v[1], v[2], 1.0] {
                    rgba.extend(f32_to_f16(c).to_le_bytes());
                }
            }
        }
    }
    Ok(Decoded {
        width: w as u32,
        height: h as u32,
        rgba,
        staged: None,
        final_format: Some(PixelFormat::Rgba16Float),
    })
}

fn parse3<'a>(mut it: impl Iterator<Item = &'a str>) -> Option<[f32; 3]> {
    Some([
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ])
}

type Job = Box<dyn FnOnce() + Send>;

/// Runs image decodes on a fixed pool of threads (all cores but two, which
/// render and encode), in request order: a sequence's frames prefetched
/// ahead decode in parallel without oversubscribing the CPU.
fn decode_pool(job: Job) {
    static POOL: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Job>>> = std::sync::OnceLock::new();
    let tx = POOL.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        let n = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .saturating_sub(2)
            .max(2);
        for i in 0..n {
            let rx = rx.clone();
            let _ = std::thread::Builder::new()
                .name(format!("ultrafast-image-{i}"))
                .spawn(move || {
                    loop {
                        let job = match rx.lock().unwrap().recv() {
                            Ok(job) => job,
                            Err(_) => return,
                        };
                        job();
                    }
                });
        }
        std::sync::Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send(job);
}

/// A still WebP through libwebp (SIMD; animated files and other formats
/// return None and go through `image`), decoded straight into a
/// GPU-visible buffer when `device` is given.
fn decode_webp(path: &Path, device: Option<&Device>) -> Option<Decoded> {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("webp")) {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let (mut w, mut h) = (0, 0);
    // SAFETY: libwebp reads `data` and writes at most `len` bytes into the
    // destination, which nothing else uses yet.
    unsafe {
        if libwebp_sys::WebPGetInfo(data.as_ptr(), data.len(), &mut w, &mut h) == 0 || w <= 0 || h <= 0 {
            return None;
        }
        let len = w as usize * h as usize * 4;
        let decode = |dst: &mut [u8]| {
            !libwebp_sys::WebPDecodeRGBAInto(data.as_ptr(), data.len(), dst.as_mut_ptr(), len, w * 4).is_null()
        };
        let (rgba, staged) = match device {
            Some(device) => {
                let buf = device.create_buffer(len, None);
                if !decode(&mut buf.mapped()[..len]) {
                    return None;
                }
                (Vec::new(), Some(buf))
            }
            None => {
                let mut rgba = vec![0u8; len];
                if !decode(&mut rgba) {
                    return None;
                }
                (rgba, None)
            }
        };
        Some(Decoded {
            width: w as u32,
            height: h as u32,
            rgba,
            staged,
            final_format: None,
        })
    }
}

pub struct ImageCache {
    entries: HashMap<String, ImageState>,
    /// Last use (in `tick`s) of each entry, for LRU eviction.
    used: HashMap<String, u64>,
    tick: u64,
    base_dir: PathBuf,
    /// Requested ahead of need (the next frames of a sequence): decoded on
    /// their own threads meanwhile, and not waited for until asked for.
    ahead: std::collections::HashSet<String>,
    /// Decoders write straight into GPU-visible buffers on this device.
    device: Option<Device>,
    /// Each entry's file as it was when it was read.
    stamps: HashMap<String, Option<FileStamp>>,
}

/// Decoded images kept on the GPU; older ones are evicted (sequences would
/// otherwise grow without bound).
const MAX_IMAGES: usize = 96;

impl ImageCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            used: HashMap::new(),
            tick: 0,
            base_dir: PathBuf::from("."),
            ahead: Default::default(),
            device: None,
            stamps: HashMap::new(),
        }
    }

    /// Reads again the images whose files changed since they were read (a
    /// sheet rebuilt, a frame re-rendered in place): their old copies are
    /// dropped and they decode anew, as on first use. Returns how many; costs
    /// a stat each.
    pub fn drop_changed(&mut self) -> usize {
        let changed: Vec<String> = self
            .stamps
            .iter()
            .filter(|(src, was)| !src.contains("://") && file_stamp(&crate::media_path(&self.base_dir, src)) != **was)
            .map(|(src, _)| src.clone())
            .collect();
        for src in &changed {
            self.entries.remove(src);
            self.used.remove(src);
            self.ahead.remove(src);
            self.stamps.remove(src);
            self.request_now(src);
        }
        changed.len()
    }

    pub fn set_base_dir(&mut self, dir: &Path) {
        self.base_dir = dir.to_path_buf();
    }

    /// Decode into buffers `device` reads (sparing the render thread the
    /// upload copy).
    pub fn set_device(&mut self, device: &Device) {
        self.device = Some(device.clone());
    }

    /// Starts decoding `src` ahead of need (a sequence's coming frames).
    pub fn prefetch(&mut self, src: &str) {
        if !self.entries.contains_key(src) {
            self.ahead.insert(src.to_string());
            self.request_now(src);
        }
    }

    /// Starts decoding `src` if it is not known yet; an exact render waits
    /// for it.
    pub fn request(&mut self, src: &str) {
        self.ahead.remove(src);
        self.request_now(src);
    }

    fn request_now(&mut self, src: &str) {
        if self.entries.contains_key(src) {
            return;
        }
        let path = crate::media_path(&self.base_dir, src);
        self.stamps.insert(src.to_string(), file_stamp(&path));
        let (tx, rx) = channel();
        let device = self.device.clone();
        decode_pool(Box::new(move || {
            let is_cube = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube"));
            let r = if is_cube {
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))
                    .and_then(|t| parse_cube(&t).map_err(|e| format!("{}: {e}", path.display())))
            } else {
                decode_webp(&path, device.as_ref()).map(Ok).unwrap_or_else(|| {
                    image::open(&path)
                        .map(|img| {
                            let rgba = img.to_rgba8();
                            Decoded {
                                width: rgba.width(),
                                height: rgba.height(),
                                rgba: rgba.into_raw(),
                                staged: None,
                                final_format: None,
                            }
                        })
                        .map_err(|e| format!("{}: {e}", path.display()))
                })
            };
            let _ = tx.send(r);
        }));
        self.entries.insert(src.to_string(), ImageState::Loading(rx));
    }

    /// Advances loading images. Newly decoded images are uploaded (counted as
    /// uploads); the returned textures still need the working-space pass,
    /// which the caller encodes and then reports with `mark_ready`.
    pub fn poll(&mut self, device: &Device, block: bool) -> Vec<(String, Texture)> {
        let mut uploaded = Vec::new();
        for (src, st) in self.entries.iter_mut() {
            if let ImageState::Loading(rx) = st {
                let r = if block && !self.ahead.contains(src) {
                    rx.recv().ok()
                } else {
                    rx.try_recv().ok()
                };
                match r {
                    None => {}
                    Some(Ok(d)) => {
                        let desc = TextureDesc {
                            width: d.width,
                            height: d.height,
                            format: d.final_format.unwrap_or(PixelFormat::Rgba8Unorm),
                            cpu_readable: false,
                        };
                        let tex = match &d.staged {
                            Some(buf) => device.upload_texture_from(desc, buf),
                            None => device.upload_texture(desc, &d.rgba),
                        };
                        if d.final_format.is_some() {
                            *st = ImageState::Ready(tex);
                        } else {
                            uploaded.push((src.clone(), tex.clone()));
                            *st = ImageState::Uploaded(tex);
                        }
                    }
                    Some(Err(e)) => *st = ImageState::Failed(e),
                }
            }
        }
        uploaded
    }

    pub fn mark_ready(&mut self, src: &str, tex: Texture) {
        self.entries.insert(src.to_string(), ImageState::Ready(tex));
    }

    pub fn get(&mut self, src: &str) -> Option<&ImageState> {
        self.used.insert(src.to_string(), self.tick);
        self.entries.get(src)
    }

    /// Advances the LRU clock and evicts the least recently used ready
    /// images beyond the cap (never ones used in the last few frames, which
    /// in-flight GPU work may still read).
    pub fn end_frame(&mut self) {
        self.tick += 1;
        // Frames fetched ahead that were never asked for (a seek) become
        // ordinary entries the LRU may drop.
        if self.ahead.len() > 64 {
            self.ahead.clear();
        }
        if self.entries.len() <= MAX_IMAGES {
            return;
        }
        let mut ready: Vec<(u64, String)> = self
            .entries
            .iter()
            .filter(|(_, s)| matches!(s, ImageState::Ready(_) | ImageState::Failed(_)))
            .filter(|(k, _)| !self.ahead.contains(*k))
            .map(|(k, _)| (self.used.get(k).copied().unwrap_or(0), k.clone()))
            .filter(|(u, _)| *u + 4 < self.tick)
            .collect();
        ready.sort();
        for (_, k) in ready.into_iter().take(self.entries.len() - MAX_IMAGES) {
            self.entries.remove(&k);
            self.used.remove(&k);
            self.stamps.remove(&k);
        }
    }

    /// Whether `src` is loading, loaded or failed (not dropped or never asked for).
    pub fn is_known(&self, src: &str) -> bool {
        self.entries.contains_key(src)
    }

    pub fn ready(&self, src: &str) -> Option<&Texture> {
        match self.entries.get(src) {
            Some(ImageState::Ready(t)) => Some(t),
            _ => None,
        }
    }

    /// GPU bytes of the images held.
    pub fn bytes(&self) -> u64 {
        self.entries
            .values()
            .filter_map(|s| match s {
                ImageState::Ready(t) | ImageState::Uploaded(t) => Some(t.desc()),
                _ => None,
            })
            .map(|d| d.width as u64 * d.height as u64 * d.format.bytes_per_pixel() as u64)
            .sum()
    }

    pub fn loading(&self) -> usize {
        self.entries
            .values()
            .filter(|s| matches!(s, ImageState::Loading(_) | ImageState::Uploaded(_)))
            .count()
    }
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_conversion() {
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(0.5), 0x3800);
        assert_eq!(f32_to_f16(0.0), 0);
        assert_eq!(f32_to_f16(-2.0), 0xc000);
    }

    #[test]
    fn identity_cube_parses() {
        let mut s = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    s += &format!("{r} {g} {b}\n");
                }
            }
        }
        let d = parse_cube(&s).unwrap();
        assert_eq!((d.width, d.height), (4, 2));
    }
}
