//! Where the server keeps what it writes: a local directory, behind the
//! [`Store`] interface (renders, uploads and their records, and the signed
//! download URLs for them). Bucket stores (S3, R2, GCS) are ultrafast Cloud's
//! and implement the same trait.
//!
//!   file:///DIR        a local directory (the server's own storage)

use crate::util::{self, FileHashes};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// An object's identity and metadata, whatever the store.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectMeta {
    pub bucket: String,
    pub name: String,
    /// GCS generation, S3 version id (else its ETag), or a local stamp.
    pub generation: String,
    pub size: u64,
    /// Base64, where the store reports one.
    pub crc32c: Option<String>,
    pub md5: Option<String>,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

/// How a create-only write ended.
pub enum Created {
    Done(ObjectMeta),
    /// The object already existed (nothing was written).
    Exists,
}

pub trait Store: Send + Sync {
    fn scheme(&self) -> &'static str;
    /// An object's metadata (a given version, or the live one); None if it
    /// does not exist.
    fn stat(&self, bucket: &str, name: &str, version: Option<&str>) -> Result<Option<ObjectMeta>>;
    /// Streams bytes `[start, end)` of a version (the whole object when
    /// `range` is None) into `out`; returns how many were written.
    fn read(
        &self,
        bucket: &str,
        name: &str,
        version: &str,
        range: Option<(u64, u64)>,
        out: &mut dyn Write,
    ) -> Result<u64>;
    /// Creates `name` from `file` only if nothing has that name.
    fn create(
        &self,
        bucket: &str,
        name: &str,
        file: &Path,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        hashes: &FileHashes,
    ) -> Result<Created>;
    /// Creates a small object from bytes, create-only.
    fn create_bytes(&self, bucket: &str, name: &str, bytes: &[u8], content_type: &str) -> Result<Created>;
    /// Writes a small object, replacing what was there.
    fn put_bytes(&self, bucket: &str, name: &str, bytes: &[u8], content_type: &str) -> Result<()>;
    /// A URL anyone may GET the object from until `ttl` passes, if the
    /// store can sign one itself (else the server signs its own).
    fn presign(&self, _bucket: &str, _name: &str, _ttl: Duration) -> Option<String> {
        None
    }
    /// Bounds every following call by `d` (a request's deadline).
    fn set_deadline(&self, _d: Option<Instant>) {}
}

/// Reads a whole small object (records), None if it does not exist.
pub fn get_bytes(store: &dyn Store, bucket: &str, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let Some(meta) = store.stat(bucket, name, None)? else {
        return Ok(None);
    };
    if meta.size > limit {
        bail!("{name} is {} bytes (more than {limit})", meta.size);
    }
    let mut buf = Vec::with_capacity(meta.size as usize);
    store.read(bucket, name, &meta.generation, None, &mut buf)?;
    Ok(Some(buf))
}

// ---------------------------------------------------------------- local disk

/// A directory: `NAME` is `ROOT/NAME`, its metadata `ROOT/NAME.ufmeta`.
pub struct Local {
    pub root: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Sidecar {
    generation: String,
    content_type: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    crc32c: Option<String>,
    md5: Option<String>,
}

impl Local {
    pub fn new(root: &Path) -> Result<Local> {
        std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        Ok(Local {
            root: root.canonicalize()?,
        })
    }

    /// The file for `name`, refusing names that climb out of the root.
    pub fn path(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || name.starts_with('/')
            || name.contains('\\')
            || name.contains('\0')
            || name.split('/').any(|s| s.is_empty() || s == "." || s == "..")
            || name.ends_with(".ufmeta")
        {
            bail!("`{name}` is not an object name");
        }
        Ok(self.root.join(name))
    }

    fn sidecar(p: &Path) -> PathBuf {
        let mut s = p.as_os_str().to_owned();
        s.push(".ufmeta");
        PathBuf::from(s)
    }

    fn stamp() -> String {
        let t = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}{:06}", t.as_micros(), util::unique() % 1_000_000)
    }

    fn meta(&self, name: &str, p: &Path) -> Result<Option<ObjectMeta>> {
        let m = match std::fs::metadata(p) {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let side: Sidecar = std::fs::read(Self::sidecar(p))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| Sidecar {
                generation: m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_micros().to_string())
                    .unwrap_or_default(),
                ..Default::default()
            });
        Ok(Some(ObjectMeta {
            bucket: String::new(),
            name: name.to_string(),
            generation: side.generation,
            size: m.len(),
            crc32c: side.crc32c,
            md5: side.md5,
            content_type: side.content_type,
            metadata: side.metadata,
        }))
    }

    /// Links `tmp` at `p` only if nothing is there; removes `tmp` either way.
    fn link_new(tmp: &Path, p: &Path) -> Result<bool> {
        let r = std::fs::hard_link(tmp, p);
        let _ = std::fs::remove_file(tmp);
        match r {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn write_sidecar(p: &Path, side: &Sidecar) -> Result<()> {
        let s = Self::sidecar(p);
        let tmp = s.with_extension(format!("tmp{}", util::unique()));
        std::fs::write(&tmp, serde_json::to_vec(side)?)?;
        std::fs::rename(&tmp, &s)?;
        Ok(())
    }

    fn tmp_for(p: &Path) -> PathBuf {
        let mut s = p.as_os_str().to_owned();
        s.push(format!(".part{}-{}", std::process::id(), util::unique()));
        PathBuf::from(s)
    }
}

impl Store for Local {
    fn scheme(&self) -> &'static str {
        "file"
    }

    fn stat(&self, _bucket: &str, name: &str, version: Option<&str>) -> Result<Option<ObjectMeta>> {
        let p = self.path(name)?;
        let m = self.meta(name, &p)?;
        Ok(m.filter(|m| version.is_none_or(|v| v == m.generation)))
    }

    fn read(
        &self,
        _bucket: &str,
        name: &str,
        _version: &str,
        range: Option<(u64, u64)>,
        out: &mut dyn Write,
    ) -> Result<u64> {
        let p = self.path(name)?;
        let mut f = std::fs::File::open(&p).with_context(|| format!("reading {name}"))?;
        Ok(match range {
            None => std::io::copy(&mut f, out)?,
            Some((a, b)) => {
                if b <= a {
                    return Ok(0);
                }
                f.seek(SeekFrom::Start(a))?;
                std::io::copy(&mut f.take(b - a), out)?
            }
        })
    }

    fn create(
        &self,
        _bucket: &str,
        name: &str,
        file: &Path,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        hashes: &FileHashes,
    ) -> Result<Created> {
        let p = self.path(name)?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if p.exists() {
            return Ok(Created::Exists);
        }
        let tmp = Self::tmp_for(&p);
        // Same filesystem: a link, no copy.
        if std::fs::hard_link(file, &tmp).is_err() {
            std::fs::copy(file, &tmp)?;
        }
        let side = Sidecar {
            generation: Self::stamp(),
            content_type: Some(content_type.to_string()),
            metadata: metadata.clone(),
            crc32c: Some(hashes.crc32c.clone()),
            md5: Some(hashes.md5.clone()),
        };
        if !Self::link_new(&tmp, &p)? {
            return Ok(Created::Exists);
        }
        Self::write_sidecar(&p, &side)?;
        Ok(Created::Done(
            self.meta(name, &p)?.ok_or_else(|| anyhow!("{name} vanished"))?,
        ))
    }

    fn create_bytes(&self, _bucket: &str, name: &str, bytes: &[u8], content_type: &str) -> Result<Created> {
        let p = self.path(name)?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = Self::tmp_for(&p);
        std::fs::write(&tmp, bytes)?;
        if !Self::link_new(&tmp, &p)? {
            return Ok(Created::Exists);
        }
        Self::write_sidecar(
            &p,
            &Sidecar {
                generation: Self::stamp(),
                content_type: Some(content_type.to_string()),
                metadata: BTreeMap::new(),
                crc32c: Some(util::crc32c_b64(bytes)),
                md5: Some(util::md5_b64(bytes)),
            },
        )?;
        Ok(Created::Done(
            self.meta(name, &p)?.ok_or_else(|| anyhow!("{name} vanished"))?,
        ))
    }

    fn put_bytes(&self, _bucket: &str, name: &str, bytes: &[u8], content_type: &str) -> Result<()> {
        let p = self.path(name)?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = Self::tmp_for(&p);
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &p)?;
        Self::write_sidecar(
            &p,
            &Sidecar {
                generation: Self::stamp(),
                content_type: Some(content_type.to_string()),
                ..Default::default()
            },
        )
    }
}

// ---------------------------------------------------------------- the stores

/// Where the server keeps what it writes: a local directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Storage {
    /// `file`.
    pub scheme: String,
    /// Empty for `file`.
    pub bucket: String,
    /// Object name prefix (empty for `file`: the directory is the store's root).
    pub prefix: String,
    /// The local directory.
    pub dir: Option<PathBuf>,
}

/// Why bucket URIs are refused here.
pub const BUCKETS_ARE_CLOUD: &str = "bucket storage (s3://, r2://, gs://) is an ultrafast Cloud feature; \
     this server keeps renders and uploads on local disk";

impl Storage {
    /// Parses `ULTRAFAST_STORAGE`: `file:///dir` or a plain path.
    pub fn parse(s: &str) -> Result<Storage> {
        let s = s.trim();
        if ["s3://", "r2://", "gs://"].iter().any(|p| s.starts_with(p)) {
            bail!("ULTRAFAST_STORAGE `{s}`: {BUCKETS_ARE_CLOUD} (file:///DIR or a directory)");
        }
        let path = s.strip_prefix("file://").unwrap_or(s);
        if path.is_empty() || s.contains("://") && !s.starts_with("file://") {
            bail!("ULTRAFAST_STORAGE `{s}`: use file:///DIR or a directory");
        }
        Ok(Storage {
            scheme: "file".into(),
            bucket: String::new(),
            prefix: String::new(),
            dir: Some(PathBuf::from(path)),
        })
    }

    /// The object name for `rel` (e.g. `renders/ID/film.mp4`).
    pub fn name(&self, rel: &str) -> String {
        format!("{}{rel}", self.prefix)
    }

    /// The URI of `name` in this storage.
    pub fn uri(&self, name: &str) -> String {
        match &self.dir {
            Some(d) => format!("file://{}/{name}", d.display().to_string().trim_end_matches('/')),
            None => format!("{}://{}/{name}", self.scheme, self.bucket),
        }
    }

    pub fn describe(&self) -> String {
        match &self.dir {
            Some(d) => d.display().to_string(),
            None => format!("{}://{}/{}", self.scheme, self.bucket, self.prefix),
        }
    }
}

/// The stores the server uses, by scheme: here, the local directory.
pub struct Stores {
    pub local: Local,
}

impl Stores {
    pub fn new(storage: &Storage) -> Result<Stores> {
        let dir = storage.dir.as_ref().ok_or_else(|| anyhow!("{BUCKETS_ARE_CLOUD}"))?;
        Ok(Stores {
            local: Local::new(dir)?,
        })
    }

    pub fn get(&self, scheme: &str) -> Option<&dyn Store> {
        match scheme {
            "file" => Some(&self.local),
            _ => None,
        }
    }

    /// The store holding the server's own storage.
    pub fn main(&self, _storage: &Storage) -> &dyn Store {
        &self.local
    }

    pub fn set_deadline(&self, d: Option<Instant>) {
        self.local.set_deadline(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_parses() {
        let s = Storage::parse("file:///srv/uf").unwrap();
        assert_eq!(s.dir.as_deref(), Some(Path::new("/srv/uf")));
        assert_eq!(s.uri(&s.name("a")), "file:///srv/uf/a");
        assert_eq!(Storage::parse("/srv/uf").unwrap(), s);
        assert!(Storage::parse("ftp://x/y").is_err());
        let e = Storage::parse("s3://my-bucket/renders").unwrap_err().to_string();
        assert!(e.contains("ultrafast Cloud"), "{e}");
        assert!(Storage::parse("gs://b").is_err() && Storage::parse("r2://b").is_err());
    }

    #[test]
    fn local_is_create_only() {
        let dir = std::env::temp_dir().join(format!(
            "uf-store-{}",
            util::unique() + std::process::id() as u64 * 1000
        ));
        let l = Local::new(&dir).unwrap();
        assert!(matches!(
            l.create_bytes("", "a/b.json", b"{}", "application/json").unwrap(),
            Created::Done(_)
        ));
        assert!(matches!(
            l.create_bytes("", "a/b.json", b"[]", "application/json").unwrap(),
            Created::Exists
        ));
        let m = l.stat("", "a/b.json", None).unwrap().unwrap();
        assert_eq!((m.size, m.content_type.as_deref()), (2, Some("application/json")));
        let mut out = Vec::new();
        l.read("", "a/b.json", &m.generation, Some((1, 2)), &mut out).unwrap();
        assert_eq!(out, b"}");
        l.put_bytes("", "a/b.json", b"[1]", "application/json").unwrap();
        assert_eq!(get_bytes(&l, "", "a/b.json", 100).unwrap().unwrap(), b"[1]");
        assert!(l.path("../x").is_err() && l.path("/etc/passwd").is_err() && l.path("a//b").is_err());
        assert!(l.stat("", "missing", None).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
