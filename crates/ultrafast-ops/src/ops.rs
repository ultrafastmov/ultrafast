//! Op registry: how each document op maps onto the Slang op library, which
//! image ports it has, and how its params pack into the fixed-size float
//! block the shader reads.
//!
//! Packing runs every frame for animated groups, so it is plain arithmetic
//! into a reused buffer.

use crate::OpsError;
use ultrafast_graph::{Format, Node, Param};

pub use ultrafast_geom::{Affine, parse_hex, srgb_to_linear, text};

/// How an op participates in code generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `g_<fn>(ctx, o)`: produces a color from a position.
    Gen,
    /// `g_<fn>(ctx, o, T)`: like `Gen`, sampling an external resource texture.
    GenResource,
    /// `f_<fn>(ctx, inputs..., o)`: combines inputs evaluated at the same ctx.
    Map,
    /// `w_<fn>(ctx, o)`: remaps the context its single input is evaluated in.
    Warp,
    /// Evaluates `map` at ctx, then `source` at a displaced ctx.
    Displace,
    /// Multi-tap op that must read materialized textures (blur).
    Blur,
    /// Agent-written Slang. Inputs are materialized and sampled freely.
    Custom,
    /// Vector geometry rasterized by the GPU coverage rasterizer (paths,
    /// text). Absorbs downstream transforms so geometry stays sharp.
    Raster,
    /// Scene data (meshes, lights, cameras): not an image. Consumed by
    /// `render3d`.
    Object,
    /// Renders connected 3D objects into an image.
    Render3D,
    /// Deterministic particle system drawn as sprites.
    Particles,
    /// Stateful simulation stepped once per frame (fluid, feedback):
    /// materialized, with its own persistent state textures.
    Sim,
}

#[derive(Debug)]
pub struct OpDef {
    /// Document op name.
    pub name: &'static str,
    /// Slang function suffix (`g_`/`f_`/`w_` + this).
    pub func: &'static str,
    pub form: Form,
    /// Image input ports, in evaluation order.
    pub ports: &'static [&'static str],
    /// Packed param floats.
    pub params: usize,
}

macro_rules! op {
    ($name:literal, $func:literal, $form:ident, [$($p:literal),*], $n:expr) => {
        OpDef { name: $name, func: $func, form: Form::$form, ports: &[$($p),*], params: $n }
    };
}

pub static OPS: &[OpDef] = &[
    op!("solid", "solid", Gen, [], 4),
    op!("gradient", "gradient_linear", Gen, [], GRADIENT_PARAMS),
    op!("gradient", "gradient_radial", Gen, [], GRADIENT_PARAMS),
    op!("gradient", "gradient_conic", Gen, [], GRADIENT_PARAMS),
    op!("shape", "shape", Gen, [], 13),
    op!("checker", "checker", Gen, [], 9),
    op!("noise", "noise", Gen, [], 14),
    op!("image", "image", GenResource, [], 10),
    op!("video", "image", GenResource, [], 10),
    op!("opacity", "opacity", Map, ["source"], 1),
    op!("over", "over", Map, ["fg", "bg"], 2),
    op!("blend", "blend", Map, ["fg", "bg"], 3),
    op!("composite", "composite", Map, ["fg", "bg"], 1),
    op!("mask", "mask", Map, ["source", "matte"], 1),
    op!("mix", "mix", Map, ["a", "b"], 1),
    op!("adjust", "adjust", Map, ["source"], 6),
    op!("levels", "levels", Map, ["source"], 5),
    op!("color_matrix", "color_matrix", Map, ["source"], 20),
    op!("lut", "lut", Map, ["source"], 3),
    op!("invert", "invert", Map, ["source"], 1),
    op!("tint", "tint", Map, ["source"], 8),
    op!("threshold", "threshold", Map, ["source"], 2),
    op!("crop", "crop", Map, ["source"], 5),
    op!("transform", "transform", Warp, ["source"], 7),
    op!("corner_pin", "corner_pin", Warp, ["source"], 10),
    op!("wave", "wave", Warp, ["source"], 4),
    op!("twirl", "twirl", Warp, ["source"], 4),
    op!("tile", "tile", Warp, ["source"], 4),
    op!("displace", "displace", Displace, ["source", "map"], 2),
    op!("blur", "blur", Blur, ["source"], 4),
    op!("shader", "shader", Custom, ["in0", "in1", "in2", "in3"], 17),
    op!("path", "path", Raster, [], 5),
    op!("text", "text", Raster, [], 5),
    op!("mesh", "mesh", Object, ["texture"], 0),
    op!("light", "light", Object, [], 0),
    op!("camera", "camera", Object, [], 0),
    // Sound: mixed into exports, not part of the image graph.
    op!("audio", "audio", Object, [], 0),
    op!("render3d", "render3d", Render3D, ["*"], 0),
    op!("particles", "particles", Particles, ["sprite"], 0),
    op!("swarm", "particles", Particles, [], 0),
    op!("fluid", "fluid", Sim, ["dye", "stir"], 0),
    op!("feedback", "feedback", Sim, ["in0", "in1", "in2", "in3"], 0),
];

/// Ops whose kernel variant depends on a param.
pub fn resolve(id: &str, node: &Node) -> Result<&'static OpDef, OpsError> {
    // Only gradients pick among several kernels by param; every other op
    // name maps to exactly one definition.
    let func = match node.op.as_str() {
        "gradient" => Some(match text(node, "kind").unwrap_or("linear") {
            "linear" => "gradient_linear",
            "radial" => "gradient_radial",
            "conic" => "gradient_conic",
            other => {
                return Err(bad(id, "kind", format!("unknown gradient kind `{other}`")));
            }
        }),
        _ => None,
    };
    OPS.iter()
        .find(|d| d.name == node.op && func.is_none_or(|f| d.func == f))
        .ok_or_else(|| OpsError::UnknownOp {
            node: id.to_string(),
            op: node.op.clone(),
        })
}

/// Ports actually connected for this node. Custom shaders take any subset
/// of their ports; every other op requires all of them.
pub fn connected_ports(def: &OpDef, id: &str, node: &Node) -> Result<Vec<String>, OpsError> {
    // `*`: any input names (render3d takes any number of objects).
    if def.ports == ["*"] {
        return Ok(node.inputs.keys().cloned().collect());
    }
    let mut out = Vec::new();
    for p in def.ports {
        if node.inputs.contains_key(*p) {
            out.push(p.to_string());
        } else if !matches!(def.form, Form::Custom | Form::Object | Form::Particles | Form::Sim) {
            return Err(OpsError::MissingPort {
                node: id.to_string(),
                port: p.to_string(),
            });
        }
    }
    Ok(out)
}

/// Ops that sample an external resource texture (bound after their inputs).
pub fn uses_resource(def: &OpDef) -> bool {
    def.form == Form::GenResource || def.name == "lut"
}

/// Extra identity that changes the generated code (beyond op and wiring).
pub fn variant_key(def: &OpDef, node: &Node) -> String {
    match def.form {
        Form::Custom | Form::Sim => text(node, "code").unwrap_or("").to_string(),
        // Resource identity: kind and source. Video decoders are per node
        // (each node has its own playhead).
        Form::GenResource => format!("{}:{}", node.op, text(node, "src").unwrap_or("")),
        _ if uses_resource(def) => format!("{}:{}", node.op, text(node, "src").unwrap_or("")),
        _ => String::new(),
    }
}

/// The input an inactive node (outside its timing window) passes through:
/// the destination of a composite, the source of an effect. Generators have
/// none and go transparent. This gives layer semantics: a layer outside its
/// window leaves what is under it untouched. `connected` is the number of
/// connected inputs (custom shaders may leave ports open).
pub fn passthrough(def: &OpDef, connected: usize) -> Option<usize> {
    // Simulations show nothing outside their window (their state holds);
    // particles neither (their input is a sprite, not a layer to show).
    if matches!(def.form, Form::Sim | Form::Particles) {
        return None;
    }
    let i = match def.name {
        "over" | "blend" | "composite" => 1,
        _ => 0,
    };
    (i < connected).then_some(i)
}

/// A shader node's `resolution` below 1: it renders at that fraction of
/// the frame size and is upscaled (for soft, expensive procedural looks).
pub fn render_scale(node: &Node) -> Option<f64> {
    if node.op != "shader" {
        return None;
    }
    let r = node.params.get("resolution")?.eval(0.0)?.first().copied()?;
    (r < 0.999).then_some(r.clamp(0.1, 1.0))
}

/// A shader that asks to be sampled under motion blur (`motion_blur:
/// true`: it moves things), instead of holding at the frame's time.
pub fn samples_motion(node: &Node) -> bool {
    matches!(node.params.get("motion_blur"), Some(Param::Bool(true)))
}

/// Ops whose output changes with time even when no param is animated.
pub fn time_varying(def: &OpDef, node: &Node) -> bool {
    def.name == "video"
        || def.name == "particles"
        || def.name == "swarm"
        || def.form == Form::Sim
        // A shader that reads the clock animates by itself.
        || (def.name == "shader" && text(node, "code").is_some_and(|c| c.contains("time(")))
        || (def.name == "image" && text(node, "src").is_some_and(|s| s.contains("{frame")))
}

/// Resolves an image-sequence path: `{frame}` or `{frame:05}` becomes the
/// frame number at local time `t` (`fps` param, default the document's).
pub fn sequence_path(src: &str, node: &Node, t: f64, doc_fps: f64) -> String {
    let Some(start) = src.find("{frame") else {
        return src.to_string();
    };
    let Some(end) = src[start..].find('}').map(|e| start + e) else {
        return src.to_string();
    };
    let fps = Packer { id: "", node, t }.num("fps", doc_fps);
    let first = Packer { id: "", node, t }.num("first_frame", 0.0);
    // Local times land a hair under a frame boundary after offsets
    // ((t - offset) * fps = 9.999999...): count those as the boundary.
    let frame = ((t * fps + 1e-6).floor() + first).max(0.0) as u64;
    let spec = &src[start + 6..end];
    let width: usize = spec
        .strip_prefix(':')
        .and_then(|w| w.trim_start_matches('0').parse().ok())
        .unwrap_or(0);
    format!("{}{:0width$}{}", &src[..start], frame, &src[end + 1..], width = width)
}

/// Source time of a `video` node at composition time `t`: the `time` param
/// if set (animate it for time remapping), else `start + t * speed`.
/// `loop` wraps at the source duration; otherwise the last frame holds.
pub fn video_time(node: &Node, t: f64, duration: f64) -> f64 {
    let p = Packer { id: "", node, t };
    let raw = if node.params.contains_key("time") {
        p.num("time", 0.0)
    } else {
        p.num("start", 0.0) + t * p.num("speed", 1.0)
    };
    let looping = matches!(node.params.get("loop"), Some(Param::Bool(true)));
    if looping && duration > 0.0 {
        raw.rem_euclid(duration)
    } else {
        raw.clamp(0.0, duration.max(0.0))
    }
}

/// Size of a resource the op samples, needed to pack placement params.
#[derive(Debug, Clone, Copy)]
pub struct ResourceInfo {
    pub width: u32,
    pub height: u32,
}

pub fn pack(
    def: &OpDef,
    id: &str,
    node: &Node,
    fmt: &Format,
    t: f64,
    res: Option<ResourceInfo>,
    out: &mut Vec<f32>,
) -> Result<(), OpsError> {
    let start = out.len();
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    let center = [w / 2.0, h / 2.0];
    let p = Packer { id, node, t };
    match def.func {
        "solid" => p.color(out, "color", [0.0, 0.0, 0.0, 1.0])?,
        "gradient_linear" | "gradient_radial" | "gradient_conic" => {
            let (a, b) = if def.func == "gradient_radial" {
                ([1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0])
            } else {
                ([0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0])
            };
            p.gradient(out, def.func, a, b, center, h)?;
        }
        "shape" => {
            p.color(out, "color", [1.0, 1.0, 1.0, 1.0])?;
            fade(out, p.num("opacity", 1.0));
            p.vec(out, "center", &center);
            // `scale` about the center scales the size, corners and stroke.
            let sc = p.vecn("scale", &[1.0, 1.0]);
            let k = (sc[0].abs() + sc[1].abs()) / 2.0;
            let size = p.vecn("size", &[w / 4.0, w / 4.0]);
            out.extend([
                (size[0] * sc[0].abs() / 2.0) as f32,
                (size[1] * sc[1].abs() / 2.0) as f32,
            ]);
            out.push((p.num("radius", 0.0) * k) as f32);
            let ellipse = match text(node, "kind").unwrap_or("rect") {
                "rect" => false,
                "ellipse" | "circle" => true,
                other => return Err(bad(id, "kind", format!("unknown shape kind `{other}`"))),
            };
            out.push(ellipse as u8 as f32);
            p.vec(out, "softness", &[1.0]);
            out.push(p.num("rotation", 0.0).to_radians() as f32);
            out.push((p.num("stroke", 0.0) * k) as f32);
        }
        "checker" => {
            p.color(out, "a", [0.2, 0.2, 0.2, 1.0])?;
            p.color(out, "b", [0.4, 0.4, 0.4, 1.0])?;
            p.vec(out, "size", &[32.0]);
        }
        "noise" => {
            p.color(out, "a", [0.0, 0.0, 0.0, 1.0])?;
            p.color(out, "b", [1.0, 1.0, 1.0, 1.0])?;
            p.vec(out, "scale", &[200.0, 200.0]);
            p.vec(out, "offset", &[0.0, 0.0]);
            p.vec(out, "evolution", &[0.0]);
            p.vec(out, "octaves", &[4.0]);
        }
        "image" => {
            let (rect, window) = placement(&p, w, h, res)?;
            out.extend(rect.iter().map(|x| *x as f32));
            out.push(p.num("opacity", 1.0) as f32);
            out.push(space(id, node)?);
            out.extend(window.iter().map(|x| *x as f32));
        }
        "opacity" => out.push(p.num("opacity", 1.0) as f32),
        "over" => {
            out.push(p.num("opacity", 1.0) as f32);
            out.push(space(id, node)?);
        }
        "blend" => {
            let mode = text(node, "mode").unwrap_or("normal");
            let m = BLEND_MODES
                .iter()
                .position(|n| *n == mode)
                .ok_or_else(|| bad(id, "mode", format!("unknown blend mode `{mode}`")))?;
            out.push(m as f32);
            out.push(p.num("opacity", 1.0) as f32);
            out.push(space(id, node)?);
        }
        "composite" => {
            let op = text(node, "operator").unwrap_or("over");
            let i = ["in", "out", "atop", "xor", "plus", "over"]
                .iter()
                .position(|n| *n == op)
                .ok_or_else(|| bad(id, "operator", format!("unknown operator `{op}`")))?;
            out.push(i as f32);
        }
        "mask" => out.push(match text(node, "mode").unwrap_or("alpha") {
            "alpha" => 0.0,
            "alpha_inverted" | "inverted" => 1.0,
            "luma" => 2.0,
            "luma_inverted" => 3.0,
            other => return Err(bad(id, "mode", format!("unknown mask mode `{other}`"))),
        }),
        "mix" => out.push(p.num("t", 0.5) as f32),
        "adjust" => {
            out.push(p.num("exposure", 0.0) as f32);
            out.push(p.num("contrast", 1.0) as f32);
            out.push(p.num("saturation", 1.0) as f32);
            out.push(p.num("hue", 0.0).to_radians() as f32);
            out.push(p.num("brightness", 0.0) as f32);
            out.push(p.num("gamma", 1.0) as f32);
        }
        "levels" => {
            for (name, d) in [
                ("in_black", 0.0),
                ("in_white", 1.0),
                ("gamma", 1.0),
                ("out_black", 0.0),
                ("out_white", 1.0),
            ] {
                out.push(p.num(name, d) as f32);
            }
        }
        "color_matrix" => {
            let mut ident = vec![0.0; 20];
            for i in 0..4 {
                ident[i * 5 + i] = 1.0;
            }
            p.vec(out, "matrix", &ident);
        }
        "invert" => out.push(p.num("amount", 1.0) as f32),
        "lut" => {
            out.push(p.num("amount", 1.0) as f32);
            out.push(res.map(|r| r.height as f32).unwrap_or(0.0));
            out.push(match text(node, "space").unwrap_or("srgb") {
                "srgb" => 0.0,
                "linear" => 1.0,
                o => return Err(bad(id, "space", format!("unknown space `{o}` (srgb | linear)"))),
            });
        }
        "tint" => {
            p.color(out, "dark", [0.0, 0.0, 0.0, 1.0])?;
            p.color(out, "light", [1.0, 1.0, 1.0, 1.0])?;
        }
        "threshold" => {
            out.push(p.num("level", 0.5) as f32);
            out.push(p.num("softness", 0.01) as f32);
        }
        "crop" => {
            p.vec(out, "rect", &[0.0, 0.0, w, h]);
            p.vec(out, "softness", &[1.0]);
        }
        "transform" => {
            let pos = p.vecn("position", &center);
            let anchor = p.vecn("anchor", &center);
            let scale = p.vecn("scale", &[1.0, 1.0]);
            let rot = p.num("rotation", 0.0).to_radians();
            let skew = p.num("skew", 0.0).to_radians();
            // Forward: p' = T(pos) * R * K(skew) * S * T(-anchor) * p. Pack the inverse.
            let m = Affine::translate(pos[0], pos[1])
                .mul(Affine::rotate(rot))
                .mul(Affine::skew_x(skew))
                .mul(Affine::scale(scale[0], scale[1]))
                .mul(Affine::translate(-anchor[0], -anchor[1]));
            let inv = m
                .inverse()
                .ok_or_else(|| bad(id, "scale", "transform is not invertible (zero scale)".into()))?;
            out.extend(inv.0.iter().map(|x| *x as f32));
            out.push(inv.det().abs().sqrt() as f32);
        }
        "corner_pin" => {
            let c = p.vecn("corners", &[0.0, 0.0, w, 0.0, w, h, 0.0, h]);
            let src = p.vecn("source_rect", &[0.0, 0.0, w, h]);
            let from = [
                [src[0], src[1]],
                [src[0] + src[2], src[1]],
                [src[0] + src[2], src[1] + src[3]],
                [src[0], src[1] + src[3]],
            ];
            let to = [[c[0], c[1]], [c[2], c[3]], [c[4], c[5]], [c[6], c[7]]];
            // Homography dest -> source, so the shader maps output to input.
            let hm = homography(&to, &from).ok_or_else(|| bad(id, "corners", "corners are degenerate".into()))?;
            out.extend(hm.iter().map(|x| *x as f32));
            let area = |q: &[[f64; 2]; 4]| {
                let mut a = 0.0;
                for i in 0..4 {
                    let (p0, p1) = (q[i], q[(i + 1) % 4]);
                    a += p0[0] * p1[1] - p1[0] * p0[1];
                }
                (a / 2.0).abs()
            };
            out.push((area(&from) / area(&to).max(1e-6)).sqrt() as f32);
        }
        "wave" => {
            out.push(p.num("amplitude", 10.0) as f32);
            out.push(p.num("wavelength", 100.0) as f32);
            out.push(p.num("phase", 0.0).to_radians() as f32);
            out.push(p.num("direction", 0.0).to_radians() as f32);
        }
        "twirl" => {
            p.vec(out, "center", &center);
            out.push(p.num("radius", w.min(h) / 2.0) as f32);
            out.push(p.num("angle", 90.0).to_radians() as f32);
        }
        "tile" => {
            p.vec(out, "origin", &[0.0, 0.0]);
            p.vec(out, "size", &[w, h]);
        }
        "displace" => p.vec(out, "amount", &[20.0, 20.0]),
        "blur" => {
            // Packed per pass by the executor (direction varies).
            out.extend([p.num("radius", 8.0) as f32, 1.0, 0.0, 0.0]);
        }
        // Scene objects are read directly by the 3D pass.
        "mesh" | "light" | "camera" | "render3d" | "audio" | "particles" | "swarm" | "fluid" | "feedback" => {}
        "shader" => {
            let mut u = p.vecn("u", &[0.0; 16]);
            for (i, slot) in u.iter_mut().enumerate() {
                const U: [&str; 16] = [
                    "u0", "u1", "u2", "u3", "u4", "u5", "u6", "u7", "u8", "u9", "u10", "u11", "u12", "u13", "u14",
                    "u15",
                ];
                if let Some(v) = node.params.get(U[i]).and_then(|v| v.eval_vals(t)) {
                    *slot = v.first().copied().unwrap_or(0.0);
                }
            }
            out.extend(u.iter().map(|x| *x as f32));
            // The node's local time (components and timing shift it).
            out.push(t as f32);
        }
        "path" | "text" => {
            p.color(out, "color", [1.0, 1.0, 1.0, 1.0])?;
            fade(out, p.num("opacity", 1.0));
            out.push(match text(node, "fill_rule").unwrap_or("nonzero") {
                "nonzero" => 0.0,
                "evenodd" => 1.0,
                o => return Err(bad(id, "fill_rule", format!("unknown fill rule `{o}`"))),
            });
        }
        other => unreachable!("no packer for {other}"),
    }
    debug_assert_eq!(out.len() - start, def.params, "param block size for {}", def.func);
    Ok(())
}

/// Fades the premultiplied color just packed by `opacity`.
fn fade(out: &mut [f32], opacity: f64) {
    let k = opacity.clamp(0.0, 1.0) as f32;
    let n = out.len();
    for v in &mut out[n - 4..] {
        *v *= k;
    }
}

/// Forward matrix of a `transform` node:
/// p' = T(position) * R(rotation) * K(skew) * S(scale) * T(-anchor) * p.
pub fn transform_affine(node: &Node, fmt: &Format, t: f64) -> Affine {
    let p = Packer { id: "", node, t };
    let center = [fmt.width as f64 / 2.0, fmt.height as f64 / 2.0];
    let pos = p.vecn("position", &center);
    let anchor = p.vecn("anchor", &center);
    let scale = p.vecn("scale", &[1.0, 1.0]);
    let rot = p.num("rotation", 0.0).to_radians();
    let skew = p.num("skew", 0.0).to_radians();
    Affine::translate(pos[0], pos[1])
        .mul(Affine::rotate(rot))
        .mul(Affine::skew_x(skew))
        .mul(Affine::scale(scale[0], scale[1]))
        .mul(Affine::translate(-anchor[0], -anchor[1]))
}

/// The placement params of a `shape` ([`shape_motion_px`] measures them).
pub const SHAPE_PLACEMENT: &[&str] = &["center", "size", "scale", "rotation", "radius", "stroke", "softness"];

/// How far (px, an upper bound) a `shape` node's edges move between local
/// times l0 and l1 through its placement params.
pub fn shape_motion_px(node: &Node, fmt: &Format, l0: f64, l1: f64) -> f64 {
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    let at = |t: f64| {
        let p = Packer { id: "", node, t };
        let c = p.vecn("center", &[w / 2.0, h / 2.0]);
        let s = p.vecn("size", &[w / 4.0, w / 4.0]);
        let k = p.vecn("scale", &[1.0, 1.0]);
        let half = [(s[0] * k[0]).abs() / 2.0, (s[1] * k[1]).abs() / 2.0];
        let extra = [p.num("radius", 0.0), p.num("stroke", 0.0), p.num("softness", 1.0)];
        (c, half, p.num("rotation", 0.0).to_radians(), extra)
    };
    let (c0, e0, r0, x0) = at(l0);
    let (c1, e1, r1, x1) = at(l1);
    (c1[0] - c0[0]).hypot(c1[1] - c0[1])
        + (e1[0] - e0[0]).abs().max((e1[1] - e0[1]).abs())
        + (r1 - r0).abs() * e0[0].hypot(e0[1]).max(e1[0].hypot(e1[1]))
        + x0.iter().zip(&x1).map(|(a, b)| (a - b).abs()).sum::<f64>()
}

/// How far (px, an upper bound) a `wave` node's displacement changes
/// between local times l0 and l1 through its amplitude and phase.
pub fn wave_motion_px(node: &Node, l0: f64, l1: f64) -> f64 {
    let at = |t: f64| {
        let p = Packer { id: "", node, t };
        (p.num("amplitude", 10.0), p.num("phase", 0.0).to_radians())
    };
    let ((a0, p0), (a1, p1)) = (at(l0), at(l1));
    (a1 - a0).abs() + a0.abs().max(a1.abs()) * (p1 - p0).abs()
}

/// Axis-aligned bounds (x0, y0, x1, y1) of where a node can be non-transparent.
pub type Rect = [f64; 4];
pub const UNBOUNDED: Rect = [-1e9, -1e9, 1e9, 1e9];
pub const EMPTY: Rect = [1e9, 1e9, -1e9, -1e9];

fn union(a: Rect, b: Rect) -> Rect {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn intersect(a: Rect, b: Rect) -> Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// Ops whose fg input is guarded by its bounds in fused kernels: pixels
/// outside skip evaluating the fg subtree entirely.
pub fn guarded(def: &OpDef) -> bool {
    matches!(def.name, "over" | "blend")
}

/// Where an image or video frame is drawn ([x, y, w, h] px) and the part
/// of it that shows ([u0, v0, u1, v1]), by `fit` inside `rect` (a box) or
/// the frame. A picture keeps its aspect ratio unless `fit` is "fill": in
/// a box it defaults to "cover" (the box filled, the overflow cropped
/// evenly), in the frame to "contain". A box of the picture's own shape
/// draws it exactly there, as before.
fn placement(p: &Packer, w: f64, h: f64, res: Option<ResourceInfo>) -> Result<([f64; 4], [f64; 4]), OpsError> {
    let r = res.unwrap_or(ResourceInfo { width: 1, height: 1 });
    let (iw, ih) = (r.width as f64, r.height as f64);
    let boxed = p.node.params.contains_key("rect");
    let b = if boxed {
        let v = p.vecn("rect", &[0.0, 0.0, iw, ih]);
        [v[0], v[1], v[2], v[3]]
    } else {
        [0.0, 0.0, w, h]
    };
    let whole = [0.0, 0.0, 1.0, 1.0];
    let fit = text(p.node, "fit").unwrap_or(if boxed { "cover" } else { "contain" });
    // Within 1%: rounding in a box sized for the picture never crops it.
    let same_shape = res.is_some() && b[2] > 0.0 && b[3] > 0.0 && ((b[2] / b[3]) / (iw / ih) - 1.0).abs() < 1e-2;
    let centered = |sw: f64, sh: f64| [b[0] + (b[2] - sw) / 2.0, b[1] + (b[3] - sh) / 2.0, sw, sh];
    Ok(match fit {
        "fill" => (b, whole),
        _ if boxed && same_shape && fit != "none" => (b, whole),
        "none" => (centered(iw, ih), whole),
        "contain" => {
            let s = (b[2] / iw).min(b[3] / ih);
            (centered(iw * s, ih * s), whole)
        }
        // Without a box the frame crops the overflow; in a box the window does.
        "cover" if !boxed => {
            let s = (b[2] / iw).max(b[3] / ih);
            (centered(iw * s, ih * s), whole)
        }
        "cover" => {
            let s = (b[2] / iw).max(b[3] / ih);
            let (u, v) = (b[2] / (iw * s), b[3] / (ih * s));
            (b, [(1.0 - u) / 2.0, (1.0 - v) / 2.0, (1.0 + u) / 2.0, (1.0 + v) / 2.0])
        }
        other => return Err(bad(p.id, "fit", format!("unknown fit `{other}`"))),
    })
}

/// Conservative bounds of a node's output in its own coordinate space,
/// given its inputs' bounds. Anything unknown is unbounded.
pub fn bounds(def: &OpDef, node: &Node, fmt: &Format, t: f64, inputs: &[Rect], res: Option<ResourceInfo>) -> Rect {
    let p = Packer { id: "", node, t };
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    let input = |i: usize| inputs.get(i).copied().unwrap_or(UNBOUNDED);
    match def.func {
        "shape" => {
            let c = p.vecn("center", &[w / 2.0, h / 2.0]);
            let sc = p.vecn("scale", &[1.0, 1.0]);
            let size = p.vecn("size", &[w / 4.0, w / 4.0]);
            let size = [size[0] * sc[0].abs(), size[1] * sc[1].abs()];
            // Bounding circle of the (possibly rotated) shape, plus room for
            // the stroke and antialiasing even under strong minification.
            let r = (size[0] * size[0] + size[1] * size[1]).sqrt() / 2.0
                + p.num("stroke", 0.0)
                + p.num("softness", 1.0).max(1.0) * 8.0;
            [c[0] - r, c[1] - r, c[0] + r, c[1] + r]
        }
        "image" => match (node.params.contains_key("rect"), text(node, "fit"), res) {
            (true, Some("none"), Some(_)) => match placement(&p, w, h, res) {
                Ok((r, _)) => [r[0] - 1.0, r[1] - 1.0, r[0] + r[2] + 1.0, r[1] + r[3] + 1.0],
                Err(_) => UNBOUNDED,
            },
            (true, Some("none"), None) => UNBOUNDED,
            (true, _, _) => {
                let r = p.vecn("rect", &[0.0, 0.0, w, h]);
                [r[0] - 1.0, r[1] - 1.0, r[0] + r[2] + 1.0, r[1] + r[3] + 1.0]
            }
            _ => UNBOUNDED,
        },
        "opacity" | "adjust" | "levels" | "invert" | "tint" | "threshold" => input(0),
        "crop" => {
            let r = p.vecn("rect", &[0.0, 0.0, w, h]);
            intersect(input(0), [r[0] - 1.0, r[1] - 1.0, r[0] + r[2] + 1.0, r[1] + r[3] + 1.0])
        }
        "mask" => match text(node, "mode").unwrap_or("alpha") {
            "alpha" | "luma" => intersect(input(0), input(1)),
            _ => input(0),
        },
        "over" | "blend" | "mix" => union(input(0), input(1)),
        "transform" => {
            let b = input(0);
            if b[0] <= -1e8 || b[0] > b[2] {
                return b;
            }
            let m = transform_affine(node, fmt, t);
            let pts = [(b[0], b[1]), (b[2], b[1]), (b[0], b[3]), (b[2], b[3])].map(|(x, y)| m.apply(x, y));
            let xs = pts.map(|p| p.0);
            let ys = pts.map(|p| p.1);
            let f = |v: [f64; 4], min: bool| {
                v.into_iter().fold(if min { f64::MAX } else { f64::MIN }, |a, b| {
                    if min { a.min(b) } else { a.max(b) }
                })
            };
            [
                f(xs, true) - 2.0,
                f(ys, true) - 2.0,
                f(xs, false) + 2.0,
                f(ys, false) + 2.0,
            ]
        }
        _ => UNBOUNDED,
    }
}

/// Gradient block: stop count, space, 4 geometry floats, 8 stops of
/// (position, premultiplied rgba).
pub const GRADIENT_PARAMS: usize = 6 + 8 * 5;

/// Parses gradient stops: `"0:#ff0000, 0.5:#00ff0080, 1:#0000ff"`, or a
/// flat vector of `[pos, r, g, b, a]` groups (sRGB, straight alpha).
fn parse_stops(p: &Param, t: f64) -> Option<Vec<(f64, [f64; 4])>> {
    match p {
        Param::Text(s) => s
            .split(',')
            .map(|part| {
                let (pos, col) = part.trim().split_once(':')?;
                Some((pos.trim().parse().ok()?, parse_hex(col.trim())?))
            })
            .collect(),
        p => {
            let v = p.eval(t)?;
            (v.len() % 5 == 0 && !v.is_empty())
                .then(|| v.chunks_exact(5).map(|c| (c[0], [c[1], c[2], c[3], c[4]])).collect())
        }
    }
}

pub const BLEND_MODES: &[&str] = &[
    "normal",
    "multiply",
    "screen",
    "overlay",
    "darken",
    "lighten",
    "color_dodge",
    "color_burn",
    "hard_light",
    "soft_light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
    "add",
    "subtract",
    "divide",
];

fn space(id: &str, node: &Node) -> Result<f32, OpsError> {
    match text(node, "space").unwrap_or("linear") {
        "linear" => Ok(0.0),
        "srgb" => Ok(1.0),
        other => Err(bad(id, "space", format!("unknown space `{other}` (linear | srgb)"))),
    }
}

fn bad(id: &str, param: &str, reason: String) -> OpsError {
    OpsError::BadParam {
        node: id.to_string(),
        param: param.to_string(),
        reason,
    }
}

struct Packer<'a> {
    id: &'a str,
    node: &'a Node,
    t: f64,
}

impl Packer<'_> {
    fn vecn(&self, name: &str, default: &[f64]) -> ultrafast_graph::Vals {
        let mut v = self
            .node
            .params
            .get(name)
            .and_then(|p| p.eval_vals(self.t))
            .unwrap_or_else(|| ultrafast_graph::Vals::from_slice(default));
        // Broadcast a scalar to the expected width; pad or truncate otherwise.
        if v.len() == 1 && default.len() > 1 {
            v = ultrafast_graph::Vals::filled(v[0], default.len());
        }
        v.resize(default.len(), 0.0);
        v
    }

    fn vec(&self, out: &mut Vec<f32>, name: &str, default: &[f64]) {
        out.extend(self.vecn(name, default).into_iter().map(|x| x as f32));
    }

    fn num(&self, name: &str, default: f64) -> f64 {
        self.vecn(name, &[default])[0]
    }

    /// Packs a gradient: stops (from `stops`, else `a`/`b`), interpolation
    /// space, and geometry for the kind.
    fn gradient(
        &self,
        out: &mut Vec<f32>,
        func: &str,
        a: [f64; 4],
        b: [f64; 4],
        center: [f64; 2],
        h: f64,
    ) -> Result<(), OpsError> {
        let mut stops = match self.node.params.get("stops") {
            Some(p) => parse_stops(p, self.t).ok_or_else(|| {
                bad(
                    self.id,
                    "stops",
                    "use \"pos:#rrggbb, ...\" or [pos, r, g, b, a, ...]".into(),
                )
            })?,
            None => {
                let mut ca = Vec::new();
                let mut cb = Vec::new();
                self.color_straight(&mut ca, "a", a)?;
                self.color_straight(&mut cb, "b", b)?;
                vec![(0.0, [ca[0], ca[1], ca[2], ca[3]]), (1.0, [cb[0], cb[1], cb[2], cb[3]])]
            }
        };
        stops.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
        stops.truncate(8);
        let encoded = match text(self.node, "space").unwrap_or("linear") {
            "linear" => false,
            "srgb" => true,
            o => return Err(bad(self.id, "space", format!("unknown space `{o}` (linear | srgb)"))),
        };
        out.push(stops.len() as f32);
        out.push(encoded as u8 as f32);
        match func {
            "gradient_linear" => {
                self.vec(out, "start", &[0.0, 0.0]);
                self.vec(out, "end", &[0.0, h]);
            }
            "gradient_radial" => {
                self.vec(out, "center", &center);
                self.vec(out, "radius", &[center[0].max(center[1])]);
                out.push(0.0);
            }
            _ => {
                self.vec(out, "center", &center);
                out.push(self.num("angle", 0.0).to_radians() as f32);
                out.push(0.0);
            }
        }
        for k in 0..8 {
            let (pos, c) = stops.get(k).copied().unwrap_or((1.0, [0.0; 4]));
            // Premultiplied, in the interpolation space.
            let al = c[3].clamp(0.0, 1.0);
            let ch = |x: f64| if encoded { x * al } else { srgb_to_linear(x) * al };
            out.extend([pos as f32, ch(c[0]) as f32, ch(c[1]) as f32, ch(c[2]) as f32, al as f32]);
        }
        Ok(())
    }

    /// A document color as straight sRGB (no conversion).
    fn color_straight(&self, out: &mut Vec<f64>, name: &str, default: [f64; 4]) -> Result<(), OpsError> {
        let c = match self.node.params.get(name) {
            None => default,
            Some(Param::Text(s)) => parse_hex(s).ok_or_else(|| {
                bad(
                    self.id,
                    name,
                    format!("`{s}` is not a #rgb, #rrggbb or #rrggbbaa color"),
                )
            })?,
            Some(p) => {
                let v = p.eval(self.t).unwrap_or_default();
                match v.len() {
                    3 => [v[0], v[1], v[2], 1.0],
                    4 => [v[0], v[1], v[2], v[3]],
                    _ => return Err(bad(self.id, name, "color needs 3 or 4 components".into())),
                }
            }
        };
        out.extend(c);
        Ok(())
    }

    /// Document colors are straight-alpha, sRGB-encoded: `[r,g,b(,a)]` in 0..1
    /// or `#rrggbb(aa)`. The engine works in linear premultiplied.
    fn color(&self, out: &mut Vec<f32>, name: &str, default: [f64; 4]) -> Result<(), OpsError> {
        let c = match self.node.params.get(name) {
            None => default,
            Some(Param::Text(s)) => parse_hex(s).ok_or_else(|| {
                bad(
                    self.id,
                    name,
                    format!("`{s}` is not a #rgb, #rrggbb or #rrggbbaa color"),
                )
            })?,
            Some(p) => {
                let v = p.eval(self.t).unwrap_or_default();
                match v.len() {
                    3 => [v[0], v[1], v[2], 1.0],
                    4 => [v[0], v[1], v[2], v[3]],
                    _ => return Err(bad(self.id, name, "color needs 3 or 4 components".into())),
                }
            }
        };
        let a = c[3].clamp(0.0, 1.0);
        out.extend([
            (srgb_to_linear(c[0]) * a) as f32,
            (srgb_to_linear(c[1]) * a) as f32,
            (srgb_to_linear(c[2]) * a) as f32,
            a as f32,
        ]);
        Ok(())
    }
}

/// Homography mapping 4 `from` points to 4 `to` points, row-major 3x3.
pub fn homography(from: &[[f64; 2]; 4], to: &[[f64; 2]; 4]) -> Option<[f64; 9]> {
    // Solve the 8x8 linear system with h33 = 1.
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (from[i][0], from[i][1]);
        let (u, v) = (to[i][0], to[i][1]);
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for col in 0..8 {
        let piv = (col..8).max_by(|&a, &b| m[a][col].abs().partial_cmp(&m[b][col].abs()).unwrap())?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        for row in 0..8 {
            if row != col {
                let f = m[row][col] / m[col][col];
                for k in col..9 {
                    m[row][k] -= f * m[col][k];
                }
            }
        }
    }
    let mut h = [0.0; 9];
    for i in 0..8 {
        h[i] = m[i][8] / m[i][i];
    }
    h[8] = 1.0;
    Some(h)
}

#[cfg(test)]
mod tests {
    fn placed(params: &str) -> ([f64; 4], [f64; 4]) {
        let node: Node =
            serde_json::from_str(&format!(r#"{{"op":"image","params":{{"src":"a.jpg"{params}}}}}"#)).unwrap();
        let p = Packer {
            id: "img",
            node: &node,
            t: 0.0,
        };
        // A portrait picture (1280x1551) on a 1080x1920 frame.
        placement(
            &p,
            1080.0,
            1920.0,
            Some(ResourceInfo {
                width: 1280,
                height: 1551,
            }),
        )
        .unwrap()
    }

    #[test]
    fn pictures_keep_their_shape_in_a_box_unless_asked_to_fill() {
        let whole = [0.0, 0.0, 1.0, 1.0];
        // Cover (the default in a box): the box filled, the sides cropped evenly.
        let (r, w) = placed(r#","rect":[40,190,1000,1560]"#);
        assert_eq!(r, [40.0, 190.0, 1000.0, 1560.0]);
        let u = (1000.0 / 1560.0) / (1280.0 / 1551.0);
        assert!(
            (w[0] - (1.0 - u) / 2.0).abs() < 1e-9
                && (w[2] - (1.0 + u) / 2.0).abs() < 1e-9
                && w[1] == 0.0
                && w[3] == 1.0,
            "{w:?}"
        );
        assert_eq!(placed(r#","rect":[40,190,1000,1560],"fit":"cover""#), (r, w));
        // Contain: the whole picture, centred in the box.
        let (r, w) = placed(r#","rect":[40,190,1000,1560],"fit":"contain""#);
        assert!(
            (r[2] - 1000.0).abs() < 1e-9 && (r[3] - 1000.0 * 1551.0 / 1280.0).abs() < 1e-9 && w == whole,
            "{r:?}"
        );
        assert!((r[1] - (190.0 + (1560.0 - r[3]) / 2.0)).abs() < 1e-9);
        // Fill stretches only when asked for.
        assert_eq!(
            placed(r#","rect":[40,190,1000,1560],"fit":"fill""#),
            ([40.0, 190.0, 1000.0, 1560.0], whole)
        );
        // A box of the picture's own shape draws it exactly there, as before.
        assert_eq!(placed(r#","rect":[0,0,640,775.5]"#), ([0.0, 0.0, 640.0, 775.5], whole));
        // Without a box nothing changes: contain in the frame by default.
        let (r, w) = placed("");
        assert!(
            (r[2] - 1080.0).abs() < 1e-9 && (r[0]).abs() < 1e-9 && w == whole,
            "{r:?}"
        );
    }

    #[test]
    fn sequence_frames_land_on_boundaries_after_offsets() {
        let node: Node =
            serde_json::from_str(r#"{"op":"image","params":{"src":"f{frame:03}.png","first_frame":5}}"#).unwrap();
        // Local times of frames under a 0.37 s offset; some land a hair
        // under their boundary (f / 30 * 30 < f).
        let under = (0..300u32)
            .filter(|&f| ((f as f64 / 30.0 + 0.37) - 0.37) * 30.0 < f as f64)
            .count();
        assert!(under > 0);
        for f in 0..300u32 {
            let lt = (f as f64 / 30.0 + 0.37) - 0.37;
            assert_eq!(
                sequence_path("f{frame}.png", &node, lt, 30.0),
                format!("f{}.png", f + 5),
                "frame {f}"
            );
        }
    }

    use super::*;

    #[test]
    fn every_op_packs_its_declared_size() {
        let fmt = Format {
            width: 64,
            height: 32,
            fps: 30.0,
            duration: 1.0,
            motion_blur: None,
        };
        for def in OPS {
            let node = Node {
                op: def.name.into(),
                ..Default::default()
            };
            let mut out = Vec::new();
            pack(def, "n", &node, &fmt, 0.0, None, &mut out).unwrap();
            assert_eq!(out.len(), def.params, "{}", def.func);
        }
    }

    #[test]
    fn colors_are_linearized_and_premultiplied() {
        let mut node = Node {
            op: "solid".into(),
            ..Default::default()
        };
        node.params.insert("color".into(), Param::Text("#ffffff80".into()));
        let fmt = Format {
            width: 1,
            height: 1,
            fps: 1.0,
            duration: 1.0,
            motion_blur: None,
        };
        let mut out = Vec::new();
        pack(resolve("n", &node).unwrap(), "n", &node, &fmt, 0.0, None, &mut out).unwrap();
        let a = 128.0 / 255.0;
        assert!((out[0] - a as f32).abs() < 1e-6);
        assert!((out[3] - a as f32).abs() < 1e-6);
    }

    #[test]
    fn homography_maps_corners() {
        let from = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let to = [[1.0, 2.0], [12.0, 1.0], [11.0, 13.0], [0.0, 9.0]];
        let h = homography(&from, &to).unwrap();
        for i in 0..4 {
            let (x, y) = (from[i][0], from[i][1]);
            let w = h[6] * x + h[7] * y + h[8];
            let u = (h[0] * x + h[1] * y + h[2]) / w;
            let v = (h[3] * x + h[4] * y + h[5]) / w;
            assert!((u - to[i][0]).abs() < 1e-9 && (v - to[i][1]).abs() < 1e-9);
        }
    }

    #[test]
    fn affine_inverse_roundtrips() {
        let m = Affine::translate(5.0, 3.0)
            .mul(Affine::rotate(0.7))
            .mul(Affine::scale(2.0, 0.5));
        let (x, y) = m.apply(3.0, -4.0);
        let (bx, by) = m.inverse().unwrap().apply(x, y);
        assert!((bx - 3.0).abs() < 1e-9 && (by + 4.0).abs() < 1e-9);
    }
}
