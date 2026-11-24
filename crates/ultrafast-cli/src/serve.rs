//! `ultrafast serve`: the self-hosted render service (the v1 HTTP API in
//! docs/service/API.md, configured as docs/service/SELF_HOST.md says), with
//! the remote MCP server mounted at /mcp.
//!
//!   ultrafast serve [--port 8080] [--host ::]   (every address, IPv4 and IPv6)
//!   ultrafast serve key     print a new API key (and its sha256, for a keys file)

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use ultrafast_gpu::Device;
use ultrafast_render::Renderer;
use ultrafast_service::server::{self, Handle, HttpRequest, HttpResponse, Mount};

pub fn run(port: Option<u16>, host: Option<String>) -> Result<()> {
    let mut cfg = ultrafast_service::config::Config::from_env()?;
    if let Some(p) = port {
        cfg.port = p;
        if std::env::var_os("ULTRAFAST_INPUT_CACHE").is_none() {
            cfg.cache_dir = std::env::temp_dir().join(format!("ultrafast-inputs-{p}"));
        }
    }
    if let Some(h) = host {
        cfg.bind = h;
    }
    server::run(
        cfg,
        server::Options {
            mounts: vec![Arc::new(McpMount)],
            on_listen: None,
        },
    )
}

/// A new API key, for `ULTRAFAST_API_KEY`.
pub fn new_key() {
    println!("uf_live_{}", ultrafast_service::util::random_base62(32));
}

/// The remote MCP server (Streamable HTTP; the API key as its bearer token).
struct McpMount;

impl Mount for McpMount {
    fn claims(&self, method: &str, path: &str) -> bool {
        ultrafast_mcp::http::claims(method, path)
    }

    fn handle(&self, req: &HttpRequest, svc: &Handle) -> HttpResponse {
        let r = ultrafast_mcp::http::handle(
            &ultrafast_mcp::http::HttpRequest {
                method: req.method.clone(),
                path: req.path.clone(),
                query: req.query.clone(),
                headers: req.headers.clone(),
                body: req.body.clone(),
            },
            &Backend {
                svc,
                base: svc.base_url(&req.headers),
            },
        );
        HttpResponse {
            status: r.status,
            headers: r.headers,
            body: r.body,
        }
    }
}

/// The service, as the MCP tools see it (one per request).
struct Backend<'a> {
    svc: &'a Handle,
    base: String,
}

impl ultrafast_mcp::http::Backend for Backend<'_> {
    fn authenticate(&self, bearer: &str) -> Option<String> {
        self.svc.authenticate(bearer)
    }

    fn base_url(&self, headers: &[(String, String)]) -> String {
        self.svc.base_url(headers)
    }

    fn with_renderer(
        &self,
        caller: &str,
        doc: serde_json::Value,
        f: ultrafast_mcp::http::RenderFn,
    ) -> Result<serde_json::Value, String> {
        self.svc.with_renderer(caller, doc, f).map_err(|e| e.message)
    }

    fn render(
        &self,
        caller: &str,
        doc: serde_json::Value,
        settings: ultrafast_live::output::Settings,
        range: Option<[f64; 2]>,
    ) -> Result<serde_json::Value, String> {
        self.svc
            .render(caller, doc, settings, range, &self.base)
            .map(|r| serde_json::to_value(r).unwrap_or_default())
            .map_err(|e| e.message)
    }

    fn asset_path(&self, caller: &str, id: &str) -> Result<PathBuf, String> {
        self.svc.asset_path(caller, id).map_err(|e| e.message)
    }
}

/// Runs `f`; if it panics (a bug, or the GPU out of memory), the renderer
/// is rebuilt and the request fails instead of the instance going down and
/// the next request paying a cold start.
pub fn guarded<T>(r: &mut Renderer, f: impl FnOnce(&mut Renderer) -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *r))) {
        Ok(result) => result,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into());
            let base = r.base_dir().to_path_buf();
            let wait_fused = r.waits_for_fused();
            *r = Renderer::new(Device::new()?)?;
            r.set_base_dir(&base);
            r.set_wait_for_fused(wait_fused);
            Err(anyhow::anyhow!("render failed: {msg} (renderer rebuilt)"))
        }
    }
}
