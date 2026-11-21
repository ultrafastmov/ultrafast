//! What an export writes: the container, the codecs, the quality level, the
//! output size and frame rate, and the sound.
//!
//! [`Settings`] is the one spelling every surface shares: the CLI's flags,
//! the MCP tools' arguments and the HTTP API's `output` all parse into it,
//! so a name or a check never differs between them. [`Settings::resolve`]
//! turns it into a [`Target`] for one output path and document, with every
//! default filled in and every combination checked before a frame renders.
//!
//! Room is left for what comes later (several sizes or aspect ratios from
//! one pass, platform presets, 10-bit and HDR, 8K): a [`Size`] that
//! reframes, a pixel depth on [`VideoTarget`], more containers and codecs
//! are each an added variant, not a change to what is here.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use ultrafast_graph::Format;

/// Quality levels. Each sets the encoder's quality and speed, how many
/// motion-blur samples a frame takes (never the shutter: the look stays
/// the document's) and the sound's bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// A fast preview: no motion blur, the fastest encoder settings and a
    /// smaller file.
    Draft,
    /// For delivery: the document's motion blur, a high-quality encode.
    #[default]
    Standard,
    /// For uploads that get re-encoded (social platforms): more motion
    /// blur samples and a slower, better encode.
    High,
    /// For masters and re-editing: twice the motion-blur samples and the
    /// slowest, best encode.
    Master,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Draft, Quality::Standard, Quality::High, Quality::Master];

    pub fn name(self) -> &'static str {
        match self {
            Quality::Draft => "draft",
            Quality::Standard => "standard",
            Quality::High => "high",
            Quality::Master => "master",
        }
    }

    /// The document's motion blur at this level: off for a draft, more
    /// samples (the same shutter) above standard.
    pub fn motion_blur(self, mb: Option<ultrafast_graph::MotionBlur>) -> Option<ultrafast_graph::MotionBlur> {
        let mb = mb.filter(|m| m.samples > 1)?;
        let more = |n: u32, by: f64| ((n as f64 * by).ceil() as u32).clamp(n, 64.max(n));
        match self {
            Quality::Draft => None,
            Quality::Standard => Some(mb),
            Quality::High | Quality::Master => {
                let by = if self == Quality::High { 1.5 } else { 2.0 };
                Some(ultrafast_graph::MotionBlur {
                    samples: more(mb.samples, by),
                    max_samples: mb.max_samples.map(|m| more(m, by)),
                    ..mb
                })
            }
        }
    }
}

impl std::str::FromStr for Quality {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        parse_name(s, "quality")
    }
}

/// The file an export writes. `png` and `exr` are image sequences, one
/// file a frame; `wav` to `opus` are sound alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    Webm,
    Gif,
    Png,
    Exr,
    Wav,
    Mp3,
    M4a,
    Flac,
    Opus,
}

impl Container {
    pub const ALL: [Container; 12] = [
        Container::Mp4,
        Container::Mov,
        Container::Mkv,
        Container::Webm,
        Container::Gif,
        Container::Png,
        Container::Exr,
        Container::Wav,
        Container::Mp3,
        Container::M4a,
        Container::Flac,
        Container::Opus,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mov => "mov",
            Container::Mkv => "mkv",
            Container::Webm => "webm",
            Container::Gif => "gif",
            Container::Png => "png",
            Container::Exr => "exr",
            Container::Wav => "wav",
            Container::Mp3 => "mp3",
            Container::M4a => "m4a",
            Container::Flac => "flac",
            Container::Opus => "opus",
        }
    }

    /// The container a file extension names.
    pub fn from_extension(ext: &str) -> Option<Container> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => Container::Mp4,
            "mov" | "qt" => Container::Mov,
            "mkv" => Container::Mkv,
            "webm" => Container::Webm,
            "gif" => Container::Gif,
            "png" => Container::Png,
            "exr" => Container::Exr,
            "wav" => Container::Wav,
            "mp3" => Container::Mp3,
            "m4a" | "aac" => Container::M4a,
            "flac" => Container::Flac,
            "opus" | "ogg" => Container::Opus,
            _ => return None,
        })
    }

    /// One image file a frame.
    pub fn is_sequence(self) -> bool {
        matches!(self, Container::Png | Container::Exr)
    }

    /// Sound only, no picture.
    pub fn is_audio(self) -> bool {
        matches!(
            self,
            Container::Wav | Container::Mp3 | Container::M4a | Container::Flac | Container::Opus
        )
    }

    /// The video codecs it holds, the default first.
    pub fn video_codecs(self) -> &'static [VideoCodec] {
        use VideoCodec::*;
        match self {
            Container::Mp4 => &[H264, Hevc, Av1],
            Container::Mov => &[H264, Hevc, Prores],
            Container::Mkv => &[H264, Hevc, Av1, Vp9, Prores],
            Container::Webm => &[Vp9, Av1],
            Container::Gif => &[Gif],
            Container::Png => &[Png],
            Container::Exr => &[Exr],
            _ => &[],
        }
    }

    /// The audio codecs it holds, the default first.
    pub fn audio_codecs(self) -> &'static [AudioCodec] {
        use AudioCodec::*;
        match self {
            Container::Mp4 => &[Aac, Mp3, Opus],
            Container::Mov => &[Aac, Pcm, Pcm24],
            Container::Mkv => &[Aac, Opus, Mp3, Flac, Pcm, Pcm24],
            Container::Webm => &[Opus],
            Container::Wav => &[Pcm, Pcm24],
            Container::Mp3 => &[Mp3],
            Container::M4a => &[Aac],
            Container::Flac => &[Flac],
            Container::Opus => &[Opus],
            Container::Gif | Container::Png | Container::Exr => &[],
        }
    }

    /// The container a codec goes in when nothing else names one.
    fn for_codec(codec: VideoCodec) -> Container {
        match codec {
            VideoCodec::Prores => Container::Mov,
            VideoCodec::Vp9 => Container::Webm,
            VideoCodec::Gif => Container::Gif,
            VideoCodec::Png => Container::Png,
            VideoCodec::Exr => Container::Exr,
            _ => Container::Mp4,
        }
    }

    /// The MIME type of the file (of one frame for a sequence).
    pub fn content_type(self) -> &'static str {
        match self {
            Container::Mp4 => "video/mp4",
            Container::Mov => "video/quicktime",
            Container::Mkv => "video/x-matroska",
            Container::Webm => "video/webm",
            Container::Gif => "image/gif",
            Container::Png => "image/png",
            Container::Exr => "image/x-exr",
            Container::Wav => "audio/wav",
            Container::Mp3 => "audio/mpeg",
            Container::M4a => "audio/mp4",
            Container::Flac => "audio/flac",
            Container::Opus => "audio/ogg",
        }
    }
}

impl std::str::FromStr for Container {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Container::from_extension(s.trim_start_matches('.')).ok_or_else(|| {
            format!(
                "unknown format `{s}` ({})",
                Container::ALL.map(Container::name).join(" | ")
            )
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    H264,
    #[serde(alias = "h265")]
    Hevc,
    Av1,
    Vp9,
    Prores,
    /// The rest are implied by their container.
    Gif,
    Png,
    Exr,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::Hevc => "hevc",
            VideoCodec::Av1 => "av1",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Prores => "prores",
            VideoCodec::Gif => "gif",
            VideoCodec::Png => "png",
            VideoCodec::Exr => "exr",
        }
    }

    /// The CRF scale's top (0 is best), for codecs that have one.
    pub fn crf_max(self) -> Option<f64> {
        match self {
            VideoCodec::H264 | VideoCodec::Hevc => Some(51.0),
            VideoCodec::Av1 | VideoCodec::Vp9 => Some(63.0),
            _ => None,
        }
    }

    /// The CRF each quality level encodes at. Standard is what Remotion
    /// defaults to and HyperFrames' standard (H.264 18, HEVC 23, AV1 30,
    /// VP9 28); H.264's draft and high match HyperFrames' too.
    pub fn crf(self, q: Quality) -> Option<f64> {
        let [d, s, h, m] = match self {
            VideoCodec::H264 => [28.0, 18.0, 15.0, 12.0],
            VideoCodec::Hevc => [30.0, 23.0, 20.0, 16.0],
            VideoCodec::Av1 => [42.0, 30.0, 25.0, 20.0],
            VideoCodec::Vp9 => [42.0, 28.0, 24.0, 18.0],
            _ => return None,
        };
        Some(match q {
            Quality::Draft => d,
            Quality::Standard => s,
            Quality::High => h,
            Quality::Master => m,
        })
    }

    /// Whether it can carry an alpha channel (ProRes only in 4444).
    pub fn has_alpha(self) -> bool {
        matches!(
            self,
            VideoCodec::Vp9 | VideoCodec::Prores | VideoCodec::Gif | VideoCodec::Png | VideoCodec::Exr
        )
    }

    /// Chroma subsampled 4:2:0 or 4:2:2: even width and height.
    pub fn needs_even(self) -> bool {
        matches!(
            self,
            VideoCodec::H264 | VideoCodec::Hevc | VideoCodec::Av1 | VideoCodec::Vp9 | VideoCodec::Prores
        )
    }
}

impl std::str::FromStr for VideoCodec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        parse_name(s, "codec")
    }
}

/// Apple ProRes flavors, lightest first. 4444 and 4444 XQ carry alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProresProfile {
    #[serde(rename = "proxy")]
    Proxy,
    #[serde(rename = "light", alias = "lt")]
    Light,
    #[serde(rename = "standard", alias = "422")]
    Standard,
    #[serde(rename = "hq", alias = "422hq", alias = "422-hq")]
    Hq,
    #[serde(rename = "4444")]
    P4444,
    #[serde(rename = "4444-xq", alias = "4444xq")]
    P4444Xq,
}

impl ProresProfile {
    pub fn name(self) -> &'static str {
        match self {
            ProresProfile::Proxy => "proxy",
            ProresProfile::Light => "light",
            ProresProfile::Standard => "standard",
            ProresProfile::Hq => "hq",
            ProresProfile::P4444 => "4444",
            ProresProfile::P4444Xq => "4444-xq",
        }
    }

    pub fn has_alpha(self) -> bool {
        matches!(self, ProresProfile::P4444 | ProresProfile::P4444Xq)
    }

    /// ffmpeg `prores_ks` `-profile:v`.
    pub fn ffmpeg_profile(self) -> u32 {
        self as u32
    }
}

impl std::str::FromStr for ProresProfile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        parse_name(s, "ProRes profile")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioCodec {
    #[serde(rename = "aac")]
    Aac,
    #[serde(rename = "opus")]
    Opus,
    #[serde(rename = "mp3")]
    Mp3,
    #[serde(rename = "flac")]
    Flac,
    /// 16-bit PCM.
    #[serde(rename = "pcm", alias = "pcm-16", alias = "pcm16")]
    Pcm,
    #[serde(rename = "pcm-24", alias = "pcm24")]
    Pcm24,
}

impl AudioCodec {
    pub fn name(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::Opus => "opus",
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Flac => "flac",
            AudioCodec::Pcm => "pcm",
            AudioCodec::Pcm24 => "pcm-24",
        }
    }

    pub fn lossless(self) -> bool {
        matches!(self, AudioCodec::Flac | AudioCodec::Pcm | AudioCodec::Pcm24)
    }

    /// Bits per second at each quality level (lossy codecs).
    pub fn bitrate(self, q: Quality) -> Option<u64> {
        let [d, s, h, m] = match self {
            AudioCodec::Aac | AudioCodec::Mp3 => [128, 192, 256, 320],
            AudioCodec::Opus => [96, 128, 160, 192],
            _ => return None,
        };
        Some(
            1000 * match q {
                Quality::Draft => d,
                Quality::Standard => s,
                Quality::High => h,
                Quality::Master => m,
            },
        )
    }
}

impl std::str::FromStr for AudioCodec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        parse_name(s, "audio codec")
    }
}

/// An output size: `720p` (the short side, so a vertical film stays
/// vertical), `4k` (2160p), `WxH`, or `[w, h]` in JSON. A size whose aspect
/// differs from the document's is refused for now (reframing comes later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    Short(u32),
    Exact(u32, u32),
}

impl std::str::FromStr for Size {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let l = s.trim().to_ascii_lowercase();
        let bad = || format!("bad size `{s}` (720p, 1080p, 1440p, 4k, or WxH such as 1280x720)");
        if l == "4k" || l == "uhd" {
            return Ok(Size::Short(2160));
        }
        if let Some(n) = l.strip_suffix('p') {
            return match n.parse::<u32>() {
                Ok(n) if n >= 2 => Ok(Size::Short(n)),
                _ => Err(bad()),
            };
        }
        let (w, h) = l.split_once('x').ok_or_else(bad)?;
        match (w.parse::<u32>(), h.parse::<u32>()) {
            (Ok(w), Ok(h)) if w >= 2 && h >= 2 => Ok(Size::Exact(w, h)),
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Size::Short(2160) => write!(f, "4k"),
            Size::Short(n) => write!(f, "{n}p"),
            Size::Exact(w, h) => write!(f, "{w}x{h}"),
        }
    }
}

impl Serialize for Size {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Pair([u32; 2]),
        }
        match Wire::deserialize(d)? {
            Wire::Text(s) => s.parse().map_err(serde::de::Error::custom),
            Wire::Pair([w, h]) if w >= 2 && h >= 2 => Ok(Size::Exact(w, h)),
            Wire::Pair(_) => Err(serde::de::Error::custom("a size is at least 2x2")),
        }
    }
}

/// Bits per second: a number, or text with a `k` or `M` suffix (`8M`,
/// `192k`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bitrate(pub u64);

impl std::str::FromStr for Bitrate {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let (num, mul) = match t.char_indices().last() {
            Some((i, 'k' | 'K')) => (&t[..i], 1e3),
            Some((i, 'm' | 'M')) => (&t[..i], 1e6),
            _ => (t, 1.0),
        };
        match num.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 => Ok(Bitrate((v * mul).round() as u64)),
            _ => Err(format!("bad bitrate `{s}` (bits per second, e.g. 8000000, 8M or 192k)")),
        }
    }
}

impl Serialize for Bitrate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for Bitrate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Number(f64),
            Text(String),
        }
        match Wire::deserialize(d)? {
            Wire::Number(v) if v.is_finite() && v > 0.0 => Ok(Bitrate(v.round() as u64)),
            Wire::Number(v) => Err(serde::de::Error::custom(format!("bad bitrate {v}"))),
            Wire::Text(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}

/// Export settings as every surface spells them; all optional. Unknown
/// names are refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The container (default: from the output's extension, else from the
    /// codec, else mp4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Container>,
    /// The video codec (default: the container's first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<VideoCodec>,
    /// Default standard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<Quality>,
    /// Constant quality on the codec's own scale (lower is better),
    /// instead of the quality level's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crf: Option<f64>,
    /// An average bitrate instead of constant quality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<Bitrate>,
    /// Default hq, or 4444 with alpha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prores_profile: Option<ProresProfile>,
    /// Keep transparency (ProRes 4444, VP9, GIF, PNG, EXR).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<bool>,
    /// The output size, or `scale` times the document's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// The output frame rate (default the document's). Time is in seconds,
    /// so the film plays at the same speed; simulations and motion blur
    /// keep the document's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// Mix the document's sound in (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<AudioCodec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate: Option<Bitrate>,
    /// GIF: how many more times it plays after the first (0: once; default
    /// forever).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loops: Option<u32>,
}

/// How the video's bits are spent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rate {
    /// Constant quality on the codec's CRF scale.
    Crf(f64),
    /// Average bits per second.
    Bitrate(u64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoTarget {
    pub codec: VideoCodec,
    /// Set for ProRes.
    pub prores: Option<ProresProfile>,
    pub alpha: bool,
    /// None for codecs without rate control (ProRes, GIF, PNG, EXR).
    pub rate: Option<Rate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTarget {
    pub codec: AudioCodec,
    /// None for lossless codecs.
    pub bitrate: Option<u64>,
}

/// Settings resolved for one output and document: what gets written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub container: Container,
    /// None for sound alone.
    pub video: Option<VideoTarget>,
    /// How the document's sound is written, if it has any; None when the
    /// output has no sound.
    pub audio: Option<AudioTarget>,
    pub quality: Quality,
    /// Output size and frame rate.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// GIF repeats (None: forever).
    pub loops: Option<u32>,
}

impl Target {
    /// The default: an H.264 MP4 at standard quality, the document's size
    /// and rate, with its sound.
    pub fn of(format: &Format) -> Target {
        Settings::default()
            .resolve(None, format)
            .expect("the default settings resolve")
    }

    /// Whether the output is the document's own size.
    pub fn native_size(&self, format: &Format) -> bool {
        self.width == format.width && self.height == format.height
    }

    /// The video codec (None for sound alone).
    pub fn codec(&self) -> Option<VideoCodec> {
        self.video.map(|v| v.codec)
    }

    /// What it is, in a few words: `mp4 h264, standard`, `png sequence,
    /// alpha, draft`, `wav pcm`.
    pub fn label(&self) -> String {
        let what = match self.video {
            Some(_) if self.container.is_sequence() => format!("{} sequence", self.container.name()),
            Some(v) if v.codec.name() == self.container.name() => self.container.name().to_string(),
            _ => format!("{} {}", self.container.name(), self.codec_name()),
        };
        let alpha = if self.video.is_some_and(|v| v.alpha) {
            ", alpha"
        } else {
            ""
        };
        match self.video {
            Some(_) => format!("{what}{alpha}, {}", self.quality.name()),
            None => what,
        }
    }

    /// The codec as the receipt names it (`prores-hq`, `h264`, `pcm`).
    pub fn codec_name(&self) -> String {
        match (self.video, self.audio) {
            (Some(VideoTarget { prores: Some(p), .. }), _) => format!("prores-{}", p.name()),
            (Some(v), _) => v.codec.name().into(),
            (None, Some(a)) => a.codec.name().into(),
            (None, None) => "none".into(),
        }
    }
}

impl Settings {
    /// Settings from JSON (the API's `output`, an MCP tool's arguments
    /// with the other keys taken out).
    pub fn from_json(v: &serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value(v.clone()).map_err(|e| e.to_string())
    }

    /// Settings from command-line flags: `flag(name)` is the value after
    /// `--name`, `has(name)` whether `--name` is there.
    pub fn from_flags(flag: impl Fn(&str) -> Option<String>, has: impl Fn(&str) -> bool) -> Result<Settings, String> {
        fn parsed<T: std::str::FromStr<Err = String>>(v: Option<String>) -> Result<Option<T>, String> {
            v.map(|s| s.parse()).transpose()
        }
        let num = |name: &str| -> Result<Option<f64>, String> {
            flag(name)
                .map(|s| parse_number(&s).ok_or_else(|| format!("{name}: `{s}` is not a number")))
                .transpose()
        };
        Ok(Settings {
            format: parsed(flag("--format"))?,
            codec: parsed(flag("--codec"))?,
            quality: parsed(flag("--quality"))?,
            crf: num("--crf")?,
            bitrate: parsed(flag("--bitrate"))?,
            prores_profile: parsed(flag("--prores-profile"))?,
            alpha: has("--alpha").then_some(true),
            size: parsed(flag("--size"))?,
            scale: num("--scale")?,
            fps: num("--fps")?,
            audio: has("--no-audio").then_some(false),
            audio_codec: parsed(flag("--audio-codec"))?,
            audio_bitrate: parsed(flag("--audio-bitrate"))?,
            loops: flag("--loops")
                .map(|s| s.parse::<u32>().map_err(|_| format!("--loops: `{s}` is not a count")))
                .transpose()?,
        })
    }

    /// The container the settings write to `path` (None: no file name to
    /// go by): `format`, else the extension, else the codec's, else mp4.
    pub fn container(&self, path: Option<&Path>) -> Result<Container, String> {
        let ext = path
            .and_then(|p| sequence_extension(p).or_else(|| p.extension().map(|e| e.to_string_lossy().into_owned())));
        let from_ext = match &ext {
            Some(e) => Some(
                Container::from_extension(e)
                    .ok_or_else(|| format!("can't tell the format from `.{e}`: pass `format`"))?,
            ),
            None => None,
        };
        let container = match (self.format, from_ext) {
            (Some(f), Some(e)) if f != e && !(f.is_sequence() && path.is_some_and(is_sequence_dir)) => {
                return Err(format!(
                    "format `{}` doesn't match the output's `.{}` extension",
                    f.name(),
                    ext.unwrap_or_default()
                ));
            }
            (Some(f), _) => f,
            (None, Some(e)) => e,
            (None, None) if path.is_some_and(is_sequence_dir) => Container::Png,
            (None, None) => self.codec.map_or(Container::Mp4, Container::for_codec),
        };
        if path.is_some_and(is_sequence_dir) && !container.is_sequence() {
            return Err(format!(
                "a folder takes an image sequence (png or exr), not {}: name a file",
                container.name()
            ));
        }
        Ok(container)
    }

    /// Resolves the settings for an output at `path` (None: no file name to
    /// go by, as for a stored render named later) and a document of
    /// `format`.
    pub fn resolve(&self, path: Option<&Path>, format: &Format) -> Result<Target, String> {
        let quality = self.quality.unwrap_or_default();
        let container = self.container(path)?;

        // Sound.
        let wants_audio = self.audio.unwrap_or(true);
        let audio = if container.audio_codecs().is_empty() || !wants_audio {
            if let Some(a) = self.audio_codec.filter(|_| wants_audio) {
                return Err(format!(
                    "{} has no sound, so no audio codec `{}`",
                    container.name(),
                    a.name()
                ));
            }
            None
        } else {
            let allowed = container.audio_codecs();
            let codec = match self.audio_codec {
                Some(a) if allowed.contains(&a) => a,
                Some(a) => {
                    return Err(format!(
                        "{} can't hold {} sound ({})",
                        container.name(),
                        a.name(),
                        allowed.iter().map(|c| c.name()).collect::<Vec<_>>().join(" | ")
                    ));
                }
                // ProRes masters carry uncompressed sound, as editors expect.
                None if container == Container::Mov && self.codec == Some(VideoCodec::Prores) => AudioCodec::Pcm,
                None => allowed[0],
            };
            let bitrate = if codec.lossless() {
                None
            } else {
                self.audio_bitrate.map(|b| b.0).or(codec.bitrate(quality))
            };
            Some(AudioTarget { codec, bitrate })
        };
        if container.is_audio() {
            if !wants_audio {
                return Err(format!("{} is sound only, so it can't be silent", container.name()));
            }
            if let Some(c) = self.codec {
                return Err(format!(
                    "{} is sound only, so no video codec `{}`",
                    container.name(),
                    c.name()
                ));
            }
            return Ok(Target {
                container,
                video: None,
                audio,
                quality,
                width: format.width,
                height: format.height,
                fps: format.fps,
                loops: None,
            });
        }

        // Picture.
        let allowed = container.video_codecs();
        let codec = match self.codec {
            Some(c) if allowed.contains(&c) => c,
            Some(c) => {
                let homes: Vec<&str> = Container::ALL
                    .iter()
                    .filter(|k| k.video_codecs().contains(&c))
                    .map(|k| k.name())
                    .collect();
                return Err(format!(
                    "{} can't hold {} (it goes in {})",
                    container.name(),
                    c.name(),
                    homes.join(", ")
                ));
            }
            None => allowed[0],
        };
        let alpha = self.alpha.unwrap_or(false);
        if alpha && !codec.has_alpha() {
            return Err(format!(
                "{} can't keep transparency: use ProRes 4444 (.mov), VP9 (.webm), GIF, or a PNG or EXR sequence",
                codec.name()
            ));
        }
        let prores = match (codec, self.prores_profile) {
            (VideoCodec::Prores, Some(p)) if alpha && !p.has_alpha() => {
                return Err(format!("ProRes {} has no alpha channel: use 4444 or 4444-xq", p.name()));
            }
            (VideoCodec::Prores, Some(p)) => Some(p),
            (VideoCodec::Prores, None) if alpha => Some(ProresProfile::P4444),
            (VideoCodec::Prores, None) if quality == Quality::Draft => Some(ProresProfile::Proxy),
            (VideoCodec::Prores, None) => Some(ProresProfile::Hq),
            (_, Some(p)) => return Err(format!("prores_profile `{}` is for codec prores", p.name())),
            (_, None) => None,
        };
        let rate = match (self.crf, self.bitrate, codec.crf_max()) {
            (Some(_), Some(_), _) => return Err("pass crf or bitrate, not both".into()),
            (Some(c), None, Some(max)) if (0.0..=max).contains(&c) => Some(Rate::Crf(c)),
            (Some(c), None, Some(max)) => return Err(format!("crf {c} is outside {}'s 0..{max}", codec.name())),
            (None, Some(b), Some(_)) => Some(Rate::Bitrate(b.0)),
            (Some(_), _, None) | (_, Some(_), None) => {
                return Err(format!(
                    "{} has no crf or bitrate: its quality is fixed by its format",
                    codec.name()
                ));
            }
            (None, None, Some(_)) => codec.crf(quality).map(Rate::Crf),
            (None, None, None) => None,
        };
        if self.loops.is_some() && container != Container::Gif {
            return Err("loops is for GIFs".into());
        }

        // Size.
        let (w, h) = (format.width.max(1) as f64, format.height.max(1) as f64);
        let (mut ow, mut oh) = match (self.size, self.scale) {
            (Some(_), Some(_)) => return Err("pass size or scale, not both".into()),
            (None, Some(s)) if s.is_finite() && s > 0.0 => (w * s, h * s),
            (None, Some(s)) => return Err(format!("bad scale {s}")),
            (Some(Size::Short(n)), None) => {
                let s = n as f64 / w.min(h);
                (w * s, h * s)
            }
            (Some(Size::Exact(a, b)), None) => {
                let (a, b) = (a as f64, b as f64);
                if ((a / b) / (w / h) - 1.0).abs() > 0.01 {
                    return Err(format!(
                        "{a}x{b} has a different shape from the document's {w}x{h}; reframing to another aspect ratio isn't supported yet: change the document's format, or pass a size of the same shape"
                    ));
                }
                (a, b)
            }
            (None, None) => (w, h),
        };
        if codec.needs_even() {
            ow = (ow / 2.0).round() * 2.0;
            oh = (oh / 2.0).round() * 2.0;
        }
        let (ow, oh) = (ow.round().max(2.0) as u32, oh.round().max(2.0) as u32);
        if ow.max(oh) > MAX_SIDE {
            return Err(format!(
                "{ow}x{oh} is larger than the {MAX_SIDE} px a side exports support"
            ));
        }

        // Frame rate.
        let fps = match self.fps {
            Some(f) if f.is_finite() && (1.0..=240.0).contains(&f) => f,
            Some(f) => return Err(format!("bad fps {f} (1 to 240)")),
            // GIFs are big: at most 30 fps unless asked (60 becomes 30, 50 25).
            None if container == Container::Gif => {
                let mut f = format.fps;
                while f > 30.0 {
                    f /= 2.0;
                }
                f
            }
            None => format.fps,
        };
        if container == Container::Gif && fps > 50.0 {
            return Err(format!(
                "a GIF plays at most 50 fps (browsers slow faster ones down): pass fps 50 or less, not {fps}"
            ));
        }
        Ok(Target {
            container,
            video: Some(VideoTarget {
                codec,
                prores,
                alpha,
                rate,
            }),
            audio,
            quality,
            width: ow,
            height: oh,
            fps,
            loops: self.loops,
        })
    }
}

/// The largest output side (the GPU's texture limit leaves room for 8K).
pub const MAX_SIDE: u32 = 8192;

/// A path naming an image sequence's folder: ending in `/`, or a folder
/// that exists.
pub fn is_sequence_dir(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.ends_with('/') || s.ends_with(std::path::MAIN_SEPARATOR) || p.is_dir()
}

/// The extension of a frame pattern (`f-{frame:05}.png`, `f-%05d.exr`).
fn sequence_extension(p: &Path) -> Option<String> {
    let s = p.to_string_lossy();
    if is_sequence_dir(p) || !(s.contains("{frame") || s.contains('%')) {
        return None;
    }
    p.extension().map(|e| e.to_string_lossy().into_owned())
}

/// The ffmpeg image2 pattern for a sequence written at `path`: a folder
/// gets `<folder>/%05d.<ext>`; `{frame}` and `{frame:05}` become `%d` and
/// `%05d`; a `%d`-style pattern is used as it is.
pub fn sequence_pattern(path: &Path, container: Container) -> String {
    let s = path.to_string_lossy();
    if is_sequence_dir(path) {
        return path
            .join(format!("%05d.{}", container.name()))
            .to_string_lossy()
            .into_owned();
    }
    if let Some(start) = s.find("{frame")
        && let Some(len) = s[start..].find('}')
    {
        let spec = &s[start + 6..start + len];
        let width = spec
            .strip_prefix(":0")
            .or(spec.strip_prefix(':'))
            .and_then(|w| w.parse::<u32>().ok());
        let fmt = match width {
            Some(w) => format!("%0{w}d"),
            None => "%d".into(),
        };
        return format!("{}{fmt}{}", &s[..start], &s[start + len + 1..]);
    }
    if s.contains('%') {
        return s.into_owned();
    }
    // A plain file name: number the frames before the extension.
    let stem = path.with_extension("");
    format!("{}-%05d.{}", stem.to_string_lossy(), container.name())
}

/// `30`, `29.97`, or `30000/1001`.
pub fn parse_number(s: &str) -> Option<f64> {
    let v = match s.split_once('/') {
        Some((a, b)) => a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?,
        None => s.trim().parse::<f64>().ok()?,
    };
    v.is_finite().then_some(v)
}

/// Parses a lowercase serde name, with the valid names in the error.
fn parse_name<T: serde::de::DeserializeOwned>(s: &str, what: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).map_err(|_| {
        let names = match what {
            "quality" => "draft | standard | high | master",
            "codec" => "h264 | hevc | av1 | vp9 | prores",
            "ProRes profile" => "proxy | light | standard | hq | 4444 | 4444-xq",
            "audio codec" => "aac | opus | mp3 | flac | pcm | pcm-24",
            _ => "",
        };
        format!("unknown {what} `{s}` ({names})")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fmt(w: u32, h: u32, fps: f64) -> Format {
        Format {
            width: w,
            height: h,
            fps,
            duration: 2.0,
            motion_blur: None,
        }
    }

    fn resolve(json: &str, path: &str) -> Result<Target, String> {
        let s: Settings = serde_json::from_str(json).map_err(|e| e.to_string())?;
        s.resolve(Some(Path::new(path)), &fmt(1920, 1080, 30.0))
    }

    #[test]
    fn defaults_follow_the_extension() {
        let t = resolve("{}", "film.mp4").unwrap();
        assert_eq!(t.container, Container::Mp4);
        assert_eq!(t.codec(), Some(VideoCodec::H264));
        assert_eq!(t.video.unwrap().rate, Some(Rate::Crf(18.0)));
        assert_eq!(t.audio.unwrap().codec, AudioCodec::Aac);
        assert_eq!(t.audio.unwrap().bitrate, Some(192_000));
        assert_eq!((t.width, t.height, t.fps), (1920, 1080, 30.0));
        assert_eq!(resolve("{}", "film.webm").unwrap().codec(), Some(VideoCodec::Vp9));
        assert_eq!(
            resolve("{}", "film.webm").unwrap().audio.unwrap().codec,
            AudioCodec::Opus
        );
        assert_eq!(resolve("{}", "film.gif").unwrap().audio, None);
        assert_eq!(resolve("{}", "film.mov").unwrap().codec(), Some(VideoCodec::H264));
        let a = resolve("{}", "film.wav").unwrap();
        assert!(a.video.is_none());
        assert_eq!(a.audio.unwrap().codec, AudioCodec::Pcm);
        // No path: the codec picks the container.
        let s: Settings = serde_json::from_str(r#"{"codec": "prores"}"#).unwrap();
        assert_eq!(
            s.resolve(None, &fmt(1920, 1080, 30.0)).unwrap().container,
            Container::Mov
        );
    }

    #[test]
    fn prores_profiles_and_alpha() {
        let t = resolve(r#"{"codec": "prores"}"#, "m.mov").unwrap();
        assert_eq!(t.video.unwrap().prores, Some(ProresProfile::Hq));
        assert_eq!(t.audio.unwrap().codec, AudioCodec::Pcm);
        assert_eq!(t.codec_name(), "prores-hq");
        let t = resolve(r#"{"codec": "prores", "alpha": true}"#, "m.mov").unwrap();
        assert_eq!(t.video.unwrap().prores, Some(ProresProfile::P4444));
        assert!(resolve(r#"{"codec": "prores", "alpha": true, "prores_profile": "hq"}"#, "m.mov").is_err());
        let t = resolve(r#"{"codec": "prores", "prores_profile": "lt"}"#, "m.mov").unwrap();
        assert_eq!(t.video.unwrap().prores, Some(ProresProfile::Light));
        assert!(resolve(r#"{"alpha": true}"#, "film.mp4").is_err());
        assert!(resolve(r#"{"alpha": true}"#, "film.webm").unwrap().video.unwrap().alpha);
        assert!(resolve(r#"{"codec": "prores"}"#, "film.mp4").is_err());
        assert!(resolve(r#"{"crf": 20}"#, "m.mov").is_ok());
        assert!(resolve(r#"{"codec": "prores", "crf": 20}"#, "m.mov").is_err());
    }

    #[test]
    fn sizes_keep_the_shape() {
        let t = resolve(r#"{"size": "720p"}"#, "f.mp4").unwrap();
        assert_eq!((t.width, t.height), (1280, 720));
        let t = resolve(r#"{"size": "4k"}"#, "f.mp4").unwrap();
        assert_eq!((t.width, t.height), (3840, 2160));
        let t = resolve(r#"{"scale": 0.5}"#, "f.mp4").unwrap();
        assert_eq!((t.width, t.height), (960, 540));
        let t = resolve(r#"{"size": [1280, 720]}"#, "f.mp4").unwrap();
        assert_eq!((t.width, t.height), (1280, 720));
        assert!(resolve(r#"{"size": "1080x1080"}"#, "f.mp4").is_err());
        assert!(resolve(r#"{"size": "720p", "scale": 2}"#, "f.mp4").is_err());
        // A vertical film stays vertical.
        let s: Settings = serde_json::from_str(r#"{"size": "720p"}"#).unwrap();
        let t = s.resolve(Some(Path::new("v.mp4")), &fmt(1080, 1920, 30.0)).unwrap();
        assert_eq!((t.width, t.height), (720, 1280));
        // Odd sizes round to even for 4:2:0 codecs, not for PNG.
        let s: Settings = serde_json::from_str(r#"{"scale": 0.5}"#).unwrap();
        let t = s.resolve(Some(Path::new("v.mp4")), &fmt(1001, 1001, 30.0)).unwrap();
        assert_eq!((t.width, t.height), (500, 500));
        let t = s.resolve(Some(Path::new("frames/")), &fmt(1001, 1001, 30.0)).unwrap();
        assert_eq!((t.width, t.height), (501, 501));
        assert!(resolve(r#"{"scale": 10}"#, "f.mp4").is_err());
    }

    #[test]
    fn sequences() {
        let t = resolve("{}", "frames/").unwrap();
        assert_eq!(t.container, Container::Png);
        assert_eq!(resolve("{}", "out/f-{frame:05}.exr").unwrap().container, Container::Exr);
        assert_eq!(
            resolve(r#"{"format": "exr"}"#, "frames/").unwrap().container,
            Container::Exr
        );
        assert!(resolve(r#"{"format": "mp4"}"#, "frames/").is_err());
        assert_eq!(
            sequence_pattern(Path::new("frames/"), Container::Png),
            PathBuf::from("frames").join("%05d.png").to_string_lossy()
        );
        assert_eq!(
            sequence_pattern(Path::new("o/f-{frame:04}.exr"), Container::Exr),
            "o/f-%04d.exr"
        );
        assert_eq!(
            sequence_pattern(Path::new("o/f-{frame}.png"), Container::Png),
            "o/f-%d.png"
        );
        assert_eq!(
            sequence_pattern(Path::new("o/f-%03d.png"), Container::Png),
            "o/f-%03d.png"
        );
        assert_eq!(
            sequence_pattern(Path::new("o/shot.png"), Container::Png),
            "o/shot-%05d.png"
        );
    }

    #[test]
    fn rates_and_quality() {
        let t = resolve(r#"{"quality": "draft"}"#, "f.mp4").unwrap();
        assert_eq!(t.video.unwrap().rate, Some(Rate::Crf(28.0)));
        assert_eq!(t.audio.unwrap().bitrate, Some(128_000));
        let t = resolve(r#"{"bitrate": "8M"}"#, "f.mp4").unwrap();
        assert_eq!(t.video.unwrap().rate, Some(Rate::Bitrate(8_000_000)));
        let t = resolve(r#"{"crf": 23}"#, "f.mp4").unwrap();
        assert_eq!(t.video.unwrap().rate, Some(Rate::Crf(23.0)));
        assert!(resolve(r#"{"crf": 60}"#, "f.mp4").is_err());
        assert!(resolve(r#"{"crf": 60}"#, "f.webm").is_ok());
        assert!(resolve(r#"{"crf": 20, "bitrate": 1000}"#, "f.mp4").is_err());
        assert!(resolve(r#"{"bitrate": 1000}"#, "f.gif").is_err());
        assert!(resolve(r#"{"nonsense": 1}"#, "f.mp4").is_err());
        let mb = ultrafast_graph::MotionBlur {
            samples: 6,
            shutter: 0.5,
            max_samples: Some(16),
        };
        assert_eq!(Quality::Draft.motion_blur(Some(mb)), None);
        assert_eq!(Quality::Standard.motion_blur(Some(mb)), Some(mb));
        let m = Quality::Master.motion_blur(Some(mb)).unwrap();
        assert_eq!((m.samples, m.max_samples, m.shutter), (12, Some(32), 0.5));
        assert_eq!(Quality::High.motion_blur(None), None);
    }

    #[test]
    fn frame_rates_and_gifs() {
        assert_eq!(resolve(r#"{"fps": 60}"#, "f.mp4").unwrap().fps, 60.0);
        assert!(resolve(r#"{"fps": 0}"#, "f.mp4").is_err());
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            s.resolve(Some(Path::new("f.gif")), &fmt(640, 360, 60.0)).unwrap().fps,
            30.0
        );
        assert!(resolve(r#"{"fps": 60}"#, "f.gif").is_err());
        assert_eq!(resolve(r#"{"loops": 0}"#, "f.gif").unwrap().loops, Some(0));
        assert!(resolve(r#"{"loops": 0}"#, "f.mp4").is_err());
        assert_eq!(parse_number("30000/1001"), Some(30000.0 / 1001.0));
    }

    #[test]
    fn sound() {
        let t = resolve(r#"{"audio": false}"#, "f.mp4").unwrap();
        assert_eq!(t.audio, None);
        assert!(resolve(r#"{"audio": false}"#, "f.mp3").is_err());
        assert!(resolve(r#"{"audio_codec": "opus"}"#, "f.mov").is_err());
        let t = resolve(r#"{"audio_codec": "pcm-24"}"#, "f.mov").unwrap();
        assert_eq!(
            t.audio,
            Some(AudioTarget {
                codec: AudioCodec::Pcm24,
                bitrate: None
            })
        );
        assert!(resolve(r#"{"codec": "h264"}"#, "f.wav").is_err());
        assert!(resolve(r#"{"audio_codec": "aac"}"#, "f.gif").is_err());
    }

    #[test]
    fn flags() {
        let args = [
            "--quality",
            "high",
            "--size",
            "4k",
            "--fps",
            "30000/1001",
            "--bitrate",
            "12M",
        ];
        let flag = |n: &str| args.iter().position(|a| *a == n).map(|i| args[i + 1].to_string());
        let s = Settings::from_flags(flag, |n| n == "--alpha").unwrap();
        assert_eq!(s.quality, Some(Quality::High));
        assert_eq!(s.size, Some(Size::Short(2160)));
        assert_eq!(s.bitrate, Some(Bitrate(12_000_000)));
        assert_eq!(s.alpha, Some(true));
        assert!(Settings::from_flags(|n| (n == "--codec").then(|| "x".into()), |_| false).is_err());
        // Round trip through JSON, as the API stores it.
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
    }
}
