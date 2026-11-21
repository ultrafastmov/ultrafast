//! Audio: decode any media file's audio to PCM, and mix PCM into the export.
//!
//! Audio is mixed on the CPU at 48 kHz stereo float (cheap next to video)
//! and encoded to AAC by the hardware-backed writer on macOS.

/// Interleaved stereo float PCM at [`RATE`].
#[derive(Debug, Clone, Default)]
pub struct Pcm {
    pub samples: Vec<f32>,
}

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / CHANNELS
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / RATE as f64
    }

    /// Adds `src` into `self` starting at output frame `at`, reading the
    /// source from frame `from`, `len` frames, scaled per frame by `gain`.
    pub fn mix_from(&mut self, src: &Pcm, at: usize, from: usize, len: usize, gain: impl Fn(usize) -> f32) {
        let end = (at + len).min(self.frames());
        for (k, out_frame) in (at..end).enumerate() {
            let s = from + k;
            if s >= src.frames() {
                break;
            }
            let g = gain(k);
            for c in 0..CHANNELS {
                self.samples[out_frame * CHANNELS + c] += src.samples[s * CHANNELS + c] * g;
            }
        }
    }
}

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
pub use mac::decode;

/// Decodes like [`decode`], played at `rate` with its pitch kept: ffmpeg's
/// `atempo` time stretch (a playback rate that keeps the pitch).
/// Returns None if the file has no audio.
pub fn decode_rate(path: &std::path::Path, rate: f64) -> Result<Option<Pcm>, crate::MediaError> {
    if (rate - 1.0).abs() < 1e-9 {
        return decode(path);
    }
    // One atempo stage takes 0.5..100; slower rates chain halvings.
    let mut r = rate.clamp(0.01, 100.0);
    let mut chain = Vec::new();
    while r < 0.5 {
        chain.push("atempo=0.5".to_string());
        r /= 0.5;
    }
    chain.push(format!("atempo={r:.6}"));
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-map", "0:a:0?", "-af"])
        .arg(chain.join(","))
        .args(["-f", "f32le", "-ac"])
        .arg(CHANNELS.to_string())
        .arg("-ar")
        .arg(RATE.to_string())
        .arg("-")
        .output()
        .map_err(|e| crate::MediaError::Platform(format!("ffmpeg: {e} (install ffmpeg)")))?;
    if !out.status.success() {
        return Err(crate::MediaError::Platform(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    if out.stdout.is_empty() {
        return Ok(None);
    }
    let samples = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(Some(Pcm { samples }))
}

/// Decodes like [`decode_rate`], only the source's seconds `[start, end)`
/// (before the time stretch); the PCM starts at `start`. A long recording
/// of which an export plays a minute decodes that minute. Returns None if
/// the file has no audio.
pub fn decode_span(path: &std::path::Path, rate: f64, start: f64, end: f64) -> Result<Option<Pcm>, crate::MediaError> {
    let start = start.max(0.0);
    // A file without sound plays silence (ffmpeg refuses an output with no
    // stream rather than writing nothing).
    let streams = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| crate::MediaError::Platform(format!("ffprobe: {e} (install ffmpeg)")))?;
    if streams.status.success() && String::from_utf8_lossy(&streams.stdout).trim().is_empty() {
        return Ok(None);
    }
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin"]);
    if start > 0.0 {
        cmd.arg("-ss").arg(format!("{start:.6}"));
    }
    cmd.arg("-t").arg(format!("{:.6}", (end - start).max(0.0)));
    cmd.arg("-i").arg(path).args(["-vn", "-map", "0:a:0?"]);
    if (rate - 1.0).abs() >= 1e-9 {
        // One atempo stage takes 0.5..100; slower rates chain halvings.
        let mut r = rate.clamp(0.01, 100.0);
        let mut chain = Vec::new();
        while r < 0.5 {
            chain.push("atempo=0.5".to_string());
            r /= 0.5;
        }
        chain.push(format!("atempo={r:.6}"));
        cmd.arg("-af").arg(chain.join(","));
    }
    let out = cmd
        .args(["-f", "f32le", "-ac"])
        .arg(CHANNELS.to_string())
        .arg("-ar")
        .arg(RATE.to_string())
        .arg("-")
        .output()
        .map_err(|e| crate::MediaError::Platform(format!("ffmpeg: {e} (install ffmpeg)")))?;
    if !out.status.success() {
        return Err(crate::MediaError::Platform(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    if out.stdout.is_empty() {
        return Ok(None);
    }
    let samples = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(Some(Pcm { samples }))
}

/// A media file's duration in seconds (its container's), if ffprobe can
/// tell.
pub fn duration(path: &std::path::Path) -> Option<f64> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Decodes the first audio track of `path` to 48 kHz stereo float. Returns
/// None if the file has no audio.
#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
pub fn decode(path: &std::path::Path) -> Result<Option<Pcm>, crate::MediaError> {
    crate::ffmpeg::decode_audio(path)
}

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
mod mac {
    use super::{CHANNELS, Pcm, RATE};
    use crate::MediaError;
    use crate::macos::ns_dict_pub as ns_dict;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_av_foundation::{
        AVAssetReader, AVAssetReaderOutput, AVAssetReaderTrackOutput, AVMediaTypeAudio, AVURLAsset,
    };
    use objc2_core_media::{CMBlockBuffer, CMSampleBuffer};
    use objc2_foundation::{NSNumber, NSString, NSURL};
    use std::path::Path;
    use std::ptr::NonNull;

    /// Linear PCM float32 interleaved stereo at 48 kHz.
    pub(crate) fn pcm_settings() -> Retained<objc2_foundation::NSDictionary<NSString, AnyObject>> {
        let fmt = NSNumber::new_u32(0x6c70636d); // 'lpcm'
        let rate = NSNumber::new_f64(RATE as f64);
        let ch = NSNumber::new_u32(CHANNELS as u32);
        let bits = NSNumber::new_u32(32);
        let yes = NSNumber::new_bool(true);
        let no = NSNumber::new_bool(false);
        let keys = [
            NSString::from_str("AVFormatIDKey"),
            NSString::from_str("AVSampleRateKey"),
            NSString::from_str("AVNumberOfChannelsKey"),
            NSString::from_str("AVLinearPCMBitDepthKey"),
            NSString::from_str("AVLinearPCMIsFloatKey"),
            NSString::from_str("AVLinearPCMIsNonInterleaved"),
            NSString::from_str("AVLinearPCMIsBigEndianKey"),
        ];
        let vals: [&AnyObject; 7] = [&fmt, &rate, &ch, &bits, &yes, &no, &no];
        let pairs: Vec<(&NSString, &AnyObject)> = keys.iter().map(|k| &**k).zip(vals).collect();
        ns_dict(&pairs)
    }

    pub fn decode(path: &Path) -> Result<Option<Pcm>, MediaError> {
        let abs = std::fs::canonicalize(path).map_err(|e| MediaError::Open(format!("{}: {e}", path.display())))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(&abs.to_string_lossy()));
        unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let media = AVMediaTypeAudio.ok_or(MediaError::Platform("AVMediaTypeAudio".into()))?;
            #[allow(deprecated)]
            let tracks = asset.tracksWithMediaType(media);
            let Some(track) = tracks.firstObject() else {
                return Ok(None);
            };
            let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
                .map_err(|e| MediaError::Platform(e.localizedDescription().to_string()))?;
            let settings = pcm_settings();
            let output =
                AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&track, Some(&settings));
            let out: &AVAssetReaderOutput = &output;
            reader.addOutput(out);
            if !reader.startReading() {
                return Err(MediaError::Platform("audio startReading failed".into()));
            }
            let mut bytes: Vec<u8> = Vec::new();
            while let Some(sample) = output.copyNextSampleBuffer() {
                let sample: Retained<CMSampleBuffer> = sample;
                let Some(block) = sample.data_buffer() else { continue };
                let block: &CMBlockBuffer = &block;
                let len = block.data_length();
                let start = bytes.len();
                bytes.resize(start + len, 0);
                let rc = block.copy_data_bytes(0, len, NonNull::new(bytes[start..].as_mut_ptr().cast()).unwrap());
                if rc != 0 {
                    return Err(MediaError::Platform(format!(
                        "CMBlockBufferCopyDataBytes failed ({rc})"
                    )));
                }
            }
            let samples = bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok(Some(Pcm { samples }))
        }
    }
}

/// Writes interleaved stereo float PCM as a 16-bit WAV file.
pub fn write_wav(path: &std::path::Path, pcm: &Pcm) -> std::io::Result<()> {
    let data: Vec<u8> = pcm
        .samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect();
    let mut f = Vec::with_capacity(44 + data.len());
    let ch = CHANNELS as u16;
    f.extend(b"RIFF");
    f.extend((36 + data.len() as u32).to_le_bytes());
    f.extend(b"WAVEfmt ");
    f.extend(16u32.to_le_bytes());
    f.extend(1u16.to_le_bytes());
    f.extend(ch.to_le_bytes());
    f.extend(RATE.to_le_bytes());
    f.extend((RATE * ch as u32 * 2).to_le_bytes());
    f.extend((ch * 2).to_le_bytes());
    f.extend(16u16.to_le_bytes());
    f.extend(b"data");
    f.extend((data.len() as u32).to_le_bytes());
    f.extend(data);
    std::fs::write(path, f)
}
