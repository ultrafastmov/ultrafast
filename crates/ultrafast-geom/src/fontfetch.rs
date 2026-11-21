//! Fonts by name from Google Fonts: a family a document names that is not
//! installed is downloaded once (TrueType, every weight and style it has)
//! into a per-user cache and used from there. `ULTRAFAST_OFFLINE=1` turns
//! downloading off; cached families still load.

use std::path::PathBuf;
use std::process::Command;

/// Where downloaded fonts live: the platform cache directory.
pub fn cache_dir() -> PathBuf {
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(local).join("ultrafast").join("fonts");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
    };
    base.join("ultrafast").join("fonts")
}

fn slug(family: &str) -> String {
    family
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

enum Got {
    Body(Vec<u8>),
    /// The server answered no (an HTTP error).
    No,
    /// No answer at all: offline, or no curl.
    Offline,
}

fn get(url: &str) -> Got {
    get_within(url, 15)
}

fn get_within(url: &str, seconds: u32) -> Got {
    // The status code comes back after the body: curl's own exit code for an
    // HTTP error is not reliable (a 400 over HTTP/2 can exit 56, not 22).
    let Ok(o) = Command::new("curl")
        .args(["-sL", "--max-time", &seconds.to_string(), "-w", "\n%{http_code}", url])
        .output()
    else {
        return Got::Offline;
    };
    let mut body = o.stdout;
    let cut = body.iter().rposition(|&b| b == b'\n').unwrap_or(0);
    let code: u32 = std::str::from_utf8(&body[cut..])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    body.truncate(cut);
    match code {
        200..=299 => Got::Body(body),
        // 000: no answer at all.
        0 => Got::Offline,
        _ => Got::No,
    }
}

/// Noto Color Emoji with bitmap strikes (CBDT: the build Linux
/// distributions ship and the render service draws with), for a machine
/// whose emoji font draws color another way (Windows' Segoe UI Emoji uses
/// COLR layers) or that has none. Downloaded once (10.7 MB) and checked
/// against its hash; empty when offline or the download fails.
pub fn fetch_emoji() -> Vec<PathBuf> {
    const URL: &str = "https://github.com/googlefonts/noto-emoji/raw/v2.051/fonts/NotoColorEmoji.ttf";
    const SHA256: &str = "72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b";
    let dir = cache_dir().join("noto-color-emoji");
    let path = dir.join("NotoColorEmoji.ttf");
    if path.exists() {
        return vec![path];
    }
    if std::env::var_os("ULTRAFAST_OFFLINE").is_some() {
        return Vec::new();
    }
    // A failed download is not retried for a day.
    let marker = dir.join(".missing");
    if std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().map(|e| e.as_secs() < 86_400).unwrap_or(true))
    {
        return Vec::new();
    }
    let _ = std::fs::create_dir_all(&dir);
    let fetched = match get_within(URL, 120) {
        Got::Body(bytes) => {
            use sha2::Digest;
            let hash: String = sha2::Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            (hash == SHA256).then_some(bytes)
        }
        _ => None,
    };
    let Some(bytes) = fetched else {
        let _ = std::fs::write(&marker, b"");
        return Vec::new();
    };
    let partial = dir.join("NotoColorEmoji.ttf.part");
    if std::fs::write(&partial, &bytes)
        .and_then(|_| std::fs::rename(&partial, &path))
        .is_err()
    {
        return Vec::new();
    }
    vec![path]
}

/// Font files of `family`: from the cache, or downloaded into it. Empty
/// when the family is unknown or the network is off.
pub fn fetch(family: &str) -> Vec<PathBuf> {
    let dir = cache_dir().join(slug(family));
    let cached = |dir: &PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|r| {
                r.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "ttf" || x == "otf"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let have = cached(&dir);
    if !have.is_empty() || std::env::var_os("ULTRAFAST_OFFLINE").is_some() {
        return have;
    }
    // A family Google does not have is remembered for a day.
    let marker = dir.join(".missing");
    if std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().map(|e| e.as_secs() < 86_400).unwrap_or(true))
    {
        return Vec::new();
    }
    let name = family.trim().replace(' ', "+");
    let mut css = None;
    // Google refuses a weight range wider than the family has, so walk
    // the common ranges (widest first) before falling back to statics:
    // a family that stops at 900 or starts at 400 still gets every weight.
    let mut tries = Vec::new();
    for ital in [true, false] {
        for hi in [900, 800, 700] {
            for lo in [100, 200, 300, 400] {
                tries.push(if ital {
                    format!("https://fonts.googleapis.com/css2?family={name}:ital,wght@0,{lo}..{hi};1,{lo}..{hi}")
                } else {
                    format!("https://fonts.googleapis.com/css2?family={name}:wght@{lo}..{hi}")
                });
            }
        }
    }
    // All at once (each refusal is a round trip): the widest that answers.
    let answers: Vec<Got> = std::thread::scope(|scope| {
        let jobs: Vec<_> = tries.iter().map(|u| scope.spawn(move || get(u))).collect();
        jobs.into_iter().map(|j| j.join().unwrap_or(Got::Offline)).collect()
    });
    if answers.iter().all(|a| matches!(a, Got::Offline)) {
        return Vec::new();
    }
    for a in answers {
        if let Got::Body(b) = a {
            css = String::from_utf8(b).ok();
            break;
        }
    }
    // A static family (no weight axis) refuses ranges: ask for each weight
    // and style on its own and keep the ones it has (Poppins: 18 files).
    if css.is_none() {
        let bodies: Vec<String> = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..2)
                .flat_map(|ital| (1..=9).map(move |w| (ital, w * 100)))
                .map(|(ital, w)| {
                    let u = format!("https://fonts.googleapis.com/css2?family={name}:ital,wght@{ital},{w}");
                    scope.spawn(move || match get(&u) {
                        Got::Body(b) => String::from_utf8(b).ok(),
                        _ => None,
                    })
                })
                .collect();
            jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
        });
        if !bodies.is_empty() {
            css = Some(bodies.join("\n"));
        }
    }
    if css.is_none() {
        for u in [
            format!("https://fonts.googleapis.com/css2?family={name}:ital,wght@0,400;0,700;1,400;1,700"),
            format!("https://fonts.googleapis.com/css2?family={name}:wght@400;700"),
            format!("https://fonts.googleapis.com/css2?family={name}"),
        ] {
            match get(&u) {
                Got::Body(b) => {
                    css = String::from_utf8(b).ok();
                    break;
                }
                Got::No => continue,
                Got::Offline => return Vec::new(),
            }
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    let Some(css) = css else {
        let _ = std::fs::write(&marker, b"");
        return Vec::new();
    };
    let mut urls: Vec<String> = Vec::new();
    for part in css.split("url(").skip(1) {
        if let Some(u) = part.split(')').next()
            && (u.ends_with(".ttf") || u.ends_with(".otf"))
            && !urls.contains(&u.to_string())
        {
            urls.push(u.to_string());
        }
    }
    // Every weight and style at once.
    let out: Vec<PathBuf> = std::thread::scope(|scope| {
        let jobs: Vec<_> = urls
            .iter()
            .enumerate()
            .map(|(k, u)| {
                let dir = &dir;
                scope.spawn(move || {
                    let Got::Body(bytes) = get(u) else { return None };
                    let ext = if u.ends_with(".otf") { "otf" } else { "ttf" };
                    let p = dir.join(format!("{}-{k}.{ext}", slug(family)));
                    std::fs::write(&p, bytes).ok().map(|_| p)
                })
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
    });
    if !out.is_empty() {
        eprintln!(
            "ultrafast: font `{family}` downloaded from Google Fonts into {}",
            dir.display()
        );
    }
    out
}

/// Fetches the families not yet cached, several at once (a document naming
/// a few new families would otherwise wait for each in turn).
pub fn prefetch(families: &[String]) {
    std::thread::scope(|scope| {
        for f in families {
            scope.spawn(move || {
                let _ = fetch(f);
            });
        }
    });
}
