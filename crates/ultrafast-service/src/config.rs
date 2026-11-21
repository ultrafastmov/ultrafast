//! Settings from the environment. docs/service/SELF_HOST.md describes each.
//!
//!   PORT / ULTRAFAST_BIND   listen port (8080) and address (0.0.0.0)
//!   ULTRAFAST_DATA          the server's own directory: local storage, its secret and
//!                           generated key ($XDG_DATA_HOME/ultrafast)
//!   ULTRAFAST_STORAGE       where renders and uploads go: file:///DIR or a path
//!                           ($ULTRAFAST_DATA/storage)
//!   ULTRAFAST_PUBLIC_URL    the server's URL as clients see it (else from Host)
//!   ULTRAFAST_API_KEY       the API key callers send (else one is generated and kept
//!                           in $ULTRAFAST_DATA/api-key)
//!   ULTRAFAST_AUTH          auto (default): the API key; none: no auth (caller "local")
//!   ULTRAFAST_HTTPS_HOSTS   if set, the only hosts https:// inputs may come from
//!   ULTRAFAST_SECRET        signs download URLs (else generated)
//!   ULTRAFAST_QUEUE         requests that may wait for the GPU (16)
//!   ULTRAFAST_USD_PER_MINUTE  price for /v1/estimate (none)
//!   ULTRAFAST_URL_TTL_HOURS   signed URL lifetime (168)
//!   ULTRAFAST_MAX_DURATION_S, ULTRAFAST_MAX_UPLOAD_GB
//!   ULTRAFAST_INPUT_CACHE       input cache directory ($TMPDIR/ultrafast-inputs)
//!   ULTRAFAST_INPUT_CACHE_GB    its size bound (8)
//!   ULTRAFAST_MAX_INPUT_GB      bytes a request may download (4)
//!   ULTRAFAST_IMAGE_DIGEST      the image's digest, for receipts
//!   ULTRAFAST_PARTIAL_MIN_MB    media this large is read in part (32)
//!
//! Several keys, OIDC sign-in, bucket storage and grants are ultrafast
//! Cloud's; their settings are refused here rather than ignored.

use std::path::PathBuf;

/// Whose uploads a request may read: its caller's.
#[derive(Debug, Clone, Default)]
pub struct Grants {
    pub caller: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// The API key.
    Key,
    None,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub body_bytes: usize,
    pub document_bytes: usize,
    pub duration_s: f64,
    pub side: u32,
    pub pixels: u64,
    pub fps: f64,
    pub input_bytes: u64,
    pub archive_entries: usize,
    pub archive_bytes: u64,
    pub archive_ratio: u64,
    pub wall_s: f64,
    pub inline_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            body_bytes: 8 << 20,
            document_bytes: 5 << 20,
            duration_s: 3.0 * 3600.0,
            side: 3840,
            pixels: 8_294_400,
            fps: 60.0,
            input_bytes: 4 << 30,
            archive_entries: 20_000,
            archive_bytes: 2 << 30,
            archive_ratio: 200,
            wall_s: 600.0,
            inline_bytes: 32 << 20,
        }
    }
}

/// The caller id the API key stands for.
pub const KEY_CALLER: &str = "default";

/// Settings this server refuses: they configure ultrafast Cloud.
const CLOUD_SETTINGS: &[&str] = &[
    "ULTRAFAST_API_KEYS_FILE",
    "ULTRAFAST_OIDC_ISSUER",
    "ULTRAFAST_OIDC_AUDIENCE",
    "ULTRAFAST_OIDC_ALLOW",
    "ULTRAFAST_CALLERS",
    "ULTRAFAST_READ",
    "ULTRAFAST_WRITE",
    "ULTRAFAST_WEBHOOK_SECRET",
];

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub bind: String,
    pub auth: Auth,
    /// The API key's sha256 (hex).
    pub key_sha256: Option<String>,
    /// A key made because none was configured (printed at start).
    pub generated_key: Option<String>,
    /// Empty: any public host.
    pub https_hosts: Vec<String>,
    pub data_dir: PathBuf,
    pub storage: crate::store::Storage,
    pub public_url: Option<String>,
    /// Signs download URLs.
    pub secret: Vec<u8>,
    pub queue: usize,
    pub usd_per_minute: Option<f64>,
    pub url_ttl: std::time::Duration,
    pub max_upload_bytes: u64,
    pub cache_dir: PathBuf,
    pub cache_bytes: u64,
    pub limits: Limits,
    pub image_digest: String,
    /// Media at least this large is read in part when a render uses only
    /// windows of it.
    pub partial_min: u64,
    /// Reads larger than this go in parts of this size.
    pub part_bytes: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let list = |k: &str| -> Vec<String> {
            env(k)
                .map(|v| {
                    v.split([',', ' ', '\n'])
                        .map(str::trim)
                        .filter(|x| !x.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        if let Some(k) = CLOUD_SETTINGS.iter().find(|k| env(k).is_some()) {
            anyhow::bail!(
                "{k} configures ultrafast Cloud (several keys, OIDC sign-in, bucket grants and webhooks); \
                 this server takes one API key (ULTRAFAST_API_KEY) and keeps its files on local disk"
            );
        }
        // The key: ULTRAFAST_API_KEY, or ULTRAFAST_API_KEYS holding one.
        let mut key = env("ULTRAFAST_API_KEY").map(|k| k.trim().to_string());
        if key.is_none() {
            let keys = list("ULTRAFAST_API_KEYS");
            if keys.len() > 1 {
                anyhow::bail!(
                    "ULTRAFAST_API_KEYS has {} keys: this server takes one (ULTRAFAST_API_KEY); \
                     several keys with their own limits are an ultrafast Cloud feature",
                    keys.len()
                );
            }
            key = keys
                .into_iter()
                .next()
                .map(|k| k.split_once('=').map_or(k.clone(), |(_, v)| v.to_string()));
        }
        if let Some(k) = &key
            && k.len() < 16
        {
            anyhow::bail!("ULTRAFAST_API_KEY: keys need at least 16 characters");
        }
        let data_dir = env("ULTRAFAST_DATA")
            .map(PathBuf::from)
            .or_else(|| env("XDG_DATA_HOME").map(|d| PathBuf::from(d).join("ultrafast")))
            .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/share/ultrafast")))
            .unwrap_or_else(|| PathBuf::from("ultrafast-data"));
        let storage = match env("ULTRAFAST_STORAGE") {
            Some(s) => crate::store::Storage::parse(&s)?,
            None => crate::store::Storage::parse(&data_dir.join("storage").to_string_lossy())?,
        };
        let mut generated_key = None;
        let auth = match env("ULTRAFAST_AUTH").as_deref() {
            None | Some("auto") | Some("key") | Some("keys") => {
                if key.is_none() {
                    let k = load_or_make(&data_dir.join("api-key"), || {
                        format!("uf_live_{}", crate::util::random_base62(32))
                    })?;
                    generated_key = Some(k.clone());
                    key = Some(k);
                }
                Auth::Key
            }
            Some("none") => Auth::None,
            Some(other @ ("oidc" | "cloudrun")) => {
                anyhow::bail!("ULTRAFAST_AUTH: `{other}` sign-in is an ultrafast Cloud feature (auto | none)")
            }
            Some(other) => anyhow::bail!("ULTRAFAST_AUTH: `{other}` (auto | none)"),
        };
        let secret = match env("ULTRAFAST_SECRET") {
            Some(s) => s.into_bytes(),
            None => load_or_make(&data_dir.join("secret"), || {
                crate::util::hex(&crate::util::random_bytes(32))
            })?
            .into_bytes(),
        };
        let gb = |k: &str, d: u64| {
            env(k)
                .and_then(|v| v.parse::<f64>().ok())
                .map_or(d << 30, |g| (g * (1u64 << 30) as f64) as u64)
        };
        let num = |k: &str| env(k).and_then(|v| v.parse::<f64>().ok());
        let mut limits = Limits {
            input_bytes: gb("ULTRAFAST_MAX_INPUT_GB", 4),
            ..Limits::default()
        };
        if let Some(d) = num("ULTRAFAST_MAX_DURATION_S") {
            limits.duration_s = d;
        }
        Ok(Config {
            port: env("PORT").and_then(|p| p.parse().ok()).unwrap_or(8080),
            bind: env("ULTRAFAST_BIND").unwrap_or_else(|| "0.0.0.0".into()),
            auth,
            key_sha256: key.map(|k| crate::util::sha256_hex(k.as_bytes())),
            generated_key,
            https_hosts: list("ULTRAFAST_HTTPS_HOSTS")
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            data_dir,
            storage,
            public_url: env("ULTRAFAST_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string()),
            secret,
            queue: env("ULTRAFAST_QUEUE").and_then(|v| v.parse().ok()).unwrap_or(16),
            usd_per_minute: num("ULTRAFAST_USD_PER_MINUTE"),
            url_ttl: std::time::Duration::from_secs_f64(
                num("ULTRAFAST_URL_TTL_HOURS").unwrap_or(168.0).max(0.01) * 3600.0,
            ),
            max_upload_bytes: gb("ULTRAFAST_MAX_UPLOAD_GB", 20),
            // Wiped at start: one per port, so a restart reuses it and two
            // servers on one machine keep apart.
            cache_dir: env("ULTRAFAST_INPUT_CACHE").map(PathBuf::from).unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "ultrafast-inputs-{}",
                    env("PORT").unwrap_or_else(|| "8080".into())
                ))
            }),
            cache_bytes: gb("ULTRAFAST_INPUT_CACHE_GB", 8),
            limits,
            image_digest: env("ULTRAFAST_IMAGE_DIGEST").unwrap_or_else(|| "unknown".into()),
            partial_min: env("ULTRAFAST_PARTIAL_MIN_MB")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(32)
                << 20,
            part_bytes: env("ULTRAFAST_PART_KB")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&v| v >= 64)
                .unwrap_or(8 << 10)
                << 10,
        })
    }

    /// Whether https inputs may come from `host`.
    pub fn https_allowed(&self, host: &str) -> bool {
        self.https_hosts.is_empty() || self.https_hosts.iter().any(|h| h == host)
    }

    /// A caller's grants: its own uploads.
    pub fn grants_for(&self, who: &str) -> Grants {
        Grants {
            caller: who.to_string(),
        }
    }
}

impl Config {
    /// Settings for tests and embedding: no auth (caller `local`), storage
    /// and data in `dir`, nothing read from the environment.
    pub fn for_tests(dir: PathBuf) -> Config {
        Config {
            port: 0,
            bind: "127.0.0.1".into(),
            auth: Auth::None,
            key_sha256: None,
            generated_key: None,
            https_hosts: Vec::new(),
            storage: crate::store::Storage {
                scheme: "file".into(),
                bucket: String::new(),
                prefix: String::new(),
                dir: Some(dir.join("storage")),
            },
            data_dir: dir.clone(),
            public_url: None,
            secret: b"test-secret".to_vec(),
            queue: 16,
            usd_per_minute: None,
            url_ttl: std::time::Duration::from_secs(3600),
            max_upload_bytes: 1 << 30,
            cache_dir: dir.join("cache"),
            cache_bytes: 1 << 30,
            limits: Limits::default(),
            image_digest: "test".into(),
            partial_min: 32 << 20,
            part_bytes: 8 << 20,
        }
    }
}

/// The contents of `path`, or a new value made by `make` and saved there
/// (readable by this user only).
fn load_or_make(path: &std::path::Path, make: impl FnOnce() -> String) -> anyhow::Result<String> {
    if let Ok(s) = std::fs::read_to_string(path)
        && !s.trim().is_empty()
    {
        return Ok(s.trim().to_string());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    }
    let v = make();
    let mut f = {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(path)
            .map_err(|e| anyhow::anyhow!("creating {}: {e}", path.display()))?
    };
    std::io::Write::write_all(&mut f, format!("{v}\n").as_bytes())?;
    Ok(v)
}
