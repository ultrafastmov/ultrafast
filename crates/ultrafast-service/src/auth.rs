//! Who is calling: the server's API key, or nobody (auth off). Every
//! caller gets an id and its grants (its own uploads). Several keys and
//! OIDC sign-in are ultrafast Cloud's.

use crate::api::ApiError;
use crate::config::{Auth, Config, Grants, KEY_CALLER};

#[derive(Debug, Clone)]
pub struct Caller {
    pub id: String,
    /// `key` or `none`.
    pub kind: &'static str,
    pub grants: Grants,
}

pub struct Authenticator {
    auth: Auth,
    /// The key's sha256 (hex).
    key_sha256: Option<String>,
}

fn unauthorized(message: &str) -> ApiError {
    ApiError::new(401, "unauthorized", message)
}

/// The bearer token in a request's headers.
pub fn bearer(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .find_map(|(_, v)| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
                .map(str::trim)
        })
}

impl Authenticator {
    pub fn new(cfg: &Config) -> Authenticator {
        Authenticator {
            auth: cfg.auth,
            key_sha256: cfg.key_sha256.clone(),
        }
    }

    /// The caller of a request with these headers.
    pub fn caller(&self, cfg: &Config, headers: &[(String, String)]) -> Result<Caller, ApiError> {
        match self.auth {
            Auth::None => Ok(self.by_id(cfg, "local")),
            Auth::Key => {
                let token = bearer(headers).ok_or_else(|| unauthorized("send `Authorization: Bearer <key>`"))?;
                self.token(cfg, token)
            }
        }
    }

    /// The caller a bearer token stands for.
    pub fn token(&self, cfg: &Config, token: &str) -> Result<Caller, ApiError> {
        if self.auth == Auth::None {
            return Ok(self.by_id(cfg, "local"));
        }
        let sha = crate::util::sha256_hex(token.as_bytes());
        match &self.key_sha256 {
            Some(k) if crate::util::constant_eq(k.as_bytes(), sha.as_bytes()) => Ok(self.by_id(cfg, KEY_CALLER)),
            _ => Err(unauthorized("unknown API key")),
        }
    }

    /// The caller with this id (for MCP sessions).
    pub fn by_id(&self, cfg: &Config, id: &str) -> Caller {
        Caller {
            id: id.to_string(),
            kind: match self.auth {
                Auth::None => "none",
                Auth::Key => "key",
            },
            grants: cfg.grants_for(id),
        }
    }
}

/// Percent-encodes everything but `A-Z a-z 0-9 - . _ ~`, and `/` too
/// unless `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len() * 3 / 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- signed files

fn file_sig(secret: &[u8], rel: &str, expires: u64) -> String {
    let mac = crate::util::hmac_sha256(secret, format!("files\0{rel}\0{expires}").as_bytes());
    crate::util::hex(&mac[..20])
}

/// `BASE/v1/files/REL?expires=…&sig=…`: a download URL for `REL` in the
/// server's storage that works until `until`.
pub fn sign_file_url(secret: &[u8], base: &str, rel: &str, until: std::time::SystemTime) -> String {
    let expires = until
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!(
        "{}/v1/files/{}?expires={expires}&sig={}",
        base.trim_end_matches('/'),
        uri_encode(rel, true),
        file_sig(secret, rel, expires)
    )
}

/// Whether a /v1/files request for `rel` carries a valid, unexpired
/// signature.
pub fn verify_file(secret: &[u8], rel: &str, expires: &str, sig: &str) -> bool {
    let Ok(e) = expires.parse::<u64>() else {
        return false;
    };
    e >= crate::util::unix_now() && crate::util::constant_eq(file_sig(secret, rel, e).as_bytes(), sig.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn file_urls() {
        let until = std::time::SystemTime::now() + Duration::from_secs(60);
        let u = sign_file_url(b"k", "http://h:8080/", "renders/r 1/film.mp4", until);
        assert!(
            u.starts_with("http://h:8080/v1/files/renders/r%201/film.mp4?expires="),
            "{u}"
        );
        let q = u.split_once('?').unwrap().1;
        let (e, s) = q.split_once("&sig=").unwrap();
        let e = e.trim_start_matches("expires=");
        assert!(verify_file(b"k", "renders/r 1/film.mp4", e, s));
        assert!(!verify_file(b"k", "renders/r 1/other.mp4", e, s));
        assert!(!verify_file(b"x", "renders/r 1/film.mp4", e, s));
        assert!(!verify_file(b"k", "renders/r 1/film.mp4", "1", s));
    }

    #[test]
    fn one_key() {
        let mut cfg = Config::for_tests(std::path::PathBuf::new());
        cfg.auth = Auth::Key;
        cfg.key_sha256 = Some(crate::util::sha256_hex(b"uf_live_0123456789abcdef"));
        let a = Authenticator::new(&cfg);
        let c = a.token(&cfg, "uf_live_0123456789abcdef").unwrap();
        assert_eq!(
            (c.id.as_str(), c.kind, c.grants.caller.as_str()),
            (KEY_CALLER, "key", KEY_CALLER)
        );
        assert_eq!(a.token(&cfg, "uf_live_wrong").unwrap_err().status, 401);
        assert_eq!(a.caller(&cfg, &[]).unwrap_err().status, 401);
    }
}
