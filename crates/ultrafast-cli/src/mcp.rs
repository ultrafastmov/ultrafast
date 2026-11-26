//! `ultrafast mcp`: ultrafast as a Model Context Protocol server over stdio,
//! so an agent can drive it as tools (write a document, check it, look at
//! frames and contact sheets as images, edit it, export it). Documents are
//! files on this machine; the tool definitions and what they do with a
//! renderer are in `ultrafast_mcp`, shared with the remote server.
//!
//! JSON-RPC 2.0, one message per line on stdin/stdout. Anything else
//! written to stdout while serving goes to stderr instead. The server also
//! serves the ultrafast skill (`ultrafast_mcp::skills`) unless
//! `ULTRAFAST_MCP_SKILL=0`, which the plugins that ship the skill set.

use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ultrafast_graph::Document;
use ultrafast_live::Session;
use ultrafast_mcp::core;
use ultrafast_mcp::rpc::{Info, Roots, Toolbox, json_text, text};

struct Local {
    sessions: HashMap<PathBuf, Session>,
    roots: Roots,
    skills: bool,
}

/// The arguments of each tool that are file paths, absolute or relative to
/// the project folder. (`ultrafast_batch`'s `out` and `data` are relative
/// to the document's folder instead.)
fn path_keys(tool: &str) -> &'static [&'static str] {
    match tool {
        "ultrafast_new" | "ultrafast_check" | "ultrafast_frame" | "ultrafast_sheet" | "ultrafast_review"
        | "ultrafast_inspect" | "ultrafast_profile" | "ultrafast_estimate" | "ultrafast_batch" | "ultrafast_edit"
        | "ultrafast_beats" => &["path"],
        "ultrafast_export" => &["path", "out"],
        "ultrafast_decompose" => &["video", "out_dir"],
        "ultrafast_match_font" => &["path", "video"],
        "ultrafast_ocr" => &["video", "draft"],
        "ultrafast_voiceover" => &["wav", "words"],
        "ultrafast_import_svg" => &["path", "into"],
        "ultrafast_measure" | "ultrafast_outline" => &["base"],
        _ => &[],
    }
}

/// Whether `dir` is the folder of the plugin this server came with: a
/// client that runs plugin servers in their own folder (Antigravity CLI)
/// says so in PLUGIN_ROOT, Claude Code in CLAUDE_PLUGIN_ROOT.
fn plugin_folder(dir: &Path) -> bool {
    let Ok(dir) = std::fs::canonicalize(dir) else {
        return false;
    };
    ["PLUGIN_ROOT", "CLAUDE_PLUGIN_ROOT"].iter().any(|v| {
        std::env::var_os(v)
            .and_then(|root| std::fs::canonicalize(root).ok())
            .is_some_and(|root| root == dir)
    })
}

impl Local {
    /// The folder relative paths resolve against: the client's first root
    /// (that is a folder here); without one, the folder this server runs
    /// in, unless that is its plugin's own folder, which holds none of the
    /// agent's work (rather than write there, the call fails and says to use
    /// an absolute path).
    fn base(&self, rel: &Path) -> Result<PathBuf, String> {
        if let Some(root) = self.roots.known.iter().flatten().find(|r| r.is_dir()) {
            return Ok(root.clone());
        }
        let cwd = std::env::current_dir().map_err(|e| format!("the working folder: {e}"))?;
        if !plugin_folder(&cwd) {
            return Ok(cwd);
        }
        // The folder the client was started in, as a hint.
        let started = std::env::var_os("PWD")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir() && !plugin_folder(p));
        let mut e = format!(
            "`{}` is a relative path, and this server runs in its plugin's folder ({}), not in your project: use an absolute path",
            rel.display(),
            cwd.display()
        );
        if let Some(dir) = started {
            e += &format!(", e.g. {}", dir.join(rel).display());
        }
        Err(e)
    }

    /// A path argument made absolute (see [`Local::base`]).
    fn resolve(&self, p: &str) -> Result<PathBuf, String> {
        let p = Path::new(p);
        if p.is_absolute() {
            return Ok(p.to_path_buf());
        }
        Ok(self.base(p)?.join(p))
    }

    /// The `path` argument, made absolute.
    fn path(&self, a: &Value) -> Result<PathBuf, String> {
        self.resolve(arg(a, "path")?)
    }

    /// An optional path argument, made absolute.
    fn opt_path(&self, a: &Value, k: &str) -> Result<Option<PathBuf>, String> {
        a.get(k).and_then(Value::as_str).map(|p| self.resolve(p)).transpose()
    }

    fn session(&mut self, path: &Path) -> Result<&mut Session, String> {
        let key = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !self.sessions.contains_key(&key) {
            let s = Session::load(&key)?;
            self.sessions.insert(key.clone(), s);
        }
        let s = self.sessions.get_mut(&key).unwrap();
        // Pick up edits made to the file by anyone.
        s.poll_file();
        Ok(s)
    }

    /// Runs `f` with the session's renderer and document.
    fn with(
        &mut self,
        a: &Value,
        f: impl FnOnce(&mut ultrafast_render::Renderer, &Document, &Value) -> Result<Value, String>,
    ) -> Result<Value, String> {
        let p = self.path(a)?;
        let s = self.session(&p)?;
        f(&mut s.renderer, &s.doc, a)
    }
}

fn arg<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    a.get(k).and_then(Value::as_str).ok_or(format!("missing `{k}`"))
}

impl Toolbox for Local {
    fn tools(&self) -> Value {
        ultrafast_mcp::tools::local()
    }

    fn roots(&mut self) -> Option<&mut Roots> {
        Some(&mut self.roots)
    }

    fn skills(&self) -> bool {
        self.skills
    }

    fn needs_roots(&self, name: &str, a: &Value) -> bool {
        path_keys(name).iter().any(|k| {
            a.get(*k)
                .and_then(Value::as_str)
                .is_some_and(|p| Path::new(p).is_relative())
        })
    }

    fn call(&mut self, name: &str, a: &Value) -> Result<Value, String> {
        let mut out = self.run(name, a)?;
        // A driver that renders on the CPU makes every render many times
        // slower. The server's stderr reaches the client's log, not the
        // agent: say it in the results of the tools that render.
        let renders = [
            "ultrafast_frame",
            "ultrafast_sheet",
            "ultrafast_review",
            "ultrafast_export",
            "ultrafast_batch",
        ];
        if renders.contains(&name)
            && let Some(s) = self.sessions.values().next()
            && s.renderer.device().is_cpu()
            && let Some(content) = out.get_mut("content").and_then(Value::as_array_mut)
        {
            let note = ultrafast_gpu::cpu_device_warning(&s.renderer.device().name());
            content.push(json!({ "type": "text", "text": format!("warning: {note}") }));
        }
        Ok(out)
    }
}

impl Local {
    fn run(&mut self, name: &str, a: &Value) -> Result<Value, String> {
        match name {
            "ultrafast_ops" => Ok(core::ops(a)),
            "ultrafast_new" => {
                let p = self.path(a)?;
                let src = core::template(a)?;
                if p.exists() {
                    return Err(format!("{} exists; not overwriting", p.display()));
                }
                std::fs::write(&p, src).map_err(|e| format!("{}: {e}", p.display()))?;
                Ok(text(format!("wrote {}", p.display())))
            }
            "ultrafast_check" => self.with(a, |r, d, _| Ok(core::check(r, d))),
            "ultrafast_frame" => self.with(a, core::frame),
            "ultrafast_sheet" => self.with(a, core::sheet),
            "ultrafast_review" => self.with(a, core::review),
            "ultrafast_inspect" => self.with(a, core::inspect),
            "ultrafast_profile" => self.with(a, core::profile),
            "ultrafast_estimate" => {
                let p = self.path(a)?;
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.with(a, |r, d, a| core::estimate(r, d, a, &name))
            }
            "ultrafast_batch" => {
                let p = self.path(a)?;
                let dir = p.parent().unwrap_or(Path::new(".")).to_path_buf();
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or("film".into());
                let pattern = a
                    .get("out")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or(format!("{stem}-{{row}}.mp4"));
                let rows = core::batch_rows(a, Some(&dir))?;
                let opts = core::export_options(a)?;
                let s = self.session(&p)?;
                let jobs = core::batch_jobs(&s.doc, &rows, &pattern)?;
                for (i, (d, out)) in jobs.iter().enumerate() {
                    opts.settings
                        .resolve(Some(Path::new(out)), &d.format)
                        .map_err(|e| format!("row {}: {e}", i + 1))?;
                }
                let mut done = Vec::new();
                for (i, (d, out)) in jobs.iter().enumerate() {
                    let out = if Path::new(out).is_relative() {
                        dir.join(out)
                    } else {
                        PathBuf::from(out)
                    };
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
                    }
                    let rep = ultrafast_live::export::export_video(&mut s.renderer, d, &out, opts, |_, _| {})
                        .map_err(|e| format!("row {}: {e}", i + 1))?;
                    done.push(json!({ "row": i + 1, "out": out, "frames": rep.frames, "seconds": rep.seconds }));
                }
                Ok(json_text(&json!({ "videos": done })))
            }
            "ultrafast_edit" => {
                let p = self.path(a)?;
                let src = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let doc = Document::from_json(&src).map_err(|e| e.to_string())?;
                let (doc, report) = core::edit(doc, a)?;
                std::fs::write(&p, doc.to_json()).map_err(|e| e.to_string())?;
                Ok(json_text(&report))
            }
            "ultrafast_export" => {
                let out = self.resolve(arg(a, "out")?)?;
                let opts = core::export_options(a)?;
                let p = self.path(a)?;
                let s = self.session(&p)?;
                let doc = s.doc.clone();
                // Every combination is checked before anything renders.
                let target = opts.settings.resolve(Some(&out), &doc.format)?;
                if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                let rep = ultrafast_live::export::export_target(&mut s.renderer, &doc, &out, target, opts, |_, _| {})?;
                let t = rep.target;
                Ok(text(if t.video.is_none() {
                    format!(
                        "exported {} sound in {:.2} s -> {}",
                        t.label(),
                        rep.seconds,
                        out.display()
                    )
                } else {
                    format!(
                        "exported {} frames ({}x{} @ {} fps, {}) in {:.2} s ({:.0} fps) -> {}",
                        rep.frames,
                        t.width,
                        t.height,
                        (t.fps * 1000.0).round() / 1000.0,
                        t.label(),
                        rep.seconds,
                        rep.fps,
                        out.display()
                    )
                }))
            }
            "ultrafast_decompose" => {
                let video = self.resolve(arg(a, "video")?)?;
                let dir = self.resolve(arg(a, "out_dir")?)?;
                let mut opts = ultrafast_recon::layers::LayerOptions::default();
                if let Some(t) = a.get("threshold").and_then(Value::as_f64) {
                    opts.threshold = t as f32;
                }
                if let Some(c) = a.get("colors").and_then(Value::as_u64) {
                    opts.max_colors = c as usize;
                }
                let device = ultrafast_gpu::Device::new().map_err(|e| e.to_string())?;
                let (_, r) = ultrafast_recon::layers::decompose(&device, &video, &dir, &opts, |_| {})?;
                // The summary: where it went, how close it is, what text came back.
                let shots: Vec<Value> = r["shots"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .map(|sh| {
                                let layers = sh["layers"].as_array().cloned().unwrap_or_default();
                                json!({
                                    "shot": sh["shot"], "start": sh["start"], "end": sh["end"],
                                    "elements": sh["elements"], "layers": layers.len(),
                                    "text": layers.iter().filter(|l| l.get("text").is_some()).collect::<Vec<_>>(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json_text(&json!({
                    "document": r["document"], "mean_psnr": r["mean_psnr"], "min_psnr": r["min_psnr"],
                    "worst": r["worst"], "shots": shots,
                })))
            }
            "ultrafast_match_font" => {
                let p = self.path(a)?;
                let video = self.resolve(arg(a, "video")?)?;
                let node = arg(a, "node")?;
                let t = a.get("t").and_then(Value::as_f64).ok_or("missing `t`")?;
                let mut opts = ultrafast_recon::fonts::MatchOptions::default();
                if let Some(f) = a.get("font").and_then(Value::as_str) {
                    let mut parts = f.split(':');
                    let font = parts.next().unwrap_or("").to_string();
                    let weight = parts.next().and_then(|w| w.parse().ok()).unwrap_or(400);
                    opts.only = Some((font, weight, parts.next() == Some("italic")));
                }
                if let Some(v) = a.get("ink").and_then(Value::as_array) {
                    let v: Vec<f64> = v.iter().filter_map(Value::as_f64).collect();
                    if v.len() != 4 {
                        return Err("`ink` is [x0, y0, x1, y1]".into());
                    }
                    opts.ink = Some([v[0], v[1], v[2], v[3]]);
                }
                let base = p.parent().unwrap_or(Path::new(".")).to_path_buf();
                let s = self.session(&p)?;
                let doc = s.doc.clone();
                let device = ultrafast_gpu::Device::new().map_err(|e| e.to_string())?;
                let (matched, report) =
                    ultrafast_recon::fonts::match_font(&device, &doc, &base, &video, node, t, &opts)?;
                std::fs::write(&p, matched.to_json()).map_err(|e| e.to_string())?;
                Ok(json_text(&report))
            }
            "ultrafast_ocr" => {
                let video = self.resolve(arg(a, "video")?)?;
                let out = self.opt_path(a, "draft")?;
                let t = a.get("t").and_then(Value::as_f64).unwrap_or(0.0);
                let (report, draft) = crate::ocr_impl(&video, t).map_err(|e| e.to_string())?;
                if let Some(out) = out {
                    std::fs::write(&out, draft).map_err(|e| format!("{}: {e}", out.display()))?;
                }
                Ok(json_text(&report))
            }
            "ultrafast_beats" => core::beats(&self.path(a)?, a),
            "ultrafast_voiceover" => {
                let (wav, words) = (self.resolve(arg(a, "wav")?)?, self.resolve(arg(a, "words")?)?);
                let out = crate::voiceover(
                    arg(a, "text")?,
                    &wav,
                    &words,
                    a.get("voice").and_then(Value::as_str),
                    a.get("rate").and_then(Value::as_f64).map(|r| r.to_string()).as_deref(),
                )
                .map_err(|e| e.to_string())?;
                Ok(text(out))
            }
            "ultrafast_import_svg" => {
                let p = self.path(a)?;
                let into = self.opt_path(a, "into")?;
                let data = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let fit = a.get("size").and_then(Value::as_array).and_then(|v| match v.len() {
                    0 => None,
                    1 => v[0].as_f64().map(|x| [x, x]),
                    _ => Some([v[0].as_f64()?, v[1].as_f64()?]),
                });
                let imp = ultrafast_render::svg::import(&data, p.parent(), fit)?;
                match into {
                    Some(doc_path) => {
                        let name = a
                            .get("name")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .or_else(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| "svg".into());
                        let s =
                            std::fs::read_to_string(&doc_path).map_err(|e| format!("{}: {e}", doc_path.display()))?;
                        let mut doc = Document::from_json(&s).map_err(|e| e.to_string())?;
                        doc.components.insert(name.clone(), imp.component);
                        std::fs::write(&doc_path, doc.to_json()).map_err(|e| e.to_string())?;
                        Ok(json_text(
                            &json!({ "component": name, "size": imp.size, "warnings": imp.warnings }),
                        ))
                    }
                    None => Ok(json_text(
                        &json!({ "component": imp.component, "size": imp.size, "warnings": imp.warnings }),
                    )),
                }
            }
            "ultrafast_measure" | "ultrafast_outline" => {
                // Font files named by relative path are found from `base`,
                // or the project folder.
                let base = match self.opt_path(a, "base")? {
                    Some(b) => b,
                    None => self.base(Path::new(".")).unwrap_or_else(|_| PathBuf::from(".")),
                };
                let device = ultrafast_gpu::Device::new().map_err(|e| e.to_string())?;
                let mut r = ultrafast_render::Renderer::new(device).map_err(|e| e.to_string())?;
                r.set_base_dir(&base);
                core::measure(&mut r, a, name == "ultrafast_outline")
            }
            o => Err(format!("unknown tool `{o}`")),
        }
    }
}

pub fn serve() -> Result<()> {
    let mut server = Local {
        sessions: HashMap::new(),
        roots: Roots::default(),
        skills: ultrafast_mcp::skills::wanted(),
    };
    let info = Info::new(ultrafast_mcp::tools::instructions(false));
    ultrafast_mcp::rpc::serve_stdio(&mut server, &info)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every argument that names a file is resolved against the project
    /// folder, and only those are.
    #[test]
    fn path_keys_match_the_tool_schemas() {
        let files = [
            "path", "out", "video", "out_dir", "wav", "words", "draft", "into", "base",
        ];
        for t in ultrafast_mcp::tools::local().as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            let props = t["inputSchema"]["properties"].as_object().unwrap();
            let keys = path_keys(name);
            for k in keys {
                assert!(props.contains_key(*k), "{name}: no `{k}`");
            }
            for k in props.keys().filter(|k| files.contains(&k.as_str())) {
                // ultrafast_batch's `out` is a pattern in the document's folder.
                let own = name == "ultrafast_batch" && k == "out";
                assert!(keys.contains(&k.as_str()) || own, "{name}: `{k}` is not resolved");
            }
        }
        assert!(path_keys("not_a_tool").is_empty());
    }
}
