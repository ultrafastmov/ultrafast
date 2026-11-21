//! `raymarch`: 3D as a signed-distance scene marched per pixel. The agent
//! writes only the scene — `float2 map(float3 p)` returning (distance,
//! material id), and optionally `material(...)` — and the op supplies the
//! rest: a camera, soft shadows, ambient occlusion, a traced reflection
//! bounce, an environment, tone mapping and jittered antialiasing. Edges
//! are resolved per sample, so it reads as rendered rather than rasterized.
//!
//! The node expands into a `shader` before planning. Camera, target, light
//! position, exposure and five free knobs (`k0`..`k4`, read as `knob(i)`)
//! become shader params, so animating them never recompiles; the light's
//! color, the environment, samples and background are baked into the code.

use ultrafast_graph::{Document, Node, Param};

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "raymarch")
}

const LIB: &str = r#"
// ---- raymarch library -------------------------------------------------
float3 rotX(float3 p, float a) { float c = cos(a), s = sin(a); return float3(p.x, c * p.y - s * p.z, s * p.y + c * p.z); }
float3 rotY(float3 p, float a) { float c = cos(a), s = sin(a); return float3(c * p.x + s * p.z, p.y, -s * p.x + c * p.z); }
float3 rotZ(float3 p, float a) { float c = cos(a), s = sin(a); return float3(c * p.x - s * p.y, s * p.x + c * p.y, p.z); }
float rm_hash(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
float fbm3(float3 p, int oct) {
  float s = 0.0, a = 0.5;
  for (int i = 0; i < oct; i++) { s += a * snoise(p); p = p * 2.03 + float3(5.1, 1.7, 3.3); a *= 0.5; }
  return s;
}
float knob(int i) { return param(11 + i); }
float sdSphere(float3 p, float r) { return length(p) - r; }
float sdBox(float3 p, float3 b, float r) { float3 q = abs(p) - b + r; return length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0) - r; }
float sdBox2(float2 p, float2 b, float r) { float2 q = abs(p) - b + r; return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r; }
// Upright cylinder: radius r, half height h, edges rounded by rr.
float sdCylinder(float3 p, float r, float h, float rr) {
  float2 d = float2(length(p.xz) - r + rr, abs(p.y) - h + rr);
  return min(max(d.x, d.y), 0.0) + length(max(d, 0.0)) - rr;
}
float sdTorus(float3 p, float R, float r) { return length(float2(length(p.xz) - R, p.y)) - r; }
float sdCapsule(float3 p, float3 a, float3 b, float r) {
  float3 pa = p - a, ba = b - a;
  float h = saturate(dot(pa, ba) / dot(ba, ba));
  return length(pa - ba * h) - r;
}
// A flat band (ring) of radius R around axis n: width w, thickness th.
float sdBand(float3 p, float3 n, float R, float w, float th) {
  float y = dot(p, n);
  float r = length(p - n * y);
  return sdBox2(float2(r - R, y), float2(w, th), min(w, th) * 0.6);
}
float sdPlane(float3 p, float3 n, float h) { return dot(p, n) + h; }
float smin(float a, float b, float k) { float h = saturate(0.5 + 0.5 * (b - a) / k); return lerp(b, a, h) - k * h * (1.0 - h); }
// Nearest of two (distance, id) hits.
float2 opU(float2 a, float2 b) { return a.x < b.x ? a : b; }
// A 2D ultrafast layer on input i as a distance field in world units: draw the
// layer white on transparent and blur it by `soften` px; `scale` is px per
// world unit, the frame's center at the origin, y up. Exact near the edge,
// small far from it, so bound it with a box (max with sdBox) for speed.
float layerDistance(int i, float2 xy, float scale, float soften) {
  float a = input(i, float2(size().x * 0.5 + xy.x * scale, size().y * 0.5 - xy.y * scale)).a;
  return (0.5 - a) * 2.0 * soften / scale;
}
// A 2D distance extruded to `depth` (total) along z, its edges rounded by r.
float extrude(float d2, float z, float depth, float r) {
  float2 w = float2(d2 + r, abs(z) - depth * 0.5 + r);
  return min(max(w.x, w.y), 0.0) + length(max(w, 0.0)) - r;
}
// Preset materials by id: 0 matte, 1 brass, 2 polished brass, 3 steel,
// 4 gold, 5 copper, 6 black lacquer, 7 white ceramic, 8 rubber, 9 clear
// glass, 10 chrome. A material with metallic < 0 is glass (albedo tints
// it, roughness frosts it).
void presetMaterial(float id, inout float3 albedo, inout float metallic, inout float roughness) {
  int k = int(id + 0.5);
  if (k == 1) { albedo = float3(0.80, 0.56, 0.26); metallic = 1.0; roughness = 0.3; }
  else if (k == 2) { albedo = float3(0.86, 0.62, 0.30); metallic = 1.0; roughness = 0.13; }
  else if (k == 3) { albedo = float3(0.85, 0.86, 0.88); metallic = 1.0; roughness = 0.12; }
  else if (k == 4) { albedo = float3(1.0, 0.77, 0.34); metallic = 1.0; roughness = 0.18; }
  else if (k == 5) { albedo = float3(0.95, 0.60, 0.48); metallic = 1.0; roughness = 0.22; }
  else if (k == 6) { albedo = float3(0.02, 0.02, 0.022); metallic = 0.0; roughness = 0.12; }
  else if (k == 7) { albedo = float3(0.85, 0.84, 0.80); metallic = 0.0; roughness = 0.3; }
  else if (k == 8) { albedo = float3(0.05, 0.05, 0.05); metallic = 0.0; roughness = 0.8; }
  else if (k == 9) { albedo = float3(0.96, 0.98, 1.0); metallic = -1.0; roughness = 0.0; }
  else if (k == 10) { albedo = float3(0.95, 0.95, 0.96); metallic = 1.0; roughness = 0.04; }
}
"#;

const DEFAULT_MATERIAL: &str = r#"
void material(float id, float3 p, float3 n, inout float3 albedo, inout float metallic, inout float roughness) {
  presetMaterial(id, albedo, metallic, roughness);
}
"#;

const TRACE: &str = r#"
// ---- tracing ------------------------------------------------------------
float3 normalAt(float3 p) {
  float2 e = float2(0.0015, -0.0015);
  return normalize(e.xyy * map(p + e.xyy).x + e.yyx * map(p + e.yyx).x + e.yxy * map(p + e.yxy).x + e.xxx * map(p + e.xxx).x);
}
float2 march(float3 ro, float3 rd, float maxd) {
  float t = 0.0;
  for (int i = 0; i < 180; i++) {
    float2 h = map(ro + rd * t);
    if (h.x < 0.0004 * t + 0.0002) return float2(t, h.y);
    t += h.x * 0.9;
    if (t > maxd) break;
  }
  return float2(-1.0, -1.0);
}
float softShadow(float3 ro, float3 rd, float maxd) {
  float res = 1.0, t = 0.02;
  for (int i = 0; i < 56; i++) {
    float h = map(ro + rd * t).x;
    res = min(res, 10.0 * h / t);
    t += clamp(h, 0.01, 0.3);
    if (res < 0.002 || t > maxd) break;
  }
  return saturate(res);
}
float ambientOcclusion(float3 p, float3 n) {
  float occ = 0.0, sca = 1.0;
  for (int i = 0; i < 5; i++) {
    float h = 0.015 + 0.09 * float(i);
    occ += (h - map(p + n * h).x) * sca;
    sca *= 0.75;
  }
  return saturate(1.0 - 2.2 * occ);
}
float ggx(float nh, float a) { float a2 = a * a; float d = nh * nh * (a2 - 1.0) + 1.0; return a2 / (3.14159 * d * d); }
float3 aces(float3 x) { return saturate((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)); }
"#;

/// Environments, as seen in reflections and as ambient light.
fn environment(kind: &str) -> &'static str {
    match kind {
        "room" => {
            r#"
float3 env(float3 d) {
  // A candle-lit study: the flame and its glow, a cold window, a warm ceiling.
  float3 c = lerp(float3(0.010, 0.006, 0.004), float3(0.07, 0.04, 0.022), saturate(d.y * 0.9 + 0.3));
  float k = dot(d, normalize(float3(-0.80, 0.35, 0.48)));
  c += float3(1.0, 0.55, 0.22) * (pow(saturate(k), 900.0) * 40.0 + pow(saturate(k), 12.0) * 0.8);
  c += float3(0.50, 0.62, 0.85) * smoothstep(0.93, 0.96, dot(d, normalize(float3(0.7, 0.45, -0.55)))) * 0.7;
  c += float3(0.95, 0.72, 0.45) * 0.35 * smoothstep(0.3, 0.7, d.y);
  return c;
}"#
        }
        "night" => {
            r#"
float3 env(float3 d) {
  // Night: deep blue above, a warm glow along the horizon, dark water below.
  float3 c = lerp(float3(0.012, 0.02, 0.04), float3(0.002, 0.004, 0.012), saturate(d.y * 1.5));
  c += float3(0.8, 0.4, 0.15) * exp(-abs(d.y) * 18.0) * 0.6;
  if (d.y < 0.0) c *= 0.4;
  return c;
}"#
        }
        "sky" => {
            r#"
float3 env(float3 d) {
  // Daylight: a blue sky, a sun, a pale horizon, warm ground.
  float3 c = lerp(float3(0.75, 0.82, 0.92), float3(0.22, 0.42, 0.85), saturate(d.y * 1.6));
  float s = dot(d, normalize(float3(0.4, 0.55, 0.3)));
  c += float3(1.0, 0.92, 0.8) * (pow(saturate(s), 800.0) * 60.0 + pow(saturate(s), 8.0) * 0.4);
  if (d.y < 0.0) c = lerp(float3(0.35, 0.3, 0.26), float3(0.12, 0.1, 0.08), saturate(-d.y * 3.0));
  return c;
}"#
        }
        "horizon" => {
            r#"
float3 env(float3 d) {
  // The classic chrome world: a deep sky brightening to a hot horizon line,
  // dark warm ground below it, two soft panels high on either side.
  float e = d.y;
  float3 c = e > 0.0 ? lerp(float3(1.4, 1.5, 1.6), float3(0.05, 0.12, 0.3), pow(saturate(e * 2.2), 0.6))
                     : lerp(float3(0.30, 0.20, 0.13), float3(0.01, 0.008, 0.006), saturate(-e * 6.0));
  c += float3(3.0, 3.0, 3.0) * exp(-abs(e) / 0.006);
  float a = atan2(d.x, d.z);
  c += float3(1.6, 1.6, 1.7) * smoothstep(0.25, 0.18, abs(a - 1.2)) * smoothstep(0.15, 0.35, e) * smoothstep(0.8, 0.6, e);
  c += float3(1.2, 1.3, 1.5) * smoothstep(0.2, 0.14, abs(a + 1.5)) * smoothstep(0.1, 0.3, e) * smoothstep(0.75, 0.55, e);
  return c;
}"#
        }
        "lightbox" => {
            r#"
float3 env(float3 d) {
  // A ring of tall light bars round a dark floor: made for glass and chrome.
  float a = atan2(d.x, d.z);
  float bars = smoothstep(0.32, 0.36, abs(frac(a / 6.2831853 * 9.0) - 0.5));
  float3 c = float3(0.012, 0.012, 0.014) + float3(1.0, 0.98, 0.95) * 2.2 * bars * smoothstep(-0.05, 0.1, d.y) * smoothstep(0.85, 0.5, d.y);
  c += float3(0.08, 0.08, 0.09) * smoothstep(0.6, 1.0, d.y);
  return c;
}"#
        }
        // A photographer's studio: softboxes over a dark floor.
        _ => {
            r#"
float3 env(float3 d) {
  float3 c = lerp(float3(0.02, 0.02, 0.022), float3(0.10, 0.10, 0.11), saturate(d.y * 0.8 + 0.4));
  c += float3(1.0, 0.97, 0.92) * 3.0 * smoothstep(0.86, 0.9, dot(d, normalize(float3(-0.5, 0.75, 0.4))));
  c += float3(0.9, 0.95, 1.0) * 1.6 * smoothstep(0.94, 0.97, dot(d, normalize(float3(0.8, 0.3, -0.3))));
  c += float3(1.0, 1.0, 1.0) * 0.8 * smoothstep(0.05, 0.0, abs(d.x - 0.6)) * smoothstep(0.1, 0.4, d.y);
  return c;
}"#
        }
    }
}

const LIT: &str = r#"
// ---- shading -------------------------------------------------------------
float3 lightPos() { return float3(param(7), param(8), param(9)); }
// A surface lit simply (no glass, no traced bounce): what glass and
// reflections see.
float3 surface(float3 p, float3 n, float3 v, float id) {
  float3 alb = float3(0.8, 0.8, 0.8); float metal = 0.0, rough = 0.5;
  material(id, p, n, alb, metal, rough);
  if (metal < -0.5) return env(reflect(-v, n)) * 0.3;
  float3 L = lightPos() - p;
  float3 diffuse = alb * (1.0 - metal) * (saturate(dot(n, normalize(L))) * LIGHTC / dot(L, L) + env(n) * 0.4);
  return diffuse + (metal > 0.5 ? alb * env(reflect(-v, n)) * 0.8 : float3(0.0, 0.0, 0.0));
}
// Glass (a material with metallic < 0): refract in, march through the
// solid to where the ray leaves it, refract out per color channel
// (dispersion), tint by albedo with distance, reflect by Fresnel.
float3 glassAt(float3 p, float3 n, float3 v, float3 tint, float rough) {
  float3 rd = refract(-v, n, 1.0 / GLASS_IOR);
  float3 pos = p;
  float travel = 0.0;
  float3 ne = n;
  // Through the solid: march to where the ray meets the inside of the
  // surface; leave if it can, else reflect inside (up to four times).
  for (int b = 0; b < 4; b++) {
    float t = 0.02;
    for (int i = 0; i < 64; i++) {
      float h = -map(pos + rd * t).x;
      if (h < 0.0005) break;
      t += max(h, 0.004);
    }
    pos += rd * t;
    travel += t;
    ne = -normalAt(pos);
    float3 out_ = refract(rd, ne, GLASS_IOR);
    if (dot(out_, out_) > 1e-6) break;
    rd = reflect(rd, ne);
  }
  // What lies behind: the scene where the middle of the spectrum lands,
  // else the environment, split by wavelength.
  float3 mid = refract(rd, ne, GLASS_IOR);
  if (dot(mid, mid) < 1e-6) mid = reflect(rd, ne);
  float2 behind = march(pos + mid * 0.01, mid, 20.0);
  float3 col = float3(0.0, 0.0, 0.0);
  if (behind.x > 0.0) {
    float3 hp = pos + mid * (0.01 + behind.x);
    col = surface(hp, normalAt(hp), -mid, behind.y);
  } else {
    for (int c = 0; c < 3; c++) {
      float eta = GLASS_IOR * (1.0 + (float(c) - 1.0) * 0.012 * GLASS_DISPERSION);
      float3 rdo = refract(rd, ne, eta);
      if (dot(rdo, rdo) < 1e-6) rdo = reflect(rd, ne);
      float3 e = lerp(env(rdo), env(normalize(rdo + ne * rough)), saturate(rough * 2.0));
      col[c] = e[c];
    }
  }
  col *= pow(max(tint, 0.001), travel * 0.6);
  float nv = saturate(dot(n, v));
  float fr = 0.04 + 0.96 * pow(1.0 - nv, 5.0);
  return lerp(col, env(reflect(-v, n)), fr);
}
float3 lit(float3 p, float3 n, float3 v, float id, bool bounce) {
  float3 alb = float3(0.8, 0.8, 0.8); float metal = 0.0, rough = 0.5;
  material(id, p, n, alb, metal, rough);
  if (metal < -0.5) return glassAt(p, n, v, alb, rough);
  float3 Lv = lightPos() - p;
  float dist = length(Lv);
  float3 L = Lv / dist;
  float3 Lc = LIGHTC / (dist * dist);
  float nl = saturate(dot(n, L));
  float sh = nl > 0.0 ? softShadow(p + n * 0.003, L, dist) : 0.0;
  float occ = ambientOcclusion(p, n);
  float3 f0 = lerp(float3(0.04, 0.04, 0.04), alb, metal);
  float nv = saturate(dot(n, v));
  float3 F = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
  float3 h = normalize(L + v);
  float spec = ggx(saturate(dot(n, h)), max(rough * rough, 0.002)) * 0.25;
  float3 col = alb * (1.0 - metal) * (nl * sh * Lc + env(n) * 0.6 * occ);
  col += F * spec * nl * sh * Lc;
  float3 r = reflect(-v, n);
  // Rough surfaces see a blurred environment: lean toward the normal.
  float3 refl = lerp(env(r), env(normalize(r + n)) * 0.6, saturate((rough - 0.15) * 2.0));
  if (BOUNCE && bounce && rough < 0.45) {
    float2 hit = march(p + n * 0.004, r, 14.0);
    if (hit.x > 0.0) {
      float3 hp = p + n * 0.004 + r * hit.x;
      float3 hn = normalAt(hp);
      float3 a2 = float3(0.8, 0.8, 0.8); float m2 = 0.0, r2 = 0.5;
      material(hit.y, hp, hn, a2, m2, r2);
      float3 L2 = lightPos() - hp;
      refl = a2 * (saturate(dot(hn, normalize(L2))) * LIGHTC / dot(L2, L2) * 0.8 + env(hn) * 0.3)
           + (m2 > 0.5 ? a2 * env(reflect(r, hn)) * 0.6 : float3(0.0, 0.0, 0.0));
    }
  }
  col += refl * F * occ * lerp(1.0, 0.55, saturate(rough * 1.6));
  return col;
}
"#;

const DRIVER: &str = r#"
// ---- camera and samples ------------------------------------------------------
float4 shade(float2 pos, float px) {
  float2 sz = size();
  float3 target = float3(param(3), param(4), param(5));
  float az = radians(param(0)), el = radians(param(1));
  float3 eye = target + param(2) * float3(sin(az) * cos(el), sin(el), cos(az) * cos(el));
  float3 fw = normalize(target - eye);
  float3 rt = normalize(cross(fw, float3(0.0, 1.0, 0.0)));
  float3 up = cross(rt, fw);
  float focal = 1.0 / tan(radians(param(6)) * 0.5);
  float3 acc = float3(0.0, 0.0, 0.0);
  float cover = 0.0;
  for (int s = 0; s < SAMPLES; s++) {
    // Sub-pixel jitter (turned by the clock when the scene moves, so the
    // motion-blur samples of a frame antialias too).
    float a = (float(s) + 0.5) / float(SAMPLES) * 6.2831853 + JITTER;
    float2 q = (pos + (SAMPLES > 1 ? float2(cos(a), sin(a)) * 0.38 : float2(0.0, 0.0)) - sz * 0.5) / (sz.y * 0.5);
    float3 rd = normalize(fw * focal + rt * q.x - up * q.y);
    float2 hit = march(eye, rd, MAXD);
    if (hit.x < 0.0) {
      acc += BACKGROUND;
    } else {
      float3 p = eye + rd * hit.x;
      acc += lit(p, normalAt(p), -rd, hit.y, true);
      cover += 1.0;
    }
  }
  float3 c = aces(acc / float(SAMPLES) * param(10));
  // Misses add nothing when the background is transparent, so the average
  // is already premultiplied by coverage.
  float alpha = OPAQUE ? 1.0 : cover / float(SAMPLES);
  return float4(c, alpha);
}
"#;

fn text<'a>(node: &'a Node, k: &str) -> Option<&'a str> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s),
        _ => None,
    }
}

fn num(node: &Node, k: &str, d: f64) -> f64 {
    match node.params.get(k) {
        Some(Param::Number(v)) => *v,
        _ => d,
    }
}

/// Component `i` of a vector param (static, keyed or expression list).
fn component(p: &Param, i: usize) -> Option<Param> {
    match p {
        Param::Number(v) => Some(Param::Number(*v)),
        Param::Vector(v) => v.get(i).map(|x| Param::Number(*x)),
        Param::Animated { keys, repeat } => Some(Param::Animated {
            keys: keys
                .iter()
                .map(|k| {
                    let mut k = k.clone();
                    k.v = vec![k.v.get(i).or(k.v.last()).copied().unwrap_or(0.0)];
                    k
                })
                .collect(),
            repeat: *repeat,
        }),
        _ => None,
    }
}

fn color3(node: &Node, k: &str, d: [f64; 3]) -> [f64; 3] {
    match node.params.get(k) {
        Some(Param::Text(s)) => crate::transitions::linear(s),
        Some(Param::Vector(v)) if v.len() >= 3 => [v[0], v[1], v[2]],
        _ => d,
    }
}

/// `doc` with every `raymarch` node turned into a `shader`.
pub fn expand(doc: &Document) -> (Document, Vec<(String, String)>) {
    let mut d = doc.clone();
    let mut problems = Vec::new();
    let ids: Vec<String> = doc
        .nodes
        .iter()
        .filter(|(_, n)| n.op == "raymarch")
        .map(|(k, _)| k.clone())
        .collect();
    for id in ids {
        let node = d.nodes[&id].clone();
        let scene = text(&node, "scene").unwrap_or("");
        if !scene.contains("map(") {
            problems.push((
                id.clone(),
                "`raymarch` needs `scene`: Slang defining float2 map(float3 p) -> (distance, material id)".into(),
            ));
        }
        let env_kind = text(&node, "environment").unwrap_or("studio");
        let samples = num(&node, "samples", 2.0).clamp(1.0, 8.0) as i32;
        let light = color3(&node, "light_color", [1.0, 0.95, 0.88]);
        let li = num(&node, "light_intensity", 40.0);
        let bg = text(&node, "background").unwrap_or("environment");
        let (background, opaque) = match bg {
            "transparent" => ("float3(0.0, 0.0, 0.0)".to_string(), false),
            "environment" => (format!("env(rd) * {:.4}", num(&node, "background_level", 0.35)), true),
            c => {
                let k = crate::transitions::linear(c);
                (format!("float3({:.5}, {:.5}, {:.5})", k[0], k[1], k[2]), true)
            }
        };
        // Jitter by the clock only when something moves, so a still scene
        // stays static (rendered once, then cached).
        let moving = node.is_animated() || scene.contains("time(");
        let header = format!(
            "#define SAMPLES {samples}\n#define MAXD {:.2}\n#define BOUNCE {}\n#define OPAQUE {}\n#define JITTER {}\n#define BACKGROUND ({background})\nstatic const float3 LIGHTC = float3({:.5}, {:.5}, {:.5});\n#define GLASS_IOR {:.4}\n#define GLASS_DISPERSION {:.4}\n",
            num(&node, "max_distance", 80.0),
            if matches!(node.params.get("bounce"), Some(Param::Bool(false))) {
                "false"
            } else {
                "true"
            },
            if opaque { "true" } else { "false" },
            if moving {
                "(rm_hash(float2(time() * 37.0, 1.3)) * 6.2831853)"
            } else {
                "0.4"
            },
            light[0] * li,
            light[1] * li,
            light[2] * li,
            num(&node, "ior", 1.5).max(1.0),
            num(&node, "dispersion", 1.0).max(0.0),
        );
        let material = if scene.contains("material(") {
            ""
        } else {
            DEFAULT_MATERIAL
        };
        let code = format!(
            "{header}{LIB}\n// ---- scene\n{scene}\n{material}{TRACE}{}{LIT}{DRIVER}",
            environment(env_kind)
        );
        let mut params: std::collections::BTreeMap<String, Param> = std::collections::BTreeMap::new();
        params.insert("code".into(), Param::Text(code));
        let vec_param =
            |k: &str, def: [f64; 3]| -> Param { node.params.get(k).cloned().unwrap_or(Param::Vector(def.to_vec())) };
        let slots: [(&str, [f64; 3], usize); 3] = [
            ("orbit", [30.0, 20.0, 8.0], 0),
            ("target", [0.0, 0.0, 0.0], 3),
            ("light", [-4.0, 5.0, 4.0], 7),
        ];
        for (k, def, at) in slots {
            let p = vec_param(k, def);
            for i in 0..3 {
                match component(&p, i) {
                    Some(c) => {
                        params.insert(format!("u{}", at + i), c);
                    }
                    None => problems.push((id.clone(), format!("`{k}` must be a vec3 or keys of vec3"))),
                }
            }
        }
        params.insert(
            "u6".into(),
            node.params.get("fov").cloned().unwrap_or(Param::Number(35.0)),
        );
        params.insert(
            "u10".into(),
            node.params.get("exposure").cloned().unwrap_or(Param::Number(1.0)),
        );
        for i in 0..5 {
            if let Some(p) = node.params.get(&format!("k{i}")) {
                params.insert(format!("u{}", 11 + i), p.clone());
            }
        }
        if let Some(r) = node.params.get("resolution") {
            params.insert("resolution".into(), r.clone());
        }
        let n = d.nodes.get_mut(&id).unwrap();
        n.op = "shader".into();
        n.params = params;
        // A shader outside its window passes its first input through, and
        // here that is a texture the scene reads, not something to show. So
        // a timed scene sits in a composite over nothing: outside the window
        // the composite shows nothing, and the scene is not rendered at all.
        if node.timing.is_some_and(|t| t.has_window()) {
            let scene = std::mem::take(n);
            let (inner, clear) = (format!("{id}/scene"), format!("{id}/clear"));
            let timing = scene.timing;
            d.nodes.insert(inner.clone(), scene);
            d.nodes.insert(
                clear.clone(),
                Node {
                    op: "solid".into(),
                    params: [("color".to_string(), Param::Text("#00000000".into()))].into(),
                    ..Default::default()
                },
            );
            d.nodes.insert(
                id.clone(),
                Node {
                    op: "over".into(),
                    inputs: [("fg".to_string(), inner), ("bg".to_string(), clear)].into(),
                    timing,
                    ..Default::default()
                },
            );
        }
    }
    (d, problems)
}
