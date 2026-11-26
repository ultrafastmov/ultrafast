//! ultrafast renderer: compiles documents into fused GPU plans and executes them.
//!
//! Per frame the renderer: (1) checks the document's structure signature and
//! recompiles the plan only if it changed, (2) packs params for every group
//! into the frame's param arena, (3) skips static groups whose inputs and
//! params are unchanged, (4) dispatches one kernel per group, falling back to
//! tier-0 kernels for groups whose fused kernel is still compiling.
//!
//! Every texture a kernel reads is *placed*: a texture plus the frame
//! position of its top-left texel. Frame-sized intermediates sit at the
//! origin; rasterized paths and text get textures sized to their bounds, so
//! their cost scales with what they cover, not with the frame.

// Engine code: wide kernel and pass signatures and indexed loops over
// parallel arrays are the clearest form here.
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::should_implement_trait
)]

pub mod autofaces;
pub mod blocks;
pub mod captions;
mod charts;
pub mod code;
pub mod codegen;
mod glyph_atlas;
pub mod images;
pub mod kernels;
pub mod layout;
pub mod lottie;
pub mod particles;
pub mod plan;
pub mod pushdown;
pub mod raymarch;
pub mod review;
pub mod scene3d;
mod sim;
mod specialize;
pub mod svg;
pub mod swarm;
pub mod tables;
pub mod transitions;

// Text, vector geometry, the face camera and the op vocabulary live in CPU
// crates (tools use them without a GPU), re-exported at
// their old paths.
pub use ultrafast_framing::follow;
pub use ultrafast_geom::{fontfetch, geom, lru, markup};
pub use ultrafast_ops::{builtins, ops, schema};

use images::ImageCache;
use kernels::{KernelState, Kernels};
use ops::ResourceInfo;
use plan::{GroupKind, Plan, TexSource};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use ultrafast_gpu::{
    Bindings, Buffer, BufferBinding, CommandList, Device, GpuError, PixelFormat, Submitted, Texture, TextureDesc,
};
use ultrafast_graph::{Document, DocumentError, NodeId};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(transparent)]
    Gpu(#[from] GpuError),
    #[error(transparent)]
    Document(#[from] DocumentError),
    #[error("node `{node}` has unknown op `{op}`")]
    UnknownOp { node: NodeId, op: String },
    #[error("node `{node}` is missing required input `{port}`")]
    MissingPort { node: NodeId, port: String },
    #[error("node `{node}` param `{param}`: {reason}")]
    BadParam {
        node: NodeId,
        param: String,
        reason: String,
    },
    #[error("node `{node}` needs {count} texture inputs in one kernel (max 16)")]
    TooManyInputs { node: NodeId, count: usize },
    #[error("kernel setup failed: {0}")]
    Setup(String),
}

/// Geometry errors are the render errors of the same name.
impl From<ultrafast_geom::GeomError> for RenderError {
    fn from(e: ultrafast_geom::GeomError) -> Self {
        match e {
            ultrafast_geom::GeomError::BadParam { node, param, reason } => {
                RenderError::BadParam { node, param, reason }
            }
        }
    }
}

/// Op errors are the render errors of the same name.
impl From<ultrafast_ops::OpsError> for RenderError {
    fn from(e: ultrafast_ops::OpsError) -> Self {
        match e {
            ultrafast_ops::OpsError::UnknownOp { node, op } => RenderError::UnknownOp { node, op },
            ultrafast_ops::OpsError::MissingPort { node, port } => RenderError::MissingPort { node, port },
            ultrafast_ops::OpsError::BadParam { node, param, reason } => RenderError::BadParam { node, param, reason },
        }
    }
}

/// How [`Renderer::render_readback`] hands a frame back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readback {
    /// sRGB RGBA8.
    Srgb8,
    /// sRGB-encoded RGBA half floats, for 10-bit codecs.
    SrgbHalf,
    /// Linear RGBA half floats (EXR); straight alpha, as encoders that
    /// premultiply take it.
    LinearHalf,
}

/// How the renderer treats kernels that are still compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Never stall: use tier-0 fallbacks and placeholders while compiling or
    /// loading. For live preview.
    Live,
    /// Wait for every fused kernel and resource. Deterministic; for export.
    Exact,
}

/// What one frame cost and how it was produced.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameReport {
    pub replanned: bool,
    pub kernels_run: usize,
    pub groups_cached: usize,
    /// Groups skipped because nothing they feed is visible at this time.
    pub groups_skipped: usize,
    /// Groups rendered with tier-0 kernels because their fused kernel was
    /// still compiling.
    pub groups_fallback: usize,
    /// Simulation steps taken, and frames rendered only to bring
    /// simulations up to this one (after a seek).
    pub sim_steps: usize,
    pub sim_catchup_frames: usize,
    /// Motion blur: samples taken this frame (1 when nothing moved), the
    /// group up to which they were taken, and how many groups up to it read
    /// nothing moving and ran once instead.
    pub mb_samples: u32,
    pub mb_split: usize,
    pub mb_once: usize,
    /// Images still decoding and videos whose frame for this time was not
    /// decoded yet (live mode draws what it has): a later frame at the same
    /// time will differ, so a paused player draws again.
    pub media_pending: usize,
    pub cpu_encode_ms: f64,
    pub plan_ms: f64,
}

/// A problem tied to a node that does not stop rendering (bad shader code,
/// missing image). Agents read these to fix their documents.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub node: NodeId,
    pub message: String,
}

impl Diagnostic {
    /// "warning" for parts of an imported file ultrafast leaves out and for
    /// text that renders but not all of it shows, "error" for problems that
    /// stop a node from rendering.
    pub fn severity(&self) -> &'static str {
        if ["lottie: ", "svg: ", "text: "]
            .iter()
            .any(|p| self.message.starts_with(p))
        {
            "warning"
        } else {
            "error"
        }
    }
}

/// Why a text's keyed `reveal` never shows all of it, if it doesn't: its
/// keys stop below the glyphs it has (a typewriter keyed for a shorter
/// text, then edited or filled from a CSV row with a longer one).
fn reveal_falls_short(node: &ultrafast_graph::Node, units: usize) -> Option<String> {
    let Some(ultrafast_graph::Param::Animated { keys, .. }) = node.params.get("reveal") else {
        return None;
    };
    let most = keys
        .iter()
        .filter_map(|k| k.v.first().copied())
        .fold(f64::NEG_INFINITY, f64::max);
    let softness = match node.params.get("reveal_softness") {
        Some(ultrafast_graph::Param::Number(s)) => s.max(1e-3),
        _ => 1.0,
    };
    // Glyph i shows fully once the reveal reaches i + softness.
    let needed = units as f64 - 1.0 + softness;
    if units == 0 || !most.is_finite() || most >= needed - 1e-6 {
        return None;
    }
    let shown = ((most - softness + 1.0).max(0.0).floor() as usize).min(units);
    let fmt = |v: f64| format!("{}", (v * 100.0).round() / 100.0);
    Some(format!(
        "text: its `reveal` keys stop at {}, so the last {} of its {units} glyphs never show in full: key it up to {}",
        fmt(most),
        units - shown,
        fmt(needed)
    ))
}

/// The final image of a frame, handed to [`Renderer::submit_frame_with`]
/// callbacks so they can consume it in the same command buffer.
pub struct FrameOutput<'a> {
    pub list: &'a mut CommandList,
    pub kernels: &'a Kernels,
    pub texture: &'a Texture,
    /// Frame position of the texture's top-left texel.
    pub origin: [f32; 2],
    /// Frame header (time, frame, width, height) for kernel params.
    pub header: [f32; 4],
}

/// How a frame maps onto the textures it renders into: `px` frame pixels
/// per device texel (1 at the document's own size, 0.5 for twice its
/// size) and the device size. Everything in a document stays in frame
/// pixels; only texture sizes and texel steps follow the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct View {
    pub px: f32,
    pub w: u32,
    pub h: u32,
}

impl View {
    /// The document at its own size.
    pub fn native(doc: &Document) -> View {
        View {
            px: 1.0,
            w: doc.format.width,
            h: doc.format.height,
        }
    }

    /// The document drawn at `w` x `h`: one texel step for both axes, the
    /// finer of the two, so every texel is inside the frame.
    pub fn sized(doc: &Document, size: Option<(u32, u32)>) -> View {
        let (fw, fh) = (doc.format.width.max(1), doc.format.height.max(1));
        match size {
            Some((w, h)) if (w, h) != (fw, fh) && w > 0 && h > 0 => View {
                px: 1.0 / (w as f32 / fw as f32).max(h as f32 / fh as f32),
                w,
                h,
            },
            _ => View::native(doc),
        }
    }
}

/// A texture positioned in the frame.
#[derive(Clone)]
struct Placed {
    tex: Texture,
    origin: [f32; 2],
    /// Frame px per texel: 1, or more for a blur kept at the reduced
    /// resolution it was computed at (the kernels reading it sample it).
    scale: f32,
}

struct TexturePool {
    free: HashMap<TextureDesc, Vec<Texture>>,
    max_free_per_desc: usize,
    /// Bytes held in free lists, and the most they may hold: raster
    /// textures come in many sizes, so a per-size count alone is unbounded.
    free_bytes: u64,
    max_free_bytes: u64,
    /// When each size was last leased or given back: a full pool lets go
    /// of the sizes nothing has asked for longest (type that grew past a
    /// size, last week's shot) to keep the ones every frame uses.
    clock: u64,
    last_used: HashMap<TextureDesc, u64>,
}

fn tex_bytes(d: TextureDesc) -> u64 {
    d.width as u64 * d.height as u64 * d.format.bytes_per_pixel() as u64
}

impl TexturePool {
    fn lease(&mut self, device: &Device, desc: TextureDesc) -> Texture {
        self.clock += 1;
        self.last_used.insert(desc, self.clock);
        match self.free.get_mut(&desc).and_then(Vec::pop) {
            Some(t) => {
                self.free_bytes -= tex_bytes(desc);
                t
            }
            None => device.create_texture(desc),
        }
    }

    fn give_back(&mut self, tex: Texture) {
        let desc = tex.desc();
        let bytes = tex_bytes(desc);
        if bytes > self.max_free_bytes || self.free.get(&desc).is_some_and(|l| l.len() >= self.max_free_per_desc) {
            return;
        }
        self.clock += 1;
        self.last_used.insert(desc, self.clock);
        // Make room by dropping the sizes asked for least recently.
        while self.free_bytes + bytes > self.max_free_bytes {
            let Some(old) = self
                .free
                .iter()
                .filter(|(d, l)| !l.is_empty() && **d != desc)
                .min_by_key(|(d, _)| self.last_used.get(*d).copied().unwrap_or(0))
                .map(|(d, _)| *d)
            else {
                return;
            };
            let list = self.free.get_mut(&old).unwrap();
            while self.free_bytes + bytes > self.max_free_bytes && list.pop().is_some() {
                self.free_bytes -= tex_bytes(old);
            }
            if list.is_empty() {
                self.free.remove(&old);
                self.last_used.remove(&old);
            }
        }
        self.free.entry(desc).or_default().push(tex);
        self.free_bytes += bytes;
    }
}

type StaticKey = (Vec<f32>, Vec<usize>);

#[derive(Default)]
struct Profiler {
    pending: Vec<(String, Submitted)>,
    totals: HashMap<String, (f64, u32)>,
}

/// An element under a point of the frame ([`Renderer::pick`]).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Picked {
    /// The document node it belongs to (edit this one).
    pub node: String,
    pub op: String,
    /// The node that drew it (inside a component or block: `node/...`).
    pub part: String,
    /// Frame px [x0, y0, x1, y1].
    pub bounds: [f32; 4],
}

/// An element on screen ([`Renderer::visible`]).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Visible {
    #[serde(flatten)]
    pub element: Picked,
    /// What it reads, for text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Where a document's GPU time goes ([`Renderer::profile_frames`]).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProfileReport {
    pub frames: u64,
    pub from: f64,
    pub gpu_ms_per_frame: f64,
    /// Most expensive first.
    pub groups: Vec<GroupShare>,
    /// Work outside groups: motion-blur averaging, output conversion.
    pub rest_ms_per_frame: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GroupShare {
    /// The group's kind and the nodes it renders.
    pub group: String,
    pub ms_per_frame: f64,
    pub share: f64,
    /// Motion-blur samples run groups several times a frame.
    pub runs_per_frame: f64,
}

/// What one render group cost on the GPU while profiling.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GroupCost {
    /// The group's kind and the nodes it renders.
    pub group: String,
    /// Total GPU milliseconds.
    pub ms: f64,
    /// Times it ran (motion blur samples run groups several times a frame).
    pub runs: u32,
}

struct Active {
    plan: Plan,
    /// Frame-sized group outputs. A slot gets a texture the first time a
    /// live group needs it and gives it back when unused slots outgrow the
    /// memory budget (long films plan thousands of slots; a frame touches
    /// a few). Non-resident slots hold the placeholder.
    slots: Vec<Texture>,
    resident: Vec<bool>,
    /// Frame each slot was last needed in.
    slot_used: Vec<u64>,
    /// Groups writing each slot (their caches die with its texture).
    slot_groups: Vec<Vec<usize>>,
    slot_desc: TextureDesc,
    /// The view the slots are sized for.
    view: View,
    /// Frame each group was last live in (cold raster textures are given
    /// back under the same budget).
    group_used: Vec<u64>,
    /// For static groups: params and input identities of the last run.
    cache: Vec<Option<StaticKey>>,
    /// Bounds-sized output textures of raster groups.
    raster: Vec<Option<Placed>>,
    /// What each raster group last drew (geometry and paint), so geometry
    /// that holds still is not drawn again.
    drawn: Vec<u64>,
    /// Fused groups that only pass an earlier group's output through at
    /// the current time (inactive layers, empty foregrounds) show it
    /// directly instead of copying it.
    alias: Vec<Option<usize>>,
    /// Groups whose `raster` entry is another group's texture shown as it is
    /// (an inactive blur passing its source through): not theirs to reuse
    /// or return to the pool.
    borrowed: Vec<bool>,
    /// Where each group's output can be non-transparent (frame px), as the
    /// last pass that ran it found.
    bounds: Vec<ops::Rect>,
    /// Per blur group: the fused groups reading it, when every reader can
    /// sample it at the reduced resolution it is computed at (fused groups
    /// outside their loops, and blurs); None when it must be full size.
    low_readers: Vec<Option<Vec<usize>>>,
}

/// A motion-blurred frame's schedule, for the passes that run it.
struct MbFrame {
    /// Per group: read by a group of a later phase, so what it makes must
    /// last until then (no showing a slot another group may write first).
    crosses: Vec<bool>,
    /// The shutter's span (seconds): particles drawn once blur themselves
    /// along their paths over it.
    span: f64,
}

/// One sampled group's average across a frame's samples.
struct MbAccum {
    /// Running sum (newest, spare) of frame-sized outputs.
    sum: Option<(Texture, Texture)>,
    /// Copies of bounds-sized outputs, summed over their union at the end.
    stash: Vec<Placed>,
    /// Union of the samples' content bounds.
    bounds: ops::Rect,
}

impl Default for MbAccum {
    fn default() -> Self {
        MbAccum {
            sum: None,
            stash: Vec::new(),
            bounds: ops::EMPTY,
        }
    }
}

/// Which groups a pass over the plan runs, and what it returns.
#[derive(Default, Clone, Copy)]
struct Pass<'a> {
    /// Only these groups (default: every group that reaches the output now).
    only: Option<&'a [bool]>,
    /// The group whose output the pass returns (default: the last).
    target: Option<usize>,
    /// Groups whose outputs were replaced outside the pass.
    changed: &'a [usize],
    /// Keep the bounds earlier passes this frame found for the groups this
    /// one does not run (motion blur's samples read what its once-a-frame
    /// pass made).
    keep_bounds: bool,
}

pub struct Renderer {
    device: Device,
    kernels: Kernels,
    active: Option<Active>,
    seen_revision: Option<(u64, u64)>,
    pool: TexturePool,
    images: ImageCache,
    placeholder: Texture,
    export: Option<Texture>,
    mode: Mode,
    diagnostics: Vec<Diagnostic>,
    scratch: Vec<f32>,
    ran: Vec<bool>,
    /// Per-group GPU timing: each group submitted on its own (see
    /// [`Renderer::set_profiling`]).
    profile: Option<Profiler>,
    /// ULTRAFAST_NO_SKIP: draw every live group every time (a reference for
    /// checking that skipped work was really redundant).
    no_skip: bool,
    /// ULTRAFAST_NO_FRAMED: rasterize off-frame content even when nothing reads
    /// it (a reference for checking [`plan::Plan::framed`]).
    no_framed: bool,
    /// Exact renders wait for a group's fused kernel (default); without,
    /// groups whose ops all have tier-0 kernels render op by op while it
    /// compiles (a new look starts at once; see `set_wait_for_fused`).
    wait_fused: bool,
    fonts: geom::Fonts,
    base_dir: PathBuf,
    /// Built geometry per node: full key, content key (shape without
    /// placement), the transform it was built under, the geometry.
    geometry: HashMap<NodeId, (u64, u64, ops::Affine, Arc<geom::Geometry>)>,
    /// The geometry each node last had built from scratch (key, content
    /// key, placement), which turns and scales map instead of rebuilding.
    built: HashMap<NodeId, (u64, u64, ops::Affine, Arc<geom::Geometry>)>,
    bounds: HashMap<NodeId, [f32; 4]>,
    acc: Option<Buffer>,
    videos: HashMap<NodeId, VideoState>,
    flat: Option<((u64, u64), Arc<ultrafast_graph::Flat>)>,
    /// Loudness envelopes of soundtracks driving params (120 per second,
    /// normalized), by file, band and release.
    envelopes: HashMap<String, Option<Arc<Vec<f32>>>>,
    /// Where swarm particles settle, by the shape they form.
    swarm_homes: HashMap<u64, Arc<Vec<[f32; 2]>>>,
    three: Option<Three>,
    /// Ping-pong accumulation targets for motion blur.
    accum: Option<(Texture, Texture)>,
    /// Color glyph (emoji) bitmaps, created on first use.
    glyph_atlas: Option<glyph_atlas::GlyphAtlas>,
    diff_buf: Option<Buffer>,
    /// Particle pipelines (over, add), the sequential index buffer, and a
    /// white 1x1 fallback sprite.
    sprites: Option<(
        ultrafast_gpu::DrawPipeline,
        ultrafast_gpu::DrawPipeline,
        Buffer,
        Texture,
    )>,
    /// Decoded video frames referenced by in-flight GPU work, by frame number.
    retained: std::collections::VecDeque<(u64, Arc<ultrafast_media::VideoFrame>)>,
    /// Seconds spent waiting for decoded video frames, and for images an
    /// exact render needs (decode and upload).
    media_wait: f64,
    /// Media not ready for the frame being encoded (see FrameReport::media_pending).
    media_pending: usize,
    image_wait: f64,
    frame_no: u64,
    /// Simulation state by node.
    sims: HashMap<NodeId, sim::SimState>,
    /// Built blocks by their params and frame size.
    blocks: HashMap<String, Arc<ultrafast_graph::Component>>,
    /// Built caption tracks by their params and transcript file time.
    captions: HashMap<String, Arc<ultrafast_graph::Component>>,
    /// Built face-follow cameras by their params, face file time and format.
    follows: HashMap<String, Arc<ultrafast_graph::Component>>,
    /// Imported SVG files by (src, size), with the file time they were read at.
    svgs: HashMap<String, (Option<std::time::SystemTime>, Arc<svg::Imported>)>,
    lotties: HashMap<String, (Option<std::time::SystemTime>, Arc<lottie::Imported>)>,
    /// The frame being rendered (motion-blur samples share it).
    frame_t: f64,
    /// While rendering motion-blur samples: (sample, samples, frame time,
    /// shutter seconds).
    mb_sample: Option<(u32, u32, f64, f64)>,
    /// Sample everything every frame (reference renders; ULTRAFAST_MB_FULL=1).
    mb_full: bool,
    /// The motion-blurred frame being encoded, its sampled groups' averages,
    /// and textures it used that go back to the pool next frame.
    mb_frame: Option<MbFrame>,
    mb_acc: Vec<Option<MbAccum>>,
    mb_release: Vec<Texture>,
    /// Motion blur turned off (a player while it plays: one sample a frame).
    mb_off: bool,
    /// Bytes of resident slot textures before unused ones are given back.
    slot_budget: u64,
    /// Frames are rendered once each, in order (an export): no seeking
    /// back, so simulations keep no checkpoints.
    pub(crate) sequential: bool,
    /// The output size asked for (None: each document's own).
    out_size: Option<(u32, u32)>,
    /// This frame's view.
    pub(crate) view: View,
}

/// GPU state of the 3D pass, created on first use.
struct Three {
    pipeline: ultrafast_gpu::DrawPipeline,
    shadow_pipeline: ultrafast_gpu::DrawPipeline,
    /// Light-space depth (R32F) and its depth attachment.
    shadow_map: Texture,
    shadow_depth: Texture,
    /// Multisampled color and depth attachments, at frame size.
    attachments: Option<(Texture, Texture)>,
    /// Depth of field: camera-distance pass, its target (R32F) and depth
    /// attachment, and the sharp image it blurs (all at frame size).
    dist_pipeline: ultrafast_gpu::DrawPipeline,
    dof: Option<(Texture, Texture, Texture)>,
    white: Texture,
    /// Vertex buffer, index buffer, index count, local bounds (min, max).
    meshes: HashMap<String, (Buffer, Buffer, u32, [[f32; 3]; 2])>,
}

#[derive(Default)]
struct VideoState {
    path: String,
    /// The file as it was when it was opened.
    stamp: Option<images::FileStamp>,
    source: Option<ultrafast_media::VideoSource>,
    error: Option<String>,
    prepared: Option<Texture>,
    prepared_pts: Option<f64>,
}

/// Raster output textures are allocated in steps of this many pixels so
/// small changes in bounds reuse the same texture.
const RASTER_BUCKET: u32 = 64;
/// Largest raster region side, in pixels.
const MAX_RASTER: i64 = 8192;
/// Frames of an image sequence decoded ahead of the playhead.
const SEQUENCE_AHEAD: usize = 12;

/// Where a document's media reference points: URLs (`https://...`, decoded
/// by ffmpeg straight from the network) and absolute paths as they are,
/// relative paths under `base`.
pub fn media_path(base: &Path, src: &str) -> PathBuf {
    if src.contains("://") || Path::new(src).is_absolute() {
        PathBuf::from(src)
    } else {
        base.join(src)
    }
}

impl Renderer {
    pub fn new(device: Device) -> Result<Self, RenderError> {
        let kernels = Kernels::new(device.clone()).map_err(RenderError::Setup)?;
        Self::with_kernels(device, kernels)
    }

    /// A renderer whose ops' tier-0 kernels compile only when a frame first
    /// needs one ([`Kernels::on_demand`]), for a command that renders and
    /// exits (an export, a frame, a check): its exact renders wait for
    /// their fused kernels anyway. On the first run after an install or an
    /// upgrade it starts about 0.9 s sooner. [`Renderer::new`] builds them
    /// all first, so a live session never waits on the compiler.
    pub fn new_on_demand(device: Device) -> Result<Self, RenderError> {
        let kernels = Kernels::on_demand(device.clone()).map_err(RenderError::Setup)?;
        Self::with_kernels(device, kernels)
    }

    fn with_kernels(device: Device, kernels: Kernels) -> Result<Self, RenderError> {
        let placeholder = device.create_texture(TextureDesc {
            width: 1,
            height: 1,
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        });
        let r = Self {
            device,
            kernels,
            active: None,
            seen_revision: None,
            pool: TexturePool {
                free: HashMap::new(),
                max_free_per_desc: 128,
                free_bytes: 0,
                max_free_bytes: 1 << 30,
                clock: 0,
                last_used: HashMap::new(),
            },
            images: ImageCache::new(),
            placeholder,
            export: None,
            mode: Mode::Exact,
            diagnostics: Vec::new(),
            scratch: Vec::with_capacity(256),
            ran: Vec::new(),
            profile: None,
            no_skip: std::env::var_os("ULTRAFAST_NO_SKIP").is_some(),
            no_framed: std::env::var_os("ULTRAFAST_NO_FRAMED").is_some(),
            wait_fused: true,
            fonts: geom::Fonts::new(),
            base_dir: PathBuf::from("."),
            geometry: HashMap::new(),
            built: HashMap::new(),
            bounds: HashMap::new(),
            acc: None,
            videos: HashMap::new(),
            flat: None,
            envelopes: HashMap::new(),
            swarm_homes: HashMap::new(),
            three: None,
            accum: None,
            glyph_atlas: None,
            diff_buf: None,
            sprites: None,
            retained: std::collections::VecDeque::new(),
            media_wait: 0.0,
            media_pending: 0,
            image_wait: 0.0,
            frame_no: 0,
            sims: HashMap::new(),
            svgs: HashMap::new(),
            lotties: HashMap::new(),
            captions: HashMap::new(),
            follows: HashMap::new(),
            blocks: HashMap::new(),
            frame_t: 0.0,
            mb_sample: None,
            mb_full: std::env::var("ULTRAFAST_MB_FULL").is_ok(),
            mb_frame: None,
            mb_acc: Vec::new(),
            mb_release: Vec::new(),
            mb_off: false,
            slot_budget: 0,
            sequential: false,
            out_size: None,
            view: View { px: 1.0, w: 1, h: 1 },
        };
        // Slots may take a third of GPU memory, shared by ULTRAFAST_WORKERS
        // processes (ULTRAFAST_SLOT_BUDGET_MB overrides).
        let mut r = r;
        r.slot_budget = std::env::var("ULTRAFAST_SLOT_BUDGET_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|mb| mb << 20)
            .unwrap_or_else(|| {
                // Processes sharing the GPU (parallel export) split it.
                let workers = std::env::var("ULTRAFAST_WORKERS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1)
                    .max(1);
                r.device.memory_bytes() / 3 / workers
            });
        // New textures are uninitialized; clear the placeholder once.
        let mut list = r.device.begin();
        list.dispatch(
            &r.kernels.utility("k_clear"),
            &r.placeholder,
            &[],
            &[0.0, 0.0, 1.0, 1.0],
        );
        list.submit().wait()?;
        let device = r.device.clone();
        r.images.set_device(&device);
        Ok(r)
    }

    /// Bytes resident group slots and kept raster textures may use before
    /// cold ones are given back.
    pub fn set_slot_budget(&mut self, bytes: u64) {
        self.slot_budget = bytes;
        self.pool.max_free_bytes = (bytes / 8).min(1 << 30);
    }

    /// Declares that frames will be rendered once each, in order (an
    /// export), so state kept only for seeking back can be skipped.
    pub fn set_sequential(&mut self, on: bool) {
        self.sequential = on;
    }

    /// Lets go of everything held for the current document (plan, group
    /// and raster textures, decoders, images, simulations, built parts)
    /// and keeps what any document reuses: compiled kernels, pipelines and
    /// fonts. A warm renderer serving one document after another calls this
    /// between them, so GPU memory returns to its baseline instead of
    /// collecting every earlier document's resources. Call with no frames
    /// in flight.
    /// Pins font files to their family, weight and style for what renders
    /// next, until [`Renderer::unpin_fonts`] (see
    /// [`geom::Fonts::pin`]): a document laid out elsewhere with those
    /// files sets its text with them here.
    pub fn pin_fonts(&mut self, pins: &[geom::FontPin]) -> Result<(), String> {
        self.release_document();
        self.fonts.pin(pins)
    }

    /// How what renders next sets the writing systems its fonts do not
    /// cover, until [`Renderer::unpin_fonts`] (see
    /// [`geom::Fonts::set_script_fonts`]).
    pub fn set_script_fonts(&mut self, rows: Vec<geom::ScriptFont>) {
        self.release_document();
        self.fonts.set_script_fonts(rows);
    }

    /// Drops the pins of [`Renderer::pin_fonts`].
    pub fn unpin_fonts(&mut self) {
        self.release_document();
        self.fonts.unpin();
    }

    /// Drops the decoded images and video decoders whose files changed on
    /// disk since they were read (a look re-rendered into the same file, a
    /// sheet rebuilt), and the cached results made from them, so the next
    /// frame shows the files as they are now. Returns how many it dropped;
    /// costs a stat per image and video held. A player calls it when a
    /// document is (re)loaded and every so often while it is open.
    pub fn refresh_changed_media(&mut self) -> usize {
        let mut n = self.images.drop_changed();
        let base = self.base_dir.clone();
        self.videos.retain(|_, v| {
            if v.path.is_empty() || v.path.contains("://") {
                return true;
            }
            let same = images::file_stamp(&media_path(&base, &v.path)) == v.stamp;
            n += (!same) as usize;
            same
        });
        if n > 0
            && let Some(a) = self.active.as_mut()
        {
            for c in a.cache.iter_mut() {
                *c = None;
            }
        }
        n
    }

    pub fn release_document(&mut self) {
        self.active = None;
        self.seen_revision = None;
        self.pool.free.clear();
        self.pool.last_used.clear();
        self.pool.free_bytes = 0;
        let base = self.base_dir.clone();
        self.images = ImageCache::new();
        self.images.set_base_dir(&base);
        self.images.set_device(&self.device);
        self.export = None;
        self.geometry.clear();
        self.built.clear();
        self.bounds.clear();
        self.acc = None;
        self.videos.clear();
        self.flat = None;
        self.envelopes.clear();
        self.swarm_homes.clear();
        if let Some(three) = self.three.as_mut() {
            three.attachments = None;
            three.dof = None;
            three.meshes.clear();
        }
        self.accum = None;
        self.glyph_atlas = None;
        self.diff_buf = None;
        self.retained.clear();
        self.sims.clear();
        self.blocks.clear();
        self.captions.clear();
        self.follows.clear();
        self.svgs.clear();
        self.lotties.clear();
        self.diagnostics.clear();
    }

    /// Seconds this renderer has spent waiting for decoded media and
    /// uploading decoded images (part of the CPU time of the frames).
    pub fn media_wait(&self) -> f64 {
        self.media_wait + self.image_wait
    }

    /// The part of [`media_wait`](Self::media_wait) spent on images.
    pub fn image_wait(&self) -> f64 {
        self.image_wait
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Profiling: every group is submitted to the GPU on its own and timed,
    /// which is slower but says what each part of a film costs. Read the
    /// totals with [`Renderer::profile`].
    pub fn set_profiling(&mut self, on: bool) {
        self.profile = on.then(Profiler::default);
    }

    /// GPU time per group so far (waits for groups still running), most
    /// expensive first.
    pub fn profile(&mut self) -> Result<Vec<GroupCost>, RenderError> {
        let Some(p) = &mut self.profile else {
            return Ok(Vec::new());
        };
        for (label, sub) in p.pending.drain(..) {
            let ms = sub.wait()?.gpu_ms;
            let e = p.totals.entry(label).or_insert((0.0, 0));
            e.0 += ms;
            e.1 += 1;
        }
        let mut out: Vec<GroupCost> = p
            .totals
            .iter()
            .map(|(k, &(ms, runs))| GroupCost {
                group: k.clone(),
                ms,
                runs,
            })
            .collect();
        out.sort_by(|a, b| b.ms.total_cmp(&a.ms));
        Ok(out)
    }

    /// Whether to skip work that cannot change the frame (redrawing still
    /// geometry, copying layers that only pass through). On by default;
    /// off (or ULTRAFAST_NO_SKIP) renders a reference to check it against.
    pub fn set_skip_redundant(&mut self, on: bool) {
        self.no_skip = !on;
    }

    /// Renders `frames` frames from `from` seconds with every group timed
    /// on its own: where the GPU time of `doc` goes, per frame.
    pub fn profile_frames(&mut self, doc: &Document, frames: u64, from: f64) -> Result<ProfileReport, RenderError> {
        // Compile the plan's kernels first so they are not timed.
        self.submit_frame(doc, from, false)?.0.wait()?;
        self.set_profiling(true);
        let first = (from * doc.format.fps).round() as u64;
        let n = doc.format.frame_count().max(1);
        let frames = frames.max(1);
        let mut rest = 0.0;
        let result = (|| {
            for i in 0..frames {
                let (sub, _) = self.submit_frame(doc, doc.format.time_of((first + i) % n), false)?;
                rest += sub.wait()?.gpu_ms;
                if i % 16 == 15 {
                    self.profile()?;
                }
            }
            self.profile()
        })();
        self.set_profiling(false);
        let costs = result?;
        let f = frames as f64;
        let total = costs.iter().map(|c| c.ms).sum::<f64>() + rest;
        Ok(ProfileReport {
            frames,
            from,
            gpu_ms_per_frame: total / f,
            groups: costs
                .into_iter()
                .map(|c| GroupShare {
                    share: c.ms / total.max(1e-9),
                    ms_per_frame: c.ms / f,
                    runs_per_frame: c.runs as f64 / f,
                    group: c.group,
                })
                .collect(),
            rest_ms_per_frame: rest / f,
        })
    }

    /// Motion blur samples every group on every frame instead of only what
    /// moves (reference renders).
    pub fn set_full_motion_blur(&mut self, full: bool) {
        self.mb_full = full;
    }

    /// Turns the document's motion blur on or off for the frames that
    /// follow. A player turns it off while it plays (one render a frame
    /// keeps playback smooth) and back on when it stops, so a still frame
    /// shows exactly what the export will.
    pub fn set_motion_blur(&mut self, on: bool) {
        self.mb_off = !on;
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Whether exact renders wait for fused kernels still compiling. Off,
    /// such a group renders op by op with the tier-0 kernels (built at
    /// startup) until its fused kernel is ready: the same image to within
    /// half-float rounding of the intermediates, with no compile stall when
    /// a document's structure is new (a new look on a render service).
    /// Groups with an op that has no tier-0 kernel (custom shaders) still
    /// wait.
    pub fn set_wait_for_fused(&mut self, on: bool) {
        self.wait_fused = on;
    }

    pub fn waits_for_fused(&self) -> bool {
        self.wait_fused
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Directory relative resource paths resolve against.
    pub fn set_base_dir(&mut self, dir: &Path) {
        self.base_dir = dir.to_path_buf();
        self.images.set_base_dir(dir);
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn plan(&self) -> Option<&Plan> {
        self.active.as_ref().map(|a| &a.plan)
    }

    pub fn kernels(&self) -> &Kernels {
        &self.kernels
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
    }

    fn diagnose(&mut self, node: &str, message: String) {
        let d = Diagnostic {
            node: node.to_string(),
            message,
        };
        if !self.diagnostics.contains(&d) {
            self.diagnostics.push(d);
        }
    }

    /// Bounds (min x, min y, max x, max y) in output pixels of a path or text
    /// node, as of the last frame it was rasterized.
    /// Outlines of a text node as SVG path data (pixels, placed), for
    /// morphing letters or reusing type as vector shapes.
    pub fn text_outline(&mut self, node: &ultrafast_graph::Node) -> Result<String, RenderError> {
        let fmt = ultrafast_graph::Format {
            width: 1,
            height: 1,
            fps: 1.0,
            duration: 1.0,
            motion_blur: None,
        };
        let mut all = kurbo::BezPath::new();
        for (p, _, _) in geom::text_paths(
            "text",
            node,
            &fmt,
            0.0,
            ops::Affine::IDENTITY,
            &mut self.fonts,
            &self.base_dir,
        )?
        .0
        {
            all.extend(p);
        }
        Ok(all.to_svg())
    }

    /// Glyph-by-glyph metrics of a single-line text node (see
    /// [`geom::measure`]), with this renderer's fonts and base directory.
    pub fn measure_text(&mut self, node: &ultrafast_graph::Node) -> Result<geom::LineMetrics, RenderError> {
        Ok(geom::measure("text", node, 0.0, &mut self.fonts, &self.base_dir)?)
    }

    pub fn node_bounds(&self, id: &str) -> Option<[f32; 4]> {
        self.bounds.get(id).copied()
    }

    /// What is under frame point (x, y) at `t`, as last rendered: the
    /// smallest visible element there (text, path, shape, image, video),
    /// named by the document node it belongs to (a component instance or
    /// block for its parts) and by the part itself, with its frame bounds.
    pub fn pick(&self, doc: &Document, t: f64, x: f32, y: f32) -> Option<Picked> {
        let (px, py) = (x as f64, y as f64);
        self.elements(doc, t)
            .into_iter()
            .filter(|(p, _)| {
                let b = p.bounds;
                px >= b[0] as f64 && px <= b[2] as f64 && py >= b[1] as f64 && py <= b[3] as f64
            })
            .min_by(|a, b| {
                let area = |p: &Picked| (p.bounds[2] - p.bounds[0]) as f64 * (p.bounds[3] - p.bounds[1]) as f64;
                area(&a.0).total_cmp(&area(&b.0))
            })
            .map(|(p, _)| p)
    }

    /// Every visible element at `t`, as last rendered (render `t` first):
    /// texts, paths, shapes, images and videos that are active and inside
    /// the frame, with their document node, bounds, and the words of text.
    pub fn visible(&self, doc: &Document, t: f64) -> Vec<Visible> {
        let (fw, fh) = (doc.format.width as f32, doc.format.height as f32);
        self.elements(doc, t)
            .into_iter()
            .filter(|(p, _)| p.bounds[2] > 0.0 && p.bounds[3] > 0.0 && p.bounds[0] < fw && p.bounds[1] < fh)
            .map(|(p, text)| Visible { element: p, text })
            .collect()
    }

    fn elements(&self, doc: &Document, t: f64) -> Vec<(Picked, Option<String>)> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let plan = &active.plan;
        let (fw, fh) = (doc.format.width as f64, doc.format.height as f64);
        let flat = self.flat.as_ref().map(|(_, f)| f.clone());
        let node_of = |id: &str| {
            flat.as_ref()
                .and_then(|f| f.doc.nodes.get(id).cloned())
                .or_else(|| doc.nodes.get(id).cloned())
        };
        // What reaches the output at `t`: an inactive composite passes its
        // background through (a transition before its change shows only
        // the outgoing shot), an inactive effect its source, and nothing
        // shows through a zero opacity.
        let n = plan.nodes.len();
        let mut reaches = vec![false; n];
        let output = flat
            .as_ref()
            .map(|f| f.doc.output.clone())
            .unwrap_or_else(|| doc.output.clone());
        let mut stack: Vec<usize> = plan
            .nodes
            .iter()
            .position(|pn| pn.id == output)
            .or(n.checked_sub(1))
            .into_iter()
            .collect();
        while let Some(i) = stack.pop() {
            if reaches[i] {
                continue;
            }
            reaches[i] = true;
            let pn = &plan.nodes[i];
            let (lt, on) = local_time(pn, t);
            let num = |k: &str, d: f64| {
                node_of(&pn.id)
                    .and_then(|nd| nd.params.get(k).and_then(|p| p.eval(lt)))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(d)
            };
            let ports = pn.def.ports;
            let port = |name: &str| {
                (pn.inputs.len() == ports.len())
                    .then(|| ports.iter().position(|p| *p == name).map(|k| pn.inputs[k]))
                    .flatten()
            };
            let next: Vec<usize> = match (on, pn.def.name) {
                (false, _) => port("bg").or(port("source")).into_iter().collect(),
                (true, "over" | "blend") if num("opacity", 1.0) <= 1e-3 => port("bg").into_iter().collect(),
                (true, "opacity") if num("opacity", 1.0) <= 1e-3 => Vec::new(),
                (true, "mix") if num("t", 0.5) <= 1e-3 => port("a").into_iter().collect(),
                (true, "mix") if num("t", 0.5) >= 1.0 - 1e-3 => port("b").into_iter().collect(),
                _ => pn.inputs.clone(),
            };
            stack.extend(next);
        }
        // Each element is then walked up to the output: under an opaque
        // layer (a card cut in over it), cropped away or faded out on every
        // path there, it is not on screen. Text and paths start where their
        // raster chain ends, which is where their bounds are.
        let fdoc: &Document = match flat.as_ref() {
            Some(f) if needs_flat(doc) => &f.doc,
            _ => doc,
        };
        let consumers = review::consumers(plan);
        let opaque = opaque_rects(plan, fdoc, t);
        let mut root_of: Vec<usize> = (0..n).collect();
        for g in &plan.groups {
            if let Some(rc) = &g.raster {
                root_of[rc.node] = g.root;
            }
        }
        let mut out = Vec::new();
        for (i, pn) in plan.nodes.iter().enumerate() {
            let (lt, on) = local_time(pn, t);
            if !on || !reaches[i] {
                continue;
            }
            // Faded out entirely.
            if node_of(&pn.id)
                .and_then(|nd| nd.params.get("opacity").and_then(|p| p.eval(lt)))
                .and_then(|v| v.first().copied())
                .is_some_and(|o| o <= 1e-3)
            {
                continue;
            }
            let b = match self.bounds.get(&pn.id) {
                Some(b) if b[0] < b[2] => [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64],
                Some(_) => continue,
                None if pn.inputs.is_empty() && matches!(pn.def.name, "shape" | "image" | "video") => {
                    let Some(node) = node_of(&pn.id) else { continue };
                    let r = ops::bounds(pn.def, &node, &doc.format, lt, &[], None);
                    // Whole-frame backgrounds are not things to point at.
                    if r[0] <= 0.0 && r[1] <= 0.0 && r[2] >= fw && r[3] >= fh {
                        continue;
                    }
                    r
                }
                None => continue,
            };
            if !review::reaches_screen(plan, fdoc, &consumers, &opaque, root_of[i], b, 1.0, 1e-3, t) {
                continue;
            }
            let node = pn.id.split('/').next().unwrap_or(&pn.id).to_string();
            let text = (pn.def.name == "text")
                .then(|| node_of(&pn.id).map(|n| geom::display_text(&n, lt)))
                .flatten();
            let r = |v: f64| ((v * 10.0).round() / 10.0) as f32;
            out.push((
                Picked {
                    op: doc.nodes.get(&node).map(|n| n.op.clone()).unwrap_or_default(),
                    node,
                    part: pn.id.clone(),
                    bounds: [r(b[0]), r(b[1]), r(b[2]), r(b[3])],
                },
                text,
            ));
        }
        out
    }

    /// Frame bounds of a node's vector content: its own, or the union over
    /// the nodes of a component instance (`id/...`), as last rendered.
    pub fn content_bounds(&self, id: &str) -> Option<[f32; 4]> {
        let prefix = format!("{id}/");
        self.bounds
            .iter()
            .filter(|(k, b)| (k.as_str() == id || k.starts_with(&prefix)) && b[0] < b[2])
            .map(|(_, b)| *b)
            .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
    }

    /// Replans if the document's structure changed. Returns whether it did.
    /// Document with components expanded, cached per revision.
    fn flattened(&mut self, doc: &Document) -> Result<Arc<ultrafast_graph::Flat>, RenderError> {
        let key = (doc.instance, doc.revision);
        if let Some((k, f)) = &self.flat
            && *k == key
        {
            return Ok(f.clone());
        }
        // Components whose nodes expand from their params get a copy per
        // instance with the instance's values in, so those nodes expand
        // wherever they are.
        let specialized;
        let doc = match specialize::per_instance(
            doc,
            &["block", "code", "stack", "captions", "clip_camera", "svg", "lottie"],
        ) {
            Some(d) => {
                specialized = d;
                &specialized
            }
            None => doc,
        };
        let with_blocks;
        let doc = if uses_op(doc, &["block", "code"]) {
            let mut d = doc.clone();
            self.in_scopes(&mut d, Self::expand_blocks);
            if self.blocks.len() > 64 {
                self.blocks.clear();
            }
            with_blocks = d;
            &with_blocks
        } else {
            doc
        };
        let laid_out;
        let doc = if uses_op(doc, &["stack"]) {
            let base = self.base_dir.clone();
            let (mut d, mut problems) = if layout::used_by(doc) {
                layout::expand(doc, &mut self.fonts, &base)
            } else {
                (doc.clone(), Vec::new())
            };
            // Stacks inside components lay out in the component's own graph;
            // its input ports stand in as empty layers while they do.
            let inner: Vec<String> = d
                .components
                .iter()
                .filter(|(_, c)| c.nodes.values().any(|n| n.op == "stack"))
                .map(|(k, _)| k.clone())
                .collect();
            for name in inner {
                let c = &d.components[&name];
                let mut nodes = c.nodes.clone();
                let ports: Vec<String> = c.inputs.iter().map(|p| format!("${p}")).collect();
                for port in &ports {
                    nodes.insert(
                        port.clone(),
                        ultrafast_graph::Node {
                            op: "solid".into(),
                            params: [("color".to_string(), ultrafast_graph::Param::Text("#00000000".into()))].into(),
                            ..Default::default()
                        },
                    );
                }
                let scoped = Document {
                    version: d.version,
                    format: d.format,
                    output: c.output.clone(),
                    nodes,
                    components: Default::default(),
                    revision: 0,
                    structure_revision: 0,
                    instance: 0,
                    unread: Vec::new(),
                    short_keys: false,
                };
                let (mut out, p) = layout::expand(&scoped, &mut self.fonts, &base);
                problems.extend(p.into_iter().map(|(n, m)| (format!("{name}/{n}"), m)));
                for port in &ports {
                    out.nodes.remove(port);
                }
                if let Some(c) = d.components.get_mut(&name) {
                    c.nodes = out.nodes;
                }
            }
            for (n, m) in problems {
                self.diagnose(&n, m);
            }
            laid_out = d;
            &laid_out
        } else {
            doc
        };
        let with_raymarch;
        let doc = if let Some((d, problems)) = expand_everywhere(doc, raymarch::used_by, raymarch::expand) {
            for (n, m) in problems {
                self.diagnose(&n, m);
            }
            with_raymarch = d;
            &with_raymarch
        } else {
            doc
        };
        let with_transitions;
        let doc = if let Some((d, problems)) = expand_everywhere(doc, transitions::used_by, transitions::expand) {
            for (n, m) in problems {
                self.diagnose(&n, m);
            }
            with_transitions = d;
            &with_transitions
        } else {
            doc
        };
        let with_captions;
        let doc = if uses_op(doc, &["captions"]) {
            let mut d = doc.clone();
            self.in_scopes(&mut d, Self::expand_captions);
            if self.captions.len() > 32 {
                self.captions.clear();
            }
            with_captions = d;
            &with_captions
        } else {
            doc
        };
        let with_follow;
        let doc = if uses_op(doc, &["clip_camera"]) {
            let mut d = doc.clone();
            self.in_scopes(&mut d, Self::expand_follow);
            if self.follows.len() > 32 {
                self.follows.clear();
            }
            with_follow = d;
            &with_follow
        } else {
            doc
        };
        let expanded;
        let doc = if svg::used_by(doc) {
            let base = self.base_dir.clone();
            let mut problems = Vec::new();
            let svgs = &mut self.svgs;
            expanded = svg::expand(
                doc,
                |_, node| {
                    let key = format!("{:?}", (node.params.get("src"), node.params.get("size")));
                    let src = match node.params.get("src") {
                        Some(ultrafast_graph::Param::Text(s)) => base.join(s),
                        _ => PathBuf::new(),
                    };
                    let mtime = std::fs::metadata(&src).and_then(|m| m.modified()).ok();
                    if let Some((m, imp)) = svgs.get(&key)
                        && *m == mtime
                    {
                        return Ok(imp.clone());
                    }
                    let imp = Arc::new(svg::import_node(node, &base)?);
                    svgs.insert(key, (mtime, imp.clone()));
                    Ok(imp)
                },
                |node, msg| problems.push((node.to_string(), msg)),
            );
            for (n, m) in problems {
                self.diagnose(&n, m);
            }
            &expanded
        } else {
            doc
        };
        let with_lottie;
        let doc = if lottie::used_by(doc) {
            let base = self.base_dir.clone();
            let mut problems = Vec::new();
            let cache = &mut self.lotties;
            let fonts = &mut self.fonts;
            with_lottie = lottie::expand(
                doc,
                |_, node| {
                    let key = format!("{:?}", (node.params.get("src"), node.params.get("size")));
                    let src = match node.params.get("src") {
                        Some(ultrafast_graph::Param::Text(s)) => base.join(s),
                        _ => PathBuf::new(),
                    };
                    let mtime = std::fs::metadata(&src).and_then(|m| m.modified()).ok();
                    if let Some((m, imp)) = cache.get(&key)
                        && *m == mtime
                    {
                        return Ok(imp.clone());
                    }
                    let imp = Arc::new(lottie::import_node(node, &base, fonts)?);
                    cache.insert(key, (mtime, imp.clone()));
                    Ok(imp)
                },
                |node, msg| problems.push((node.to_string(), msg)),
            );
            for (n, m) in problems {
                self.diagnose(&n, m);
            }
            &with_lottie
        } else {
            doc
        };
        let mut f = if builtins::used_by(doc) {
            builtins::with_builtins(doc).flatten()?
        } else {
            doc.flatten()?
        };
        self.bake_audio_drivers(&mut f.doc);
        if pushdown::wanted(&f.doc) {
            pushdown::distribute(&mut f);
        }
        let f = Arc::new(f);
        self.flat = Some((key, f.clone()));
        Ok(f)
    }

    /// Runs an expansion pass on the document's own nodes and on each
    /// component's (`scope` names where, for the components a pass makes),
    /// adding what it makes to the document's components.
    fn in_scopes(
        &mut self,
        d: &mut Document,
        mut pass: impl FnMut(
            &mut Self,
            &mut BTreeMap<String, ultrafast_graph::Node>,
            &mut BTreeMap<String, ultrafast_graph::Component>,
            &ultrafast_graph::Format,
            &str,
        ),
    ) {
        let fmt = d.format;
        let mut made = BTreeMap::new();
        pass(self, &mut d.nodes, &mut made, &fmt, "");
        let names: Vec<String> = d.components.keys().cloned().collect();
        for name in names {
            let Some(c) = d.components.get_mut(&name) else { continue };
            let mut nodes = std::mem::take(&mut c.nodes);
            pass(self, &mut nodes, &mut made, &fmt, &format!("{name}/"));
            if let Some(c) = d.components.get_mut(&name) {
                c.nodes = nodes;
            }
        }
        d.components.extend(made);
    }

    /// `block` and `code` nodes become components of their own.
    fn expand_blocks(
        &mut self,
        nodes: &mut BTreeMap<String, ultrafast_graph::Node>,
        made: &mut BTreeMap<String, ultrafast_graph::Component>,
        fmt: &ultrafast_graph::Format,
        scope: &str,
    ) {
        let base = self.base_dir.clone();
        let ids: Vec<String> = nodes
            .iter()
            .filter(|(_, n)| n.op == "block" || n.op == "code")
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let node = nodes[&id].clone();
            let key = format!("{}|{:?}|{:?}", node.op, node.params, (fmt.width, fmt.height));
            let built = match self.blocks.get(&key) {
                Some(c) => Ok(c.clone()),
                None if node.op == "code" => code::build(&node, fmt, &mut self.fonts, &base).map(Arc::new),
                None => blocks::build(&node, fmt, &mut self.fonts, &base).map(Arc::new),
            };
            let n = nodes.get_mut(&id).unwrap();
            match built {
                Ok(c) => {
                    self.blocks.insert(key, c.clone());
                    // A block costs nothing once it has exited: it
                    // gets a window over its own lifetime.
                    if n.op == "block"
                        && let Some(life) = blocks::lifetime(n)
                    {
                        let tm = n.timing.get_or_insert_with(Default::default);
                        if !tm.has_window() && tm.loop_duration.is_none() && tm.speed > 0.0 {
                            tm.in_point = Some(tm.offset);
                            tm.out_point = Some(tm.offset + life / tm.speed);
                        }
                    }
                    let name = format!("{}:{scope}{id}", n.op);
                    made.insert(name.clone(), (*c).clone());
                    n.op = name;
                    n.params.retain(|k, _| blocks::PARAMS.contains(&k.as_str()));
                }
                Err(e) => {
                    self.diagnose(&format!("{scope}{id}"), e);
                    n.op = "solid".into();
                    n.params = [("color".to_string(), ultrafast_graph::Param::Text("#00000000".into()))].into();
                }
            }
        }
    }

    /// `captions` nodes become components of word nodes.
    fn expand_captions(
        &mut self,
        nodes: &mut BTreeMap<String, ultrafast_graph::Node>,
        made: &mut BTreeMap<String, ultrafast_graph::Component>,
        fmt: &ultrafast_graph::Format,
        scope: &str,
    ) {
        let base = self.base_dir.clone();
        let ids: Vec<String> = nodes
            .iter()
            .filter(|(_, n)| n.op == "captions")
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let node = nodes[&id].clone();
            let src_time = match node.params.get("src") {
                Some(ultrafast_graph::Param::Text(s)) => {
                    std::fs::metadata(base.join(s)).and_then(|m| m.modified()).ok()
                }
                _ => None,
            };
            let key = format!("{:?}|{:?}|{:?}", node.params, src_time, (fmt.width, fmt.height));
            let built = match self.captions.get(&key) {
                Some(c) => Ok(c.clone()),
                None => captions::words_of(&node, &base)
                    .and_then(|w| captions::build(&w, &node, fmt, &mut self.fonts, &base))
                    .map(Arc::new),
            };
            let n = nodes.get_mut(&id).unwrap();
            match built {
                Ok(c) => {
                    self.captions.insert(key, c.clone());
                    let name = format!("captions:{scope}{id}");
                    made.insert(name.clone(), (*c).clone());
                    n.op = name;
                    n.params.retain(|k, _| captions::PARAMS.contains(&k.as_str()));
                }
                Err(e) => {
                    self.diagnose(&format!("{scope}{id}"), e);
                    n.op = "solid".into();
                    n.params = [("color".to_string(), ultrafast_graph::Param::Text("#00000000".into()))].into();
                }
            }
        }
    }

    /// A `clip_camera` node with `faces`: as given, or tracked from its
    /// footage (see [`autofaces`]) when it has none.
    fn with_faces(
        &mut self,
        node: &ultrafast_graph::Node,
        fmt: &ultrafast_graph::Format,
        base: &Path,
    ) -> Result<ultrafast_graph::Node, String> {
        use ultrafast_graph::Param;
        if node.params.contains_key("faces") {
            return Ok(node.clone());
        }
        let Some(Param::Text(src)) = node.params.get("src") else {
            return Err("`clip_camera` needs `src` (the source video)".into());
        };
        let num = |k: &str| {
            node.params
                .get(k)
                .and_then(|p| p.eval(0.0))
                .and_then(|v| v.first().copied())
        };
        let start = num("start").unwrap_or(0.0);
        let duration = num("duration").unwrap_or(fmt.duration);
        let path = if Path::new(src).is_absolute() {
            PathBuf::from(src)
        } else {
            base.join(src)
        };
        let faces = autofaces::track(&self.device, &path, start, start + duration)?;
        let mut node = node.clone();
        node.params.insert("faces".into(), Param::Text(faces.to_string()));
        Ok(node)
    }

    /// `clip_camera` nodes become components of keyed video nodes.
    fn expand_follow(
        &mut self,
        nodes: &mut BTreeMap<String, ultrafast_graph::Node>,
        made: &mut BTreeMap<String, ultrafast_graph::Component>,
        fmt: &ultrafast_graph::Format,
        scope: &str,
    ) {
        let base = self.base_dir.clone();
        let ids: Vec<String> = nodes
            .iter()
            .filter(|(_, n)| n.op == "clip_camera")
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let node = nodes[&id].clone();
            let key = format!(
                "{:?}|{:?}|{:?}",
                node.params,
                follow::inputs_time(&node, &base),
                (fmt.width, fmt.height, fmt.fps.to_bits(), fmt.duration.to_bits())
            );
            let built = match self.follows.get(&key) {
                Some(c) => Ok(c.clone()),
                None => self
                    .with_faces(&node, fmt, &base)
                    .and_then(|node| follow::build(&node, fmt, &base).map(Arc::new)),
            };
            let n = nodes.get_mut(&id).unwrap();
            match built {
                Ok(c) => {
                    self.follows.insert(key, c.clone());
                    let name = format!("clip_camera:{scope}{id}");
                    made.insert(name.clone(), (*c).clone());
                    n.op = name;
                    n.params.retain(|k, _| follow::PARAMS.contains(&k.as_str()));
                }
                Err(e) => {
                    self.diagnose(&format!("{scope}{id}"), e);
                    n.op = "solid".into();
                    n.params = [("color".to_string(), ultrafast_graph::Param::Text("#00000000".into()))].into();
                }
            }
        }
    }

    /// Fetches the font families `doc` names that this machine lacks,
    /// several at once, before its first render would fetch them one by one.
    pub fn prefetch_fonts(&mut self, doc: &Document) {
        let mut names: Vec<String> = Vec::new();
        let mut scan = |nodes: &BTreeMap<String, ultrafast_graph::Node>| {
            for n in nodes.values() {
                for k in ["font", "em_font"] {
                    if let Some(ultrafast_graph::Param::Text(f)) = n.params.get(k) {
                        for name in f.split(',').map(|x| x.trim().trim_matches(|c| c == '"' || c == '\'')) {
                            if !name.is_empty() && !names.iter().any(|x| x == name) {
                                names.push(name.to_string());
                            }
                        }
                    }
                }
            }
        };
        scan(&doc.nodes);
        for c in doc.components.values() {
            scan(&c.nodes);
        }
        let missing: Vec<String> = names.into_iter().filter(|n| self.fonts.lacks(n)).collect();
        if !missing.is_empty() {
            fontfetch::prefetch(&missing);
        }
    }

    /// Replaces audio-driven params with keyframes (60 per second) that
    /// follow the soundtrack's loudness in the chosen band.
    fn bake_audio_drivers(&mut self, doc: &mut Document) {
        let duration = doc.format.duration;
        let mut baked: Vec<(String, String, ultrafast_graph::Param)> = Vec::new();
        for (id, node) in &doc.nodes {
            for (name, p) in &node.params {
                let ultrafast_graph::Param::Audio { audio } = p else {
                    continue;
                };
                let Some(env) = self.envelope(audio) else { continue };
                let (lo, hi) = (
                    audio.range.first().copied().unwrap_or(0.0),
                    audio.range.get(1).copied().unwrap_or(1.0),
                );
                let n = (duration * 60.0).ceil() as usize + 1;
                let keys: Vec<ultrafast_graph::Key> = (0..n)
                    .map(|i| {
                        let t = i as f64 / 60.0;
                        let k = ((t + audio.start) * 120.0).round().max(0.0) as usize;
                        let v = env.get(k).copied().unwrap_or(0.0) as f64;
                        ultrafast_graph::Key {
                            t,
                            v: vec![lo + (hi - lo) * v],
                            ease: ultrafast_graph::Ease::Linear,
                        }
                    })
                    .collect();
                baked.push((
                    id.clone(),
                    name.clone(),
                    ultrafast_graph::Param::Animated { keys, repeat: None },
                ));
            }
        }
        for (id, name, p) in baked {
            doc.nodes.get_mut(&id).unwrap().params.insert(name, p);
        }
    }

    fn envelope(&mut self, a: &ultrafast_graph::driver::AudioDriver) -> Option<Arc<Vec<f32>>> {
        let key = format!("{}|{}|{}", a.src, a.band, a.smooth);
        if let Some(e) = self.envelopes.get(&key) {
            return e.clone();
        }
        let path = media_path(&self.base_dir, &a.src);
        let env = match ultrafast_media::audio::decode(&path) {
            Ok(Some(pcm)) => Some(Arc::new(audio_envelope(&pcm, &a.band, a.smooth))),
            Ok(None) => {
                self.diagnose("audio", format!("{} has no audio track", path.display()));
                None
            }
            Err(e) => {
                self.diagnose("audio", format!("{}: {e}", path.display()));
                None
            }
        };
        self.envelopes.insert(key, env.clone());
        env
    }

    fn ensure_plan(&mut self, doc: &Document, chains: Option<&plan::Chains>) -> Result<bool, RenderError> {
        self.view = View::sized(doc, self.out_size);
        let view = self.view;
        let same_view = self.active.as_ref().is_some_and(|a| a.view == view);
        if !same_view {
            // Geometry built for another density is not mapped to this one
            // (outlines are flattened and glyphs drawn for their size): a
            // size renders the same however the renderer got to it.
            self.geometry.clear();
            self.built.clear();
        }
        if same_view && self.seen_revision == Some((doc.instance, doc.revision)) {
            return Ok(false);
        }
        self.seen_revision = Some((doc.instance, doc.revision));
        let sig = plan::signature(doc, chains);
        if same_view
            && let Some(active) = self.active.as_mut()
            && active.plan.signature == sig
        {
            // Timing is a value like any other: a new offset, speed or window
            // keeps the plan, and its nodes take their new timing chains.
            for pn in &mut active.plan.nodes {
                pn.chain = chains.and_then(|c| c.get(&pn.id)).cloned().unwrap_or_default();
            }
            return Ok(false);
        }
        let plan = plan::compile(doc, chains)?;
        // Per-node caches of nodes that no longer exist are dropped.
        self.geometry.retain(|id, _| doc.nodes.contains_key(id));
        self.built.retain(|id, _| doc.nodes.contains_key(id));
        self.bounds.retain(|id, _| doc.nodes.contains_key(id));
        if let Some(three) = self.three.as_mut()
            && three.meshes.len() > 64
        {
            three.meshes.clear();
        }
        // Video decoders of nodes that no longer exist are dropped.
        self.videos
            .retain(|id, _| doc.nodes.get(id).is_some_and(|n| n.op == "video"));
        // Queue every fused kernel this plan needs; known ones are no-ops.
        let jobs: Vec<(String, String)> = plan
            .groups
            .iter()
            .filter(|g| g.kind == GroupKind::Fused || (g.kind == GroupKind::Sim && !g.key.is_empty()))
            .map(|g| (g.key.clone(), g.body.clone()))
            .collect();
        self.kernels.request(jobs);
        for g in &plan.groups {
            for t in &g.textures {
                if let TexSource::Resource(src) = t
                    && let Some(path) = src.strip_prefix("image:").or_else(|| src.strip_prefix("lut:"))
                {
                    // Sequences are requested per frame.
                    if !path.contains("{frame") {
                        self.images.request(path);
                    }
                }
            }
        }
        if let Some(old) = self.active.take() {
            for (t, r) in old.slots.into_iter().zip(old.resident) {
                if r {
                    self.pool.give_back(t);
                }
            }
            for (p, b) in old.raster.into_iter().zip(old.borrowed) {
                if let (Some(p), false) = (p, b) {
                    self.pool.give_back(p.tex);
                }
            }
        }
        let desc = TextureDesc {
            width: view.w,
            height: view.h,
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        };
        let mut slot_groups = vec![Vec::new(); plan.slot_count];
        for (gi, g) in plan.groups.iter().enumerate() {
            if let Some(s) = slot_groups.get_mut(g.output) {
                s.push(gi);
            }
        }
        let n = plan.groups.len();
        let low = low_readers(&plan);
        self.active = Some(Active {
            slots: vec![self.placeholder.clone(); plan.slot_count],
            resident: vec![false; plan.slot_count],
            slot_used: vec![0; plan.slot_count],
            slot_groups,
            slot_desc: desc,
            view,
            group_used: vec![0; n],
            plan,
            cache: vec![None; n],
            raster: vec![None; n],
            drawn: vec![0; n],
            alias: vec![None; n],
            borrowed: vec![false; n],
            bounds: vec![ops::UNBOUNDED; n],
            low_readers: low,
        });
        Ok(true)
    }

    /// Renders the frames that follow at `size` (device pixels) instead of
    /// each document's own size; None goes back to that. The document stays
    /// in its own frame pixels: a 1920x1080 document at 3840x2160 is drawn
    /// with twice the texels (text, paths, blurs, particles, 3D re-rendered
    /// at the finer grid), not scaled up. A size of another shape keeps the
    /// finer of the two scales and crops the last texel row or column.
    pub fn set_output_size(&mut self, size: Option<(u32, u32)>) {
        self.out_size = size;
    }

    /// The output size asked for (None: each document's own).
    pub fn output_size(&self) -> Option<(u32, u32)> {
        self.out_size
    }

    /// The size frames of `doc` render at, in device pixels.
    pub fn frame_size(&self, doc: &Document) -> (u32, u32) {
        let v = View::sized(doc, self.out_size);
        (v.w, v.h)
    }

    fn export_texture(&mut self, w: u32, h: u32, format: PixelFormat) -> Texture {
        let desc = TextureDesc {
            width: w,
            height: h,
            format,
            cpu_readable: true,
        };
        match &self.export {
            Some(t) if t.desc() == desc => t.clone(),
            _ => {
                let t = self.device.create_texture(desc);
                self.export = Some(t.clone());
                t
            }
        }
    }

    /// Encodes and submits frame `t` (seconds). With `export`, the final image
    /// is also encoded to sRGB RGBA8 in a CPU-readable texture for
    /// [`Renderer::read_export`]. Does not wait for the GPU.
    pub fn submit_frame(
        &mut self,
        doc: &Document,
        t: f64,
        export: bool,
    ) -> Result<(Submitted, FrameReport), RenderError> {
        if !export {
            return self.submit_frame_with(doc, t, |_| {});
        }
        let (w, h) = self.frame_size(doc);
        let ex = self.export_texture(w, h, PixelFormat::Rgba8Unorm);
        self.submit_frame_with(doc, t, |o| {
            let mut p = o.header.to_vec();
            p.extend(o.origin);
            p.push(0.0);
            o.list
                .dispatch(&o.kernels.utility("k_encode_srgb8"), &ex, &[o.texture], &p);
        })
    }

    /// Encodes frame `t`, then lets `finish` add work that consumes the
    /// final image (export, display, video encode) before submitting.
    pub fn submit_frame_with(
        &mut self,
        doc: &Document,
        t: f64,
        finish: impl FnOnce(FrameOutput),
    ) -> Result<(Submitted, FrameReport), RenderError> {
        let start = Instant::now();
        let flat = if needs_flat(doc) {
            Some(self.flattened(doc)?)
        } else {
            None
        };
        let doc: &Document = flat.as_ref().map(|f| &f.doc).unwrap_or(doc);
        let chains = flat.as_ref().map(|f| &f.chains);
        let replanned = self.ensure_plan(doc, chains)?;
        let plan_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut report = FrameReport {
            replanned,
            plan_ms,
            ..Default::default()
        };
        self.media_pending = 0;
        // Under memory pressure (zooming layers replace big textures every
        // frame), finish the frames in flight and let go of what they and
        // the pool hold before starting another.
        if self.device.memory_in_use() > self.device.memory_bytes() / 10 * 7 {
            self.pool.free.clear();
            self.pool.last_used.clear();
            self.pool.free_bytes = 0;
            self.device.reclaim();
        }
        let mut list = self.device.begin();
        for tex in std::mem::take(&mut self.mb_release) {
            self.pool.give_back(tex);
        }
        // Frames older than the in-flight window are no longer read by the GPU.
        self.frame_no += 1;
        while self.retained.front().is_some_and(|(n, _)| *n + 4 < self.frame_no) {
            self.retained.pop_front();
        }
        self.prepare_images(&mut list);

        let mut active = self.active.take().expect("plan");
        self.trim_slots(&mut active);
        self.frame_t = t;
        let result = self
            .sim_catch_up(&mut active, doc, t, &mut list, &mut report)
            .and_then(
                |_| match doc.format.motion_blur.filter(|m| m.samples > 1 && !self.mb_off) {
                    None => self.encode_groups(&mut active, doc, t, &mut list, &mut report),
                    Some(mb) => self.encode_motion_blur(&mut active, doc, t, mb, &mut list, &mut report),
                },
            );
        self.active = Some(active);
        let output = result?;
        // Consumers get the frame in device texels: the texture at the
        // view's density, its origin and the header's size in texels.
        let output = self.on_device_grid(output, doc, t, &mut list);
        let v = self.view;
        finish(FrameOutput {
            list: &mut list,
            kernels: &self.kernels,
            texture: &output.tex,
            origin: [output.origin[0] / v.px, output.origin[1] / v.px],
            header: [t as f32, 1.0, v.w as f32, v.h as f32],
        });
        report.cpu_encode_ms = start.elapsed().as_secs_f64() * 1000.0;
        report.media_pending = self.media_pending;
        self.images.end_frame();
        Ok((list.submit(), report))
    }

    /// `p` at the view's density: as it is when it already is, else
    /// resampled into a frame-sized texture (given back next frame).
    fn on_device_grid(&mut self, p: Placed, doc: &Document, t: f64, list: &mut CommandList) -> Placed {
        let v = self.view;
        let aligned = |o: f32| (o / v.px - (o / v.px).round()).abs() < 1e-3;
        if p.scale == v.px && aligned(p.origin[0]) && aligned(p.origin[1]) {
            return p;
        }
        let desc = TextureDesc {
            width: v.w,
            height: v.h,
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        };
        let tex = self.pool.lease(&self.device, desc);
        let taps = (v.px / p.scale).ceil().max(1.0);
        let mut prm = header(doc, t, v).to_vec();
        prm.extend([v.px, 0.0, 0.0, p.origin[0], p.origin[1], p.scale, taps]);
        list.dispatch(&self.kernels.utility("k_resample"), &tex, &[&p.tex], &prm);
        self.mb_release.push(tex.clone());
        Placed {
            tex,
            origin: [0.0, 0.0],
            scale: v.px,
        }
    }

    /// Keeps frame-to-frame GPU memory under the budget: while resident
    /// slots and kept raster textures exceed it, the ones no frame needed
    /// lately are given back, oldest first. Their groups' caches are dropped,
    /// so they render again when next needed.
    fn trim_slots(&mut self, active: &mut Active) {
        let slot_bytes = tex_bytes(active.slot_desc);
        // (last used, slot or group, bytes)
        let mut held = 0u64;
        let mut cold: Vec<(u64, Result<usize, usize>, u64)> = Vec::new();
        for s in 0..active.slots.len() {
            if active.resident[s] {
                held += slot_bytes;
                if active.slot_used[s] + 1 < self.frame_no {
                    cold.push((active.slot_used[s], Ok(s), slot_bytes));
                }
            }
        }
        for g in 0..active.raster.len() {
            if let (Some(p), false) = (&active.raster[g], active.borrowed[g]) {
                let b = tex_bytes(p.tex.desc());
                held += b;
                if active.group_used[g] + 1 < self.frame_no {
                    cold.push((active.group_used[g], Err(g), b));
                }
            }
        }
        if std::env::var_os("ULTRAFAST_DEBUG_MEM").is_some()
            && self.frame_no
                % std::env::var("ULTRAFAST_DEBUG_MEM")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n: &u64| n > 1)
                    .unwrap_or(30)
                == 0
        {
            eprintln!(
                "mem: frame {}: slots+rasters {} MiB (budget {} MiB, {} cold), pool {} MiB, acc {} MiB, sims {}, images {} MiB, allocated {}/{} MiB, device {} MiB, rss {} MiB",
                self.frame_no,
                held >> 20,
                self.slot_budget >> 20,
                cold.len(),
                self.pool.free_bytes >> 20,
                self.acc.as_ref().map_or(0, |b| b.len()) >> 20,
                self.sims.len(),
                self.images.bytes() >> 20,
                self.device.memory_allocated().0 >> 20,
                self.device.memory_allocated().1 >> 20,
                self.device.memory_in_use() >> 20,
                std::fs::read_to_string("/proc/self/statm")
                    .ok()
                    .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
                    .map_or(0, |pages| pages * 4096 >> 20)
            );
            if self.frame_no % 10 == 0 {
                eprintln!("mem:   {}", self.device.memory_breakdown(8));
            }
        }
        if held <= self.slot_budget {
            return;
        }
        cold.sort_by_key(|c| c.0);
        for (_, which, bytes) in cold {
            if held <= self.slot_budget {
                break;
            }
            held -= bytes;
            // Dropped, not pooled: the point is to free the memory.
            match which {
                Ok(s) => {
                    drop(std::mem::replace(&mut active.slots[s], self.placeholder.clone()));
                    active.resident[s] = false;
                    for &g in &active.slot_groups[s] {
                        active.cache[g] = None;
                    }
                }
                Err(g) => {
                    drop(active.raster[g].take());
                    active.drawn[g] = 0;
                }
            }
        }
    }

    /// Uploads newly decoded images and converts them to the working space.
    fn prepare_images(&mut self, list: &mut CommandList) {
        let block = self.mode == Mode::Exact;
        let t0 = Instant::now();
        let fresh = self.images.poll(&self.device, block);
        self.image_wait += t0.elapsed().as_secs_f64();
        if fresh.is_empty() {
            return;
        }
        let prep = self.kernels.utility("k_prep_srgb8");
        for (src, raw) in fresh {
            let d = raw.desc();
            let tex = self.device.create_texture(TextureDesc {
                width: d.width,
                height: d.height,
                format: PixelFormat::Rgba16Float,
                cpu_readable: false,
            });
            list.dispatch(&prep, &tex, &[&raw], &[0.0, 0.0, d.width as f32, d.height as f32, 0.0]);
            self.images.mark_ready(&src, tex);
        }
    }

    fn resource(
        &mut self,
        key: &str,
        node: &str,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> (Texture, Option<ResourceInfo>) {
        if let Some(rest) = key.strip_prefix("video:") {
            let path = rest.split('\u{1f}').next().unwrap_or(rest);
            return self.video_frame(path, node, doc, t, list);
        }
        let raw = key
            .strip_prefix("image:")
            .or_else(|| key.strip_prefix("lut:"))
            .unwrap_or(key);
        // A sequence's key carries its node id after a unit separator.
        let raw = raw.split('\u{1f}').next().unwrap_or(raw);
        // Image sequences resolve to this frame's file.
        let resolved;
        let src = if raw.contains("{frame") && !node.is_empty() {
            resolved = ops::sequence_path(raw, &doc.nodes[node], t, doc.format.fps);
            self.images.request(&resolved);
            // The next frames decode on other threads while this one renders.
            for i in 1..=SEQUENCE_AHEAD {
                let next = ops::sequence_path(raw, &doc.nodes[node], t + i as f64 / doc.format.fps, doc.format.fps);
                if next != resolved {
                    self.images.prefetch(&next);
                }
            }
            if self.mode == Mode::Exact {
                self.prepare_images(list);
            }
            resolved.as_str()
        } else {
            // Still images are requested when the plan is built, but the LRU
            // may drop one before its first use (a cutout sequence filling
            // the cache while the image waits for its window): ask again,
            // or it stays a placeholder for the rest of an export.
            if !self.images.is_known(raw) {
                self.images.request(raw);
                if self.mode == Mode::Exact {
                    self.prepare_images(list);
                }
            }
            raw
        };
        match self.images.get(src) {
            Some(images::ImageState::Ready(t)) => {
                let d = t.desc();
                (
                    t.clone(),
                    Some(ResourceInfo {
                        width: d.width,
                        height: d.height,
                    }),
                )
            }
            Some(images::ImageState::Failed(e)) => {
                let e = e.clone();
                self.diagnose(node, e);
                (self.placeholder.clone(), None)
            }
            _ => {
                self.media_pending += 1;
                (self.placeholder.clone(), None)
            }
        }
    }

    /// The current frame of a `video` node, converted to the working space.
    /// Decoded frames arrive zero-copy from the hardware decoder (or as the
    /// decoder's bytes in GPU-visible memory); one pass per new source
    /// frame converts, turns upright and linearizes them into a node-owned
    /// texture.
    fn video_frame(
        &mut self,
        path: &str,
        node: &str,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> (Texture, Option<ResourceInfo>) {
        let state = self.videos.entry(node.to_string()).or_default();
        if state.path != path {
            let full = media_path(&self.base_dir, path);
            *state = VideoState {
                path: path.to_string(),
                stamp: images::file_stamp(&full),
                ..Default::default()
            };
            match ultrafast_media::VideoSource::open(&self.device, &full) {
                Ok(s) => state.source = Some(s),
                Err(e) => state.error = Some(e.to_string()),
            }
        }
        if let Some(e) = state.error.clone() {
            self.diagnose(node, e);
            return (self.placeholder.clone(), None);
        }
        let source = state.source.as_ref().unwrap();
        let info = ResourceInfo {
            width: source.info.width,
            height: source.info.height,
        };
        let tau = ops::video_time(&doc.nodes[node], t, source.info.duration);
        let waited = source.waited();
        let frame = source.frame_at(tau, self.mode == Mode::Exact);
        self.media_wait += source.waited() - waited;
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                state.error = Some(e.to_string());
                self.diagnose(node, e.to_string());
                return (self.placeholder.clone(), None);
            }
        };
        let Some(frame) = frame else {
            self.media_pending += 1;
            return match &state.prepared {
                Some(p) => (p.clone(), Some(info)),
                None => (self.placeholder.clone(), Some(info)),
            };
        };
        if state.prepared.is_none() || state.prepared_pts != Some(frame.pts) {
            let tex = state
                .prepared
                .get_or_insert_with(|| {
                    self.device.create_texture(TextureDesc {
                        width: info.width,
                        height: info.height,
                        format: PixelFormat::Rgba16Float,
                        cpu_readable: false,
                    })
                })
                .clone();
            let mut p = vec![0.0, 0.0, info.width as f32, info.height as f32];
            p.extend(source.format.params(source.info.rotation));
            let kernel = self.kernels.utility("k_prep_video");
            #[cfg(all(target_os = "macos", not(feature = "vulkan")))]
            let (input, data) = (frame.texture(), BufferBinding::Data(&[0u8; 16]));
            #[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
            let (input, data) = (&self.placeholder, BufferBinding::Buffer(frame.buffer()));
            list.dispatch_ex(
                &kernel,
                &tex,
                &[input],
                &p,
                Bindings {
                    buffers: &[data],
                    ..Default::default()
                },
            );
            state.prepared_pts = Some(frame.pts);
            // The decoded frame must outlive the GPU work reading it.
            self.retained.push_back((self.frame_no, frame));
        }
        (state.prepared.clone().unwrap(), Some(info))
    }

    /// Renders `mb.samples` sub-frames across the shutter (more where
    /// things move fast, up to `mb.max_samples`) and averages them layer by
    /// layer, as a compositor blurs each moving layer. Every
    /// group runs in one phase: groups that read nothing moving run once, at
    /// the frame's time, first (phase 0); groups holding a moving layer run
    /// per sample, in waves (odd phases), chained to the sampled groups of
    /// their wave they read; a sampled group read by a later phase is
    /// averaged across the samples, and the groups that read it run once,
    /// after its wave (even phases), on that average. A frame where nothing
    /// moves renders once.
    fn encode_motion_blur(
        &mut self,
        active: &mut Active,
        doc: &Document,
        t: f64,
        mb: ultrafast_graph::MotionBlur,
        list: &mut CommandList,
        report: &mut FrameReport,
    ) -> Result<Placed, RenderError> {
        let span = mb.shutter / doc.format.fps;
        let moving = if self.mb_full {
            vec![true; active.plan.nodes.len()]
        } else {
            self.moving_nodes(&active.plan, doc, t - span / 2.0, t + span / 2.0)
        };
        let n = match mb.max_samples {
            Some(max) if max > mb.samples => {
                // Motion measured in device texels: a larger view takes more
                // samples for the same move, so it blurs as smoothly.
                mb.samples_for(
                    self.motion_extent_px(&active.plan, doc, &moving, t - span / 2.0, t + span / 2.0)
                        / self.view.px as f64,
                )
            }
            _ => mb.samples,
        };
        let plan = &active.plan;
        let ng = plan.groups.len();
        // What reaches the output at any time in the shutter (a cut inside
        // it shows the shot before it in the first samples).
        let mut live = live_groups(plan, t);
        for s in 0..n {
            let ts = t - span / 2.0 + span * (s as f64 + 0.5) / n as f64;
            for (l, m) in live.iter_mut().zip(live_groups(plan, ts)) {
                *l |= m;
            }
        }
        let here: Vec<bool> = plan.groups.iter().map(|g| g.nodes.iter().any(|&m| moving[m])).collect();
        // Phases in plan order (inputs come first). A group that reads
        // nothing moving: 0. One holding a moving node: odd, the wave of the
        // sampled groups it reads, or the next one after anything else it
        // reads (whose sampled inputs it then reads as averages). Any other
        // group reading something moving: even, after what it reads.
        let mut phase = vec![0u32; ng];
        let mut affected = vec![false; ng];
        for g in 0..ng {
            let mut p = 0u32;
            for ts in &plan.groups[g].textures {
                let TexSource::Group(i) = *ts else { continue };
                if !affected[i] {
                    continue;
                }
                let sampled_i = phase[i] % 2 == 1;
                p = p.max(match (here[g], sampled_i) {
                    (true, true) | (false, false) => phase[i],
                    _ => phase[i] + 1,
                });
            }
            affected[g] = here[g] || p > 0;
            phase[g] = match (affected[g], here[g]) {
                (false, _) => 0,
                (true, true) => p.max(1) | 1,
                (true, false) => p.max(2),
            };
        }
        let sampled: Vec<bool> = phase.iter().map(|p| p % 2 == 1).collect();
        if !(0..ng).any(|g| sampled[g] && live[g]) {
            report.mb_samples = 1;
            return self.encode_groups(active, doc, t, list, report);
        }
        let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); ng];
        for (c, gr) in plan.groups.iter().enumerate() {
            for ts in &gr.textures {
                if let TexSource::Group(s) = *ts {
                    consumers[s].push(c);
                }
            }
        }
        // Read by a later phase: what it makes must last until then.
        let crosses: Vec<bool> = (0..ng)
            .map(|g| consumers[g].iter().any(|&c| phase[c] > phase[g]))
            .collect();
        // A group that runs once and crosses, with a slot that another
        // group writes while it is still needed, renders into a texture of
        // its own this frame.
        let writes_slot = |k: usize| {
            matches!(
                plan.groups[k].kind,
                GroupKind::Fused | GroupKind::Render3D | GroupKind::Sim
            )
        };
        let pinned: Vec<usize> = (0..ng)
            .filter(|&g| {
                let gr = &plan.groups[g];
                !sampled[g]
                    && crosses[g]
                    && live[g]
                    && gr.kind == GroupKind::Fused
                    && !gr.is_static
                    && (0..ng).any(|k| {
                        k != g
                            && writes_slot(k)
                            && plan.groups[k].output == gr.output
                            && (phase[k] > phase[g] || (phase[k] == phase[g] && k > g))
                    })
            })
            .collect();
        report.mb_samples = n;
        report.mb_split = (0..ng).rev().find(|&g| sampled[g] && live[g]).unwrap_or(0);
        report.mb_once = (0..ng).filter(|&g| !sampled[g] && live[g]).count();
        if std::env::var_os("ULTRAFAST_DEBUG_MB").is_some() {
            let names: Vec<&str> = moving
                .iter()
                .enumerate()
                .filter(|(_, m)| **m)
                .map(|(i, _)| plan.nodes[i].id.as_str())
                .collect();
            let per: Vec<String> = (0..ng)
                .filter(|&g| sampled[g] && live[g])
                .map(|g| format!("g{g}@{}", phase[g]))
                .collect();
            eprintln!(
                "t={t:.3} {} once ({} pinned), sampled {}; moving: {}",
                report.mb_once,
                pinned.len(),
                per.join(" "),
                names.join(" ")
            );
        }
        for &g in &pinned {
            let tex = self.pool.lease(&self.device, active.slot_desc);
            active.raster[g] = Some(Placed {
                tex,
                origin: [0.0, 0.0],
                scale: active.view.px,
            });
        }
        self.mb_frame = Some(MbFrame { crosses, span });
        let mut installed = Vec::new();
        let out = self.motion_blur_phases(active, doc, t, n, span, &phase, &live, &mut installed, list, report);
        self.mb_frame = None;
        self.mb_sample = None;
        self.mb_acc.clear();
        // Pinned textures and averages are this frame's only; they go back
        // to the pool once the next frame is under way.
        for g in pinned.into_iter().chain(installed) {
            if let Some(p) = active.raster[g].take() {
                self.mb_release.push(p.tex);
            }
        }
        out
    }

    /// The phases of a motion-blurred frame, in order. Returns the output.
    #[allow(clippy::too_many_arguments)]
    fn motion_blur_phases(
        &mut self,
        active: &mut Active,
        doc: &Document,
        t: f64,
        n: u32,
        span: f64,
        phase: &[u32],
        live: &[bool],
        installed: &mut Vec<usize>,
        list: &mut CommandList,
        report: &mut FrameReport,
    ) -> Result<Placed, RenderError> {
        let ng = active.plan.groups.len();
        let crosses = self.mb_frame.as_ref().map(|f| f.crosses.clone()).unwrap_or_default();
        let last = phase.iter().copied().max().unwrap_or(0);
        for p in 0..=last {
            if p % 2 == 0 {
                let only: Vec<bool> = (0..ng).map(|g| phase[g] == p && live[g]).collect();
                if only.iter().any(|&o| o) {
                    let pass = Pass {
                        only: Some(&only),
                        keep_bounds: p > 0,
                        ..Default::default()
                    };
                    self.encode_groups_masked(active, doc, t, list, report, pass)?;
                }
                continue;
            }
            // A wave: its groups per sample; those a later phase reads (and
            // the output) are averaged.
            self.mb_acc.clear();
            self.mb_acc.resize_with(ng, || None);
            for g in 0..ng {
                if phase[g] == p && (crosses[g] || g == ng - 1) {
                    self.mb_acc[g] = Some(MbAccum::default());
                }
            }
            // Every sample renders the wave's groups, including those not
            // reaching the output at that instant (a cut in the shutter): an
            // average is of the layer itself, and what reads it shows or
            // hides it in its own samples.
            let only: Vec<bool> = (0..ng).map(|g| phase[g] == p && live[g]).collect();
            for s in 0..n {
                let ts = t - span / 2.0 + span * (s as f64 + 0.5) / n as f64;
                self.mb_sample = Some((s, n, t, span));
                let pass = Pass {
                    only: Some(&only),
                    keep_bounds: true,
                    ..Default::default()
                };
                let r = self.encode_groups_masked(active, doc, ts, list, report, pass);
                self.mb_sample = None;
                r?;
            }
            for g in 0..ng {
                if let Some(acc) = self.mb_acc[g].take() {
                    self.mb_install(active, g, acc, n, t, list, installed);
                }
            }
        }
        Ok(placed_output(active, ng - 1))
    }

    /// Adds the output of sampled group `gi` (as its readers see it now) to
    /// its average: a running sum when frame-sized, else a copy of each
    /// sample, summed over their union once the wave is done.
    fn mb_collect(&mut self, gi: usize, out: Placed, bounds: ops::Rect, doc: &Document, list: &mut CommandList) {
        let Some((_, n, t, _)) = self.mb_sample else {
            return;
        };
        let Some(mut acc) = self.mb_acc.get_mut(gi).and_then(Option::take) else {
            return;
        };
        acc.bounds = [
            acc.bounds[0].min(bounds[0]),
            acc.bounds[1].min(bounds[1]),
            acc.bounds[2].max(bounds[2]),
            acc.bounds[3].max(bounds[3]),
        ];
        let frame = TextureDesc {
            width: self.view.w,
            height: self.view.h,
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        };
        // Large outputs (a frame-sized slot, particles all over) go into a
        // frame-sized running sum; small ones (a word) are copied and summed
        // over their union at the end.
        let d = out.tex.desc();
        let large = d.width as u64 * d.height as u64 * 4 >= frame.width as u64 * frame.height as u64;
        if acc.sum.is_some() || (large && acc.stash.is_empty()) {
            let first = acc.sum.is_none();
            let (sum, spare) = acc.sum.take().unwrap_or_else(|| {
                (
                    self.pool.lease(&self.device, frame),
                    self.pool.lease(&self.device, frame),
                )
            });
            let mut p = header(doc, t, self.view).to_vec();
            p.extend([out.origin[0], out.origin[1], 1.0 / n as f32, first as u8 as f32]);
            list.dispatch(&self.kernels.utility("k_accumulate"), &spare, &[&out.tex, &sum], &p);
            acc.sum = Some((spare, sum));
        } else {
            let tex = self.pool.lease(&self.device, d);
            let p = [t as f32, 1.0, d.width as f32, d.height as f32];
            list.dispatch(&self.kernels.utility("k_copy"), &tex, &[&out.tex], &p);
            acc.stash.push(Placed {
                tex,
                origin: out.origin,
                scale: out.scale,
            });
        }
        self.mb_acc[gi] = Some(acc);
    }

    /// Makes a wave's average of group `g` its output for the rest of the
    /// frame only (`installed`: next frame draws the group afresh, in a
    /// texture of its own size).
    #[allow(clippy::too_many_arguments)]
    fn mb_install(
        &mut self,
        active: &mut Active,
        g: usize,
        acc: MbAccum,
        n: u32,
        t: f64,
        list: &mut CommandList,
        installed: &mut Vec<usize>,
    ) {
        // What the averages were summed from goes straight back to the pool
        // (later work in this frame writes it only after these reads).
        let placed = if let Some((sum, spare)) = acc.sum {
            self.pool.give_back(spare);
            for s in acc.stash {
                self.pool.give_back(s.tex);
            }
            Placed {
                tex: sum,
                origin: [0.0, 0.0],
                scale: self.view.px,
            }
        } else if !acc.stash.is_empty() {
            let mut u = ops::EMPTY;
            for s in &acc.stash {
                let e = extent(s);
                u = [u[0].min(e[0]), u[1].min(e[1]), u[2].max(e[2]), u[3].max(e[3])];
            }
            // The union on the device grid (texels of the view's size).
            let px = self.view.px as f64;
            let (x0, y0) = ((u[0] / px).floor(), (u[1] / px).floor());
            let bucket = |v: f64| ((v.max(1.0) as u32).div_ceil(RASTER_BUCKET) * RASTER_BUCKET).max(RASTER_BUCKET);
            let desc = TextureDesc {
                width: bucket((u[2] / px).ceil() - x0),
                height: bucket((u[3] / px).ceil() - y0),
                format: PixelFormat::Rgba16Float,
                cpu_readable: false,
            };
            let (mut a, mut b) = (self.pool.lease(&self.device, desc), self.pool.lease(&self.device, desc));
            let k = self.kernels.utility("k_accumulate");
            for (i, s) in acc.stash.iter().enumerate() {
                let p = [
                    t as f32,
                    1.0,
                    desc.width as f32,
                    desc.height as f32,
                    s.origin[0] / px as f32 - x0 as f32,
                    s.origin[1] / px as f32 - y0 as f32,
                    1.0 / n as f32,
                    (i == 0) as u8 as f32,
                ];
                list.dispatch(&k, &b, &[&s.tex, &a], &p);
                std::mem::swap(&mut a, &mut b);
            }
            self.pool.give_back(b);
            for s in acc.stash {
                self.pool.give_back(s.tex);
            }
            Placed {
                tex: a,
                origin: [(x0 * px) as f32, (y0 * px) as f32],
                scale: px as f32,
            }
        } else {
            // Not shown during the samples: nothing to average.
            return;
        };
        let borrowed = std::mem::replace(&mut active.borrowed[g], false);
        if let Some(old) = active.raster[g].replace(placed)
            && !borrowed
        {
            self.mb_release.push(old.tex);
        }
        active.drawn[g] = 0;
        installed.push(g);
        active.alias[g] = None;
        active.bounds[g] = acc.bounds;
    }

    /// How far (px) the fastest moving layer travels over the shutter
    /// [t0, t1], which sets how many samples an adaptive motion blur takes.
    /// A vector layer is measured on its bounds as last drawn under its own
    /// placement and the transforms it absorbs; any other moving transform
    /// on the corners of what it moves (its source's bounds); both mapped
    /// into the frame by the transforms after them, and only while on
    /// screen. A transform over unbounded content is measured on the
    /// frame's corners, shapes and waves by their own measures. What it
    /// cannot measure (shaders, glyph animation) adds nothing: those take
    /// the format's `samples`.
    fn motion_extent_px(&self, plan: &Plan, doc: &Document, moving: &[bool], t0: f64, t1: f64) -> f64 {
        let (w, h) = (doc.format.width as f64, doc.format.height as f64);
        let tm = (t0 + t1) / 2.0;
        let n = plan.nodes.len();
        let affine_of = |i: usize, t: f64| {
            let pn = &plan.nodes[i];
            ops::transform_affine(&doc.nodes[&pn.id], &doc.format, local_time(pn, t).0)
        };
        // Where each node's output lands in the frame: the transforms after
        // it (along its last consumer), at the frame's time.
        let mut down: Vec<Option<ops::Affine>> = vec![None; n];
        if n > 0 {
            down[n - 1] = Some(ops::Affine::IDENTITY);
        }
        for i in (0..n).rev() {
            let Some(d) = down[i] else { continue };
            let pn = &plan.nodes[i];
            let d = if pn.def.name == "transform" && local_time(pn, tm).1 {
                d.mul(affine_of(i, tm))
            } else {
                d
            };
            for &c in &pn.inputs {
                down[c].get_or_insert(d);
            }
        }
        let down = |i: usize| down[i].unwrap_or(ops::Affine::IDENTITY);
        let corners = |b: ops::Rect| [(b[0], b[1]), (b[2], b[1]), (b[0], b[3]), (b[2], b[3])];
        let bbox = |pts: [(f64, f64); 4]| {
            pts.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |r, &(x, y)| {
                [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)]
            })
        };
        // The part of `b` (in the space `f` maps into the frame) that the
        // frame shows, if any.
        let visible = |b: ops::Rect, f: ops::Affine| -> Option<ops::Rect> {
            let back = f.inverse()?;
            let fr = bbox(corners([0.0, 0.0, w, h]).map(|(x, y)| back.apply(x, y)));
            let r = [b[0].max(fr[0]), b[1].max(fr[1]), b[2].min(fr[2]), b[3].min(fr[3])];
            (r[0] < r[2] && r[1] < r[3] && {
                let s = bbox(corners(r).map(|(x, y)| f.apply(x, y)));
                s[2] > 0.0 && s[3] > 0.0 && s[0] < w && s[1] < h
            })
            .then_some(r)
        };
        let mut chained = vec![false; n];
        let mut most = 0.0f64;
        for g in &plan.groups {
            let Some(rc) = g.raster.as_ref() else { continue };
            for &ti in &rc.transforms {
                chained[ti] = true;
            }
            if !moving[rc.node] && !rc.transforms.iter().any(|&ti| moving[ti]) {
                continue;
            }
            let rn = &plan.nodes[rc.node];
            let Some(b) = self.bounds.get(&rn.id) else { continue };
            let b = [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64];
            // Its bounds are in its chain's output space: the frame, unless
            // transforms follow it.
            let d = down(*rc.transforms.last().unwrap_or(&rc.node));
            let Some(b) = visible(b, d) else { continue };
            let node = &doc.nodes[&rn.id];
            let full = |t: f64| {
                let outer = rc
                    .transforms
                    .iter()
                    .fold(ops::Affine::IDENTITY, |acc, &ti| affine_of(ti, t).mul(acc));
                outer.mul(geom::placement_affine(node, local_time(rn, t).0))
            };
            let (f0, fm, f1) = (full(t0), full(tm), full(t1));
            // (A layer at zero scale is nowhere: its growth shows in the
            // other pairs.)
            for (a, z) in [(f0, f1), (f0, fm), (fm, f1)] {
                let Some(back) = a.inverse() else { continue };
                let m = z.mul(back);
                for (x, y) in corners(b) {
                    let (u, v) = m.apply(x, y);
                    let ((x, y), (u, v)) = (d.apply(x, y), d.apply(u, v));
                    most = most.max((u - x).hypot(v - y));
                }
            }
        }
        // Bounds of each node's output in its own space, at the frame's time.
        let mut local = vec![ops::EMPTY; n];
        for (i, pn) in plan.nodes.iter().enumerate() {
            let ins: Vec<ops::Rect> = pn.inputs.iter().map(|&c| local[c]).collect();
            let (lt, on) = local_time(pn, tm);
            local[i] = if on {
                ops::bounds(pn.def, &doc.nodes[&pn.id], &doc.format, lt, &ins, None)
            } else {
                ops::passthrough(pn.def, ins.len())
                    .map(|k| ins[k])
                    .unwrap_or(ops::EMPTY)
            };
        }
        for (i, pn) in plan.nodes.iter().enumerate() {
            if !moving[i] || chained[i] || !matches!(pn.def.name, "transform" | "shape" | "wave") {
                continue;
            }
            let (l0, lm, l1) = (local_time(pn, t0).0, local_time(pn, tm).0, local_time(pn, t1).0);
            let node = &doc.nodes[&pn.id];
            let b = pn.inputs.first().map(|&s| local[s]).unwrap_or(ops::UNBOUNDED);
            if pn.def.name == "transform" && b[0] > -1e8 && b[2] < 1e8 {
                if b[0] > b[2] || b[1] > b[3] {
                    continue;
                }
                // What it moves, where the frame shows it, at each time.
                let d = down(i);
                let at = |t: f64| d.mul(affine_of(i, t));
                let (f0, fm, f1) = (at(t0), at(tm), at(t1));
                let Some(b) = visible(b, fm) else { continue };
                for (a, z) in [(f0, f1), (f0, fm), (fm, f1)] {
                    for (x, y) in corners(b) {
                        let ((x0, y0), (x1, y1)) = (a.apply(x, y), z.apply(x, y));
                        most = most.max((x1 - x0).hypot(y1 - y0));
                    }
                }
                continue;
            }
            let f = |a: f64, b: f64| match pn.def.name {
                "transform" => transform_motion_px(node, &doc.format, a, b),
                "shape" => ops::shape_motion_px(node, &doc.format, a, b),
                _ => ops::wave_motion_px(node, a, b),
            };
            most = most.max(f(l0, l1).max(f(l0, lm)).max(f(lm, l1)));
        }
        most
    }

    /// Plan nodes whose image moves during [t0, t1], which motion blur
    /// samples: params animating to different values, appearing or
    /// vanishing, particles and simulations. Time-driven content holds at
    /// the frame's time instead (footage and image-sequence frames, shader
    /// clocks: a camera already blurred its footage, and flicker or noise
    /// would average away), and so do changes that move nothing on screen
    /// (colors, opacities, grades; a transform moving content less than
    /// half a pixel; a 3D scene moving less than 0.75 px). Overlay textures
    /// (grain) count as still.
    fn moving_nodes(&self, plan: &Plan, doc: &Document, t0: f64, t1: f64) -> Vec<bool> {
        let mut moving = vec![false; plan.nodes.len()];
        for (i, pn) in plan.nodes.iter().enumerate() {
            if pn.is_static || plan.overlay[i] {
                continue;
            }
            // Simulations step on the first sample (as seeks replay them),
            // so they stay with the sampled groups.
            if pn.def.form == ops::Form::Sim {
                moving[i] = true;
                continue;
            }
            let (l0, on0) = local_time(pn, t0);
            let (l1, on1) = local_time(pn, t1);
            if on0 != on1 {
                moving[i] = true;
                continue;
            }
            if !on0 {
                continue;
            }
            // A particle system blurs itself along its particles' paths
            // (drawn once); a swarm is sampled.
            if pn.def.name == "swarm" {
                moving[i] = true;
                continue;
            }
            if pn.def.name == "particles" {
                continue;
            }
            let node = &doc.nodes[&pn.id];
            // Also the middle of the shutter: what turns back within it (a
            // key at the frame's time) ends where it began.
            let lm = local_time(pn, (t0 + t1) / 2.0).0;
            let varies = |p: &ultrafast_graph::Param| {
                p.is_animated() && {
                    let v0 = p.eval(l0);
                    v0 != p.eval(l1) || v0 != p.eval(lm)
                }
            };
            // A shader is an effect: held at the frame's time (its clock and
            // its u values: flicker, pulses, glitches stay crisp) unless it
            // asks to be sampled.
            if pn.def.name == "shader" {
                moving[i] =
                    ops::samples_motion(node) && (ops::time_varying(pn.def, node) || node.params.values().any(varies));
                continue;
            }
            // A counter's number or decoding text changes glyphs, not position.
            let changed: Vec<&str> = node
                .params
                .iter()
                .filter(|(k, p)| {
                    *k != "number" && *k != "scramble" && !schema::moves_nothing(pn.def.name, k) && varies(p)
                })
                .map(|(k, _)| k.as_str())
                .collect();
            // Moving less than 1.5 px over the shutter blurs nothing anyone
            // can see (a 3D scene renders once per 1.5 px too).
            // The most it moves between the shutter's ends and its middle.
            let most = |f: &dyn Fn(f64, f64) -> f64| f(l0, l1).max(f(l0, lm)).max(f(lm, l1));
            moving[i] = match pn.def.name {
                _ if changed.is_empty() => false,
                "transform" => most(&|a, b| transform_motion_px(node, &doc.format, a, b)) >= 1.5,
                "shape" if changed.iter().all(|k| ops::SHAPE_PLACEMENT.contains(k)) => {
                    most(&|a, b| ops::shape_motion_px(node, &doc.format, a, b)) >= 1.5
                }
                "wave" if changed.iter().all(|k| matches!(*k, "amplitude" | "phase")) => {
                    most(&|a, b| ops::wave_motion_px(node, a, b)) >= 1.5
                }
                _ => true,
            };
        }
        for g in &plan.groups {
            if g.kind == GroupKind::Render3D
                && g.nodes.iter().any(|&m| moving[m])
                && self.scene_motion_px(plan, g, doc, t0, t1) < 0.75
            {
                for &m in &g.nodes {
                    moving[m] = false;
                }
            }
        }
        // A simulation steps on the first sample, reading what it reads at
        // that instant (as a seek's catch-up does): that moves with it.
        for (gi, g) in plan.groups.iter().enumerate() {
            if g.kind == GroupKind::Sim {
                for (i, pn) in plan.nodes.iter().enumerate() {
                    if !pn.is_static && plan::bit(&plan.group_up[gi], i) {
                        moving[i] = true;
                    }
                }
            }
        }
        moving
    }

    /// Encodes every group; returns the placed output of the last one.
    fn encode_groups(
        &mut self,
        active: &mut Active,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
        report: &mut FrameReport,
    ) -> Result<Placed, RenderError> {
        self.encode_groups_masked(active, doc, t, list, report, Pass::default())
    }

    /// Like [`Renderer::encode_groups`], running the groups `pass` picks
    /// and returning its target's output.
    fn encode_groups_masked(
        &mut self,
        active: &mut Active,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
        report: &mut FrameReport,
        pass: Pass,
    ) -> Result<Placed, RenderError> {
        let Pass {
            only,
            target,
            changed,
            keep_bounds,
        } = pass;
        let Active {
            plan,
            slots,
            resident,
            slot_used,
            slot_groups,
            slot_desc,
            view: _,
            group_used,
            cache,
            raster,
            drawn,
            alias,
            borrowed,
            bounds,
            low_readers,
        } = active;
        self.ran.clear();
        self.ran.resize(plan.groups.len(), false);
        for &g in changed {
            self.ran[g] = true;
            alias[g] = None;
        }
        // Work that cannot reach the output at this time (layers outside their
        // windows, and everything only they use) is skipped.
        let live = match only {
            Some(m) => m.to_vec(),
            None => live_groups(plan, t),
        };
        // Live groups' slots get textures before anything reads them.
        for (gi, g) in plan.groups.iter().enumerate() {
            if !live[gi] {
                continue;
            }
            group_used[gi] = self.frame_no;
            let s = g.output;
            if s >= slots.len() {
                continue;
            }
            slot_used[s] = self.frame_no;
            if !resident[s] {
                slots[s] = self.pool.lease(&self.device, *slot_desc);
                resident[s] = true;
                for &w in &slot_groups[s] {
                    cache[w] = None;
                }
            }
        }
        let px = self.view.px;
        let placed_of = |raster: &Vec<Option<Placed>>, alias: &Vec<Option<usize>>, mut gi: usize| -> Placed {
            while let Some(src) = alias[gi] {
                gi = src;
            }
            match &raster[gi] {
                Some(p) => p.clone(),
                None => Placed {
                    tex: slots[plan.groups[gi].output].clone(),
                    origin: [0.0, 0.0],
                    scale: px,
                },
            }
        };
        let last = target.unwrap_or(plan.groups.len() - 1);

        let frame_rect = [0.0, 0.0, doc.format.width as f64, doc.format.height as f64];
        // Where each group's output can be non-transparent (frame px).
        if !keep_bounds || bounds.len() != plan.groups.len() {
            bounds.clear();
            bounds.resize(plan.groups.len(), ops::UNBOUNDED);
        }
        let content = bounds;
        let collect = self.mb_sample.is_some() && !self.mb_acc.is_empty();
        for (gi, g) in plan.groups.iter().enumerate() {
            // Motion blur: the group before, as its readers see it, joins
            // its average (before anything later can write its slot).
            if collect && gi > 0 && live[gi - 1] && self.mb_acc[gi - 1].is_some() {
                let out = placed_of(raster, alias, gi - 1);
                self.mb_collect(gi - 1, out, content[gi - 1], doc, list);
            }
            if !live[gi] {
                report.groups_skipped += 1;
                continue;
            }
            content[gi] = ops::UNBOUNDED;
            // Resolve input textures (placed) and resource sizes.
            let mut inputs: Vec<Placed> = Vec::with_capacity(g.textures.len());
            let mut res_info: HashMap<&str, ResourceInfo> = HashMap::new();
            for ts in &g.textures {
                match ts {
                    TexSource::Group(s) => inputs.push(placed_of(raster, alias, *s)),
                    TexSource::Resource(src) => {
                        let user = g.nodes.iter().map(|&n| &plan.nodes[n]).find(|n| n.variant == *src);
                        // Under motion blur, footage and image sequences hold
                        // the frame's own picture across the shutter.
                        let timed = src.starts_with("video:") || src.contains("{frame");
                        let at = match self.mb_sample {
                            Some((_, _, tc, _)) if timed => tc,
                            _ => t,
                        };
                        let (user_id, user_t, user_on) = match user {
                            Some(n) => (n.id.clone(), local_time(n, at).0, local_time(n, t).1),
                            None => (String::new(), at, true),
                        };
                        // A media node outside its window in a fused group
                        // that is live for its other layers: nothing samples
                        // it, so do not decode (or seek) it.
                        if !user_on && (src.starts_with("video:") || src.contains("{frame")) {
                            inputs.push(Placed {
                                tex: self.placeholder.clone(),
                                origin: [0.0, 0.0],
                                scale: 1.0,
                            });
                            continue;
                        }
                        let (tex, info) = self.resource(src, &user_id, doc, user_t, list);
                        if let Some(i) = info {
                            res_info.insert(src.as_str(), i);
                        }
                        inputs.push(Placed {
                            tex,
                            origin: [0.0, 0.0],
                            scale: 1.0,
                        });
                    }
                }
            }

            // Pack params: header, then the group's layout (node blocks,
            // literal values, padding, texture origins), then the blocks of
            // the run layers showing now.
            self.scratch.clear();
            self.scratch.extend(header(doc, t, self.view));
            let mut run_headers: Vec<(usize, usize)> = Vec::new();
            for slot in &g.params {
                match slot {
                    plan::ParamSlot::Node(ni) => {
                        let pn = &plan.nodes[*ni];
                        let info = if ops::uses_resource(pn.def) {
                            res_info.get(pn.variant.as_str()).copied()
                        } else {
                            None
                        };
                        let (lt, on) = local_time(pn, t);
                        // A static group is time-invariant: pack it at one
                        // fixed time so its params compare equal from frame
                        // to frame (a shader that never reads time() still
                        // receives it).
                        let lt = if g.is_static { 0.0 } else { lt };
                        ops::pack(
                            pn.def,
                            &pn.id,
                            &doc.nodes[&pn.id],
                            &doc.format,
                            lt,
                            info,
                            &mut self.scratch,
                        )?;
                        self.hold_shader_clock(pn, &doc.nodes[&pn.id], g.is_static);
                        if pn.gated {
                            self.scratch.push(on as u8 as f32);
                        }
                        if ops::guarded(pn.def) {
                            let b = subtree_bounds(plan, g, pn.inputs[0], doc, t, &inputs, &res_info, &content);
                            self.scratch.extend(b.map(|v| v as f32));
                        }
                        if let Some(tables) = &pn.tables {
                            self.scratch.extend_from_slice(tables);
                        }
                    }
                    plan::ParamSlot::Run(r) => {
                        run_headers.push((*r, self.scratch.len()));
                        self.scratch.extend([0.0, 0.0]);
                    }
                    plan::ParamSlot::Value(v) => self.scratch.push(*v),
                    plan::ParamSlot::Pad(n) => self.scratch.extend(std::iter::repeat_n(0.0, *n)),
                    plan::ParamSlot::Origins => {
                        for p in &inputs {
                            self.scratch.extend(p.origin);
                            self.scratch.push(p.scale);
                        }
                    }
                }
            }
            for (r, at) in run_headers {
                let run = &g.runs[r];
                let start = self.scratch.len();
                let mut shown = 0;
                for (kind, ci, layer) in &run.layers {
                    let pn = &plan.nodes[*ci];
                    let (lt, on) = local_time(pn, t);
                    // Out of its window, or nothing to lay: the background
                    // shows through, as it would without the layer.
                    if !on {
                        continue;
                    }
                    let b = subtree_bounds(plan, g, pn.inputs[0], doc, t, &inputs, &res_info, &content);
                    if b[0] >= b[2] || b[1] >= b[3] {
                        continue;
                    }
                    let lt = if g.is_static { 0.0 } else { lt };
                    let block = self.scratch.len();
                    self.scratch.push(*kind as f32);
                    ops::pack(
                        pn.def,
                        &pn.id,
                        &doc.nodes[&pn.id],
                        &doc.format,
                        lt,
                        None,
                        &mut self.scratch,
                    )?;
                    self.hold_shader_clock(pn, &doc.nodes[&pn.id], g.is_static);
                    self.scratch.extend(b.map(|v| v as f32));
                    match layer {
                        codegen::LayerRef::Texture(binding) => {
                            let d = inputs[*binding].tex.desc();
                            self.scratch.extend([*binding as f32, d.width as f32, d.height as f32]);
                        }
                        codegen::LayerRef::Gen(gi) => {
                            let gn = &plan.nodes[*gi];
                            let (glt, _) = local_time(gn, t);
                            let glt = if g.is_static { 0.0 } else { glt };
                            ops::pack(
                                gn.def,
                                &gn.id,
                                &doc.nodes[&gn.id],
                                &doc.format,
                                glt,
                                None,
                                &mut self.scratch,
                            )?;
                            self.hold_shader_clock(gn, &doc.nodes[&gn.id], g.is_static);
                        }
                    }
                    self.scratch.resize(block + run.stride, 0.0);
                    shown += 1;
                }
                self.scratch[at] = start as f32;
                self.scratch[at + 1] = shown as f32;
            }
            match g.kind {
                GroupKind::Fused => {
                    content[gi] = intersect(
                        subtree_bounds(plan, g, g.root, doc, t, &inputs, &res_info, &content),
                        frame_rect,
                    )
                }
                GroupKind::Render3D | GroupKind::Sim => content[gi] = frame_rect,
                _ => {}
            }

            // A fused group that at this time only passes an input through
            // (its layers are out of their windows, or what it composites
            // over is empty here) shows that input as it is.
            alias[gi] = None;
            if g.kind == GroupKind::Fused && gi != last && !self.no_skip {
                let crossing = self.mb_frame.as_ref().is_some_and(|f| f.crosses[gi]);
                alias[gi] = passthrough_group(plan, g, g.root, doc, t, &inputs, &res_info, &content).filter(|&src| {
                    // Only while nothing reuses the source's slot before the
                    // last group that reads this one (with motion blur, a
                    // later phase reads it: only a texture of its own).
                    let mut f = src;
                    while let Some(n) = alias[f] {
                        f = n;
                    }
                    if let Some(p) = &raster[f] {
                        // A blur kept at reduced resolution is only for its own
                        // readers' kernels.
                        return p.scale == px;
                    }
                    if crossing {
                        return false;
                    }
                    let slot = plan.groups[f].output;
                    let last_use = (gi + 1..plan.groups.len())
                        .filter(|&k| plan.groups[k].textures.contains(&TexSource::Group(gi)))
                        .max()
                        .unwrap_or(gi);
                    !(gi + 1..=last_use).any(|k| plan.groups[k].output == slot)
                });
                if let Some(src) = alias[gi] {
                    if std::env::var_os("ULTRAFAST_DEBUG_ALIAS").is_some() {
                        eprintln!("t={t:.3} g{gi} -> g{src} (last g{last})");
                    }
                    content[gi] = content[src];
                    self.ran[gi] = self.ran[src];
                    cache[gi] = None;
                    report.groups_cached += 1;
                    continue;
                }
            }

            // Raster geometry identity: node params, absorbed transforms, time.
            // A raster chain shows while its raster node is active; a timed
            // transform outside its window passes it through unmoved.
            let raster_geo = g.raster.as_ref().map(|rc| {
                // Built on the device grid: the view's density times the
                // transforms the raster absorbed (curves flattened and glyphs
                // drawn for that size).
                let d = 1.0 / px as f64;
                let outer = ops::Affine::scale(d, d).mul(absorbed(plan, doc, &rc.transforms, t));
                let rn = &plan.nodes[rc.node];
                let (lt, on) = local_time(rn, t);
                let key = if on {
                    geom::geometry_key(&doc.nodes[&rn.id], lt, &outer)
                } else {
                    0
                };
                (rc.node, outer, key, lt, on)
            });

            // Static groups rerun only when params, inputs or upstream changed.
            // The time/frame header is excluded: static means time-invariant.
            // (Only static groups own their output slot across frames, so
            // only they can be reused this way.)
            if g.is_static {
                let mut ids: Vec<usize> = inputs.iter().map(|p| p.tex.id()).collect();
                if let Some((_, _, key, _, _)) = raster_geo {
                    ids.push(key as usize);
                }
                if g.kind == GroupKind::Render3D {
                    ids.push(scene_key(plan, g, doc, t) as usize);
                }
                let upstream_ran = g
                    .textures
                    .iter()
                    .any(|ts| matches!(ts, TexSource::Group(s) if self.ran[*s]));
                if std::env::var_os("ULTRAFAST_DEBUG_CACHE").is_some() {
                    let why = match &cache[gi] {
                        _ if upstream_ran => {
                            let ups: Vec<usize> = g
                                .textures
                                .iter()
                                .filter_map(|ts| match ts {
                                    TexSource::Group(s) if self.ran[*s] => Some(*s),
                                    _ => None,
                                })
                                .collect();
                            format!("upstream ran: {ups:?}")
                        }
                        None => "first run".to_string(),
                        Some((p, _)) if p[..] != self.scratch[2..] => {
                            let k = p.iter().zip(&self.scratch[2..]).position(|(a, b)| a != b);
                            format!("params differ at {k:?} (len {} vs {})", p.len(), self.scratch.len() - 2)
                        }
                        Some((_, i)) if *i != ids => format!("inputs differ: {i:?} vs {ids:?}"),
                        _ => String::new(),
                    };
                    if !why.is_empty() {
                        eprintln!("t={t:.3} g{gi} reruns: {why}");
                    }
                }
                if !upstream_ran
                    && let Some((p, i)) = &cache[gi]
                    && p[..] == self.scratch[2..]
                    && *i == ids
                {
                    report.groups_cached += 1;
                    if matches!(g.kind, GroupKind::Raster | GroupKind::Blur) {
                        content[gi] = extent(&placed_of(raster, alias, gi));
                    }
                    continue;
                }
                cache[gi] = Some((self.scratch[2..].to_vec(), ids));
            }

            let mut in_tex: Vec<&Texture> = inputs.iter().map(|p| &p.tex).collect();
            if g.pad_textures {
                in_tex.resize(plan::MAX_TEXTURES, &self.placeholder);
            }
            match g.kind {
                GroupKind::Raster => {
                    let (rn, outer, key, lt, on) = raster_geo.expect("raster group");
                    let framed = plan.framed[gi] && !self.no_framed;
                    // The same geometry in the same paint as last time: its
                    // texture still holds it (text that has settled, shapes
                    // between keys, motion-blur samples of still layers).
                    let paint = {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        (key, on, framed).hash(&mut h);
                        for v in &self.scratch[4..9] {
                            v.to_bits().hash(&mut h);
                        }
                        h.finish() | 1
                    };
                    if drawn[gi] == paint
                        && !self.no_skip
                        && let Some(p) = &raster[gi]
                    {
                        content[gi] = extent(p);
                        report.groups_cached += 1;
                        continue;
                    }
                    drawn[gi] = paint;
                    let geo = if on {
                        self.geometry(&plan.nodes[rn].id, doc, lt, outer, key)?
                    } else {
                        Arc::new(geom::Geometry::default())
                    };
                    let prev = raster[gi].take();
                    // Color glyphs keep their own colors and fade with the
                    // node's opacity only.
                    let opacity = doc.nodes[&plan.nodes[rn].id]
                        .params
                        .get("opacity")
                        .and_then(|p| p.eval(lt))
                        .and_then(|v| v.first().copied())
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0) as f32;
                    let placed = self.encode_raster(&geo, prev, opacity, framed, doc, t, list);
                    content[gi] = extent(&placed);
                    raster[gi] = Some(placed);
                    report.kernels_run += 2;
                }
                GroupKind::Particles => {
                    let prev = raster[gi].take();
                    let sprite = inputs.first().cloned();
                    let placed = self.encode_particles(plan, g, sprite, prev, doc, t, list)?;
                    content[gi] = extent(&placed);
                    raster[gi] = Some(placed);
                    report.kernels_run += 1;
                }
                GroupKind::Render3D => {
                    let out = slots[g.output].clone();
                    // Motion blur: a 3D scene is rendered only as often as
                    // it moves (about every 1.5 px on screen during the
                    // shutter); samples in between reuse the last render.
                    let plan_mb = self.mb_sample.filter(|_| !self.mb_full).map(|(s, n, tc, span)| {
                        let m = self.scene_motion_px(plan, g, doc, tc - span / 2.0, tc + span / 2.0) / px;
                        if s == 0 && std::env::var("ULTRAFAST_DEBUG_MB").is_ok() {
                            eprintln!("t={tc:.2} group {gi}: 3D moves {m:.2} px over the shutter");
                        }
                        let k = if m.is_finite() {
                            ((m / 1.5).ceil() as u32).clamp(1, n)
                        } else {
                            n
                        };
                        let bucket = |s: u32| s * k / n;
                        let first = s == 0 || bucket(s) != bucket(s - 1);
                        let at = tc - span / 2.0 + span * (bucket(s) as f64 + 0.5) / k as f64;
                        (first, at)
                    });
                    match plan_mb {
                        Some((false, _)) => report.groups_cached += 1,
                        Some((true, at)) => {
                            self.encode_render3d(plan, g, &inputs, &out, doc, at, list)?;
                            report.kernels_run += 1;
                        }
                        None => {
                            self.encode_render3d(plan, g, &inputs, &out, doc, t, list)?;
                            report.kernels_run += 1;
                        }
                    }
                }
                GroupKind::Sim => {
                    let out = slots[g.output].clone();
                    self.encode_sim(plan, gi, &inputs, &out, doc, t, list, report)?;
                    report.kernels_run += 1;
                }
                GroupKind::Blur => {
                    // Blur only the region the source covers (plus the blur's
                    // reach), at reduced resolution for large radii.
                    let src = inputs[0].clone();
                    let src_bounds = match g.textures[0] {
                        TexSource::Group(s) => intersect(extent(&src), content[s]),
                        _ => extent(&src),
                    };
                    // A texture this group borrowed is not its own to reuse.
                    let prev = raster[gi]
                        .take()
                        .filter(|_| !std::mem::replace(&mut borrowed[gi], false));
                    if local_time(&plan.nodes[g.root], t).1 {
                        // Kept at its reduced resolution when every reader can
                        // sample it there and has its kernel (not while motion
                        // blur averages it, nor for a fallback reader).
                        let keep_low = self.mb_sample.is_none()
                            && low_readers[gi].as_ref().is_some_and(|rs| {
                                rs.iter().all(|&c| {
                                    matches!(self.kernels.get(&plan.groups[c].key), Some(KernelState::Ready(_)))
                                })
                            });
                        let (placed, b) =
                            self.encode_blur(&src, src_bounds, self.scratch[4], prev, keep_low, doc, t, list);
                        raster[gi] = Some(placed);
                        content[gi] = b;
                        report.kernels_run += 2;
                    } else {
                        // Inactive: the source passes through untouched. Shown
                        // as it is when nothing writes its slot before the last
                        // reader of this group; otherwise copied, since a
                        // later group would reuse that slot first.
                        let slot = slots.iter().position(|s| s.id() == src.tex.id());
                        let last_use = (gi + 1..plan.groups.len())
                            .filter(|&k| plan.groups[k].textures.contains(&TexSource::Group(gi)))
                            .max()
                            .unwrap_or(gi);
                        let crossing = self.mb_frame.as_ref().is_some_and(|f| f.crosses[gi]);
                        let clobbered =
                            slot.is_some_and(|sl| crossing || (gi + 1..=last_use).any(|k| plan.groups[k].output == sl));
                        if clobbered {
                            let desc = src.tex.desc();
                            let tex = match prev {
                                Some(p) if p.tex.desc() == desc => p.tex,
                                other => {
                                    if let Some(p) = other {
                                        self.pool.give_back(p.tex);
                                    }
                                    self.pool.lease(&self.device, desc)
                                }
                            };
                            list.dispatch(
                                &self.kernels.utility("k_copy"),
                                &tex,
                                &[&src.tex],
                                &header(doc, t, self.view),
                            );
                            raster[gi] = Some(Placed {
                                tex,
                                origin: src.origin,
                                scale: src.scale,
                            });
                        } else {
                            if let Some(p) = prev {
                                self.pool.give_back(p.tex);
                            }
                            raster[gi] = Some(src);
                            borrowed[gi] = true;
                        }
                        content[gi] = src_bounds;
                    }
                }
                GroupKind::Fused => {
                    // A fused group pinned for this frame (motion blur) renders
                    // into its own texture instead of its shared slot.
                    let out = &match &raster[gi] {
                        Some(p) => p.tex.clone(),
                        None => slots[g.output].clone(),
                    };
                    let op_by_op = || {
                        g.nodes
                            .iter()
                            .all(|&n| self.kernels.tier0(plan.nodes[n].def.func).is_some())
                    };
                    let state = match self.mode {
                        Mode::Exact if self.wait_fused || !op_by_op() => self.kernels.wait(&g.key),
                        _ => self.kernels.get(&g.key).unwrap_or(KernelState::Pending),
                    };
                    let scale = ops::render_scale(&doc.nodes[&plan.nodes[g.root].id]);
                    match state {
                        KernelState::Ready(k) if scale.is_some() => {
                            // Reduced resolution: render coarse texels, then
                            // upscale into the slot.
                            let r = scale.unwrap() as f32;
                            let v = self.view;
                            let low = TextureDesc {
                                width: ((v.w as f32 * r).ceil() as u32).max(1),
                                height: ((v.h as f32 * r).ceil() as u32).max(1),
                                format: PixelFormat::Rgba16Float,
                                cpu_readable: false,
                            };
                            let tmp = self.pool.lease(&self.device, low);
                            self.scratch[1] = v.px / r;
                            list.dispatch(&k, &tmp, &in_tex, &self.scratch);
                            self.scratch[1] = v.px;
                            let mut p = header(doc, t, v).to_vec();
                            p.extend([v.px, 0.0, 0.0, 0.0, 0.0, v.px / r, 1.0]);
                            list.dispatch(&self.kernels.utility("k_resample"), out, &[&tmp], &p);
                            self.pool.give_back(tmp);
                            report.kernels_run += 2;
                        }
                        KernelState::Ready(k) => {
                            list.dispatch(&k, out, &in_tex, &self.scratch);
                            report.kernels_run += 1;
                        }
                        KernelState::Pending => {
                            self.encode_fallback(plan, gi, &inputs, out, doc, t, list, &res_info)?;
                            report.groups_fallback += 1;
                            // A fallback result is not final: never cache it.
                            cache[gi] = None;
                        }
                        KernelState::Failed(e) => {
                            self.diagnose(&plan.nodes[g.root].id.clone(), e);
                            list.dispatch(&self.kernels.utility("k_clear"), out, &[], &header(doc, t, self.view));
                        }
                    }
                }
            }
            self.ran[gi] = true;
            if self.profile.is_some() {
                let ids: Vec<&str> = g.nodes.iter().map(|&n| plan.nodes[n].id.as_str()).collect();
                let names = if ids.len() > 4 {
                    format!("{} +{}", ids[..3].join(" "), ids.len() - 3)
                } else {
                    ids.join(" ")
                };
                let label = format!("g{gi} {:?} [{names}]", g.kind);
                let sub = std::mem::replace(list, self.device.begin()).submit();
                // Timed now: a backend may reuse the submission's slot (and
                // its timestamps) before a later read.
                let ms = sub.wait()?.gpu_ms;
                if let Some(p) = &mut self.profile {
                    let e = p.totals.entry(label).or_insert((0.0, 0));
                    e.0 += ms;
                    e.1 += 1;
                }
            }
        }
        let gl = plan.groups.len();
        if collect && gl > 0 && live[gl - 1] && self.mb_acc[gl - 1].is_some() {
            let out = placed_of(raster, alias, gl - 1);
            self.mb_collect(gl - 1, out, content[gl - 1], doc, list);
        }
        Ok(placed_of(raster, alias, last))
    }

    /// Under motion blur a shader's clock reads the frame's time, not the
    /// sample's (its last packed value, right after `ops::pack`), unless the
    /// shader asks to be sampled.
    fn hold_shader_clock(&mut self, pn: &plan::PlanNode, node: &ultrafast_graph::Node, is_static: bool) {
        if pn.def.name == "shader"
            && !ops::samples_motion(node)
            && let Some((_, _, tc, _)) = self.mb_sample
            && let Some(v) = self.scratch.last_mut()
        {
            *v = if is_static { 0.0 } else { local_time(pn, tc).0 as f32 };
        }
    }

    /// Largest on-screen movement (frame px) of any mesh's bounds between
    /// t0 and t1; infinite when it cannot tell (meshes not built yet,
    /// visibility changing, corners behind the camera).
    fn scene_motion_px(&self, plan: &Plan, g: &plan::Group, doc: &Document, t0: f64, t1: f64) -> f32 {
        use scene3d::{Mat4, Params};
        let Some(three) = self.three.as_ref() else {
            return f32::INFINITY;
        };
        let (fw, fh) = (doc.format.width as f32, doc.format.height as f32);
        let project = |t: f64| -> Option<Vec<[f32; 2]>> {
            let (mut eye, mut target, mut up) = ([0.0f32, 0.0, 5.0], [0.0f32, 0.0, 0.0], [0.0f32, 1.0, 0.0]);
            let (mut fov, mut near, mut far) = (45.0f32, 0.1f32, 1000.0f32);
            for &o in &g.nodes[..g.nodes.len() - 1] {
                let pn = &plan.nodes[o];
                let (lt, on) = local_time(pn, t);
                if pn.def.name != "camera" || !on {
                    continue;
                }
                let node = &doc.nodes[&pn.id];
                let p = Params { node, t: lt };
                target = p.vec("target", target);
                up = p.vec("up", up);
                fov = p.num("fov", fov);
                near = p.num("near", near);
                far = p.num("far", far);
                eye = if pn_has(node, "orbit") {
                    let [az, el, dist] = p.vec("orbit", [0.0, 0.0, 5.0]);
                    let (az, el) = (az.to_radians(), el.to_radians());
                    [
                        target[0] + dist * el.cos() * az.sin(),
                        target[1] + dist * el.sin(),
                        target[2] + dist * el.cos() * az.cos(),
                    ]
                } else {
                    p.vec("position", eye)
                };
            }
            let vp = Mat4::perspective(fov, fw / fh.max(1.0), near, far).mul(&Mat4::look_at(eye, target, up));
            let mut pts = Vec::new();
            for &o in &g.nodes[..g.nodes.len() - 1] {
                let pn = &plan.nodes[o];
                if pn.def.name != "mesh" {
                    continue;
                }
                let (lt, on) = local_time(pn, t);
                if !on {
                    // Appearing or vanishing within the shutter is motion.
                    pts.push([-1e6, -1e6]);
                    continue;
                }
                let node = &doc.nodes[&pn.id];
                let [lo, hi] = three.meshes.get(&scene3d::mesh_key(node))?.3;
                let m = Params { node, t: lt }.model().0;
                let v = &vp.0;
                for corner in 0..8 {
                    let c = [
                        if corner & 1 == 0 { lo[0] } else { hi[0] },
                        if corner & 2 == 0 { lo[1] } else { hi[1] },
                        if corner & 4 == 0 { lo[2] } else { hi[2] },
                    ];
                    let w = [
                        m[0] * c[0] + m[1] * c[1] + m[2] * c[2] + m[3],
                        m[4] * c[0] + m[5] * c[1] + m[6] * c[2] + m[7],
                        m[8] * c[0] + m[9] * c[1] + m[10] * c[2] + m[11],
                        1.0,
                    ];
                    let clip: Vec<f32> = (0..4).map(|r| (0..4).map(|k| v[r * 4 + k] * w[k]).sum()).collect();
                    // Corners behind the camera (a ground plane) say nothing.
                    if clip[3] <= 1e-3 {
                        pts.push([f32::NAN, f32::NAN]);
                        continue;
                    }
                    pts.push([
                        (clip[0] / clip[3] * 0.5 + 0.5) * fw,
                        (0.5 - clip[1] / clip[3] * 0.5) * fh,
                    ]);
                }
            }
            Some(pts)
        };
        match (project(t0), project(t1)) {
            (Some(a), Some(b)) if a.len() == b.len() => a
                .iter()
                .zip(&b)
                .map(|(p, q)| {
                    let d = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
                    if d.is_nan() { 0.0 } else { d }
                })
                .fold(0.0, f32::max),
            _ => f32::INFINITY,
        }
    }

    /// Renders a `render3d` group: every connected mesh, lit by the connected
    /// lights (the first directional light casts a shadow map), seen through
    /// the connected camera, with depth and 4x MSAA.
    #[allow(clippy::too_many_arguments)]
    fn encode_render3d(
        &mut self,
        plan: &Plan,
        g: &plan::Group,
        inputs: &[Placed],
        out: &Texture,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> Result<(), RenderError> {
        use scene3d::{Mat4, Params};
        const SAMPLES: u32 = 4;
        const SHADOW: u32 = 2048;
        let (fw, fh) = (doc.format.width, doc.format.height);
        // The scene draws on the device grid (the view's size); the shaders
        // place textures and find screen positions in its texels, and every
        // input texture is at the view's density.
        let v = self.view;
        let (dw, dh) = (v.w, v.h);
        let dev = |o: [f32; 2]| [o[0] / v.px, o[1] / v.px];
        if self.three.is_none() {
            let target = if self.device.backend() == "metal" {
                ultrafast_shaderc::Target::Metal
            } else {
                ultrafast_shaderc::Target::Spirv
            };
            use ultrafast_shaderc::Stage;
            let code = ultrafast_shaderc::Compiler::new()
                .compile_stages(
                    include_str!("../shaders/lib3d.slang"),
                    &[
                        ("vs_mesh", Stage::Vertex),
                        ("fs_mesh", Stage::Fragment),
                        ("vs_shadow", Stage::Vertex),
                        ("fs_shadow", Stage::Fragment),
                        ("fs_dist", Stage::Fragment),
                    ],
                    target,
                )
                .map_err(|e| RenderError::Setup(e.to_string()))?;
            let lib = self.device.library(&code.code)?;
            let pipeline = self.device.draw_pipeline(
                &lib,
                "vs_mesh",
                "fs_mesh",
                SAMPLES,
                true,
                ultrafast_gpu::Blend::Opaque,
                PixelFormat::Rgba16Float,
            )?;
            let shadow_pipeline = self.device.draw_pipeline(
                &lib,
                "vs_shadow",
                "fs_shadow",
                1,
                true,
                ultrafast_gpu::Blend::Opaque,
                PixelFormat::R32Float,
            )?;
            let dist_pipeline = self.device.draw_pipeline(
                &lib,
                "vs_shadow",
                "fs_dist",
                1,
                true,
                ultrafast_gpu::Blend::Opaque,
                PixelFormat::R32Float,
            )?;
            // 1x1 white RGBA16F (half 1.0 = 0x3c00).
            let one = 0x3c00u16.to_le_bytes();
            let white = self.device.upload_texture(
                TextureDesc {
                    width: 1,
                    height: 1,
                    format: PixelFormat::Rgba16Float,
                    cpu_readable: false,
                },
                &[one[0], one[1], one[0], one[1], one[0], one[1], one[0], one[1]],
            );
            let shadow_map = self.device.create_texture(TextureDesc {
                width: SHADOW,
                height: SHADOW,
                format: PixelFormat::R32Float,
                cpu_readable: false,
            });
            let shadow_depth = self
                .device
                .create_attachment(SHADOW, SHADOW, PixelFormat::Depth32Float, 1);
            self.three = Some(Three {
                pipeline,
                shadow_pipeline,
                shadow_map,
                shadow_depth,
                attachments: None,
                dist_pipeline,
                dof: None,
                white,
                meshes: HashMap::new(),
            });
        }

        let root = &plan.nodes[g.root];
        let (root_t, active) = local_time(root, t);
        let rp = Params {
            node: &doc.nodes[&root.id],
            t: root_t,
        };
        let clear = rp.color("background", [0.0, 0.0, 0.0, 0.0]);
        // Outside its time window the scene draws nothing, background included.
        let clear = if active {
            [clear[0] * clear[3], clear[1] * clear[3], clear[2] * clear[3], clear[3]]
        } else {
            [0.0; 4]
        };
        let sky = rp.color("sky", [0.9, 0.95, 1.0, 1.0]);
        let ground = rp.color("ground", [0.35, 0.3, 0.28, 1.0]);
        let ambient = rp.num("ambient", 0.35);
        let shadows_on = !matches!(
            doc.nodes[&root.id].params.get("shadows"),
            Some(ultrafast_graph::Param::Bool(false))
        );
        let shadow_strength = rp.num("shadow_strength", 0.85);
        // Fog toward `fog_color` (the background by default) between
        // `fog` [start, end] world units from the camera.
        let fog = match doc.nodes[&root.id].params.get("fog").and_then(|p| p.eval(root_t)) {
            Some(v) if v.len() >= 2 => Some((v[0] as f32, (v[1] as f32).max(v[0] as f32 + 1e-3))),
            _ => None,
        };
        let fog_color = {
            let c = rp.color("fog_color", rp.color("background", [0.0, 0.0, 0.0, 1.0]));
            [c[0], c[1], c[2]]
        };
        let env_intensity = rp.num("environment_intensity", 1.0);
        let env_rotation = rp.num("environment_rotation", 0.0);
        // An image on the `environment` port lights and reflects in the scene.
        let env: Option<usize> = doc.nodes[&root.id].inputs.get("environment").and_then(|src| {
            let si = root.inputs.iter().copied().find(|&i| plan.nodes[i].id == *src)?;
            g.textures
                .iter()
                .position(|ts| *ts == TexSource::Group(plan.group_of[si]))
        });
        let env_origin = dev(env.map(|i| inputs[i].origin).unwrap_or([0.0, 0.0]));
        // A blurred copy of the environment, for rough surfaces and diffuse
        // light (a sparse disk over the sharp image would band).
        let env_soft: Option<Placed> = match env {
            Some(i) if active => {
                let frame_rect = [0.0, 0.0, fw as f64, fh as f64];
                let sigma = (fw.max(fh) as f32) * 0.025;
                Some(
                    self.encode_blur(&inputs[i], frame_rect, sigma, None, false, doc, t, list)
                        .0,
                )
            }
            _ => None,
        };
        let env_soft_origin = dev(env_soft.as_ref().map(|p| p.origin).unwrap_or([0.0, 0.0]));
        // Constant depth bias, in shadow-map texels of depth.
        let shadow_bias = rp.num("shadow_bias", 1.0);

        // Camera and lights.
        let aspect = fw as f32 / fh.max(1) as f32;
        let mut eye = [0.0f32, 0.0, 5.0];
        let mut target = [0.0f32, 0.0, 0.0];
        let mut up = [0.0f32, 1.0, 0.0];
        let (mut fov, mut near, mut far) = (45.0f32, 0.1f32, 1000.0f32);
        let mut lights: Vec<[f32; 8]> = Vec::new();
        let mut meshes: Vec<usize> = Vec::new();
        for &o in &g.nodes[..g.nodes.len() - 1] {
            let pn = &plan.nodes[o];
            let (lt, on) = local_time(pn, t);
            if !on || !active {
                continue;
            }
            let p = Params {
                node: &doc.nodes[&pn.id],
                t: lt,
            };
            match pn.def.name {
                "camera" => {
                    target = p.vec("target", target);
                    up = p.vec("up", up);
                    fov = p.num("fov", fov);
                    near = p.num("near", near);
                    far = p.num("far", far);
                    eye = if pn_has(&doc.nodes[&pn.id], "orbit") {
                        // Orbit: azimuth, elevation (degrees), distance around target.
                        let [az, el, dist] = p.vec("orbit", [0.0, 0.0, 5.0]);
                        let (az, el) = (az.to_radians(), el.to_radians());
                        [
                            target[0] + dist * el.cos() * az.sin(),
                            target[1] + dist * el.sin(),
                            target[2] + dist * el.cos() * az.cos(),
                        ]
                    } else {
                        p.vec("position", eye)
                    };
                }
                "light" => {
                    let c = p.color("color", [1.0, 1.0, 1.0, 1.0]);
                    let point = p.text("kind") == Some("point");
                    let v = if point {
                        p.vec("position", [0.0, 2.0, 2.0])
                    } else {
                        p.vec("direction", [0.4, 0.8, 0.6])
                    };
                    lights.push([
                        point as u8 as f32,
                        v[0],
                        v[1],
                        v[2],
                        c[0],
                        c[1],
                        c[2],
                        p.num("intensity", 3.0),
                    ]);
                }
                "mesh" => meshes.push(o),
                _ => {}
            }
        }
        if lights.is_empty() {
            // A default key and rim light so an unlit scene is still readable.
            lights.push([0.0, 0.5, 0.8, 0.6, 1.0, 0.98, 0.95, 3.0]);
            lights.push([0.0, -0.6, 0.3, -0.7, 0.6, 0.7, 1.0, 1.2]);
        }
        // The first directional light casts the shadow: put it first.
        if let Some(i) = lights.iter().position(|l| l[0] < 0.5) {
            lights.swap(0, i);
        }
        lights.truncate(16);
        let view = Mat4::look_at(eye, target, up);
        let view_proj = Mat4::perspective(fov, aspect, near, far).mul(&view);

        // Meshes: geometry from the cache, uploaded once.
        let three = self.three.as_mut().unwrap();
        if three
            .attachments
            .as_ref()
            .is_none_or(|(c, _)| c.desc().width != dw || c.desc().height != dh)
        {
            three.attachments = Some((
                self.device.create_attachment(dw, dh, PixelFormat::Rgba16Float, SAMPLES),
                self.device
                    .create_attachment(dw, dh, PixelFormat::Depth32Float, SAMPLES),
            ));
        }
        struct MeshDraw {
            key: String,
            model: Mat4,
            params: Vec<f32>,
            texture: Option<usize>,
            /// Glass: transmission, index of refraction, thickness,
            /// dispersion.
            glass: Option<[f32; 4]>,
        }
        let mut draws_params: Vec<MeshDraw> = Vec::new();
        let mut scene_min = [f32::MAX; 3];
        let mut scene_max = [f32::MIN; 3];
        for &o in &meshes {
            let pn = &plan.nodes[o];
            let node = &doc.nodes[&pn.id];
            let key = scene3d::mesh_key(node);
            if !three.meshes.contains_key(&key) {
                let m = scene3d::build_mesh(&pn.id, node, &self.base_dir, &mut self.fonts)?;
                let vb = self
                    .device
                    .create_buffer(m.vertices.len() * 4, Some(f32_bytes(&m.vertices)));
                let ib = self
                    .device
                    .create_buffer(m.indices.len() * 4, Some(u32_bytes(&m.indices)));
                let mut lo = [f32::MAX; 3];
                let mut hi = [f32::MIN; 3];
                for v in m.vertices.chunks_exact(8) {
                    for k in 0..3 {
                        lo[k] = lo[k].min(v[k]);
                        hi[k] = hi[k].max(v[k]);
                    }
                }
                three
                    .meshes
                    .insert(key.clone(), (vb, ib, m.indices.len() as u32, [lo, hi]));
            }
            let p = Params {
                node,
                t: local_time(pn, t).0,
            };
            let model = p.model();
            // World bounds for fitting the shadow camera.
            let [lo, hi] = three.meshes[&key].3;
            for corner in 0..8 {
                let c = [
                    if corner & 1 == 0 { lo[0] } else { hi[0] },
                    if corner & 2 == 0 { lo[1] } else { hi[1] },
                    if corner & 4 == 0 { lo[2] } else { hi[2] },
                ];
                let m = &model.0;
                let w = [
                    m[0] * c[0] + m[1] * c[1] + m[2] * c[2] + m[3],
                    m[4] * c[0] + m[5] * c[1] + m[6] * c[2] + m[7],
                    m[8] * c[0] + m[9] * c[1] + m[10] * c[2] + m[11],
                ];
                for k in 0..3 {
                    scene_min[k] = scene_min[k].min(w[k]);
                    scene_max[k] = scene_max[k].max(w[k]);
                }
            }
            let normal = model.inverse_affine().transpose();
            let base = p.color("color", [0.8, 0.8, 0.8, 1.0]);
            let emissive = p.color("emissive", [0.0, 0.0, 0.0, 1.0]);
            let texture = pn.inputs.first().and_then(|&src| {
                g.textures
                    .iter()
                    .position(|ts| *ts == TexSource::Group(plan.group_of[src]))
            });
            let origin = dev(texture.map(|i| inputs[i].origin).unwrap_or([0.0, 0.0]));
            let mut prm = header(doc, t, self.view).to_vec();
            prm.extend(model.0);
            prm.extend(view_proj.0);
            prm.extend(normal.0);
            prm.extend(base);
            prm.extend([
                p.num("metallic", 0.0),
                p.num("roughness", 0.5),
                texture.is_some() as u8 as f32,
                p.num("opacity", 1.0),
            ]);
            prm.extend([emissive[0], emissive[1], emissive[2], 0.0]);
            prm.extend([eye[0], eye[1], eye[2], lights.len() as f32]);
            prm.extend([sky[0], sky[1], sky[2], ambient, ground[0], ground[1], ground[2], 0.0]);
            prm.extend([origin[0], origin[1], fw as f32 / v.px, fh as f32 / v.px]);
            let transmission = p.num("transmission", 0.0);
            let glass = (transmission > 0.0).then(|| {
                [
                    transmission.min(1.0),
                    p.num("ior", 1.45).max(1.0),
                    p.num("thickness", 0.5),
                    p.num("dispersion", 0.0),
                ]
            });
            draws_params.push(MeshDraw {
                key,
                model,
                params: prm,
                texture,
                glass,
            });
        }

        // Shadow camera: orthographic along the shadow light, fitted to the
        // scene's bounding sphere.
        let shadow_light = lights
            .first()
            .filter(|l| l[0] < 0.5 && shadows_on && !draws_params.is_empty())
            .copied();
        let light_vp = shadow_light.map(|l| {
            let c = [
                (scene_min[0] + scene_max[0]) / 2.0,
                (scene_min[1] + scene_max[1]) / 2.0,
                (scene_min[2] + scene_max[2]) / 2.0,
            ];
            let r = ((0..3).map(|k| (scene_max[k] - scene_min[k]).powi(2)).sum::<f32>()).sqrt() / 2.0 + 1e-3;
            let len = (l[1] * l[1] + l[2] * l[2] + l[3] * l[3]).sqrt().max(1e-6);
            let d = [l[1] / len, l[2] / len, l[3] / len];
            let eye = [c[0] + d[0] * r * 2.0, c[1] + d[1] * r * 2.0, c[2] + d[2] * r * 2.0];
            let up = if d[1].abs() > 0.95 {
                [0.0, 0.0, 1.0]
            } else {
                [0.0, 1.0, 0.0]
            };
            let view = Mat4::look_at(eye, c, up);
            let (n, f) = (r * 0.5, r * 3.5);
            let ortho = Mat4([
                1.0 / r,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0 / r,
                0.0,
                0.0,
                0.0,
                0.0,
                -1.0 / (f - n),
                -n / (f - n),
                0.0,
                0.0,
                0.0,
                1.0,
            ]);
            (ortho.mul(&view), 2.0 * r)
        });
        for d in &mut draws_params {
            match light_vp {
                Some((lvp, extent)) => {
                    d.params.extend(lvp.0);
                    // Biases scale with how much world one shadow texel covers.
                    let texel_world = extent / SHADOW as f32;
                    d.params
                        .extend([1.0, shadow_bias / SHADOW as f32, 1.0 / SHADOW as f32, shadow_strength]);
                    d.params.push(texel_world * 1.5);
                }
                None => {
                    d.params.extend(Mat4::IDENTITY.0);
                    d.params.extend([0.0; 5]);
                }
            }
            // [101..106] environment, [106..112] fog.
            d.params.extend([
                env.is_some() as u8 as f32,
                env_intensity,
                env_rotation,
                env_origin[0],
                env_origin[1],
                fog.is_some() as u8 as f32,
                fog.map(|f| f.0).unwrap_or(0.0),
                fog.map(|f| f.1).unwrap_or(1.0),
                fog_color[0],
                fog_color[1],
                fog_color[2],
            ]);
            // [112..114] blurred environment origin, [114..118] glass,
            // [118..120] unused.
            let g = d.glass.unwrap_or([0.0; 4]);
            d.params
                .extend([env_soft_origin[0], env_soft_origin[1], g[0], g[1], g[2], g[3], 0.0, 0.0]);
            for l in &lights {
                d.params.extend(l);
            }
        }

        if active && let Some((lvp, _)) = light_vp {
            let shadow_params: Vec<Vec<f32>> = draws_params
                .iter()
                .map(|d| {
                    let mut p = header(doc, t, self.view).to_vec();
                    p.extend(d.model.0);
                    p.extend(lvp.0);
                    p
                })
                .collect();
            let shadow_draws: Vec<ultrafast_gpu::Draw> = draws_params
                .iter()
                .zip(&shadow_params)
                .map(|(d, p)| {
                    let (vb, ib, n, _) = &three.meshes[&d.key];
                    ultrafast_gpu::Draw {
                        pipeline: &three.shadow_pipeline,
                        params: p,
                        vertices: ultrafast_gpu::BufferBinding::Buffer(vb),
                        indices: ib,
                        index_count: *n,
                        texture: &three.white,
                        texture2: &three.white,
                        texture3: &three.white,
                        texture4: &three.white,
                    }
                })
                .collect();
            list.draw_pass(
                ultrafast_gpu::DrawTarget {
                    resolve: &three.shadow_map,
                    color: None,
                    depth: Some(&three.shadow_depth),
                    clear: [1.0, 1.0, 1.0, 1.0],
                },
                &shadow_draws,
            );
        }
        // Depth of field: a camera-distance pass, the sharp image, a gather.
        let aperture = rp.num("aperture", 0.0);
        let focus = if pn_has(&doc.nodes[&root.id], "focus") {
            rp.num("focus", 5.0)
        } else {
            ((eye[0] - target[0]).powi(2) + (eye[1] - target[1]).powi(2) + (eye[2] - target[2]).powi(2)).sqrt()
        };
        let dof_on = active && aperture > 0.25 && !draws_params.is_empty();
        if dof_on {
            if three
                .dof
                .as_ref()
                .is_none_or(|(d, _, _)| d.desc().width != dw || d.desc().height != dh)
            {
                let tex = |format| TextureDesc {
                    width: dw,
                    height: dh,
                    format,
                    cpu_readable: false,
                };
                three.dof = Some((
                    self.device.create_texture(tex(PixelFormat::R32Float)),
                    self.device.create_attachment(dw, dh, PixelFormat::Depth32Float, 1),
                    self.device.create_texture(tex(PixelFormat::Rgba16Float)),
                ));
            }
            let dist_params: Vec<Vec<f32>> = draws_params
                .iter()
                .map(|d| {
                    let mut p = header(doc, t, self.view).to_vec();
                    p.extend(d.model.0);
                    p.extend(view_proj.0);
                    p.extend(eye);
                    p
                })
                .collect();
            let (dist, dist_depth, _) = three.dof.as_ref().unwrap();
            let dist_draws: Vec<ultrafast_gpu::Draw> = draws_params
                .iter()
                .zip(&dist_params)
                .map(|(d, p)| {
                    let (vb, ib, n, _) = &three.meshes[&d.key];
                    ultrafast_gpu::Draw {
                        pipeline: &three.dist_pipeline,
                        params: p,
                        vertices: ultrafast_gpu::BufferBinding::Buffer(vb),
                        indices: ib,
                        index_count: *n,
                        texture: &three.white,
                        texture2: &three.white,
                        texture3: &three.white,
                        texture4: &three.white,
                    }
                })
                .collect();
            list.draw_pass(
                ultrafast_gpu::DrawTarget {
                    resolve: dist,
                    color: None,
                    depth: Some(dist_depth),
                    clear: [1.0e6, 0.0, 0.0, 1.0],
                },
                &dist_draws,
            );
        }
        let (color, depth) = three.attachments.as_ref().unwrap();
        // Glass draws after everything else, refracting the finished opaque
        // scene (which it samples instead of an albedo texture).
        let draws: Vec<ultrafast_gpu::Draw> = if active {
            draws_params
                .iter()
                .filter(|d| d.glass.is_none())
                .map(|d| {
                    let (vb, ib, n, _) = &three.meshes[&d.key];
                    ultrafast_gpu::Draw {
                        pipeline: &three.pipeline,
                        params: &d.params,
                        vertices: ultrafast_gpu::BufferBinding::Buffer(vb),
                        indices: ib,
                        index_count: *n,
                        texture: d.texture.map(|i| &inputs[i].tex).unwrap_or(&three.white),
                        texture2: &three.shadow_map,
                        texture3: env.map(|i| &inputs[i].tex).unwrap_or(&three.white),
                        texture4: env_soft.as_ref().map(|p| &p.tex).unwrap_or(&three.white),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let target_tex = if dof_on { &three.dof.as_ref().unwrap().2 } else { out };
        list.draw_pass(
            ultrafast_gpu::DrawTarget {
                resolve: target_tex,
                color: Some(color),
                depth: Some(depth),
                clear,
            },
            &draws,
        );
        if dof_on {
            let (dist, _, sharp) = three.dof.as_ref().unwrap();
            // Gather at half resolution, then mix back by each pixel's blur.
            // Both passes work in device texels: the header's frame is the
            // device's, and the aperture (the largest blur, frame px) is
            // measured in its texels.
            let half = TextureDesc {
                width: dw.div_ceil(2),
                height: dh.div_ceil(2),
                format: PixelFormat::Rgba16Float,
                cpu_readable: false,
            };
            let soft = self.pool.lease(&self.device, half);
            let (tf, dwf, dhf) = (t as f32, dw as f32, dh as f32);
            let ap = aperture / v.px;
            let p = [tf, 2.0, dwf, dhf, focus, ap, (aperture * 2.5).clamp(24.0, 64.0)];
            list.dispatch(&self.kernels.utility("k_dof"), &soft, &[sharp, dist], &p);
            let p = [tf, 1.0, dwf, dhf, focus, ap, 0.0, 2.0];
            list.dispatch(&self.kernels.utility("k_dof_mix"), out, &[sharp, &soft, dist], &p);
            self.pool.give_back(soft);
        }
        if active && draws_params.iter().any(|d| d.glass.is_some()) {
            let frame = TextureDesc {
                width: dw,
                height: dh,
                format: PixelFormat::Rgba16Float,
                cpu_readable: false,
            };
            let scene = self.pool.lease(&self.device, frame);
            list.dispatch(
                &self.kernels.utility("k_copy"),
                &scene,
                &[out],
                &header(doc, t, self.view),
            );
            let glass_tex = self.pool.lease(&self.device, frame);
            let three = self.three.as_ref().unwrap();
            let (color, depth) = three.attachments.as_ref().unwrap();
            let glass: Vec<ultrafast_gpu::Draw> = draws_params
                .iter()
                .filter(|d| d.glass.is_some())
                .map(|d| {
                    let (vb, ib, n, _) = &three.meshes[&d.key];
                    ultrafast_gpu::Draw {
                        pipeline: &three.pipeline,
                        params: &d.params,
                        vertices: ultrafast_gpu::BufferBinding::Buffer(vb),
                        indices: ib,
                        index_count: *n,
                        texture: &scene,
                        texture2: &three.shadow_map,
                        texture3: env.map(|i| &inputs[i].tex).unwrap_or(&three.white),
                        texture4: env_soft.as_ref().map(|p| &p.tex).unwrap_or(&three.white),
                    }
                })
                .collect();
            list.draw_pass(
                ultrafast_gpu::DrawTarget {
                    resolve: &glass_tex,
                    color: Some(color),
                    depth: Some(depth),
                    clear: [0.0; 4],
                },
                &glass,
            );
            let mut p = header(doc, t, self.view).to_vec();
            p.extend([0.0, 0.0, 0.0, 0.0]);
            list.dispatch(&self.kernels.utility("k_over_frames"), out, &[&glass_tex, &scene], &p);
            self.pool.give_back(scene);
            self.pool.give_back(glass_tex);
        }
        if let Some(p) = env_soft {
            self.pool.give_back(p.tex);
        }
        Ok(())
    }

    /// Gaussian blur of `src` over the region its content covers plus the
    /// blur's reach. Large radii run at 1/2..1/8 resolution (the result is
    /// band-limited anyway) and are upsampled into the output, or with
    /// `keep_low` returned at that resolution (its readers sample it).
    /// Returns the placed result and its content bounds.
    #[allow(clippy::too_many_arguments)]
    fn encode_blur(
        &mut self,
        src: &Placed,
        src_bounds: ops::Rect,
        sigma: f32,
        prev: Option<Placed>,
        keep_low: bool,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> (Placed, ops::Rect) {
        let (fw, fh) = (doc.format.width as f64, doc.format.height as f64);
        let pad = (3.0 * sigma as f64).ceil() + 2.0;
        let r = intersect(
            [
                src_bounds[0] - pad,
                src_bounds[1] - pad,
                src_bounds[2] + pad,
                src_bounds[3] + pad,
            ],
            [-pad, -pad, fw + pad, fh + pad],
        );
        // The region on the device grid: texels of the view's size (px
        // frame px each), so a blur at a larger size keeps its detail.
        let px = self.view.px;
        let pxd = px as f64;
        let (x0, y0) = ((r[0] / pxd).floor(), (r[1] / pxd).floor());
        let (x1, y1) = ((r[2] / pxd).ceil().max(x0 + 1.0), (r[3] / pxd).ceil().max(y0 + 1.0));
        let (rw, rh) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let bucket = |v: u32| (v.div_ceil(RASTER_BUCKET) * RASTER_BUCKET).max(RASTER_BUCKET);
        let desc = |w: u32, h: u32| TextureDesc {
            width: w,
            height: h,
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        };
        // Downscale so each pass needs about 3 sigma of taps at >= 3 texels.
        let mut f = 1.0f32;
        while f < 8.0 && sigma / px / (f * 2.0) >= 3.0 {
            f *= 2.0;
        }
        let out_desc = if f > 1.0 && keep_low {
            desc(bucket(rw.div_ceil(f as u32)), bucket(rh.div_ceil(f as u32)))
        } else {
            desc(bucket(rw), bucket(rh))
        };
        let out = match prev {
            Some(p) if p.tex.desc() == out_desc => p.tex,
            Some(p) => {
                self.pool.give_back(p.tex);
                self.pool.lease(&self.device, out_desc)
            }
            None => self.pool.lease(&self.device, out_desc),
        };
        let blur = self.kernels.utility("k_blur_region");
        let resample = self.kernels.utility("k_resample");
        let hdr = header(doc, t, self.view);
        let (ox, oy) = ((x0 * pxd) as f32, (y0 * pxd) as f32);
        let p = |v: &[f32]| {
            let mut x = hdr.to_vec();
            x.extend_from_slice(v);
            x
        };
        let si = src.scale;
        // Texel sizes in frame px: the view's, and f of them downscaled.
        let (t1, tf) = (px, f * px);
        let scale = if f == 1.0 {
            let tmp = self.pool.lease(&self.device, out_desc);
            list.dispatch(
                &blur,
                &tmp,
                &[&src.tex],
                &p(&[sigma, 1.0, 0.0, t1, ox, oy, src.origin[0], src.origin[1], si]),
            );
            list.dispatch(&blur, &out, &[&tmp], &p(&[sigma, 0.0, 1.0, t1, ox, oy, ox, oy, t1]));
            self.pool.give_back(tmp);
            t1
        } else if keep_low {
            let b = self.pool.lease(&self.device, out_desc);
            list.dispatch(
                &resample,
                &out,
                &[&src.tex],
                &p(&[tf, ox, oy, src.origin[0], src.origin[1], si, 2.0]),
            );
            list.dispatch(&blur, &b, &[&out], &p(&[sigma, 1.0, 0.0, tf, ox, oy, ox, oy, tf]));
            list.dispatch(&blur, &out, &[&b], &p(&[sigma, 0.0, 1.0, tf, ox, oy, ox, oy, tf]));
            self.pool.give_back(b);
            tf
        } else {
            let low = desc(bucket(rw.div_ceil(f as u32)), bucket(rh.div_ceil(f as u32)));
            let a = self.pool.lease(&self.device, low);
            let b = self.pool.lease(&self.device, low);
            list.dispatch(
                &resample,
                &a,
                &[&src.tex],
                &p(&[tf, ox, oy, src.origin[0], src.origin[1], si, 2.0]),
            );
            list.dispatch(&blur, &b, &[&a], &p(&[sigma, 1.0, 0.0, tf, ox, oy, ox, oy, tf]));
            list.dispatch(&blur, &a, &[&b], &p(&[sigma, 0.0, 1.0, tf, ox, oy, ox, oy, tf]));
            list.dispatch(&resample, &out, &[&a], &p(&[t1, ox, oy, ox, oy, tf, 1.0]));
            self.pool.give_back(a);
            self.pool.give_back(b);
            t1
        };
        (
            Placed {
                tex: out,
                origin: [ox, oy],
                scale,
            },
            [x0 * pxd, y0 * pxd, x1 * pxd, y1 * pxd],
        )
    }

    /// Draws a particle system into a texture sized to its bounds.
    #[allow(clippy::too_many_arguments)]
    fn encode_particles(
        &mut self,
        plan: &Plan,
        g: &plan::Group,
        sprite: Option<Placed>,
        prev: Option<Placed>,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> Result<Placed, RenderError> {
        let pn = &plan.nodes[g.root];
        let (lt, on) = local_time(pn, t);
        let node = &doc.nodes[&pn.id];
        let sys = if on && node.op == "swarm" {
            swarm::evaluate(&pn.id, node, lt, &mut self.fonts, &self.base_dir, &mut self.swarm_homes)?
        } else if on {
            particles::evaluate(&pn.id, node, lt)?
        } else {
            particles::Particles {
                data: Vec::new(),
                count: 0,
                bounds: [0.0; 4],
            }
        };
        if self.sprites.is_none() {
            let target = if self.device.backend() == "metal" {
                ultrafast_shaderc::Target::Metal
            } else {
                ultrafast_shaderc::Target::Spirv
            };
            let code = ultrafast_shaderc::Compiler::new()
                .compile_stages(
                    include_str!("../shaders/particles.slang"),
                    &[
                        ("vs_particle", ultrafast_shaderc::Stage::Vertex),
                        ("fs_particle", ultrafast_shaderc::Stage::Fragment),
                    ],
                    target,
                )
                .map_err(|e| RenderError::Setup(e.to_string()))?;
            let lib = self.device.library(&code.code)?;
            let over = self.device.draw_pipeline(
                &lib,
                "vs_particle",
                "fs_particle",
                1,
                false,
                ultrafast_gpu::Blend::Over,
                PixelFormat::Rgba16Float,
            )?;
            let add = self.device.draw_pipeline(
                &lib,
                "vs_particle",
                "fs_particle",
                1,
                false,
                ultrafast_gpu::Blend::Add,
                PixelFormat::Rgba16Float,
            )?;
            let one = 0x3c00u16.to_le_bytes();
            let white = self.device.upload_texture(
                TextureDesc {
                    width: 1,
                    height: 1,
                    format: PixelFormat::Rgba16Float,
                    cpu_readable: false,
                },
                &[one[0], one[1], one[0], one[1], one[0], one[1], one[0], one[1]],
            );
            self.sprites = Some((over, add, self.device.create_buffer(16, None), white));
        }
        // Under motion blur, drawn once, each particle blurs itself along its
        // path over the shutter (within the sampled waves the samples do).
        let span = match (&self.mb_frame, self.mb_sample) {
            (Some(f), None) if !self.mb_full => f.span,
            _ => 0.0,
        };
        let reach = if span > 0.0 {
            sys.data
                .chunks_exact(12)
                .map(|d| (d[8] as f64).hypot(d[9] as f64))
                .fold(0.0, f64::max)
                * span
                / 2.0
                + 1.0
        } else {
            0.0
        };
        // Region: the particles' bounds, clipped to a window around the
        // frame, in device texels (px frame px each).
        let px = self.view.px as f64;
        let (fw, fh) = (self.view.w as f64, self.view.h as f64);
        let b = [
            (sys.bounds[0] - reach) / px,
            (sys.bounds[1] - reach) / px,
            (sys.bounds[2] + reach) / px,
            (sys.bounds[3] + reach) / px,
        ];
        let r = if sys.count == 0 {
            [0.0, 0.0, 1.0, 1.0]
        } else {
            intersect(
                [b[0].floor(), b[1].floor(), b[2].ceil(), b[3].ceil()],
                [-fw, -fh, 2.0 * fw, 2.0 * fh],
            )
        };
        let (rw, rh) = (((r[2] - r[0]).max(1.0)) as u32, ((r[3] - r[1]).max(1.0)) as u32);
        let bucket = |v: u32| (v.div_ceil(RASTER_BUCKET) * RASTER_BUCKET).max(RASTER_BUCKET);
        let desc = TextureDesc {
            width: bucket(rw).min(8192),
            height: bucket(rh).min(8192),
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        };
        let out = match prev {
            Some(p) if p.tex.desc() == desc => p.tex,
            Some(p) => {
                self.pool.give_back(p.tex);
                self.pool.lease(&self.device, desc)
            }
            None => self.pool.lease(&self.device, desc),
        };
        // Sequential indices 0..6n, grown as needed.
        let need = sys.count * 6 * 4;
        let (over, add, mut idx, white) = self.sprites.clone().unwrap();
        if idx.len() < need {
            let n = (sys.count * 6).next_power_of_two();
            let seq: Vec<u32> = (0..n as u32).collect();
            idx = self.device.create_buffer(n * 4, Some(u32_bytes(&seq)));
            self.sprites.as_mut().unwrap().2 = idx.clone();
        }
        let additive = ops::text(node, "blend") == Some("add");
        // Region origin and size in frame px: particles are placed there.
        let mut p = header(doc, t, self.view).to_vec();
        p.extend([
            (r[0] * px) as f32,
            (r[1] * px) as f32,
            (desc.width as f64 * px) as f32,
            (desc.height as f64 * px) as f32,
            span as f32,
        ]);
        let sprite_tex = sprite.as_ref().map(|s| &s.tex).unwrap_or(&white);
        let draws = if sys.count > 0 {
            vec![ultrafast_gpu::Draw {
                pipeline: if additive { &add } else { &over },
                params: &p,
                vertices: ultrafast_gpu::BufferBinding::Data(f32_bytes(&sys.data)),
                indices: &idx,
                index_count: (sys.count * 6) as u32,
                texture: sprite_tex,
                texture2: &white,
                texture3: &white,
                texture4: &white,
            }]
        } else {
            Vec::new()
        };
        list.draw_pass(
            ultrafast_gpu::DrawTarget {
                resolve: &out,
                color: None,
                depth: None,
                clear: [0.0; 4],
            },
            &draws,
        );
        Ok(Placed {
            tex: out,
            origin: [(r[0] * px) as f32, (r[1] * px) as f32],
            scale: px as f32,
        })
    }

    /// Copies a placed texture into a frame-sized one (tier-0 `opacity` at 1).
    fn copy_placed(
        &self,
        src: &Placed,
        dst: &Texture,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> Result<(), RenderError> {
        let k = self.kernels.tier0_kernel("opacity").map_err(RenderError::Setup)?;
        let mut p = header(doc, t, self.view).to_vec();
        p.extend([1.0, src.origin[0], src.origin[1], src.scale]);
        list.dispatch(&k, dst, &[&src.tex], &p);
        Ok(())
    }

    /// Flattened geometry for a raster node, rebuilt only when its key changes.
    fn geometry(
        &mut self,
        id: &str,
        doc: &Document,
        t: f64,
        outer: ops::Affine,
        key: u64,
    ) -> Result<Arc<geom::Geometry>, RenderError> {
        if let Some((k, _, _, g)) = self.geometry.get(id)
            && *k == key
        {
            return Ok(g.clone());
        }
        let node = &doc.nodes[id];
        let content = geom::content_key(node, t);
        let full = outer.mul(geom::placement_affine(node, t));
        // Moved, turned or shrunk but otherwise unchanged (the common
        // animated case): map the geometry as last built instead of building
        // it again (curves flattened for its size stay as fine; grown, it is
        // built again).
        if let Some((_, c, built_at, built)) = self.built.get(id)
            && *c == content
            && let Some(back) = built_at.inverse()
        {
            let m = full.mul(back);
            let same_linear = (0..4).all(|i| (built_at.0[i] - full.0[i]).abs() < 1e-9);
            // Largest stretch of the 2x2 part: its biggest singular value.
            let [a, b, cc, d, ..] = m.0;
            let (p, q) = ((a * a + b * b + cc * cc + d * d) / 2.0, (a * d - b * cc).abs());
            let stretch = (p + (p * p - q * q).max(0.0).sqrt()).sqrt();
            if same_linear || stretch <= 1.0005 {
                let g = Arc::new(if same_linear {
                    built.translated(full.0[4] - built_at.0[4], full.0[5] - built_at.0[5])
                } else {
                    built.transformed(m.0)
                });
                self.bounds.insert(id.to_string(), self.frame_bounds(g.bounds));
                self.geometry.insert(id.to_string(), (key, content, full, g.clone()));
                return Ok(g);
            }
        }
        let g = Arc::new(match node.op.as_str() {
            "text" => geom::text_node(id, node, &doc.format, t, outer, &mut self.fonts, &self.base_dir)?,
            _ => geom::path(id, node, &doc.format, t, outer)?,
        });
        if let Some(m) = reveal_falls_short(node, g.units) {
            self.diagnose(id, m);
        }
        self.bounds.insert(id.to_string(), self.frame_bounds(g.bounds));
        self.geometry.insert(id.to_string(), (key, content, full, g.clone()));
        self.built.insert(id.to_string(), (key, content, full, g.clone()));
        Ok(g)
    }

    /// Bounds of geometry built on the device grid, in frame px (what
    /// picking, inspection and motion extents read).
    fn frame_bounds(&self, b: [f32; 4]) -> [f32; 4] {
        let px = self.view.px;
        [b[0] * px, b[1] * px, b[2] * px, b[3] * px]
    }

    /// The color glyphs of `geo` for `k_raster_resolve`: their bitmaps put
    /// in the atlas (recorded on `list` before the resolve reads them), and
    /// twelve floats each (see `glyph_images` in lib.slang).
    fn glyph_records(&mut self, geo: &geom::Geometry, list: &mut CommandList) -> Vec<f32> {
        if geo.images.is_empty() {
            return Vec::new();
        }
        let put = self.kernels.utility("k_atlas_put");
        let atlas = self
            .glyph_atlas
            .get_or_insert_with(|| glyph_atlas::GlyphAtlas::new(&self.device));
        let mut place_all = |atlas: &mut glyph_atlas::GlyphAtlas| -> Vec<Option<[u32; 4]>> {
            geo.images
                .iter()
                .map(|im| atlas.place(&self.device, list, &put, &im.bitmap))
                .collect()
        };
        let generation = atlas.generation;
        let mut rects = place_all(atlas);
        // The atlas started over partway: place them all again.
        if atlas.generation != generation {
            rects = place_all(atlas);
        }
        let mut out = Vec::with_capacity(geo.images.len() * 12);
        for (im, rect) in geo.images.iter().zip(rects) {
            let Some(r) = rect else { continue };
            let m = im.xf;
            let det = m[0] * m[3] - m[1] * m[2];
            if det.abs() < 1e-12 {
                continue;
            }
            // Inverse of (u, v) -> (a u + c v + e, b u + d v + f).
            let (ia, ib, ic, id) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
            let (ie, if_) = (-(ia * m[4] + ic * m[5]), -(ib * m[4] + id * m[5]));
            out.extend([
                ia as f32,
                ic as f32,
                ie as f32,
                im.weight,
                ib as f32,
                id as f32,
                if_ as f32,
                0.0,
                r[0] as f32,
                r[1] as f32,
                r[2] as f32,
                r[3] as f32,
            ]);
        }
        out
    }

    /// Rasterizes geometry into a texture covering its bounds: clear the
    /// accumulator rows it touches, deposit segment areas, prefix-sum rows
    /// into coverage times color. Reuses `prev` when its size still fits.
    #[allow(clippy::too_many_arguments)]
    fn encode_raster(
        &mut self,
        geo: &geom::Geometry,
        prev: Option<Placed>,
        image_opacity: f32,
        framed: bool,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
    ) -> Placed {
        // The region covers the geometry's bounds, even off-frame: content
        // placed outside the frame may be moved in by later warps, blurs or
        // shaders. It is limited to a window around the frame, to the frame
        // itself when nothing downstream looks outside it (layers zoomed far
        // past the frame then cost a frame, not nine), and to the texture
        // size limit.
        // In device texels: the geometry was built on the view's grid.
        let (fw, fh) = (self.view.w as i64, self.view.h as i64);
        let (wx0, wy0, wx1, wy1) = if framed {
            (0, 0, fw, fh)
        } else {
            (-fw, -fh, 2 * fw, 2 * fh)
        };
        let b = geo.bounds;
        let (mut x0, mut y0, mut x1, mut y1) = if geo.is_empty() || b[0] > b[2] {
            (0, 0, 1, 1)
        } else {
            (
                (b[0].floor() as i64 - 1).max(wx0),
                (b[1].floor() as i64 - 1).max(wy0),
                (b[2].ceil() as i64 + 2).min(wx1),
                (b[3].ceil() as i64 + 2).min(wy1),
            )
        };
        x1 = x1.min(x0 + MAX_RASTER);
        y1 = y1.min(y0 + MAX_RASTER);
        if x1 <= x0 || y1 <= y0 {
            (x0, y0, x1, y1) = (0, 0, 1, 1);
        }
        let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let bucket = |v: usize| ((v as u32).div_ceil(RASTER_BUCKET) * RASTER_BUCKET).max(RASTER_BUCKET);
        let desc = TextureDesc {
            width: bucket(rw),
            height: bucket(rh),
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        };
        // The last texture is kept while the geometry fits and fills at
        // least four fifths of it (the resolve clears the rest, so slack
        // costs GPU time). Geometry that grows (zooming type) gets an eighth
        // more room, so it does not need a new texture on every frame and
        // motion-blur sample; each replaced one stays alive until the frames
        // using it finish.
        let area = |d: TextureDesc| d.width as u64 * d.height as u64;
        let tex = match prev {
            Some(p)
                if p.tex.desc().width >= desc.width
                    && p.tex.desc().height >= desc.height
                    && area(p.tex.desc()) * 4 <= 5 * area(desc) =>
            {
                p.tex
            }
            Some(p) => {
                let grew = p.tex.desc().width < desc.width || p.tex.desc().height < desc.height;
                self.pool.give_back(p.tex);
                let room = |v: usize, b: u32| {
                    if grew {
                        bucket(v + v / 8).min(MAX_RASTER as u32 + RASTER_BUCKET)
                    } else {
                        b
                    }
                };
                let grown = TextureDesc {
                    width: room(rw, desc.width),
                    height: room(rh, desc.height),
                    ..desc
                };
                self.pool.lease(&self.device, grown)
            }
            None => self.pool.lease(&self.device, desc),
        };

        // One accumulator plane per color group (grown with room to spare:
        // a replaced buffer also lives until in-flight frames finish).
        let groups = geo.groups.len().max(1);
        let bytes = (rw + 2) * rh * 4 * groups;
        if self.acc.as_ref().is_none_or(|b| b.len() < bytes) {
            self.acc = Some(self.device.create_private_buffer(bytes + bytes / 2));
        }
        let acc = self.acc.clone().unwrap();
        list.clear_buffer_range(&acc, 0, bytes);

        let hdr = header(doc, t, self.view);
        let n = geo.count();
        if n > 0 {
            let seg_bytes =
                unsafe { std::slice::from_raw_parts(geo.segments.as_ptr().cast::<u8>(), geo.segments.len() * 4) };
            let mut p = hdr.to_vec();
            p.extend([n as f32, rw as f32, rh as f32, x0 as f32, y0 as f32]);
            list.dispatch_ex(
                &self.kernels.utility("k_raster_lines"),
                &tex,
                &[],
                &p,
                Bindings {
                    buffers: &[BufferBinding::Buffer(&acc), BufferBinding::Data(seg_bytes)],
                    grid: Some((n as u32, 1)),
                    threadgroup: Some((64, 1)),
                },
            );
        }
        // Packed after the header: [4..8] color, [8] fill rule.
        let mut p = hdr.to_vec();
        p.push(self.scratch[8]);
        p.extend_from_slice(&self.scratch[4..8]);
        p.extend([rw as f32, rh as f32, groups as f32]);
        // Group colors: the node's own, or a group's color faded with it.
        let node_alpha = self.scratch[7];
        if geo.groups.is_empty() {
            p.extend_from_slice(&self.scratch[4..8]);
        }
        for g in &geo.groups {
            match g {
                None => p.extend_from_slice(&self.scratch[4..8]),
                Some(c) => {
                    let a = c[3] * node_alpha;
                    p.extend([c[0] * a, c[1] * a, c[2] * a, a]);
                }
            }
        }
        // Color glyphs: their bitmaps in the atlas, and per glyph the map
        // from frame px to its unit square.
        let records = self.glyph_records(geo, list);
        p.extend([(records.len() / 12) as f32, x0 as f32, y0 as f32, image_opacity]);
        let atlas = match (&self.glyph_atlas, records.is_empty()) {
            (Some(a), false) => a.tex.clone(),
            _ => self.placeholder.clone(),
        };
        let record_bytes: &[u8] = if records.is_empty() {
            &[0u8; 48]
        } else {
            unsafe { std::slice::from_raw_parts(records.as_ptr().cast::<u8>(), records.len() * 4) }
        };
        list.dispatch_ex(
            &self.kernels.utility("k_raster_resolve"),
            &tex,
            &[&atlas],
            &p,
            Bindings {
                buffers: &[BufferBinding::Buffer(&acc), BufferBinding::Data(record_bytes)],
                grid: Some((256, tex.desc().height)),
                threadgroup: Some((256, 1)),
            },
        );
        Placed {
            tex,
            origin: [x0 as f32 * self.view.px, y0 as f32 * self.view.px],
            scale: self.view.px,
        }
    }

    /// Runs a fused group as a chain of tier-0 kernels (one per node, through
    /// pooled temporaries) while its fused kernel compiles.
    #[allow(clippy::too_many_arguments)]
    fn encode_fallback(
        &mut self,
        plan: &Plan,
        gi: usize,
        inputs: &[Placed],
        out: &Texture,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
        res_info: &HashMap<&str, ResourceInfo>,
    ) -> Result<(), RenderError> {
        let g = &plan.groups[gi];
        let desc = out.desc();
        let mut value: HashMap<usize, Placed> = HashMap::new();
        let mut leased = Vec::new();
        let external =
            |ts: TexSource| -> Option<Placed> { g.textures.iter().position(|x| *x == ts).map(|p| inputs[p].clone()) };
        for &ni in &g.nodes {
            let pn = &plan.nodes[ni];
            let target = if ni == g.root {
                out.clone()
            } else {
                let tex = self.pool.lease(&self.device, desc);
                leased.push(tex.clone());
                tex
            };
            // Inputs: inlined nodes come from temporaries, materialized ones
            // are this group's external textures.
            let port_tex: Vec<Placed> = pn
                .inputs
                .iter()
                .map(|&src| {
                    value.get(&src).cloned().unwrap_or_else(|| {
                        external(TexSource::Group(plan.group_of[src])).unwrap_or(Placed {
                            tex: self.placeholder.clone(),
                            origin: [0.0, 0.0],
                            scale: 1.0,
                        })
                    })
                })
                .collect();
            let mut p = header(doc, t, self.view).to_vec();
            let info = if ops::uses_resource(pn.def) {
                res_info.get(pn.variant.as_str()).copied()
            } else {
                None
            };
            let (lt, on) = local_time(pn, t);
            ops::pack(pn.def, &pn.id, &doc.nodes[&pn.id], &doc.format, lt, info, &mut p)?;
            if ops::guarded(pn.def) {
                p.extend(ops::UNBOUNDED.map(|v| v as f32));
            }
            // Inactive nodes pass their destination/source through, or go
            // transparent if they have none.
            if !on {
                match ops::passthrough(pn.def, port_tex.len()) {
                    Some(i) => self.copy_placed(&port_tex[i], &target, doc, t, list)?,
                    None => list.dispatch(&self.kernels.utility("k_clear"), &target, &[], &p),
                }
                value.insert(
                    ni,
                    Placed {
                        tex: target,
                        origin: [0.0, 0.0],
                        scale: self.view.px,
                    },
                );
                continue;
            }
            match self.kernels.tier0(pn.def.func).cloned() {
                Some(t0) => {
                    let k = self.kernels.tier0_kernel(pn.def.func).map_err(RenderError::Setup)?;
                    let bound: Vec<Placed> = t0
                        .bindings
                        .iter()
                        .map(|&b| {
                            if b == usize::MAX {
                                external(TexSource::Resource(pn.variant.clone())).unwrap_or(Placed {
                                    tex: self.placeholder.clone(),
                                    origin: [0.0, 0.0],
                                    scale: 1.0,
                                })
                            } else {
                                port_tex[b].clone()
                            }
                        })
                        .collect();
                    for b in &bound {
                        p.extend(b.origin);
                        p.push(b.scale);
                    }
                    let refs: Vec<&Texture> = bound.iter().map(|b| &b.tex).collect();
                    list.dispatch(&k, &target, &refs, &p);
                }
                None => list.dispatch(&self.kernels.utility("k_clear"), &target, &[], &p),
            }
            value.insert(
                ni,
                Placed {
                    tex: target,
                    origin: [0.0, 0.0],
                    scale: self.view.px,
                },
            );
        }
        // Temporaries go back to the pool now: later frames reuse them in
        // queue order, after this frame's reads.
        for tex in leased {
            self.pool.give_back(tex);
        }
        Ok(())
    }

    /// Mean squared error (8-bit sRGB units, over black) between frame `t`
    /// and `reference` (an RGBA8 sRGB texture of the frame's size), reduced
    /// on the GPU: only one partial sum per 16x16 tile is read back.
    pub fn frame_mse(&mut self, doc: &Document, t: f64, reference: &Texture) -> Result<f64, RenderError> {
        let (w, h) = self.frame_size(doc);
        let groups = (w.div_ceil(16) * h.div_ceil(16)) as usize;
        if self.diff_buf.as_ref().is_none_or(|b| b.len() < groups * 4) {
            self.diff_buf = Some(self.device.create_buffer(groups * 4, None));
        }
        let buf = self.diff_buf.clone().unwrap();
        let (sub, _) = self.submit_frame_with(doc, t, |o| {
            let mut p = o.header.to_vec();
            p.extend(o.origin);
            o.list.dispatch_ex(
                &o.kernels.utility("k_diff"),
                o.texture,
                &[o.texture, reference],
                &p,
                Bindings {
                    buffers: &[BufferBinding::Buffer(&buf)],
                    grid: Some((w, h)),
                    threadgroup: Some((16, 16)),
                },
            );
        })?;
        sub.wait()?;
        let sum: u64 = self
            .device
            .read_buffer(&buf, groups * 4)
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as u64)
            .sum();
        Ok(sum as f64 / (w as f64 * h as f64 * 3.0))
    }

    /// Renders frame `t` as a video without alpha shows it: flattened over
    /// black by the export's own encode (sRGB RGBA8, alpha 255). Where the
    /// document draws nothing, that is black, not see-through.
    pub fn render_opaque_rgba8(&mut self, doc: &Document, t: f64) -> Result<(Vec<u8>, FrameReport), RenderError> {
        self.render_readback(doc, t, Readback::Srgb8, true)
    }

    /// Renders frame `t`, waits, and returns tightly packed sRGB RGBA8
    /// pixels with straight alpha (transparent where nothing is drawn).
    pub fn render_rgba8(&mut self, doc: &Document, t: f64) -> Result<(Vec<u8>, FrameReport), RenderError> {
        let (sub, report) = self.submit_frame(doc, t, true)?;
        sub.wait()?;
        let mut px = self.read_export();
        clear_transparent(&mut px);
        Ok((px, report))
    }

    /// Renders frame `t`, waits, and returns it tightly packed as `kind`.
    /// `opaque` flattens it over black, as a video without alpha shows it;
    /// otherwise alpha is straight.
    pub fn render_readback(
        &mut self,
        doc: &Document,
        t: f64,
        kind: Readback,
        opaque: bool,
    ) -> Result<(Vec<u8>, FrameReport), RenderError> {
        let format = match kind {
            Readback::Srgb8 => PixelFormat::Rgba8Unorm,
            Readback::SrgbHalf | Readback::LinearHalf => PixelFormat::Rgba16Float,
        };
        let mode = match (kind, opaque) {
            (Readback::LinearHalf, true) => 3.0,
            (Readback::LinearHalf, false) => 2.0,
            (_, true) => 1.0,
            (_, false) => 0.0,
        };
        let (w, h) = self.frame_size(doc);
        let ex = self.export_texture(w, h, format);
        let (sub, report) = self.submit_frame_with(doc, t, |o| {
            let mut p = o.header.to_vec();
            p.extend(o.origin);
            p.push(mode);
            o.list
                .dispatch(&o.kernels.utility("k_encode_srgb8"), &ex, &[o.texture], &p);
        })?;
        sub.wait()?;
        let mut px = self.read_export();
        if kind == Readback::Srgb8 && !opaque {
            clear_transparent(&mut px);
        }
        Ok((px, report))
    }

    /// Renders frame `t` the way the player shows it: letterboxed into a
    /// `width` x `height` view with the timeline drawn over it (`hud`: on,
    /// playhead 0..1, loop in, loop out, emphasis, playing). RGBA8, sRGB.
    pub fn render_presented(
        &mut self,
        doc: &Document,
        t: f64,
        width: u32,
        height: u32,
        hud: [f32; 6],
        marks: &[([f32; 4], f32)],
    ) -> Result<Vec<u8>, RenderError> {
        let view = self.device.create_texture(TextureDesc {
            width,
            height,
            format: PixelFormat::Rgba8Unorm,
            cpu_readable: true,
        });
        let (fw, fh) = self.frame_size(doc);
        let (fw, fh) = (fw as f32, fh as f32);
        let (dw, dh) = (width as f32, height as f32);
        let s = (dw / fw).min(dh / fh);
        let rect = [(dw - fw * s) / 2.0, (dh - fh * s) / 2.0, fw * s, fh * s];
        let (sub, _) = self.submit_frame_with(doc, t, |o| {
            let mut p = o.header.to_vec();
            p.extend(o.origin);
            p.extend(rect);
            p.extend([dw, dh, 0.07, 0.07, 0.08]);
            p.extend(hud);
            // Edit outlines: frame-space bounds mapped into the view.
            p.push(marks.len().min(4) as f32);
            for k in 0..4 {
                match marks.get(k) {
                    Some((b, a)) => p.extend([
                        rect[0] + b[0] * s - 6.0,
                        rect[1] + b[1] * s - 6.0,
                        rect[0] + b[2] * s + 6.0,
                        rect[1] + b[3] * s + 6.0,
                        *a,
                    ]),
                    None => p.extend([0.0; 5]),
                }
            }
            o.list.dispatch_ex(
                &o.kernels.utility("k_present"),
                &view,
                &[o.texture],
                &p,
                ultrafast_gpu::Bindings {
                    grid: Some((width, height)),
                    ..Default::default()
                },
            );
        })?;
        sub.wait()?;
        Ok(self.device.readback(&view))
    }

    /// Reads the last exported frame. Counted as a readback.
    pub fn read_export(&self) -> Vec<u8> {
        self.device
            .readback(self.export.as_ref().expect("no frame exported yet"))
    }
}

/// Straight-alpha RGBA8 whose alpha rounded to 0 carries no color: the
/// encoder divides color by coverage, and coverage a hair above zero (the
/// tails exact-coverage rasterization leaves along a row) would otherwise
/// paint full color into transparent pixels, invisible when composited but
/// seen as streaks by anything that reads the color channels alone.
fn clear_transparent(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        if p[3] == 0 {
            p[..3].fill(0);
        }
    }
}

type Expansion = (Document, Vec<(String, String)>);

/// Runs a node expansion on the document and inside every component (a
/// film pulled in as a component keeps its raymarch scenes and
/// transitions). None when nothing uses it.
/// Whether a node of the document or of one of its components is one of
/// `ops`.
fn uses_op(doc: &Document, ops: &[&str]) -> bool {
    let any = |nodes: &BTreeMap<String, ultrafast_graph::Node>| nodes.values().any(|n| ops.contains(&n.op.as_str()));
    any(&doc.nodes) || doc.components.values().any(|c| any(&c.nodes))
}

fn expand_everywhere(
    doc: &Document,
    used_by: fn(&Document) -> bool,
    expand: fn(&Document) -> Expansion,
) -> Option<Expansion> {
    let as_doc = |c: &ultrafast_graph::Component| Document {
        version: doc.version,
        format: doc.format,
        output: c.output.clone(),
        nodes: c.nodes.clone(),
        components: Default::default(),
        revision: 0,
        structure_revision: 0,
        instance: 0,
        unread: Vec::new(),
        short_keys: false,
    };
    let inner: Vec<&String> = doc
        .components
        .iter()
        .filter(|(_, c)| used_by(&as_doc(c)))
        .map(|(name, _)| name)
        .collect();
    if !used_by(doc) && inner.is_empty() {
        return None;
    }
    let (mut out, mut problems) = if used_by(doc) {
        expand(doc)
    } else {
        (doc.clone(), Vec::new())
    };
    for name in inner {
        let (d, p) = expand(&as_doc(&doc.components[name]));
        if let Some(c) = out.components.get_mut(name) {
            c.nodes = d.nodes;
        }
        problems.extend(p.into_iter().map(|(n, m)| (format!("{name}/{n}"), m)));
    }
    Some((out, problems))
}

/// Whether rendering `doc` goes through [`Renderer::flattened`].
fn needs_flat(doc: &Document) -> bool {
    doc.needs_flatten()
        || builtins::used_by(doc)
        || svg::used_by(doc)
        || lottie::used_by(doc)
        || captions::used_by(doc)
        || follow::used_by(doc)
        || layout::used_by(doc)
        || blocks::used_by(doc)
        || code::used_by(doc)
        || raymarch::used_by(doc)
        || transitions::used_by(doc)
        || pushdown::wanted(doc)
        || !doc.components.is_empty()
}

/// Kernel header: time, output texel size in frame pixels (the view's: 1
/// at the document's own size, 0.5 at twice it), frame width and height
/// (always the document's).
fn header(doc: &Document, t: f64, view: View) -> [f32; 4] {
    [t as f32, view.px, doc.format.width as f32, doc.format.height as f32]
}

/// Loudness of `pcm` in a band, 120 values per second, with an instant
/// attack and a `release` (seconds), normalized so the loudest moment is 1.
fn audio_envelope(pcm: &ultrafast_media::audio::Pcm, band: &str, release: f64) -> Vec<f32> {
    let rate = ultrafast_media::audio::RATE as f32;
    let one_pole = |cut: f32| 1.0 - (-2.0 * std::f32::consts::PI * cut / rate).exp();
    let (lo_a, hi_a) = match band {
        "low" => (one_pole(160.0), 0.0),
        "mid" => (one_pole(2400.0), one_pole(250.0)),
        "high" => (1.0, one_pole(4500.0)),
        _ => (1.0, 0.0),
    };
    let win = (rate / 120.0) as usize;
    let mut out = Vec::with_capacity(pcm.frames() / win + 1);
    let (mut lp, mut hp_lp, mut acc, mut n) = (0.0f32, 0.0f32, 0.0f32, 0usize);
    for f in pcm.samples.chunks_exact(ultrafast_media::audio::CHANNELS) {
        let x = (f[0] + f[1]) * 0.5;
        lp += lo_a * (x - lp);
        hp_lp += hi_a * (lp - hp_lp);
        let y = lp - hp_lp;
        acc += y * y;
        n += 1;
        if n == win {
            out.push((acc / n as f32).sqrt());
            acc = 0.0;
            n = 0;
        }
    }
    // Peak follower: rises at once, falls over `release`.
    let fall = (-1.0 / (120.0 * release.max(1e-3) as f32)).exp();
    let mut e = 0.0f32;
    for v in out.iter_mut() {
        e = v.max(e * fall);
        *v = e;
    }
    let peak = out.iter().copied().fold(1e-9f32, f32::max);
    for v in out.iter_mut() {
        *v /= peak;
    }
    out
}

/// Which groups can affect the output at time `t`: walk back from the
/// output; an inactive gated node keeps only its passthrough input alive.
fn live_groups(plan: &Plan, t: f64) -> Vec<bool> {
    let roots: Vec<usize> = plan.nodes.len().checked_sub(1).into_iter().collect();
    live_from(plan, t, &roots)
}

/// Groups needed to evaluate `roots` (node indices) at time `t`.
fn live_from(plan: &Plan, t: f64, roots: &[usize]) -> Vec<bool> {
    let n = plan.nodes.len();
    let mut live = vec![false; n];
    let mut stack: Vec<usize> = roots.to_vec();
    while let Some(i) = stack.pop() {
        if live[i] {
            continue;
        }
        live[i] = true;
        let pn = &plan.nodes[i];
        if pn.gated && !local_time(pn, t).1 {
            if let Some(k) = ops::passthrough(pn.def, pn.inputs.len()) {
                stack.push(pn.inputs[k]);
            }
            continue;
        }
        stack.extend(pn.inputs.iter().copied());
    }
    plan.groups.iter().map(|g| g.nodes.iter().any(|&m| live[m])).collect()
}

/// Local time of a node at composition time `t`, and whether it is active
/// (inside every visibility window of its timing chain).
/// Per blur group, the fused groups reading it when every reader samples
/// its inputs where it needs them (fused groups outside their loops, which
/// read layer textures texel for texel, and other blurs): such a blur can
/// stay at the reduced resolution it is computed at. Never the output.
fn low_readers(plan: &Plan) -> Vec<Option<Vec<usize>>> {
    let ng = plan.groups.len();
    (0..ng)
        .map(|g| {
            if plan.groups[g].kind != GroupKind::Blur || g + 1 == ng {
                return None;
            }
            let mut fused = Vec::new();
            for (c, gr) in plan.groups.iter().enumerate() {
                let bindings: Vec<usize> = (0..gr.textures.len())
                    .filter(|&b| gr.textures[b] == TexSource::Group(g))
                    .collect();
                if bindings.is_empty() {
                    continue;
                }
                match gr.kind {
                    GroupKind::Blur => {}
                    GroupKind::Fused
                        if !gr.runs.iter().any(|r| {
                            r.layers
                                .iter()
                                .any(|(_, _, l)| matches!(l, codegen::LayerRef::Texture(b) if bindings.contains(b)))
                        }) =>
                    {
                        fused.push(c)
                    }
                    _ => return None,
                }
            }
            Some(fused)
        })
        .collect()
}

/// A group's output as its readers see it (through passthroughs).
fn placed_output(active: &Active, mut gi: usize) -> Placed {
    while let Some(src) = active.alias[gi] {
        gi = src;
    }
    match &active.raster[gi] {
        Some(p) => p.clone(),
        None => Placed {
            tex: active.slots[active.plan.groups[gi].output].clone(),
            origin: [0.0, 0.0],
            scale: active.view.px,
        },
    }
}

/// How far (frame px) a `transform` node moves what it shows between local
/// times l0 and l1: the largest shift of any point of the frame.
fn transform_motion_px(node: &ultrafast_graph::Node, fmt: &ultrafast_graph::Format, l0: f64, l1: f64) -> f64 {
    let a0 = ops::transform_affine(node, fmt, l0);
    let a1 = ops::transform_affine(node, fmt, l1);
    let Some(back) = a0.inverse() else {
        return f64::INFINITY;
    };
    let m = a1.mul(back);
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
        .iter()
        .map(|&(x, y)| {
            let (u, v) = m.apply(x, y);
            (u - x).hypot(v - y)
        })
        .fold(0.0, f64::max)
}

/// The absorbed transforms of a raster chain as one affine at time `t`. A
/// timed transform outside its window passes its source through, so it adds
/// nothing.
pub(crate) fn absorbed(plan: &Plan, doc: &Document, transforms: &[usize], t: f64) -> ops::Affine {
    transforms.iter().fold(ops::Affine::IDENTITY, |acc, &ti| {
        let tn = &plan.nodes[ti];
        match local_time(tn, t) {
            (lt, true) => ops::transform_affine(&doc.nodes[&tn.id], &doc.format, lt).mul(acc),
            (_, false) => acc,
        }
    })
}

/// Per plan node, where its output is fully opaque at `t` ([`ops::opaque`],
/// in its own space): an inactive node shows its passthrough input, or
/// nothing.
pub(crate) fn opaque_rects(plan: &Plan, doc: &Document, t: f64) -> Vec<ops::Rect> {
    let mut out = vec![ops::EMPTY; plan.nodes.len()];
    for (i, pn) in plan.nodes.iter().enumerate() {
        let ins: Vec<ops::Rect> = pn.inputs.iter().map(|&c| out[c]).collect();
        out[i] = match (local_time(pn, t), doc.nodes.get(&pn.id)) {
            ((lt, true), Some(node)) => ops::opaque(pn.def, node, &doc.format, lt, &ins),
            ((_, true), None) => ops::EMPTY,
            ((_, false), _) => ops::passthrough(pn.def, ins.len())
                .map(|k| ins[k])
                .unwrap_or(ops::EMPTY),
        };
    }
    out
}

fn local_time(n: &plan::PlanNode, t: f64) -> (f64, bool) {
    match ultrafast_graph::resolve_chain(&n.chain, t) {
        Some(lt) => (lt, true),
        None => (t, false),
    }
}

fn pn_has(node: &ultrafast_graph::Node, param: &str) -> bool {
    node.params.contains_key(param)
}

fn f32_bytes(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

fn u32_bytes(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

/// Hash of every parameter a 3D pass depends on, for static caching.
fn scene_key(plan: &Plan, g: &plan::Group, doc: &Document, t: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for &m in &g.nodes {
        let pn = &plan.nodes[m];
        let (lt, on) = local_time(pn, t);
        on.hash(&mut h);
        geom::geometry_key(&doc.nodes[&pn.id], lt, &ops::Affine::IDENTITY).hash(&mut h);
    }
    h.finish()
}

/// Bounds of node `ni`'s output as seen inside group `g` (for guards).
/// Nodes outside the group are textures: their bounds are the placed
/// texture's extent.
fn subtree_bounds(
    plan: &Plan,
    g: &plan::Group,
    ni: usize,
    doc: &Document,
    t: f64,
    inputs: &[Placed],
    res_info: &HashMap<&str, ResourceInfo>,
    content: &[ops::Rect],
) -> ops::Rect {
    if !plan.in_group(plan.group_of[g.root], ni) {
        let gs = plan.group_of[ni];
        return match g.textures.iter().position(|ts| *ts == TexSource::Group(gs)) {
            Some(i) => intersect(extent(&inputs[i]), content[gs]),
            None => ops::UNBOUNDED,
        };
    }
    let pn = &plan.nodes[ni];
    let (lt, on) = local_time(pn, t);
    let mut buf = [ops::EMPTY; 8];
    let heap: Vec<ops::Rect>;
    let ins: &[ops::Rect] = if pn.inputs.len() <= buf.len() {
        for (k, &c) in pn.inputs.iter().enumerate() {
            buf[k] = subtree_bounds(plan, g, c, doc, t, inputs, res_info, content);
        }
        &buf[..pn.inputs.len()]
    } else {
        heap = pn
            .inputs
            .iter()
            .map(|&c| subtree_bounds(plan, g, c, doc, t, inputs, res_info, content))
            .collect();
        &heap
    };
    if !on {
        return ops::passthrough(pn.def, ins.len())
            .map(|i| ins[i])
            .unwrap_or(ops::EMPTY);
    }
    let info = if ops::uses_resource(pn.def) {
        res_info.get(pn.variant.as_str()).copied()
    } else {
        None
    };
    ops::bounds(pn.def, &doc.nodes[&pn.id], &doc.format, lt, ins, info)
}

/// The earlier group whose output equals fused group `g`'s at `t`, when
/// all `g` would do is pass it through: inactive layers and effects pass
/// their source, a composite over an empty foreground its background.
#[allow(clippy::too_many_arguments)]
fn passthrough_group(
    plan: &Plan,
    g: &plan::Group,
    ni: usize,
    doc: &Document,
    t: f64,
    inputs: &[Placed],
    res_info: &HashMap<&str, ResourceInfo>,
    content: &[ops::Rect],
) -> Option<usize> {
    if !plan.in_group(plan.group_of[g.root], ni) {
        let gs = plan.group_of[ni];
        return g.textures.contains(&TexSource::Group(gs)).then_some(gs);
    }
    let pn = &plan.nodes[ni];
    if !local_time(pn, t).1 {
        let i = ops::passthrough(pn.def, pn.inputs.len())?;
        return passthrough_group(plan, g, pn.inputs[i], doc, t, inputs, res_info, content);
    }
    if pn.def.name == "over" && pn.inputs.len() == 2 {
        let fg = subtree_bounds(plan, g, pn.inputs[0], doc, t, inputs, res_info, content);
        if fg[0] >= fg[2] || fg[1] >= fg[3] {
            return passthrough_group(plan, g, pn.inputs[1], doc, t, inputs, res_info, content);
        }
    }
    None
}

/// Frame extent of a placed texture.
fn extent(p: &Placed) -> ops::Rect {
    let d = p.tex.desc();
    let (x, y, s) = (p.origin[0] as f64, p.origin[1] as f64, p.scale as f64);
    [x, y, x + d.width as f64 * s, y + d.height as f64 * s]
}

fn intersect(a: ops::Rect, b: ops::Rect) -> ops::Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reveal_keyed_short_of_its_text_is_flagged() {
        let text = |reveal: &str| -> ultrafast_graph::Node {
            serde_json::from_str(&format!(r#"{{"op":"text","params":{{"text":"x","reveal":{reveal}}}}}"#)).unwrap()
        };
        // The title template's subtitle: 24 glyphs typed in.
        let typed = text(r#"{"keys":[{"t":1,"v":0},{"t":2,"v":24}]}"#);
        assert_eq!(reveal_falls_short(&typed, 24), None);
        assert_eq!(reveal_falls_short(&typed, 20), None);
        let m = reveal_falls_short(&typed, 31).unwrap();
        assert!(
            m.starts_with("text: ") && m.contains("the last 7 of its 31 glyphs never show") && m.ends_with("up to 31"),
            "{m}"
        );
        // A soft reveal needs its softness past the last glyph: type.json's
        // 33 glyphs at softness 4 stopped at 34, leaving "PU" faded and low.
        let soft = text(r#"{"keys":[{"t":0,"v":0},{"t":2,"v":34}]},"reveal_softness":4"#);
        let m = reveal_falls_short(&soft, 33).unwrap();
        assert!(
            m.contains("the last 2 of its 33 glyphs") && m.ends_with("up to 36"),
            "{m}"
        );
        assert_eq!(
            Diagnostic {
                node: "sub".into(),
                message: m
            }
            .severity(),
            "warning"
        );
        // A fixed reveal shows part of the text on purpose.
        assert_eq!(reveal_falls_short(&text("5"), 31), None);
    }
}
