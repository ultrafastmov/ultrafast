//! macOS media: AVFoundation + VideoToolbox hardware decode/encode, with
//! frames shared with Metal through IOSurface-backed CVPixelBuffers and a
//! CVMetalTextureCache. Decoded frames are never copied by the CPU, and
//! encoded frames are written by a GPU kernel directly into the encoder's
//! pixel buffers.

use crate::{Codec, MediaError, VideoInfo};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderOutput, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVAssetWriter,
    AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVAssetWriterStatus, AVFileTypeMPEG4,
    AVFileTypeQuickTimeMovie, AVMediaTypeVideo, AVURLAsset, AVVideoAverageBitRateKey, AVVideoCodecKey,
    AVVideoCodecTypeH264, AVVideoCodecTypeHEVC, AVVideoColorPrimaries_ITU_R_709_2, AVVideoColorPrimariesKey,
    AVVideoColorPropertiesKey, AVVideoCompressionPropertiesKey, AVVideoHeightKey, AVVideoTransferFunction_ITU_R_709_2,
    AVVideoTransferFunctionKey, AVVideoWidthKey, AVVideoYCbCrMatrix_ITU_R_709_2, AVVideoYCbCrMatrixKey,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{
    CMAudioFormatDescriptionCreate, CMAudioSampleBufferCreateReadyWithPacketDescriptions, CMBlockBuffer,
    CMFormatDescription, CMSampleBuffer, CMTime, CMTimeRange,
};
use objc2_core_video::{
    CVImageBuffer, CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferPool, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use objc2_metal::MTLPixelFormat;
use std::path::Path;
use std::ptr::NonNull;
use ultrafast_gpu::{Device, PixelFormat, Texture};

/// CoreFoundation / AVFoundation objects used from one thread at a time but
/// moved between threads (decoder thread -> render thread).
pub struct SendBox<T>(pub T);
unsafe impl<T> Send for SendBox<T> {}
unsafe impl<T> Sync for SendBox<T> {}

fn cf_key(k: &CFString) -> &NSString {
    // CFString and NSString are toll-free bridged.
    unsafe { &*(k as *const CFString as *const NSString) }
}

pub(crate) fn ns_dict_pub(pairs: &[(&NSString, &AnyObject)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    ns_dict(pairs)
}

fn ns_dict(pairs: &[(&NSString, &AnyObject)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<&NSString> = pairs.iter().map(|p| p.0).collect();
    let vals: Vec<&AnyObject> = pairs.iter().map(|p| p.1).collect();
    NSDictionary::from_slices(&keys, &vals)
}

fn bgra_buffer_attributes(w: Option<(usize, usize)>) -> Retained<NSDictionary<NSString, AnyObject>> {
    let fmt = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let yes = NSNumber::new_bool(true);
    let empty: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    let mut pairs: Vec<(&NSString, &AnyObject)> = unsafe {
        vec![
            (cf_key(kCVPixelBufferPixelFormatTypeKey), &fmt),
            (cf_key(kCVPixelBufferMetalCompatibilityKey), &yes),
            (cf_key(kCVPixelBufferIOSurfacePropertiesKey), &empty),
        ]
    };
    let (wn, hn);
    if let Some((w, h)) = w {
        wn = NSNumber::new_usize(w);
        hn = NSNumber::new_usize(h);
        unsafe {
            pairs.push((cf_key(objc2_core_video::kCVPixelBufferWidthKey), &wn));
            pairs.push((cf_key(objc2_core_video::kCVPixelBufferHeightKey), &hn));
        }
    }
    ns_dict(&pairs)
}

/// Maps CVPixelBuffers to Metal textures without copying.
pub struct Interop {
    cache: SendBox<CFRetained<CVMetalTextureCache>>,
    device: Device,
}

/// A texture view of a pixel buffer. Keeps the pixel buffer and the CoreVideo
/// texture alive for as long as the GPU may use it.
pub struct MappedFrame {
    pub texture: Texture,
    _cv: SendBox<CFRetained<CVMetalTexture>>,
    pub(crate) buffer: SendBox<CFRetained<CVPixelBuffer>>,
    /// The cache the texture came from: releasing a CoreVideo texture after
    /// its cache crashes, so every frame keeps its cache alive (declared
    /// last, so it drops last).
    _cache: SendBox<CFRetained<CVMetalTextureCache>>,
}

impl Interop {
    pub fn new(device: &Device) -> Result<Self, MediaError> {
        let mut out: *mut CVMetalTextureCache = std::ptr::null_mut();
        let rc = unsafe {
            CVMetalTextureCache::create(None, None, device.raw_device(), None, NonNull::new(&mut out).unwrap())
        };
        if rc != kCVReturnSuccess || out.is_null() {
            return Err(MediaError::Platform(format!("CVMetalTextureCacheCreate failed ({rc})")));
        }
        let cache = unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) };
        Ok(Self {
            cache: SendBox(cache),
            device: device.clone(),
        })
    }

    /// Wraps a BGRA pixel buffer as a Metal texture (zero copy).
    pub fn map(&self, pb: CFRetained<CVPixelBuffer>) -> Result<MappedFrame, MediaError> {
        let w = CVPixelBufferGetWidth(&pb);
        let h = CVPixelBufferGetHeight(&pb);
        let mut out: *mut CVMetalTexture = std::ptr::null_mut();
        let image: &CVImageBuffer = &pb;
        let rc = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.cache.0,
                image,
                None,
                MTLPixelFormat::BGRA8Unorm,
                w,
                h,
                0,
                NonNull::new(&mut out).unwrap(),
            )
        };
        if rc != kCVReturnSuccess || out.is_null() {
            return Err(MediaError::Platform(format!(
                "CVMetalTextureCacheCreateTextureFromImage failed ({rc})"
            )));
        }
        let cv = unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) };
        let raw = CVMetalTextureGetTexture(&cv).ok_or(MediaError::Platform("no Metal texture".into()))?;
        let texture = self.device.wrap_texture(raw, PixelFormat::Bgra8Unorm);
        Ok(MappedFrame {
            texture,
            _cv: SendBox(cv),
            buffer: SendBox(pb),
            _cache: SendBox(self.cache.0.clone()),
        })
    }
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, MediaError> {
    let abs = std::fs::canonicalize(path).map_err(|e| MediaError::Open(format!("{}: {e}", path.display())))?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(&abs.to_string_lossy())))
}

/// Sequential hardware decoder with seeking.
pub struct Reader {
    track: SendBox<Retained<AVAssetTrack>>,
    asset: SendBox<Retained<AVURLAsset>>,
    reader: Option<SendBox<(Retained<AVAssetReader>, Retained<AVAssetReaderTrackOutput>)>>,
    pub info: VideoInfo,
    /// Frames arrive as BGRA textures in the track's coded orientation.
    pub format: crate::FrameFormat,
}

/// One decoded frame: presentation time (seconds) and its pixel buffer.
pub struct Decoded {
    pub pts: f64,
    pub buffer: SendBox<CFRetained<CVPixelBuffer>>,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let url = file_url(path)?;
        unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let media = AVMediaTypeVideo.ok_or(MediaError::Platform("AVMediaTypeVideo".into()))?;
            #[allow(deprecated)]
            let tracks = asset.tracksWithMediaType(media);
            let track = tracks
                .firstObject()
                .ok_or_else(|| MediaError::Open(format!("{}: no video track", path.display())))?;
            let size = track.naturalSize();
            let fps = snap_fps(track.nominalFrameRate() as f64);
            let duration = asset.duration().seconds();
            // The display transform's rotation (y down: positive is
            // clockwise), to the nearest quarter turn.
            let t = track.preferredTransform();
            let rotation = ((t.b.atan2(t.a).to_degrees() / 90.0).round() as i64).rem_euclid(4) as u32 * 90;
            let coded = (size.width.round() as u32, size.height.round() as u32);
            let (width, height) = if rotation % 180 == 90 {
                (coded.1, coded.0)
            } else {
                coded
            };
            let info = VideoInfo {
                width,
                height,
                fps: if fps > 0.0 { fps } else { 30.0 },
                duration,
                rotation,
            };
            Ok(Self {
                track: SendBox(track),
                asset: SendBox(asset),
                reader: None,
                info,
                format: crate::FrameFormat {
                    layout: crate::Layout::Texture,
                    coded,
                    kr: 0.2126,
                    kb: 0.0722,
                    full_range: false,
                },
            })
        }
    }

    /// Restarts decoding so the next frame returned covers time `t`.
    pub fn seek(&mut self, t: f64) -> Result<(), MediaError> {
        if let Some(r) = self.reader.take() {
            unsafe { r.0.0.cancelReading() };
        }
        unsafe {
            let reader = AVAssetReader::assetReaderWithAsset_error(&self.asset.0)
                .map_err(|e| MediaError::Platform(e.localizedDescription().to_string()))?;
            let settings = bgra_buffer_attributes(None);
            let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                &self.track.0,
                Some(&settings),
            );
            // Frames are handed to the GPU, never written: skip the copy.
            output.setAlwaysCopiesSampleData(false);
            let as_output: &AVAssetReaderOutput = &output;
            if !reader.canAddOutput(as_output) {
                return Err(MediaError::Platform("cannot add reader output".into()));
            }
            reader.addOutput(as_output);
            if t > 0.0 {
                let start = CMTime::with_seconds(t.max(0.0), 600_000);
                let dur = CMTime::with_seconds((self.info.duration - t).max(0.001) + 1.0, 600_000);
                reader.setTimeRange(CMTimeRange { start, duration: dur });
            }
            if !reader.startReading() {
                let e = reader
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                return Err(MediaError::Platform(format!("startReading failed: {e}")));
            }
            self.reader = Some(SendBox((reader, output)));
        }
        Ok(())
    }

    /// Next decoded frame, or None at the end of the stream.
    pub fn next(&mut self) -> Result<Option<Decoded>, MediaError> {
        if self.reader.is_none() {
            self.seek(0.0)?;
        }
        let r = self.reader.as_ref().unwrap();
        unsafe {
            let Some(sample): Option<Retained<CMSampleBuffer>> = r.0.1.copyNextSampleBuffer() else {
                if r.0.0.status() == AVAssetReaderStatus::Failed {
                    let e =
                        r.0.0
                            .error()
                            .map(|e| e.localizedDescription().to_string())
                            .unwrap_or_default();
                    return Err(MediaError::Platform(format!("decode failed: {e}")));
                }
                return Ok(None);
            };
            let pts = sample.presentation_time_stamp().seconds();
            let Some(image) = sample.image_buffer() else {
                return Ok(None);
            };
            let pb: CFRetained<CVPixelBuffer> = image;
            Ok(Some(Decoded {
                pts,
                buffer: SendBox(pb),
            }))
        }
    }
}

/// Hardware encoder. Frames are rendered by the GPU straight into pixel
/// buffers from the encoder's pool.
pub struct Writer {
    writer: SendBox<Retained<AVAssetWriter>>,
    input: SendBox<Retained<AVAssetWriterInput>>,
    /// AAC audio input and the PCM format description of what we append.
    audio: Option<SendBox<(Retained<AVAssetWriterInput>, CFRetained<CMFormatDescription>)>>,
    audio_frames: u64,
    audio_done: bool,
    adaptor: SendBox<Retained<AVAssetWriterInputPixelBufferAdaptor>>,
    interop: Interop,
    fps: f64,
    pub width: u32,
    pub height: u32,
}

impl Writer {
    pub fn create(
        device: &Device,
        path: &Path,
        width: u32,
        height: u32,
        fps: f64,
        codec: Codec,
        bitrate: Option<u64>,
        audio_bitrate: Option<u32>,
    ) -> Result<Self, MediaError> {
        let _ = std::fs::remove_file(path);
        let abs_parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let abs = std::fs::canonicalize(abs_parent)
            .map_err(|e| MediaError::Open(format!("{}: {e}", abs_parent.display())))?
            .join(path.file_name().ok_or(MediaError::Open("no file name".into()))?);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&abs.to_string_lossy()));
        let is_mov = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mov"));
        unsafe {
            let file_type = if is_mov {
                AVFileTypeQuickTimeMovie
            } else {
                AVFileTypeMPEG4
            }
            .ok_or(MediaError::Platform("file type".into()))?;
            let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
                .map_err(|e| MediaError::Platform(e.localizedDescription().to_string()))?;

            let codec_name = match codec {
                Codec::H264 => AVVideoCodecTypeH264,
                Codec::Hevc => AVVideoCodecTypeHEVC,
            }
            .ok_or(MediaError::Platform("codec".into()))?;
            let w = NSNumber::new_u32(width);
            let h = NSNumber::new_u32(height);
            let rate = NSNumber::new_u64(bitrate.unwrap_or_else(|| default_bitrate(width, height, fps, codec)));
            let compression = ns_dict(&[(AVVideoAverageBitRateKey.unwrap(), &rate)]);
            let color = ns_dict(&[
                (
                    AVVideoColorPrimariesKey.unwrap(),
                    AVVideoColorPrimaries_ITU_R_709_2.unwrap(),
                ),
                (
                    AVVideoTransferFunctionKey.unwrap(),
                    AVVideoTransferFunction_ITU_R_709_2.unwrap(),
                ),
                (AVVideoYCbCrMatrixKey.unwrap(), AVVideoYCbCrMatrix_ITU_R_709_2.unwrap()),
            ]);
            let settings = ns_dict(&[
                (AVVideoCodecKey.unwrap(), codec_name),
                (AVVideoWidthKey.unwrap(), &w),
                (AVVideoHeightKey.unwrap(), &h),
                (AVVideoCompressionPropertiesKey.unwrap(), &compression),
                (AVVideoColorPropertiesKey.unwrap(), &color),
            ]);
            let media = AVMediaTypeVideo.ok_or(MediaError::Platform("AVMediaTypeVideo".into()))?;
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(media, Some(&settings));
            input.setExpectsMediaDataInRealTime(false);
            let attrs = bgra_buffer_attributes(Some((width as usize, height as usize)));
            let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, Some(&attrs));
            if !writer.canAddInput(&input) {
                return Err(MediaError::Platform("cannot add writer input".into()));
            }
            writer.addInput(&input);
            let audio = if let Some(bits) = audio_bitrate {
                Some(SendBox(add_audio_input(&writer, bits)?))
            } else {
                None
            };
            if !writer.startWriting() {
                let e = writer
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                return Err(MediaError::Platform(format!("startWriting failed: {e}")));
            }
            writer.startSessionAtSourceTime(CMTime::new(0, 600));
            Ok(Self {
                writer: SendBox(writer),
                input: SendBox(input),
                adaptor: SendBox(adaptor),
                audio,
                audio_frames: 0,
                audio_done: false,
                interop: Interop::new(device)?,
                fps,
                width,
                height,
            })
        }
    }

    /// A pixel buffer from the encoder's pool, mapped as a BGRA texture for
    /// the GPU to render into.
    pub fn frame(&self) -> Result<MappedFrame, MediaError> {
        unsafe {
            let pool: Option<Retained<CVPixelBufferPool>> = self.adaptor.0.pixelBufferPool();
            let pool = pool.ok_or(MediaError::Platform("encoder pixel buffer pool unavailable".into()))?;
            let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
            let rc = CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::new(&mut out).unwrap());
            if rc != kCVReturnSuccess || out.is_null() {
                return Err(MediaError::Platform(format!(
                    "CVPixelBufferPoolCreatePixelBuffer failed ({rc})"
                )));
            }
            self.interop.map(CFRetained::from_raw(NonNull::new(out).unwrap()))
        }
    }

    /// The writer's error, if it failed.
    pub fn failure(&self) -> Option<String> {
        unsafe {
            (self.writer.0.status() == AVAssetWriterStatus::Failed).then(|| {
                self.writer
                    .0
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default()
            })
        }
    }

    /// Whether the video input takes a frame now.
    pub fn video_ready(&self) -> bool {
        unsafe { self.input.0.isReadyForMoreMediaData() }
    }

    /// Whether the audio input (if any) takes samples now.
    pub fn audio_ready(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|a| unsafe { a.0.0.isReadyForMoreMediaData() })
    }

    /// Appends a rendered frame (the GPU must have finished writing it).
    pub fn append(&self, frame: &MappedFrame, index: u64) -> Result<(), MediaError> {
        unsafe {
            while !self.input.0.isReadyForMoreMediaData() {
                if self.writer.0.status() == AVAssetWriterStatus::Failed {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            // Timescale chosen so common rates (23.976, 29.97, 59.94) are exact.
            let ts = 60_000i32;
            let pts = CMTime::new((index as f64 * ts as f64 / self.fps).round() as i64, ts);
            if !self
                .adaptor
                .0
                .appendPixelBuffer_withPresentationTime(&frame.buffer.0, pts)
            {
                let e = self
                    .writer
                    .0
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                return Err(MediaError::Platform(format!("append failed: {e}")));
            }
        }
        Ok(())
    }

    /// Appends interleaved 48 kHz stereo float audio, continuing where the
    /// previous call ended.
    pub fn append_audio(&mut self, samples: &[f32]) -> Result<(), MediaError> {
        let Some(audio) = &self.audio else { return Ok(()) };
        let frames = samples.len() / crate::audio::CHANNELS;
        if frames == 0 {
            return Ok(());
        }
        let (input, fmt) = &audio.0;
        let len = samples.len() * 4;
        unsafe {
            let mut bb: *mut CMBlockBuffer = std::ptr::null_mut();
            let rc = CMBlockBuffer::create_with_memory_block(
                None,
                std::ptr::null_mut(),
                len,
                None,
                std::ptr::null(),
                0,
                len,
                0,
                NonNull::new(&mut bb).unwrap(),
            );
            if rc != 0 || bb.is_null() {
                return Err(MediaError::Platform(format!(
                    "CMBlockBufferCreateWithMemoryBlock failed ({rc})"
                )));
            }
            let bb = CFRetained::from_raw(NonNull::new(bb).unwrap());
            let rc = CMBlockBuffer::replace_data_bytes(NonNull::new(samples.as_ptr() as *mut _).unwrap(), &bb, 0, len);
            if rc != 0 {
                return Err(MediaError::Platform(format!(
                    "CMBlockBufferReplaceDataBytes failed ({rc})"
                )));
            }
            let mut sb: *mut CMSampleBuffer = std::ptr::null_mut();
            let pts = CMTime::new(self.audio_frames as i64, crate::audio::RATE as i32);
            let rc = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                None,
                &bb,
                fmt,
                frames as isize,
                pts,
                std::ptr::null(),
                NonNull::new(&mut sb).unwrap(),
            );
            if rc != 0 || sb.is_null() {
                return Err(MediaError::Platform(format!("audio sample buffer failed ({rc})")));
            }
            let sb: Retained<CMSampleBuffer> = Retained::from_raw(sb).unwrap();
            while !input.isReadyForMoreMediaData() {
                if self.writer.0.status() == AVAssetWriterStatus::Failed {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            if !input.appendSampleBuffer(&sb) {
                let e = self
                    .writer
                    .0
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                return Err(MediaError::Platform(format!("audio append failed: {e}")));
            }
        }
        self.audio_frames += frames as u64;
        Ok(())
    }

    /// Finishes the file and waits for it to be written.
    /// Declares the audio complete, so the writer stops holding video back
    /// waiting for more of it.
    pub fn finish_audio(&mut self) {
        if !self.audio_done {
            if let Some(a) = &self.audio {
                unsafe { a.0.0.markAsFinished() };
            }
            self.audio_done = true;
        }
    }

    pub fn finish(mut self) -> Result<(), MediaError> {
        self.finish_audio();
        unsafe {
            self.input.0.markAsFinished();
            let (tx, rx) = std::sync::mpsc::channel();
            let block = block2::RcBlock::new(move || {
                let _ = tx.send(());
            });
            self.writer.0.finishWritingWithCompletionHandler(&block);
            let _ = rx.recv();
            if self.writer.0.status() != AVAssetWriterStatus::Completed {
                let e = self
                    .writer
                    .0
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                return Err(MediaError::Platform(format!("finish failed: {e}")));
            }
        }
        Ok(())
    }
}

fn default_bitrate(w: u32, h: u32, fps: f64, codec: Codec) -> u64 {
    // ~0.1 bits per pixel per frame for H.264 at high quality, less for HEVC.
    let bpp = match codec {
        Codec::H264 => 0.12,
        Codec::Hevc => 0.08,
    };
    (w as f64 * h as f64 * fps * bpp) as u64
}

/// Containers report frame rates as floats (29.999998); snap to the common
/// exact rates so frame times line up.
fn snap_fps(fps: f64) -> f64 {
    const RATES: [f64; 9] = [
        24000.0 / 1001.0,
        24.0,
        25.0,
        30000.0 / 1001.0,
        30.0,
        50.0,
        60000.0 / 1001.0,
        60.0,
        120.0,
    ];
    RATES.iter().copied().find(|r| (r - fps).abs() < 2e-3).unwrap_or(fps)
}

/// Adds an AAC audio input (48 kHz stereo, `bits` per second) fed with
/// float PCM.
unsafe fn add_audio_input(
    writer: &AVAssetWriter,
    bits: u32,
) -> Result<(Retained<AVAssetWriterInput>, CFRetained<CMFormatDescription>), MediaError> {
    unsafe {
        let aac = NSNumber::new_u32(0x61616320); // 'aac '
        let rate = NSNumber::new_f64(crate::audio::RATE as f64);
        let ch = NSNumber::new_u32(crate::audio::CHANNELS as u32);
        let bitrate = NSNumber::new_u32(bits);
        let keys = [
            NSString::from_str("AVFormatIDKey"),
            NSString::from_str("AVSampleRateKey"),
            NSString::from_str("AVNumberOfChannelsKey"),
            NSString::from_str("AVEncoderBitRateKey"),
        ];
        let vals: [&AnyObject; 4] = [&aac, &rate, &ch, &bitrate];
        let pairs: Vec<(&NSString, &AnyObject)> = keys.iter().map(|k| &**k).zip(vals).collect();
        let settings = ns_dict(&pairs);
        let media = objc2_av_foundation::AVMediaTypeAudio.ok_or(MediaError::Platform("AVMediaTypeAudio".into()))?;
        let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(media, Some(&settings));
        input.setExpectsMediaDataInRealTime(false);
        if !writer.canAddInput(&input) {
            return Err(MediaError::Platform("cannot add audio input".into()));
        }
        writer.addInput(&input);
        let asbd = objc2_core_audio_types::AudioStreamBasicDescription {
            mSampleRate: crate::audio::RATE as f64,
            mFormatID: 0x6c70636d, // 'lpcm'
            mFormatFlags: 1 | 8,   // float, packed
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let mut fmt: *const objc2_core_media::CMAudioFormatDescription = std::ptr::null();
        let rc = CMAudioFormatDescriptionCreate(
            None,
            NonNull::from(&asbd),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            None,
            NonNull::new(&mut fmt).unwrap(),
        );
        if rc != 0 || fmt.is_null() {
            return Err(MediaError::Platform(format!(
                "CMAudioFormatDescriptionCreate failed ({rc})"
            )));
        }
        let fmt = CFRetained::from_raw(NonNull::new(fmt as *mut CMFormatDescription).unwrap());
        Ok((input, fmt))
    }
}
