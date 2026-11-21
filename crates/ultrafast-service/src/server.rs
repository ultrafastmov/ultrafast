//! The HTTP server behind `ultrafast serve`: routes, authentication, the
//! GPU queue, uploads, signed downloads, and mounts for routes other crates
//! serve (the remote MCP server). One machine, one API key, local disk; jobs
//! with webhooks, bucket storage and sign-in are ultrafast Cloud's and answer
//! `501 cloud_only` here.
//!
//! Threads: one accepts connections and hands each request to a thread of
//! its own; one owns the GPU and the [`Engine`] and runs work in turn.

use crate::api::*;
use crate::auth::{Authenticator, Caller};
use crate::config::Config;
use crate::job::{Ctx, Engine, Reply};
use crate::records::{self, AssetRecord};
use crate::store::{Created, Storage, Store, Stores};
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tiny_http::{Header, Method, Request, Response, Server};

/// A request in plain types, for mounts.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    /// Without the `?`.
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Routes another crate serves on this server (the remote MCP server and
/// its OAuth endpoints). A mount authenticates its own requests (see
/// [`Handle::authenticate`]); they reach it before the server's auth.
pub trait Mount: Send + Sync {
    fn claims(&self, method: &str, path: &str) -> bool;
    fn handle(&self, req: &HttpRequest, svc: &Handle) -> HttpResponse;
}

type Work = Box<dyn FnOnce(&mut Engine) + Send>;

#[derive(Default)]
struct Lanes {
    /// Requests (and mounts' GPU work), in the order they came.
    interactive: VecDeque<Work>,
}

struct Shared {
    cfg: Config,
    auth: Authenticator,
    /// The HTTP side's stores (the engine has its own, with deadlines).
    stores: Stores,
    started: Instant,
    ready: AtomicBool,
    failed: AtomicBool,
    busy: AtomicBool,
    device: Mutex<String>,
    lanes: Mutex<Lanes>,
    wake: Condvar,
    mounts: Vec<Arc<dyn Mount>>,
    open: AtomicUsize,
}

/// The server as other code sees it: authenticate a token, run work on the
/// GPU with a document's inputs resolved, render to storage.
#[derive(Clone)]
pub struct Handle(Arc<Shared>);

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(400, "bad_request", message)
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(404, "not_found", format!("no {what}"))
}

/// Visible ASCII only (header values).
fn ascii(v: &str) -> String {
    v.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect()
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), ascii(v).as_bytes()).expect("ascii header")
}

type Body = Response<Box<dyn Read + Send>>;

fn respond_bytes(status: u16, content_type: &str, body: Vec<u8>, extra: &[(String, String)]) -> Body {
    let n = body.len();
    // A known length goes out as Content-Length (not chunked), so clients
    // see the size.
    let mut r = Response::new(
        status.into(),
        vec![header("Content-Type", content_type)],
        Box::new(std::io::Cursor::new(body)) as Box<dyn Read + Send>,
        Some(n),
        None,
    )
    .with_chunked_threshold(usize::MAX);
    for (k, v) in extra {
        r = r.with_header(header(k, v));
    }
    r
}

fn json(status: u16, v: &Value) -> Body {
    respond_bytes(status, "application/json", v.to_string().into_bytes(), &[])
}

fn error(e: &ApiError) -> Body {
    let mut extra = Vec::new();
    if e.status == 401 {
        extra.push(("WWW-Authenticate".to_string(), "Bearer realm=\"ultrafast\"".to_string()));
    }
    if e.retry && matches!(e.status, 429 | 503) {
        extra.push(("Retry-After".to_string(), "2".to_string()));
    }
    respond_bytes(e.status, "application/json", e.body().into_bytes(), &extra)
}

fn reply(r: Result<Reply, ApiError>) -> Body {
    match r {
        Ok(Reply::Json(s, body)) => respond_bytes(s, "application/json", body.into_bytes(), &[]),
        Ok(Reply::Bytes {
            content_type,
            body,
            headers,
        }) => respond_bytes(200, content_type, body, &headers),
        Err(e) => error(&e),
    }
}

/// One structured log line (Cloud Logging reads `severity` and `message`).
fn log(fields: Value) {
    eprintln!("{fields}");
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| crate::refs::decode(&v.replace('+', " ")).unwrap_or_default())
    })
}

/// The base URL clients reach this server at.
fn base_url(cfg: &Config, headers: &[(String, String)]) -> String {
    if let Some(u) = &cfg.public_url {
        return u.clone();
    }
    let get = |k: &str| {
        headers
            .iter()
            .find(|(h, _)| h.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.split(',').next().unwrap_or("").trim().to_string())
    };
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .unwrap_or_else(|| format!("127.0.0.1:{}", cfg.port));
    let proto = get("x-forwarded-proto").unwrap_or_else(|| "http".into());
    let proto = if proto == "https" { "https" } else { "http" };
    format!("{proto}://{}", ascii(&host).replace(['/', ' ', '?', '#'], ""))
}

impl Shared {
    fn queued(&self) -> usize {
        self.lanes.lock().unwrap().interactive.len()
    }

    /// Runs `f` on the GPU thread and waits for it.
    fn on_gpu<T: Send + 'static>(&self, f: impl FnOnce(&mut Engine) -> T + Send + 'static) -> Result<T, ApiError> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(ApiError::new(503, "unavailable", "the GPU could not be initialized"));
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        {
            let mut lanes = self.lanes.lock().unwrap();
            if lanes.interactive.len() >= self.cfg.queue {
                return Err(ApiError::new(
                    429,
                    "busy",
                    format!("{} requests are already waiting for the GPU", lanes.interactive.len()),
                )
                .with("limit", "queue"));
            }
            lanes.interactive.push_back(Box::new(move |e: &mut Engine| {
                let _ = tx.send(f(e));
            }));
        }
        self.wake.notify_one();
        rx.recv()
            .map_err(|_| ApiError::new(503, "unavailable", "the renderer stopped"))
    }

    fn main_store(&self) -> &dyn Store {
        self.stores.main(&self.cfg.storage)
    }

    fn storage(&self) -> &Storage {
        &self.cfg.storage
    }
}

// ---------------------------------------------------------------- uploads

/// Streams the part named `file` (or the first part with a file name) of a
/// multipart body into `out`; returns the part's file name and type.
pub fn read_multipart(
    body: &mut dyn Read,
    boundary: &str,
    out: &mut dyn Write,
    limit: u64,
) -> Result<(Option<String>, Option<String>, u64), String> {
    let delim = format!("\r\n--{boundary}").into_bytes();
    let mut buf: Vec<u8> = b"\r\n".to_vec();
    let mut chunk = vec![0u8; 256 * 1024];
    let mut eof = false;
    let mut fill = |buf: &mut Vec<u8>, eof: &mut bool| -> Result<(), String> {
        if *eof {
            return Ok(());
        }
        let n = body.read(&mut chunk).map_err(|e| format!("reading the upload: {e}"))?;
        if n == 0 {
            *eof = true;
        }
        buf.extend_from_slice(&chunk[..n]);
        Ok(())
    };
    let find = |hay: &[u8], needle: &[u8], from: usize| -> Option<usize> {
        let first = needle[0];
        let mut i = from;
        while i + needle.len() <= hay.len() {
            let k = hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first)?;
            let at = i + k;
            if &hay[at..at + needle.len()] == needle {
                return Some(at);
            }
            i = at + 1;
        }
        None
    };
    // The first delimiter (a preamble before it is ignored).
    loop {
        if let Some(at) = find(&buf, &delim, 0) {
            buf.drain(..at + delim.len());
            break;
        }
        if eof {
            return Err("not a multipart body (no boundary)".into());
        }
        let keep = buf.len().saturating_sub(delim.len());
        buf.drain(..keep);
        fill(&mut buf, &mut eof)?;
    }
    loop {
        // After a delimiter: `--` ends the body, CRLF starts a part.
        while buf.len() < 2 && !eof {
            fill(&mut buf, &mut eof)?;
        }
        if buf.starts_with(b"--") {
            return Err("the upload has no `file` part".into());
        }
        // Headers.
        let headers_end = loop {
            if let Some(at) = find(&buf, b"\r\n\r\n", 0) {
                break at;
            }
            if buf.len() > 16 * 1024 || eof {
                return Err("bad multipart part headers".into());
            }
            fill(&mut buf, &mut eof)?;
        };
        let head = String::from_utf8_lossy(&buf[..headers_end]).to_string();
        buf.drain(..headers_end + 4);
        let mut name = None;
        let mut filename = None;
        let mut ctype = None;
        for line in head.split("\r\n") {
            let Some((k, v)) = line.split_once(':') else { continue };
            let k = k.trim().to_ascii_lowercase();
            if k == "content-disposition" {
                for p in v.split(';').map(str::trim) {
                    if let Some(x) = p.strip_prefix("name=") {
                        name = Some(x.trim_matches('"').to_string());
                    } else if let Some(x) = p.strip_prefix("filename=") {
                        filename = Some(x.trim_matches('"').to_string());
                    }
                }
            } else if k == "content-type" {
                ctype = Some(v.trim().to_string());
            }
        }
        let wanted = name.as_deref() == Some("file") || filename.is_some();
        // The part's content, up to the next delimiter.
        let mut written = 0u64;
        loop {
            if let Some(at) = find(&buf, &delim, 0) {
                if wanted {
                    written += at as u64;
                    if written > limit {
                        return Err(format!("over_limit:{written}"));
                    }
                    out.write_all(&buf[..at]).map_err(|e| e.to_string())?;
                }
                buf.drain(..at + delim.len());
                break;
            }
            if eof {
                return Err("the upload ended early".into());
            }
            let keep = buf.len().saturating_sub(delim.len());
            if wanted && keep > 0 {
                written += keep as u64;
                if written > limit {
                    return Err(format!("over_limit:{written}"));
                }
                out.write_all(&buf[..keep]).map_err(|e| e.to_string())?;
            }
            buf.drain(..keep);
            fill(&mut buf, &mut eof)?;
        }
        if wanted {
            return Ok((filename, ctype, written));
        }
    }
}

/// A writer that hashes what passes through it.
struct Hashing<W: Write> {
    inner: W,
    sha: sha2::Sha256,
    md5: md5::Md5,
    crc: u32,
    n: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        use md5::Digest as _;
        let k = self.inner.write(b)?;
        sha2::Digest::update(&mut self.sha, &b[..k]);
        self.md5.update(&b[..k]);
        self.crc = crc32c::crc32c_append(self.crc, &b[..k]);
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Shared {
    fn upload(&self, caller: &Caller, req: &mut Request, query: &str) -> Result<AssetRecord, ApiError> {
        let headers = req_headers(req);
        let get = |k: &str| {
            headers
                .iter()
                .find(|(h, _)| h.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.clone())
        };
        let ctype = get("content-type").unwrap_or_default();
        let limit = self.cfg.max_upload_bytes;
        if let Some(len) = get("content-length").and_then(|v| v.parse::<u64>().ok())
            && len > limit + 64 * 1024
        {
            return Err(over_upload(len, limit));
        }
        let tmp_dir = self.cfg.data_dir.join("uploads-tmp");
        std::fs::create_dir_all(&tmp_dir).map_err(|e| ApiError::new(503, "unavailable", e.to_string()))?;
        let tmp = tmp_dir.join(format!("{}-{}", std::process::id(), crate::util::unique()));
        struct Rm(PathBuf);
        impl Drop for Rm {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _rm = Rm(tmp.clone());
        let file = std::fs::File::create(&tmp).map_err(|e| ApiError::new(503, "unavailable", e.to_string()))?;
        let mut w = Hashing {
            inner: std::io::BufWriter::with_capacity(1 << 20, file),
            sha: <sha2::Sha256 as sha2::Digest>::new(),
            md5: <md5::Md5 as md5::Digest>::new(),
            crc: 0,
            n: 0,
        };
        let (filename, part_type) = if ctype.to_ascii_lowercase().starts_with("multipart/form-data") {
            let boundary = ctype
                .split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("boundary="))
                .map(|b| b.trim_matches('"').to_string())
                .filter(|b| !b.is_empty() && b.len() <= 200)
                .ok_or_else(|| bad("multipart upload without a boundary"))?;
            let (f, t, _) = read_multipart(req.as_reader(), &boundary, &mut w, limit).map_err(|e| {
                match e.strip_prefix("over_limit:") {
                    Some(n) => over_upload(n.parse().unwrap_or(0), limit),
                    None => bad(e),
                }
            })?;
            (f, t)
        } else {
            let mut r = req.as_reader().take(limit + 1);
            std::io::copy(&mut r, &mut w).map_err(|e| bad(format!("reading the upload: {e}")))?;
            if w.n > limit {
                return Err(over_upload(w.n, limit));
            }
            (
                query_param(query, "filename").or_else(|| get("x-filename")),
                (!ctype.is_empty()).then_some(ctype.clone()),
            )
        };
        w.flush()
            .map_err(|e| ApiError::new(503, "unavailable", e.to_string()))?;
        let size = w.n;
        if size == 0 {
            return Err(bad("the upload is empty"));
        }
        let hashes = crate::util::FileHashes {
            crc32c: crate::util::b64(&w.crc.to_be_bytes()),
            md5: crate::util::b64(&md5::Digest::finalize(w.md5)),
            sha256: crate::util::hex(&sha2::Digest::finalize(w.sha)),
            size,
        };
        drop(w.inner);
        let filename = records::clean_filename(filename.as_deref().unwrap_or("upload"));
        let content_type = part_type
            .filter(|t| !t.is_empty() && t != "application/octet-stream")
            .unwrap_or_else(|| records::content_type_for(&filename).to_string());
        let raw = crate::util::random_base62(22);
        let storage = self.storage();
        let object = storage.name(&format!("assets/{raw}/{filename}"));
        let store = self.main_store();
        let meta: std::collections::BTreeMap<String, String> = [
            ("ultrafast-asset".to_string(), format!("asset_{raw}")),
            ("ultrafast-sha256".to_string(), hashes.sha256.clone()),
        ]
        .into();
        let obj = match store
            .create(&storage.bucket, &object, &tmp, &content_type, &meta, &hashes)
            .map_err(|e| records::store_down(store, &e))?
        {
            Created::Done(o) => o,
            Created::Exists => return Err(ApiError::new(503, "unavailable", "asset id collision; retry")),
        };
        let rec = AssetRecord {
            id: format!("asset_{raw}"),
            uri: format!("asset://{raw}"),
            bytes: size,
            sha256: hashes.sha256,
            filename,
            content_type,
            created_at: crate::util::now_rfc3339(),
            owner: caller.id.clone(),
            object,
            generation: obj.generation,
        };
        store
            .create_bytes(
                &storage.bucket,
                &records::asset_record_name(storage, &raw),
                &serde_json::to_vec(&rec).unwrap_or_default(),
                "application/json",
            )
            .map_err(|e| records::store_down(store, &e))?;
        Ok(rec)
    }
}

fn over_upload(value: u64, max: u64) -> ApiError {
    ApiError::new(413, "over_limit", "the upload is too large")
        .with("limit", "upload_bytes")
        .with("value", value)
        .with("max", max)
}

// ---------------------------------------------------------------- files

impl Shared {
    /// `GET /v1/files/REL?expires=…&sig=…`, with byte ranges.
    fn file(&self, rel_enc: &str, query: &str, headers: &[(String, String)], head: bool) -> Body {
        let rel = match crate::refs::decode(rel_enc) {
            Ok(r) => r,
            Err(e) => return error(&bad(e)),
        };
        let (Some(expires), Some(sig)) = (query_param(query, "expires"), query_param(query, "sig")) else {
            return error(&ApiError::new(
                401,
                "unauthorized",
                "a signed URL needs `expires` and `sig`",
            ));
        };
        if !crate::auth::verify_file(&self.cfg.secret, &rel, &expires, &sig) {
            return error(&ApiError::new(
                401,
                "unauthorized",
                "the signature is wrong or has expired",
            ));
        }
        let storage = self.storage();
        let name = storage.name(&rel);
        let store = self.main_store();
        let meta = match store.stat(&storage.bucket, &name, None) {
            Ok(Some(m)) => m,
            Ok(None) => return error(&not_found("such file")),
            Err(e) => return error(&records::store_down(store, &e)),
        };
        let size = meta.size;
        let range = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("range"))
            .and_then(|(_, v)| parse_range(v, size));
        let (status, from, to) = match range {
            Some((a, b)) => (206u16, a, b),
            None => (200u16, 0, size),
        };
        let ct = meta
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into());
        let mut extra = vec![
            ("Accept-Ranges".to_string(), "bytes".to_string()),
            ("Cache-Control".to_string(), "private, max-age=3600".to_string()),
            (
                "Content-Disposition".to_string(),
                format!("inline; filename=\"{}\"", records::clean_filename(&rel)),
            ),
        ];
        if status == 206 {
            extra.push((
                "Content-Range".into(),
                format!("bytes {from}-{}/{size}", to.saturating_sub(1)),
            ));
        }
        let reader: Box<dyn Read + Send> = if head {
            Box::new(std::io::empty())
        } else {
            match self.stores.local.path(&name).and_then(|p| Ok(std::fs::File::open(p)?)) {
                Ok(mut f) => {
                    use std::io::Seek;
                    let _ = f.seek(std::io::SeekFrom::Start(from));
                    Box::new(f.take(to - from))
                }
                Err(e) => return error(&ApiError::new(503, "unavailable", e.to_string())),
            }
        };
        let len = (to - from) as usize;
        let mut r = Response::new(
            status.into(),
            vec![header("Content-Type", &ct)],
            reader,
            Some(len),
            None,
        )
        .with_chunked_threshold(usize::MAX);
        for (k, v) in extra {
            r = r.with_header(header(&k, &v));
        }
        r
    }
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` → `[from, to)`.
fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let spec = v.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (from, to) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.saturating_sub(n), size)
        }
        (a, "") => (a.parse().ok()?, size),
        (a, b) => (a.parse().ok()?, (b.parse::<u64>().ok()? + 1).min(size)),
    };
    (from < to && from < size).then_some((from, to))
}

// ---------------------------------------------------------------- routing

fn req_headers(req: &Request) -> Vec<(String, String)> {
    req.headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect()
}

fn read_body(req: &mut Request, limit: usize) -> Result<Vec<u8>, ApiError> {
    let mut body = Vec::new();
    req.as_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| bad(format!("reading the body: {e}")))?;
    if body.len() > limit {
        return Err(ApiError::new(413, "over_limit", "the request body is too large")
            .with("limit", "body_bytes")
            .with("max", limit));
    }
    Ok(body)
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| bad(format!("request: {e}")))
}

impl Handle {
    fn shared(&self) -> &Arc<Shared> {
        &self.0
    }

    /// The caller id a bearer token (API key or OIDC ID token) stands for.
    pub fn authenticate(&self, bearer: &str) -> Option<String> {
        let s = self.shared();
        s.auth.token(&s.cfg, bearer).ok().map(|c| c.id)
    }

    /// The server's URL as a client with these headers reaches it.
    pub fn base_url(&self, headers: &[(String, String)]) -> String {
        base_url(&self.shared().cfg, headers)
    }

    pub fn config(&self) -> &Config {
        &self.shared().cfg
    }

    /// Runs `f` on the GPU thread with `doc`'s inputs resolved as
    /// `/v1/frame` resolves them (and the renderer's base directory set).
    pub fn with_renderer<T: Send + 'static>(
        &self,
        caller: &str,
        doc: Value,
        f: impl FnOnce(&mut ultrafast_render::Renderer, &ultrafast_graph::Document) -> Result<T, String> + Send + 'static,
    ) -> Result<T, ApiError> {
        let s = self.shared();
        let grants = s.auth.by_id(&s.cfg, caller).grants;
        s.on_gpu(move |e| {
            e.with_document(&doc, None, None, &grants, |r, m| {
                f(r, &m.doc).map_err(|msg| ApiError::new(500, "render_failed", msg))
            })
        })?
    }

    /// Renders `doc` to the server's storage as `settings` ask and returns
    /// the receipt.
    pub fn render(
        &self,
        caller: &str,
        doc: Value,
        settings: Settings,
        range: Option<[f64; 2]>,
        base_url: &str,
    ) -> Result<Receipt, ApiError> {
        let s = self.shared();
        let who = s.auth.by_id(&s.cfg, caller);
        let output = Output {
            range,
            ..Output::with(settings)
        };
        let req = RenderRequest {
            document: doc,
            output: Some(output),
            attempt: None,
            deadline: None,
        };
        let base = base_url.to_string();
        let grants = who.grants.clone();
        let r = s.on_gpu(move |e| {
            let mut ctx = Ctx::sync(&e.cfg, Instant::now());
            ctx.base_url = base;
            e.render_with(req, &grants, &mut ctx)
        });
        match r?? {
            Reply::Json(_, body) => {
                serde_json::from_str(&body).map_err(|e| ApiError::new(500, "render_failed", e.to_string()))
            }
            Reply::Bytes { .. } => Err(ApiError::new(500, "render_failed", "unexpected inline video")),
        }
    }

    /// A local copy of one of the caller's uploads (`asset://ID`,
    /// `asset_ID` or `ID`).
    pub fn asset_path(&self, caller: &str, id: &str) -> Result<PathBuf, ApiError> {
        let s = self.shared();
        let raw = id
            .trim_start_matches("asset://")
            .trim_start_matches("asset_")
            .split("!/")
            .next()
            .unwrap_or("");
        let rec = records::load_asset(&s.stores, s.storage(), raw)?
            .filter(|a| a.owner == caller)
            .ok_or_else(|| not_found("such upload"))?;
        let dir = s.cfg.data_dir.join("asset-cache").join(raw);
        let path = dir.join(&rec.filename);
        if path.exists() {
            return Ok(path);
        }
        std::fs::create_dir_all(&dir).map_err(|e| ApiError::new(503, "unavailable", e.to_string()))?;
        let tmp = dir.join(format!(".part{}", crate::util::unique()));
        let store = s.main_store();
        let mut f = std::fs::File::create(&tmp).map_err(|e| ApiError::new(503, "unavailable", e.to_string()))?;
        store
            .read(&s.storage().bucket, &rec.object, &rec.generation, None, &mut f)
            .map_err(|e| records::store_down(store, &e))?;
        std::fs::rename(&tmp, &path).map_err(|e| ApiError::new(503, "unavailable", e.to_string()))?;
        Ok(path)
    }

    fn health(&self) -> (u16, Health) {
        let s = self.shared();
        let ready = s.ready.load(Ordering::SeqCst);
        (
            if ready { 200 } else { 503 },
            Health {
                ok: !s.failed.load(Ordering::SeqCst),
                ready,
                busy: s.busy.load(Ordering::SeqCst),
                device: s.device.lock().unwrap().clone(),
                backend: ultrafast_gpu::BACKEND.into(),
                version: crate::job::VERSION.into(),
                git: crate::job::git().into(),
                image_digest: s.cfg.image_digest.clone(),
                uptime_s: s.started.elapsed().as_secs_f64(),
                queued: s.queued(),
                storage: s.cfg.storage.scheme.clone(),
            },
        )
    }

    /// Answers one request (on its own thread).
    fn serve(&self, mut req: Request) {
        let s = self.shared().clone();
        let t0 = Instant::now();
        let method = req.method().clone();
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let (path, query) = (path.to_string(), query.to_string());
        let headers = req_headers(&req);
        let m = method.as_str().to_ascii_uppercase();
        // Mounts first: they authenticate their own requests.
        if let Some(mount) = s.mounts.iter().find(|mt| mt.claims(&m, &path)).cloned() {
            let body = match read_body(&mut req, s.cfg.limits.body_bytes) {
                Ok(b) => b,
                Err(e) => {
                    let _ = req.respond(error(&e));
                    return;
                }
            };
            let r = mount.handle(
                &HttpRequest {
                    method: m.clone(),
                    path: path.clone(),
                    query: query.clone(),
                    headers: headers.clone(),
                    body,
                },
                self,
            );
            // Server-sent events stream; everything else has a length.
            let sse = r
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.starts_with("text/event-stream"));
            let mut resp = Response::new(
                r.status.into(),
                Vec::new(),
                Box::new(std::io::Cursor::new(r.body.clone())) as Box<dyn Read + Send>,
                Some(r.body.len()),
                None,
            );
            if !sse {
                resp = resp.with_chunked_threshold(usize::MAX);
            }
            for (k, v) in &r.headers {
                if let Ok(h) = Header::from_bytes(k.as_bytes(), ascii(v).as_bytes()) {
                    resp = resp.with_header(h);
                }
            }
            let _ = req.respond(resp);
            log_request(&m, &path, r.status, t0, "");
            return;
        }
        let (status, caller_id, resp) = self.route(&mut req, &method, &path, &query, &headers);
        let _ = req.respond(resp);
        if path != "/v1/health" && path != "/health" {
            log_request(&m, &path, status, t0, &caller_id);
        }
    }

    fn route(
        &self,
        req: &mut Request,
        method: &Method,
        path: &str,
        query: &str,
        headers: &[(String, String)],
    ) -> (u16, String, Body) {
        let s = self.shared().clone();
        let done = |r: Body| (r.status_code().0, String::new(), r);
        // No authentication: health, signed files.
        match (method, path) {
            (Method::Get, "/v1/health" | "/health" | "/") => {
                let (st, h) = self.health();
                return done(json(st, &serde_json::to_value(h).unwrap_or_default()));
            }
            (Method::Get | Method::Head, p) if p.starts_with("/v1/files/") => {
                return done(s.file(&p["/v1/files/".len()..], query, headers, *method == Method::Head));
            }
            _ => {}
        }
        let caller = match s.auth.caller(&s.cfg, headers) {
            Ok(c) => c,
            Err(e) => return done(error(&e)),
        };
        let who = caller.id.clone();
        let out = |r: Body| (r.status_code().0, who.clone(), r);
        let base = base_url(&s.cfg, headers);
        let segs: Vec<&str> = path.trim_end_matches('/').split('/').skip(1).collect();
        let gpu_body = |req: &mut Request| read_body(req, s.cfg.limits.body_bytes);
        match (method, segs.as_slice()) {
            (Method::Get, ["v1", "me"]) => out(json(
                200,
                &serde_json::json!({"caller": caller.id, "auth": caller.kind}),
            )),
            (Method::Get, ["v1", "templates"]) => out(json(
                200,
                &serde_json::json!({"templates": ultrafast_ops::templates::NAMES}),
            )),
            (Method::Get, ["v1", "templates", name]) => match ultrafast_ops::templates::get(name) {
                Some(doc) => out(respond_bytes(200, "application/json", doc.as_bytes().to_vec(), &[])),
                None => out(error(&not_found(&format!(
                    "template `{name}` ({})",
                    ultrafast_ops::templates::NAMES.join(", ")
                )))),
            },
            (Method::Post, ["v1", "render"]) | (Method::Post, ["render"]) => {
                let legacy = segs == ["render"];
                let r = gpu_body(req).and_then(|body| {
                    let mut rr: RenderRequest = if legacy {
                        // `ultrafast serve`'s old form: the document is the body.
                        let doc: Value = serde_json::from_slice(&body).map_err(|e| bad(format!("document: {e}")))?;
                        let num = |k: &str| query_param(query, k).and_then(|v| v.parse::<f64>().ok());
                        let range = match (num("from"), num("to")) {
                            (None, None) => None,
                            (a, b) => Some([a.unwrap_or(0.0), b.unwrap_or(f64::MAX)]),
                        };
                        // Export settings as query parameters too (`codec`,
                        // `format`, `quality`, `size`, `fps`, ...).
                        let flag = |k: &str| query_param(query, k.trim_start_matches("--").replace('-', "_").as_str());
                        let has = |k: &str| match k {
                            "--alpha" => query_param(query, "alpha").is_some_and(|v| v != "false" && v != "0"),
                            "--no-audio" => query_param(query, "audio").is_some_and(|v| v == "false" || v == "0"),
                            _ => false,
                        };
                        let settings = Settings::from_flags(flag, has).map_err(bad)?;
                        RenderRequest {
                            document: doc,
                            output: Some(Output {
                                inline: true,
                                range,
                                ..Output::with(settings)
                            }),
                            attempt: None,
                            deadline: None,
                        }
                    } else {
                        parse(&body)?
                    };
                    if legacy
                        && let Some(o) = rr.output.as_mut()
                        && let Some(r) = o.range.as_mut()
                        && r[1] == f64::MAX
                    {
                        let d = rr.document["format"]["duration"].as_f64().unwrap_or(0.0);
                        r[1] = d;
                    }
                    let grants = caller.grants.clone();
                    let base = base.clone();
                    let r = s.on_gpu(move |e| {
                        let mut ctx = Ctx::sync(&e.cfg, Instant::now());
                        ctx.base_url = base;
                        e.render_with(rr, &grants, &mut ctx)
                    });
                    r?
                });
                out(reply(r))
            }
            (Method::Post, ["v1", "frame"]) => out(reply(
                self.gpu_json(req, &caller, |e, r: FrameRequest, g| e.frame(r, g)),
            )),
            (Method::Post, ["v1", "sheet"]) => out(reply(
                self.gpu_json(req, &caller, |e, r: SheetRequest, g| e.sheet(r, g)),
            )),
            (Method::Post, ["v1", "check"]) => out(reply(
                self.gpu_json(req, &caller, |e, r: CheckRequest, g| e.check(r, g)),
            )),
            (Method::Post, ["v1", "review"]) => out(reply(
                self.gpu_json(req, &caller, |e, r: ReviewRequest, g| e.review(r, g)),
            )),
            (Method::Post, ["v1", "estimate"]) => {
                out(reply(
                    self.gpu_json(req, &caller, |e, r: EstimateRequest, g| e.estimate(r, g)),
                ))
            }
            (_, ["v1", "jobs", ..]) => out(error(&ApiError::cloud_only(
                "jobs",
                "jobs and their webhooks are an ultrafast Cloud feature; this server renders while you wait: \
                 POST /v1/render (stored, or `output.inline`)",
            ))),
            (Method::Post, ["v1", "uploads"]) => match s.upload(&caller, req, query) {
                Ok(rec) => out(json(201, &rec.public())),
                Err(e) => out(error(&e)),
            },
            (Method::Get, ["v1", "uploads", id]) => {
                let raw = id.trim_start_matches("asset_");
                match records::load_asset(&s.stores, s.storage(), raw) {
                    Ok(Some(a)) if a.owner == caller.id => out(json(200, &a.public())),
                    Ok(_) => out(error(&not_found("such upload"))),
                    Err(e) => out(error(&e)),
                }
            }
            _ => out(error(&not_found(&format!("route {} {path}", method.as_str())))),
        }
    }

    /// Reads a JSON request body and runs `f` with it on the GPU.
    fn gpu_json<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        req: &mut Request,
        caller: &Caller,
        f: impl FnOnce(&mut Engine, T, &crate::config::Grants) -> Result<Reply, ApiError> + Send + 'static,
    ) -> Result<Reply, ApiError> {
        let s = self.shared();
        let body = read_body(req, s.cfg.limits.body_bytes)?;
        let r: T = parse(&body)?;
        let grants = caller.grants.clone();
        s.on_gpu(move |e| f(e, r, &grants))?
    }
}

fn log_request(method: &str, path: &str, status: u16, t0: Instant, caller: &str) {
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    log(serde_json::json!({
        // 501 is `cloud_only`: the caller asked for a Cloud feature, nothing failed.
        "severity": if status >= 500 && status != 501 {
            "ERROR"
        } else if status >= 400 {
            "WARNING"
        } else {
            "INFO"
        },
        "message": format!("{method} {path} {status} in {ms:.0} ms"),
        "path": path, "httpStatus": status, "ms": ms, "caller": caller,
    }));
}

/// What `run` was given besides the configuration.
#[derive(Default)]
pub struct Options {
    pub mounts: Vec<Arc<dyn Mount>>,
    /// Called once the server listens (with its address), before the GPU
    /// is ready.
    pub on_listen: Option<Box<dyn FnOnce(std::net::SocketAddr) + Send>>,
}

/// Prints the generated key where it is easy to find in logs.
fn announce_key(cfg: &Config) {
    if let Some(key) = &cfg.generated_key {
        let file = cfg.data_dir.join("api-key");
        eprintln!(
            "\n========================================================================\n\
             ultrafast serve: API key (no ULTRAFAST_API_KEY set, so one was made)\n\n    \
             ULTRAFAST_API_KEY={key}\n\n\
             Saved in {}. Send it as `Authorization: Bearer <key>`.\n\
             Set ULTRAFAST_API_KEY to choose your own, or ULTRAFAST_AUTH=none to turn auth off.\n\
             ========================================================================\n",
            file.display()
        );
    }
}

/// Runs the server until the process ends.
pub fn run(cfg: Config, opts: Options) -> anyhow::Result<()> {
    let started = Instant::now();
    // Inputs read only what they embed: an SVG, Lottie or glTF file never
    // reaches another path on the server (other requests' inputs).
    // SAFETY: called before this function starts any thread.
    unsafe { std::env::set_var("ULTRAFAST_NO_EXTERNAL_FILES", "1") };
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", cfg.data_dir.display()))?;
    let addr = format!("{}:{}", cfg.bind, cfg.port);
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("cannot listen on {addr}: {e}"))?;
    let local = server.server_addr().to_ip();
    let stores = Stores::new(&cfg.storage)?;
    let shared = Arc::new(Shared {
        auth: Authenticator::new(&cfg),
        stores,
        started,
        ready: AtomicBool::new(false),
        failed: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        device: Mutex::new(String::new()),
        lanes: Mutex::new(Lanes::default()),
        wake: Condvar::new(),
        mounts: opts.mounts,
        open: AtomicUsize::new(0),
        cfg,
    });
    let cfg = &shared.cfg;
    announce_key(cfg);
    log(serde_json::json!({
        "severity": "INFO",
        "message": format!(
            "ultrafast serve: listening on {addr} (storage {}, auth {})",
            cfg.storage.describe(),
            match cfg.auth {
                crate::config::Auth::None => "off",
                crate::config::Auth::Key => "API key",
            }
        ),
    }));
    if let (Some(f), Some(a)) = (opts.on_listen, local) {
        f(a);
    }
    // The GPU thread.
    {
        let shared = shared.clone();
        std::thread::Builder::new().name("render".into()).spawn(move || {
            let cfg = shared.cfg.clone();
            let init = (|| -> anyhow::Result<Engine> {
                let device = ultrafast_gpu::Device::new()?;
                *shared.device.lock().unwrap() = device.name().to_string();
                let renderer = ultrafast_render::Renderer::new(device)?;
                Ok(Engine {
                    stores: Stores::new(&cfg.storage)?,
                    cache: crate::inputs::Cache::new(&cfg.cache_dir, cfg.cache_bytes)?,
                    renderer,
                    start_s: None,
                    cfg,
                })
            })();
            let mut engine = match init {
                Ok(e) => e,
                Err(e) => {
                    log(serde_json::json!({"severity": "ERROR", "message": format!("ultrafast serve: GPU init failed: {e:#}")}));
                    shared.failed.store(true, Ordering::SeqCst);
                    // Health answers 503 so a platform replaces the server;
                    // waiting work is dropped (its callers get 503).
                    loop {
                        let mut lanes = shared.lanes.lock().unwrap();
                        lanes.interactive.clear();
                        drop(shared.wake.wait_timeout(lanes, Duration::from_secs(5)));
                    }
                }
            };
            let ready_s = shared.started.elapsed().as_secs_f64();
            engine.start_s = Some(ready_s);
            shared.ready.store(true, Ordering::SeqCst);
            log(serde_json::json!({
                "severity": "INFO",
                "message": format!("ultrafast serve ready ({}, {}) in {ready_s:.2} s",
                    shared.device.lock().unwrap(), ultrafast_gpu::BACKEND),
            }));
            loop {
                let next = {
                    let mut lanes = shared.lanes.lock().unwrap();
                    loop {
                        if let Some(w) = lanes.interactive.pop_front() {
                            break w;
                        }
                        lanes = shared.wake.wait(lanes).unwrap();
                    }
                };
                shared.busy.store(true, Ordering::SeqCst);
                next(&mut engine);
                engine.cache.trim(&[]);
                shared.busy.store(false, Ordering::SeqCst);
            }
        })?;
    }
    let handle = Handle(shared.clone());
    for req in server.incoming_requests() {
        if shared.open.load(Ordering::SeqCst) >= 256 {
            let _ = req.respond(error(&ApiError::new(503, "unavailable", "too many open requests")));
            continue;
        }
        shared.open.fetch_add(1, Ordering::SeqCst);
        let h = handle.clone();
        let spawned = std::thread::Builder::new().name("request".into()).spawn(move || {
            h.serve(req);
            h.0.open.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            shared.open.fetch_sub(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// When a request arrives (for logs and tests).
pub fn now() -> SystemTime {
    SystemTime::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_streams_the_file_part() {
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhello\r\n--XyZ\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"a b.mp4\"\r\nContent-Type: video/mp4\r\n\r\n\
\x00\x01\r\n--Xy\r\nZZ\r\n--XyZ--\r\n";
        let mut out = Vec::new();
        let (name, ct, n) = read_multipart(&mut &body[..], "XyZ", &mut out, 1 << 20).unwrap();
        assert_eq!(name.as_deref(), Some("a b.mp4"));
        assert_eq!(ct.as_deref(), Some("video/mp4"));
        assert_eq!(out, b"\x00\x01\r\n--Xy\r\nZZ");
        assert_eq!(n, out.len() as u64);
        assert!(
            read_multipart(&mut &body[..], "XyZ", &mut Vec::new(), 4)
                .unwrap_err()
                .starts_with("over_limit")
        );
        assert!(read_multipart(&mut &b"nothing"[..], "XyZ", &mut Vec::new(), 4).is_err());
    }

    /// A large file across many reads keeps every byte, even when the
    /// delimiter straddles reads.
    #[test]
    fn multipart_large_body() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut body =
            b"--b0undary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\n\r\n".to_vec();
        body.extend_from_slice(&data);
        body.extend_from_slice(b"\r\n--b0undary--\r\n");
        let mut out = Vec::new();
        read_multipart(&mut &body[..], "b0undary", &mut out, 1 << 30).unwrap();
        assert_eq!(out.len(), data.len());
        assert!(out == data);
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 10)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 100)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 100)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 100)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
    }
}
