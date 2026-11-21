//! A development server for the remote MCP tools: Streamable HTTP behind an
//! API key, on this machine's GPU, for testing clients against `ultrafast_mcp::http`
//! without the full service. The real server is the self-hosted service
//! (`ultrafast serve`), which mounts the same handler with its own storage,
//! input resolution and auth.
//!
//!   cargo run --release -p ultrafast-mcp --example dev_server -- [PORT]
//!
//! `ULTRAFAST_API_KEYS` (comma-separated) are the accepted keys; unset, any
//! request is accepted. Exports are written to a temporary folder and
//! served at `/files/`.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};
use ultrafast_graph::Document;
use ultrafast_mcp::http::{self, Backend, HttpRequest, RenderFn};

enum Work {
    Run(Value, RenderFn, Sender<Result<Value, String>>),
}

struct Dev {
    keys: Vec<String>,
    gpu: Mutex<Sender<Work>>,
    out: PathBuf,
    media: PathBuf,
    origin: String,
}

fn on_gpu(rx: std::sync::mpsc::Receiver<Work>, media: PathBuf) {
    let mut r = ultrafast_render::Renderer::new(ultrafast_gpu::Device::new().expect("GPU")).expect("renderer");
    r.set_base_dir(&media);
    for Work::Run(doc, f, reply) in rx {
        let res = Document::from_json(&doc.to_string())
            .map_err(|e| e.to_string())
            .and_then(|d| f(&mut r, &d));
        let _ = reply.send(res);
    }
}

impl Dev {
    fn run(&self, doc: Value, f: RenderFn) -> Result<Value, String> {
        let (tx, rx) = channel();
        self.gpu
            .lock()
            .unwrap()
            .send(Work::Run(doc, f, tx))
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
}

impl Backend for Dev {
    fn authenticate(&self, bearer: &str) -> Option<String> {
        if self.keys.is_empty() {
            return Some("local".into());
        }
        self.keys.iter().position(|k| k == bearer).map(|i| format!("key{i}"))
    }

    fn base_url(&self, headers: &[(String, String)]) -> String {
        if let Ok(u) = std::env::var("ULTRAFAST_PUBLIC_URL") {
            return u;
        }
        let host = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "127.0.0.1".into());
        format!("http://{host}")
    }

    fn with_renderer(&self, _caller: &str, doc: Value, f: RenderFn) -> Result<Value, String> {
        self.run(doc, f)
    }

    fn render(&self, _caller: &str, doc: Value, codec: Option<&str>) -> Result<Value, String> {
        let name = format!(
            "{}.mp4",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let path = self.out.join(&name);
        let codec = match codec {
            Some("hevc") => ultrafast_media::Codec::Hevc,
            _ => ultrafast_media::Codec::H264,
        };
        let mut receipt = self.run(
            doc,
            Box::new(move |r, d| {
                let opts = ultrafast_live::export::ExportOptions {
                    codec,
                    ..Default::default()
                };
                let rep = ultrafast_live::export::export_video(r, d, &path, opts, |_, _| {})?;
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Ok(json!({ "frames": rep.frames, "render_seconds": rep.seconds, "bytes": bytes }))
            }),
        )?;
        receipt["url"] = json!(format!("{}/files/{name}", self.origin));
        Ok(receipt)
    }

    fn asset_path(&self, _caller: &str, id: &str) -> Result<PathBuf, String> {
        let p = self.media.join(id.trim_start_matches("asset://"));
        if p.is_file() {
            Ok(p)
        } else {
            Err(format!("no asset {id} in {}", self.media.display()))
        }
    }
}

fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(8765);
    let out = std::env::temp_dir().join("ultrafast-mcp-dev");
    std::fs::create_dir_all(&out).unwrap();
    let media = std::env::current_dir().unwrap();
    let (tx, rx) = channel();
    {
        let media = media.clone();
        std::thread::spawn(move || on_gpu(rx, media));
    }
    let keys: Vec<String> = std::env::var("ULTRAFAST_API_KEYS")
        .unwrap_or_default()
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    let dev = Dev {
        keys,
        gpu: Mutex::new(tx),
        out: out.clone(),
        media,
        origin: std::env::var("ULTRAFAST_PUBLIC_URL").unwrap_or(format!("http://127.0.0.1:{port}")),
    };
    let server = tiny_http::Server::http(("127.0.0.1", port)).expect("listen");
    eprintln!(
        "ultrafast MCP dev server on http://127.0.0.1:{port}/mcp (exports in {})",
        out.display()
    );
    for mut rq in server.incoming_requests() {
        let url = rq.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let method = rq.method().as_str().to_uppercase();
        let mut body = Vec::new();
        let _ = std::io::Read::read_to_end(rq.as_reader(), &mut body);
        let headers: Vec<(String, String)> = rq
            .headers()
            .iter()
            .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
            .collect();
        if http::claims(&method, path) {
            let req = HttpRequest {
                method,
                path: path.to_string(),
                query: query.to_string(),
                headers,
                body,
            };
            let resp = http::handle(&req, &dev);
            let rpc: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            eprintln!(
                "{} {} {} v={} -> {}",
                req.method,
                req.path,
                rpc["method"].as_str().unwrap_or(""),
                req.header("MCP-Protocol-Version").unwrap_or("-"),
                resp.status
            );
            let mut r = tiny_http::Response::from_data(resp.body).with_status_code(resp.status);
            for (k, v) in resp.headers {
                if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                    r.add_header(h);
                }
            }
            let _ = rq.respond(r);
        } else if let Some(name) = path
            .strip_prefix("/files/")
            .filter(|n| !n.contains('/') && !n.contains(".."))
        {
            match std::fs::File::open(out.join(name)) {
                Ok(f) => {
                    let _ = rq.respond(tiny_http::Response::from_file(f));
                }
                Err(_) => {
                    let _ = rq.respond(tiny_http::Response::empty(404));
                }
            }
        } else {
            let _ = rq.respond(tiny_http::Response::from_string("ultrafast MCP dev server").with_status_code(404));
        }
    }
}
