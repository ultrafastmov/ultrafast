//! Lottie import: a Lottie (Bodymovin) animation becomes ordinary ultrafast
//! nodes, so every shape, fill, stroke and keyframe of it is editable.
//!
//! Shape layers become `path` nodes (one per geometry and paint, in paint
//! order) with Lottie's keyframes and eases as ultrafast keys; group, layer and
//! parent transforms become `transform` nodes stacked on each path, so they
//! are absorbed into the geometry and stay sharp. Precomps are inlined with
//! their time offsets and stretch; masks and track mattes become `mask`
//! nodes; solids, images and text layers become `path`, `image` and `text`.
//! Animated shapes morph between their keyframes.
//!
//! Not (yet) imported: expressions, time remapping, repeaters, merge paths,
//! rounded corners, 3D layers, layer effects and blend modes. They are
//! reported as warnings.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use ultrafast_graph::{Component, Document, Node, Param, Timing};

pub struct Imported {
    pub component: Component,
    /// Composition size in px after fitting, and duration in seconds.
    pub size: [f64; 2],
    pub duration: f64,
    pub warnings: Vec<String>,
}

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "lottie")
        || doc
            .components
            .values()
            .any(|c| c.nodes.values().any(|n| n.op == "lottie"))
}

/// Parameters a placed Lottie instance exposes.
pub const PLACEMENT: &[&str] = &["position", "anchor", "scale", "rotation", "opacity"];

// ------------------------------------------------------------ values

fn nums(v: &Value) -> Vec<f64> {
    match v {
        Value::Number(n) => vec![n.as_f64().unwrap_or(0.0)],
        Value::Array(a) => a.iter().filter_map(Value::as_f64).collect(),
        _ => Vec::new(),
    }
}

fn first(v: Option<&Value>) -> f64 {
    v.map(nums).and_then(|n| n.first().copied()).unwrap_or(0.0)
}

/// A stroke's width as a path's `stroke`, or None when it is 0 throughout:
/// nothing to draw (a path's `stroke: 0` fills it instead). Keys at 0
/// become a hairline, so the path stays a stroke at those moments.
fn stroke_width(w: Param) -> Option<Param> {
    const HAIRLINE: f64 = 1e-3;
    match w {
        Param::Number(v) if v <= 0.0 => None,
        Param::Animated { mut keys, repeat } => {
            if keys.iter().all(|k| k.v.iter().all(|x| *x <= 0.0)) {
                return None;
            }
            for x in keys.iter_mut().flat_map(|k| k.v.iter_mut()) {
                *x = x.max(HAIRLINE);
            }
            Some(Param::Animated { keys, repeat })
        }
        w => Some(w),
    }
}

fn keyframes(prop: &Value) -> Option<&Vec<Value>> {
    let k = prop.get("k")?.as_array()?;
    k.first().filter(|f| f.is_object() && f.get("t").is_some()).map(|_| k)
}

/// Solves a CSS-style cubic bezier ease (x1, y1, x2, y2) at progress `x`.
fn bezier(e: [f64; 4], x: f64) -> f64 {
    let [x1, y1, x2, y2] = e;
    let c = |a: f64, b: f64, u: f64| 3.0 * a * u * (1.0 - u) * (1.0 - u) + 3.0 * b * u * u * (1.0 - u) + u * u * u;
    let (mut lo, mut hi) = (0.0, 1.0);
    let mut u = x;
    for _ in 0..30 {
        let v = c(x1, x2, u);
        if (v - x).abs() < 1e-6 {
            break;
        }
        if v < x {
            lo = u;
        } else {
            hi = u;
        }
        u = (lo + hi) / 2.0;
    }
    c(y1, y2, u)
}

/// A key's ease toward the next one: hold, or the cubic bezier from its
/// out tangent (`o`) to the next key's in tangent (`i`, stored on it).
enum Ease {
    Hold,
    Bezier([f64; 4]),
}

fn ease_of(key: &Value) -> Ease {
    if key.get("h").and_then(Value::as_f64) == Some(1.0) {
        return Ease::Hold;
    }
    match (key.get("o"), key.get("i")) {
        (Some(o), Some(i)) => Ease::Bezier([
            first(o.get("x")),
            first(o.get("y")),
            first(i.get("x")),
            first(i.get("y")),
        ]),
        _ => Ease::Bezier([0.0, 0.0, 1.0, 1.0]),
    }
}

/// A key's start value (`s`), or the previous key's end (`e`, old files).
fn key_value(keys: &[Value], i: usize) -> Option<&Value> {
    keys[i]
        .get("s")
        .or_else(|| i.checked_sub(1).and_then(|p| keys[p].get("e")))
}

/// A numeric property at frame `f` (Lottie's own interpolation).
fn eval(prop: &Value, f: f64) -> Vec<f64> {
    let Some(keys) = keyframes(prop) else {
        return prop.get("k").map(nums).unwrap_or_default();
    };
    let t = |i: usize| keys[i]["t"].as_f64().unwrap_or(0.0);
    let val = |i: usize| key_value(keys, i).map(nums).unwrap_or_default();
    if f <= t(0) {
        return val(0);
    }
    for i in 0..keys.len() - 1 {
        if f < t(i + 1) {
            let a = val(i);
            let b = keys[i].get("e").map(nums).unwrap_or_else(|| val(i + 1));
            let u = ((f - t(i)) / (t(i + 1) - t(i)).max(1e-9)).clamp(0.0, 1.0);
            let p = match ease_of(&keys[i]) {
                Ease::Hold => 0.0,
                Ease::Bezier(e) => bezier(e, u),
            };
            return a
                .iter()
                .zip(b.iter().chain(std::iter::repeat(&0.0)))
                .map(|(x, y)| x + (y - x) * p)
                .collect();
        }
    }
    let n = keys.len() - 1;
    let last = val(n);
    if last.is_empty() {
        keys[n - 1].get("e").map(nums).unwrap_or_else(|| val(n - 1))
    } else {
        last
    }
}

/// Key times (frames) of a property; empty when static.
fn key_times(prop: &Value) -> Vec<f64> {
    keyframes(prop)
        .map(|k| k.iter().filter_map(|x| x["t"].as_f64()).collect())
        .unwrap_or_default()
}

/// How much a text range selector applies to the letter at `ind` (of
/// `total`) at frame `f`, as lottie-web computes it.
fn selector_mult(sel: &Value, f: f64, ind: f64, total: f64) -> f64 {
    let v = |k: &str, d: f64| {
        sel.get(k)
            .map(|p| eval(p, f).first().copied().unwrap_or(d))
            .unwrap_or(d)
    };
    let by_index = sel.get("r").and_then(Value::as_f64) == Some(2.0);
    let divisor = if by_index { 1.0 } else { 100.0 / total.max(1e-9) };
    let o = v("o", 0.0) / divisor;
    let mut s = v("s", 0.0) / divisor + o;
    let mut e = if sel.get("e").is_some() {
        v("e", 100.0) / divisor + o
    } else if by_index {
        total + o
    } else {
        100.0 / divisor + o
    };
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let (ne, xe) = (v("ne", 0.0), v("xe", 0.0));
    let (mut x1, mut y1, mut x2, mut y2) = (0.0, 0.0, 1.0, 1.0);
    if ne > 0.0 {
        x1 = ne / 100.0;
    } else {
        y1 = -ne / 100.0;
    }
    if xe > 0.0 {
        x2 = 1.0 - xe / 100.0;
    } else {
        y2 = 1.0 + xe / 100.0;
    }
    let ease = |m: f64| bezier([x1, y1, x2, y2], m.clamp(0.0, 1.0));
    let ramp = || (0.5 / (e - s) + (ind - s) / (e - s)).clamp(0.0, 1.0);
    let mut mult = match first(sel.get("sh")) as i64 {
        2 => ease(if e == s { (ind >= e) as u8 as f64 } else { ramp() }),
        3 => ease(if e == s { (ind < e) as u8 as f64 } else { 1.0 - ramp() }),
        4 => ease(if e == s {
            0.0
        } else {
            let m = ramp();
            if m < 0.5 { m * 2.0 } else { 1.0 - 2.0 * (m - 0.5) }
        }),
        5 => ease(if e == s {
            0.0
        } else {
            let tot = e - s;
            let i = (ind + 0.5 - s).clamp(0.0, tot);
            let x = -tot / 2.0 + i;
            let a = tot / 2.0;
            (1.0 - (x * x) / (a * a)).max(0.0).sqrt()
        }),
        6 => ease(if e == s {
            0.0
        } else {
            let i = (ind + 0.5 - s).clamp(0.0, e - s);
            (1.0 + (std::f64::consts::PI + std::f64::consts::TAU * i / (e - s)).cos()) / 2.0
        }),
        _ => {
            let mut m = 0.0;
            if ind >= s.floor() {
                m = if ind - s < 0.0 {
                    (e.min(1.0) - (s - ind)).clamp(0.0, 1.0)
                } else {
                    (e - ind).clamp(0.0, 1.0)
                };
            }
            ease(m)
        }
    };
    let sm = v("sm", 100.0);
    if sm != 100.0 {
        let k = (sm * 0.01).max(1e-8);
        let threshold = 0.5 - k * 0.5;
        mult = if mult < threshold {
            0.0
        } else {
            ((mult - threshold) / k).min(1.0)
        };
    }
    mult * v("a", 100.0) / 100.0
}

// ------------------------------------------------------------ geometry

#[derive(Clone, Debug, Default)]
struct Bez {
    v: Vec<[f64; 2]>,
    i: Vec<[f64; 2]>,
    o: Vec<[f64; 2]>,
    closed: bool,
}

fn pts(v: Option<&Value>) -> Vec<[f64; 2]> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|p| {
                    let n = nums(p);
                    [n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0)]
                })
                .collect()
        })
        .unwrap_or_default()
}

fn bez_of(v: &Value) -> Bez {
    // Shape keyframe values are wrapped in a one-element array.
    let v = match v {
        Value::Array(a) => a.first().unwrap_or(&Value::Null),
        v => v,
    };
    Bez {
        v: pts(v.get("v")),
        i: pts(v.get("i")),
        o: pts(v.get("o")),
        closed: v.get("c").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// A shape property at frame `f`: vertices interpolate one to one.
fn shape_at(prop: &Value, f: f64) -> Bez {
    let Some(keys) = keyframes(prop) else {
        return prop.get("k").map(bez_of).unwrap_or_default();
    };
    let t = |i: usize| keys[i]["t"].as_f64().unwrap_or(0.0);
    let val = |i: usize| key_value(keys, i).map(bez_of).unwrap_or_default();
    if f <= t(0) {
        return val(0);
    }
    for i in 0..keys.len() - 1 {
        if f < t(i + 1) {
            let a = val(i);
            let b = keys[i].get("e").map(bez_of).unwrap_or_else(|| val(i + 1));
            let u = ((f - t(i)) / (t(i + 1) - t(i)).max(1e-9)).clamp(0.0, 1.0);
            let p = match ease_of(&keys[i]) {
                Ease::Hold => 0.0,
                Ease::Bezier(e) => bezier(e, u),
            };
            if a.v.len() != b.v.len() {
                return if p < 0.5 { a } else { b };
            }
            let lerp = |x: &[[f64; 2]], y: &[[f64; 2]]| -> Vec<[f64; 2]> {
                x.iter()
                    .zip(y)
                    .map(|(x, y)| [x[0] + (y[0] - x[0]) * p, x[1] + (y[1] - x[1]) * p])
                    .collect()
            };
            return Bez {
                v: lerp(&a.v, &b.v),
                i: lerp(&a.i, &b.i),
                o: lerp(&a.o, &b.o),
                closed: a.closed,
            };
        }
    }
    let n = keys.len() - 1;
    let last = val(n);
    if last.v.is_empty() {
        keys[n - 1].get("e").map(bez_of).unwrap_or_else(|| val(n - 1))
    } else {
        last
    }
}

fn bez_d(b: &Bez, d: &mut String) {
    use std::fmt::Write;
    let n = b.v.len();
    if n == 0 {
        return;
    }
    let g = |a: &[[f64; 2]], k: usize| a.get(k).copied().unwrap_or([0.0, 0.0]);
    let _ = write!(d, "M{:.3} {:.3}", b.v[0][0], b.v[0][1]);
    let segs = if b.closed { n } else { n - 1 };
    for k in 0..segs {
        let (a, z) = (k, (k + 1) % n);
        let c1 = [b.v[a][0] + g(&b.o, a)[0], b.v[a][1] + g(&b.o, a)[1]];
        let c2 = [b.v[z][0] + g(&b.i, z)[0], b.v[z][1] + g(&b.i, z)[1]];
        let _ = write!(
            d,
            " C{:.3} {:.3} {:.3} {:.3} {:.3} {:.3}",
            c1[0], c1[1], c2[0], c2[1], b.v[z][0], b.v[z][1]
        );
    }
    if b.closed {
        d.push_str(" Z");
    }
}

/// Path data of one geometry item at frame `f`.
fn item_d(item: &Value, f: f64) -> String {
    let mut d = String::new();
    let reverse = item.get("d").and_then(Value::as_f64) == Some(3.0);
    match item["ty"].as_str().unwrap_or("") {
        "sh" => {
            let mut b = shape_at(&item["ks"], f);
            // Older exporters mark closed shapes on the item.
            b.closed |= item.get("closed").and_then(Value::as_bool) == Some(true);
            bez_d(&b, &mut d)
        }
        "rc" => {
            let p = eval(&item["p"], f);
            let s = eval(&item["s"], f);
            let r = eval(&item["r"], f).first().copied().unwrap_or(0.0);
            let (cx, cy) = (p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0));
            let (w, h) = (s.first().copied().unwrap_or(0.0), s.get(1).copied().unwrap_or(0.0));
            let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
            let (x0, y0, x1, y1) = (cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
            let k = r * 0.4477;
            d = format!(
                "M{} {} L{} {} C{} {} {} {} {} {} L{} {} C{} {} {} {} {} {} L{} {} C{} {} {} {} {} {} L{} {} C{} {} {} {} {} {} Z",
                x1,
                y0 + r,
                x1,
                y1 - r,
                x1,
                y1 - k,
                x1 - k,
                y1,
                x1 - r,
                y1,
                x0 + r,
                y1,
                x0 + k,
                y1,
                x0,
                y1 - k,
                x0,
                y1 - r,
                x0,
                y0 + r,
                x0,
                y0 + k,
                x0 + k,
                y0,
                x0 + r,
                y0,
                x1 - r,
                y0,
                x1 - k,
                y0,
                x1,
                y0 + k,
                x1,
                y0 + r
            );
        }
        "el" => {
            let p = eval(&item["p"], f);
            let s = eval(&item["s"], f);
            let (cx, cy) = (p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0));
            let (rx, ry) = (
                s.first().copied().unwrap_or(0.0) / 2.0,
                s.get(1).copied().unwrap_or(0.0) / 2.0,
            );
            let (kx, ky) = (rx * 0.5523, ry * 0.5523);
            d = format!(
                "M{cx} {} C{} {} {} {} {} {cy} C{} {} {} {} {cx} {} C{} {} {} {} {} {cy} C{} {} {} {} {cx} {} Z",
                cy - ry,
                cx + kx,
                cy - ry,
                cx + rx,
                cy - ky,
                cx + rx,
                cx + rx,
                cy + ky,
                cx + kx,
                cy + ry,
                cy + ry,
                cx - kx,
                cy + ry,
                cx - rx,
                cy + ky,
                cx - rx,
                cx - rx,
                cy - ky,
                cx - kx,
                cy - ry,
                cy - ry
            );
        }
        "sr" => {
            let p = eval(&item["p"], f);
            let (cx, cy) = (p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0));
            let n = eval(&item["pt"], f).first().copied().unwrap_or(5.0).max(2.0).round() as usize;
            let or = eval(&item["or"], f).first().copied().unwrap_or(0.0);
            let ir = eval(&item["ir"], f).first().copied().unwrap_or(or * 0.5);
            let rot = eval(&item["r"], f).first().copied().unwrap_or(0.0);
            let star = item.get("sy").and_then(Value::as_f64) != Some(2.0);
            let count = if star { n * 2 } else { n };
            for k in 0..count {
                let r = if star && k % 2 == 1 { ir } else { or };
                let a = (rot - 90.0).to_radians() + std::f64::consts::TAU * k as f64 / count as f64;
                d.push_str(&format!(
                    "{}{:.3} {:.3} ",
                    if k == 0 { "M" } else { "L" },
                    cx + r * a.cos(),
                    cy + r * a.sin()
                ));
            }
            d.push('Z');
        }
        _ => {}
    }
    if reverse && !d.is_empty() {
        // Direction only matters for fills of overlapping contours; ultrafast
        // fills nonzero by default, so a reversed contour cuts a hole when
        // it should. Reverse via kurbo.
        if let Ok(p) = kurbo::BezPath::from_svg(&d) {
            d = reverse_path(&p).to_svg();
        }
    }
    d
}

fn reverse_path(p: &kurbo::BezPath) -> kurbo::BezPath {
    use kurbo::ParamCurve;
    let mut out = kurbo::BezPath::new();
    let segs: Vec<kurbo::PathSeg> = p.segments().collect();
    if segs.is_empty() {
        return p.clone();
    }
    out.move_to(segs.last().unwrap().end());
    for s in segs.iter().rev() {
        match s.reverse() {
            kurbo::PathSeg::Line(l) => out.line_to(l.p1),
            kurbo::PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
            kurbo::PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
        }
    }
    out.close_path();
    out
}

/// Frames at which an item's geometry changes shape (empty: static).
fn item_times(item: &Value) -> Vec<f64> {
    let mut t = Vec::new();
    for k in ["ks", "p", "s", "r", "pt", "or", "ir"] {
        if let Some(p) = item.get(k) {
            t.extend(key_times(p));
        }
    }
    t.sort_by(f64::total_cmp);
    t.dedup();
    t
}

// ------------------------------------------------------------ building

/// A 2D transform stacked on paths: params and the local time it reads.
#[derive(Clone)]
struct Tf {
    params: BTreeMap<String, Param>,
    timing: Timing,
}

/// Time mapping of the composition being built into document time:
/// document = offset + comp * stretch, and a window in document time.
#[derive(Clone, Copy)]
struct Clock {
    offset: f64,
    stretch: f64,
    window: (Option<f64>, Option<f64>),
}

struct Builder<'f> {
    /// Fonts, to lay out text that animates letter by letter.
    fonts: &'f mut crate::geom::Fonts,
    fr: f64,
    n: usize,
    nodes: BTreeMap<String, Node>,
    warnings: Vec<String>,
    assets: BTreeMap<String, Value>,
    base: Option<PathBuf>,
    images: PathBuf,
    /// Expressions that could not be turned into ultrafast drivers.
    unsupported: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

fn param(v: Value) -> Param {
    serde_json::from_value(v).unwrap_or(Param::Number(0.0))
}

impl Builder<'_> {
    fn id(&mut self, base: &str) -> String {
        self.n += 1;
        format!("{base}{}", self.n)
    }

    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    fn add(
        &mut self,
        base: &str,
        op: &str,
        params: BTreeMap<String, Param>,
        inputs: &[(&str, String)],
        timing: Option<Timing>,
    ) -> String {
        let id = self.id(base);
        self.nodes.insert(
            id.clone(),
            Node {
                op: op.into(),
                params,
                inputs: inputs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
                timing,
            },
        );
        id
    }

    /// A Lottie property as a ultrafast param in document seconds: static, or
    /// keys with Lottie's eases. `map` converts each value.
    fn prop(&self, prop: &Value, clock: &Clock, map: &dyn Fn(Vec<f64>) -> Vec<f64>) -> Option<Param> {
        let p = self.prop_raw(prop, clock, map)?;
        match prop.get("x").and_then(Value::as_str) {
            Some(x) => Some(self.expression(x, p, map)),
            None => Some(p),
        }
    }

    /// The common After Effects expressions, as ultrafast drivers: loopOut
    /// (cycle, pingpong) repeats the keys, wiggle(f, a) wiggles around them,
    /// time * k (plus value) becomes an expression of t. Others are counted
    /// and the property keeps its own keys.
    fn expression(&self, x: &str, p: Param, map: &dyn Fn(Vec<f64>) -> Vec<f64>) -> Param {
        let code: String = x.chars().filter(|c| !c.is_whitespace()).collect();
        let args = |name: &str| -> Option<Vec<String>> {
            let i = code.find(&format!("{name}("))? + name.len() + 1;
            let j = code[i..].find(')')? + i;
            Some(
                code[i..j]
                    .split(',')
                    .map(|a| a.trim_matches(['\'', '"']).to_string())
                    .collect(),
            )
        };
        if let Some(a) = args("loopOut") {
            if let Param::Animated { keys, .. } = p {
                let repeat = match a.first().map(String::as_str) {
                    Some("pingpong") => Some(ultrafast_graph::driver::Repeat::Pingpong),
                    Some("cycle") | Some("") | None => Some(ultrafast_graph::driver::Repeat::Cycle),
                    _ => {
                        self.unsupported.borrow_mut().insert(x.to_string());
                        None
                    }
                };
                return Param::Animated { keys, repeat };
            }
            return p;
        }
        if let Some(a) = args("wiggle") {
            let n = |i: usize| a.get(i).and_then(|v| v.parse::<f64>().ok());
            if let (Some(freq), Some(amp)) = (n(0), n(1)) {
                let dims = match &p {
                    Param::Vector(v) => v.len(),
                    Param::Animated { keys, .. } => keys.first().map(|k| k.v.len()).unwrap_or(1),
                    _ => 1,
                };
                let amplitude = map(vec![amp; dims.max(1)]);
                return param(json!({"wiggle": {"frequency": freq, "amplitude": amplitude,
                                               "base": serde_json::to_value(&p).unwrap_or(Value::Null), "octaves": 1}}));
            }
        }
        // The whole expression, without Bodymovin's `$bm_rt =` wrapper.
        let body = code
            .trim_start_matches("var$bm_rt;")
            .trim_start_matches("$bm_rt=")
            .trim_end_matches(';')
            .to_string();
        // clamp(value, a, b): the value itself, as keyed.
        if body.starts_with("clamp(value,") {
            return p;
        }
        // time * k, alone or added to the value; nothing else.
        let rate = |e: &str| -> Option<f64> {
            for pre in ["$bm_mul(time,", "mul(time,"] {
                if let Some(rest) = e.strip_prefix(pre) {
                    return rest.strip_suffix(')')?.parse().ok();
                }
            }
            if let Some(rest) = e.strip_prefix("time*") {
                return rest.parse().ok();
            }
            e.strip_suffix("*time")?.parse().ok()
        };
        let (k, added) = match rate(&body) {
            Some(k) => (Some(k), false),
            None => {
                let inner = body
                    .strip_prefix("$bm_sum(value,")
                    .and_then(|r| r.strip_suffix(')'))
                    .or_else(|| body.strip_prefix("value+"));
                (inner.and_then(rate), true)
            }
        };
        if let Some(k) = k {
            let base = match &p {
                Param::Number(v) => vec![*v],
                Param::Vector(v) => v.clone(),
                _ => vec![0.0],
            };
            let rate = map(vec![k; base.len()]);
            let exprs: Vec<String> = base
                .iter()
                .zip(rate.iter().chain(std::iter::repeat(&0.0)))
                .map(|(b, r)| {
                    if added {
                        format!("{b} + {r} * t")
                    } else {
                        format!("{r} * t")
                    }
                })
                .collect();
            return if exprs.len() == 1 {
                param(json!({"expr": exprs[0]}))
            } else {
                param(json!({"expr": exprs}))
            };
        }
        self.unsupported.borrow_mut().insert(x.to_string());
        p
    }

    fn prop_raw(&self, prop: &Value, _clock: &Clock, map: &dyn Fn(Vec<f64>) -> Vec<f64>) -> Option<Param> {
        let as_param = |v: Vec<f64>| match v.len() {
            1 => Param::Number(v[0]),
            _ => Param::Vector(v),
        };
        let Some(keys) = keyframes(prop) else {
            let v = map(prop.get("k").map(nums)?);
            return (!v.is_empty()).then(|| as_param(v));
        };
        // Spatial tangents (`to`/`ti`): the value travels a curve, at its
        // eased share of the curve's length. Sampled into linear keys.
        let curved = keys.iter().any(|k| {
            ["to", "ti"]
                .iter()
                .any(|n| k.get(*n).is_some_and(|v| nums(v).iter().any(|x| x.abs() > 1e-9)))
        });
        if curved {
            let mut out = Vec::new();
            for i in 0..keys.len() {
                let t0 = keys[i]["t"].as_f64().unwrap_or(0.0);
                let Some(a) = key_value(keys, i).map(nums).filter(|v| !v.is_empty()) else {
                    continue;
                };
                let next = (i + 1 < keys.len()).then(|| {
                    let b = keys[i]
                        .get("e")
                        .map(nums)
                        .filter(|v| !v.is_empty())
                        .or_else(|| key_value(keys, i + 1).map(nums));
                    (keys[i + 1]["t"].as_f64().unwrap_or(t0), b.unwrap_or_else(|| a.clone()))
                });
                let Some((t1, b)) = next.filter(|(t1, _)| *t1 > t0) else {
                    out.push(json!({"t": t0 / self.fr, "v": map(a)}));
                    continue;
                };
                let to = keys[i].get("to").map(nums).unwrap_or_default();
                let ti = keys[i].get("ti").map(nums).unwrap_or_default();
                let at = |v: &Vec<f64>, k: usize| v.get(k).copied().unwrap_or(0.0);
                let dims = a.len().min(b.len());
                let point = |u: f64| -> Vec<f64> {
                    (0..dims)
                        .map(|k| {
                            let (p0, p3) = (at(&a, k), at(&b, k));
                            let (p1, p2) = (p0 + at(&to, k), p3 + at(&ti, k));
                            let m = 1.0 - u;
                            m * m * m * p0 + 3.0 * m * m * u * p1 + 3.0 * m * u * u * p2 + u * u * u * p3
                        })
                        .collect()
                };
                // Arc-length table of the curve.
                let n = 48;
                let mut len = vec![0.0];
                let mut prev = point(0.0);
                for j in 1..=n {
                    let q = point(j as f64 / n as f64);
                    let d = prev.iter().zip(&q).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt();
                    len.push(len[j - 1] + d);
                    prev = q;
                }
                let total = len[n].max(1e-9);
                let along = |share: f64| -> Vec<f64> {
                    let target = share.clamp(0.0, 1.0) * total;
                    let j = len.iter().position(|&l| l >= target).unwrap_or(n).max(1);
                    let f = (target - len[j - 1]) / (len[j] - len[j - 1]).max(1e-9);
                    point((j as f64 - 1.0 + f) / n as f64)
                };
                let hold = matches!(ease_of(&keys[i]), Ease::Hold);
                let steps = if hold { 1 } else { 10 };
                for j in 0..steps {
                    let u = j as f64 / steps as f64;
                    let share = match ease_of(&keys[i]) {
                        Ease::Hold => 0.0,
                        Ease::Bezier(e) => bezier(e, u),
                    };
                    let mut k = json!({"t": (t0 + (t1 - t0) * u) / self.fr, "v": map(along(share))});
                    if hold {
                        k["ease"] = json!("hold");
                    }
                    out.push(k);
                }
            }
            return match out.len() {
                0 => None,
                1 => serde_json::from_value::<Vec<f64>>(out[0]["v"].clone())
                    .ok()
                    .map(as_param),
                _ => Some(param(json!({"keys": out}))),
            };
        }
        let mut out = Vec::new();
        let mut last: Vec<f64> = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let t = key["t"].as_f64().unwrap_or(0.0);
            let v = key_value(keys, i).map(nums).filter(|v| !v.is_empty());
            let v = match v {
                Some(v) => map(v),
                None => last.clone(),
            };
            if v.is_empty() {
                continue;
            }
            last = v.clone();
            // Key times are the node's local seconds (its timing carries the
            // layer's start and stretch).
            let mut k = json!({"t": t / self.fr, "v": v});
            match ease_of(key) {
                Ease::Hold => k["ease"] = json!("hold"),
                Ease::Bezier(e) => {
                    if e != [0.0, 0.0, 1.0, 1.0] {
                        k["ease"] = json!({"bezier": e});
                    }
                }
            }
            out.push(k);
        }
        // Spatial curves (to/ti) are followed as straight lines between keys.
        match out.len() {
            0 => None,
            1 => Some(as_param(last)),
            _ => Some(param(json!({"keys": out}))),
        }
    }

    /// The ultrafast transform of a Lottie transform (`ks` or a group `tr`).
    fn transform(&mut self, ks: &Value, clock: &Clock) -> Tf {
        let mut p = BTreeMap::new();
        let id = |v: Vec<f64>| v;
        let xy = |mut v: Vec<f64>| {
            v.truncate(2);
            v.resize(2, 0.0);
            v
        };
        p.insert(
            "anchor".into(),
            self.prop(&ks["a"], clock, &xy).unwrap_or(Param::Vector(vec![0.0, 0.0])),
        );
        let pos = if ks["p"].get("s").and_then(Value::as_bool) == Some(true) {
            // Separated dimensions: keep x and y keys apart only when static.
            let (x, y) = (&ks["p"]["x"], &ks["p"]["y"]);
            if keyframes(x).is_some() || keyframes(y).is_some() {
                self.warn("separated x/y position keys are merged".into());
                let mut times: Vec<f64> = key_times(x).into_iter().chain(key_times(y)).collect();
                times.sort_by(f64::total_cmp);
                times.dedup();
                let keys: Vec<Value> = times
                    .iter()
                    .map(|&t| {
                        json!({"t": t / self.fr,
                               "v": [eval(x, t).first().copied().unwrap_or(0.0), eval(y, t).first().copied().unwrap_or(0.0)]})
                    })
                    .collect();
                Some(param(json!({"keys": keys})))
            } else {
                Some(Param::Vector(vec![first(x.get("k")), first(y.get("k"))]))
            }
        } else {
            self.prop(&ks["p"], clock, &xy)
        };
        p.insert("position".into(), pos.unwrap_or(Param::Vector(vec![0.0, 0.0])));
        let scale = |v: Vec<f64>| {
            let mut v: Vec<f64> = v.into_iter().take(2).map(|x| x / 100.0).collect();
            if v.len() == 1 {
                v.push(v[0]);
            }
            v
        };
        p.insert(
            "scale".into(),
            self.prop(&ks["s"], clock, &scale)
                .unwrap_or(Param::Vector(vec![1.0, 1.0])),
        );
        let rot = if ks.get("r").is_some() { &ks["r"] } else { &ks["rz"] };
        p.insert(
            "rotation".into(),
            self.prop(rot, clock, &id).unwrap_or(Param::Number(0.0)),
        );
        if ks
            .get("sk")
            .is_some_and(|s| first(s.get("k")) != 0.0 || keyframes(s).is_some())
        {
            self.warn("skew is not imported".into());
        }
        Tf {
            params: p,
            timing: clock.timing(false),
        }
    }

    /// Opacity of a transform (0..1), when not always 1.
    fn opacity(&self, ks: &Value, clock: &Clock) -> Option<Param> {
        let o = ks.get("o")?;
        if keyframes(o).is_none() && (first(o.get("k")) - 100.0).abs() < 1e-9 {
            return None;
        }
        self.prop(o, clock, &|v| v.into_iter().take(1).map(|x| x / 100.0).collect())
    }

    /// Stacks transforms (innermost first) and an opacity on `node`.
    fn chain(&mut self, mut node: String, tfs: &[Tf], opacity: &[(Param, Timing)]) -> String {
        for tf in tfs {
            node = self.add(
                "tf",
                "transform",
                tf.params.clone(),
                &[("source", node)],
                Some(tf.timing),
            );
        }
        for (o, timing) in opacity {
            node = self.add(
                "fade",
                "opacity",
                [("opacity".to_string(), o.clone())].into(),
                &[("source", node)],
                Some(*timing),
            );
        }
        node
    }

    /// Path nodes for geometry `items` (one path, or a chain morphing
    /// through the frames where it changes), with `params`.
    fn geometry(
        &mut self,
        items: &[Value],
        params: &BTreeMap<String, Param>,
        _clock: &Clock,
        timing: Timing,
    ) -> Vec<String> {
        let d_at = |f: f64| -> String {
            let mut d = String::new();
            for it in items {
                d.push_str(&item_d(it, f));
                d.push(' ');
            }
            d.trim().to_string()
        };
        let mut times: Vec<f64> = items.iter().flat_map(item_times).collect();
        times.sort_by(f64::total_cmp);
        times.dedup();
        if times.len() < 2 {
            let mut p = params.clone();
            p.insert("d".into(), Param::Text(d_at(times.first().copied().unwrap_or(0.0))));
            return vec![self.add("path", "path", p, &[], Some(timing))];
        }
        // Sample between keys so eases and vertex-wise interpolation hold,
        // and morph one path through the samples.
        let mut samples = Vec::new();
        for w in times.windows(2) {
            let steps = 4;
            for s in 0..steps {
                samples.push(w[0] + (w[1] - w[0]) * s as f64 / steps as f64);
            }
        }
        samples.push(*times.last().unwrap());
        let shapes: Vec<String> = samples.iter().map(|&f| d_at(f)).collect();
        let keys: Vec<Value> = samples
            .iter()
            .enumerate()
            .map(|(j, &f)| json!({"t": f / self.fr, "v": [j as f64]}))
            .collect();
        let mut p = params.clone();
        p.insert("d".into(), Param::Text(shapes[0].clone()));
        p.insert("to".into(), Param::Text(shapes[1..].join(" | ")));
        p.insert("morph".into(), param(json!({"keys": keys})));
        vec![self.add("path", "path", p, &[], Some(timing))]
    }

    fn color(&self, prop: &Value, opacity: Option<&Value>, clock: &Clock) -> Param {
        // Colors are 0..1 (older files: 0..255), opacity 0..100.
        let base = |v: Vec<f64>| -> Vec<f64> {
            let k = if v.iter().take(3).any(|x| *x > 1.0) { 255.0 } else { 1.0 };
            let mut c: Vec<f64> = v.iter().take(3).map(|x| (x / k).clamp(0.0, 1.0)).collect();
            c.resize(3, 0.0);
            c
        };
        let o = opacity
            .map(|o| eval(o, 0.0).first().copied().unwrap_or(100.0) / 100.0)
            .unwrap_or(1.0);
        let animated_o = opacity.is_some_and(|o| keyframes(o).is_some());
        if animated_o && keyframes(prop).is_none() {
            // Opacity keys carry the (static) color along.
            let c = base(prop.get("k").map(nums).unwrap_or_default());
            return self
                .prop(opacity.unwrap(), clock, &move |v| {
                    let mut c = c.clone();
                    c.push(v.first().copied().unwrap_or(100.0) / 100.0);
                    c
                })
                .unwrap_or(Param::Vector(vec![1.0, 1.0, 1.0, 1.0]));
        }
        self.prop(prop, clock, &move |v| {
            let mut c = base(v);
            c.push(o);
            c
        })
        .unwrap_or(Param::Vector(vec![1.0, 1.0, 1.0, o]))
    }

    /// Paint params of a fill or stroke.
    fn paint(&mut self, p: &Value, clock: &Clock) -> BTreeMap<String, Param> {
        let mut out = BTreeMap::new();
        let ty = p["ty"].as_str().unwrap_or("");
        out.insert("color".into(), self.color(&p["c"], p.get("o"), clock));
        if ty == "st" {
            out.insert(
                "stroke".into(),
                self.prop(&p["w"], clock, &|v| v.into_iter().take(1).collect())
                    .unwrap_or(Param::Number(1.0)),
            );
            let cap = match p.get("lc").and_then(Value::as_f64) {
                Some(2.0) => "round",
                Some(3.0) => "square",
                _ => "butt",
            };
            let join = match p.get("lj").and_then(Value::as_f64) {
                Some(2.0) => "round",
                Some(3.0) => "bevel",
                _ => "miter",
            };
            out.insert("cap".into(), Param::Text(cap.into()));
            out.insert("join".into(), Param::Text(join.into()));
            if let Some(d) = p.get("d").and_then(Value::as_array) {
                let mut dash = Vec::new();
                let mut off = 0.0;
                for e in d {
                    let v = first(e["v"].get("k"));
                    match e["n"].as_str() {
                        Some("o") => off = v,
                        _ => dash.push(v),
                    }
                }
                if !dash.is_empty() {
                    out.insert("dash".into(), Param::Vector(dash));
                    out.insert("dash_offset".into(), Param::Number(off));
                }
            }
        } else if p.get("r").and_then(Value::as_f64) == Some(2.0) {
            out.insert("fill_rule".into(), Param::Text("evenodd".into()));
        }
        out
    }

    /// A gradient fill: a gradient through the group's transforms, shown
    /// through the geometry.
    fn gradient(&mut self, g: &Value, clock: &Clock) -> BTreeMap<String, Param> {
        let n = g["g"]["p"].as_u64().unwrap_or(2) as usize;
        let raw = eval(&g["g"]["k"], 0.0);
        if keyframes(&g["g"]["k"]).is_some() {
            self.warn("animated gradient colors use their first key".into());
        }
        let mut stops = Vec::new();
        for k in 0..n.min(8) {
            let at = |j: usize| raw.get(k * 4 + j).copied().unwrap_or(0.0);
            // Alpha stops follow the color stops: pairs of (offset, alpha).
            let a = raw.get(n * 4 + k * 2 + 1).copied().unwrap_or(1.0);
            let o = g.get("o").map(|o| first(o.get("k")) / 100.0).unwrap_or(1.0);
            stops.extend([at(0), at(1), at(2), at(3), a * o]);
        }
        let mut p = BTreeMap::new();
        p.insert("stops".into(), Param::Vector(stops));
        p.insert("space".into(), Param::Text("srgb".into()));
        let xy = |mut v: Vec<f64>| {
            v.truncate(2);
            v
        };
        let s = self.prop(&g["s"], clock, &xy).unwrap_or(Param::Vector(vec![0.0, 0.0]));
        let e = self
            .prop(&g["e"], clock, &xy)
            .unwrap_or(Param::Vector(vec![100.0, 0.0]));
        if g.get("t").and_then(Value::as_f64) == Some(2.0) {
            let (s0, e0) = (eval(&g["s"], 0.0), eval(&g["e"], 0.0));
            let r = ((e0.first().copied().unwrap_or(0.0) - s0.first().copied().unwrap_or(0.0)).powi(2)
                + (e0.get(1).copied().unwrap_or(0.0) - s0.get(1).copied().unwrap_or(0.0)).powi(2))
            .sqrt();
            p.insert("kind".into(), Param::Text("radial".into()));
            p.insert("center".into(), s);
            p.insert("radius".into(), Param::Number(r.max(1.0)));
        } else {
            p.insert("kind".into(), Param::Text("linear".into()));
            p.insert("start".into(), s);
            p.insert("end".into(), e);
        }
        p
    }

    /// Layers of a shape group, back to front. `tfs` are the transforms
    /// outside this group (innermost first); `opacity` likewise.
    #[allow(clippy::too_many_arguments)]
    fn shapes(
        &mut self,
        items: &[Value],
        tfs: &[Tf],
        opacity: &[(Param, Timing)],
        trim: Option<&(Param, Param)>,
        clock: &Clock,
        timing: Timing,
        out: &mut Vec<String>,
    ) {
        // This group's own transform and opacity.
        let mut tfs_here = tfs.to_vec();
        let mut op_here = opacity.to_vec();
        if let Some(tr) = items.iter().find(|i| i["ty"] == "tr") {
            let t = self.transform(tr, clock);
            tfs_here.insert(0, t);
            if let Some(o) = self.opacity(tr, clock) {
                op_here.insert(0, (o, timing));
            }
        }
        // A trim applies to every path above it in this group.
        let trim_here = items.iter().rev().find(|i| i["ty"] == "tm").map(|tm| {
            let pct = |v: Vec<f64>| v.into_iter().take(1).map(|x| x / 100.0).collect();
            (
                self.prop(&tm["s"], clock, &pct).unwrap_or(Param::Number(0.0)),
                self.prop(&tm["e"], clock, &pct).unwrap_or(Param::Number(1.0)),
            )
        });
        let trim = trim_here.as_ref().or(trim);
        // Rounded corners round the rectangles above them.
        let mut items = items.to_vec();
        if let Some(rd) = items.iter().position(|i| i["ty"] == "rd") {
            let r = items[rd]["r"].clone();
            for it in items[..rd].iter_mut() {
                if it["ty"] == "rc" && first(it["r"].get("k")) == 0.0 && keyframes(&it["r"]).is_none() {
                    it["r"] = r.clone();
                } else if it["ty"] == "sh" {
                    self.warn("rounded corners on paths are not imported".into());
                }
            }
        }
        // A repeater draws everything above it again and again, each copy
        // through its transform once more, fading from start to end opacity.
        if let Some(rp) = items.iter().position(|i| i["ty"] == "rp") {
            let r = items[rp].clone();
            let base: Vec<Value> = items[..rp].to_vec();
            if keyframes(&r["c"]).is_some() {
                self.warn("animated repeater counts use their first value".into());
            }
            let count = first(r["c"].get("k")).round().max(0.0) as usize;
            let offset = first(r["o"].get("k")).round() as i64;
            let step = self.transform(&r["tr"], clock);
            let (so, eo) = (
                r["tr"].get("so").map(|v| first(v.get("k"))).unwrap_or(100.0) / 100.0,
                r["tr"].get("eo").map(|v| first(v.get("k"))).unwrap_or(100.0) / 100.0,
            );
            let mut order: Vec<usize> = (0..count).collect();
            // Composite "below": each new copy goes under the last.
            if first(r.get("m")) == 2.0 {
                order.reverse();
            }
            for i in order {
                let k = i as i64 + offset;
                let mut tfs_i = Vec::new();
                for _ in 0..k.max(0) {
                    tfs_i.push(step.clone());
                }
                tfs_i.extend(tfs_here.iter().cloned());
                let mut op_i = op_here.clone();
                let o = if count > 1 {
                    so + (eo - so) * i as f64 / (count - 1) as f64
                } else {
                    so
                };
                if (o - 1.0).abs() > 1e-9 {
                    op_i.insert(0, (Param::Number(o), timing));
                }
                self.shapes_body(&base, &tfs_i, &op_i, trim, clock, timing, out);
            }
            return;
        }
        self.shapes_body(&items, &tfs_here, &op_here, trim, clock, timing, out);
    }

    /// The items of a group, back to front, under its transforms.
    #[allow(clippy::too_many_arguments)]
    fn shapes_body(
        &mut self,
        items: &[Value],
        tfs_here: &[Tf],
        op_here: &[(Param, Timing)],
        trim: Option<&(Param, Param)>,
        clock: &Clock,
        timing: Timing,
        out: &mut Vec<String>,
    ) {
        let (tfs_here, op_here) = (tfs_here.to_vec(), op_here.to_vec());
        for it in items {
            match it["ty"].as_str().unwrap_or("") {
                "mm" => self.warn("merge paths are not imported".into()),
                "gs" => self.warn("gradient strokes use a solid stroke".into()),
                _ => {}
            }
        }
        // Back to front: the last item is furthest back. A paint applies to
        // the geometry above it, nested groups included (each through its
        // own transforms).
        for (k, it) in items.iter().enumerate().rev() {
            let ty = it["ty"].as_str().unwrap_or("");
            if it.get("hd").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            match ty {
                "fl" | "st" | "gf" | "gs" => {
                    let mut paint = if ty == "gf" {
                        BTreeMap::new()
                    } else {
                        let mut st = it.clone();
                        if ty == "gs" {
                            // A solid stroke in the first stop's color.
                            let g = eval(&it["g"]["k"], 0.0);
                            st["c"] = json!({"k": [g.get(1).copied().unwrap_or(1.0), g.get(2).copied().unwrap_or(1.0), g.get(3).copied().unwrap_or(1.0), 1.0]});
                            st["ty"] = json!("st");
                        }
                        self.paint(&st, clock)
                    };
                    if let Some(w) = paint.remove("stroke") {
                        match stroke_width(w) {
                            Some(w) => paint.insert("stroke".into(), w),
                            None => continue,
                        };
                    }
                    if let Some((s, e)) = trim {
                        paint.insert("trim_start".into(), s.clone());
                        paint.insert("trim_end".into(), e.clone());
                    }
                    let grad = (ty == "gf").then(|| self.gradient(it, clock));
                    self.paint_geometry(
                        &items[..k],
                        &paint,
                        grad.as_ref(),
                        &tfs_here,
                        &op_here,
                        clock,
                        timing,
                        out,
                    );
                }
                "gr" => {
                    let empty = Vec::new();
                    let sub = it["it"].as_array().unwrap_or(&empty).clone();
                    self.shapes(&sub, &tfs_here, &op_here, trim, clock, timing, out);
                }
                _ => {}
            }
        }
    }

    /// Paints the geometry among `items` (and inside groups among them)
    /// with one paint, each group through its own transforms.
    #[allow(clippy::too_many_arguments)]
    fn paint_geometry(
        &mut self,
        items: &[Value],
        paint: &BTreeMap<String, Param>,
        grad: Option<&BTreeMap<String, Param>>,
        tfs: &[Tf],
        opacity: &[(Param, Timing)],
        clock: &Clock,
        timing: Timing,
        out: &mut Vec<String>,
    ) {
        // Nested groups first (they are below this level's own paths only
        // when they come later; approximated as behind).
        for it in items.iter().rev() {
            if it["ty"] == "gr" && it.get("hd").and_then(Value::as_bool) != Some(true) {
                let sub = it["it"].as_array().cloned().unwrap_or_default();
                let mut tfs_g = tfs.to_vec();
                let mut op_g = opacity.to_vec();
                if let Some(tr) = sub.iter().find(|i| i["ty"] == "tr") {
                    let t = self.transform(tr, clock);
                    tfs_g.insert(0, t);
                    if let Some(o) = self.opacity(tr, clock) {
                        op_g.insert(0, (o, timing));
                    }
                }
                self.paint_geometry(&sub, paint, grad, &tfs_g, &op_g, clock, timing, out);
            }
        }
        let geo: Vec<Value> = items
            .iter()
            .filter(|i| {
                matches!(i["ty"].as_str(), Some("sh" | "rc" | "el" | "sr"))
                    && i.get("hd").and_then(Value::as_bool) != Some(true)
            })
            .cloned()
            .collect();
        if geo.is_empty() {
            return;
        }
        for path in self.geometry(&geo, paint, clock, timing) {
            let node = match grad {
                None => self.chain(path, tfs, opacity),
                Some(g) => {
                    // The gradient lives in the same space as the geometry.
                    let shape = self.chain(path, tfs, &[]);
                    let gr = self.add("gradient", "gradient", g.clone(), &[], Some(timing));
                    let gr = self.chain(gr, tfs, &[]);
                    let m = self.add(
                        "gradient_fill",
                        "mask",
                        BTreeMap::new(),
                        &[("source", gr), ("matte", shape)],
                        Some(timing),
                    );
                    self.chain(m, &[], opacity)
                }
            };
            out.push(node);
        }
    }

    /// `node` shown only inside the rectangle (0, 0, w, h) of the space the
    /// transforms `tfs` place.
    fn clip(&mut self, node: String, w: f64, h: f64, tfs: &[Tf], timing: Timing) -> String {
        let p: BTreeMap<String, Param> = [
            ("d".to_string(), Param::Text(format!("M0 0 H{w} V{h} H0 Z"))),
            ("color".to_string(), Param::Text("#ffffff".into())),
        ]
        .into();
        let rect = self.add("bounds", "path", p, &[], Some(timing));
        let matte = self.chain(rect, tfs, &[]);
        self.add(
            "clipped",
            "mask",
            BTreeMap::new(),
            &[("source", node), ("matte", matte)],
            Some(timing),
        )
    }

    /// Composites `layers` (back to front) with `over` in sRGB, as Lottie
    /// players blend.
    fn stack(&mut self, layers: &[(String, Option<Param>, Timing)]) -> Option<String> {
        let mut acc: Option<String> = None;
        for (l, opacity, timing) in layers {
            acc = Some(match acc {
                None => match opacity {
                    Some(o) => self.add(
                        "fade",
                        "opacity",
                        [("opacity".to_string(), o.clone())].into(),
                        &[("source", l.clone())],
                        Some(*timing),
                    ),
                    None => l.clone(),
                },
                Some(bg) => {
                    let mut p: BTreeMap<String, Param> = [("space".to_string(), Param::Text("srgb".into()))].into();
                    if let Some(o) = opacity {
                        p.insert("opacity".into(), o.clone());
                    }
                    self.add("over", "over", p, &[("fg", l.clone()), ("bg", bg)], Some(*timing))
                }
            });
        }
        acc
    }

    /// The transforms of layer `ind` and its parents (innermost first).
    fn layer_tfs(&mut self, layers: &[Value], idx: usize, clock: &Clock, outer: &[Tf]) -> Vec<Tf> {
        let mut out = Vec::new();
        let mut cur = Some(idx);
        let mut guard = 0;
        while let Some(i) = cur {
            let l = &layers[i];
            let lc = layer_clock(l, clock, self.fr);
            let tf = self.transform(&l["ks"], &lc);
            out.push(tf);
            cur = l.get("parent").and_then(Value::as_f64).and_then(|p| {
                layers
                    .iter()
                    .position(|x| x.get("ind").and_then(Value::as_f64) == Some(p))
            });
            guard += 1;
            if guard > 32 {
                break;
            }
        }
        out.extend(outer.iter().cloned());
        out
    }

    /// The image of one layer (None for nulls and empty layers).
    fn layer(&mut self, layers: &[Value], idx: usize, clock: &Clock, outer: &[Tf], depth: usize) -> Option<String> {
        let l = &layers[idx];
        if l.get("hd").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let lc = layer_clock(l, clock, self.fr);
        let timing = lc.timing(true);
        if l.get("ddd").and_then(Value::as_f64) == Some(1.0) {
            self.warn("3D layers are flattened".into());
        }
        if l.get("ef").is_some_and(|e| e.as_array().is_some_and(|a| !a.is_empty())) {
            self.warn("layer effects are not imported".into());
        }
        let tfs = self.layer_tfs(layers, idx, clock, outer);
        let mut parts: Vec<String> = Vec::new();
        match l["ty"].as_u64().unwrap_or(99) {
            // Shape layer.
            4 => {
                let items = l["shapes"].as_array().cloned().unwrap_or_default();
                self.shapes(&items, &tfs, &[], None, &lc, timing, &mut parts);
            }
            // Solid.
            1 => {
                let (w, h) = (first(l.get("sw")), first(l.get("sh")));
                let c = l["sc"].as_str().unwrap_or("#000000");
                let p: BTreeMap<String, Param> = [
                    ("d".to_string(), Param::Text(format!("M0 0 H{w} V{h} H0 Z"))),
                    ("color".to_string(), Param::Text(c.to_string())),
                ]
                .into();
                let path = self.add("solid", "path", p, &[], Some(timing));
                parts.push(self.chain(path, &tfs, &[]));
            }
            // Image.
            2 => {
                let asset = l["refId"].as_str().and_then(|r| self.assets.get(r)).cloned();
                if let Some(a) = asset
                    && let Some(src) = self.image_src(&a)
                {
                    let (w, h) = (first(a.get("w")), first(a.get("h")));
                    let p: BTreeMap<String, Param> = [
                        ("src".to_string(), Param::Text(src)),
                        ("rect".to_string(), Param::Vector(vec![0.0, 0.0, w, h])),
                    ]
                    .into();
                    let img = self.add("image", "image", p, &[], Some(timing));
                    parts.push(self.chain(img, &tfs, &[]));
                }
            }
            // Text.
            5 => {
                for t in self.text_layer(l, timing) {
                    parts.push(self.chain(t, &tfs, &[]));
                }
            }
            // Precomp: its layers, inlined under this layer's transforms.
            0 => {
                if depth > 8 {
                    self.warn("precomps nested too deep".into());
                    return None;
                }
                let refid = l["refId"].as_str().unwrap_or("");
                let Some(asset) = self.assets.get(refid).cloned() else {
                    self.warn(format!("missing precomp `{refid}`"));
                    return None;
                };
                if l.get("tm").is_some() {
                    self.warn("time remapping is not imported".into());
                }
                let sub = asset["layers"].as_array().cloned().unwrap_or_default();
                let inner = precomp_clock(l, clock, self.fr);
                if let Some(c) = self.comp(&sub, &inner, &tfs, depth + 1) {
                    // A precomp shows only inside its own size.
                    let (w, h) = (first(l.get("w")), first(l.get("h")));
                    parts.push(if w > 0.0 && h > 0.0 {
                        self.clip(c, w, h, &tfs, timing)
                    } else {
                        c
                    });
                }
            }
            _ => return None,
        }
        let mut node = self.stack(&parts.into_iter().map(|p| (p, None, timing)).collect::<Vec<_>>())?;
        // Masks: the union of the mask shapes, through the layer's transforms.
        if let Some(masks) = l
            .get("masksProperties")
            .and_then(Value::as_array)
            .filter(|m| !m.is_empty())
        {
            let mut mattes = Vec::new();
            let mut inverted = false;
            for (k, m) in masks.iter().enumerate() {
                let mode = m["mode"].as_str().unwrap_or("a");
                if mode == "n" {
                    continue;
                }
                if k == 0 {
                    inverted = mode == "s" || m.get("inv").and_then(Value::as_bool) == Some(true);
                } else if mode != "a" {
                    self.warn("mask modes other than add combine as add".into());
                }
                // Mask outlines are always closed.
                let item = json!({"ty": "sh", "ks": m["pt"], "closed": true});
                let mut p = BTreeMap::new();
                p.insert("color".to_string(), Param::Text("#ffffff".into()));
                for path in self.geometry(&[item], &p, &lc, timing) {
                    mattes.push((self.chain(path, &tfs, &[]), None, timing));
                }
            }
            if let Some(matte) = self.stack(&mattes) {
                let mode = if inverted { "alpha_inverted" } else { "alpha" };
                node = self.add(
                    "masked",
                    "mask",
                    [("mode".to_string(), Param::Text(mode.into()))].into(),
                    &[("source", node), ("matte", matte)],
                    Some(timing),
                );
            }
        }
        Some(node)
    }

    /// A text layer: one `text` node, or one per letter when animators
    /// move, scale, turn, fade or recolor letters.
    fn text_layer(&mut self, l: &Value, timing: Timing) -> Vec<String> {
        let Some(doc) = l["t"]["d"]["k"]
            .as_array()
            .and_then(|k| k.first())
            .map(|k| k["s"].clone())
        else {
            return Vec::new();
        };
        if l["t"]["d"]["k"].as_array().is_some_and(|k| k.len() > 1) {
            self.warn("animated text documents use their first state".into());
        }
        if doc.get("sz").is_some() {
            self.warn("paragraph text boxes are set as point text".into());
        }
        let size = first(doc.get("s")).max(1.0);
        let c = nums(&doc["fc"]);
        let k = if c.iter().any(|x| *x > 1.0) { 255.0 } else { 1.0 };
        let color: Vec<f64> = c.iter().take(3).map(|x| x / k).chain([1.0]).collect();
        let j = first(doc.get("j")) as i64;
        let align = match j {
            1 => "right",
            2 => "center",
            _ => "left",
        };
        let text = doc["t"].as_str().unwrap_or("").replace(['\r', '\u{3}'], "\n");
        let (family, weight, italic) = self.font_of(doc["f"].as_str().unwrap_or(""));
        let tracking = first(doc.get("tr")) * size / 1000.0;
        let lh = Some(first(doc.get("lh"))).filter(|v| *v > 0.0).unwrap_or(size * 1.2);
        let mut p: BTreeMap<String, Param> = BTreeMap::new();
        p.insert("markup".into(), Param::Bool(false));
        p.insert("size".into(), Param::Number(size));
        p.insert("font".into(), Param::Text(family.clone()));
        p.insert("weight".into(), Param::Number(weight));
        if italic {
            p.insert("style".into(), Param::Text("italic".into()));
        }
        if tracking != 0.0 {
            p.insert("tracking".into(), Param::Number(tracking));
        }
        // Lottie players set letters one by one: no kerning or ligatures.
        p.insert("features".into(), Param::Text("liga=0,calt=0,kern=0".into()));
        let animators = l["t"]["a"].as_array().cloned().unwrap_or_default();
        if animators.is_empty() {
            p.insert("text".into(), Param::Text(text));
            p.insert("color".into(), Param::Vector(color));
            p.insert("align".into(), Param::Text(align.into()));
            p.insert("valign".into(), Param::Text("baseline".into()));
            p.insert("position".into(), Param::Vector(vec![0.0, 0.0]));
            p.insert("line_height".into(), Param::Number(lh / size));
            return vec![self.add("text", "text", p, &[], Some(timing))];
        }
        self.letters(&text, &p, &animators, &color, j, lh, timing)
    }

    /// Letters of animated text, each its own node with sampled keys.
    #[allow(clippy::too_many_arguments)]
    fn letters(
        &mut self,
        text: &str,
        style: &BTreeMap<String, Param>,
        animators: &[Value],
        color: &[f64],
        justify: i64,
        lh: f64,
        timing: Timing,
    ) -> Vec<String> {
        // Lay each line out (no ligatures, so letters are glyphs).
        struct Letter {
            ch: String,
            center: [f64; 2],
            adv: f64,
            line: usize,
            space: bool,
            word: usize,
        }
        let mut letters: Vec<Letter> = Vec::new();
        let base = self.base.clone().unwrap_or_else(|| PathBuf::from("."));
        let mut word = 0;
        for (li, line) in text.split('\n').enumerate() {
            let mut node = Node {
                op: "text".into(),
                params: style.clone(),
                ..Default::default()
            };
            node.params.insert("text".into(), Param::Text(line.to_string()));
            node.params
                .insert("features".into(), Param::Text("liga=0,calt=0,kern=0".into()));
            let Ok(m) = crate::geom::measure("lottie", &node, 0.0, self.fonts, &base) else {
                self.warn("text could not be laid out".into());
                return Vec::new();
            };
            let shift = match justify {
                1 => -m.advance,
                2 => -m.advance / 2.0,
                _ => 0.0,
            };
            for g in &m.glyphs {
                let space = g.text.trim().is_empty();
                letters.push(Letter {
                    ch: g.text.clone(),
                    center: [shift + g.x + g.advance / 2.0, li as f64 * lh],
                    adv: g.advance,
                    line: li,
                    space,
                    word,
                });
                if space {
                    word += 1;
                }
            }
            word += 1;
        }
        // Each animator's index of every letter and its count, by what its
        // selector is based on (1 characters, 2 without spaces, 3 words,
        // 4 lines).
        let indexes: Vec<(Vec<f64>, f64)> = animators
            .iter()
            .map(|a| {
                let based = first(a["s"].get("b")).max(1.0) as i64;
                let mut ind = 0.0;
                let mut out = Vec::new();
                let mut last_word = usize::MAX;
                let mut last_line = usize::MAX;
                for l in &letters {
                    let idx = match based {
                        2 => {
                            let i = ind;
                            if !l.space {
                                ind += 1.0;
                            }
                            i
                        }
                        3 => {
                            if l.word != last_word {
                                if last_word != usize::MAX {
                                    ind += 1.0;
                                }
                                last_word = l.word;
                            }
                            ind
                        }
                        4 => {
                            if l.line != last_line {
                                if last_line != usize::MAX {
                                    ind += 1.0;
                                }
                                last_line = l.line;
                            }
                            ind
                        }
                        _ => {
                            ind += 1.0;
                            ind - 1.0
                        }
                    };
                    out.push(idx);
                }
                let total = if matches!(based, 3 | 4) { ind + 1.0 } else { ind };
                (out, total)
            })
            .collect();
        if animators.iter().any(|a| first(a["s"].get("rn")) != 0.0) {
            self.warn("randomized text selectors play in order".into());
        }
        // Frames where anything animates.
        fn times(v: &Value, out: &mut Vec<f64>) {
            match v {
                Value::Object(m) => {
                    out.extend(key_times(v));
                    m.values().for_each(|x| times(x, out));
                }
                Value::Array(a) => a.iter().for_each(|x| times(x, out)),
                _ => {}
            }
        }
        let mut t = Vec::new();
        animators.iter().for_each(|a| times(a, &mut t));
        t.retain(|x| x.is_finite());
        let frames: Vec<f64> = match (t.iter().cloned().reduce(f64::min), t.iter().cloned().reduce(f64::max)) {
            (Some(a), Some(b)) if b > a => (a.floor() as i64..=b.ceil() as i64).map(|f| f as f64).collect(),
            _ => vec![0.0],
        };
        let prop = |a: &Value, k: &str, f: f64| -> Option<Vec<f64>> { a.get(k).map(|p| eval(p, f)) };
        let mut out = Vec::new();
        for (li, l) in letters.iter().enumerate() {
            if l.space {
                continue;
            }
            // Per frame: offset, anchor, scale, rotation, color with alpha.
            let mut series: Vec<[f64; 11]> = Vec::with_capacity(frames.len());
            for &f in &frames {
                let (mut off, mut anchor, mut scale, mut rot, mut op) = ([0.0; 2], [0.0; 2], [1.0; 2], 0.0, 1.0);
                let mut fc = [color[0], color[1], color[2]];
                let mut track = 0.0;
                for (ai, a) in animators.iter().enumerate() {
                    let (idx, total) = &indexes[ai];
                    // Tracking also pushes along every letter after this one.
                    let props = &a["a"];
                    if let Some(tv) = prop(props, "t", f) {
                        for k in 0..=li {
                            if !letters[k].space || k == li {
                                track += tv.first().copied().unwrap_or(0.0) * selector_mult(&a["s"], f, idx[k], *total);
                            }
                        }
                    }
                    let m = selector_mult(&a["s"], f, idx[li], *total);
                    if m == 0.0 {
                        continue;
                    }
                    if let Some(v) = prop(props, "a", f) {
                        anchor[0] += v.first().copied().unwrap_or(0.0) * m;
                        anchor[1] += v.get(1).copied().unwrap_or(0.0) * m;
                    }
                    if let Some(v) = prop(props, "s", f) {
                        scale[0] *= 1.0 + (v.first().copied().unwrap_or(100.0) / 100.0 - 1.0) * m;
                        scale[1] *= 1.0 + (v.get(1).copied().unwrap_or(100.0) / 100.0 - 1.0) * m;
                    }
                    if let Some(v) = prop(props, "r", f) {
                        rot += v.first().copied().unwrap_or(0.0) * m;
                    }
                    if let Some(v) = prop(props, "o", f) {
                        let o = v.first().copied().unwrap_or(100.0) / 100.0;
                        op += (o * m - op) * m;
                    }
                    if let Some(v) = prop(props, "fc", f) {
                        let k = if v.iter().take(3).any(|x| *x > 1.0) { 255.0 } else { 1.0 };
                        for c in 0..3 {
                            fc[c] += (v.get(c).copied().unwrap_or(0.0) / k - fc[c]) * m;
                        }
                    }
                    if let Some(v) = prop(props, "p", f) {
                        off[0] += v.first().copied().unwrap_or(0.0) * m;
                        off[1] += v.get(1).copied().unwrap_or(0.0) * m;
                    }
                }
                series.push([
                    l.center[0] + off[0] + track,
                    l.center[1] + off[1],
                    l.adv / 2.0 + anchor[0],
                    anchor[1],
                    scale[0],
                    scale[1],
                    rot,
                    fc[0].clamp(0.0, 1.0),
                    fc[1].clamp(0.0, 1.0),
                    fc[2].clamp(0.0, 1.0),
                    op.clamp(0.0, 1.0),
                ]);
            }
            let mut p = style.clone();
            p.insert("text".into(), Param::Text(l.ch.clone()));
            p.insert("valign".into(), Param::Text("baseline".into()));
            p.insert("features".into(), Param::Text("liga=0,calt=0,kern=0".into()));
            for (name, range) in [
                ("position", 0..2),
                ("anchor", 2..4),
                ("scale", 4..6),
                ("rotation", 6..7),
                ("color", 7..11),
            ] {
                p.insert(name.into(), self.sampled(&frames, &series, range));
            }
            out.push(self.add("letter", "text", p, &[], Some(timing)));
        }
        out
    }

    /// Keys from per-frame samples of `range` of each row: constant runs
    /// and straight stretches collapse.
    fn sampled(&self, frames: &[f64], rows: &[[f64; 11]], range: std::ops::Range<usize>) -> Param {
        let v = |i: usize| rows[i][range.clone()].to_vec();
        let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3);
        let as_param = |x: Vec<f64>| {
            if x.len() == 1 {
                Param::Number(x[0])
            } else {
                Param::Vector(x)
            }
        };
        if (1..rows.len()).all(|i| close(&v(i), &v(0))) {
            return as_param(v(0));
        }
        let mut keep = vec![0];
        for i in 1..rows.len() - 1 {
            let (a, b, c) = (v(i - 1), v(i), v(i + 1));
            let mid: Vec<f64> = a.iter().zip(&c).map(|(x, y)| (x + y) / 2.0).collect();
            if !close(&b, &mid) {
                keep.push(i);
            }
        }
        keep.push(rows.len() - 1);
        keep.dedup();
        let keys: Vec<Value> = keep
            .iter()
            .map(|&i| json!({"t": frames[i] / self.fr, "v": v(i)}))
            .collect();
        param(json!({"keys": keys}))
    }

    /// Family, weight and italic of a font named in the fonts list.
    fn font_of(&self, name: &str) -> (String, f64, bool) {
        let entry = self
            .assets
            .get("__fonts")
            .and_then(|f| f.as_array())
            .and_then(|list| list.iter().find(|x| x["fName"].as_str() == Some(name)))
            .cloned();
        let family = entry
            .as_ref()
            .and_then(|x| x["fFamily"].as_str())
            .unwrap_or(name)
            .to_string();
        let style = entry
            .as_ref()
            .and_then(|x| x["fStyle"].as_str())
            .unwrap_or("")
            .to_lowercase()
            .replace([' ', '-'], "");
        let weight = [
            ("thin", 100.0),
            ("extralight", 200.0),
            ("ultralight", 200.0),
            ("light", 300.0),
            ("medium", 500.0),
            ("semibold", 600.0),
            ("demibold", 600.0),
            ("extrabold", 800.0),
            ("ultrabold", 800.0),
            ("heavy", 800.0),
            ("black", 900.0),
            ("bold", 700.0),
        ]
        .iter()
        .find(|(k, _)| style.contains(k))
        .map(|(_, w)| *w)
        .unwrap_or(400.0);
        (family, weight, style.contains("italic") || style.contains("oblique"))
    }

    fn image_src(&mut self, a: &Value) -> Option<String> {
        let p = a["p"].as_str()?;
        if let Some(data) = p.strip_prefix("data:") {
            // Embedded: written once to a cache file named by its hash.
            let (meta, b64) = data.split_once(',')?;
            let ext = if meta.contains("jpeg") || meta.contains("jpg") {
                "jpg"
            } else {
                "png"
            };
            let bytes = base64(b64)?;
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            let path = self.images.join(format!("{:016x}.{ext}", h.finish()));
            if !path.exists() {
                let _ = std::fs::create_dir_all(&self.images);
                std::fs::write(&path, &bytes).ok()?;
            }
            return Some(path.to_string_lossy().into_owned());
        }
        if !crate::svg::external_files_allowed() {
            // Only embedded images (above).
            return None;
        }
        let dir = a["u"].as_str().unwrap_or("");
        let rel = Path::new(dir).join(p);
        Some(match &self.base {
            Some(b) => b.join(rel).to_string_lossy().into_owned(),
            None => rel.to_string_lossy().into_owned(),
        })
    }

    /// A composition's layers, back to front, composited.
    fn comp(&mut self, layers: &[Value], clock: &Clock, outer: &[Tf], depth: usize) -> Option<String> {
        let mut stack: Vec<(String, Option<Param>, Timing)> = Vec::new();
        let mut i = layers.len();
        while i > 0 {
            i -= 1;
            let l = &layers[i];
            // A matte layer is used by the layer below it, not drawn.
            if l.get("td").and_then(Value::as_f64).is_some_and(|v| v != 0.0) {
                continue;
            }
            let lc = layer_clock(l, clock, self.fr);
            let timing = lc.timing(true);
            let Some(mut node) = self.layer(layers, i, clock, outer, depth) else {
                continue;
            };
            let tt = l.get("tt").and_then(Value::as_f64).unwrap_or(0.0);
            if tt > 0.0 && i > 0 {
                // The matte is the layer just above (or the one named by tp).
                let mi = l
                    .get("tp")
                    .and_then(Value::as_f64)
                    .and_then(|p| {
                        layers
                            .iter()
                            .position(|x| x.get("ind").and_then(Value::as_f64) == Some(p))
                    })
                    .unwrap_or(i - 1);
                if let Some(matte) = self.layer(layers, mi, clock, outer, depth) {
                    let mc = layer_clock(&layers[mi], clock, self.fr);
                    let matte = match self.opacity(&layers[mi]["ks"], &mc) {
                        Some(o) => self.add(
                            "fade",
                            "opacity",
                            [("opacity".to_string(), o)].into(),
                            &[("source", matte)],
                            Some(mc.timing(false)),
                        ),
                        None => matte,
                    };
                    let mode = match tt as i64 {
                        2 => "alpha_inverted",
                        3 => "luma",
                        4 => "luma_inverted",
                        _ => "alpha",
                    };
                    node = self.add(
                        "matted",
                        "mask",
                        [("mode".to_string(), Param::Text(mode.into()))].into(),
                        &[("source", node), ("matte", matte)],
                        None,
                    );
                }
            }
            let opacity = self.opacity(&l["ks"], &lc);
            let mode = match first(l.get("bm")) as i64 {
                1 => Some("multiply"),
                2 => Some("screen"),
                3 => Some("overlay"),
                4 => Some("darken"),
                5 => Some("lighten"),
                6 => Some("color_dodge"),
                7 => Some("color_burn"),
                8 => Some("hard_light"),
                9 => Some("soft_light"),
                10 => Some("difference"),
                11 => Some("exclusion"),
                12 => Some("hue"),
                13 => Some("saturation"),
                14 => Some("color"),
                15 => Some("luminosity"),
                16 => Some("add"),
                _ => None,
            };
            let below = if mode.is_some() { self.stack(&stack) } else { None };
            match (mode, below) {
                // A blend mode: this layer blends over everything below it.
                (Some(mode), Some(below)) => {
                    let mut p: BTreeMap<String, Param> = [
                        ("mode".to_string(), Param::Text(mode.into())),
                        ("space".to_string(), Param::Text("srgb".into())),
                    ]
                    .into();
                    if let Some(o) = opacity {
                        p.insert("opacity".into(), o);
                    }
                    let b = self.add("blend", "blend", p, &[("fg", node), ("bg", below)], Some(timing));
                    stack = vec![(b, None, Timing::default())];
                }
                _ => stack.push((node, opacity, timing)),
            }
        }
        self.stack(&stack)
    }
}

impl Clock {
    /// Node timing reading this clock: local seconds of the composition,
    /// optionally inside its window.
    fn timing(&self, window: bool) -> Timing {
        Timing {
            in_point: if window { self.window.0 } else { None },
            out_point: if window { self.window.1 } else { None },
            offset: self.offset,
            speed: 1.0 / self.stretch,
            ..Default::default()
        }
    }
}

/// The clock of a layer's own properties. Lottie players subtract the
/// layer's start time (`st`) from both the frame and every key time, so
/// keys read the containing composition's time; only the window (`ip`,
/// `op`, also composition time) is the layer's own.
fn layer_clock(l: &Value, parent: &Clock, fr: f64) -> Clock {
    let to_doc = |f: f64| parent.offset + f / fr * parent.stretch;
    let (ip, op) = (l.get("ip").and_then(Value::as_f64), l.get("op").and_then(Value::as_f64));
    let mut w = (ip.map(to_doc), op.map(to_doc));
    if let Some(a) = parent.window.0 {
        w.0 = Some(w.0.map_or(a, |x| x.max(a)));
    }
    if let Some(b) = parent.window.1 {
        w.1 = Some(w.1.map_or(b, |x| x.min(b)));
    }
    Clock {
        offset: parent.offset,
        stretch: parent.stretch,
        window: w,
    }
}

/// The clock inside a precomp layer: its composition starts at the
/// layer's `st` and runs `sr` times slower.
fn precomp_clock(l: &Value, parent: &Clock, fr: f64) -> Clock {
    let st = first(l.get("st"));
    let sr = l.get("sr").and_then(Value::as_f64).filter(|s| *s > 0.0).unwrap_or(1.0);
    let lc = layer_clock(l, parent, fr);
    Clock {
        offset: parent.offset + st / fr * parent.stretch,
        stretch: parent.stretch * sr,
        window: lc.window,
    }
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    };
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut acc = 0u32;
        for (k, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * k);
        }
        let n = chunk.len();
        out.push((acc >> 16) as u8);
        if n > 2 {
            out.push((acc >> 8) as u8);
        }
        if n > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

/// Imports Lottie JSON. `fit` scales the composition to fit that box.
pub fn import(
    data: &[u8],
    base: Option<&Path>,
    fit: Option<[f64; 2]>,
    fonts: &mut crate::geom::Fonts,
) -> Result<Imported, String> {
    let root: Value = serde_json::from_slice(data).map_err(|e| format!("not Lottie JSON: {e}"))?;
    let fr = root["fr"]
        .as_f64()
        .filter(|f| *f > 0.0)
        .ok_or("not Lottie JSON: no frame rate `fr`")?;
    let (w, h) = (root["w"].as_f64().unwrap_or(512.0), root["h"].as_f64().unwrap_or(512.0));
    let (ip, op) = (first(root.get("ip")), root["op"].as_f64().unwrap_or(fr));
    let mut assets: BTreeMap<String, Value> = root["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| Some((x["id"].as_str()?.to_string(), x.clone())))
                .collect()
        })
        .unwrap_or_default();
    if let Some(list) = root["fonts"]["list"].as_array() {
        assets.insert("__fonts".into(), Value::Array(list.clone()));
    }
    let mut b = Builder {
        fonts,
        fr,
        n: 0,
        nodes: BTreeMap::new(),
        warnings: Vec::new(),
        assets,
        base: base.map(Path::to_path_buf),
        images: std::env::temp_dir().join("ultrafast-lottie"),
        unsupported: Default::default(),
    };
    let k = match fit {
        Some([fw, fh]) => (fw / w).min(fh / h),
        None => 1.0,
    };
    // Fit, then the instance's placement, outermost.
    let fit_tf = Tf {
        params: [
            ("position".to_string(), Param::Vector(vec![0.0, 0.0])),
            ("anchor".to_string(), Param::Vector(vec![0.0, 0.0])),
            ("scale".to_string(), Param::Vector(vec![k, k])),
            ("rotation".to_string(), Param::Number(0.0)),
        ]
        .into(),
        timing: Timing::default(),
    };
    let place = Tf {
        params: ["position", "anchor", "scale", "rotation"]
            .iter()
            .map(|p| (p.to_string(), Param::Text(format!("${p}"))))
            .collect(),
        timing: Timing::default(),
    };
    let clock = Clock {
        offset: -ip / fr,
        stretch: 1.0,
        window: (Some(0.0), Some((op - ip) / fr)),
    };
    let layers = root["layers"].as_array().cloned().unwrap_or_default();
    let outer = [fit_tf, place];
    let art = match b.comp(&layers, &clock, &outer, 0) {
        // Like a player's canvas, the composition ends at its edges.
        Some(c) => b.clip(c, w, h, &outer, Timing::default()),
        None => b.add(
            "empty",
            "solid",
            [("color".to_string(), Param::Text("#00000000".into()))].into(),
            &[],
            None,
        ),
    };
    let unsupported = b.unsupported.borrow().len();
    if unsupported > 0 {
        b.warn(format!(
            "{unsupported} After Effects expressions are not run (loopOut, wiggle and time are); those properties keep their own keys"
        ));
    }
    let out = b.add(
        "lottie_out",
        "opacity",
        [("opacity".to_string(), Param::Text("$opacity".into()))].into(),
        &[("source", art)],
        None,
    );
    let mut nodes = b.nodes;
    // The output under a stable name.
    let node = nodes.remove(&out).unwrap();
    nodes.insert("lottie_out".into(), node);
    let params: BTreeMap<String, Param> = [
        ("position".to_string(), Param::Vector(vec![0.0, 0.0])),
        ("anchor".to_string(), Param::Vector(vec![0.0, 0.0])),
        ("scale".to_string(), Param::Vector(vec![1.0, 1.0])),
        ("rotation".to_string(), Param::Number(0.0)),
        ("opacity".to_string(), Param::Number(1.0)),
    ]
    .into();
    Ok(Imported {
        component: Component {
            nodes,
            output: "lottie_out".into(),
            params,
            inputs: Vec::new(),
        },
        size: [w * k, h * k],
        duration: (op - ip) / fr,
        warnings: b.warnings,
    })
}

/// Reads the `lottie` node's file and imports it.
pub fn import_node(node: &Node, base: &Path, fonts: &mut crate::geom::Fonts) -> Result<Imported, String> {
    let src = match node.params.get("src") {
        Some(Param::Text(s)) => s.clone(),
        _ => return Err("`lottie` needs `src` (a Lottie .json file)".into()),
    };
    let path = if Path::new(&src).is_absolute() {
        Path::new(&src).to_path_buf()
    } else {
        base.join(&src)
    };
    let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fit = node
        .params
        .get("size")
        .and_then(|p| p.eval(0.0))
        .and_then(|v| match v.len() {
            0 => None,
            1 => Some([v[0], v[0]]),
            _ => Some([v[0], v[1]]),
        });
    import(&data, path.parent(), fit, fonts)
}

/// `doc` with every `lottie` node replaced by an instance of its imported
/// component (looping over its duration unless `loop` is false).
pub fn expand(
    doc: &Document,
    mut load: impl FnMut(&str, &Node) -> Result<std::sync::Arc<Imported>, String>,
    mut diagnose: impl FnMut(&str, String),
) -> Document {
    let mut d = doc.clone();
    let mut add: Vec<(String, Component)> = Vec::new();
    let mut fix = |scope: &str, nodes: &mut BTreeMap<String, Node>| {
        for (id, node) in nodes.iter_mut() {
            if node.op != "lottie" {
                continue;
            }
            let path = if scope.is_empty() {
                id.clone()
            } else {
                format!("{scope}/{id}")
            };
            match load(&path, node) {
                Ok(imp) => {
                    for w in &imp.warnings {
                        diagnose(&path, format!("lottie: {w}"));
                    }
                    let looping = !matches!(node.params.get("loop"), Some(Param::Bool(false)));
                    if looping && imp.duration > 0.0 {
                        let tm = node.timing.get_or_insert_with(Default::default);
                        if tm.loop_duration.is_none() {
                            tm.loop_duration = Some(imp.duration);
                        }
                    }
                    let name = format!("lottie:{path}");
                    add.push((name.clone(), imp.component.clone()));
                    node.op = name;
                    node.params.retain(|k, _| PLACEMENT.contains(&k.as_str()));
                }
                Err(e) => {
                    diagnose(&path, e);
                    node.op = "solid".into();
                    node.params = [("color".to_string(), Param::Text("#00000000".into()))].into();
                }
            }
        }
    };
    fix("", &mut d.nodes);
    let names: Vec<String> = d.components.keys().cloned().collect();
    for n in names {
        let mut c = d.components.remove(&n).unwrap();
        fix(&n, &mut c.nodes);
        d.components.insert(n, c);
    }
    for (name, c) in add {
        d.components.insert(name, c);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_and_interpolates_like_lottie() {
        let prop = json!({"a": 1, "k": [
            {"t": 0, "s": [0, 0], "o": {"x": [0.5], "y": [0.0]}, "i": {"x": [0.5], "y": [1.0]}},
            {"t": 10, "s": [100, 50]}]});
        assert_eq!(eval(&prop, 0.0), vec![0.0, 0.0]);
        assert_eq!(eval(&prop, 10.0), vec![100.0, 50.0]);
        let mid = eval(&prop, 5.0);
        assert!((mid[0] - 50.0).abs() < 1.0, "{mid:?}");
        assert!(eval(&prop, 2.0)[0] < 20.0, "eases in");
        // Old files: end values on the key.
        let old = json!({"k": [{"t": 0, "s": [1], "e": [3]}, {"t": 4}]});
        assert_eq!(eval(&old, 4.0), vec![3.0]);
    }

    #[test]
    fn common_expressions_become_drivers() {
        let layer = |r: Value| {
            json!({"v": "5.7.0", "fr": 30, "ip": 0, "op": 60, "w": 100, "h": 100, "layers": [
                {"ty": 4, "ind": 1, "ip": 0, "op": 60, "st": 0,
                 "ks": {"p": {"k": [50, 50]}, "r": r},
                 "shapes": [{"ty": "rc", "p": {"k": [0, 0]}, "s": {"k": [10, 10]}, "r": {"k": 0}},
                            {"ty": "fl", "c": {"k": [1, 1, 1, 1]}, "o": {"k": 100}}]}]})
        };
        let rotation = |r: Value| -> Param {
            let mut fonts = crate::geom::Fonts::new();
            let imp = import(layer(r).to_string().as_bytes(), None, None, &mut fonts).unwrap();
            assert!(imp.warnings.is_empty(), "{:?}", imp.warnings);
            imp.component
                .nodes
                .values()
                .filter(|n| n.op == "transform")
                .find_map(|n| n.params.get("rotation").filter(|p| p.is_animated()).cloned())
                .expect("animated rotation")
        };
        let spin = rotation(json!({"k": 0, "x": "var $bm_rt;\n$bm_rt = $bm_mul(time, 90);"}));
        assert_eq!(spin.eval(2.0), Some(vec![180.0]));
        let looped = rotation(json!({"a": 1, "x": "var $bm_rt;\n$bm_rt = loopOut('cycle');", "k": [
            {"t": 0, "s": [0]}, {"t": 30, "s": [90]}]}));
        assert_eq!(looped.eval(1.5), looped.eval(0.5));
        let wiggle = rotation(json!({"k": 10, "x": "var $bm_rt;\n$bm_rt = wiggle(2, 15);"}));
        assert!(matches!(wiggle, Param::Wiggle { .. }));
    }

    /// A stroke 0 wide draws nothing (as a path, `stroke: 0` would fill the
    /// shape in the stroke's color); one keyed to 0 stays a stroke there.
    #[test]
    fn zero_width_strokes_draw_nothing() {
        let strokes = |w: Value| -> Vec<Param> {
            let lottie = json!({"v": "5.7.0", "fr": 30, "ip": 0, "op": 60, "w": 100, "h": 100, "layers": [
                {"ty": 4, "ind": 1, "ip": 0, "op": 60, "st": 0, "ks": {"p": {"k": [50, 50]}},
                 "shapes": [{"ty": "el", "p": {"k": [0, 0]}, "s": {"k": [40, 40]}},
                            {"ty": "st", "c": {"k": [1, 0, 0, 1]}, "o": {"k": 100}, "w": w},
                            {"ty": "fl", "c": {"k": [0, 0, 1, 1]}, "o": {"k": 100}}]}]});
            let mut fonts = crate::geom::Fonts::new();
            let imp = import(lottie.to_string().as_bytes(), None, None, &mut fonts).unwrap();
            imp.component
                .nodes
                .values()
                .filter_map(|n| n.params.get("stroke").cloned())
                .collect()
        };
        assert_eq!(strokes(json!({"k": 4})).len(), 1);
        assert!(strokes(json!({"k": 0})).is_empty());
        assert!(strokes(json!({"a": 1, "k": [{"t": 0, "s": [0]}, {"t": 30, "s": [0]}]})).is_empty());
        let grows = strokes(json!({"a": 1, "k": [{"t": 0, "s": [0]}, {"t": 30, "s": [6]}]}));
        let [w] = grows.as_slice() else { panic!("{grows:?}") };
        assert!(w.eval(0.0).unwrap()[0] > 0.0, "{w:?}");
        assert_eq!(w.eval(1.0), Some(vec![6.0]));
    }

    #[test]
    fn range_selectors_sweep_letters() {
        // Square selector over 0..50% of 4 letters, offset 50%: letters 2
        // and 3 are selected fully, 0 and 1 not.
        let sel = json!({"s": {"k": 0}, "e": {"k": 50}, "o": {"k": 50}, "a": {"k": 100}, "sh": 1, "r": 1});
        let m: Vec<f64> = (0..4).map(|i| selector_mult(&sel, 0.0, i as f64, 4.0)).collect();
        assert_eq!(m, vec![0.0, 0.0, 1.0, 1.0]);
        // Ramp up across all four: rising through the middle.
        let ramp = json!({"s": {"k": 0}, "e": {"k": 100}, "sh": 2, "r": 1});
        let r: Vec<f64> = (0..4).map(|i| selector_mult(&ramp, 0.0, i as f64, 4.0)).collect();
        assert!(r.windows(2).all(|w| w[1] > w[0]), "{r:?}");
    }

    #[test]
    fn imports_a_shape_layer() {
        let lottie = json!({"v": "5.7.0", "fr": 30, "ip": 0, "op": 60, "w": 200, "h": 200, "layers": [
        {"ty": 4, "ind": 1, "ip": 0, "op": 60, "st": 0,
         "ks": {"a": {"k": [0, 0]}, "p": {"k": [100, 100]}, "s": {"k": [100, 100]}, "r": {"a": 1, "k": [
             {"t": 0, "s": [0]}, {"t": 60, "s": [360]}]}, "o": {"k": 100}},
         "shapes": [{"ty": "gr", "it": [
             {"ty": "rc", "p": {"k": [0, 0]}, "s": {"k": [80, 40]}, "r": {"k": 8}},
             {"ty": "fl", "c": {"k": [1, 0.2, 0.4, 1]}, "o": {"k": 100}},
             {"ty": "tr", "a": {"k": [0, 0]}, "p": {"k": [0, 0]}, "s": {"k": [100, 100]}, "r": {"k": 0}, "o": {"k": 100}}
         ]}]}]});
        let mut fonts = crate::geom::Fonts::new();
        let imp = import(lottie.to_string().as_bytes(), None, None, &mut fonts).unwrap();
        assert!(imp.warnings.is_empty(), "{:?}", imp.warnings);
        assert_eq!(imp.duration, 2.0);
        // The rectangle, and the composition's clip.
        let paths: Vec<&String> = imp
            .component
            .nodes
            .iter()
            .filter(|(_, n)| n.op == "path")
            .map(|(id, _)| id)
            .collect();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(paths.iter().any(|id| id.starts_with("bounds")));
        // The rotation keys come through in seconds.
        let rot = imp
            .component
            .nodes
            .values()
            .filter(|n| n.op == "transform")
            .find_map(|n| match n.params.get("rotation") {
                Some(p @ Param::Animated { .. }) => Some(p.clone()),
                _ => None,
            })
            .expect("animated rotation");
        assert_eq!(rot.eval(1.0), Some(vec![180.0]));
    }
}
