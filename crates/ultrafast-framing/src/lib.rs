//! Face-following clip camera.
//!
//! Given the face tracks and shot boundaries a face-detection worker
//! produces for a source video, this computes, for every output frame, the
//! crop of the source that keeps the speaker framed in a vertical (or any)
//! composition: which face is the subject (talk, size and focus scores with
//! hand-off guards and look-ahead), a stabilized subject box, an anchored
//! camera that holds inside a dead zone and eases with bounded acceleration,
//! snaps on shot cuts and speaker changes, a per-shot zoom for letterboxed
//! footage, and a "protect" mode (blurred cover behind the whole frame) for
//! shots a crop would ruin.
//!
//! It follows JavaScript number semantics (`Math.round`, `Math.hypot`), so a
//! port to the web computes the same crops frame for frame. It is a pure function of its inputs: the whole track is
//! computed once (it is stateful from frame 0) and sampled per frame.

#![allow(clippy::too_many_arguments)]

use serde_json::Value;
use std::collections::HashMap;

mod anchor;
pub use anchor::{AnchorProfile, AnchorState, advance_anchor, anchor_profile};

pub mod faces;
pub mod follow;

/// `Math.round`: halves round toward +infinity.
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// `Math.hypot` as JavaScriptCore computes it (scaled, Kahan-summed), so
/// distance thresholds tip exactly where they do in the reference.
pub fn js_hypot(a: f64, b: f64) -> f64 {
    if a.is_infinite() || b.is_infinite() {
        return f64::INFINITY;
    }
    let mut max = a.abs().max(b.abs());
    if max.is_nan() {
        return f64::NAN;
    }
    if max == 0.0 {
        max = 1.0;
    }
    let mut compensation = 0.0;
    let mut sum = 0.0f64;
    for v in [a, b] {
        let s = v / max;
        let summand = s * s - compensation;
        let preliminary = sum + summand;
        compensation = (preliminary - sum) - summand;
        sum = preliminary;
    }
    sum.sqrt() * max
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// A detected face in normalized source coordinates (0..1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FaceBox {
    pub id: Option<i64>,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Speaking activity.
    pub talk: Option<f64>,
    /// Sharpness 0..1.
    pub focus: Option<f64>,
    pub focus_state: Option<bool>,
}

impl FaceBox {
    fn talk0(&self) -> f64 {
        self.talk.unwrap_or(0.0).max(0.0)
    }
    fn focus01(&self) -> f64 {
        clamp01(self.focus.unwrap_or(0.0))
    }
    fn area(&self) -> f64 {
        (self.w * self.h).max(0.0)
    }
    fn cx(&self) -> f64 {
        self.x + self.w / 2.0
    }
    fn cy(&self) -> f64 {
        self.y + self.h / 2.0
    }
}

#[derive(Debug, Clone)]
pub struct FaceFrame {
    pub frame: i64,
    pub faces: Vec<FaceBox>,
}

#[derive(Debug, Clone)]
pub struct Shot {
    pub start_frame: i64,
    pub end_frame: i64,
    pub start_boundary_kind: Option<String>,
    pub protect: bool,
    pub protect_decision: Option<bool>,
    /// Cinematic-scope push-in (clamped to the config's maximum).
    pub recommended_zoom: Option<f64>,
}

/// Face tracks and shots for one source video.
#[derive(Debug, Clone)]
pub struct Faces {
    pub source_width: Option<f64>,
    pub source_height: Option<f64>,
    pub fps: f64,
    pub frames: Vec<FaceFrame>,
    pub shots: Vec<Shot>,
}

/// Tuning. The defaults are the values production renders use.
#[derive(Debug, Clone)]
pub struct Config {
    pub avg_window_now_sec: f64,
    pub avg_window_prev_sec: f64,
    pub deadzone_base_min: f64,
    pub clamp_base_px: f64,
    pub anti_flip_lerp: f64,
    pub id_switch_cut_dist: f64,
    pub cinematic_zoom_max: f64,
    pub enable_translate_smoothing: bool,
    pub enable_primary_stabilizer: bool,
    pub enable_shot_anchor_camera: bool,
    pub enable_ema_single: bool,
    pub ema_alpha_single: f64,
    pub max_translate_delta_px: f64,
    pub primary_talk_weight: f64,
    pub primary_shot_talk_weight: f64,
    pub primary_area_weight: f64,
    pub primary_focus_weight: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            avg_window_now_sec: 0.0,
            avg_window_prev_sec: 0.0,
            deadzone_base_min: 48.0,
            clamp_base_px: 0.0,
            anti_flip_lerp: 0.85,
            id_switch_cut_dist: 0.22,
            cinematic_zoom_max: 1.35,
            enable_translate_smoothing: true,
            enable_primary_stabilizer: true,
            enable_shot_anchor_camera: true,
            enable_ema_single: false,
            ema_alpha_single: 0.15,
            max_translate_delta_px: 20.0,
            primary_talk_weight: 100.0,
            primary_shot_talk_weight: 5.0,
            primary_area_weight: 2.0,
            primary_focus_weight: 2.0,
        }
    }
}

impl Config {
    /// Defaults with the overrides a props `config` object carries.
    pub fn from_json(v: Option<&Value>) -> Self {
        let mut c = Self::default();
        let Some(o) = v.and_then(Value::as_object) else {
            return c;
        };
        let num = |k: &str, d: f64| o.get(k).and_then(Value::as_f64).unwrap_or(d);
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        c.avg_window_now_sec = num("avgWindowNowSec", c.avg_window_now_sec);
        c.avg_window_prev_sec = num("avgWindowPrevSec", c.avg_window_prev_sec);
        c.deadzone_base_min = num("deadzoneBaseMin", c.deadzone_base_min);
        c.clamp_base_px = num("clampBasePx", c.clamp_base_px);
        c.anti_flip_lerp = num("antiFlipLerp", c.anti_flip_lerp);
        c.id_switch_cut_dist = num("idSwitchCutDist", c.id_switch_cut_dist);
        c.cinematic_zoom_max = num("cinematicZoomMax", c.cinematic_zoom_max);
        c.enable_translate_smoothing = flag("enableTranslateSmoothing", c.enable_translate_smoothing);
        c.enable_primary_stabilizer = flag("enablePrimaryStabilizer", c.enable_primary_stabilizer);
        c.enable_shot_anchor_camera = flag("enableShotAnchorCamera", c.enable_shot_anchor_camera);
        c.enable_ema_single = flag("enableEmaSingle", c.enable_ema_single);
        c.ema_alpha_single = num("emaAlphaSingle", c.ema_alpha_single);
        c.max_translate_delta_px = num("maxTranslateDeltaPx", c.max_translate_delta_px);
        c.primary_talk_weight = num("primaryTalkWeight", c.primary_talk_weight);
        c.primary_shot_talk_weight = num("primaryShotTalkWeight", c.primary_shot_talk_weight);
        c.primary_area_weight = num("primaryAreaWeight", c.primary_area_weight);
        c.primary_focus_weight = num("primaryFocusWeight", c.primary_focus_weight);
        c
    }
}

fn finite(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite())
}

impl Faces {
    /// Reads a face-worker payload (`{sourceWidth, sourceHeight, fps,
    /// frames: [{frame | timeSeconds | t, faces: [{id, x, y, w, h, talk,
    /// focus, focusState}]}], shots: [...]}`), normalized the way the
    /// reference does (frames from times, sorted; shot frames from times).
    pub fn from_json(v: &Value, comp_fps: f64, cinematic_zoom_max: f64) -> Option<Self> {
        let o = v.as_object()?;
        let source_fps = finite(o.get("fps")).filter(|f| *f > 0.0).unwrap_or(comp_fps);
        let mut frames: Vec<FaceFrame> = o
            .get("frames")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|fr| {
                        let raw_frame = finite(fr.get("frame")).map(js_round);
                        let raw_time = finite(fr.get("timeSeconds")).or_else(|| finite(fr.get("t")));
                        let frame = raw_frame.or(raw_time.map(|t| js_round(t * source_fps))).unwrap_or(0.0) as i64;
                        let faces = fr
                            .get("faces")
                            .and_then(Value::as_array)
                            .map(|fs| fs.iter().map(parse_face).collect())
                            .unwrap_or_default();
                        FaceFrame { frame, faces }
                    })
                    .collect()
            })
            .unwrap_or_default();
        frames.sort_by_key(|f| f.frame);
        let mut shots: Vec<Shot> = o
            .get("shots")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|s| parse_shot(s, source_fps, cinematic_zoom_max))
                    .collect()
            })
            .unwrap_or_default();
        shots.sort_by_key(|s| s.start_frame);
        Some(Self {
            source_width: o.get("sourceWidth").and_then(Value::as_f64),
            source_height: o.get("sourceHeight").and_then(Value::as_f64),
            fps: source_fps,
            frames,
            shots,
        })
    }
}

fn parse_face(f: &Value) -> FaceBox {
    let n = |k: &str| f.get(k).and_then(Value::as_f64);
    FaceBox {
        id: f
            .get("id")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|x| x as i64))),
        x: n("x").unwrap_or(f64::NAN),
        y: n("y").unwrap_or(f64::NAN),
        w: n("w").unwrap_or(f64::NAN),
        h: n("h").unwrap_or(f64::NAN),
        talk: n("talk"),
        focus: n("focus"),
        focus_state: f.get("focusState").and_then(Value::as_bool),
    }
}

fn parse_shot(s: &Value, source_fps: f64, zoom_max: f64) -> Shot {
    let raw_start = finite(s.get("startFrame")).map(js_round);
    let raw_end = finite(s.get("endFrame")).map(js_round);
    let start_time = finite(s.get("startTimeSec"))
        .or_else(|| finite(s.get("start")))
        .unwrap_or(0.0);
    let end_time = finite(s.get("endTimeSec"))
        .or_else(|| finite(s.get("end")))
        .unwrap_or(start_time);
    let start_frame = raw_start.unwrap_or_else(|| js_round(start_time * source_fps));
    let end_raw = raw_end.unwrap_or_else(|| js_round(end_time * source_fps));
    let end_frame = if end_raw > start_frame {
        end_raw
    } else {
        start_frame + 1.0
    };
    let protect_raw = s.get("protect");
    let protect = protect_raw == Some(&Value::Bool(true));
    let protect_decision = match s.get("protectDecision").and_then(Value::as_bool) {
        Some(b) => Some(b),
        None => match protect_raw {
            Some(Value::Bool(true)) => Some(true),
            Some(Value::Bool(false)) => Some(false),
            _ => None,
        },
    };
    let recommended_zoom = s
        .get("cinematicScope")
        .and_then(|c| finite(c.get("recommendedZoom")))
        .filter(|z| *z > 0.0)
        .map(|z| zoom_max.min(z.max(1.0)));
    Shot {
        start_frame: start_frame as i64,
        end_frame: end_frame as i64,
        start_boundary_kind: s.get("startBoundaryKind").and_then(Value::as_str).map(str::to_string),
        protect,
        protect_decision,
        recommended_zoom,
    }
}

/// An axis-aligned rectangle in composition pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One drawn copy of the source: `source` is where the whole source frame
/// lands (scaled), clipped to `viewport`.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraLayer {
    pub viewport: Rect,
    pub source: Rect,
    /// Gaussian blur radius in composition px and a brightness multiplier
    /// (the protect-mode backdrop).
    pub blur_px: f64,
    pub brightness: f64,
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CameraFrame {
    pub protect: bool,
    pub zoom: f64,
    pub layers: Vec<CameraLayer>,
}

/// The clip a camera track is for.
#[derive(Debug, Clone)]
pub struct Clip {
    /// Seconds into the source.
    pub start: f64,
    pub end: f64,
    /// Output frames (default: (end - start) at `fps`).
    pub frames: usize,
    pub fps: f64,
    pub width: f64,
    pub height: f64,
}

impl Clip {
    /// From Short props: `start`, `end`, `durationInFrames`, at 30 fps into
    /// 1080x1920.
    pub fn from_props(p: &Value) -> Self {
        let fps = 30.0;
        let start = p.get("start").and_then(Value::as_f64).unwrap_or(0.0);
        let end = p.get("end").and_then(Value::as_f64).unwrap_or(30.0);
        let frames = p
            .get("durationInFrames")
            .and_then(Value::as_f64)
            .unwrap_or((end - start) * fps);
        Self {
            start,
            end,
            frames: js_round(frames).max(1.0) as usize,
            fps,
            width: 1080.0,
            height: 1920.0,
        }
    }
}

/// The camera for every frame of `clip`.
pub fn camera_track(faces: Option<&Faces>, clip: &Clip, cfg: &Config) -> Vec<CameraFrame> {
    let mut e = Engine::new(faces, clip, cfg);
    (0..clip.frames).map(|f| e.frame(f as i64)).collect()
}

/// The camera for every frame of `clip`, each with the id of the face it
/// follows there: `None` in a protected shot and wherever it follows no face
/// (it holds, or frames the face the shot shows next).
pub fn camera_track_subjects(faces: Option<&Faces>, clip: &Clip, cfg: &Config) -> Vec<(CameraFrame, Option<i64>)> {
    let mut e = Engine::new(faces, clip, cfg);
    (0..clip.frames as i64)
        .map(|f| {
            let camera = e.frame(f);
            // The subject is filled forward from frame 0 and cached, so
            // reading it after the frame leaves the camera as it was.
            let subject = if camera.protect {
                None
            } else {
                let abs = e.media_start + f;
                e.stable_primary(abs).and_then(|s| s.id)
            };
            (camera, subject)
        })
        .collect()
}

/// Everything a camera track reads, from Short props JSON.
pub fn camera_track_from_props(props: &Value) -> Vec<CameraFrame> {
    let cfg = Config::from_json(props.get("config"));
    let clip = Clip::from_props(props);
    let faces = props
        .get("faces")
        .and_then(|f| Faces::from_json(f, clip.fps, cfg.cinematic_zoom_max));
    camera_track(faces.as_ref(), &clip, &cfg)
}

#[derive(Debug, Clone, Copy)]
struct Span {
    start: i64,
    end: i64,
}

#[derive(Debug, Clone, Default)]
struct Stabilizer {
    last_frame: i64,
    current: Option<FaceBox>,
    active_id: Option<i64>,
    switch_origin: Option<FaceBox>,
    switch_target: Option<FaceBox>,
    last_switch_frame: i64,
}

impl Stabilizer {
    fn new() -> Self {
        Self {
            last_frame: -1,
            last_switch_frame: -1,
            ..Default::default()
        }
    }
}

struct Neighbors {
    a: usize,
    b: usize,
    t: f64,
}

#[derive(Clone)]
struct Forward {
    face: FaceBox,
}

struct Engine<'a> {
    faces: Option<&'a Faces>,
    cfg: &'a Config,
    fps: f64,
    comp_w: f64,
    comp_h: f64,
    src_w: f64,
    src_h: f64,
    media_start: i64,
    media_end_exclusive: i64,
    spans: Vec<Span>,
    has_any_faces: bool,
    talk_avg: HashMap<(i64, i64), HashMap<i64, f64>>,
    shot_has_faces: HashMap<(i64, i64), bool>,
    stable: Vec<Option<FaceBox>>,
    stab: Stabilizer,
    forward: HashMap<i64, Option<Forward>>,
    // Smoothing (the reference's caches, filled forward from frame 0).
    smooth: Vec<Option<f64>>,
    anchor_states: HashMap<i64, AnchorState>,
    skip_smoothing: HashMap<i64, bool>,
    ema: (Option<f64>, Option<String>),
}

const NO_SHOT: usize = usize::MAX;

impl<'a> Engine<'a> {
    fn new(faces: Option<&'a Faces>, clip: &Clip, cfg: &'a Config) -> Self {
        let fps = clip.fps;
        let eps = 1e-6;
        let media_start = (clip.start * fps + eps).floor().max(0.0) as i64;
        let media_end_exclusive = ((clip.end * fps + eps).floor() as i64).max(media_start + 1);
        let spans = faces
            .map(|f| {
                let mut v: Vec<Span> = f
                    .shots
                    .iter()
                    .map(|s| {
                        let start = s.start_frame.max(0);
                        let end = if s.end_frame > start { s.end_frame } else { start + 1 };
                        Span { start, end }
                    })
                    .collect();
                // Shots are already sorted by start frame; the clamp to 0 keeps order.
                v.sort_by_key(|s| s.start);
                v
            })
            .unwrap_or_default();
        let has_any_faces = faces.is_some_and(|f| f.frames.iter().any(|fr| !fr.faces.is_empty()));
        Self {
            faces,
            cfg,
            fps,
            comp_w: clip.width,
            comp_h: clip.height,
            src_w: faces.and_then(|f| f.source_width).unwrap_or(1920.0),
            src_h: faces.and_then(|f| f.source_height).unwrap_or(1080.0),
            media_start,
            media_end_exclusive,
            spans,
            has_any_faces,
            talk_avg: HashMap::new(),
            shot_has_faces: HashMap::new(),
            stable: Vec::new(),
            stab: Stabilizer::new(),
            forward: HashMap::new(),
            smooth: Vec::new(),
            anchor_states: HashMap::new(),
            skip_smoothing: HashMap::new(),
            ema: (None, None),
        }
    }

    fn frames(&self) -> &'a [FaceFrame] {
        self.faces.map(|f| f.frames.as_slice()).unwrap_or(&[])
    }

    fn seconds_to_frames(&self, s: f64) -> i64 {
        if !s.is_finite() {
            return 0;
        }
        js_round(s * self.fps).max(0.0) as i64
    }

    // ---- shots

    /// Index into `spans` (and the faces' shot list) of the shot holding `abs`.
    fn shot_at(&self, abs: i64) -> usize {
        if self.spans.is_empty() {
            return NO_SHOT;
        }
        let frame = abs.max(0);
        // Last span starting at or before `frame`.
        let (mut lo, mut hi, mut best) = (0i64, self.spans.len() as i64 - 1, -1i64);
        while lo <= hi {
            let mid = (lo + hi) >> 1;
            if self.spans[mid as usize].start <= frame {
                best = mid;
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        if best >= 0 && frame < self.spans[best as usize].end {
            best as usize
        } else {
            NO_SHOT
        }
    }

    fn shot(&self, i: usize) -> Option<&'a Shot> {
        if i == NO_SHOT {
            None
        } else {
            self.faces.map(|f| &f.shots[i])
        }
    }

    fn span(&self, i: usize) -> Span {
        self.spans[i]
    }

    fn shot_key(&self, i: usize) -> (i64, i64) {
        let s = self.span(i);
        (s.start, s.end)
    }

    fn at_shot_start(&self, abs: i64) -> bool {
        let s = self.shot_at(abs);
        s != NO_SHOT && abs.max(0) == self.span(s).start
    }

    fn shot_zoom(&self, shot: usize) -> f64 {
        match self.shot(shot).and_then(|s| s.recommended_zoom) {
            Some(z) if z.is_finite() && z > 1.0 => self.cfg.cinematic_zoom_max.min(z.max(1.0)),
            _ => 1.0,
        }
    }

    fn is_protect(&self, abs: i64) -> bool {
        match self.shot(self.shot_at(abs)) {
            None => false,
            Some(s) => s.protect_decision.unwrap_or(s.protect),
        }
    }

    // ---- face samples

    /// Samples around `target` that never cross the shot holding it.
    fn neighbors(&self, target: i64) -> Option<Neighbors> {
        let key = target.max(0);
        let frames = self.frames();
        if frames.is_empty() {
            return None;
        }
        let shot = self.shot_at(key);
        let (start, end) = if shot == NO_SHOT {
            (i64::MIN, i64::MAX)
        } else {
            let s = self.span(shot);
            (s.start, s.end)
        };
        let inside = |f: i64| f >= start && f < end;
        if !inside(key) {
            return None;
        }
        let (mut lo, mut hi) = (0usize, frames.len());
        while lo < hi {
            let mid = (lo + hi) >> 1;
            if frames[mid].frame < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < frames.len() && frames[lo].frame == key && inside(frames[lo].frame) {
            return Some(Neighbors { a: lo, b: lo, t: 0.0 });
        }
        let mut li = lo as i64 - 1;
        while li >= 0 && frames[li as usize].frame >= end {
            li -= 1;
        }
        let lower = (li >= 0 && inside(frames[li as usize].frame)).then_some(li as usize);
        let mut ui = lo;
        while ui < frames.len() && frames[ui].frame < start {
            ui += 1;
        }
        let upper = (ui < frames.len() && inside(frames[ui].frame)).then_some(ui);
        match (lower, upper) {
            (None, None) => None,
            (None, Some(u)) => Some(Neighbors { a: u, b: u, t: 0.0 }),
            (Some(l), None) => Some(Neighbors { a: l, b: l, t: 0.0 }),
            (Some(l), Some(u)) => {
                let span = (frames[u].frame - frames[l].frame).max(1) as f64;
                let t = ((key - frames[l].frame) as f64 / span).clamp(0.0, 1.0);
                Some(Neighbors { a: l, b: u, t })
            }
        }
    }

    fn faces_at_raw(&self, abs: i64) -> &'a [FaceBox] {
        let Some(nb) = self.neighbors(abs) else {
            return &[];
        };
        let frames = self.frames();
        let shot = self.shot_at(abs);
        if shot == NO_SHOT {
            return if nb.t <= 0.5 {
                &frames[nb.a].faces
            } else {
                &frames[nb.b].faces
            };
        }
        let s = self.span(shot);
        let inside = |f: i64| f >= s.start && f < s.end;
        let (a_in, b_in) = (inside(frames[nb.a].frame), inside(frames[nb.b].frame));
        if a_in && !b_in {
            return &frames[nb.a].faces;
        }
        if b_in && !a_in {
            return &frames[nb.b].faces;
        }
        if !a_in && !b_in {
            return &[];
        }
        if nb.t <= 0.5 {
            &frames[nb.a].faces
        } else {
            &frames[nb.b].faces
        }
    }

    /// Faces big and sharp enough to be the subject.
    fn eligible(&self, faces: &[FaceBox]) -> Vec<FaceBox> {
        let max_area = faces.iter().map(|f| f.w * f.h).fold(f64::NEG_INFINITY, f64::max);
        if !max_area.is_finite() || max_area <= 0.0 {
            return faces.to_vec();
        }
        let max_focus = faces.iter().map(|f| f.focus01()).fold(f64::NEG_INFINITY, f64::max);
        let has_focused = faces.iter().any(|f| f.focus_state == Some(true));
        faces
            .iter()
            .filter(|f| {
                let rel = f.area() / max_area;
                if rel < 0.3 {
                    return false;
                }
                let focus = f.focus01();
                if focus < max_focus * 0.55 && rel < 0.85 {
                    return false;
                }
                !(has_focused && f.focus_state == Some(false) && rel < 0.75)
            })
            .cloned()
            .collect()
    }

    /// Average talk per face id over a shot (frames without the face count as 0).
    fn shot_talk(&mut self, shot: usize) -> HashMap<i64, f64> {
        let key = self.shot_key(shot);
        if let Some(m) = self.talk_avg.get(&key) {
            return m.clone();
        }
        let mut total = 0usize;
        let mut sum: HashMap<i64, f64> = HashMap::new();
        for fr in self.frames() {
            if fr.frame < key.0 || fr.frame >= key.1 {
                continue;
            }
            total += 1;
            for f in &fr.faces {
                if let Some(id) = f.id {
                    *sum.entry(id).or_insert(0.0) += f.talk0();
                }
            }
        }
        let avg: HashMap<i64, f64> = if total > 0 {
            sum.into_iter().map(|(k, v)| (k, v / total as f64)).collect()
        } else {
            HashMap::new()
        };
        self.talk_avg.insert(key, avg.clone());
        avg
    }

    fn score(&mut self, face: &FaceBox, shot: usize) -> f64 {
        let mut shot_talk = 0.0;
        if shot != NO_SHOT
            && self.cfg.primary_shot_talk_weight > 0.0
            && let Some(id) = face.id
        {
            shot_talk = self.shot_talk(shot).get(&id).copied().unwrap_or(0.0).max(0.0);
        }
        face.talk0() * self.cfg.primary_talk_weight
            + face.area() * self.cfg.primary_area_weight
            + face.focus01() * self.cfg.primary_focus_weight
            + shot_talk * self.cfg.primary_shot_talk_weight
    }

    /// The top `count` faces by talk, size, focus and shot talk.
    fn top_by_talk(&mut self, faces: &[FaceBox], count: usize, shot: usize) -> Vec<FaceBox> {
        let avg = if shot != NO_SHOT {
            Some(self.shot_talk(shot))
        } else {
            None
        };
        let w = self.cfg.clone();
        let key = |f: &FaceBox| {
            let agg = match (&avg, f.id) {
                (Some(m), Some(id)) if w.primary_shot_talk_weight > 0.0 => m.get(&id).copied().unwrap_or(0.0).max(0.0),
                _ => 0.0,
            };
            let score = f.talk0() * w.primary_talk_weight
                + f.area() * w.primary_area_weight
                + f.focus01() * w.primary_focus_weight
                + agg * w.primary_shot_talk_weight;
            (score, f.talk0(), f.focus01(), f.area())
        };
        let mut v: Vec<(usize, (f64, f64, f64, f64))> = faces.iter().map(key).enumerate().collect();
        // The reference's comparator: score, then talk, then focus, then
        // area, all descending; ties keep input order (stable sort).
        v.sort_by(|(_, a), (_, b)| {
            let c = |x: f64, y: f64| y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal);
            if a.0 != b.0 {
                return c(a.0, b.0);
            }
            if a.1 != b.1 {
                return c(a.1, b.1);
            }
            if a.2 != b.2 {
                return c(a.2, b.2);
            }
            c(a.3, b.3)
        });
        v.into_iter().take(count).map(|(i, _)| faces[i].clone()).collect()
    }

    fn interpolate(f0: &FaceBox, f1: &FaceBox, t: f64) -> FaceBox {
        let wc = lerp(f0.w, f1.w, t);
        let cc = lerp(f0.cx(), f1.cx(), t);
        let x = (cc - wc / 2.0).clamp(0.0, 1.0);
        let w = wc.min(1.0 - x).max(0.0);
        FaceBox {
            x,
            y: lerp(f0.y, f1.y, t),
            w,
            h: lerp(f0.h, f1.h, t),
            id: if t <= 0.5 { f0.id } else { f1.id },
            talk: Some(f0.talk.unwrap_or(0.0) * (1.0 - t) + f1.talk.unwrap_or(0.0) * t),
            focus: Some(f0.focus.unwrap_or(0.0) * (1.0 - t) + f1.focus.unwrap_or(0.0) * t),
            focus_state: if t <= 0.5 { f0.focus_state } else { f1.focus_state },
        }
    }

    fn interpolated_primary(&mut self, abs: i64) -> Option<FaceBox> {
        let nb = self.neighbors(abs)?;
        let shot = self.shot_at(abs);
        let frames = self.frames();
        let ea = self.eligible(&frames[nb.a].faces);
        let eb = self.eligible(&frames[nb.b].faces);
        let a = self.top_by_talk(&ea, 1, shot);
        let b = self.top_by_talk(&eb, 1, shot);
        match (a.first(), b.first()) {
            (None, None) => None,
            (None, Some(b)) => Some(b.clone()),
            (Some(a), None) => Some(a.clone()),
            (Some(a), Some(b)) => Some(Self::interpolate(a, b, nb.t)),
        }
    }

    fn candidates(&mut self, abs: i64) -> Vec<FaceBox> {
        let Some(nb) = self.neighbors(abs) else {
            return Vec::new();
        };
        let shot = self.shot_at(abs);
        let frames = self.frames();
        let ea = self.eligible(&frames[nb.a].faces);
        let eb = self.eligible(&frames[nb.b].faces);
        let a = self.top_by_talk(&ea, 2, shot);
        let b = self.top_by_talk(&eb, 2, shot);
        let mut out = Vec::new();
        if let (Some(x), Some(y)) = (a.first(), b.first()) {
            out.push(Self::interpolate(x, y, nb.t));
        }
        if let (Some(x), Some(y)) = (a.get(1), b.get(1)) {
            out.push(Self::interpolate(x, y, nb.t));
        }
        if out.is_empty()
            && let Some(x) = a.first()
        {
            out.push(x.clone());
        }
        out
    }

    fn center_distance(a: &FaceBox, b: &FaceBox) -> f64 {
        js_hypot(a.cx() - b.cx(), (a.cy() - b.cy()) * 0.7)
    }

    // ---- frame index helpers

    fn frame_index(&self, abs: i64) -> Option<usize> {
        let frames = self.frames();
        if frames.is_empty() {
            return None;
        }
        let (mut lo, mut hi) = (0i64, frames.len() as i64 - 1);
        while lo <= hi {
            let mid = (lo + hi) >> 1;
            let fm = frames[mid as usize].frame;
            if fm < abs {
                lo = mid + 1;
            } else if fm > abs {
                hi = mid - 1;
            } else {
                return Some(mid as usize);
            }
        }
        Some(lo.clamp(0, frames.len() as i64 - 1) as usize)
    }

    fn face_in(fr: &FaceFrame, id: Option<i64>) -> Option<&FaceBox> {
        let id = id?;
        fr.faces.iter().find(|f| f.id == Some(id))
    }

    /// The face with `id` near `abs`, interpolated between its samples,
    /// within the shot and `max_gap` frames.
    fn face_for_id(&self, id: Option<i64>, abs: i64, max_gap: i64) -> Option<FaceBox> {
        id?;
        let frames = self.frames();
        if frames.is_empty() {
            return None;
        }
        let target = abs.max(0);
        let idx = self.frame_index(target)?;
        let shot = self.shot_at(target);
        let (start, end) = if shot == NO_SHOT {
            (i64::MIN, i64::MAX)
        } else {
            let s = self.span(shot);
            (s.start, s.end)
        };
        let max_gap = max_gap.max(1);
        let idx_frame = frames[idx].frame;
        let mut prev = None;
        let mut i = idx as i64;
        while i >= 0 {
            let fr = &frames[i as usize];
            if fr.frame >= end {
                i -= 1;
                continue;
            }
            if fr.frame < start {
                break;
            }
            if Self::face_in(fr, id).is_some() {
                prev = Some(i as usize);
                break;
            }
            if idx_frame - fr.frame > max_gap {
                break;
            }
            i -= 1;
        }
        let mut next = None;
        for (j, fr) in frames.iter().enumerate().skip(idx) {
            if fr.frame < start {
                continue;
            }
            if fr.frame >= end {
                break;
            }
            if Self::face_in(fr, id).is_some() {
                next = Some(j);
                break;
            }
            if fr.frame - idx_frame > max_gap {
                break;
            }
        }
        match (prev, next) {
            (None, None) => None,
            (Some(p), None) => Self::face_in(&frames[p], id).cloned(),
            (None, Some(n)) => Self::face_in(&frames[n], id).cloned(),
            (Some(p), Some(n)) => {
                let (a, b) = (&frames[p], &frames[n]);
                let fa = Self::face_in(a, id).unwrap();
                let fb = Self::face_in(b, id).unwrap();
                let span = (b.frame - a.frame).max(1) as f64;
                let t = ((target - a.frame) as f64 / span).clamp(0.0, 1.0);
                Some(Self::interpolate(fa, fb, t))
            }
        }
    }

    fn face_by_id_at(&self, abs: i64, id: Option<i64>) -> Option<FaceBox> {
        let id = id?;
        self.faces_at_raw(abs).iter().find(|f| f.id == Some(id)).cloned()
    }

    fn future_frame_for_id(&self, abs: i64, id: Option<i64>, max_ahead: i64) -> Option<i64> {
        id?;
        let frames = self.frames();
        if frames.is_empty() {
            return None;
        }
        let now = abs.max(0);
        let idx = self.frame_index(now)?;
        let shot = self.shot_at(now);
        let end = if shot == NO_SHOT { i64::MAX } else { self.span(shot).end };
        let limit = max_ahead.max(1);
        for fr in &frames[idx..] {
            if fr.frame < now {
                continue;
            }
            if fr.frame >= end || fr.frame - now > limit {
                break;
            }
            if Self::face_in(fr, id).is_some() {
                return Some(fr.frame);
            }
        }
        None
    }

    fn future_presence(&self, abs: i64, id: Option<i64>, max_ahead: i64) -> i64 {
        if id.is_none() {
            return 0;
        }
        let frames = self.frames();
        if frames.is_empty() {
            return 0;
        }
        let now = abs.max(0);
        let Some(idx) = self.frame_index(now) else {
            return 0;
        };
        let shot = self.shot_at(now);
        let end = if shot == NO_SHOT { i64::MAX } else { self.span(shot).end };
        let limit = max_ahead.max(1);
        let (mut first, mut last) = (None, None);
        for fr in &frames[idx..] {
            if fr.frame < now {
                continue;
            }
            if fr.frame >= end || fr.frame - now > limit {
                break;
            }
            if Self::face_in(fr, id).is_some() {
                if first.is_none() {
                    first = Some(fr.frame);
                }
                last = Some(fr.frame);
            } else if first.is_some() {
                break;
            }
        }
        match (first, last) {
            (Some(a), Some(b)) => b - a + 1,
            _ => 0,
        }
    }

    // ---- subject selection

    /// At a shot's start: the candidate that stays top-ranked longest over
    /// the next two seconds.
    fn future_stable(&mut self, abs: i64, candidates: &[FaceBox], shot: usize) -> Option<FaceBox> {
        if shot == NO_SHOT || candidates.is_empty() {
            return None;
        }
        let max_ahead = self.seconds_to_frames(2.0).max(1);
        let end = self.span(shot).end;
        // id -> (best score, count, longest run, best face); insertion order kept.
        let mut order: Vec<i64> = Vec::new();
        let mut info: HashMap<i64, (f64, i64, i64, FaceBox)> = HashMap::new();
        let mut current_best: Option<i64> = None;
        let mut streak = 0i64;
        for off in 0..=max_ahead {
            let frame = abs + off;
            if frame >= end {
                break;
            }
            let fc = self.candidates(frame);
            if fc.is_empty() {
                continue;
            }
            let ranking = match self.shot_at(frame) {
                NO_SHOT => shot,
                s => s,
            };
            let mut best: Option<FaceBox> = None;
            let mut best_score = f64::NEG_INFINITY;
            for c in &fc {
                let s = self.score(c, ranking);
                if s > best_score {
                    best_score = s;
                    best = Some(c.clone());
                }
            }
            match best.as_ref().and_then(|b| b.id) {
                Some(id) => {
                    if current_best == Some(id) {
                        streak += 1;
                    } else {
                        current_best = Some(id);
                        streak = 1;
                    }
                    let entry = info.entry(id).or_insert_with(|| {
                        order.push(id);
                        (best_score, 0, 0, best.clone().unwrap())
                    });
                    entry.1 += 1;
                    if streak > entry.2 {
                        entry.2 = streak;
                    }
                    if best_score > entry.0 {
                        entry.0 = best_score;
                        entry.3 = best.clone().unwrap();
                    }
                }
                None => {
                    current_best = None;
                    streak = 0;
                }
            }
        }
        if order.is_empty() {
            return None;
        }
        let (mut winner, mut best_run, mut best_count, mut best_score) = (None, -1i64, -1i64, f64::NEG_INFINITY);
        for id in &order {
            let (score, count, run, _) = &info[id];
            if *run > best_run
                || (*run == best_run && *count > best_count)
                || (*run == best_run && *count == best_count && *score > best_score)
            {
                best_run = *run;
                best_count = *count;
                best_score = *score;
                winner = Some(*id);
            }
        }
        let w = winner?;
        candidates
            .iter()
            .find(|c| c.id == Some(w))
            .cloned()
            .or_else(|| Some(info[&w].3.clone()))
    }

    /// The subject at `abs` before smoothing and hand-off guards.
    fn primary_base(&mut self, abs: i64) -> Option<FaceBox> {
        let candidates = self.candidates(abs);
        if candidates.is_empty() {
            return None;
        }
        let shot = self.shot_at(abs);
        let prev_shot = self.shot_at(abs - 1);
        let top = self
            .top_by_talk(&candidates, 1, shot)
            .into_iter()
            .next()
            .or_else(|| candidates.first().cloned());
        if shot != NO_SHOT && (prev_shot == NO_SHOT || prev_shot != shot) {
            if let Some(f) = self.future_stable(abs, &candidates, shot) {
                return Some(f);
            }
            return top;
        }
        if shot != NO_SHOT
            && let Some(top_face) = top.clone()
        {
            let s = self.span(shot);
            if s.end - s.start <= self.seconds_to_frames(4.0) {
                let anchor_frame = (s.start + 1).max(0);
                if let Some(anchor) = self.interpolated_primary(anchor_frame)
                    && anchor.id.is_some()
                    && let Some(ac) = candidates.iter().find(|c| c.id == anchor.id).cloned()
                {
                    let anchor_score = self.score(&ac, shot);
                    let top_score = self.score(&top_face, shot);
                    if top_score <= anchor_score {
                        return Some(ac);
                    }
                }
            }
        }
        let Some(prev) = self.interpolated_primary(abs - 1) else {
            return top;
        };
        let Some(top_face) = top else {
            return Some(prev);
        };
        if prev.id.is_some() && top_face.id == prev.id {
            return Some(top_face);
        }
        let prev_score = self.score(&prev, shot);
        let top_score = self.score(&top_face, shot);
        if top_score > prev_score {
            Some(top_face)
        } else {
            Some(prev)
        }
    }

    /// Whether to keep the current subject instead of switching to
    /// `candidate` (a hand-off needs a clear, lasting speaker advantage).
    fn hold_previous(&mut self, abs: i64, prev_id: Option<i64>, candidate_id: Option<i64>, window: i64) -> bool {
        let (Some(_), Some(_)) = (prev_id, candidate_id) else {
            return false;
        };
        if prev_id == candidate_id {
            return false;
        }
        let now = abs.max(0);
        if now
            >= self
                .media_start
                .max(self.media_end_exclusive - self.seconds_to_frames(0.65))
        {
            return true;
        }
        let prev_face = self.face_by_id_at(now, prev_id);
        let cand_face = self.face_by_id_at(now, candidate_id);
        let shot_now = self.shot_at(now);
        let at_start = shot_now != NO_SHOT && now == self.span(shot_now).start;
        if at_start && prev_face.is_none() {
            return false;
        }
        if let (Some(p), Some(c)) = (&prev_face, &cand_face) {
            let (pt, ct) = (p.talk0(), c.talk0());
            let (pf, cf) = (p.focus01(), c.focus01());
            let ratio = ct / pt.max(f64::EPSILON);
            let focus_ok = cf + 0.01 >= pf;
            let advantage = ct >= 0.05 && ct >= pt + 0.05 && ratio >= 2.5 && focus_ok;
            if !advantage {
                return true;
            }
        }
        if at_start {
            return false;
        }
        let min_hold = 30;
        let lookahead = window.max(self.seconds_to_frames(2.0));
        let max_ahead = lookahead.max(1);
        let future_prev = self.future_frame_for_id(now, prev_id, max_ahead);
        let gap = future_prev.map(|f| f - now);
        let prev_span = self.future_presence(now, prev_id, max_ahead);
        let cand_span = self.future_presence(now, candidate_id, window);
        let base_shot = self.shot_at(now);
        let (mut top_prev_run, mut prev_streak, mut cand_streak, mut top_cand_run) = (0, 0, 0, 0);
        for off in 1..=max_ahead {
            let f = now + off;
            let shot_f = self.shot_at(f);
            if base_shot != NO_SHOT && (shot_f == NO_SHOT || shot_f != base_shot) {
                break;
            }
            let cands = self.candidates(f);
            if cands.is_empty() {
                continue;
            }
            let ranking = if shot_f != NO_SHOT { shot_f } else { base_shot };
            let top = self.top_by_talk(&cands, 1, ranking).into_iter().next();
            let top_id = top.as_ref().and_then(|t| t.id);
            if top.is_some() && top_id == prev_id {
                prev_streak += 1;
                if prev_streak > top_prev_run {
                    top_prev_run = prev_streak;
                }
            } else {
                prev_streak = 0;
            }
            if top.is_some() && top_id == candidate_id {
                cand_streak += 1;
                if cand_streak > top_cand_run {
                    top_cand_run = cand_streak;
                }
            } else {
                cand_streak = 0;
            }
        }
        let cand_short = cand_span > 0 && cand_span < min_hold;
        let cand_weak = top_cand_run < min_hold;
        let prev_soon = future_prev.is_some() && gap.is_some_and(|g| g >= 0 && g <= max_ahead);
        if !prev_soon {
            return cand_short || cand_weak;
        }
        if prev_span >= min_hold && top_prev_run >= min_hold {
            return true;
        }
        cand_short || cand_weak
    }

    /// The smoothed subject at `abs` (stateful from frame 0).
    fn stable_primary(&mut self, abs: i64) -> Option<FaceBox> {
        let key = abs.max(0);
        if !self.cfg.enable_primary_stabilizer {
            return self.primary_base(key);
        }
        while (self.stable.len() as i64) <= key {
            let f = self.stable.len() as i64;
            let v = self.step_primary(f);
            self.stable.push(v);
        }
        self.stable[key as usize].clone()
    }

    fn step_primary(&mut self, frame: i64) -> Option<FaceBox> {
        const STATIC_THRESHOLD: f64 = 0.0025;
        const MOTION_DISTANCE: f64 = 0.085;
        const BASE_BLEND: f64 = 0.35;
        const MAX_BLEND: f64 = 0.92;
        const ID_REUSE_DISTANCE: f64 = 0.022;
        const HARD_SWITCH_DISTANCE: f64 = 0.12;
        const SAME_ID_SNAP_DISTANCE: f64 = 0.1;
        const EDGE_JUMP_DISTANCE: f64 = 0.07;
        const EDGE_BAND: f64 = 0.18;
        let switch_frames = self.seconds_to_frames(0.25).max(3);
        let hold_window = self.seconds_to_frames(1.0).max(3);

        let prev_smoothed = self.stab.current.clone();
        let prev_active = self.stab.active_id.or(prev_smoothed.as_ref().and_then(|f| f.id));
        let base = self.primary_base(frame);
        let mut candidate = base.clone();
        let fallback_prev = if prev_active.is_some() {
            self.face_for_id(prev_active, frame, hold_window)
        } else {
            None
        };
        let shot_this = self.shot_at(frame);
        let shot_prev = self.shot_at(frame - 1);
        let same_shot =
            shot_this != NO_SHOT && shot_prev != NO_SHOT && self.shot_key(shot_this) == self.shot_key(shot_prev);

        if candidate.is_none() && fallback_prev.is_some() && same_shot {
            candidate = fallback_prev.clone();
        }
        if prev_active.is_some() {
            let cid = candidate.as_ref().and_then(|c| c.id);
            if cid.is_some()
                && cid != prev_active
                && self.hold_previous(frame, prev_active, cid, hold_window)
                && same_shot
            {
                candidate = fallback_prev.clone().or_else(|| prev_smoothed.clone()).or(candidate);
                if let Some(c) = candidate.as_mut()
                    && c.id.is_none()
                {
                    c.id = prev_active;
                }
            }
        }
        if candidate.is_none() && prev_smoothed.is_some() && same_shot {
            candidate = prev_smoothed.clone();
        }
        let Some(mut candidate) = candidate else {
            self.stab.current = None;
            self.stab.active_id = None;
            self.stab.switch_origin = None;
            self.stab.switch_target = None;
            self.stab.last_switch_frame = -1;
            self.stab.last_frame = frame;
            return None;
        };

        let mut candidate_id = candidate.id;
        let mut same_id = candidate_id.is_some() && candidate_id == prev_active;
        if !same_id
            && prev_active.is_some()
            && let Some(ps) = &prev_smoothed
            && same_shot
            && Self::center_distance(ps, &candidate) < ID_REUSE_DISTANCE
        {
            same_id = true;
            candidate_id = prev_active;
            candidate.id = prev_active;
        }
        if same_id && prev_active.is_some() && candidate.id.is_none() {
            candidate.id = prev_active;
            candidate_id = prev_active;
        }

        let mut in_switch = false;
        let boundary_cut = self.at_shot_start(frame);
        let dist = prev_smoothed.as_ref().map(|p| Self::center_distance(p, &candidate));
        let cx = candidate.cx();
        let near_edge = cx < EDGE_BAND || cx > 1.0 - EDGE_BAND;
        let far_jump = !same_id && dist.is_some_and(|d| d > HARD_SWITCH_DISTANCE);
        let large_move = same_id && dist.is_some_and(|d| d > SAME_ID_SNAP_DISTANCE);
        let edge_snap = same_id && near_edge && dist.is_some_and(|d| d > EDGE_JUMP_DISTANCE);
        let hard_cut = boundary_cut || far_jump || large_move || edge_snap;

        let st = &mut self.stab;
        if !same_id {
            st.switch_origin = Some(st.current.clone().unwrap_or_else(|| candidate.clone()));
            st.switch_target = Some(candidate.clone());
            st.last_switch_frame = frame;
            st.active_id = candidate_id;
            in_switch = !hard_cut;
        } else if st.switch_origin.is_some()
            && st.switch_target.is_some()
            && st.last_switch_frame >= 0
            && frame - st.last_switch_frame < switch_frames
        {
            st.switch_target = Some(candidate.clone());
            in_switch = !hard_cut;
        }

        if hard_cut {
            st.current = Some(candidate.clone());
            st.switch_origin = None;
            st.switch_target = None;
            st.last_switch_frame = -1;
            st.active_id = candidate_id;
            st.last_frame = frame;
            return st.current.clone();
        }

        if in_switch && st.switch_origin.is_some() && st.switch_target.is_some() && st.last_switch_frame >= 0 {
            let since = (frame - st.last_switch_frame) as f64;
            let progress = (since / switch_frames as f64).clamp(0.0, 1.0);
            let eased = ease_in_out_cubic(progress);
            st.current = Some(blend(
                st.switch_origin.as_ref(),
                st.switch_target.as_ref().unwrap(),
                eased,
            ));
            if progress >= 1.0 {
                st.switch_origin = None;
                st.switch_target = None;
                st.last_switch_frame = -1;
                st.current = Some(candidate.clone());
            }
        } else {
            match st.current.take() {
                None => st.current = Some(candidate.clone()),
                Some(cur) => {
                    let d = Self::center_distance(&cur, &candidate);
                    st.current = Some(if d < STATIC_THRESHOLD {
                        FaceBox {
                            talk: candidate.talk,
                            focus: candidate.focus,
                            ..cur
                        }
                    } else {
                        let n = (d / MOTION_DISTANCE).min(1.0);
                        let b = BASE_BLEND + (MAX_BLEND - BASE_BLEND) * ease_in_out_cubic(n);
                        blend(Some(&cur), &candidate, b)
                    });
                }
            }
            st.switch_origin = None;
            st.switch_target = None;
            st.last_switch_frame = -1;
        }

        match st.current.as_mut() {
            Some(cur) => {
                cur.talk = candidate.talk;
                cur.focus = candidate.focus;
                if candidate_id.is_some() {
                    cur.id = candidate_id;
                    st.active_id = candidate_id;
                } else if cur.id.is_some() {
                    st.active_id = cur.id;
                }
            }
            None => st.active_id = None,
        }
        st.last_frame = frame;
        st.current.clone()
    }

    /// The first face at or after `abs` within its shot.
    fn forward_primary(&mut self, abs: i64) -> Option<FaceBox> {
        let key = abs.max(0);
        if let Some(v) = self.forward.get(&key) {
            return v.as_ref().map(|f| f.face.clone());
        }
        let frames = self.frames();
        if frames.is_empty() {
            self.forward.insert(key, None);
            return None;
        }
        let shot = self.shot_at(key);
        let search_end = if shot == NO_SHOT {
            frames.last().map(|f| f.frame).unwrap_or(key) + 1
        } else {
            self.span(shot).end
        };
        let Some(idx) = self.frame_index(key) else {
            self.forward.insert(key, None);
            return None;
        };
        let mut found = None;
        for fr in &frames[idx..] {
            if fr.frame < key {
                continue;
            }
            if fr.frame >= search_end {
                break;
            }
            if fr.faces.is_empty() {
                continue;
            }
            if let Some(p) = self.top_by_talk(&fr.faces, 1, shot).into_iter().next() {
                found = Some(Forward { face: p });
                break;
            }
        }
        self.forward.insert(key, found.clone());
        found.map(|f| f.face)
    }

    fn shot_faces(&mut self, shot: usize) -> bool {
        let key = self.shot_key(shot);
        if let Some(v) = self.shot_has_faces.get(&key) {
            return *v;
        }
        let mut has = false;
        for fr in self.frames() {
            if fr.frame < key.0 {
                continue;
            }
            if fr.frame >= key.1 {
                break;
            }
            if !fr.faces.is_empty() {
                has = true;
                break;
            }
        }
        self.shot_has_faces.insert(key, has);
        has
    }

    // ---- offsets (composition px; the source is scaled to fill the height)

    fn translate_x(&self, face: &FaceBox, zoom: f64, allow_overflow: bool) -> f64 {
        let scale = (self.comp_h * zoom) / self.src_h;
        let scaled_w = self.src_w * scale;
        let viewport = self.comp_w;
        let mut offset = viewport / 2.0 - face.cx() * scaled_w;
        if !allow_overflow {
            let min = viewport - scaled_w;
            if offset < min {
                offset = min;
            }
            if offset > 0.0 {
                offset = 0.0;
            }
        }
        offset
    }

    fn centered(&self, zoom: f64) -> f64 {
        let src_h = if self.src_h == 0.0 { 1.0 } else { self.src_h };
        let src_w = if self.src_w == 0.0 { 1.0 } else { self.src_w };
        let scale = (self.comp_h / src_h) * zoom.max(1.0);
        if !scale.is_finite() || scale <= 0.0 {
            return 0.0;
        }
        let w = src_w * scale;
        if !w.is_finite() {
            return 0.0;
        }
        js_round((self.comp_w - w) / 2.0)
    }

    fn raw_offset(&mut self, abs: i64) -> Option<f64> {
        let now = abs.max(0);
        let allow = !self.cfg.enable_primary_stabilizer;
        let shot = self.shot_at(now);
        let zoom = self.shot_zoom(shot);
        if let Some(f) = self.stable_primary(now) {
            return Some(self.translate_x(&f, zoom, allow));
        }
        if let Some(f) = self.forward_primary(now) {
            return Some(self.translate_x(&f, zoom, allow));
        }
        if shot != NO_SHOT {
            if !self.shot_faces(shot) {
                return Some(self.centered(zoom));
            }
        } else if !self.has_any_faces {
            return Some(self.centered(zoom));
        }
        None
    }

    fn averaged_raw(&mut self, abs: i64, window_sec: f64) -> Option<f64> {
        let now = abs.max(0);
        let samples = self.seconds_to_frames(window_sec).min(30);
        let shot = self.shot_at(now);
        let shot_start = if shot == NO_SHOT { 0 } else { self.span(shot).start };
        let now_face = self.stable_primary(now);
        let (mut sum, mut count) = (0.0, 0);
        for i in 0..samples {
            let f = now - i;
            if f < 0 || f < shot_start {
                break;
            }
            let then = self.stable_primary(f);
            if let (Some(n), Some(t)) = (&now_face, &then)
                && n.id.is_some()
                && t.id.is_some()
                && t.id != n.id
                && Self::center_distance(n, t) >= self.cfg.id_switch_cut_dist
            {
                break;
            }
            if let Some(r) = self.raw_offset(f) {
                sum += r;
                count += 1;
            }
        }
        (count > 0).then(|| sum / count as f64)
    }

    fn movement_profile(&mut self, abs: i64) -> Profile {
        let now = abs.max(0);
        let prev = (now - 1).max(0);
        let raw_now = self.raw_offset(now);
        let raw_prev = self.raw_offset(prev);
        let face_now = self.stable_primary(now);
        let face_prev = self.stable_primary(prev);
        let same_id = matches!((&face_now, &face_prev), (Some(a), Some(b)) if a.id.is_some() && a.id == b.id);
        let mut p = Profile {
            clamp_px: self.cfg.clamp_base_px,
            deadzone_px: self.cfg.deadzone_base_min,
            disable_deadzone: false,
            skip_cache_clamp: false,
            raw_now,
            raw_prev,
        };
        if let (Some(rn), Some(rp)) = (raw_now, raw_prev) {
            let d = (rn - rp).abs();
            let shift = match (&face_now, &face_prev) {
                (Some(a), Some(b)) => js_hypot(a.cx() - b.cx(), (a.cy() - b.cy()) * 0.7),
                _ => 0.0,
            };
            if !same_id {
                p.clamp_px = d.max(60.0);
                p.deadzone_px = 8.0;
                p.disable_deadzone = true;
                p.skip_cache_clamp = true;
            } else {
                if d >= 80.0 || shift > 0.18 {
                    p.clamp_px = (d * 0.9).max(36.0);
                    p.deadzone_px = 24f64.min(self.cfg.deadzone_base_min * 0.5);
                    p.disable_deadzone = true;
                } else if d >= 36.0 || shift > 0.08 {
                    p.clamp_px = 24f64.max(self.cfg.clamp_base_px);
                    p.deadzone_px = self.cfg.deadzone_base_min.min(32.0);
                } else if d <= 12.0 && shift < 0.03 {
                    p.clamp_px = self.cfg.clamp_base_px.min(8.0);
                    p.deadzone_px = self.cfg.deadzone_base_min;
                }
                p.clamp_px = self.cfg.max_translate_delta_px.min(p.clamp_px);
            }
        }
        p
    }

    /// The subject-framing offset before the camera's own smoothing.
    fn single_offset(&mut self, abs: i64) -> Option<f64> {
        let now = abs.max(0);
        let raw_now = self.raw_offset(now)?;
        if !self.cfg.enable_primary_stabilizer {
            return Some(js_round(raw_now));
        }
        if !self.cfg.enable_translate_smoothing {
            return Some(raw_now);
        }
        let profile = self.movement_profile(now);
        let shot = self.shot_at(now);
        if self.at_shot_start(now) {
            return Some(js_round(raw_now));
        }
        let start_frame = if shot == NO_SHOT {
            None
        } else {
            Some(self.span(shot).start)
        };
        let in_start_window = shot != NO_SHOT && now <= start_frame.unwrap_or(now);
        if in_start_window {
            let prev_frame = start_frame.unwrap_or(0).max(now - 1);
            if prev_frame < now
                && let Some(prev_raw) = self.raw_offset(prev_frame)
            {
                let limit = profile.clamp_px.max(self.cfg.max_translate_delta_px);
                return Some(js_round(clamp_delta(raw_now, Some(prev_raw), limit)));
            }
            return Some(js_round(raw_now));
        }
        let prev_face = self.stable_primary(now - 1);
        let curr_face = self.stable_primary(now);
        if let (Some(p), Some(c)) = (&prev_face, &curr_face)
            && p.id.is_some()
            && c.id.is_some()
            && p.id != c.id
            && Self::center_distance(p, c) >= 0.3
        {
            return Some(js_round(raw_now));
        }
        let avg_now = self.averaged_raw(now, self.cfg.avg_window_now_sec).unwrap_or(raw_now);
        let avg_prev = self
            .averaged_raw(now - 1, self.cfg.avg_window_prev_sec)
            .unwrap_or(avg_now);
        let raw_prev_i = profile.raw_prev;
        let raw_now_i = profile.raw_now.unwrap_or(raw_now);
        let jump = match (&prev_face, &curr_face) {
            (Some(p), Some(c)) => Self::center_distance(p, c),
            _ => 0.0,
        };
        let near_edge = curr_face.as_ref().is_some_and(|c| c.cx() < 0.2 || c.cx() > 0.8);
        let raw_delta = raw_prev_i.map(|p| (raw_now_i - p).abs());
        let delta_avg = (raw_now_i - avg_prev).abs();
        let eff = raw_delta.unwrap_or(delta_avg);
        const FAST_FACE: f64 = 0.075;
        const EDGE_FACE: f64 = 0.045;
        const FAST_RAW: f64 = 32.0;
        const EXTREME_RAW: f64 = 56.0;
        let snap = eff >= EXTREME_RAW
            || (jump > FAST_FACE && eff >= FAST_RAW)
            || (near_edge && eff >= FAST_RAW)
            || jump > FAST_FACE * 1.3
            || (near_edge && jump > EDGE_FACE);
        if snap {
            self.skip_smoothing.insert(now, true);
            self.skip_smoothing.insert(now + 1, true);
            if self.cfg.enable_ema_single {
                self.ema = (None, Some(self.ema_key(shot)));
            }
            return Some(js_round(raw_now_i));
        }
        let mut candidate = avg_now;
        if let Some(rp) = raw_prev_i {
            let d = (raw_now_i - rp).abs();
            if d >= 80.0 {
                candidate = raw_now_i;
            } else if d >= 36.0 {
                candidate = lerp(raw_now_i, candidate, 0.3);
            } else if d <= 10.0 {
                candidate = lerp(candidate, raw_now_i, 0.2);
            }
        }
        let dz = if profile.disable_deadzone {
            0.0
        } else {
            profile.deadzone_px
        };
        let mut dz_candidate = if profile.disable_deadzone {
            candidate
        } else if (candidate - avg_prev).abs() < dz {
            avg_prev
        } else {
            candidate
        };
        if let Some(prev_off) = self.averaged_raw(now - 2, self.cfg.avg_window_prev_sec) {
            let dir_prev = sign(avg_prev - prev_off);
            let dir_now = sign(dz_candidate - avg_prev);
            if dir_prev != 0.0 && dir_now != 0.0 && dir_prev != dir_now {
                dz_candidate = lerp(dz_candidate, avg_prev, self.cfg.anti_flip_lerp);
            }
        }
        let jump_mag = (raw_now - avg_prev).abs();
        if jump_mag >= self.cfg.max_translate_delta_px {
            return Some(js_round(raw_now));
        }
        let out = clamp_delta(dz_candidate, Some(avg_prev), profile.clamp_px.max(4.0));
        let mut smoothed = out;
        if self.cfg.enable_ema_single {
            let key = self.ema_key(shot);
            if self.at_shot_start(now) || self.ema.1.as_deref() != Some(key.as_str()) {
                self.ema.0 = None;
            }
            let prev = self.ema.0.unwrap_or(out);
            let e = prev + self.cfg.ema_alpha_single * (out - prev);
            self.ema = (Some(e), Some(key));
            smoothed = e;
        }
        Some(js_round(smoothed))
    }

    fn ema_key(&self, shot: usize) -> String {
        if shot == NO_SHOT {
            "none".into()
        } else {
            let (a, b) = self.shot_key(shot);
            format!("{a}:{b}")
        }
    }

    /// The offset, held from earlier in the shot while faces are missing.
    fn held_offset(&mut self, abs: i64) -> Option<f64> {
        let now = abs.max(0);
        if let Some(v) = self.single_offset(now) {
            return Some(v);
        }
        let hold = self.seconds_to_frames(10.0).min(300);
        let shot = self.shot_at(now);
        let start = if shot == NO_SHOT { 0 } else { self.span(shot).start };
        for i in 1..=hold {
            let f = now - i;
            if f < 0 || f < start {
                break;
            }
            if let Some(p) = self.single_offset(f) {
                return Some(if self.cfg.enable_translate_smoothing {
                    js_round(p)
                } else {
                    p
                });
            }
        }
        None
    }

    fn skip_translate_smoothing(&mut self, abs: i64) -> bool {
        let key = abs.max(0);
        if let Some(v) = self.skip_smoothing.get(&key) {
            return *v;
        }
        let skip = if self.is_protect(abs) || self.at_shot_start(abs) {
            true
        } else {
            let prev = self.stable_primary((key - 1).max(0));
            let curr = self.stable_primary(abs);
            matches!((&prev, &curr), (Some(p), Some(c))
                if p.id.is_some() && c.id.is_some() && p.id != c.id
                    && Self::center_distance(p, c) >= self.cfg.id_switch_cut_dist)
        };
        self.skip_smoothing.insert(key, skip);
        skip
    }

    /// The camera offset at `abs`, filled forward from frame 0.
    fn smoothed(&mut self, abs: i64) -> Option<f64> {
        let target = abs.max(0);
        if self.smooth.is_empty() {
            let seed = self.held_offset(0);
            self.smooth.push(seed);
        }
        while (self.smooth.len() as i64) <= target {
            let k = self.smooth.len() as i64;
            let v = self.smooth_step(k);
            self.smooth.push(v);
        }
        self.smooth[target as usize]
    }

    fn smooth_step(&mut self, k: i64) -> Option<f64> {
        let desired = self.held_offset(k)?;
        if !self.cfg.enable_primary_stabilizer {
            return Some(desired);
        }
        if self.cfg.enable_shot_anchor_camera {
            let shot = self.shot_at(k);
            let face = self.stable_primary(k);
            let at_start = self.at_shot_start(k);
            let boundary = if at_start {
                self.shot(shot).and_then(|s| s.start_boundary_kind.clone())
            } else {
                None
            };
            let incoming = self.faces_at_raw(k);
            let visible = self.eligible(incoming).len();
            let mut previous = self.anchor_states.get(&(k - 1)).cloned();
            // Frame zero is only seeded in the offset cache; a join on frame
            // one still needs that exact preceding camera.
            if previous.is_none() && k == 1 && boundary.as_deref() == Some("stitch_join") {
                let prev_shot = self.shot_at(0);
                if let Some(offset) = self.smooth[0] {
                    previous = Some(AnchorState {
                        face_id: self.stable_primary(0).and_then(|f| f.id),
                        mode: 0,
                        offset_px: offset,
                        velocity_px: 0.0,
                        shot_key: (prev_shot != NO_SHOT).then(|| self.shot_key(prev_shot)),
                    });
                }
            }
            let state = advance_anchor(
                desired,
                face.as_ref().map(|f| f.h),
                face.as_ref().and_then(|f| f.id),
                at_start,
                previous.as_ref(),
                (shot != NO_SHOT).then(|| self.shot_key(shot)),
                visible,
                boundary.as_deref(),
            );
            let out = state.offset_px;
            self.anchor_states.insert(k, state);
            return Some(out);
        }
        let profile = self.movement_profile(k);
        if profile.skip_cache_clamp || self.skip_translate_smoothing(k) {
            return Some(desired);
        }
        let Some(prev) = self.smooth[(k - 1) as usize] else {
            return Some(desired);
        };
        let limit = if profile.clamp_px >= self.cfg.max_translate_delta_px {
            profile.clamp_px.max(self.cfg.max_translate_delta_px)
        } else {
            6f64.max(self.cfg.max_translate_delta_px.min(profile.clamp_px))
        };
        let d = (desired - prev).clamp(-limit, limit);
        Some(prev + d)
    }

    /// The camera for output frame `f`.
    fn frame(&mut self, f: i64) -> CameraFrame {
        let abs = self.media_start + f;
        let protect = self.is_protect(abs);
        let shot = self.shot_at(abs);
        let zoom = self.shot_zoom(shot);
        let (cw, ch) = (self.comp_w, self.comp_h);
        let aspect = if self.src_w > 0.0 && self.src_h > 0.0 {
            self.src_w / self.src_h
        } else {
            cw / ch
        };
        let full = Rect {
            x: 0.0,
            y: 0.0,
            width: cw,
            height: ch,
        };
        if protect {
            // Blurred cover behind the whole frame, the source contained on top.
            let cover = ch.max(cw / aspect) * 1.1;
            let contain = ch.min(cw / aspect);
            return CameraFrame {
                protect,
                zoom,
                layers: vec![
                    CameraLayer {
                        viewport: full,
                        source: Rect {
                            x: (cw - cover * aspect) / 2.0,
                            y: (ch - cover) / 2.0,
                            width: cover * aspect,
                            height: cover,
                        },
                        blur_px: 70.0,
                        brightness: 1.05,
                        opacity: 0.72,
                    },
                    CameraLayer {
                        viewport: full,
                        source: Rect {
                            x: (cw - contain * aspect) / 2.0,
                            y: (ch - contain) / 2.0,
                            width: contain * aspect,
                            height: contain,
                        },
                        blur_px: 0.0,
                        brightness: 1.0,
                        opacity: 1.0,
                    },
                ],
            };
        }
        let smoothed = if self.cfg.enable_translate_smoothing {
            self.smoothed(abs)
        } else {
            self.single_offset(abs)
        };
        let x = smoothed.unwrap_or_else(|| self.centered(zoom));
        let x = if x.is_finite() { x } else { 0.0 };
        let y = if zoom > 1.0 { -(ch * (zoom - 1.0)) / 2.0 } else { 0.0 };
        CameraFrame {
            protect,
            zoom,
            layers: vec![CameraLayer {
                viewport: full,
                source: Rect {
                    x,
                    y,
                    width: ch * aspect * zoom,
                    height: ch * zoom,
                },
                blur_px: 0.0,
                brightness: 1.0,
                opacity: 1.0,
            }],
        }
    }
}

struct Profile {
    clamp_px: f64,
    deadzone_px: f64,
    disable_deadzone: bool,
    skip_cache_clamp: bool,
    raw_now: Option<f64>,
    raw_prev: Option<f64>,
}

fn clamp_delta(curr: f64, prev: Option<f64>, max_delta: f64) -> f64 {
    match prev {
        None => curr,
        Some(p) if curr > p + max_delta => p + max_delta,
        Some(p) if curr < p - max_delta => p - max_delta,
        Some(_) => curr,
    }
}

fn sign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn ease_in_out_cubic(t: f64) -> f64 {
    let c = t.clamp(0.0, 1.0);
    if c < 0.5 {
        4.0 * c * c * c
    } else {
        let inv = -2.0 * c + 2.0;
        1.0 - (inv * inv * inv) / 2.0
    }
}

fn blend(origin: Option<&FaceBox>, target: &FaceBox, t: f64) -> FaceBox {
    let f = t.clamp(0.0, 1.0);
    match origin {
        None => target.clone(),
        Some(o) => FaceBox {
            x: lerp(o.x, target.x, f),
            y: lerp(o.y, target.y, f),
            w: lerp(o.w, target.w, f),
            h: lerp(o.h, target.h, f),
            ..target.clone()
        },
    }
}
