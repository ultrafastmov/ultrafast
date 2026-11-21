//! MCP over Streamable HTTP, for clients that cannot run `uf mcp` (agent
//! frameworks, hosted agents, sandboxes without a GPU).
//!
//! The service mounts this: requests [`claims`] returns true for go to
//! [`handle`] before the service's own auth, and the service implements
//! [`Backend`] so the tools render on its GPU and store on its storage.
//!
//! | route | |
//! |---|---|
//! | `POST /mcp` (and `POST /`) | JSON-RPC; answered as JSON (or one SSE event when the client only accepts a stream) |
//! | `GET`, `DELETE /mcp` | 405: no server-initiated stream, no sessions |
//!
//! Clients send the server's API key as `Authorization: Bearer <key>`; a
//! call without one gets 401. A server that also signs clients in with
//! OAuth (ultrafast Cloud) passes a [`SignIn`] to [`handle_with`]: it answers
//! its own routes (metadata, registration, the sign-in page, tokens) and
//! names its metadata in the 401's `WWW-Authenticate` challenge.

use crate::remote::Remote;
pub use crate::remote::{Backend, RenderFn};
use crate::rpc::{self, Info};
use base64::Engine;
use serde_json::{Value, json};

/// Largest request body accepted (a document with inline data).
pub const MAX_BODY: usize = 32 << 20;

/// An HTTP request, as the mounting server hands it over.
#[derive(Clone, Debug, Default)]
pub struct HttpRequest {
    pub method: String,
    /// The path without the query, e.g. `/mcp`.
    pub path: String,
    /// The query string without `?`.
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// A header's value (names compare case-insensitively).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The response to send.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response with a body of `content_type` (none when empty).
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        let mut r = Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        };
        if !content_type.is_empty() {
            r.headers.push(("Content-Type".into(), content_type.into()));
        }
        r
    }

    /// A JSON response.
    pub fn json(status: u16, v: &Value) -> Self {
        Self::new(status, "application/json", v.to_string())
    }

    /// Adds a header.
    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    /// Adds the headers browser-based clients (MCP Inspector) need.
    pub fn cors(self) -> Self {
        self.header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
            .header(
                "Access-Control-Allow-Headers",
                "Authorization, Content-Type, Accept, Mcp-Protocol-Version, Mcp-Method, Mcp-Name, Mcp-Session-Id, Last-Event-ID",
            )
            .header("Access-Control-Expose-Headers", "WWW-Authenticate, Mcp-Session-Id")
            .header("Access-Control-Max-Age", "86400")
    }
}

/// Sign-in for MCP clients that use OAuth, supplied by the server that
/// mounts this module (ultrafast Cloud). The open server has none: clients
/// send an API key.
pub trait SignIn: Send + Sync {
    /// Whether this answers the request (its metadata and endpoints).
    fn claims(&self, method: &str, path: &str) -> bool;
    /// Answers a request it claims. `base` is the public origin.
    fn handle(&self, req: &HttpRequest, base: &str, backend: &dyn Backend) -> HttpResponse;
    /// The `WWW-Authenticate` value for an unauthenticated MCP call at path
    /// `at` (`/mcp`, or empty for `/`), e.g. `Bearer resource_metadata="..."`.
    fn challenge(&self, base: &str, at: &str) -> String;
}

/// The MCP endpoint's path as it was reached (`/mcp`, or empty for `/`).
fn mcp_path(path: &str) -> Option<&'static str> {
    match path.trim_end_matches('/') {
        "" => Some(""),
        "/mcp" => Some("/mcp"),
        _ => None,
    }
}

/// Whether [`handle`] answers this request. `GET /` stays the service's
/// (its health check); `POST /` is MCP, so a bare origin works as the URL.
pub fn claims(method: &str, path: &str) -> bool {
    match mcp_path(path) {
        Some("") => matches!(method, "POST" | "DELETE" | "OPTIONS"),
        Some(_) => true,
        None => false,
    }
}

/// Answers a request [`claims`] returned true for.
pub fn handle(req: &HttpRequest, backend: &dyn Backend) -> HttpResponse {
    handle_with(req, backend, None)
}

/// [`handle`], with OAuth sign-in: requests `sign_in` claims go to it.
pub fn handle_with(req: &HttpRequest, backend: &dyn Backend, sign_in: Option<&dyn SignIn>) -> HttpResponse {
    let base = backend.base_url(&req.headers).trim_end_matches('/').to_string();
    let method = req.method.as_str();
    if let Some(s) = sign_in.filter(|s| s.claims(method, &req.path)) {
        return s.handle(req, &base, backend);
    }
    let Some(at) = mcp_path(&req.path) else {
        return HttpResponse::json(404, &json!({ "error": "not_found" }));
    };
    if method == "OPTIONS" {
        return HttpResponse::new(204, "", Vec::new()).cors();
    }
    if req.body.len() > MAX_BODY {
        return HttpResponse::json(413, &json!({ "error": "too_large", "limit": MAX_BODY })).cors();
    }
    if method == "POST" {
        return mcp(req, backend, &base, at, sign_in).cors();
    }
    HttpResponse::json(
        405,
        &json!({ "error": "method_not_allowed", "message": "POST JSON-RPC messages; this server opens no event stream and keeps no sessions" }),
    )
    .header("Allow", "POST, OPTIONS")
    .cors()
}

fn bearer(req: &HttpRequest) -> Option<&str> {
    let h = req.header("Authorization")?.trim();
    let (scheme, token) = h.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn mcp(req: &HttpRequest, backend: &dyn Backend, base: &str, at: &str, sign_in: Option<&dyn SignIn>) -> HttpResponse {
    let token = bearer(req);
    let Some(caller) = backend.authenticate(token.unwrap_or("")) else {
        let mut challenge = match sign_in {
            Some(s) => s.challenge(base, at),
            None => "Bearer realm=\"ultrafast\"".to_string(),
        };
        if token.is_some() {
            challenge.push_str(", error=\"invalid_token\"");
        }
        let message = match sign_in {
            Some(_) => "Sign in with OAuth, or send an API key as `Authorization: Bearer <key>`",
            None => "Send this server's API key as `Authorization: Bearer <key>`",
        };
        return HttpResponse::json(401, &json!({ "error": "unauthorized", "message": message }))
            .header("WWW-Authenticate", challenge);
    };
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(m) => m,
        Err(e) => {
            return HttpResponse::json(
                400,
                &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }),
            );
        }
    };
    if let Some(bad) = header_problem(req, &msg) {
        return bad;
    }
    let info = Info::new(crate::tools::instructions(true));
    let mut remote = Remote { backend, caller };
    let Some(rpc::Reply { message: reply, status }) = rpc::handle(&msg, &mut remote, &info) else {
        // Notifications and responses are acknowledged without a body.
        return HttpResponse::new(202, "", Vec::new());
    };
    let accept = req.header("Accept").unwrap_or("");
    if !accept.is_empty()
        && !accept.contains("application/json")
        && !accept.contains("*/*")
        && accept.contains("text/event-stream")
    {
        return HttpResponse::new(
            status,
            "text/event-stream",
            format!("event: message\ndata: {reply}\n\n"),
        )
        .header("Cache-Control", "no-cache");
    }
    HttpResponse::json(status, &reply)
}

/// Checks the headers against the message. A stateless-era request
/// (2026-07-28) must repeat its version, method and tool name in
/// `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name`; an `initialize`-era
/// one may send a version header, which must be one this server speaks.
fn header_problem(req: &HttpRequest, msg: &Value) -> Option<HttpResponse> {
    let reject = |code: i64, message: String, data: Option<Value>| {
        let mut body = json!({ "jsonrpc": "2.0", "id": msg.get("id").cloned().unwrap_or(Value::Null), "error": { "code": code, "message": message } });
        if let Some(d) = data {
            body["error"]["data"] = d;
        }
        Some(HttpResponse::json(400, &body))
    };
    let header_version = req.header("MCP-Protocol-Version");
    let Some(version) = rpc::stateless_version(msg) else {
        return match header_version {
            Some(v) if !rpc::LEGACY.contains(&v) && !rpc::STATELESS.contains(&v) => reject(
                -32600,
                format!(
                    "unsupported MCP-Protocol-Version {v}; this server speaks {}",
                    rpc::VERSIONS.join(", ")
                ),
                None,
            ),
            _ => None,
        };
    };
    if !rpc::STATELESS.contains(&version) {
        return reject(
            -32022,
            "Unsupported protocol version".into(),
            Some(json!({ "supported": rpc::VERSIONS, "requested": version })),
        );
    }
    if header_version != Some(version) {
        return reject(
            -32020,
            format!("Header mismatch: MCP-Protocol-Version {header_version:?} does not match the body's {version}"),
            None,
        );
    }
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    if !method.is_empty() && req.header("Mcp-Method").map(decode_header) != Some(method.to_string()) {
        return reject(
            -32020,
            format!("Header mismatch: Mcp-Method does not match `{method}`"),
            None,
        );
    }
    if matches!(method, "tools/call" | "resources/read" | "prompts/get") {
        let params = msg.get("params");
        let name = params
            .and_then(|p| p.get("name").or_else(|| p.get("uri")))
            .and_then(Value::as_str)
            .unwrap_or("");
        if req.header("Mcp-Name").map(decode_header) != Some(name.to_string()) {
            return reject(
                -32020,
                format!("Header mismatch: Mcp-Name does not match `{name}`"),
                None,
            );
        }
    }
    None
}

/// A header value, decoding the `=?base64?...?=` form used for values that
/// are not plain ASCII.
fn decode_header(v: &str) -> String {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        if let Some(s) = base64::engine::general_purpose::STANDARD
            .decode(inner)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
        {
            return s;
        }
    }
    v.to_string()
}
