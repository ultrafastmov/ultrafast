//! Input references in documents: an object in a bucket
//! (`gs://BUCKET/OBJECT#GENERATION`, `s3://BUCKET/KEY[#VERSION]`,
//! `r2://BUCKET/KEY[#VERSION]`), an upload (`asset://ID`), a member of a
//! zip archive (`…#GENERATION!/MEMBER`, `asset://ID!/MEMBER`), or an
//! `https://` URL.

use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ref {
    Object {
        /// `gs`, `s3` or `r2`.
        scheme: &'static str,
        bucket: String,
        name: String,
        /// The GCS generation or S3 version id (required for `gs`).
        generation: Option<String>,
        /// A member of the zip archive `name` (may hold a `{frame}` pattern).
        member: Option<String>,
    },
    /// An upload: the random part of its id (`asset_ID`).
    Asset {
        id: String,
        member: Option<String>,
    },
    Https {
        url: String,
    },
}

impl Ref {
    /// The object this reference reads (without the archive member), in
    /// canonical form: the name percent-encoded where it must be.
    pub fn object(&self) -> String {
        match self {
            Ref::Object {
                scheme,
                bucket,
                name,
                generation,
                ..
            } => match generation {
                Some(g) => format!("{scheme}://{bucket}/{}#{g}", encode(name)),
                None => format!("{scheme}://{bucket}/{}", encode(name)),
            },
            Ref::Asset { id, .. } => format!("asset://{id}"),
            Ref::Https { url } => url.clone(),
        }
    }

    /// `scheme://bucket/name` with the object name decoded (prefix checks
    /// compare actual names).
    pub fn location(&self) -> String {
        match self {
            Ref::Object {
                scheme, bucket, name, ..
            } => format!("{scheme}://{bucket}/{name}"),
            Ref::Asset { id, .. } => format!("asset://{id}"),
            Ref::Https { url } => url.clone(),
        }
    }

    pub fn member(&self) -> Option<&str> {
        match self {
            Ref::Object { member, .. } | Ref::Asset { member, .. } => member.as_deref(),
            Ref::Https { .. } => None,
        }
    }

    /// The object's file extension (lowercase, with the dot), if any. An
    /// asset's comes from its upload's file name (see the resolver).
    pub fn extension(&self) -> String {
        let path = match self {
            Ref::Object { name, .. } => name.as_str(),
            Ref::Asset { .. } => "",
            Ref::Https { url } => url.split(['?', '#']).next().unwrap_or(url),
        };
        extension_of(path)
    }
}

/// The extension of a path's last segment (lowercase, with the dot).
pub fn extension_of(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) => {
            format!(".{}", ext.to_ascii_lowercase())
        }
        _ => String::new(),
    }
}

/// Whether `s` is meant as a reference (then [`parse`] says if it is a
/// good one).
pub fn looks_like_ref(s: &str) -> bool {
    ["gs://", "s3://", "r2://", "asset://", "https://", "http://"]
        .iter()
        .any(|p| s.starts_with(p))
}

/// Decodes `%XX` escapes (object names and archive members escape `%`,
/// `#` and `?`, and may escape anything else).
pub fn decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
            let Some(hex) = hex else {
                return Err(format!("`{s}`: `%` must start an escape like %23"));
            };
            out.push(u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("`{s}` does not decode to UTF-8"))
}

/// Escapes what a name must not carry literally in a reference: `%`, `#`,
/// `?` and control characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' | '#' | '?' => out.push_str(&format!("%{:02X}", c as u32)),
            c if c.is_control() => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn parse_member(m: &str) -> Result<String, String> {
    let m = decode(m)?;
    if m.is_empty()
        || m.starts_with('/')
        || m.contains('\\')
        || m.contains('\0')
        || m.split('/').any(|seg| seg == ".." || seg == "." || seg.is_empty())
    {
        return Err(format!("`{m}` is not an archive member path"));
    }
    Ok(m)
}

/// Whether `id` can be an asset id (the part after `asset_`).
pub fn valid_asset_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Parses a reference; the error says what is wrong with it. The object
/// name (and archive member) are percent-decoded once, here.
pub fn parse(s: &str) -> Result<Ref, String> {
    if let Some(rest) = s.strip_prefix("asset://") {
        let (id, member) = match rest.split_once("!/") {
            Some((id, m)) => (id, Some(parse_member(m)?)),
            None => (rest, None),
        };
        let id = id.strip_prefix("asset_").unwrap_or(id);
        if !valid_asset_id(id) {
            return Err(format!("`{id}` is not an asset id (asset://ID from POST /v1/uploads)"));
        }
        return Ok(Ref::Asset {
            id: id.to_string(),
            member,
        });
    }
    for scheme in ["gs", "s3", "r2"] {
        let Some(rest) = s.strip_prefix(&format!("{scheme}://")) else {
            continue;
        };
        let (bucket, rest) = rest
            .split_once('/')
            .ok_or_else(|| format!("{scheme}:// reference without an object name"))?;
        if !valid_bucket(bucket) {
            return Err(format!("`{bucket}` is not a bucket name"));
        }
        let (name, fragment) = match rest.split_once('#') {
            Some((n, f)) => (n, Some(f)),
            None if scheme == "gs" => return Err("pin a generation: gs://BUCKET/OBJECT#GENERATION".into()),
            None => (rest, None),
        };
        if name.contains('?') {
            return Err("escape `?` in object names as %3F".into());
        }
        let name = decode(name)?;
        if name.is_empty() || name.len() > 1024 || name.contains('\n') || name.contains('\r') {
            return Err("bad object name".into());
        }
        let (generation, member) = match fragment {
            None => (None, None),
            Some(f) => match f.split_once("!/") {
                Some((g, m)) => (Some(g), Some(parse_member(m)?)),
                None => (Some(f), None),
            },
        };
        let generation = match generation {
            Some("") if scheme != "gs" => None,
            Some(g) if scheme == "gs" => {
                if g.is_empty() || g.len() > 19 || !g.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(format!("`{g}` is not a generation (decimal digits)"));
                }
                Some(g.to_string())
            }
            Some(v) => {
                if v.len() > 1024 || !v.bytes().all(|b| b.is_ascii_graphic()) {
                    return Err(format!("`{v}` is not a version id"));
                }
                Some(v.to_string())
            }
            None => None,
        };
        return Ok(Ref::Object {
            scheme,
            bucket: bucket.to_string(),
            name,
            generation,
            member,
        });
    }
    if s.starts_with("https://") {
        let host = host_of(s).ok_or("bad https URL")?;
        if host.is_empty() {
            return Err("bad https URL".into());
        }
        return Ok(Ref::Https { url: s.to_string() });
    }
    if s.starts_with("http://") {
        return Err("inputs must use https://".into());
    }
    Err("not a reference".into())
}

/// The host of an `https://` URL (lowercase, without port or user info).
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => authority,
    };
    Some(host.to_ascii_lowercase())
}

fn valid_bucket(b: &str) -> bool {
    (3..=222).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'.'))
}

/// Calls `f` on every param value that may hold inputs: the file params of
/// built-in ops ([`FILE_PARAMS`]), every param of a component instance and
/// every exposed default of a component (their meaning is the component's),
/// the `src` of every audio driver (`{"audio": {"src": …}}`, on any
/// param), and `meta.fonts[].src`.
fn input_slots(doc: &mut Value, f: &mut dyn FnMut(&mut Value)) {
    let components: BTreeSet<String> = doc
        .get("components")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let nodes = |nodes: &mut Value, f: &mut dyn FnMut(&mut Value)| {
        for node in nodes.as_object_mut().into_iter().flat_map(|m| m.values_mut()) {
            let op = node.get("op").and_then(Value::as_str).unwrap_or("").to_string();
            let Some(params) = node.get_mut("params").and_then(Value::as_object_mut) else {
                continue;
            };
            if components.contains(&op) {
                params.values_mut().for_each(&mut *f);
            } else {
                for (k, v) in params.iter_mut() {
                    if FILE_PARAMS.contains(&(op.as_str(), k.as_str())) {
                        f(v);
                    } else {
                        audio_driver_srcs(v, f);
                    }
                }
            }
        }
    };
    if let Some(n) = doc.get_mut("nodes") {
        nodes(n, f);
    }
    if let Some(cs) = doc.get_mut("components").and_then(Value::as_object_mut) {
        for c in cs.values_mut() {
            if let Some(n) = c.get_mut("nodes") {
                nodes(n, f);
            }
            if let Some(p) = c.get_mut("params").and_then(Value::as_object_mut) {
                p.values_mut().for_each(&mut *f);
            }
        }
    }
    if let Some(fonts) = doc
        .get_mut("meta")
        .and_then(|m| m.get_mut("fonts"))
        .and_then(Value::as_array_mut)
    {
        for font in fonts {
            if let Some(src) = font.get_mut("src") {
                f(src);
            }
        }
    }
}

/// Calls `f` on the `src` of every audio driver inside a param value (a
/// driver can sit in a keyframe or a vector as well).
fn audio_driver_srcs(v: &mut Value, f: &mut dyn FnMut(&mut Value)) {
    match v {
        Value::Object(o) => {
            if let Some(src) = o.get_mut("audio").and_then(|a| a.get_mut("src")) {
                f(src);
            }
            for (k, x) in o.iter_mut() {
                if k != "audio" {
                    audio_driver_srcs(x, f);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| audio_driver_srcs(x, f)),
        _ => {}
    }
}

/// Calls `f` on every string inside `v` (keyframe values included).
fn strings_mut(v: &mut Value, f: &mut dyn FnMut(&mut String)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter_mut().for_each(|x| strings_mut(x, f)),
        Value::Object(o) => o.values_mut().for_each(|x| strings_mut(x, f)),
        _ => {}
    }
}

/// Every string in a document's input slots that is meant as a reference.
pub fn collect(doc: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut copy = doc.clone();
    input_slots(&mut copy, &mut |v| {
        strings_mut(v, &mut |s| {
            if looks_like_ref(s) {
                out.insert(s.clone());
            }
        })
    });
    out
}

/// Replaces every input-slot string that `map` has a value for.
pub fn rewrite(doc: &mut Value, map: &std::collections::HashMap<String, String>) {
    input_slots(doc, &mut |v| {
        strings_mut(v, &mut |s| {
            if let Some(r) = map.get(s.as_str()) {
                *s = r.clone();
            }
        })
    });
}

/// `(op, param)` pairs that name files the renderer reads.
pub const FILE_PARAMS: &[(&str, &str)] = &[
    ("image", "src"),
    ("video", "src"),
    ("audio", "src"),
    ("svg", "src"),
    ("lottie", "src"),
    ("lut", "src"),
    ("mesh", "src"),
    ("captions", "src"),
    ("clip_camera", "src"),
    ("clip_camera", "faces"),
    ("block", "logo"),
    ("text", "font"),
    ("text", "em_font"),
    ("block", "display_font"),
    ("block", "text_font"),
    ("block", "mono_font"),
    ("mesh", "font"),
    ("swarm", "font"),
    ("code", "font"),
    ("captions", "font"),
];

/// Font params take family names as well as files.
pub fn is_font_param(param: &str) -> bool {
    param.ends_with("font")
}

/// Whether a font param value is a family name rather than a file.
pub fn is_family_name(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    !s.contains('/')
        && !s.contains('\\')
        && !s.starts_with('.')
        && !s.starts_with('~')
        && ![".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2", ".dfont"]
            .iter()
            .any(|e| lower.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_references() {
        assert_eq!(
            parse("gs://b-1/a/b.mp4#123").unwrap(),
            Ref::Object {
                scheme: "gs",
                bucket: "b-1".into(),
                name: "a/b.mp4".into(),
                generation: Some("123".into()),
                member: None
            }
        );
        let r = parse("s3://my-bucket/in/a.mp4").unwrap();
        assert_eq!(r.object(), "s3://my-bucket/in/a.mp4");
        assert_eq!(r.location(), "s3://my-bucket/in/a.mp4");
        let r = parse("r2://b-1/x.zip#v7!/frame_{frame:04}.png").unwrap();
        assert_eq!(r.object(), "r2://b-1/x.zip#v7");
        assert_eq!(r.member(), Some("frame_{frame:04}.png"));
        let r = parse("asset://7KqH2abcdEFGH").unwrap();
        assert_eq!(r.object(), "asset://7KqH2abcdEFGH");
        assert_eq!(parse("asset://asset_7KqH2abcdEFGH").unwrap(), r);
        assert_eq!(
            parse("asset://7KqH2abcdEFGH!/a/b.png").unwrap().member(),
            Some("a/b.png")
        );
        assert!(parse("asset://short").is_err() && parse("asset://7KqH2abcdEFGH!/../x").is_err());
        let r = parse("gs://bkt/c/foreground_frames.zip#99!/frame_{frame:06}.webp").unwrap();
        assert_eq!(r.member(), Some("frame_{frame:06}.webp"));
        assert_eq!(r.object(), "gs://bkt/c/foreground_frames.zip#99");
        assert_eq!(r.extension(), ".zip");
        assert!(parse("gs://bkt/x.mp4").unwrap_err().contains("generation"));
        assert!(parse("gs://bkt/x.mp4#latest").is_err());
        assert!(parse("gs://bkt/x.zip#1!/../etc/passwd").is_err());
        assert!(parse("gs://bkt/x.zip#1!//abs").is_err());
        assert!(parse("gs://BKT/x#1").is_err());
        // Escapes decode once; the canonical form escapes %, # and ?.
        let r = parse("gs://bkt/runs/a%23b%3Fc%25d%20e.mp4#123").unwrap();
        assert_eq!(r.location(), "gs://bkt/runs/a#b?c%d e.mp4");
        assert_eq!(r.object(), "gs://bkt/runs/a%23b%3Fc%25d e.mp4#123");
        assert_eq!(parse(&r.object()).unwrap(), r);
        assert!(parse("gs://bkt/x?y.mp4#1").is_err());
        assert!(parse("gs://bkt/x%2.mp4#1").is_err());
        let m = parse("gs://bkt/c.zip#9!/frame%23_{frame:06}.webp").unwrap();
        assert_eq!(m.member(), Some("frame#_{frame:06}.webp"));
        assert!(parse("http://example.com/x.png").is_err());
        assert_eq!(
            host_of("https://Fonts.GStatic.com:443/s/x.ttf").as_deref(),
            Some("fonts.gstatic.com")
        );
        assert!(host_of("https://user@host/x").is_none());
    }

    #[test]
    fn collects_and_rewrites_params_only() {
        let mut doc = serde_json::json!({
            "nodes": {
                "v": {"op": "video", "params": {"src": "gs://b/v.mp4#1"}},
                "t": {"op": "text", "params": {"text": "gs://not-an-input-but-says-so#1", "font": "Inter"}}
            },
            "components": {"c": {"nodes": {"i": {"op": "image", "params": {"src": "gs://b/i.png#2"}}},
                                  "params": {"logo": "gs://b/l.svg#3"}, "output": "i"}},
            "meta": {"fonts": [{"src": "gs://b/f.ttf#4"}]},
            "output": "v"
        });
        let refs = collect(&doc);
        // The text node's `text` is words, not an input.
        assert_eq!(refs.len(), 4, "{refs:?}");
        // An audio driver's source, on any param, is an input.
        let mut driven = doc.clone();
        driven["nodes"]["t"]["params"]["size"] =
            serde_json::json!({"audio": {"src": "asset://AAAAAAAAAAAAAAAAAAAAAA", "band": "low"}});
        assert!(collect(&driven).contains("asset://AAAAAAAAAAAAAAAAAAAAAA"));
        let map = [("gs://b/v.mp4#1".to_string(), "/x/v.mp4".to_string())].into();
        rewrite(&mut doc, &map);
        assert_eq!(doc["nodes"]["v"]["params"]["src"], "/x/v.mp4");
        assert!(is_family_name("Inter") && !is_family_name("fonts/Inter.ttf") && !is_family_name("Inter.TTF"));
    }
}
