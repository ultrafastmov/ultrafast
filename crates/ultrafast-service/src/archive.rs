//! Unpacking an input zip archive (cutout frames) with bounds: entry count,
//! total unpacked size and compression ratio checked from the directory
//! before anything is written; only plain files with safe relative names;
//! every entry's CRC-32 checked as it is read.

use crate::config::Limits;
use anyhow::{Result, bail};
use std::path::Path;

/// Unpacks `zip` into `dir` (created; must not exist). Returns the number
/// of files.
pub fn unpack(zip: &Path, dir: &Path, limits: &Limits) -> Result<u64> {
    let file = std::fs::File::open(zip)?;
    let packed = file.metadata()?.len();
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    if ar.len() > limits.archive_entries {
        bail!("{} entries (at most {})", ar.len(), limits.archive_entries);
    }
    // The directory first: sizes, names and kinds, before writing anything.
    let mut total = 0u64;
    let mut names = Vec::with_capacity(ar.len());
    for i in 0..ar.len() {
        let e = ar.by_index_raw(i)?;
        let shown = e.name().map(|n| n.into_owned()).unwrap_or_else(|_| format!("#{i}"));
        if e.is_dir() {
            names.push(None);
            continue;
        }
        if e.is_symlink() {
            bail!("entry `{shown}` is a link");
        }
        if let Some(mode) = e.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 {
                bail!("entry `{shown}` is not a plain file");
            }
        }
        let Some(rel) = e.enclosed_name() else {
            bail!("entry `{shown}` has an unsafe name");
        };
        if rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            bail!("entry `{shown}` has an unsafe name");
        }
        total = total.saturating_add(e.size());
        names.push(Some(rel));
    }
    if total > limits.archive_bytes {
        bail!("{total} bytes unpacked (at most {})", limits.archive_bytes);
    }
    if packed > 0 && total / packed.max(1) > limits.archive_ratio {
        bail!(
            "compression ratio {} (at most {})",
            total / packed.max(1),
            limits.archive_ratio
        );
    }
    let tmp = dir.with_extension(format!("unpacking{}", crate::util::unique()));
    std::fs::create_dir_all(&tmp)?;
    let result = (|| -> Result<u64> {
        let mut files = 0u64;
        let mut written = 0u64;
        for (i, rel) in names.into_iter().enumerate() {
            let Some(rel) = rel else { continue };
            let out = tmp.join(&rel);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut e = ar.by_index(i)?;
            let mut f = std::fs::File::create(&out)?;
            // At most the size the directory declared (a lying entry stops
            // there); reading to the end checks the CRC.
            let declared = e.size();
            let n = std::io::copy(&mut (&mut e).take(declared + 1), &mut f)?;
            if n > declared {
                bail!("entry `{}` is larger than declared", rel.display());
            }
            written += n;
            if written > limits.archive_bytes {
                bail!("more than {} bytes unpacked", limits.archive_bytes);
            }
            files += 1;
        }
        Ok(files)
    })();
    match result {
        Ok(files) => {
            std::fs::rename(&tmp, dir)?;
            Ok(files)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

use std::io::Read as _;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ultrafast-zip-{}-{}.zip",
            std::process::id(),
            crate::util::unique()
        ));
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        for (name, data) in entries {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        p
    }

    #[test]
    fn unpacks_and_refuses() {
        let limits = Limits::default();
        let z = zip_with(&[("frame_000000.webp", b"a"), ("frame_000001.webp", b"bb")]);
        let dir = z.with_extension("d");
        assert_eq!(unpack(&z, &dir, &limits).unwrap(), 2);
        assert_eq!(std::fs::read(dir.join("frame_000001.webp")).unwrap(), b"bb");
        let bad = zip_with(&[("../escape.txt", b"x")]);
        assert!(unpack(&bad, &bad.with_extension("d"), &limits).is_err());
        let few = Limits {
            archive_entries: 1,
            ..Limits::default()
        };
        assert!(unpack(&z, &z.with_extension("d2"), &few).is_err());
        let bomb = zip_with(&[("big.bin", &vec![0u8; 4 << 20])]);
        let tight = Limits {
            archive_ratio: 100,
            ..Limits::default()
        };
        let err = unpack(&bomb, &bomb.with_extension("d"), &tight)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ratio"), "{err}");
        for p in [z, bad, bomb] {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_dir_all(p.with_extension("d"));
        }
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("never"));
    }
}
