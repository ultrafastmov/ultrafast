//! Resolving a document's references to local files: every one authorized
//! against the caller's read prefixes first, then fetched (whole, as a
//! sparse copy of the byte ranges the render uses, or unpacked when it is
//! an archive) into a bounded cache keyed by object and generation, and
//! linked into a directory of the request's own, which is the only place
//! the rewritten document may read from.

use crate::api::{ApiError, FontRecord, InputRecord};
use crate::config::{Config, Grants};
use crate::mp4ranges as mr;
use crate::refs::{self, Ref};
use crate::store::{ObjectMeta, Storage, Store, Stores};
use crate::usage::Usage;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

/// Concurrent range requests per object (ranges larger than the
/// configured part size go in parts).
const FETCHERS: usize = 8;
/// Objects resolved at once (each with its own parts in flight): every
/// request to a bucket in another region costs a round trip of a few
/// hundred milliseconds, so they overlap rather than queue.
const OBJECTS: usize = 8;

/// A font pin from `meta.fonts`.
#[derive(Debug, Clone)]
pub struct FontPin {
    pub family: String,
    pub weight: u32,
    pub italic: bool,
    pub src: String,
    pub sha256: Option<String>,
}

pub struct Resolved {
    /// Param string -> local path inside the request's directory.
    pub map: HashMap<String, String>,
    pub inputs: Vec<InputRecord>,
    pub fonts: Vec<FontRecord>,
    /// Pinned font files, for the renderer to register for this render.
    pub font_files: Vec<(FontPin, PathBuf)>,
    /// `meta.script_fonts`: how this document sets writing systems.
    pub script_fonts: Vec<ultrafast_render::geom::ScriptFont>,
    pub fetch_s: f64,
    pub unpack_s: f64,
    pub downloaded: u64,
}

/// The cache: files by object, with sizes and last use (this process
/// made every file in it, so the index is complete).
pub struct Cache {
    pub dir: PathBuf,
    pub limit: u64,
    index: Mutex<HashMap<PathBuf, SystemTime>>,
    /// Unpacked members already checked against a hint: (path, sha256).
    verified: Mutex<std::collections::HashSet<(PathBuf, String)>>,
    /// Metadata of pinned objects this process has read, by their
    /// canonical reference (`gs://b/o#generation`, `asset://ID`). A pinned
    /// version's bytes never change, so a cached copy needs no new request
    /// (the caller's prefixes are still checked first).
    meta: Mutex<HashMap<String, ObjectMeta>>,
}

impl Cache {
    pub fn new(dir: &Path, limit: u64) -> std::io::Result<Self> {
        // A fresh instance starts empty; leftovers of a crashed process are
        // not trusted.
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir.join("objects"))?;
        std::fs::create_dir_all(dir.join("req"))?;
        Ok(Cache {
            dir: dir.to_path_buf(),
            limit,
            index: Mutex::new(HashMap::new()),
            verified: Mutex::new(Default::default()),
            meta: Mutex::new(HashMap::new()),
        })
    }

    /// Where an object's bytes live, by a key naming exactly one content.
    fn path(&self, key: &str, ext: &str) -> PathBuf {
        let key = crate::util::sha256_hex(key.as_bytes());
        self.dir.join("objects").join(format!("{}{ext}", &key[..32]))
    }

    fn touch(&self, p: &Path) {
        self.index.lock().unwrap().insert(p.to_path_buf(), SystemTime::now());
    }

    /// A new directory for one request.
    pub fn request_dir(&self) -> std::io::Result<PathBuf> {
        let nonce = crate::util::sha256_hex(
            format!("{:?}{}{}", SystemTime::now(), std::process::id(), crate::util::unique()).as_bytes(),
        );
        let d = self.dir.join("req").join(&nonce[..24]);
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }

    /// Evicts least recently used entries until the cache fits its bound
    /// (entries `keep` uses are spared).
    pub fn trim(&self, keep: &[PathBuf]) {
        let mut index = self.index.lock().unwrap();
        let size = |p: &Path| -> u64 { disk_bytes(p) };
        let mut entries: Vec<(PathBuf, SystemTime, u64)> =
            index.iter().map(|(p, t)| (p.clone(), *t, size(p))).collect();
        let mut total: u64 = entries.iter().map(|e| e.2).sum();
        if total <= self.limit {
            return;
        }
        entries.sort_by_key(|e| e.1);
        for (p, _, bytes) in entries {
            if total <= self.limit {
                break;
            }
            if keep.iter().any(|k| k == &p) {
                continue;
            }
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
                self.verified.lock().unwrap().retain(|(f, _)| !f.starts_with(&p));
            } else {
                let _ = std::fs::remove_file(&p);
                let _ = std::fs::remove_file(p.with_extension(ranges_ext(&p)));
            }
            index.remove(&p);
            total = total.saturating_sub(bytes);
        }
    }
}

fn ranges_ext(p: &Path) -> String {
    match p.extension() {
        Some(e) => format!("{}.ranges", e.to_string_lossy()),
        None => "ranges".into(),
    }
}

/// Bytes a file or directory occupies on disk (sparse files count what
/// they hold).
fn disk_bytes(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p)
            .map(|d| d.flatten().map(|e| disk_bytes(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) => allocated(&m),
        Err(_) => 0,
    }
}

#[cfg(unix)]
fn allocated(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.blocks() * 512
}

/// Without block counts, a file's length.
#[cfg(not(unix))]
fn allocated(m: &std::fs::Metadata) -> u64 {
    m.len()
}

/// Writes all of `buf` at `offset`, so parallel range reads can fill one file.
#[cfg(unix)]
fn write_at(f: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, offset)
}

#[cfg(windows)]
fn write_at(f: &std::fs::File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Every param string reading an object, with the archive member it names.
type Uses = Vec<(String, Option<String>)>;

/// Bytes fetched: the request's total (bounded by the input limit) and one
/// object's own.
struct Tally<'a> {
    total: &'a AtomicU64,
    mine: AtomicU64,
}

/// One object resolved: its param strings' local paths and its record.
struct One {
    map: Vec<(String, String)>,
    input: InputRecord,
    unpack_s: f64,
}

pub struct Resolver<'a> {
    pub cfg: &'a Config,
    pub stores: &'a Stores,
    pub storage: &'a Storage,
    pub cache: &'a Cache,
}

/// Where one object's bytes come from.
struct Src<'a> {
    store: &'a dyn Store,
    bucket: String,
    name: String,
}

fn unavailable(location: &str, reason: &str, message: String) -> ApiError {
    ApiError::new(422, "input_unavailable", message)
        .with("location", location)
        .with("reason", reason)
}

impl Resolver<'_> {
    /// Resolves every reference in `doc` (param strings) for a render that
    /// uses them as `usage` says, into `dir`.
    pub fn resolve(
        &self,
        doc: &Value,
        usage: &HashMap<String, Usage>,
        grants: &Grants,
        dir: &Path,
    ) -> Result<Resolved, ApiError> {
        let t0 = Instant::now();
        let hints = archive_hints(doc)?;
        let strings = refs::collect(doc);
        // Parse and authorize everything before fetching anything.
        // Per object: its reference and every param string reading it (with
        // the archive member, if one).
        let mut objects: BTreeMap<String, (Ref, Uses)> = BTreeMap::new();
        for s in &strings {
            let r = refs::parse(s).map_err(|e| ApiError::new(400, "bad_request", format!("`{s}`: {e}")))?;
            match &r {
                Ref::Asset { .. } => {}
                Ref::Object { .. } => {
                    return Err(ApiError::cloud_only(
                        "bucket_inputs",
                        format!(
                            "{}: reading inputs from buckets (gs://, s3://, r2://) is an ultrafast Cloud feature; \
                             upload the file (POST /v1/uploads) and use its asset:// id, or an https:// URL",
                            r.location()
                        ),
                    )
                    .with("location", s));
                }
                Ref::Https { url } => {
                    let host = refs::host_of(url).unwrap_or_default();
                    if !self.cfg.https_allowed(&host) {
                        return Err(ApiError::new(
                            403,
                            "input_not_allowed",
                            format!("https inputs from `{host}` are not allowed here"),
                        )
                        .with("location", s));
                    }
                }
            }
            let member = r.member().map(str::to_string);
            objects
                .entry(r.object())
                .or_insert_with(|| (r.clone(), Vec::new()))
                .1
                .push((s.clone(), member));
        }
        let downloaded = AtomicU64::new(0);
        let mut out = Resolved {
            map: HashMap::new(),
            inputs: Vec::new(),
            fonts: Vec::new(),
            font_files: Vec::new(),
            script_fonts: Vec::new(),
            fetch_s: 0.0,
            unpack_s: 0.0,
            downloaded: 0,
        };
        // Every object at once (bounded): a cache hit costs nothing, and the
        // rest overlap their round trips.
        let list: Vec<(usize, &String, &Ref, &Uses)> = objects
            .iter()
            .enumerate()
            .map(|(k, (o, (r, u)))| (k, o, r, u))
            .collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Result<One, ApiError>>>> = Mutex::new((0..list.len()).map(|_| None).collect());
        std::thread::scope(|s| {
            for _ in 0..OBJECTS.min(list.len()) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(k, object, r, uses)) = list.get(i) else {
                            break;
                        };
                        // Once one object failed, the rest are not worth fetching.
                        if results.lock().unwrap().iter().any(|x| matches!(x, Some(Err(_)))) {
                            break;
                        }
                        let one = self.resolve_one(k, object, r, uses, usage, &hints, dir, &downloaded, grants);
                        results.lock().unwrap()[i] = Some(one);
                    }
                });
            }
        });
        let mut unpack_s = 0.0;
        for one in results.into_inner().unwrap() {
            // In the documents' order: the first failure is the one reported.
            let Some(one) = one else { continue };
            let one = one?;
            out.map.extend(one.map);
            out.inputs.push(one.input);
            unpack_s += one.unpack_s;
        }
        if out.inputs.len() != list.len() {
            // Stopped early by a failure reported above; never reached.
            return Err(ApiError::new(500, "render_failed", "an input was not resolved"));
        }
        out.fetch_s = t0.elapsed().as_secs_f64() - unpack_s;
        out.unpack_s = unpack_s;
        out.downloaded = downloaded.load(Ordering::Relaxed);
        // Font pins: verified against their hashes, recorded.
        for pin in font_pins(doc)? {
            let Some(local) = out.map.get(&pin.src).cloned() else {
                continue;
            };
            let bytes = std::fs::read(&local).map_err(|e| unavailable(&pin.src, "font", e.to_string()))?;
            let sha = crate::util::sha256_hex(&bytes);
            if let Some(want) = &pin.sha256
                && !want.eq_ignore_ascii_case(&sha)
            {
                return Err(unavailable(
                    &pin.src,
                    "integrity",
                    format!("font {} has sha256 {sha}, the pin says {want}", pin.src),
                ));
            }
            out.fonts.push(FontRecord {
                family: pin.family.clone(),
                weight: pin.weight,
                style: if pin.italic { "italic" } else { "normal" }.into(),
                uri: pin.src.clone(),
                sha256: sha,
            });
            out.font_files.push((pin, PathBuf::from(local)));
        }
        out.script_fonts = script_fonts(doc)?;
        Ok(out)
    }

    /// Fetches (or finds in the cache) one object and links it into `dir`.
    #[allow(clippy::too_many_arguments)]
    fn resolve_one(
        &self,
        k: usize,
        object: &str,
        r: &Ref,
        uses: &Uses,
        usage: &HashMap<String, Usage>,
        hints: &HashMap<String, BTreeMap<String, MemberHint>>,
        dir: &Path,
        downloaded: &AtomicU64,
        grants: &Grants,
    ) -> Result<One, ApiError> {
        let downloaded = &Tally {
            total: downloaded,
            mine: AtomicU64::new(0),
        };
        let link_base = dir.join(format!("in{k}"));
        let mut map = Vec::new();
        let mut unpack_s = 0.0;
        let input = match r {
            Ref::Https { url } => {
                let local = link_base.with_extension(r.extension().trim_start_matches('.'));
                let size = self.fetch_https(url, &local, downloaded)?;
                for (s, _) in uses {
                    map.push((s.clone(), local.to_string_lossy().into_owned()));
                }
                InputRecord {
                    uri: url.clone(),
                    size,
                    bytes_read: size,
                    partial: false,
                    entries: None,
                }
            }
            Ref::Object { .. } | Ref::Asset { .. } => {
                let (src, meta, ext, cache_key) = self.locate(r, object, grants)?;
                let archive = uses.iter().any(|(_, m)| m.is_some()) || ext == ".zip";
                let media = matches!(ext.as_str(), ".mp4" | ".mov" | ".m4v" | ".m4a");
                let mut windows = Vec::new();
                let mut whole = false;
                for (s, _) in uses {
                    match usage.get(s) {
                        Some(u) => {
                            whole |= u.whole;
                            windows.extend(u.windows.iter().copied());
                        }
                        None => whole = true,
                    }
                }
                let cached = self.cache.path(&cache_key, &ext);
                let mut partial = false;
                let mut entries = None;
                if archive {
                    self.fetch_whole(&src, &meta, &cached, downloaded, object)?;
                    let unpacked = cached.with_extension("d");
                    if !unpacked.exists() {
                        let u0 = Instant::now();
                        crate::archive::unpack(&cached, &unpacked, &self.cfg.limits)
                            .map_err(|e| unavailable(object, "archive", format!("{object}: {e:#}")))?;
                        unpack_s += u0.elapsed().as_secs_f64();
                    }
                    self.cache.touch(&unpacked);
                    entries = Some(std::fs::read_dir(&unpacked).map(|d| d.count() as u64).unwrap_or(0));
                    if let Some(members) = hints.get(object) {
                        let v0 = Instant::now();
                        self.verify_members(object, &unpacked, members)?;
                        unpack_s += v0.elapsed().as_secs_f64();
                    }
                    let link = link_base.with_extension("d");
                    symlink(&unpacked, &link)?;
                    for (s, member) in uses {
                        let target = match member {
                            Some(m) => link.join(m),
                            None => {
                                // The archive itself, not a member.
                                let l = link_base.with_extension("zip");
                                symlink(&cached, &l)?;
                                l
                            }
                        };
                        map.push((s.clone(), target.to_string_lossy().into_owned()));
                    }
                } else {
                    if media && !whole && !windows.is_empty() && meta.size >= self.cfg.partial_min {
                        partial = self.fetch_partial(&src, &meta, &cached, &windows, downloaded, object)?;
                    }
                    if !partial {
                        self.fetch_whole(&src, &meta, &cached, downloaded, object)?;
                    }
                    let link = link_base.with_extension(ext.trim_start_matches('.'));
                    symlink(&cached, &link)?;
                    for (s, _) in uses {
                        map.push((s.clone(), link.to_string_lossy().into_owned()));
                    }
                }
                InputRecord {
                    uri: object.to_string(),
                    size: meta.size,
                    bytes_read: downloaded.mine.load(Ordering::Relaxed),
                    partial,
                    entries,
                }
            }
        };
        Ok(One { map, input, unpack_s })
    }

    /// The store, metadata, file extension and cache key of an object or
    /// asset (an asset must be the caller's).
    fn locate(
        &self,
        r: &Ref,
        object: &str,
        grants: &Grants,
    ) -> Result<(Src<'_>, ObjectMeta, String, String), ApiError> {
        let store_err = |store: &dyn Store, e: anyhow::Error| {
            let mut err = unavailable(object, store.scheme(), format!("{e:#}"));
            err.retry = true;
            err
        };
        match r {
            Ref::Asset { id, .. } => {
                let rec = crate::records::load_asset(self.stores, self.storage, id)?
                    .filter(|a| a.owner == grants.caller || grants.caller == "*")
                    .ok_or_else(|| {
                        ApiError::new(
                            403,
                            "input_not_allowed",
                            format!("{object} is not an upload of this caller's"),
                        )
                        .with("location", object)
                    })?;
                let store = self.stores.main(self.storage);
                let known = self.cache.meta.lock().unwrap().get(object).cloned();
                let meta = match known {
                    Some(m) => m,
                    None => {
                        let m = store
                            .stat(&self.storage.bucket, &rec.object, None)
                            .map_err(|e| store_err(store, e))?
                            .ok_or_else(|| unavailable(object, "not_found", format!("{object}: its bytes are gone")))?;
                        self.cache.meta.lock().unwrap().insert(object.to_string(), m.clone());
                        m
                    }
                };
                let ext = refs::extension_of(&rec.filename);
                Ok((
                    Src {
                        store,
                        bucket: self.storage.bucket.clone(),
                        name: rec.object,
                    },
                    meta,
                    ext,
                    object.to_string(),
                ))
            }
            Ref::Object {
                scheme,
                bucket,
                name,
                generation,
                ..
            } => {
                let store = self.stores.get(scheme).ok_or_else(|| {
                    ApiError::new(400, "bad_request", format!("{scheme}:// inputs are not available here"))
                })?;
                let pinned = generation.is_some();
                let known = pinned
                    .then(|| self.cache.meta.lock().unwrap().get(object).cloned())
                    .flatten();
                let meta = match known {
                    Some(m) => m,
                    None => match store.stat(bucket, name, generation.as_deref()) {
                        Ok(Some(m)) => {
                            if pinned {
                                self.cache.meta.lock().unwrap().insert(object.to_string(), m.clone());
                            }
                            m
                        }
                        Ok(None) => {
                            return Err(unavailable(
                                object,
                                "not_found",
                                match generation {
                                    Some(g) => format!("version {g} of {scheme}://{bucket}/{name} does not exist"),
                                    None => format!("{scheme}://{bucket}/{name} does not exist"),
                                },
                            ));
                        }
                        Err(e) => return Err(store_err(store, e)),
                    },
                };
                // Unpinned objects are cached by the version read.
                let key = if pinned {
                    object.to_string()
                } else {
                    format!("{object}#{}", meta.generation)
                };
                Ok((
                    Src {
                        store,
                        bucket: bucket.clone(),
                        name: name.clone(),
                    },
                    meta,
                    r.extension(),
                    key,
                ))
            }
            Ref::Https { .. } => unreachable!("https inputs are fetched directly"),
        }
    }

    /// Every hinted member of an unpacked archive exists with its size and
    /// sha256 (checked once per member file per instance).
    fn verify_members(&self, object: &str, dir: &Path, members: &BTreeMap<String, MemberHint>) -> Result<(), ApiError> {
        for (name, hint) in members {
            let bad = |reason: &str, message: String| {
                unavailable(&format!("{object}!/{}", refs::encode(name)), reason, message)
            };
            if name.is_empty()
                || name.starts_with('/')
                || name.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
            {
                return Err(bad("integrity", format!("hint for `{name}`: not a member name")));
            }
            let path = dir.join(name);
            let Ok(meta) = std::fs::metadata(&path) else {
                return Err(bad("integrity", format!("{object} has no member `{name}`")));
            };
            if let Some(size) = hint.size
                && meta.len() != size
            {
                return Err(bad(
                    "integrity",
                    format!("{object}!/{name} is {} bytes, the hint says {size}", meta.len()),
                ));
            }
            if let Some(want) = &hint.sha256 {
                let key = (path.clone(), want.to_ascii_lowercase());
                if self.cache.verified.lock().unwrap().contains(&key) {
                    continue;
                }
                let got = crate::util::hash_file(&path)
                    .map_err(|e| bad("cache", e.to_string()))?
                    .sha256;
                if !got.eq_ignore_ascii_case(want) {
                    return Err(bad(
                        "integrity",
                        format!("{object}!/{name} has sha256 {got}, the hint says {want}"),
                    ));
                }
                self.cache.verified.lock().unwrap().insert(key);
            }
        }
        Ok(())
    }

    fn charge(&self, downloaded: &Tally, n: u64) -> Result<(), ApiError> {
        downloaded.mine.fetch_add(n, Ordering::Relaxed);
        let total = downloaded.total.fetch_add(n, Ordering::Relaxed) + n;
        if total > self.cfg.limits.input_bytes {
            return Err(ApiError::new(
                413,
                "over_limit",
                format!("inputs need more than {} bytes", self.cfg.limits.input_bytes),
            )
            .with("limit", "input_bytes")
            .with("value", total)
            .with("max", self.cfg.limits.input_bytes));
        }
        Ok(())
    }

    /// The whole object in the cache (fetched unless already there whole),
    /// its CRC32C checked against GCS's.
    fn fetch_whole(
        &self,
        src: &Src,
        meta: &ObjectMeta,
        cached: &Path,
        downloaded: &Tally,
        object: &str,
    ) -> Result<(), ApiError> {
        let (store, bucket, name, generation) = (src.store, &src.bucket, &src.name, &meta.generation);
        let sidecar = cached.with_extension(ranges_ext(cached));
        if cached.exists() && !sidecar.exists() {
            self.cache.touch(cached);
            return Ok(());
        }
        self.charge(downloaded, meta.size)?;
        let tmp = cached.with_extension(format!("part{}", crate::util::unique()));
        let size = meta.size;
        let result = (|| -> anyhow::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            let part = self.cfg.part_bytes;
            if size <= part {
                let n = store.read(bucket, name, generation, None, &mut f)?;
                if n != size {
                    anyhow::bail!("read {n} of {size} bytes");
                }
                return Ok(());
            }
            // In parts over several connections: one stream to a bucket in
            // another region tops out at a few tens of MB/s.
            f.set_len(size)?;
            let parts: Vec<(u64, u64)> = (0..size.div_ceil(part))
                .map(|i| (i * part, ((i + 1) * part).min(size)))
                .collect();
            let next = AtomicU64::new(0);
            let errors = Mutex::new(Vec::new());
            std::thread::scope(|s| {
                for _ in 0..FETCHERS.min(parts.len()) {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed) as usize;
                            let Some(&(a, b)) = parts.get(i) else { break };
                            if !errors.lock().unwrap().is_empty() {
                                break;
                            }
                            let mut buf = Vec::with_capacity((b - a) as usize);
                            let r = store
                                .read(bucket, name, generation, Some((a, b)), &mut buf)
                                .and_then(|n| {
                                    if n != b - a {
                                        anyhow::bail!("read {n} of {} bytes at {a}", b - a);
                                    }
                                    Ok(write_at(&f, &buf, a)?)
                                });
                            if let Err(e) = r {
                                errors.lock().unwrap().push(e);
                            }
                        }
                    });
                }
            });
            match errors.into_inner().unwrap().into_iter().next() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            let mut err = unavailable(object, store.scheme(), format!("{object}: {e:#}"));
            err.retry = true;
            return Err(err);
        }
        // The store's own checksum, where it gives one (local disk is ours).
        if store.scheme() != "file" {
            let check = match (&meta.crc32c, &meta.md5) {
                (Some(want), _) => Some(("CRC32C", want, crate::util::crc32c_file(&tmp).unwrap_or_default())),
                (None, Some(want)) => Some((
                    "MD5",
                    want,
                    crate::util::hash_file(&tmp).map(|h| h.md5).unwrap_or_default(),
                )),
                _ => None,
            };
            if let Some((kind, want, got)) = check
                && &got != want
            {
                let _ = std::fs::remove_file(&tmp);
                return Err(unavailable(
                    object,
                    "integrity",
                    format!("{object}: {kind} {got}, the store says {want}"),
                ));
            }
        }
        let _ = std::fs::remove_file(&sidecar);
        std::fs::rename(&tmp, cached).map_err(|e| unavailable(object, "cache", e.to_string()))?;
        self.cache.touch(cached);
        Ok(())
    }

    /// A sparse copy holding the file's head, top-level box headers, its
    /// index and the samples inside `windows`. False if the file is not a
    /// plain MP4/MOV (the caller fetches it whole).
    fn fetch_partial(
        &self,
        src: &Src,
        meta: &ObjectMeta,
        cached: &Path,
        windows: &[(f64, f64)],
        downloaded: &Tally,
        object: &str,
    ) -> Result<bool, ApiError> {
        let (store, bucket, name, generation) = (src.store, &src.bucket, &src.name, &meta.generation);
        let sidecar = cached.with_extension(ranges_ext(cached));
        if cached.exists() && !sidecar.exists() {
            // Already here whole.
            self.cache.touch(cached);
            return Ok(false);
        }
        let size = meta.size;
        let fail = |e: anyhow::Error| {
            let mut err = unavailable(object, store.scheme(), format!("{object}: {e:#}"));
            err.retry = true;
            err
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(cached)
            .map_err(|e| unavailable(object, "cache", e.to_string()))?;
        file.set_len(size)
            .map_err(|e| unavailable(object, "cache", e.to_string()))?;
        let have = Mutex::new(mr::load_ranges(&sidecar));
        let fetch = |want: Vec<(u64, u64)>| -> Result<(), ApiError> {
            let need = mr::missing(&want, &have.lock().unwrap());
            if need.is_empty() {
                return Ok(());
            }
            self.charge(downloaded, mr::total(&need))?;
            // Large ranges in parts, all of it over a few connections.
            let mut parts = Vec::new();
            let part = self.cfg.part_bytes;
            for (a, b) in need {
                let mut x = a;
                while x < b {
                    parts.push((x, (x + part).min(b)));
                    x += part;
                }
            }
            let next = AtomicU64::new(0);
            let errors = Mutex::new(Vec::new());
            std::thread::scope(|s| {
                for _ in 0..FETCHERS.min(parts.len()) {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed) as usize;
                            let Some(&(a, b)) = parts.get(i) else { break };
                            let mut buf = Vec::with_capacity((b - a) as usize);
                            match store.read(bucket, name, generation, Some((a, b)), &mut buf) {
                                Ok(_) => {
                                    if let Err(e) = write_at(&file, &buf, a) {
                                        errors.lock().unwrap().push(anyhow::anyhow!(e));
                                    }
                                }
                                Err(e) => errors.lock().unwrap().push(e),
                            }
                        }
                    });
                }
            });
            if let Some(e) = errors.into_inner().unwrap().into_iter().next() {
                return Err(fail(e));
            }
            let mut h = have.lock().unwrap();
            let mut all = h.clone();
            all.extend(parts);
            *h = mr::merge(all, 0);
            mr::save_ranges(&sidecar, &h).map_err(|e| unavailable(object, "cache", e.to_string()))?;
            Ok(())
        };
        fetch(vec![(0, mr::HEAD.min(size))])?;
        // Top-level boxes: headers beyond the head fetched as met.
        let boxes = {
            let mut local = std::fs::File::open(cached).map_err(|e| unavailable(object, "cache", e.to_string()))?;
            mr::top_boxes(size, |at, n| {
                if !mr::covered(&have.lock().unwrap(), at, at + n) {
                    fetch(vec![(at, (at + 64 * 1024).min(size))]).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                }
                Ok(mr::read_local(&mut local, at, n)?)
            })
        };
        let Ok(boxes) = boxes else {
            return Ok(false);
        };
        let Some(moov) = boxes.iter().find(|b| &b.kind == b"moov") else {
            return Ok(false);
        };
        if boxes.iter().any(|b| &b.kind == b"moof") {
            // Fragmented: samples are indexed per fragment.
            return Ok(false);
        }
        fetch(vec![(moov.offset, moov.offset + moov.size)])?;
        let parsed = std::fs::File::open(cached)
            .map_err(anyhow::Error::from)
            .and_then(|f| Ok(mp4::Mp4Reader::read_header(std::io::BufReader::new(f), size)?))
            .and_then(|mp4| mr::tracks(&mp4));
        let tracks = match parsed {
            Ok(t) if !t.is_empty() => t,
            _ => return Ok(false),
        };
        fetch(mr::sample_ranges(&tracks, windows))?;
        self.cache.touch(cached);
        Ok(true)
    }

    /// A public https URL to `local`: every hop of a redirect must be an
    /// allowed host that resolves only to public addresses.
    fn fetch_https(&self, url: &str, local: &Path, downloaded: &Tally) -> Result<u64, ApiError> {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                .https_only(true)
                .timeout_connect(Some(std::time::Duration::from_secs(10)))
                .timeout_global(Some(std::time::Duration::from_secs(600)))
                .user_agent(concat!("ultrafast/", env!("CARGO_PKG_VERSION")))
                .build(),
        );
        let mut at = url.to_string();
        let mut resp = None;
        for _ in 0..6 {
            let host = refs::host_of(&at).ok_or_else(|| unavailable(url, "https", format!("bad URL `{at}`")))?;
            if !self.cfg.https_allowed(&host) {
                return Err(ApiError::new(
                    403,
                    "input_not_allowed",
                    format!("https inputs from `{host}` are not allowed here"),
                )
                .with("location", url));
            }
            // Every address the host resolves to must be public.
            let addrs: Vec<std::net::SocketAddr> = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), 443))
                .map(|a| a.collect())
                .unwrap_or_default();
            if addrs.is_empty() || addrs.iter().any(|a| !public(a.ip())) {
                return Err(ApiError::new(
                    403,
                    "input_not_allowed",
                    format!("`{host}` does not resolve to public addresses"),
                )
                .with("location", url));
            }
            let r = agent
                .get(&at)
                .call()
                .map_err(|e| unavailable(url, "https", e.to_string()))?;
            let status = r.status().as_u16();
            if (300..400).contains(&status) {
                let next = r
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| unavailable(url, "https", format!("{at}: HTTP {status} without a Location")))?;
                at = join_url(&at, next);
                if !at.starts_with("https://") {
                    return Err(unavailable(url, "https", format!("redirected off https to {at}")));
                }
                continue;
            }
            if status != 200 {
                return Err(unavailable(url, "https", format!("{at}: HTTP {status}")));
            }
            resp = Some(r);
            break;
        }
        let mut resp = resp.ok_or_else(|| unavailable(url, "https", "too many redirects".into()))?;
        let limit = self.cfg.limits.input_bytes;
        let tmp = local.with_extension(format!("part{}", crate::util::unique()));
        let mut f = std::fs::File::create(&tmp).map_err(|e| unavailable(url, "cache", e.to_string()))?;
        let mut reader = resp.body_mut().with_config().limit(limit + 1).reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut n = 0u64;
        loop {
            let k = match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(k) => k,
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(unavailable(url, "https", e.to_string()));
                }
            };
            if let Err(e) = self.charge(downloaded, k as u64) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            std::io::Write::write_all(&mut f, &buf[..k]).map_err(|e| unavailable(url, "cache", e.to_string()))?;
            n += k as u64;
        }
        drop(f);
        std::fs::rename(&tmp, local).map_err(|e| unavailable(url, "cache", e.to_string()))?;
        Ok(n)
    }
}

/// Resolves a redirect's Location against the URL it came from.
fn join_url(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if let Some(l) = location.strip_prefix("//") {
        return format!("{scheme}://{l}");
    }
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    let path = rest[authority.len()..].split(['?', '#']).next().unwrap_or("");
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    format!("{scheme}://{authority}{dir}/{location}")
}

fn symlink(target: &Path, link: &Path) -> Result<(), ApiError> {
    let _ = std::fs::remove_file(link);
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(target, link);
    // Symlinks need Developer Mode on Windows: else a hard link or a copy.
    #[cfg(windows)]
    let linked = if target.is_dir() {
        let _ = std::fs::remove_dir(link).or_else(|_| std::fs::remove_dir_all(link));
        std::os::windows::fs::symlink_dir(target, link).or_else(|_| copy_dir(target, link))
    } else {
        std::os::windows::fs::symlink_file(target, link)
            .or_else(|_| std::fs::hard_link(target, link))
            .or_else(|_| std::fs::copy(target, link).map(|_| ()))
    };
    linked.map_err(|e| ApiError::new(500, "render_failed", format!("linking an input: {e}")))
}

#[cfg(windows)]
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// Whether an address is on the public internet (not loopback, private,
/// link-local, metadata, carrier-grade NAT, multicast or unspecified).
pub fn public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_multicast()
                || v.is_broadcast()
                || v.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0))
        }
        std::net::IpAddr::V6(v) => {
            let s = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || v.to_ipv4_mapped().is_some_and(|m| !public(std::net::IpAddr::V4(m))))
        }
    }
}

/// What a member of an archive must be (`meta.archives`).
#[derive(Debug, Clone, Default)]
pub struct MemberHint {
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

/// `meta.archives` of a document: per archive (keyed by its canonical
/// `gs://…#generation`), the members and what each must be.
pub fn archive_hints(doc: &Value) -> Result<HashMap<String, BTreeMap<String, MemberHint>>, ApiError> {
    let mut out = HashMap::new();
    let Some(list) = doc.get("meta").and_then(|m| m.get("archives")) else {
        return Ok(out);
    };
    let bad = |m: String| ApiError::new(400, "bad_request", format!("meta.archives: {m}"));
    let list = list
        .as_object()
        .ok_or_else(|| bad("must be an object keyed by archive reference".into()))?;
    for (key, v) in list {
        let r = refs::parse(key).map_err(|e| bad(format!("`{key}`: {e}")))?;
        if r.member().is_some() {
            return Err(bad(format!("`{key}`: name the archive, not a member")));
        }
        let members = v
            .get("members")
            .and_then(Value::as_object)
            .ok_or_else(|| bad(format!("`{key}` needs `members`")))?;
        let mut m = BTreeMap::new();
        for (name, h) in members {
            let sha256 = h.get("sha256").and_then(Value::as_str).map(str::to_string);
            if let Some(s) = &sha256
                && (s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(bad(format!("`{key}` member `{name}`: sha256 must be 64 hex digits")));
            }
            m.insert(
                name.clone(),
                MemberHint {
                    sha256,
                    size: h.get("size").and_then(Value::as_u64),
                },
            );
        }
        out.insert(r.object(), m);
    }
    Ok(out)
}

/// `meta.script_fonts` of a document: which pinned faces set which scripts.
pub fn script_fonts(doc: &Value) -> Result<Vec<ultrafast_render::geom::ScriptFont>, ApiError> {
    let Some(list) = doc.get("meta").and_then(|m| m.get("script_fonts")) else {
        return Ok(Vec::new());
    };
    let bad = |m: String| ApiError::new(400, "bad_request", format!("meta.script_fonts: {m}"));
    let rows: Vec<ultrafast_render::geom::ScriptFont> =
        serde_json::from_value(list.clone()).map_err(|e| bad(e.to_string()))?;
    if rows.len() > 8 {
        return Err(bad("at most 8 rows".into()));
    }
    for row in &rows {
        if row.scripts.is_empty() || row.default.trim().is_empty() {
            return Err(bad("a row needs `scripts` and `default`".into()));
        }
        if !(row.scale > 0.1 && row.scale <= 4.0) || !(row.leading > 0.1 && row.leading <= 4.0) {
            return Err(bad("`scale` and `leading` are out of range".into()));
        }
    }
    Ok(rows)
}

/// `meta.fonts` of a document.
pub fn font_pins(doc: &Value) -> Result<Vec<FontPin>, ApiError> {
    let Some(list) = doc.get("meta").and_then(|m| m.get("fonts")) else {
        return Ok(Vec::new());
    };
    let bad = |m: String| ApiError::new(400, "bad_request", format!("meta.fonts: {m}"));
    let list = list.as_array().ok_or_else(|| bad("must be a list".into()))?;
    list.iter()
        .enumerate()
        .map(|(i, f)| {
            let s = |k: &str| f.get(k).and_then(Value::as_str);
            let family = s("family")
                .filter(|x| !x.is_empty())
                .ok_or_else(|| bad(format!("[{i}] needs `family`")))?;
            let src = s("src").ok_or_else(|| bad(format!("[{i}] needs `src`")))?;
            if !refs::looks_like_ref(src) || src.starts_with("http://") {
                return Err(bad(format!(
                    "[{i}] `src` must be a reference (asset://, https://, gs://, s3:// or r2://)"
                )));
            }
            Ok(FontPin {
                family: family.to_string(),
                weight: f.get("weight").and_then(Value::as_u64).unwrap_or(400) as u32,
                italic: matches!(s("style"), Some("italic") | Some("oblique")),
                src: src.to_string(),
                sha256: s("sha256").map(str::to_string),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_addresses() {
        for a in [
            "10.0.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!public(a.parse().unwrap()), "{a}");
        }
        for a in ["142.250.1.1", "2607:f8b0::1"] {
            assert!(public(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn redirects_resolve() {
        assert_eq!(join_url("https://a.com/x/y.mp4?q=1", "z.mp4"), "https://a.com/x/z.mp4");
        assert_eq!(join_url("https://a.com/x/y.mp4", "/v/z.mp4"), "https://a.com/v/z.mp4");
        assert_eq!(join_url("https://a.com/x", "//cdn.b.com/z"), "https://cdn.b.com/z");
        assert_eq!(join_url("https://a.com/x", "https://c.com/q"), "https://c.com/q");
    }
}
