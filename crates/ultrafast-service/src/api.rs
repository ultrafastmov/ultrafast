//! The v1 wire types (docs/service/API.md). Every fixture under
//! docs/service/fixtures parses into these types (see the tests), so the
//! contract and the code cannot drift apart.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
pub use ultrafast_live::output::{AudioCodec, Bitrate, Container, ProresProfile, Quality, Settings, Size, VideoCodec};

/// `POST /v1/render`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderRequest {
    /// A ultrafast document (parsed after its inputs are resolved).
    pub document: Value,
    /// Where the video goes (default: the server's storage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<Attempt>,
    /// RFC 3339 UTC (`...Z`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    /// Refuses a document its check warns about (a param its node doesn't
    /// take, a value of the wrong type): `400 invalid_document` with the
    /// warnings as `issues`, before anything is fetched or rendered.
    /// Without it they are the receipt's `warnings`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

/// Where the render goes and what it is. The encode settings are
/// `ultrafast export`'s (see `ultrafast_live::output::Settings`); every one
/// is optional, and the format follows `name`'s extension when it has one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// A bucket object to create (`gs://`, `s3://`, `r2://`): ultrafast
    /// Cloud's (this server answers `501 cloud_only`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The stored file's name (default `film.<format>`, or `frames.zip`
    /// for an image sequence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The file in the response body instead (small previews).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inline: bool,
    /// `mp4` (default), `mov`, `mkv`, `webm`, `gif`, `png` or `exr` (image
    /// sequences, as a zip), or sound alone: `wav`, `mp3`, `m4a`, `flac`,
    /// `opus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Container>,
    /// `h264` (default), `hevc`, `av1`, `vp9` or `prores`.
    #[serde(default)]
    pub codec: Option<VideoCodec>,
    /// `draft`, `standard` (default), `high` or `master`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<Quality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crf: Option<f64>,
    /// Bits per second (or `"8M"`), or null for the quality level's.
    #[serde(default)]
    pub bitrate: Option<Bitrate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prores_profile: Option<ProresProfile>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alpha: bool,
    /// `720p`, `1080p`, `4k`, `WxH` or `[w, h]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(default = "yes")]
    pub audio: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<AudioCodec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate: Option<Bitrate>,
    /// GIF repeats after the first play (0: once; default forever).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loops: Option<u32>,
    /// `[from, to]` seconds of the document.
    #[serde(default)]
    pub range: Option<[f64; 2]>,
}

fn yes() -> bool {
    true
}

impl Default for Output {
    fn default() -> Self {
        Output {
            uri: None,
            name: None,
            inline: false,
            format: None,
            codec: None,
            quality: None,
            crf: None,
            bitrate: None,
            prores_profile: None,
            alpha: false,
            size: None,
            scale: None,
            fps: None,
            audio: true,
            audio_codec: None,
            audio_bitrate: None,
            loops: None,
            range: None,
        }
    }
}

impl Output {
    /// The encode settings.
    pub fn settings(&self) -> Settings {
        Settings {
            format: self.format,
            codec: self.codec,
            quality: self.quality,
            crf: self.crf,
            bitrate: self.bitrate,
            prores_profile: self.prores_profile,
            alpha: self.alpha.then_some(true),
            size: self.size,
            scale: self.scale,
            fps: self.fps,
            audio: (!self.audio).then_some(false),
            audio_codec: self.audio_codec,
            audio_bitrate: self.audio_bitrate,
            loops: self.loops,
        }
    }

    /// An output with these encode settings.
    pub fn with(s: Settings) -> Self {
        Output {
            format: s.format,
            codec: s.codec,
            quality: s.quality,
            crf: s.crf,
            bitrate: s.bitrate,
            prores_profile: s.prores_profile,
            alpha: s.alpha.unwrap_or(false),
            size: s.size,
            scale: s.scale,
            fps: s.fps,
            audio: s.audio.unwrap_or(true),
            audio_codec: s.audio_codec,
            audio_bitrate: s.audio_bitrate,
            loops: s.loops,
            ..Output::default()
        }
    }

    /// The name the stored file goes by, when it gives one the settings
    /// can read a format from (a sequence's `.zip` gives none).
    pub fn named_path(&self) -> Option<&std::path::Path> {
        self.name
            .as_deref()
            .filter(|n| !n.to_ascii_lowercase().ends_with(".zip"))
            .map(std::path::Path::new)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub render_id: String,
    pub attempt_id: String,
}

/// `POST /v1/frame`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameRequest {
    pub document: Value,
    /// Seconds (`at` on the wire; `t` is accepted too).
    #[serde(default, alias = "at")]
    pub t: f64,
    #[serde(default)]
    pub format: ImageFormat,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

/// `POST /v1/sheet`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetRequest {
    pub document: Value,
    /// Frames spread evenly over the document (default 9).
    #[serde(default)]
    pub count: Option<usize>,
    /// Or exactly these times.
    #[serde(default)]
    pub at: Option<Vec<f64>>,
    #[serde(default)]
    pub columns: Option<usize>,
    #[serde(default)]
    pub format: ImageFormat,
}

/// `POST /v1/check`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRequest {
    pub document: Value,
}

/// `POST /v1/review`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub document: Value,
    /// `margin` (default) or `social`.
    #[serde(default)]
    pub safe: Option<String>,
    /// Seconds between text probes (default 0.1).
    #[serde(default)]
    pub step: Option<f64>,
}

/// `POST /v1/estimate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstimateRequest {
    pub document: Value,
    #[serde(default)]
    pub range: Option<[f64; 2]>,
    /// A render request's `output`, so it can be sent as is: priced at its
    /// size and timed with its quality level and frame rate.
    #[serde(default)]
    pub output: Option<Output>,
}

/// What a render produced and from what (`200`, and `409` replays).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub receipt_version: u32,
    /// The render's id (`attempt.render_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Where to download the video, and until when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_expires_at: Option<String>,
    /// Seconds, frames, bytes and sha256 (hex) of the video written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Rendered 1080p minutes (720p and below count half, above 1080p three).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
    /// `rendered`, or `already_rendered` on a replay.
    pub status: String,
    #[serde(default)]
    pub attempt: Option<Attempt>,
    #[serde(default)]
    pub object: Option<ObjectIdentity>,
    #[serde(default)]
    pub manifest: Option<ManifestRef>,
    pub video: VideoInfo,
    pub renderer: RendererInfo,
    pub document: DocumentInfo,
    #[serde(default)]
    pub inputs: Vec<InputRecord>,
    #[serde(default)]
    pub fonts: Vec<FontRecord>,
    #[serde(default)]
    pub timings: Option<Timings>,
    /// What the document's check warns about and what the renderer found
    /// while rendering (an image it could not read, text that never shows
    /// in full), one line each: ``node `title`: ...``.
    #[serde(default)]
    pub warnings: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectIdentity {
    pub uri: String,
    pub bucket: String,
    pub name: String,
    /// Decimal int64.
    pub generation: String,
    pub size: u64,
    /// Base64, as GCS reports it.
    pub crc32c: String,
    pub md5: String,
    /// Hex sha256 of the bytes.
    pub sha256: String,
    pub content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestRef {
    pub uri: String,
    pub generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoInfo {
    pub frames: u64,
    /// The output's frame rate and size.
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    /// `h264`, `hevc`, `av1`, `vp9`, `prores-<profile>`, `gif`, `png`,
    /// `exr`, or for sound alone its codec.
    pub codec: String,
    pub audio: bool,
    pub range: [f64; 2],
    /// The container (`mp4`, ..., `png` for a zipped sequence).
    #[serde(default = "mp4")]
    pub format: String,
    #[serde(default)]
    pub quality: Quality,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alpha: bool,
}

fn mp4() -> String {
    "mp4".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RendererInfo {
    pub service: String,
    pub version: String,
    pub git: String,
    pub image_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentInfo {
    pub sha256: String,
    pub format_version: u32,
    /// The document's `meta`, as declared (null without one).
    #[serde(default)]
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InputRecord {
    /// With `#generation`.
    pub uri: String,
    pub size: u64,
    pub bytes_read: u64,
    /// Only byte ranges were read.
    pub partial: bool,
    /// Archives: entries unpacked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FontRecord {
    pub family: String,
    pub weight: u32,
    pub style: String,
    pub uri: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timings {
    /// Only on an instance's first request.
    pub instance_start_s: Option<f64>,
    pub fetch_s: f64,
    pub unpack_s: f64,
    pub plan_s: f64,
    pub render_s: f64,
    pub encode_s: f64,
    pub upload_s: f64,
    pub total_s: f64,
}

/// `GET /v1/health`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Health {
    pub ok: bool,
    pub ready: bool,
    pub busy: bool,
    pub device: String,
    pub backend: String,
    pub version: String,
    pub git: String,
    pub image_digest: String,
    pub uptime_s: f64,
    /// Requests waiting for the GPU.
    #[serde(default)]
    pub queued: usize,
    /// The storage's scheme (`file`; ultrafast Cloud's are `s3`, `r2`, `gs`).
    #[serde(default)]
    pub storage: String,
}

/// Every error body: `{"error": {...}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorBody {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retry: bool,
    /// Per-code fields (`issues`, `location`, `object`, ...).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        let retry = matches!(status, 429 | 500 | 503);
        ApiError {
            status,
            code: code.to_string(),
            message: message.into(),
            retry,
            extra: Map::new(),
        }
    }

    pub fn with(mut self, key: &str, value: impl Serialize) -> Self {
        self.extra
            .insert(key.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
        self
    }

    /// A feature this server does not have: ultrafast Cloud's (`501`,
    /// `cloud_only`, `feature`).
    pub fn cloud_only(feature: &str, message: impl Into<String>) -> Self {
        ApiError::new(501, "cloud_only", message).with("feature", feature)
    }

    pub fn body(&self) -> String {
        serde_json::to_string(&ErrorBody { error: self.clone() }).unwrap_or_default()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// The canonical form hashed for `ultrafast-document-sha256` (see
/// [`ultrafast_graph::canonical_json`]).
pub fn canonical_json(v: &Value) -> String {
    ultrafast_graph::canonical_json(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/service/fixtures")
    }

    fn read(name: &str) -> Value {
        let p = fixtures().join(name);
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    /// Parses `name` into `T` and back, and checks nothing was lost.
    fn round_trip<T: Serialize + for<'de> Deserialize<'de>>(name: &str) -> T {
        let v = read(name);
        let t: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let back = serde_json::to_value(&t).unwrap();
        // Everything the fixture says comes back (fields it leaves out may
        // come back as their defaults; 30 and 30.0 are the same number).
        fn covers(fixture: &Value, back: &Value, at: &str) {
            match (fixture, back) {
                (Value::Object(f), Value::Object(b)) => {
                    for (k, v) in f {
                        covers(v, b.get(k).unwrap_or(&Value::Null), &format!("{at}.{k}"));
                    }
                }
                (Value::Array(f), Value::Array(b)) => {
                    assert_eq!(f.len(), b.len(), "{at}");
                    for (i, (x, y)) in f.iter().zip(b).enumerate() {
                        covers(x, y, &format!("{at}[{i}]"));
                    }
                }
                (Value::Number(f), Value::Number(b)) => assert_eq!(f.as_f64(), b.as_f64(), "{at}"),
                _ => assert_eq!(fixture, back, "{at}"),
            }
        }
        covers(&v, &back, name);
        t
    }

    #[test]
    fn requests_parse() {
        let r: RenderRequest = round_trip("render-request.json");
        let o = r.output.as_ref().unwrap();
        assert_eq!(o.codec, Some(VideoCodec::H264));
        assert!(o.uri.as_deref().unwrap().starts_with("gs://"));
        assert!(r.attempt.is_some() && r.deadline.is_some());
        let r: RenderRequest = round_trip("render-request-inline.json");
        let o = r.output.as_ref().unwrap();
        assert!(o.inline && o.uri.is_none());
        assert_eq!((o.quality, o.size), (Some(Quality::Draft), Some(Size::Short(720))));
        // A transparent ProRes master at 4K and 60 fps, resolved as `ultrafast
        // export` would.
        let r: RenderRequest = round_trip("render-request-master.json");
        let o = r.output.as_ref().unwrap();
        let doc = ultrafast_graph::Document::from_json(&r.document.to_string()).unwrap();
        let t = o.settings().resolve(o.named_path(), &doc.format).unwrap();
        assert_eq!(t.container, Container::Mov);
        assert_eq!(t.codec_name(), "prores-4444");
        assert_eq!((t.width, t.height, t.fps), (2160, 3840, 60.0));
        assert!(t.video.unwrap().alpha);
        assert_eq!(t.audio.unwrap().codec, AudioCodec::Pcm24);
        // Settings that don't go together are refused before anything runs.
        let bad: Output = serde_json::from_value(serde_json::json!({"alpha": true, "codec": "h264"})).unwrap();
        assert!(bad.settings().resolve(None, &doc.format).is_err());
        assert!(serde_json::from_value::<Output>(serde_json::json!({"quality": "ultra"})).is_err());
        let f: FrameRequest = round_trip("frame-request.json");
        assert_eq!(f.format, ImageFormat::Png);
        // The site's forms: `at` for `t`, no `output` at all.
        let f: FrameRequest = serde_json::from_value(serde_json::json!({"document": {}, "at": 2.5})).unwrap();
        assert_eq!(f.t, 2.5);
        let r: RenderRequest = serde_json::from_value(serde_json::json!({"document": {}})).unwrap();
        assert!(r.output.is_none());
        assert!(Output::default().audio);
        let _: SheetRequest = round_trip("sheet-request.json");
        let _: EstimateRequest = round_trip("estimate-request.json");
        let _: ReviewRequest = round_trip("review-request.json");
    }

    #[test]
    fn misspelled_fields_are_refused() {
        let mut v = read("render-request.json");
        v["outptu"] = v["output"].clone();
        assert!(serde_json::from_value::<RenderRequest>(v).is_err());
    }

    #[test]
    fn receipts_parse() {
        let r: Receipt = round_trip("receipt.json");
        assert_eq!(r.status, "rendered");
        let o = r.object.unwrap();
        assert_eq!(o.sha256.len(), 64);
        let r: Receipt = round_trip("receipt-already-rendered.json");
        assert_eq!(r.status, "already_rendered");
        let r: Receipt = round_trip("receipt-stored.json");
        assert!(r.url.is_some() && r.sha256.is_some() && r.minutes.is_some());
        let _: Health = round_trip("health.json");
    }

    #[test]
    fn errors_parse() {
        let dir = fixtures().join("errors");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let name = format!("errors/{}", p.file_name().unwrap().to_string_lossy());
            let b: ErrorBody = round_trip(&name);
            let status: u16 = p.file_name().unwrap().to_string_lossy()[..3].parse().unwrap();
            assert_eq!(b.error.status, status, "{name}");
            n += 1;
        }
        assert!(n >= 12);
    }

    #[test]
    fn object_metadata_fits_and_hashes_match() {
        let m = read("object-metadata.json");
        let meta = m["metadata"].as_object().unwrap();
        let bytes: usize = meta.iter().map(|(k, v)| k.len() + v.as_str().unwrap().len()).sum();
        assert!(bytes < 2048, "{bytes} bytes of metadata");
        // The document hash in the metadata is the canonical hash of the
        // fixture request's document.
        let req = read("render-request.json");
        let sha = {
            use sha2::Digest;
            let d = sha2::Sha256::digest(canonical_json(&req["document"]).as_bytes());
            d.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        assert_eq!(meta["ultrafast-document-sha256"].as_str().unwrap(), sha);
        let receipt = read("receipt.json");
        assert_eq!(receipt["document"]["sha256"].as_str().unwrap(), sha);
    }

    /// The canonical text and hash of a document with awkward numbers and
    /// strings: what a composer must send byte for byte. `UPDATE_FIXTURES=1`
    /// rewrites the expected files.
    #[test]
    fn canonical_fixture() {
        let dir = fixtures().join("canonical");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("document.json")).unwrap()).unwrap();
        let text = canonical_json(&v);
        let sha = {
            use sha2::Digest;
            sha2::Sha256::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        if std::env::var("UPDATE_FIXTURES").is_ok() {
            std::fs::write(dir.join("canonical.json"), &text).unwrap();
            std::fs::write(dir.join("sha256.txt"), format!("{sha}\n")).unwrap();
        }
        assert_eq!(std::fs::read_to_string(dir.join("canonical.json")).unwrap(), text);
        assert_eq!(std::fs::read_to_string(dir.join("sha256.txt")).unwrap().trim(), sha);
        // A fixed point: the canonical text re-read is the same text.
        let again: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(canonical_json(&again), text);
        // And it is a document ultrafast renders.
        let doc = ultrafast_graph::Document::from_json(&text).unwrap();
        assert!(
            ultrafast_render::schema::check(&doc)
                .iter()
                .all(|i| i.severity != "error")
        );
    }

    #[test]
    fn fixture_document_is_a_valid_document() {
        let req = read("render-request.json");
        let doc = ultrafast_graph::Document::from_json(&req["document"].to_string()).unwrap();
        let errors: Vec<_> = ultrafast_render::schema::check(&doc)
            .into_iter()
            .filter(|i| i.severity == "error")
            .map(|i| format!("{}: {}", i.node, i.message))
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }
}
