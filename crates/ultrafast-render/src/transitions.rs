//! `transition`: from one shot (`a`) to the next (`b`) over `duration`
//! seconds starting `at`. Before it only `a` renders, after it only `b`
//! (three windowed layers, so neither shot costs anything outside its
//! time); during it one shader blends them by kind.

use serde_json::json;
use std::collections::BTreeMap;
use ultrafast_graph::{Document, Node, Param};

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "transition")
}

pub const KINDS: &[&str] = &[
    "crossfade",
    "dip",
    "wipe",
    "push",
    "slide",
    "iris",
    "zoom",
    "dissolve",
    "glitch",
    "blur",
];

const SHADER: &str = r#"
float tr_hash(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
bool tr_inside(float2 p, float2 sz) { return p.x >= 0.0 && p.y >= 0.0 && p.x < sz.x && p.y < sz.y; }
float4 shade(float2 pos, float px) {
  float2 sz = size();
  float2 uv = pos / sz;
  int kind = int(param(0) + 0.5);
  float p = saturate(param(1));
  float ang = param(2) * 0.01745329;
  float2 dir = float2(cos(ang), sin(ang));
  float soft = max(param(3), 1e-3);
  float3 col = float3(param(4), param(5), param(6));
  float2 c = float2(param(7), param(8));
  if (kind == 3 || kind == 4) {
    // Push: both move along dir. Slide: the next shot slides over.
    float2 travel = dir * sz;
    float2 pb = pos + travel * (1.0 - p);
    float4 b = tr_inside(pb, sz) ? input(1, pb) : float4(0.0);
    if (kind == 4) return b + input(0, pos) * (1.0 - b.a);
    float2 pa = pos + travel * -p;
    float4 a = tr_inside(pa, sz) ? input(0, pa) : float4(0.0);
    return b + a * (1.0 - b.a);
  }
  if (kind == 6) {
    // Zoom through: the shot rushes toward the viewer, the next settles in.
    float sa = 1.0 + p * p * 1.4;
    float sb = 1.0 + 0.35 * (1.0 - p) * (1.0 - p);
    float4 a = float4(0.0), b = float4(0.0);
    float streak = sin(3.14159 * p) * 0.06;
    for (int k = 0; k < 8; k++) {
      float f = 1.0 + streak * (float(k) / 7.0 - 0.5);
      a += input(0, c + (pos - c) / (sa * f));
      b += input(1, c + (pos - c) / (sb * f));
    }
    return lerp(a, b, smoothstep(0.35, 0.65, p)) / 8.0;
  }
  if (kind == 9) {
    // Blur through: both soften toward the middle.
    float r = sin(3.14159 * p) * soft * 90.0;
    float4 a = float4(0.0), b = float4(0.0);
    for (int k = 0; k < 16; k++) {
      float rr = r * sqrt((float(k) + 0.5) / 16.0);
      float th = float(k) * 2.39996;
      float2 o = rr * float2(cos(th), sin(th));
      a += input(0, pos + o);
      b += input(1, pos + o);
    }
    return lerp(a, b, smoothstep(0.2, 0.8, p)) / 16.0;
  }
  if (kind == 8) {
    // Glitch: torn slices and split channels around the cut.
    float k = sin(3.14159 * p);
    float row = floor(uv.y * 28.0);
    float step_ = floor(p * 14.0);
    float jitter = (tr_hash(float2(row, step_)) - 0.5) * k * 0.25 * sz.x;
    float2 q = pos + float2(jitter, 0.0);
    bool next = tr_hash(float2(row, step_ + 7.0)) < p;
    float split = k * 18.0;
    float4 m = next ? input(1, q) : input(0, q);
    float4 r = next ? input(1, q + float2(split, 0.0)) : input(0, q + float2(split, 0.0));
    float4 bl = next ? input(1, q - float2(split, 0.0)) : input(0, q - float2(split, 0.0));
    return float4(r.r, m.g, bl.b, max(m.a, max(r.a, bl.a)));
  }
  float4 A = input(0, pos), B = input(1, pos);
  if (kind == 1) {
    float4 C = float4(col, 1.0);
    return p < 0.5 ? lerp(A, C, smoothstep(0.0, 0.5, p)) : lerp(C, B, smoothstep(0.5, 1.0, p));
  }
  if (kind == 2) {
    // Wipe: the next shot uncovers along dir, with a soft edge.
    float s = dot(uv - 0.5, dir) / (0.5 * (abs(dir.x) + abs(dir.y))) * 0.5 + 0.5;
    float q = p * (1.0 + 2.0 * soft) - soft;
    float m = smoothstep(q + soft, q - soft, s);
    float edge = smoothstep(soft, 0.0, abs(s - q)) * (1.0 - abs(2.0 * p - 1.0));
    return lerp(A, B, m) + float4(col * edge * param(9), 0.0);
  }
  if (kind == 5) {
    // Iris: a circle from the center opens onto the next shot.
    float far = max(max(length(c), length(c - float2(sz.x, 0.0))), max(length(c - float2(0.0, sz.y)), length(c - sz)));
    float R = p * far * (1.0 + soft);
    float m = smoothstep(R, R - soft * sz.y * 0.25, length(pos - c));
    return lerp(A, B, m);
  }
  if (kind == 7) {
    // Burn through fractal noise: the old shot chars just ahead of a ragged
    // front, the front glows in `color`, the new shot shows behind it.
    float2 g = pos / max(param(10), 1.0);
    float f = 0.0, amp = 0.5;
    for (int o = 0; o < 4; o++) { f += amp * snoise(float3(g, 3.7 + o * 1.9)); g = g * 2.13 + float2(7.1, 3.3); amp *= 0.5; }
    float n = saturate(0.5 + f * 0.85);
    float q = p * (1.0 + 2.0 * soft) - soft;
    float d = n - q;  // > 0: still the old shot
    float m = smoothstep(soft * 0.08, -soft * 0.08, d);
    float live = step(0.001, p) * step(p, 0.999);
    float burnt = smoothstep(soft, 0.0, d) * step(0.0, d) * live;
    float edge = exp(-abs(d) / max(soft * 0.06, 1e-4)) * live;
    A = float4(A.rgb * (1.0 - 0.8 * burnt * burnt), A.a);
    return lerp(A, B, m) + float4(col * edge * param(9), 0.0);
  }
  return lerp(A, B, p);
}"#;

fn text<'a>(node: &'a Node, k: &str) -> Option<&'a str> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s),
        _ => None,
    }
}

fn num(node: &Node, k: &str, d: f64) -> f64 {
    node.params
        .get(k)
        .and_then(|p| p.eval(0.0))
        .and_then(|v| v.first().copied())
        .unwrap_or(d)
}

/// Linear-light RGB of a color string (#rrggbb).
pub(crate) fn linear(hex: &str) -> [f64; 3] {
    let h = hex.trim_start_matches('#');
    let c = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(255) as f64 / 255.0;
    let l = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [l(c(0)), l(c(2)), l(c(4))]
}

/// `doc` with every `transition` expanded. Problems are returned per node.
pub fn expand(doc: &Document) -> (Document, Vec<(String, String)>) {
    let mut d = doc.clone();
    let mut problems = Vec::new();
    let ids: Vec<String> = doc
        .nodes
        .iter()
        .filter(|(_, n)| n.op == "transition")
        .map(|(k, _)| k.clone())
        .collect();
    let (w, h) = (doc.format.width as f64, doc.format.height as f64);
    for id in ids {
        let node = d.nodes[&id].clone();
        let (Some(a), Some(b)) = (node.inputs.get("a").cloned(), node.inputs.get("b").cloned()) else {
            problems.push((
                id.clone(),
                "`transition` needs inputs `a` (outgoing) and `b` (incoming)".into(),
            ));
            continue;
        };
        let kind = text(&node, "kind").unwrap_or("crossfade");
        let Some(k) = KINDS.iter().position(|x| *x == kind) else {
            problems.push((
                id.clone(),
                format!("unknown transition `{kind}` ({})", KINDS.join(", ")),
            ));
            continue;
        };
        let at = num(&node, "at", 0.0);
        let dur = num(&node, "duration", 0.8).max(1.0 / doc.format.fps);
        // `ease`: a named curve, or [x1, y1, x2, y2] (CSS cubic-bezier).
        let ease = match node.params.get("ease") {
            Some(Param::Text(name)) => json!(name),
            Some(Param::Vector(v)) if v.len() == 4 => json!({ "bezier": v }),
            _ => match kind {
                "push" | "slide" | "wipe" | "iris" | "zoom" => json!({"bezier": [0.7, 0.0, 0.2, 1.0]}),
                _ => json!("in_out_cubic"),
            },
        };
        let progress: Param = match serde_json::from_value(json!({"keys": [
            {"t": at, "v": [0.0], "ease": ease},
            {"t": at + dur, "v": [1.0]}]}))
        {
            Ok(p) => p,
            Err(e) => {
                problems.push((id.clone(), format!("bad ease: {e}")));
                continue;
            }
        };
        let direction = num(&node, "direction", 0.0);
        // A dissolve's front glows like an ember unless told otherwise.
        let col = linear(text(&node, "color").unwrap_or(if kind == "dissolve" { "#ff9a4a" } else { "#000000" }));
        let center = node
            .params
            .get("center")
            .and_then(|p| p.eval(0.0))
            .unwrap_or_else(|| vec![w / 2.0, h / 2.0]);
        let soft = num(
            &node,
            "softness",
            match kind {
                "wipe" => 0.08,
                "iris" => 0.04,
                "dissolve" => 0.12,
                "blur" => 1.0,
                _ => 0.05,
            },
        );
        let mut p = BTreeMap::new();
        p.insert("code".to_string(), Param::Text(SHADER.to_string()));
        p.insert("u0".to_string(), Param::Number(k as f64));
        p.insert("u1".to_string(), progress);
        p.insert("u2".to_string(), Param::Number(direction));
        p.insert("u3".to_string(), Param::Number(soft));
        p.insert("u4".to_string(), Param::Number(col[0]));
        p.insert("u5".to_string(), Param::Number(col[1]));
        p.insert("u6".to_string(), Param::Number(col[2]));
        p.insert(
            "u7".to_string(),
            Param::Number(center.first().copied().unwrap_or(w / 2.0)),
        );
        p.insert(
            "u8".to_string(),
            Param::Number(center.get(1).copied().unwrap_or(h / 2.0)),
        );
        p.insert(
            "u9".to_string(),
            Param::Number(num(&node, "glow", if kind == "dissolve" { 2.0 } else { 0.0 })),
        );
        p.insert("u10".to_string(), Param::Number(num(&node, "grain", 160.0)));
        let window = |inp: Option<f64>, out: Option<f64>| -> Option<ultrafast_graph::Timing> {
            serde_json::from_value(json!({"in": inp, "out": out})).ok()
        };
        let mix = format!("{id}/mix");
        d.nodes.insert(
            mix.clone(),
            Node {
                op: "shader".into(),
                params: p,
                inputs: [("in0".to_string(), a.clone()), ("in1".to_string(), b.clone())].into(),
                ..Default::default()
            },
        );
        let empty = format!("{id}/empty");
        d.nodes.insert(
            empty.clone(),
            Node {
                op: "solid".into(),
                params: [("color".to_string(), Param::Text("#00000000".into()))].into(),
                ..Default::default()
            },
        );
        let before = format!("{id}/before");
        d.nodes.insert(
            before.clone(),
            Node {
                op: "over".into(),
                inputs: [("fg".to_string(), a), ("bg".to_string(), empty)].into(),
                timing: window(None, Some(at)),
                ..Default::default()
            },
        );
        let during = format!("{id}/during");
        d.nodes.insert(
            during.clone(),
            Node {
                op: "over".into(),
                inputs: [("fg".to_string(), mix), ("bg".to_string(), before)].into(),
                timing: window(Some(at), Some(at + dur)),
                ..Default::default()
            },
        );
        // The transition node itself: the next shot from the end on.
        d.nodes.insert(
            id.clone(),
            Node {
                op: "over".into(),
                inputs: [("fg".to_string(), b), ("bg".to_string(), during)].into(),
                timing: window(Some(at + dur), None),
                ..Default::default()
            },
        );
    }
    (d, problems)
}
