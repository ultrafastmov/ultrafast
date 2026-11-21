//! Small helpers: content hashes in the forms GCS and clients use, RFC 3339
//! UTC instants, unique names.

use base64::Engine;
use md5::Digest as _;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A process-wide counter for unique temporary names.
pub fn unique() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha2::Sha256::digest(bytes))
}

/// CRC32C big-endian, base64 (as GCS reports it).
pub fn crc32c_b64(bytes: &[u8]) -> String {
    b64(&crc32c::crc32c(bytes).to_be_bytes())
}

pub fn md5_b64(bytes: &[u8]) -> String {
    b64(&md5::Md5::digest(bytes))
}

/// A file's CRC32C and MD5 (base64) and sha256 (hex), in one read.
pub struct FileHashes {
    pub crc32c: String,
    pub md5: String,
    pub sha256: String,
    pub size: u64,
}

pub fn hash_file(path: &Path) -> std::io::Result<FileHashes> {
    let mut f = std::fs::File::open(path)?;
    let mut crc = 0u32;
    let mut md5 = md5::Md5::new();
    let mut sha = sha2::Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        crc = crc32c::crc32c_append(crc, &buf[..n]);
        md5.update(&buf[..n]);
        sha.update(&buf[..n]);
        size += n as u64;
    }
    Ok(FileHashes {
        crc32c: b64(&crc.to_be_bytes()),
        md5: b64(&md5.finalize()),
        sha256: hex(&sha.finalize()),
        size,
    })
}

/// CRC32C (base64) of a file's bytes.
pub fn crc32c_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut crc = 0u32;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        crc = crc32c::crc32c_append(crc, &buf[..n]);
    }
    Ok(b64(&crc.to_be_bytes()))
}

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `t` as RFC 3339 UTC with milliseconds (`2026-10-13T22:45:12.345Z`).
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (y, mo, da) = civil(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{da:02}T{:02}:{:02}:{:02}.{:03}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60,
        d.subsec_millis()
    )
}

pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// Parses RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SS[.fff]Z`).
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix('z'))?;
    let (date, time) = s.split_once(['T', 't'])?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let da: u32 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (time, None),
    };
    let mut t = hms.split(':');
    let h: u64 = t.next()?.parse().ok()?;
    let mi: u64 = t.next()?.parse().ok()?;
    let se: u64 = t.next()?.parse().ok()?;
    if t.next().is_some() || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let nanos = match frac {
        Some(f) if !f.is_empty() && f.len() <= 9 && f.bytes().all(|b| b.is_ascii_digit()) => {
            f.parse::<u32>().ok()? * 10u32.pow(9 - f.len() as u32)
        }
        Some(_) => return None,
        None => 0,
    };
    let days = days_from_civil(y, mo, da);
    if days < 0 {
        return None;
    }
    let secs = days as u64 * 86_400 + h * 3600 + mi * 60 + se;
    Some(UNIX_EPOCH + Duration::new(secs, nanos))
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), data)
        .as_ref()
        .to_vec()
}

/// Compares secrets in time independent of where they differ.
pub fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `n` bytes from the operating system's random source.
pub fn random_bytes(n: usize) -> Vec<u8> {
    use ring::rand::SecureRandom;
    let mut v = vec![0u8; n];
    ring::rand::SystemRandom::new()
        .fill(&mut v)
        .expect("the system random source failed");
    v
}

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A random string of `n` base-62 characters (about 5.95 bits each).
pub fn random_base62(n: usize) -> String {
    let mut out = String::with_capacity(n);
    while out.len() < n {
        for b in random_bytes(n * 2) {
            // 248 = 4 * 62: no bias.
            if b < 248 && out.len() < n {
                out.push(BASE62[(b % 62) as usize] as char);
            }
        }
    }
    out
}

/// A ULID (Crockford base-32, 26 characters): sorts by creation time.
pub fn ulid() -> String {
    const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand = random_bytes(10);
    let mut v: u128 = ms & ((1u128 << 48) - 1);
    for b in rand {
        v = (v << 8) | b as u128;
    }
    (0..26)
        .rev()
        .map(|i| CROCKFORD[((v >> (i * 5)) & 31) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips() {
        let t = parse_rfc3339("2026-10-13T22:45:12.345Z").unwrap();
        assert_eq!(rfc3339(t), "2026-10-13T22:45:12.345Z");
        assert_eq!(
            rfc3339(parse_rfc3339("2024-02-29T00:00:00Z").unwrap()),
            "2024-02-29T00:00:00.000Z"
        );
        assert!(parse_rfc3339("2026-10-13T22:45:12+05:30").is_none());
        assert!(parse_rfc3339("2026-13-01T00:00:00Z").is_none());
    }

    #[test]
    fn ids() {
        let a = ulid();
        assert_eq!(a.len(), 26);
        assert!(random_base62(22).bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(random_base62(22), random_base62(22));
        assert!(constant_eq(b"abc", b"abc") && !constant_eq(b"abc", b"abd") && !constant_eq(b"ab", b"abc"));
    }

    #[test]
    fn gcs_hash_forms() {
        // Values GCS documents for "hello world"-style checks.
        assert_eq!(md5_b64(b""), "1B2M2Y8AsgTpgAmY7PhCfg==");
        assert_eq!(crc32c_b64(b""), "AAAAAA==");
        assert_eq!(crc32c_b64(b"123456789"), "4waSgw==");
    }
}
