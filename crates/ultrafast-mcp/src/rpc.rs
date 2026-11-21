//! The JSON-RPC 2.0 side of MCP, independent of transport: one message in,
//! at most one message out. [`serve_stdio`] runs it over stdin/stdout, one
//! message per line, for `uf mcp`; [`crate::http`] runs it over Streamable
//! HTTP.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// Protocol versions this server speaks, newest first. `2026-07-28` is the
/// stateless era (every request carries its version in `_meta`; no
/// `initialize`); the others start with `initialize`.
pub const VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The stateless versions: requests carry `_meta` and there is no
/// `initialize`.
pub const STATELESS: &[&str] = &["2026-07-28"];

/// The `initialize` versions, newest first.
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

/// How long clients may cache the tool list and the server's description:
/// they change only with a new release.
const TTL_MS: u64 = 3_600_000;

/// The protocol version a request names in its `_meta`, if it is a
/// stateless-era request.
pub fn stateless_version(msg: &Value) -> Option<&str> {
    msg.get("params")?.get("_meta")?.get(META_VERSION)?.as_str()
}

/// What a server instance offers: its tool list and how to call one.
pub trait Toolbox {
    /// The tool definitions (`tools/list`).
    fn tools(&self) -> Value;
    /// Runs a tool. `Ok` is a `tools/call` result (`content`, ...); `Err` is
    /// reported to the agent as a failed call (`isError`) with that text.
    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String>;
}

/// The server's name and the instructions sent at `initialize`.
pub struct Info {
    pub name: &'static str,
    pub title: &'static str,
    pub version: &'static str,
    pub instructions: String,
}

impl Info {
    pub fn new(instructions: impl Into<String>) -> Self {
        Self {
            name: "ultrafast",
            title: "ultrafast",
            version: env!("CARGO_PKG_VERSION"),
            instructions: instructions.into(),
        }
    }
}

/// The protocol version to answer `initialize` with.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| LEGACY.iter().find(|v| **v == r).copied())
        .unwrap_or(LEGACY[0])
}

/// A tool result with one text block.
pub fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }] })
}

/// A tool result with JSON as pretty text.
pub fn json_text(v: &Value) -> Value {
    text(serde_json::to_string_pretty(v).unwrap_or_default())
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// A reply and the HTTP status it goes back with (200 but for the
/// stateless era's errors: 400 for malformed or unsupported requests, 404
/// for unknown methods).
#[derive(Debug)]
pub struct Reply {
    pub message: Value,
    pub status: u16,
}

/// Handles one JSON-RPC message (or a batch). Returns the reply, or `None`
/// for notifications and responses, which get none.
pub fn handle(msg: &Value, tb: &mut dyn Toolbox, info: &Info) -> Option<Reply> {
    if let Some(batch) = msg.as_array() {
        let out: Vec<Value> = batch
            .iter()
            .filter_map(|m| handle(m, tb, info))
            .map(|r| r.message)
            .collect();
        return (!out.is_empty()).then(|| Reply {
            message: Value::Array(out),
            status: 200,
        });
    }
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    let (Some(method), Some(id)) = (method, id) else {
        // A notification (no id) or a response to us (no method).
        return None;
    };
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let meta = params.get("_meta");
    if meta.and_then(|m| m.get(META_VERSION)).is_some() || method == "server/discover" {
        return Some(stateless(id, method, &params, tb, info));
    }
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiate(params.get("protocolVersion").and_then(Value::as_str)),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": server_info(info),
            "instructions": info.instructions,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tb.tools() })),
        "tools/call" => call(&params, tb).map_err(|(c, m, _)| (c, m)),
        // Not offered, but some clients ask anyway: answer with nothing
        // rather than an error they log.
        "resources/list" => Ok(json!({ "resources": [] })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "logging/setLevel" => Ok(json!({})),
        m => Err((-32601, format!("unknown method `{m}`"))),
    };
    Some(Reply {
        message: match result {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
            Err((code, message)) => error(id, code, message),
        },
        status: 200,
    })
}

fn server_info(info: &Info) -> Value {
    json!({ "name": info.name, "title": info.title, "version": info.version, "websiteUrl": "https://ultrafast.mov" })
}

/// `tools/call`: the result, or a protocol error (code, message, data).
fn call(params: &Value, tb: &mut dyn Toolbox) -> Result<Value, (i64, String, Option<Value>)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let known = tb
        .tools()
        .as_array()
        .is_some_and(|t| t.iter().any(|t| t["name"] == name));
    if !known {
        return Err((-32602, format!("unknown tool `{name}`"), None));
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    Ok(match tb.call(name, &args) {
        Ok(v) => v,
        Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
    })
}

/// A stateless-era request (2026-07-28): its version and capabilities in
/// `_meta`, nothing carried over from earlier requests.
fn stateless(id: Value, method: &str, params: &Value, tb: &mut dyn Toolbox, info: &Info) -> Reply {
    let fail = |code: i64, message: String, data: Option<Value>| {
        let mut e = error(id.clone(), code, message);
        if let Some(d) = data {
            e["error"]["data"] = d;
        }
        Reply {
            message: e,
            status: if code == -32601 { 404 } else { 400 },
        }
    };
    let meta = params.get("_meta");
    let Some(version) = meta.and_then(|m| m.get(META_VERSION)).and_then(Value::as_str) else {
        return fail(-32602, format!("`{method}` needs _meta.{META_VERSION}"), None);
    };
    if !STATELESS.contains(&version) {
        return fail(
            -32022,
            "Unsupported protocol version".into(),
            Some(json!({ "supported": VERSIONS, "requested": version })),
        );
    }
    if meta.and_then(|m| m.get(META_CAPS)).is_none() {
        return fail(-32602, format!("`{method}` needs _meta.{META_CAPS}"), None);
    }
    let result = match method {
        "server/discover" => Ok(json!({
            "supportedVersions": VERSIONS,
            "capabilities": { "tools": {} },
            "instructions": info.instructions,
            "ttlMs": TTL_MS,
            "cacheScope": "public",
        })),
        "tools/list" => Ok(json!({ "tools": tb.tools(), "ttlMs": TTL_MS, "cacheScope": "public" })),
        "tools/call" => call(params, tb),
        "resources/list" => Ok(json!({ "resources": [], "ttlMs": TTL_MS, "cacheScope": "public" })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [], "ttlMs": TTL_MS, "cacheScope": "public" })),
        "prompts/list" => Ok(json!({ "prompts": [], "ttlMs": TTL_MS, "cacheScope": "public" })),
        m => Err((-32601, format!("unknown method `{m}`"), None)),
    };
    match result {
        Ok(mut v) => {
            v["resultType"] = json!("complete");
            v["_meta"] = json!({ META_SERVER: server_info(info) });
            Reply {
                message: json!({ "jsonrpc": "2.0", "id": id, "result": v }),
                status: 200,
            }
        }
        Err((code, message, data)) => fail(code, message, data),
    }
}

/// Serves MCP over stdin/stdout until stdin closes. On Unix, anything else
/// the process writes to stdout (a child process, a stray print) is sent to
/// stderr instead, so it can never corrupt the protocol stream.
pub fn serve_stdio(tb: &mut dyn Toolbox, info: &Info) -> std::io::Result<()> {
    let mut out = protocol_stdout()?;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, tb, info).map(|r| r.message),
            Err(e) => Some(error(Value::Null, -32700, e.to_string())),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn protocol_stdout() -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: dup and dup2 on the process's own standard descriptors.
    unsafe {
        let fd = libc_dup(1);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc_dup2(2, 1) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(std::fs::File::from_raw_fd(fd))
    }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "dup"]
    fn libc_dup(fd: i32) -> i32;
    #[link_name = "dup2"]
    fn libc_dup2(old: i32, new: i32) -> i32;
}

#[cfg(not(unix))]
fn protocol_stdout() -> std::io::Result<std::io::Stdout> {
    Ok(std::io::stdout())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Toolbox for Echo {
        fn tools(&self) -> Value {
            json!([{ "name": "echo", "inputSchema": { "type": "object" } }])
        }
        fn call(&mut self, _: &str, args: &Value) -> Result<Value, String> {
            match args.get("fail") {
                Some(_) => Err("failed".into()),
                None => Ok(text(args.to_string())),
            }
        }
    }

    fn info() -> Info {
        Info::new("test")
    }

    #[test]
    fn initialize_negotiates_the_version() {
        let r = handle(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            &mut Echo,
            &info(),
        )
        .unwrap()
        .message;
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        let r = handle(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
            &mut Echo,
            &info(),
        )
        .unwrap()
        .message;
        assert_eq!(r["result"]["protocolVersion"], LEGACY[0]);
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(
            handle(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                &mut Echo,
                &info()
            )
            .is_none()
        );
    }

    #[test]
    fn tool_errors_and_unknown_tools() {
        let r = handle(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"fail":1}}}),
            &mut Echo,
            &info(),
        )
        .unwrap()
        .message;
        assert_eq!(r["result"]["isError"], true);
        let r = handle(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}}),
            &mut Echo,
            &info(),
        )
        .unwrap()
        .message;
        assert_eq!(r["error"]["code"], -32602);
    }

    fn modern(method: &str, params: Value) -> Value {
        let mut p = params;
        p["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        json!({ "jsonrpc": "2.0", "id": "x", "method": method, "params": p })
    }

    #[test]
    fn the_stateless_era() {
        let r = handle(&modern("server/discover", json!({})), &mut Echo, &info()).unwrap();
        assert_eq!(r.status, 200);
        let res = &r.message["result"];
        assert_eq!(res["resultType"], "complete");
        assert_eq!(res["supportedVersions"][0], "2026-07-28");
        assert!(res["ttlMs"].is_u64() && res["cacheScope"] == "public");
        let r = handle(&modern("tools/list", json!({})), &mut Echo, &info()).unwrap();
        assert_eq!(r.message["result"]["tools"][0]["name"], "echo");
        assert!(r.message["result"]["ttlMs"].is_u64());
        let r = handle(
            &modern("tools/call", json!({ "name": "echo", "arguments": {} })),
            &mut Echo,
            &info(),
        )
        .unwrap();
        assert_eq!(r.message["result"]["resultType"], "complete");
        // Unknown methods are 404; an unsupported version names what is.
        let r = handle(&modern("initialize", json!({})), &mut Echo, &info()).unwrap();
        assert_eq!((r.status, r.message["error"]["code"].as_i64()), (404, Some(-32601)));
        let mut old = modern("tools/list", json!({}));
        old["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2027-01-01");
        let r = handle(&old, &mut Echo, &info()).unwrap();
        assert_eq!((r.status, r.message["error"]["code"].as_i64()), (400, Some(-32022)));
        assert_eq!(r.message["error"]["data"]["requested"], "2027-01-01");
        // Missing capabilities are malformed.
        let mut bare = modern("tools/list", json!({}));
        bare["params"]["_meta"]
            .as_object_mut()
            .unwrap()
            .remove("io.modelcontextprotocol/clientCapabilities");
        assert_eq!(
            handle(&bare, &mut Echo, &info()).unwrap().message["error"]["code"],
            -32602
        );
    }
}
