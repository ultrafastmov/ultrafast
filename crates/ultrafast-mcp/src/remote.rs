//! The remote tool set ([`crate::tools::remote`]) run against a
//! [`Backend`]: the self-hosted service, or ultrafast Cloud behind
//! mcp.ultrafast.mov. One [`Remote`] serves one authenticated request.

use crate::core;
use crate::rpc::{Toolbox, json_text, text};
use serde_json::{Value, json};
use std::path::PathBuf;
use ultrafast_graph::Document;
use ultrafast_live::output::Settings;
use ultrafast_render::Renderer;

/// Work given to the GPU: runs with the service's renderer and the
/// document, its inputs resolved.
pub type RenderFn = Box<dyn FnOnce(&mut Renderer, &Document) -> Result<Value, String> + Send>;

/// What the HTTP server needs from the service that mounts it.
pub trait Backend: Send + Sync {
    /// The caller a bearer token belongs to (an API key or an OIDC token),
    /// or `None` to refuse it. When the service runs without auth it
    /// returns a caller for any token, the empty one included.
    fn authenticate(&self, bearer: &str) -> Option<String>;
    /// The public origin requests reach the service at, e.g.
    /// `http://127.0.0.1:8080` (no trailing slash), for links and sign-in
    /// metadata.
    fn base_url(&self, headers: &[(String, String)]) -> String;
    /// Runs `f` on the GPU with the renderer, after resolving the document's
    /// inputs (asset ids, https URLs, fonts) the way `/v1/frame` does.
    fn with_renderer(&self, caller: &str, doc: Value, f: RenderFn) -> Result<Value, String>;
    /// Renders and stores a video as `settings` ask (over `range` seconds);
    /// returns the service's receipt, with the signed download `url`.
    fn render(&self, caller: &str, doc: Value, settings: Settings, range: Option<[f64; 2]>) -> Result<Value, String>;
    /// A local file for an uploaded asset id or an allowed https URL.
    fn asset_path(&self, caller: &str, id: &str) -> Result<PathBuf, String> {
        let _ = (caller, id);
        Err("this server does not read media outside documents".into())
    }
}

/// The remote tools for one caller.
pub struct Remote<'a> {
    pub backend: &'a dyn Backend,
    pub caller: String,
}

impl Remote<'_> {
    fn gpu(
        &self,
        args: &Value,
        f: impl FnOnce(&mut Renderer, &Document, &Value) -> Result<Value, String> + Send + 'static,
    ) -> Result<Value, String> {
        let doc = core::document_value(args.get("document"))?;
        let args = args.clone();
        self.backend
            .with_renderer(&self.caller, doc, Box::new(move |r, d| f(r, d, &args)))
    }
}

/// A minimal document for tools that need a renderer but no film.
fn blank() -> Value {
    json!({ "version": 1, "format": { "width": 64, "height": 64, "fps": 30, "duration": 1 },
            "nodes": { "bg": { "op": "solid" } }, "output": "bg" })
}

impl Toolbox for Remote<'_> {
    fn tools(&self) -> Value {
        crate::tools::remote()
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "ultrafast_ops" => Ok(core::ops(args)),
            "ultrafast_new" => Ok(text(core::template(args)?)),
            "ultrafast_check" => {
                // Static problems come back even when the inputs cannot be
                // resolved; the render-time ones need the GPU.
                let doc = core::parse_document(args.get("document"))?;
                let value = core::document_value(args.get("document"))?;
                match self
                    .backend
                    .with_renderer(&self.caller, value, Box::new(|r, d| Ok(core::check(r, d))))
                {
                    Ok(v) => Ok(v),
                    Err(e) => {
                        let mut issues: Vec<Value> = ultrafast_render::schema::check(&doc)
                            .into_iter()
                            .map(|i| json!({ "node": i.node, "severity": i.severity, "message": i.message }))
                            .collect();
                        issues.push(json!({ "node": "", "severity": "error", "message": e }));
                        Ok(json_text(&json!({ "issues": issues })))
                    }
                }
            }
            "ultrafast_frame" => self.gpu(args, core::frame),
            "ultrafast_sheet" => self.gpu(args, core::sheet),
            "ultrafast_review" => self.gpu(args, core::review),
            "ultrafast_inspect" => self.gpu(args, core::inspect),
            "ultrafast_profile" => self.gpu(args, core::profile),
            "ultrafast_edit" => {
                let doc = core::parse_document(args.get("document"))?;
                let (doc, mut report) = core::edit(doc, args)?;
                report["document"] = serde_json::from_str(&doc.to_json()).map_err(|e| e.to_string())?;
                Ok(json_text(&report))
            }
            "ultrafast_export" => {
                let doc = core::document_value(args.get("document"))?;
                let settings = core::export_settings(args)?;
                let range = match (
                    args.get("from").and_then(Value::as_f64),
                    args.get("to").and_then(Value::as_f64),
                ) {
                    (None, None) => None,
                    (a, b) => Some([
                        a.unwrap_or(0.0),
                        b.or(doc["format"]["duration"].as_f64()).unwrap_or(f64::MAX),
                    ]),
                };
                let receipt = self.backend.render(&self.caller, doc, settings, range)?;
                let url = receipt.get("url").and_then(Value::as_str).unwrap_or("");
                let mut out = json_text(&receipt);
                if !url.is_empty() {
                    out["content"].as_array_mut().unwrap().insert(
                        0,
                        json!({ "type": "text", "text": format!("Rendered. Download: {url}") }),
                    );
                }
                Ok(out)
            }
            "ultrafast_estimate" => self.gpu(args, |r, d, a| core::estimate(r, d, a, "document")),
            "ultrafast_batch" => {
                let doc = core::parse_document(args.get("document"))?;
                let rows = core::batch_rows(args, None)?;
                if rows.len() > 20 {
                    return Err(format!("{} rows; send at most 20 a call", rows.len()));
                }
                let jobs = core::batch_jobs(&doc, &rows, "{row}")?;
                let settings = core::export_settings(args)?;
                let mut done = Vec::new();
                for (i, (d, _)) in jobs.into_iter().enumerate() {
                    let value: Value = serde_json::from_str(&d.to_json()).map_err(|e| e.to_string())?;
                    match self.backend.render(&self.caller, value, settings, None) {
                        Ok(receipt) => done.push(json!({ "row": i + 1, "name": rows[i].get("name"), "url": receipt.get("url"), "receipt": receipt })),
                        Err(e) => done.push(json!({ "row": i + 1, "name": rows[i].get("name"), "error": e })),
                    }
                }
                Ok(json_text(&json!({ "videos": done })))
            }
            "ultrafast_beats" => {
                let id = args.get("audio").and_then(Value::as_str).ok_or("missing `audio`")?;
                let path = self.backend.asset_path(&self.caller, id)?;
                core::beats(&path, args)
            }
            "ultrafast_import_svg" => {
                let svg = args.get("svg").and_then(Value::as_str).ok_or("missing `svg`")?;
                let fit = args.get("size").and_then(Value::as_array).and_then(|v| match v.len() {
                    0 => None,
                    1 => v[0].as_f64().map(|x| [x, x]),
                    _ => Some([v[0].as_f64()?, v[1].as_f64()?]),
                });
                let imp = ultrafast_render::svg::import(svg.as_bytes(), None, fit)?;
                Ok(json_text(
                    &json!({ "component": imp.component, "size": imp.size, "warnings": imp.warnings }),
                ))
            }
            "ultrafast_measure" | "ultrafast_outline" => {
                let outline = name == "ultrafast_outline";
                core::text_node(args)?;
                let args = args.clone();
                self.backend.with_renderer(
                    &self.caller,
                    blank(),
                    Box::new(move |r, _| core::measure(r, &args, outline)),
                )
            }
            o => Err(format!("unknown tool `{o}`")),
        }
    }
}
