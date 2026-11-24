//! One request, start to finish: check it, look at the output object,
//! resolve the inputs, render, upload create-only, write the manifest and
//! answer with the receipt. Also frames, contact sheets, checks, reviews
//! and estimates. Everything here runs on the thread that owns the GPU.

use crate::api::*;
use crate::config::{Config, Grants};
use crate::inputs::{Cache, Resolver};
use crate::store::{Created, ObjectMeta, Store, Stores};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Param};
use ultrafast_live::output::Target as ExportTarget;
use ultrafast_render::{Diagnostic, Renderer};

/// Uploads do not start this close to the deadline.
const UPLOAD_MARGIN: Duration = Duration::from_secs(2);

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn git() -> &'static str {
    option_env!("ULTRAFAST_GIT").unwrap_or("unknown")
}

/// What to answer.
pub enum Reply {
    Json(u16, String),
    Bytes {
        content_type: &'static str,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
    },
}

pub struct Engine {
    pub cfg: Config,
    /// The engine's own stores (their deadline is the current request's).
    pub stores: Stores,
    pub cache: Cache,
    pub renderer: Renderer,
    /// Seconds from process start to ready, reported once.
    pub start_s: Option<f64>,
}

/// How one render runs: when it arrived, how long it may take, where its
/// signed URL points, and who hears of its progress.
pub struct Ctx<'a> {
    pub received: Instant,
    /// The longest it may take (10 minutes).
    pub wall: Duration,
    /// The server's base URL as the client sees it (for signed URLs).
    pub base_url: String,
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
}

impl Ctx<'_> {
    pub fn sync(cfg: &Config, received: Instant) -> Ctx<'static> {
        Ctx {
            received,
            wall: Duration::from_secs_f64(cfg.limits.wall_s),
            base_url: cfg.public_url.clone().unwrap_or_default(),
            progress: None,
        }
    }
}

enum Target {
    Object {
        scheme: String,
        bucket: String,
        name: String,
        /// In the server's own storage (else the caller's `output.uri`).
        stored: bool,
    },
    Inline,
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(400, "bad_request", message)
}

fn valid_id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// A stored render's file name: `film.<format>` (`frames.zip` for an image
/// sequence) unless the request names one; a name without the format's
/// extension gets it.
fn output_name(name: Option<&str>, container: Container) -> String {
    let ext = if container.is_sequence() {
        "zip"
    } else {
        container.name()
    };
    let default = if container.is_sequence() { "frames" } else { "film" };
    let n = crate::records::clean_filename(name.unwrap_or(default));
    let has = std::path::Path::new(&n).extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case(ext) || (!container.is_sequence() && Container::from_extension(&e) == Some(container))
    });
    if has { n } else { format!("{n}.{ext}") }
}

/// Zips the files of `dir` (an image sequence and its sound) into `out`,
/// stored as they are: PNG and EXR are compressed already.
fn zip_dir(dir: &std::path::Path, out: &std::path::Path) -> std::io::Result<()> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(out)?);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    for n in names {
        zip.start_file(n.to_string_lossy(), opts)
            .map_err(std::io::Error::other)?;
        std::io::copy(&mut std::fs::File::open(dir.join(&n))?, &mut zip)?;
    }
    zip.finish().map_err(std::io::Error::other)?;
    Ok(())
}

/// Removes a request's directory however the request ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A checked document and the seconds to render.
struct Prepared {
    doc: Document,
    sha: String,
    from: f64,
    to: f64,
    /// What the check warns about, as `issues` list them.
    warnings: Vec<Value>,
}

/// Receipts list at most this many warnings: the inline receipt is a
/// header, and clients cap the size of those.
const MAX_WARNINGS: usize = 20;

/// Warnings as receipts list them, ``node `id`: message``: the check's
/// first, then what the renderer found, each once.
fn warning_lines(issues: &[Value], found: &[Diagnostic]) -> Vec<String> {
    let issues = issues
        .iter()
        .map(|i| (i["node"].as_str().unwrap_or(""), i["message"].as_str().unwrap_or("")));
    let mut out: Vec<String> = Vec::new();
    for (node, message) in issues.chain(found.iter().map(|d| (d.node.as_str(), d.message.as_str()))) {
        let line = if node.is_empty() {
            message.to_string()
        } else {
            format!("node `{node}`: {message}")
        };
        if !out.contains(&line) {
            out.push(line);
        }
    }
    if out.len() > MAX_WARNINGS {
        let more = out.len() - (MAX_WARNINGS - 1);
        out.truncate(MAX_WARNINGS - 1);
        out.push(format!("and {more} more"));
    }
    out
}

impl Engine {
    fn renderer_info(&self) -> RendererInfo {
        RendererInfo {
            service: "ultrafast-service".into(),
            version: VERSION.into(),
            git: git().into(),
            image_digest: self.cfg.image_digest.clone(),
        }
    }

    /// Parses and checks a request's document (references are still
    /// strings here) and its range.
    fn prepare(&self, document: &Value, range: Option<[f64; 2]>) -> Result<Prepared, ApiError> {
        let text = canonical_json(document);
        let lim = &self.cfg.limits;
        if text.len() > lim.document_bytes {
            return Err(ApiError::new(413, "over_limit", "the document is too large")
                .with("limit", "document_bytes")
                .with("value", text.len())
                .with("max", lim.document_bytes));
        }
        let sha = crate::util::sha256_hex(text.as_bytes());
        let doc = Document::from_json(&text).map_err(|e| {
            ApiError::new(400, "invalid_document", format!("{e}")).with(
                "issues",
                vec![serde_json::json!({"node": "", "severity": "error", "message": e.to_string()})],
            )
        })?;
        let (issues, warnings): (Vec<Value>, Vec<Value>) = ultrafast_render::schema::check(&doc)
            .into_iter()
            .filter(|i| i.severity != "info")
            .map(|i| serde_json::json!({"node": i.node, "severity": i.severity, "message": i.message}))
            .partition(|i| i["severity"] == "error");
        if !issues.is_empty() {
            return Err(ApiError::new(
                400,
                "invalid_document",
                format!(
                    "the document has {} problem{}",
                    issues.len(),
                    if issues.len() == 1 { "" } else { "s" }
                ),
            )
            .with("issues", issues));
        }
        let f = doc.format;
        let over = |limit: &str, value: f64, max: f64| {
            ApiError::new(413, "over_limit", format!("{limit} {value} exceeds {max}"))
                .with("limit", limit)
                .with("value", value)
                .with("max", max)
        };
        if f.duration > lim.duration_s {
            return Err(over("duration_s", f.duration, lim.duration_s));
        }
        if f.width.max(f.height) > lim.side {
            return Err(over("side", f.width.max(f.height) as f64, lim.side as f64));
        }
        if f.width as u64 * f.height as u64 > lim.pixels {
            return Err(over(
                "pixels",
                (f.width as u64 * f.height as u64) as f64,
                lim.pixels as f64,
            ));
        }
        if f.fps > lim.fps {
            return Err(over("fps", f.fps, lim.fps));
        }
        let [from, to] = range.unwrap_or([0.0, f.duration]);
        if !(from >= 0.0 && from < to && to <= f.duration + 1e-6) {
            return Err(bad(format!(
                "range [{from}, {to}] is not inside the document's 0..{} s",
                f.duration
            )));
        }
        Ok(Prepared {
            doc,
            sha,
            from,
            to,
            warnings,
        })
    }

    /// Resolves the inputs and returns the document to render, reading only
    /// from `dir`.
    fn materialize(
        &self,
        document: &Value,
        p: &Prepared,
        grants: &Grants,
        dir: &Path,
    ) -> Result<(Document, crate::inputs::Resolved), ApiError> {
        let usage = crate::usage::usage(&p.doc, p.from, p.to).map_err(bad)?;
        let resolved = Resolver {
            cfg: &self.cfg,
            stores: &self.stores,
            storage: &self.cfg.storage,
            cache: &self.cache,
        }
        .resolve(document, &usage, grants, dir)?;
        let mut v = document.clone();
        crate::refs::rewrite(&mut v, &resolved.map);
        let doc = Document::from_json(&v.to_string()).map_err(|e| bad(format!("document: {e}")))?;
        only_reads_from(&doc, dir)?;
        Ok((doc, resolved))
    }

    /// A synchronous render (10-minute wall).
    pub fn render(&mut self, req: RenderRequest, grants: &Grants, received: Instant) -> Result<Reply, ApiError> {
        let mut ctx = Ctx::sync(&self.cfg, received);
        self.render_with(req, grants, &mut ctx)
    }

    pub fn render_with(&mut self, req: RenderRequest, grants: &Grants, ctx: &mut Ctx) -> Result<Reply, ApiError> {
        let received = ctx.received;
        let output = req.output.clone().unwrap_or_default();
        let attempt = match &req.attempt {
            Some(a) if !valid_id(&a.render_id) || !valid_id(&a.attempt_id) => {
                return Err(bad("`attempt` ids are 1 to 128 characters of A-Z a-z 0-9 . _ : -"));
            }
            Some(a) => a.clone(),
            None => Attempt {
                render_id: format!("render_{}", crate::util::ulid()),
                attempt_id: "1".into(),
            },
        };
        let settings = output.settings();
        let container = settings.container(output.named_path()).map_err(bad)?;
        let target = match (&output.uri, output.inline) {
            (Some(_), true) => return Err(bad("`output` takes `uri` or `inline`, not both")),
            (None, true) => Target::Inline,
            (None, false) => Target::Object {
                scheme: self.cfg.storage.scheme.clone(),
                bucket: self.cfg.storage.bucket.clone(),
                name: self.cfg.storage.name(&format!(
                    "renders/{}/{}",
                    crate::records::clean_filename(&attempt.render_id),
                    output_name(output.name.as_deref(), container)
                )),
                stored: true,
            },
            (Some(uri), false) => {
                return Err(ApiError::cloud_only(
                    "bucket_output",
                    format!(
                        "`output.uri` ({uri}): writing renders to your own bucket is an ultrafast Cloud feature; \
                         this server stores them (drop `output.uri`) or answers inline (`output.inline`)"
                    ),
                ));
            }
        };
        let deadline = match &req.deadline {
            None => received + ctx.wall,
            Some(d) => {
                let at = crate::util::parse_rfc3339(d)
                    .ok_or_else(|| bad(format!("`deadline` `{d}` is not RFC 3339 UTC (…Z)")))?;
                let left = at.duration_since(SystemTime::now()).map_err(|_| {
                    ApiError::new(
                        504,
                        "deadline_exceeded",
                        "the deadline had already passed; nothing was written",
                    )
                })?;
                received + left.min(ctx.wall)
            }
        };
        let p = self.prepare(&req.document, output.range)?;
        if req.strict && !p.warnings.is_empty() {
            let n = p.warnings.len();
            return Err(ApiError::new(
                400,
                "invalid_document",
                format!(
                    "the document has {n} warning{} and the request is `strict`",
                    if n == 1 { "" } else { "s" }
                ),
            )
            .with("issues", &p.warnings));
        }
        // What gets written, checked before anything is fetched.
        let enc = settings.resolve(output.named_path(), &p.doc.format).map_err(bad)?;
        // Every store call ends by the deadline too, not only the export.
        self.stores.set_deadline(Some(deadline));
        let result = self.render_by(req, &output, enc, grants, ctx, target, Some(attempt), deadline, p);
        self.stores.set_deadline(None);
        // Whatever failed after the deadline failed because of it.
        match result {
            Err(e) if Instant::now() >= deadline && e.status != 504 && e.status != 409 => Err(ApiError::new(
                504,
                "deadline_exceeded",
                format!(
                    "the deadline passed ({}); an upload already under way may still land",
                    e.message
                ),
            )),
            other => other,
        }
    }

    /// A URL the client can download `name` (in `scheme://bucket`) from:
    /// the store's own presigned URL, or (for the server's storage) one the
    /// server signs and serves under /v1/files.
    fn download_url(
        &self,
        store: &dyn Store,
        bucket: &str,
        name: &str,
        stored: bool,
        base_url: &str,
    ) -> Option<(String, String)> {
        let ttl = self.cfg.url_ttl;
        let expires = SystemTime::now() + ttl;
        if let Some(u) = store.presign(bucket, name, ttl) {
            return Some((u, crate::util::rfc3339(expires)));
        }
        if !stored || base_url.is_empty() {
            return None;
        }
        let rel = name.strip_prefix(&self.cfg.storage.prefix).unwrap_or(name);
        Some((
            crate::auth::sign_file_url(&self.cfg.secret, base_url, rel, expires),
            crate::util::rfc3339(expires),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_by(
        &mut self,
        req: RenderRequest,
        output: &Output,
        enc: ExportTarget,
        grants: &Grants,
        ctx: &mut Ctx,
        target: Target,
        attempt: Option<Attempt>,
        deadline: Instant,
        p: Prepared,
    ) -> Result<Reply, ApiError> {
        let received = ctx.received;
        let dir = self.cache.request_dir().map_err(|e| internal(&e))?;
        let _scratch = Scratch(dir.clone());
        if let Target::Object { scheme, .. } = &target
            && self.stores.get(scheme).is_none()
        {
            return Err(bad(format!("{scheme}:// outputs are not available here")));
        }
        // Whether the output is already there, asked while the inputs are
        // fetched (each is a round trip to the bucket's region).
        let this = &*self;
        let store: Option<&dyn Store> = match &target {
            Target::Object { scheme, .. } => this.stores.get(scheme),
            Target::Inline => None,
        };
        let (existing, materialized) = std::thread::scope(|s| {
            let existing = match (&target, store) {
                (Target::Object { bucket, name, .. }, Some(st)) => Some(s.spawn(move || st.stat(bucket, name, None))),
                _ => None,
            };
            let materialized = this.materialize(&req.document, &p, grants, &dir);
            (
                existing.map(|h| h.join().expect("the output check panicked")),
                materialized,
            )
        });
        // An object already there: this request's own earlier render (a
        // replay), or someone else's.
        if let (Some(existing), Some(st)) = (existing, store)
            && let Some(obj) = existing.map_err(|e| store_down(st, &e))?
        {
            return self.replay(st, &obj, attempt.as_ref(), &p.sha, &target, &ctx.base_url);
        }
        let (doc, resolved) = materialized?;
        let scheme_of = match &target {
            Target::Object { scheme, .. } => Some(scheme.clone()),
            Target::Inline => None,
        };
        self.pin_fonts(&resolved)?;
        // Render (a sequence into a folder, zipped after).
        let sequence = enc.container.is_sequence();
        let out = if sequence {
            dir.join("frames").join("")
        } else {
            dir.join(format!("out.{}", enc.container.name()))
        };
        let opts = ultrafast_live::export::ExportOptions {
            settings: output.settings(),
            start: p.from,
            end: Some(p.to),
            deadline: Some(deadline),
        };
        let t_render = Instant::now();
        let mut first_frame: Option<f64> = None;
        let mut progress = ctx.progress.take();
        let report = self.guarded(|r| {
            r.set_base_dir(&dir);
            r.prefetch_fonts(&doc);
            ultrafast_live::export::export_target(r, &doc, &out, enc, opts, |done, total| {
                if done > 0 && first_frame.is_none() {
                    first_frame = Some(t_render.elapsed().as_secs_f64());
                }
                if let Some(f) = progress.as_mut() {
                    f(done, total);
                }
            })
            .map_err(|e| {
                if e == ultrafast_live::export::DEADLINE_PASSED {
                    ApiError::new(
                        504,
                        "deadline_exceeded",
                        "the deadline passed while rendering; nothing was written",
                    )
                } else {
                    ApiError::new(500, "render_failed", format!("render failed: {e}"))
                }
            })
        });
        // Before unpinning, which lets go of the document and them.
        let found = self.renderer.diagnostics().to_vec();
        self.renderer.unpin_fonts();
        let report = report?;
        let warnings = warning_lines(&p.warnings, &found);
        let render_s = t_render.elapsed().as_secs_f64();
        let out = if sequence {
            let zipped = dir.join("out.zip");
            zip_dir(&out, &zipped).map_err(|e| internal(&e))?;
            zipped
        } else {
            out
        };
        let content_type = if sequence {
            "application/zip"
        } else {
            enc.container.content_type()
        };
        let hashes = crate::util::hash_file(&out).map_err(|e| internal(&e))?;
        let fmt = doc.format;
        let video = VideoInfo {
            frames: report.frames,
            fps: enc.fps,
            width: enc.width,
            height: enc.height,
            duration: p.to - p.from,
            codec: enc.codec_name(),
            audio: output.audio && (enc.audio.is_some() || sequence),
            range: [p.from, p.to],
            format: enc.container.name().into(),
            quality: enc.quality,
            alpha: enc.video.is_some_and(|v| v.alpha),
        };
        // Rendered minutes at the size written (sound alone counts as the
        // smallest).
        let billed = match enc.video {
            Some(_) => ultrafast_graph::Format {
                width: enc.width,
                height: enc.height,
                ..fmt
            },
            None => ultrafast_graph::Format {
                width: 1,
                height: 1,
                ..fmt
            },
        };
        let mut receipt = Receipt {
            receipt_version: 1,
            id: attempt.as_ref().map(|a| a.render_id.clone()),
            url: None,
            url_expires_at: None,
            duration: Some(p.to - p.from),
            frames: Some(report.frames),
            bytes: Some(hashes.size),
            sha256: Some(hashes.sha256.clone()),
            minutes: Some(round3(ultrafast_live::estimate::price(&billed, p.from, p.to).minutes)),
            status: "rendered".into(),
            attempt: attempt.clone(),
            object: None,
            manifest: None,
            video,
            renderer: self.renderer_info(),
            document: DocumentInfo {
                sha256: p.sha.clone(),
                format_version: p.doc.version,
                meta: req.document.get("meta").cloned().unwrap_or(Value::Null),
            },
            inputs: resolved.inputs.clone(),
            fonts: resolved.fonts.clone(),
            timings: Some(Timings {
                instance_start_s: self.start_s.take(),
                fetch_s: resolved.fetch_s,
                unpack_s: resolved.unpack_s,
                plan_s: first_frame.unwrap_or(0.0).min(render_s),
                render_s,
                encode_s: report.timing.encode,
                upload_s: 0.0,
                total_s: 0.0,
            }),
            warnings,
            created_at: crate::util::now_rfc3339(),
        };
        match target {
            Target::Inline => {
                if hashes.size > self.cfg.limits.inline_bytes {
                    return Err(
                        ApiError::new(413, "over_limit", "the video is too large to return inline")
                            .with("limit", "inline_bytes")
                            .with("value", hashes.size)
                            .with("max", self.cfg.limits.inline_bytes),
                    );
                }
                let body = std::fs::read(&out).map_err(|e| internal(&e))?;
                if let Some(t) = receipt.timings.as_mut() {
                    t.total_s = received.elapsed().as_secs_f64();
                }
                let header = serde_json::to_string(&receipt).unwrap_or_default();
                Ok(Reply::Bytes {
                    content_type,
                    body,
                    headers: vec![
                        ("X-Ultrafast-Receipt".into(), header),
                        ("X-Ultrafast-Sha256".into(), hashes.sha256.clone()),
                    ],
                })
            }
            Target::Object {
                ref scheme,
                ref bucket,
                ref name,
                stored,
            } => {
                let store = self
                    .stores
                    .get(scheme_of.as_deref().unwrap_or(""))
                    .expect("a store for an object target");
                if Instant::now() + UPLOAD_MARGIN >= deadline {
                    return Err(ApiError::new(
                        504,
                        "deadline_exceeded",
                        "the deadline passed before the upload could start; nothing was written",
                    ));
                }
                let a = attempt.clone().expect("checked above");
                let mut meta = BTreeMap::new();
                let put = |m: &mut BTreeMap<String, String>, k: &str, v: String| {
                    m.insert(k.to_string(), v);
                };
                put(&mut meta, "ultrafast-receipt-version", "1".into());
                put(&mut meta, "ultrafast-render-id", a.render_id.clone());
                put(&mut meta, "ultrafast-attempt-id", a.attempt_id.clone());
                put(&mut meta, "ultrafast-document-sha256", p.sha.clone());
                put(&mut meta, "ultrafast-sha256", hashes.sha256.clone());
                // The same, under the plain key generic artifact stores read.
                put(&mut meta, "sha256", hashes.sha256.clone());
                put(&mut meta, "ultrafast-frames", receipt.video.frames.to_string());
                put(&mut meta, "ultrafast-duration", format!("{}", receipt.video.duration));
                put(&mut meta, "ultrafast-width", receipt.video.width.to_string());
                put(&mut meta, "ultrafast-height", receipt.video.height.to_string());
                put(&mut meta, "ultrafast-fps", format!("{}", receipt.video.fps));
                put(&mut meta, "ultrafast-codec", receipt.video.codec.clone());
                put(&mut meta, "ultrafast-format", receipt.video.format.clone());
                put(&mut meta, "ultrafast-image-digest", self.cfg.image_digest.clone());
                put(&mut meta, "ultrafast-version", format!("{VERSION}+{}", git()));
                put(
                    &mut meta,
                    "ultrafast-inputs-sha256",
                    crate::util::sha256_hex(
                        canonical_json(&serde_json::json!({"inputs": receipt.inputs, "fonts": receipt.fonts}))
                            .as_bytes(),
                    ),
                );
                put(&mut meta, "ultrafast-created-at", crate::util::now_rfc3339());
                let t_up = Instant::now();
                let created = store
                    .create(bucket, name, &out, content_type, &meta, &hashes)
                    .map_err(|e| store_down(store, &e))?;
                let obj = match created {
                    Created::Done(o) => o,
                    Created::Exists => {
                        // Another request created it meanwhile.
                        let obj = store
                            .stat(bucket, name, None)
                            .map_err(|e| store_down(store, &e))?
                            .ok_or_else(|| ApiError::new(503, "unavailable", "the output object vanished"))?;
                        return self.replay(store, &obj, attempt.as_ref(), &p.sha, &target, &ctx.base_url);
                    }
                };
                receipt.object = Some(self.identity(scheme, &obj, &hashes.sha256));
                if let Some((url, until)) = self.download_url(store, bucket, name, stored, &ctx.base_url) {
                    receipt.url = Some(url);
                    receipt.url_expires_at = Some(until);
                }
                // The detailed manifest, best effort.
                let manifest_name = format!("{name}.manifest.json");
                if let Some(t) = receipt.timings.as_mut() {
                    t.upload_s = t_up.elapsed().as_secs_f64();
                }
                let body = serde_json::to_vec_pretty(&receipt).unwrap_or_default();
                match store.create_bytes(bucket, &manifest_name, &body, "application/json") {
                    Ok(Created::Done(m)) => {
                        receipt.manifest = Some(ManifestRef {
                            uri: self.uri_of(scheme, bucket, &manifest_name),
                            generation: m.generation,
                        })
                    }
                    Ok(Created::Exists) => receipt
                        .warnings
                        .push("a manifest object already existed; left as is".into()),
                    Err(e) => receipt.warnings.push(format!("manifest not written: {e:#}")),
                }
                if let Some(t) = receipt.timings.as_mut() {
                    t.total_s = received.elapsed().as_secs_f64();
                }
                Ok(Reply::Json(200, serde_json::to_string(&receipt).unwrap_or_default()))
            }
        }
    }

    /// An output object that already exists: this request's own render (409
    /// with its receipt) or not (409 `output_conflict`).
    fn replay(
        &self,
        store: &dyn Store,
        obj: &ObjectMeta,
        attempt: Option<&Attempt>,
        doc_sha: &str,
        target: &Target,
        base_url: &str,
    ) -> Result<Reply, ApiError> {
        let (scheme, stored) = match target {
            Target::Object { scheme, stored, .. } => (scheme.as_str(), *stored),
            Target::Inline => ("gs", false),
        };
        let m = &obj.metadata;
        let get = |k: &str| m.get(k).cloned().unwrap_or_default();
        let ours = attempt.is_some_and(|a| {
            get("ultrafast-render-id") == a.render_id
                && get("ultrafast-attempt-id") == a.attempt_id
                && get("ultrafast-document-sha256") == doc_sha
        });
        if !ours {
            return Err(ApiError::new(
                409,
                "output_conflict",
                "the output object exists and is not this request's render",
            )
            .with(
                "object",
                serde_json::json!({
                    "uri": self.uri_of(scheme, &obj.bucket, &obj.name),
                    "generation": obj.generation,
                    "size": obj.size,
                    "sha256": m.get("ultrafast-sha256"),
                    "render_id": m.get("ultrafast-render-id"),
                    "attempt_id": m.get("ultrafast-attempt-id"),
                }),
            ));
        }
        let sha = get("ultrafast-sha256");
        let manifest_name = format!("{}.manifest.json", obj.name);
        let from_manifest = store
            .stat(&obj.bucket, &manifest_name, None)
            .ok()
            .flatten()
            .and_then(|mo| {
                let mut buf = Vec::new();
                store
                    .read(&obj.bucket, &manifest_name, &mo.generation, None, &mut buf)
                    .ok()?;
                let r: Receipt = serde_json::from_slice(&buf).ok()?;
                Some((r, mo.generation))
            });
        let mut receipt = match from_manifest {
            Some((mut r, generation)) => {
                r.manifest = Some(ManifestRef {
                    uri: self.uri_of(scheme, &obj.bucket, &manifest_name),
                    generation,
                });
                r
            }
            None => {
                let num = |k: &str| get(k).parse::<f64>().unwrap_or(0.0);
                Receipt {
                    receipt_version: 1,
                    id: attempt.map(|a| a.render_id.clone()),
                    url: None,
                    url_expires_at: None,
                    duration: Some(num("ultrafast-duration")),
                    frames: Some(num("ultrafast-frames") as u64),
                    bytes: Some(obj.size),
                    sha256: Some(sha.clone()),
                    minutes: None,
                    status: String::new(),
                    attempt: attempt.cloned(),
                    object: None,
                    manifest: None,
                    video: VideoInfo {
                        frames: num("ultrafast-frames") as u64,
                        fps: num("ultrafast-fps"),
                        width: num("ultrafast-width") as u32,
                        height: num("ultrafast-height") as u32,
                        duration: num("ultrafast-duration"),
                        codec: Some(get("ultrafast-codec"))
                            .filter(|c| !c.is_empty())
                            .unwrap_or_else(|| "h264".into()),
                        audio: true,
                        range: [0.0, num("ultrafast-duration")],
                        format: Some(get("ultrafast-format"))
                            .filter(|c| !c.is_empty())
                            .unwrap_or_else(|| "mp4".into()),
                        quality: Quality::Standard,
                        alpha: false,
                    },
                    renderer: RendererInfo {
                        service: "ultrafast-service".into(),
                        version: get("ultrafast-version"),
                        git: String::new(),
                        image_digest: get("ultrafast-image-digest"),
                    },
                    document: DocumentInfo {
                        sha256: get("ultrafast-document-sha256"),
                        format_version: 1,
                        meta: Value::Null,
                    },
                    inputs: Vec::new(),
                    fonts: Vec::new(),
                    timings: None,
                    warnings: vec!["rebuilt from the object's metadata (no manifest)".into()],
                    created_at: get("ultrafast-created-at"),
                }
            }
        };
        receipt.status = "already_rendered".into();
        receipt.object = Some(self.identity(scheme, obj, &sha));
        receipt.url = None;
        receipt.url_expires_at = None;
        if let Some((url, until)) = self.download_url(store, &obj.bucket, &obj.name, stored, base_url) {
            receipt.url = Some(url);
            receipt.url_expires_at = Some(until);
        }
        Ok(Reply::Json(409, serde_json::to_string(&receipt).unwrap_or_default()))
    }

    pub fn frame(&mut self, req: FrameRequest, grants: &Grants) -> Result<Reply, ApiError> {
        let p0 = self.prepare(&req.document, None)?;
        if !(req.t >= 0.0 && req.t <= p0.doc.format.duration) {
            return Err(bad(format!(
                "t {} is outside the document's 0..{} s",
                req.t, p0.doc.format.duration
            )));
        }
        let p = Prepared {
            from: req.t,
            to: req.t,
            ..p0
        };
        let dir = self.cache.request_dir().map_err(|e| internal(&e))?;
        let _scratch = Scratch(dir.clone());
        let t0 = Instant::now();
        self.stores
            .set_deadline(Some(t0 + Duration::from_secs_f64(self.cfg.limits.wall_s)));
        let materialized = self.materialize(&req.document, &p, grants, &dir);
        self.stores.set_deadline(None);
        let (doc, resolved) = materialized?;
        let fetched = t0.elapsed().as_secs_f64();
        self.pin_fonts(&resolved)?;
        let t1 = Instant::now();
        let px = self.guarded(|r| {
            r.set_base_dir(&dir);
            r.prefetch_fonts(&doc);
            r.render_rgba8(&doc, req.t)
                .map(|(px, _)| px)
                .map_err(|e| ApiError::new(500, "render_failed", format!("render failed: {e}")))
        });
        let found = self.renderer.diagnostics().to_vec();
        self.renderer.unpin_fonts();
        let px = px?;
        let (w, h) = (doc.format.width, doc.format.height);
        let (body, content_type) = match req.format {
            ImageFormat::Png => (encode_png(w, h, &px).map_err(|e| internal(&e))?, "image/png"),
            ImageFormat::Jpeg => (encode_jpeg(w, h, &px).map_err(|e| internal(&e))?, "image/jpeg"),
        };
        let header = serde_json::json!({
            "inputs": resolved.inputs,
            "timings": {"fetch_s": fetched, "render_s": t1.elapsed().as_secs_f64()},
            "document": {"sha256": p.sha},
            "warnings": warning_lines(&p.warnings, &found),
        });
        Ok(Reply::Bytes {
            content_type,
            body,
            headers: vec![("X-Ultrafast-Receipt".into(), header.to_string())],
        })
    }

    /// The document's `meta.fonts` pins, for this render only.
    fn pin_fonts(&mut self, resolved: &crate::inputs::Resolved) -> Result<(), ApiError> {
        let pins: Vec<ultrafast_render::geom::FontPin> = resolved
            .font_files
            .iter()
            .map(|(pin, path)| ultrafast_render::geom::FontPin {
                family: pin.family.clone(),
                weight: pin.weight.min(1000) as u16,
                italic: pin.italic,
                path: path.clone(),
            })
            .collect();
        if pins.is_empty() {
            return Ok(());
        }
        self.renderer
            .pin_fonts(&pins)
            .map_err(|e| ApiError::new(422, "input_unavailable", format!("meta.fonts: {e}")).with("reason", "font"))?;
        // After pinning, which clears them: they name the faces just pinned.
        self.renderer.set_script_fonts(resolved.script_fonts.clone());
        Ok(())
    }

    /// Runs `f`; a panic (a bug, or the GPU lost) rebuilds the renderer and
    /// fails the request instead of the instance.
    fn guarded<T>(&mut self, f: impl FnOnce(&mut Renderer) -> Result<T, ApiError>) -> Result<T, ApiError> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut self.renderer))) {
            Ok(r) => r,
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                match Device::new()
                    .map_err(|e| e.to_string())
                    .and_then(|d| Renderer::new(d).map_err(|e| e.to_string()))
                {
                    Ok(r) => self.renderer = r,
                    Err(e) => eprintln!("ultrafast-service: renderer rebuild failed: {e}"),
                }
                Err(ApiError::new(
                    500,
                    "render_failed",
                    format!("render failed: {msg} (renderer rebuilt)"),
                ))
            }
        }
    }
}

impl Engine {
    /// `scheme://bucket/name`, or for the local store its `file://` path.
    fn uri_of(&self, scheme: &str, bucket: &str, name: &str) -> String {
        if scheme == "file" {
            self.cfg.storage.uri(name)
        } else {
            format!("{scheme}://{bucket}/{name}")
        }
    }

    fn identity(&self, scheme: &str, obj: &ObjectMeta, sha256: &str) -> ObjectIdentity {
        ObjectIdentity {
            uri: self.uri_of(scheme, &obj.bucket, &obj.name),
            bucket: obj.bucket.clone(),
            name: obj.name.clone(),
            generation: obj.generation.clone(),
            size: obj.size,
            crc32c: obj.crc32c.clone().unwrap_or_default(),
            md5: obj.md5.clone().unwrap_or_default(),
            sha256: sha256.to_string(),
            content_type: obj.content_type.clone().unwrap_or_default(),
        }
    }
}

fn store_down(store: &dyn Store, e: &anyhow::Error) -> ApiError {
    crate::records::store_down(store, e)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn internal(e: &dyn std::fmt::Display) -> ApiError {
    ApiError::new(500, "render_failed", e.to_string())
}

/// Every file param of the document (components expanded) reads from
/// `dir`; font params may also name families.
fn only_reads_from(doc: &Document, dir: &Path) -> Result<(), ApiError> {
    let flat;
    let nodes = if doc.needs_flatten() {
        flat = doc.flatten().map_err(|e| bad(e.to_string()))?;
        &flat.doc.nodes
    } else {
        &doc.nodes
    };
    let root = dir.to_string_lossy();
    let inside = |s: &str| s.starts_with(&*root) && !s[root.len()..].split('/').any(|seg| seg == "..");
    for (id, node) in nodes {
        for (k, v) in &node.params {
            // An audio driver reads its source, whatever param it drives.
            if let Param::Audio { audio } = v {
                if !inside(&audio.src) {
                    return Err(ApiError::new(
                        403,
                        "local_path_not_allowed",
                        "audio driver sources must be references (asset://, https://, gs://, s3://, r2://)",
                    )
                    .with("node", id)
                    .with("param", k)
                    .with("value", &audio.src));
                }
                continue;
            }
            if !crate::refs::FILE_PARAMS.contains(&(node.op.as_str(), k.as_str())) {
                continue;
            }
            let Param::Text(s) = v else { continue };
            if s.is_empty()
                || (crate::refs::is_font_param(k) && crate::refs::is_family_name(s))
                || (k == "faces" && ultrafast_framing::follow::faces_inline(s))
            {
                continue;
            }
            if !inside(s) {
                return Err(
                    ApiError::new(403, "local_path_not_allowed", "file params must be gs:// references")
                        .with("node", id)
                        .with("param", k)
                        .with("value", s),
                );
            }
        }
    }
    Ok(())
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(out)
}

fn encode_jpeg(w: u32, h: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    // PNG is lossless and the default; JPEG through ffmpeg keeps this crate
    // free of an image codec stack.
    let png = encode_png(w, h, rgba)?;
    let mut child = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-nostdin", "-f", "png_pipe", "-i", "-", "-q:v", "3", "-f", "mjpeg", "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &png));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if !out.status.success() || out.stdout.is_empty() {
        anyhow::bail!("JPEG encoding failed");
    }
    Ok(out.stdout)
}

/// What `materialize` gives a GPU-side closure: the document as it renders
/// (inputs local), the directory it reads from, and the inputs record.
pub struct Materialized {
    pub doc: Document,
    pub inputs: Vec<InputRecord>,
    pub fetch_s: f64,
    /// What the document's check warns about, as `issues` list them.
    pub warnings: Vec<Value>,
}

impl Engine {
    /// Checks `document`, resolves its inputs for `range` (or the one
    /// instant `at`) and runs `f` on the renderer with the request's
    /// directory as its base (fonts pinned for this call only).
    pub fn with_document<T>(
        &mut self,
        document: &Value,
        range: Option<[f64; 2]>,
        at: Option<f64>,
        grants: &Grants,
        f: impl FnOnce(&mut Renderer, &Materialized) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut p = self.prepare(document, range)?;
        if let Some(t) = at {
            p.from = t.clamp(0.0, p.doc.format.duration);
            p.to = p.from;
        }
        let dir = self.cache.request_dir().map_err(|e| internal(&e))?;
        let _scratch = Scratch(dir.clone());
        let t0 = Instant::now();
        self.stores
            .set_deadline(Some(t0 + Duration::from_secs_f64(self.cfg.limits.wall_s)));
        let materialized = self.materialize(document, &p, grants, &dir);
        self.stores.set_deadline(None);
        let (doc, resolved) = materialized?;
        self.pin_fonts(&resolved)?;
        let m = Materialized {
            doc,
            inputs: resolved.inputs.clone(),
            fetch_s: t0.elapsed().as_secs_f64(),
            warnings: p.warnings,
        };
        let out = self.guarded(|r| {
            r.set_base_dir(&dir);
            r.prefetch_fonts(&m.doc);
            f(r, &m)
        });
        self.renderer.unpin_fonts();
        self.renderer.release_document();
        out
    }

    /// `POST /v1/sheet`.
    pub fn sheet(&mut self, req: SheetRequest, grants: &Grants) -> Result<Reply, ApiError> {
        let format = req.format;
        let t0 = Instant::now();
        let (w, h, px, inputs, fetch_s, warnings) = self.with_document(&req.document, None, None, grants, |r, m| {
            let d = m.doc.format.duration;
            let times: Vec<f64> = match &req.at {
                Some(at) if !at.is_empty() => {
                    if at.len() > 64 {
                        return Err(bad("`at` takes at most 64 times"));
                    }
                    if let Some(t) = at.iter().find(|t| !(**t >= 0.0 && **t <= d)) {
                        return Err(bad(format!("time {t} is outside the document's 0..{d} s")));
                    }
                    at.clone()
                }
                _ => {
                    let n = req.count.unwrap_or(9).clamp(1, 64);
                    (0..n).map(|i| d * (i as f64 + 0.5) / n as f64).collect()
                }
            };
            let fmt = m.doc.format;
            let cols = req
                .columns
                .unwrap_or(if fmt.height > fmt.width { 5 } else { 3 })
                .clamp(1, 16);
            let (w, h, px) = ultrafast_live::contact_sheet(r, &m.doc, &times, cols)
                .map_err(|e| ApiError::new(500, "render_failed", format!("render failed: {e}")))?;
            let warnings = warning_lines(&m.warnings, r.diagnostics());
            Ok((w, h, px, m.inputs.clone(), m.fetch_s, warnings))
        })?;
        let (body, content_type) = match format {
            ImageFormat::Png => (encode_png(w, h, &px).map_err(|e| internal(&e))?, "image/png"),
            ImageFormat::Jpeg => (encode_jpeg(w, h, &px).map_err(|e| internal(&e))?, "image/jpeg"),
        };
        let header = serde_json::json!({
            "inputs": inputs,
            "timings": {"fetch_s": fetch_s, "total_s": t0.elapsed().as_secs_f64()},
            "width": w, "height": h,
            "warnings": warnings,
        });
        Ok(Reply::Bytes {
            content_type,
            body,
            headers: vec![("X-Ultrafast-Receipt".into(), header.to_string())],
        })
    }

    /// `POST /v1/check`: the document's problems as `ultrafast check` finds
    /// them (schema, then a first frame for plan and shader errors).
    pub fn check(&mut self, req: CheckRequest, grants: &Grants) -> Result<Reply, ApiError> {
        let answer = |issues: Vec<Value>| {
            let errors = issues.iter().filter(|i| i["severity"] == "error").count();
            Ok(Reply::Json(
                200,
                serde_json::json!({"ok": errors == 0, "issues": issues}).to_string(),
            ))
        };
        let text = canonical_json(&req.document);
        if text.len() > self.cfg.limits.document_bytes {
            return Err(ApiError::new(413, "over_limit", "the document is too large")
                .with("limit", "document_bytes")
                .with("value", text.len())
                .with("max", self.cfg.limits.document_bytes));
        }
        let doc = match Document::from_json(&text) {
            Ok(d) => d,
            Err(e) => {
                return answer(vec![
                    serde_json::json!({"node": "", "severity": "error", "message": e.to_string()}),
                ]);
            }
        };
        let mut issues: Vec<Value> = ultrafast_render::schema::check(&doc)
            .into_iter()
            .map(|i| serde_json::json!({"node": i.node, "severity": i.severity, "message": i.message}))
            .collect();
        if issues.iter().any(|i| i["severity"] == "error") {
            return answer(issues);
        }
        let rendered = self.with_document(&req.document, None, Some(0.0), grants, |r, m| {
            let mut found = Vec::new();
            if let Err(e) = r.render_rgba8(&m.doc, 0.0) {
                found.push(serde_json::json!({"node": "", "severity": "error", "message": e.to_string()}));
            }
            for d in r.diagnostics() {
                found.push(serde_json::json!({"node": d.node, "severity": d.severity(), "message": d.message}));
            }
            Ok(found)
        });
        match rendered {
            Ok(found) => issues.extend(found),
            // A request the caller may not make is still an error; an input
            // that cannot be read or a bad limit is a problem with the film.
            Err(e) if matches!(e.status, 413 | 422) || e.code == "invalid_document" || e.code == "bad_request" => {
                issues.push(
                    serde_json::json!({"node": e.extra.get("node").cloned().unwrap_or_default(),
                    "severity": "error", "message": e.message}),
                );
            }
            Err(e) => return Err(e),
        }
        answer(issues)
    }

    /// `POST /v1/review`.
    pub fn review(&mut self, req: ReviewRequest, grants: &Grants) -> Result<Reply, ApiError> {
        let opts = ultrafast_render::review::ReviewOptions {
            step: req.step.unwrap_or(0.1).clamp(0.02, 5.0),
            safe: match req.safe.as_deref() {
                None | Some("margin") => ultrafast_render::review::SafeArea::Margin(0.04),
                Some("social") => ultrafast_render::review::SafeArea::Social,
                Some(o) => return Err(bad(format!("unknown `safe` `{o}` (margin, social)"))),
            },
        };
        let review = self.with_document(&req.document, None, None, grants, |r, m| {
            ultrafast_render::review::review(r, &m.doc, &opts)
                .map_err(|e| ApiError::new(500, "render_failed", format!("review failed: {e}")))
        })?;
        Ok(Reply::Json(200, serde_json::to_string(&review).unwrap_or_default()))
    }

    /// `POST /v1/estimate`.
    pub fn estimate(&mut self, req: EstimateRequest, grants: &Grants) -> Result<Reply, ApiError> {
        let range = req.range.or(req.output.as_ref().and_then(|o| o.range));
        let output = req.output.clone();
        let usd_per_minute = self.cfg.usd_per_minute;
        let est = self.with_document(&req.document, range, None, grants, |r, m| {
            // Priced and timed as the render request's `output` asks.
            let target = match &output {
                Some(o) => Some(
                    o.settings()
                        .resolve(o.named_path(), &m.doc.format)
                        .map_err(|e| ApiError::new(400, "bad_request", e))?,
                ),
                None => None,
            };
            let opts = ultrafast_live::estimate::Options {
                from: range.map(|r| r[0]),
                to: range.map(|r| r[1]),
                usd_per_minute,
                target,
                ..Default::default()
            };
            ultrafast_live::estimate::estimate(r, &m.doc, &opts)
                .map_err(|e| ApiError::new(500, "render_failed", format!("estimate failed: {e}")))
        })?;
        Ok(Reply::Json(200, serde_json::to_string(&est).unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_read_like_the_cli_and_stay_short() {
        let issues = vec![
            serde_json::json!({"node": "line1", "severity": "warning",
                "message": "`text` has no param `txt` (did you mean `text`?); it is ignored"}),
            serde_json::json!({"node": "", "severity": "warning", "message": "whole document"}),
        ];
        let found = [
            ("title", "text: never shows in full"),
            (
                "line1",
                "`text` has no param `txt` (did you mean `text`?); it is ignored",
            ),
        ]
        .map(|(node, message)| Diagnostic {
            node: node.into(),
            message: message.into(),
        });
        assert_eq!(
            warning_lines(&issues, &found),
            [
                "node `line1`: `text` has no param `txt` (did you mean `text`?); it is ignored",
                "whole document",
                "node `title`: text: never shows in full",
            ]
        );
        let many: Vec<Value> = (0..30)
            .map(|i| serde_json::json!({"node": format!("n{i}"), "message": "x"}))
            .collect();
        let lines = warning_lines(&many, &[]);
        assert_eq!(lines.len(), MAX_WARNINGS);
        assert_eq!(lines[MAX_WARNINGS - 1], "and 11 more");
    }

    #[test]
    fn stored_names_follow_the_format() {
        assert_eq!(output_name(None, Container::Mp4), "film.mp4");
        assert_eq!(output_name(None, Container::Webm), "film.webm");
        assert_eq!(output_name(None, Container::Png), "frames.zip");
        assert_eq!(output_name(Some("teaser"), Container::Gif), "teaser.gif");
        assert_eq!(output_name(Some("teaser.m4v"), Container::Mp4), "teaser.m4v");
        assert_eq!(output_name(Some("cut.mov"), Container::Mov), "cut.mov");
        assert_eq!(output_name(Some("plates"), Container::Exr), "plates.zip");
        assert_eq!(output_name(Some("plates.zip"), Container::Exr), "plates.zip");
    }
}
