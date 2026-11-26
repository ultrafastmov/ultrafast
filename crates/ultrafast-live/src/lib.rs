//! Live ultrafast sessions.
//!
//! A session owns a document and a renderer. Agents talk to it over a
//! JSON-lines protocol (one request object per line, one response per line),
//! usually through the TCP server in [`server`]. Requests are drained once
//! per frame on the render thread, so an edit shows on the next frame and
//! never waits behind other work.
//!
//! Protocol summary (`id` is echoed back if present):
//!
//! | cmd | fields | returns |
//! |---|---|---|
//! | `ping` | | `pong` |
//! | `get` | | `doc` |
//! | `set` | `doc` | `revision` |
//! | `load` | `path` | `revision` |
//! | `save` | `path?` | `path` |
//! | `edit` | `edits: [Edit]` | `change`, `revision` |
//! | `describe` | | `nodes: [{id, op, inputs}]`, `output` |
//! | `ops` | | op vocabulary: ports, params with types, defaults, docs |
//! | `check` | | static problems: unknown ops/params (with suggestions), bad values |
//! | `plan` | | `plan` (text) |
//! | `play` / `pause` | | `playing` |
//! | `seek` | `t` | `t` |
//! | `rate` | `value` | `rate` |
//! | `snapshot` | `t?`, `path?` | `path`, `width`, `height` (exact render, PNG) |
//! | `contact` | `times?`, `count?`, `path?`, `columns?` | `path`: a grid of frames for reviewing motion |
//! | `bounds` | `node` | `bounds: [x0, y0, x1, y1]` |
//! | `player` | `t?`, `width?`, `height?`, `path?` | PNG of what the player shows (frame + timeline) |
//! | `loop` | `in?`, `out?` | loop playback between in and out (omit both to clear) |
//! | `stats` | | timings, transfers, kernels, diagnostics |
//! | `quit` | | stops the session |

pub mod audio_mix;
pub mod estimate;
pub mod export;
pub mod ffmpeg;
pub mod output;
#[cfg(all(feature = "audio", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
pub mod preview_audio;
pub mod record;
pub mod server;
pub mod ts;
#[cfg(all(feature = "window", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
pub mod window;

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};
use ultrafast_graph::{Document, Edit};
use ultrafast_render::{Mode, Renderer, ops};

pub struct Session {
    pub doc: Document,
    pub renderer: Renderer,
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
    playing: bool,
    rate: f64,
    t: f64,
    clock: Instant,
    /// (cpu encode ms, gpu ms) of recent frames.
    timings: VecDeque<(f64, f64)>,
    frames: u64,
    started: Instant,
    /// Set by the `quit` request; the host loop exits.
    pub quit: bool,
    /// Bumped on every discontinuity (seek, play, pause, new document) so
    /// external clocks such as preview audio can realign.
    epoch: u64,
    /// Playback loops between these times when set.
    loop_range: Option<(f64, f64)>,
    /// Nodes recently changed by requests, for the player to point at.
    edits: VecDeque<(String, Instant)>,
    /// What a person pointed at in the player (option-click), or an agent
    /// with `select`: shown outlined until cleared.
    pub selection: Option<ultrafast_render::Picked>,
    /// A reference video to compare against, and how it is shown.
    compare: Option<(PathBuf, CompareMode)>,
    /// The document as the player shows it (with the reference over it),
    /// rebuilt when the document or the compare mode changes.
    view: Option<(Document, (u64, u64, u8))>,
    view_revision: u64,
    /// When media files were last checked for changes on disk.
    media_checked: Instant,
    /// Bumped whenever changed media files were dropped (the picture must
    /// be drawn again even though the document did not change).
    media_epoch: u64,
}

/// How the player shows a reference video next to the document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CompareMode {
    /// The document left of a divider at this fraction of the width, the
    /// reference right of it.
    Split(f32),
    /// Their difference, brightened two stops (black where they agree).
    Difference,
}

/// Node ids the compare view adds to the document.
const VIEW_IDS: [&str; 5] = [
    "__compare_ref",
    "__compare_cut",
    "__compare_mix",
    "__compare_line",
    "__compare_out",
];

const TIMING_WINDOW: usize = 240;

impl Session {
    /// Nodes changed by requests in the last `within` seconds, with how long
    /// ago, newest last.
    pub fn recent_edits(&mut self, within: f64) -> Vec<(String, f64)> {
        self.edits.retain(|(_, at)| at.elapsed().as_secs_f64() < within);
        self.edits
            .iter()
            .map(|(n, at)| (n.clone(), at.elapsed().as_secs_f64()))
            .collect()
    }

    /// Selects what is under frame point (x, y) now (None clears).
    pub fn select_at(&mut self, x: f32, y: f32) -> Option<ultrafast_render::Picked> {
        self.selection = self.renderer.pick(&self.doc, self.t, x, y);
        self.selection.clone()
    }

    pub fn new(doc: Document, path: Option<PathBuf>) -> Result<Self, String> {
        let device = ultrafast_gpu::Device::new().map_err(|e| e.to_string())?;
        let mut renderer = Renderer::new(device).map_err(|e| e.to_string())?;
        renderer.set_mode(Mode::Live);
        if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
            renderer.set_base_dir(dir);
        }
        let mtime = path.as_ref().and_then(|p| std::fs::metadata(p).ok()?.modified().ok());
        Ok(Self {
            doc,
            renderer,
            path,
            mtime,
            playing: true,
            rate: 1.0,
            t: 0.0,
            clock: Instant::now(),
            timings: VecDeque::new(),
            frames: 0,
            started: Instant::now(),
            quit: false,
            epoch: 0,
            loop_range: None,
            edits: VecDeque::new(),
            selection: None,
            compare: None,
            view: None,
            view_revision: 0,
            media_checked: Instant::now(),
            media_epoch: 0,
        })
    }

    /// Re-reads images and videos whose files changed on disk (a look
    /// re-rendered into the same path), now, or at most once a second when
    /// `throttled`. Returns whether anything changed.
    pub fn poll_media(&mut self, throttled: bool) -> bool {
        if throttled && self.media_checked.elapsed().as_secs_f64() < 1.0 {
            return false;
        }
        self.media_checked = Instant::now();
        let n = self.renderer.refresh_changed_media();
        if n > 0 {
            self.media_epoch += 1;
            eprintln!("ultrafast: {n} media file(s) changed on disk; reading them again");
        }
        n > 0
    }

    /// Changes whenever the media under the document changed.
    pub fn media_epoch(&self) -> u64 {
        self.media_epoch
    }

    /// Shows `video` against the document (`None` turns it off).
    pub fn set_compare(&mut self, compare: Option<(PathBuf, CompareMode)>) {
        let same_shape = match (&self.compare, &compare) {
            (Some((a, CompareMode::Split(_))), Some((b, CompareMode::Split(_)))) => a == b,
            _ => false,
        };
        if same_shape
            && let Some((_, CompareMode::Split(x))) = compare
            && let Some((view, _)) = self.view.as_mut()
        {
            // Moving the divider: two params, no replan.
            let w = self.doc.format.width as f64;
            let h = self.doc.format.height as f64;
            let at = (x as f64).clamp(0.0, 1.0) * w;
            let edits = [
                ultrafast_graph::Edit::SetParam {
                    node: VIEW_IDS[1].into(),
                    param: "rect".into(),
                    value: ultrafast_graph::Param::Vector(vec![at, 0.0, w - at, h]),
                },
                ultrafast_graph::Edit::SetParam {
                    node: VIEW_IDS[3].into(),
                    param: "center".into(),
                    value: ultrafast_graph::Param::Vector(vec![at, h / 2.0]),
                },
            ];
            let _ = view.apply(&edits);
            self.compare = compare;
            return;
        }
        self.compare = compare;
        self.view = None;
    }

    pub fn compare(&self) -> Option<&(PathBuf, CompareMode)> {
        self.compare.as_ref()
    }

    /// The renderer and the document as the player shows it.
    pub fn view(&mut self) -> (&mut Renderer, &Document) {
        let Some((video, mode)) = self.compare.clone() else {
            return (&mut self.renderer, &self.doc);
        };
        let key = (
            self.doc.revision,
            self.doc.structure_revision,
            matches!(mode, CompareMode::Difference) as u8,
        );
        if self.view.as_ref().is_none_or(|v| v.1 != key) {
            let (w, h) = (self.doc.format.width as f64, self.doc.format.height as f64);
            let mut v = serde_json::to_value(&self.doc).unwrap();
            let out = v["output"].clone();
            let nodes = v["nodes"].as_object_mut().unwrap();
            nodes.insert(
                VIEW_IDS[0].into(),
                json!({ "op": "video", "params": { "src": video.to_string_lossy(), "rect": [0, 0, w, h], "start": 0 } }),
            );
            match mode {
                CompareMode::Split(x) => {
                    let at = (x as f64).clamp(0.0, 1.0) * w;
                    nodes.insert(VIEW_IDS[1].into(), json!({ "op": "crop", "params": { "rect": [at, 0, w - at, h], "softness": 0 }, "inputs": { "source": VIEW_IDS[0] } }));
                    nodes.insert(
                        VIEW_IDS[2].into(),
                        json!({ "op": "over", "inputs": { "fg": VIEW_IDS[1], "bg": out } }),
                    );
                    nodes.insert(VIEW_IDS[3].into(), json!({ "op": "shape", "params": { "kind": "rect", "center": [at, h / 2.0], "size": [2, h], "color": "#22d3ee" } }));
                    nodes.insert(
                        VIEW_IDS[4].into(),
                        json!({ "op": "over", "inputs": { "fg": VIEW_IDS[3], "bg": VIEW_IDS[2] } }),
                    );
                }
                CompareMode::Difference => {
                    nodes.insert(VIEW_IDS[2].into(), json!({ "op": "blend", "params": { "mode": "difference", "space": "srgb" }, "inputs": { "fg": VIEW_IDS[0], "bg": out } }));
                    nodes.insert(
                        VIEW_IDS[4].into(),
                        json!({ "op": "adjust", "params": { "exposure": 2 }, "inputs": { "source": VIEW_IDS[2] } }),
                    );
                }
            }
            v["output"] = json!(VIEW_IDS[4]);
            let mut doc: Document = serde_json::from_value(v).unwrap();
            self.view_revision += 1;
            doc.revision = self.view_revision;
            doc.structure_revision = self.view_revision;
            self.view = Some((doc, key));
        }
        (&mut self.renderer, &self.view.as_ref().unwrap().0)
    }

    /// PSNR (dB, RGB) of the document's frame at `t` against the reference
    /// video's.
    pub fn compare_psnr(&mut self, t: f64) -> Result<f64, String> {
        let (video, _) = self.compare.clone().ok_or("no reference video to compare with")?;
        let ours = self.render_exact(t)?;
        let (w, h) = (self.doc.format.width, self.doc.format.height);
        let only = Document::from_json(
            &json!({
                "version": 1,
                "format": { "width": w, "height": h, "fps": self.doc.format.fps, "duration": self.doc.format.duration },
                "output": "v",
                "nodes": { "v": { "op": "video", "params": { "src": video.to_string_lossy(), "rect": [0, 0, w, h], "start": 0 } } }
            })
            .to_string(),
        )
        .map_err(|e| e.to_string())?;
        self.renderer.set_mode(Mode::Exact);
        let theirs = self
            .renderer
            .render_rgba8(&only, t)
            .map(|(px, _)| px)
            .map_err(|e| e.to_string());
        self.renderer.set_mode(Mode::Live);
        let theirs = theirs?;
        let mut se = 0f64;
        for (a, b) in ours.chunks_exact(4).zip(theirs.chunks_exact(4)) {
            for c in 0..3 {
                se += (a[c] as f64 - b[c] as f64).powi(2);
            }
        }
        let mse = se / (3 * (w as usize) * (h as usize)).max(1) as f64;
        Ok(if mse <= 0.0 {
            100.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let doc = read_doc(path)?;
        Self::new(doc, Some(path.to_path_buf()))
    }

    /// Current playback time, advanced by the wall clock while playing.
    pub fn advance(&mut self) -> f64 {
        let now = Instant::now();
        if self.playing {
            let dur = self.doc.format.duration.max(1e-6);
            let next = self.t + now.duration_since(self.clock).as_secs_f64() * self.rate;
            match self.loop_range {
                Some((a, b)) if next >= b || next < a => {
                    self.t = a + (next - a).rem_euclid(b - a);
                    self.epoch += 1;
                }
                _ => {
                    if next >= dur {
                        self.epoch += 1;
                    }
                    self.t = next.rem_euclid(dur);
                }
            }
        }
        self.clock = now;
        self.t
    }

    /// Loop playback between `a` and `b` (seconds), or stop looping.
    pub fn set_loop(&mut self, range: Option<(f64, f64)>) {
        self.loop_range = range.filter(|(a, b)| b - a > 1e-3);
    }

    pub fn loop_range(&self) -> Option<(f64, f64)> {
        self.loop_range
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        self.clock = Instant::now();
        self.epoch += 1;
    }

    pub fn time(&self) -> f64 {
        self.t
    }

    /// Sets the playback time from an external clock (preview audio).
    pub fn set_time(&mut self, t: f64) {
        self.t = t;
        self.clock = Instant::now();
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Directory relative media paths resolve against.
    pub fn base_dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn toggle_play(&mut self) {
        self.playing = !self.playing;
        self.clock = Instant::now();
        self.epoch += 1;
    }

    pub fn seek(&mut self, t: f64) {
        self.t = t.clamp(0.0, self.doc.format.duration);
        self.clock = Instant::now();
        self.epoch += 1;
    }

    pub fn record_frame(&mut self, cpu_ms: f64, gpu_ms: f64) {
        self.frames += 1;
        self.timings.push_back((cpu_ms, gpu_ms));
        while self.timings.len() > TIMING_WINDOW {
            self.timings.pop_front();
        }
    }

    /// Reloads the document if its file changed on disk (agents may edit the
    /// JSON directly). Plans and kernels are reused when only values changed.
    pub fn poll_file(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let Ok(m) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            return;
        };
        if Some(m) == self.mtime {
            return;
        }
        self.mtime = Some(m);
        match read_doc(&path) {
            // As the edits that turn the old version into the new one: a
            // value change keeps the plan and pipelines (no recompile, no
            // hitch), and the player outlines what changed.
            Ok(doc) => {
                self.poll_media(false);
                // What the file says that reading it drops, as it says it now.
                self.doc.unread = doc.unread.clone();
                let edits = self.doc.diff(&doc);
                if edits.is_empty() {
                    return;
                }
                if let Err(e) = self.edit(&edits) {
                    eprintln!("ultrafast: reloading {} whole ({e})", path.display());
                    self.replace(doc);
                }
            }
            Err(e) => eprintln!("ultrafast: not reloading {}: {e}", path.display()),
        }
    }

    /// Applies an edit batch, outlines what it touched for the person
    /// watching, and reports problems on those nodes (typos, wrong value
    /// shapes) so agents notice ignored params.
    fn edit(&mut self, edits: &[Edit]) -> Result<Value, String> {
        let change = self.doc.apply(edits).map_err(|e| e.to_string())?;
        let mut touched: Vec<String> = Vec::new();
        for e in edits {
            let n = match e {
                Edit::SetParam { node, .. } | Edit::RemoveParam { node, .. } => node,
                Edit::AddNode { id, .. } | Edit::AddLayer { id, .. } => id,
                Edit::Connect { node, .. } | Edit::Disconnect { node, .. } => node,
                Edit::SetInputs { node, .. } | Edit::SetTiming { node, .. } => node,
                _ => continue,
            };
            if self.doc.nodes.contains_key(n) && !touched.contains(n) {
                touched.push(n.clone());
            }
        }
        let now = Instant::now();
        for n in &touched {
            self.edits.retain(|(m, _)| m != n);
            self.edits.push_back((n.clone(), now));
        }
        while self.edits.len() > 8 {
            self.edits.pop_front();
        }
        let mut resp = json!({ "change": format!("{change:?}").to_lowercase(), "revision": self.doc.revision });
        let issues = issues_json(&self.doc, Some(&touched));
        if !issues.is_empty() {
            resp["issues"] = json!(issues);
        }
        Ok(resp)
    }

    fn replace(&mut self, mut doc: Document) {
        doc.revision = self.doc.revision + 1;
        doc.structure_revision = self.doc.structure_revision + 1;
        self.doc = doc;
        self.renderer.clear_diagnostics();
        self.epoch += 1;
    }

    /// Handles one protocol request.
    pub fn handle(&mut self, req: &Value) -> Value {
        let id = req.get("id").cloned();
        let mut resp = match self.dispatch(req) {
            Ok(v) => {
                let mut v = if v.is_object() { v } else { json!({ "result": v }) };
                v["ok"] = json!(true);
                v
            }
            Err(e) => json!({ "ok": false, "error": e }),
        };
        if let Some(id) = id {
            resp["id"] = id;
        }
        resp
    }

    fn dispatch(&mut self, req: &Value) -> Result<Value, String> {
        let cmd = req
            .get("cmd")
            .and_then(Value::as_str)
            .ok_or("request needs a `cmd` string")?;
        let num = |k: &str| req.get(k).and_then(Value::as_f64);
        let text = |k: &str| req.get(k).and_then(Value::as_str);
        match cmd {
            "ping" => Ok(json!({ "pong": true })),
            "quit" => {
                self.quit = true;
                Ok(json!({ "quitting": true }))
            }
            "get" => Ok(json!({ "doc": serde_json::to_value(&self.doc).unwrap(), "revision": self.doc.revision })),
            "set" => {
                let v = req.get("doc").ok_or("`set` needs `doc`")?;
                let doc = Document::from_json(&v.to_string()).map_err(|e| e.to_string())?;
                self.replace(doc);
                Ok(json!({ "revision": self.doc.revision }))
            }
            "load" => {
                let path = PathBuf::from(text("path").ok_or("`load` needs `path`")?);
                let doc = read_doc(&path)?;
                if let Some(dir) = path.parent() {
                    self.renderer.set_base_dir(dir);
                }
                self.mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
                self.path = Some(path);
                self.replace(doc);
                self.poll_media(false);
                Ok(json!({ "revision": self.doc.revision }))
            }
            "save" => {
                let path = text("path")
                    .map(PathBuf::from)
                    .or_else(|| self.path.clone())
                    .ok_or("no path to save to")?;
                std::fs::write(&path, self.doc.to_json()).map_err(|e| e.to_string())?;
                if Some(&path) == self.path.as_ref() {
                    self.mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
                }
                Ok(json!({ "path": path }))
            }
            "edit" => {
                let v = req.get("edits").cloned().ok_or("`edit` needs `edits`")?;
                let edits: Vec<Edit> = ultrafast_graph::parse_edits(&v)?;
                let mut resp = self.edit(&edits)?;
                // Keys the edits' nodes and timings don't have were dropped.
                let unread: Vec<Value> = ultrafast_graph::unread_in_edits(&v)
                    .iter()
                    .map(|u| json!({ "node": u.node, "severity": "warning", "message": ultrafast_render::schema::unread_message(&self.doc, u) }))
                    .collect();
                if !unread.is_empty() {
                    let mut issues = resp
                        .get("issues")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    issues.extend(unread);
                    resp["issues"] = json!(issues);
                }
                Ok(resp)
            }
            "describe" => {
                let nodes: Vec<Value> = self
                    .doc
                    .nodes
                    .iter()
                    .map(|(id, n)| json!({ "id": id, "op": n.op, "inputs": n.inputs }))
                    .collect();
                let components: Vec<Value> = self
                    .doc
                    .components
                    .iter()
                    .map(|(name, c)| json!({ "name": name, "params": c.params, "inputs": c.inputs, "nodes": c.nodes.len() }))
                    .collect();
                Ok(json!({
                    "output": self.doc.output,
                    "format": serde_json::to_value(self.doc.format).unwrap(),
                    "nodes": nodes,
                    "components": components,
                }))
            }
            "ops" => Ok(json!({ "ops": ultrafast_render::schema::vocabulary() })),
            "check" => Ok(json!({ "issues": issues_json(&self.doc, None) })),
            "plan" => {
                self.render_exact(self.t)?;
                Ok(json!({ "plan": self.renderer.plan().map(|p| p.describe()).unwrap_or_default() }))
            }
            "play" => {
                self.playing = true;
                self.clock = Instant::now();
                self.epoch += 1;
                Ok(json!({ "playing": true }))
            }
            "pause" => {
                self.playing = false;
                self.epoch += 1;
                Ok(json!({ "playing": false, "t": self.t }))
            }
            "seek" => {
                self.seek(num("t").ok_or("`seek` needs `t`")?);
                Ok(json!({ "t": self.t }))
            }
            "rate" => {
                self.rate = num("value").ok_or("`rate` needs `value`")?;
                Ok(json!({ "rate": self.rate }))
            }
            "snapshot" => {
                let t = num("t").unwrap_or(self.t);
                let alpha = req.get("alpha").and_then(Value::as_bool).unwrap_or(false);
                let px = self.render_exact_as(t, alpha)?;
                let path = text("path").map(PathBuf::from).unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("ultrafast-{}-{:.3}.png", self.doc.revision, t))
                });
                write_png(&path, self.doc.format.width, self.doc.format.height, &px)?;
                Ok(json!({ "path": path, "t": t, "width": self.doc.format.width, "height": self.doc.format.height }))
            }
            "contact" => {
                let times: Vec<f64> = match req.get("times").and_then(Value::as_array) {
                    Some(a) => a.iter().filter_map(Value::as_f64).collect(),
                    None => {
                        let n = num("count").unwrap_or(9.0).max(1.0) as usize;
                        let d = self.doc.format.duration;
                        (0..n).map(|i| d * i as f64 / n as f64).collect()
                    }
                };
                let cols = num("columns").unwrap_or(3.0).max(1.0) as usize;
                let path = text("path").map(PathBuf::from).unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("ultrafast-contact-{}.png", self.doc.revision))
                });
                let (w, h) = self.contact_sheet(&times, cols, &path)?;
                Ok(json!({ "path": path, "times": times, "width": w, "height": h }))
            }
            "bounds" => {
                let node = text("node").ok_or("`bounds` needs `node`")?;
                if self.renderer.node_bounds(node).is_none() {
                    self.render_exact(self.t)?;
                }
                let b = self
                    .renderer
                    .node_bounds(node)
                    .ok_or("bounds are known for path and text nodes only")?;
                Ok(json!({ "bounds": b }))
            }
            "player" => {
                // What the player window shows at `t`: letterboxed frame and
                // timeline, as a PNG (for checking playback UI and framing).
                let t = num("t").unwrap_or(self.t);
                let (w, h) = (
                    num("width").unwrap_or(1280.0) as u32,
                    num("height").unwrap_or(800.0) as u32,
                );
                let dur = self.doc.format.duration.max(1e-6);
                let (a, b) = self.loop_range.map(|(a, b)| (a / dur, b / dur)).unwrap_or((0.0, 0.0));
                let hud = [
                    1.0,
                    (t / dur) as f32,
                    a as f32,
                    b as f32,
                    0.0,
                    self.playing as u8 as f32,
                ];
                let (fw, fh) = (self.doc.format.width as f32, self.doc.format.height as f32);
                let edits = self.recent_edits(1.4);
                if edits.iter().any(|(n, _)| self.renderer.content_bounds(n).is_none()) {
                    // Bounds come from rendering: render this frame first.
                    self.renderer.set_mode(Mode::Exact);
                    let _ = self.renderer.render_rgba8(&self.doc, t);
                    self.renderer.set_mode(Mode::Live);
                }
                let mut marks: Vec<([f32; 4], f32)> = self
                    .recent_edits(1.4)
                    .iter()
                    .rev()
                    .take(4)
                    .map(|(n, age)| {
                        let b = self.renderer.content_bounds(n).unwrap_or([0.0, 0.0, fw, fh]);
                        (b, (1.0 - age / 1.4) as f32)
                    })
                    .collect();
                if let Some(sel) = &self.selection {
                    marks.truncate(3);
                    marks.push((sel.bounds, -1.0));
                }
                let (renderer, view) = self.view();
                renderer.set_mode(Mode::Exact);
                let px = renderer
                    .render_presented(view, t, w, h, hud, &marks)
                    .map_err(|e| e.to_string());
                renderer.set_mode(Mode::Live);
                let path = text("path")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::temp_dir().join(format!("ultrafast-player-{:.3}.png", t)));
                write_png(&path, w, h, &px?)?;
                Ok(json!({ "path": path, "t": t, "width": w, "height": h }))
            }
            "loop" => {
                let range = match (num("in"), num("out")) {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                };
                self.set_loop(range);
                Ok(json!({ "loop": self.loop_range.map(|(a, b)| [a, b]) }))
            }
            "stats" => Ok(self.stats()),
            // What the person watching pointed at (option-click in the player).
            "selection" => Ok(json!({ "selection": self.selection, "t": self.t })),
            // Point at something for the person watching: by node, or by
            // frame point; {"cmd":"select"} alone clears.
            "select" => {
                if let Some(node) = req.get("node").and_then(Value::as_str) {
                    self.render_exact(self.t)?;
                    let b = self.renderer.content_bounds(node).unwrap_or([
                        0.0,
                        0.0,
                        self.doc.format.width as f32,
                        self.doc.format.height as f32,
                    ]);
                    self.selection = Some(ultrafast_render::Picked {
                        op: self
                            .doc
                            .nodes
                            .get(node)
                            .map(|n| n.op.clone())
                            .ok_or(format!("no node `{node}`"))?,
                        node: node.to_string(),
                        part: node.to_string(),
                        bounds: b,
                    });
                } else if let (Some(x), Some(y)) = (num("x"), num("y")) {
                    self.render_exact(self.t)?;
                    self.select_at(x as f32, y as f32);
                } else {
                    self.selection = None;
                }
                Ok(json!({ "selection": self.selection }))
            }
            "compare" => {
                // A reference video beside or against the document.
                let video = text("video")
                    .map(PathBuf::from)
                    .or_else(|| self.compare.as_ref().map(|c| c.0.clone()));
                match text("mode").unwrap_or("split") {
                    "off" => self.set_compare(None),
                    "difference" => {
                        let v = video.ok_or("`compare` needs `video` the first time")?;
                        self.set_compare(Some((v, CompareMode::Difference)));
                    }
                    "split" => {
                        let v = video.ok_or("`compare` needs `video` the first time")?;
                        self.set_compare(Some((v, CompareMode::Split(num("split").unwrap_or(0.5) as f32))));
                    }
                    other => return Err(format!("unknown compare mode `{other}` (split, difference, off)")),
                }
                let t = num("t").unwrap_or(self.t);
                let psnr = if self.compare.is_some() {
                    Some(self.compare_psnr(t)?)
                } else {
                    None
                };
                Ok(json!({ "t": t, "psnr": psnr, "mode": self.compare.as_ref().map(|c| format!("{:?}", c.1)) }))
            }
            other => Err(format!("unknown cmd `{other}`")),
        }
    }

    /// Renders frame `t` exactly (waits for kernels and resources), as the
    /// export shows it: flattened over black where nothing is drawn.
    pub fn render_exact(&mut self, t: f64) -> Result<Vec<u8>, String> {
        self.render_exact_as(t, false)
    }

    /// Renders frame `t` exactly; with `alpha`, straight alpha (see-through
    /// where nothing is drawn) instead of flattened over black.
    pub fn render_exact_as(&mut self, t: f64, alpha: bool) -> Result<Vec<u8>, String> {
        self.renderer.set_mode(Mode::Exact);
        let r = if alpha {
            self.renderer.render_rgba8(&self.doc, t)
        } else {
            self.renderer.render_opaque_rgba8(&self.doc, t)
        }
        .map(|(px, _)| px)
        .map_err(|e| e.to_string());
        self.renderer.set_mode(Mode::Live);
        r
    }

    /// Renders frames at `times` into a labeled grid PNG (for reviewing
    /// motion at a glance).
    pub fn contact_sheet(&mut self, times: &[f64], cols: usize, path: &Path) -> Result<(u32, u32), String> {
        let (w, h, sheet) = contact_sheet(&mut self.renderer, &self.doc, times, cols)?;
        write_png(path, w, h, &sheet)?;
        Ok((w, h))
    }

    pub fn stats(&self) -> Value {
        let n = self.timings.len().max(1) as f64;
        let mut cpu: Vec<f64> = self.timings.iter().map(|t| t.0).collect();
        let mut gpu: Vec<f64> = self.timings.iter().map(|t| t.1).collect();
        cpu.sort_by(|a, b| a.partial_cmp(b).unwrap());
        gpu.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |v: &[f64], p: f64| {
            if v.is_empty() {
                0.0
            } else {
                v[((v.len() - 1) as f64 * p) as usize]
            }
        };
        let s = self.renderer.device().stats();
        let k = self.renderer.kernels().stats();
        let diags: Vec<Value> = self
            .renderer
            .diagnostics()
            .iter()
            .map(|d| json!({ "node": d.node, "message": d.message }))
            .collect();
        json!({
            "t": self.t,
            "playing": self.playing,
            "revision": self.doc.revision,
            "frames": self.frames,
            "uptime_s": self.started.elapsed().as_secs_f64(),
            "cpu_ms": { "mean": cpu.iter().sum::<f64>() / n + 0.0, "p95": pct(&cpu, 0.95), "max": pct(&cpu, 1.0) },
            "gpu_ms": { "mean": gpu.iter().sum::<f64>() / n + 0.0, "p95": pct(&gpu, 0.95), "max": pct(&gpu, 1.0) },
            "transfers": {
                "uploads": s.uploads, "upload_bytes": s.upload_bytes,
                "readbacks": s.readbacks, "readback_bytes": s.readback_bytes,
                "textures_created": s.textures_created,
            },
            "kernels": {
                "compiled": k.kernels, "pending": self.renderer.kernels().pending(),
                "slang_ms": k.slang_ms, "native_ms": k.native_ms,
            },
            "diagnostics": diags,
        })
    }
}

fn issues_json(doc: &Document, only: Option<&[String]>) -> Vec<Value> {
    ultrafast_render::schema::check(doc)
        .into_iter()
        .filter(|i| i.severity != "info" || only.is_none())
        .filter(|i| only.is_none_or(|ids| ids.contains(&i.node)))
        .map(|i| json!({ "node": i.node, "severity": i.severity, "message": i.message }))
        .collect()
}

/// The op vocabulary agents can use: names, image ports, and form.
/// Draws `text` (digits, '.', 's', '-') with a 3x5 pixel font scaled 3x, on
/// a dark box, into an RGBA8 image `w` pixels wide.
/// Renders exact frames of `doc` at `times` into a labeled grid (RGBA8,
/// `cols` thumbnails across, each at most 480 px wide): a whole film's
/// motion in one image. Leaves the renderer in the mode it was in.
pub fn contact_sheet(
    renderer: &mut Renderer,
    doc: &Document,
    times: &[f64],
    cols: usize,
) -> Result<(u32, u32, Vec<u8>), String> {
    let cols = cols.max(1);
    let mode = renderer.mode();
    renderer.set_mode(Mode::Exact);
    let r = (|| {
        let (fw, fh) = (doc.format.width as usize, doc.format.height as usize);
        let thumb_w = 480usize.min(fw);
        let thumb_h = (fh * thumb_w / fw).max(1);
        let rows = times.len().div_ceil(cols);
        let gap = 8;
        let (w, h) = (cols * thumb_w + (cols + 1) * gap, rows * thumb_h + (rows + 1) * gap);
        let mut sheet = vec![24u8; w * h * 4];
        for px in sheet.chunks_exact_mut(4) {
            px[3] = 255;
        }
        for (i, &t) in times.iter().enumerate() {
            // As the export shows it: black where the document draws nothing.
            let img = renderer
                .render_opaque_rgba8(doc, t)
                .map(|(px, _)| px)
                .map_err(|e| e.to_string())?;
            let (ox, oy) = (gap + (i % cols) * (thumb_w + gap), gap + (i / cols) * (thumb_h + gap));
            // Box-filter downscale.
            for ty in 0..thumb_h {
                let (y0, y1) = (ty * fh / thumb_h, ((ty + 1) * fh / thumb_h).max(ty * fh / thumb_h + 1));
                for tx in 0..thumb_w {
                    let (x0, x1) = (tx * fw / thumb_w, ((tx + 1) * fw / thumb_w).max(tx * fw / thumb_w + 1));
                    let mut acc = [0u32; 3];
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let p = &img[(y * fw + x) * 4..(y * fw + x) * 4 + 3];
                            for c in 0..3 {
                                acc[c] += p[c] as u32;
                            }
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)) as u32;
                    let d = ((oy + ty) * w + ox + tx) * 4;
                    for c in 0..3 {
                        sheet[d + c] = (acc[c] / n) as u8;
                    }
                    sheet[d + 3] = 255;
                }
            }
        }
        for (i, &t) in times.iter().enumerate() {
            let (ox, oy) = (gap + (i % cols) * (thumb_w + gap), gap + (i / cols) * (thumb_h + gap));
            draw_label(&mut sheet, w, ox + 6, oy + 6, &format!("{t:.2}s"));
        }
        Ok((w as u32, h as u32, sheet))
    })();
    renderer.set_mode(mode);
    r
}

fn draw_label(img: &mut [u8], w: usize, x: usize, y: usize, text: &str) {
    const GLYPHS: &[(char, [u8; 5])] = &[
        ('0', [7, 5, 5, 5, 7]),
        ('1', [2, 6, 2, 2, 7]),
        ('2', [7, 1, 7, 4, 7]),
        ('3', [7, 1, 7, 1, 7]),
        ('4', [5, 5, 7, 1, 1]),
        ('5', [7, 4, 7, 1, 7]),
        ('6', [7, 4, 7, 5, 7]),
        ('7', [7, 1, 2, 2, 2]),
        ('8', [7, 5, 7, 5, 7]),
        ('9', [7, 5, 7, 1, 7]),
        ('.', [0, 0, 0, 0, 2]),
        ('s', [0, 3, 6, 1, 6]),
        ('-', [0, 0, 7, 0, 0]),
    ];
    let k = 3;
    let h = img.len() / 4 / w;
    let (bw, bh) = (text.len() * 4 * k + 2 * k, 5 * k + 2 * k);
    let mut put = |px: usize, py: usize, c: [u8; 3]| {
        if px < w && py < h {
            let d = (py * w + px) * 4;
            img[d..d + 3].copy_from_slice(&c);
        }
    };
    for yy in 0..bh {
        for xx in 0..bw {
            put(x + xx, y + yy, [10, 10, 10]);
        }
    }
    for (ci, ch) in text.chars().enumerate() {
        let Some((_, rows)) = GLYPHS.iter().find(|g| g.0 == ch) else {
            continue;
        };
        for (ry, bits) in rows.iter().enumerate() {
            for rx in 0..3 {
                if bits & (4 >> rx) != 0 {
                    for sy in 0..k {
                        for sx in 0..k {
                            put(x + k + (ci * 4 + rx) * k + sx, y + k + ry * k + sy, [255, 230, 90]);
                        }
                    }
                }
            }
        }
    }
}

pub fn ops_vocabulary() -> Vec<Value> {
    let mut seen = std::collections::BTreeMap::new();
    for d in ops::OPS {
        seen.entry(d.name).or_insert_with(
            || json!({ "op": d.name, "ports": d.ports, "form": format!("{:?}", d.form).to_lowercase() }),
        );
    }
    seen.into_values().collect()
}

pub fn read_doc(path: &Path) -> Result<Document, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Document::from_json(&s).map_err(|e| format!("{}: {e}", path.display()))
}

/// RGBA8 pixels as a PNG takes them: RGB when nothing is see-through (an
/// opaque frame carries no alpha channel a viewer could act on), RGBA
/// otherwise.
pub fn png_pixels(rgba: &[u8]) -> (png::ColorType, std::borrow::Cow<'_, [u8]>) {
    if rgba.chunks_exact(4).all(|p| p[3] == 255) {
        let rgb = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        (png::ColorType::Rgb, std::borrow::Cow::Owned(rgb))
    } else {
        (png::ColorType::Rgba, std::borrow::Cow::Borrowed(rgba))
    }
}

pub fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    let (color, px) = png_pixels(rgba);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&px))
        .map_err(|e| e.to_string())
}

/// Serves requests without a window until the channel closes. Frames are
/// rendered only on demand (snapshots, contact sheets).
pub fn run_headless(mut session: Session, requests: std::sync::mpsc::Receiver<server::Request>) {
    loop {
        match requests.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(req) => {
                let resp = session.handle(&req.body);
                let _ = req.reply.send(resp);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if session.quit {
            return;
        }
        session.advance();
        session.poll_file();
    }
}
