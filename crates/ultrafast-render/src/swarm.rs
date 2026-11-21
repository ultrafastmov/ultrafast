//! Swarms: particles that assemble into type or a path and scatter again.
//! Each particle has a home inside the glyphs (sampled once, uniformly by
//! area) and a scattered start; `progress` (0 scattered, 1 formed) moves
//! every particle home along its own delayed, eased path. Closed form, like
//! `particles`: any frame renders in any order.

use crate::RenderError;
use crate::geom::Fonts;
use crate::ops::{parse_hex, srgb_to_linear, text};
use crate::particles::{Particles, STRIDE, hash};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use ultrafast_graph::{Node, Param};

/// Params that decide where the particles live (and so the cached homes).
const SHAPE_PARAMS: &[&str] = &[
    "text",
    "font",
    "weight",
    "style",
    "size",
    "position",
    "align",
    "valign",
    "tracking",
    "line_height",
    "width",
    "d",
    "count",
    "seed",
    "features",
    "variations",
];

fn num(node: &Node, name: &str, t: f64, d: f64) -> f64 {
    node.params
        .get(name)
        .and_then(|p| p.eval(t))
        .and_then(|v| v.first().copied())
        .unwrap_or(d)
}

fn vec2(node: &Node, name: &str, t: f64, d: [f64; 2]) -> [f64; 2] {
    match node.params.get(name).and_then(|p| p.eval(t)) {
        Some(v) if v.len() >= 2 => [v[0], v[1]],
        Some(v) if v.len() == 1 => [v[0], v[0]],
        _ => d,
    }
}

fn color(node: &Node, name: &str, t: f64, d: [f64; 4]) -> [f64; 4] {
    match node.params.get(name) {
        Some(Param::Text(s)) => parse_hex(s).unwrap_or(d),
        Some(p) => match p.eval(t) {
            Some(v) if v.len() == 4 => [v[0], v[1], v[2], v[3]],
            Some(v) if v.len() == 3 => [v[0], v[1], v[2], 1.0],
            _ => d,
        },
        None => d,
    }
}

/// Uniform points inside the filled outlines (nonzero rule).
fn sample_inside(paths: &[kurbo::BezPath], count: usize, seed: u32) -> Vec<[f32; 2]> {
    let mut edges: Vec<[f64; 4]> = Vec::new();
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in paths {
        let mut start = kurbo::Point::ZERO;
        let mut last = kurbo::Point::ZERO;
        kurbo::flatten(p.iter(), 0.4, |el| match el {
            kurbo::PathEl::MoveTo(q) => {
                if last != start {
                    edges.push([last.x, last.y, start.x, start.y]);
                }
                start = q;
                last = q;
            }
            kurbo::PathEl::LineTo(q) => {
                edges.push([last.x, last.y, q.x, q.y]);
                b = [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)];
                last = q;
            }
            kurbo::PathEl::ClosePath => {
                if last != start {
                    edges.push([last.x, last.y, start.x, start.y]);
                }
                last = start;
            }
            _ => {}
        });
        if last != start {
            edges.push([last.x, last.y, start.x, start.y]);
        }
    }
    if edges.is_empty() || b[0] >= b[2] {
        return Vec::new();
    }
    let inside = |x: f64, y: f64| {
        let mut w = 0;
        for e in &edges {
            let side = (e[2] - e[0]) * (y - e[1]) - (x - e[0]) * (e[3] - e[1]);
            if e[1] <= y {
                if e[3] > y && side > 0.0 {
                    w += 1;
                }
            } else if e[3] <= y && side < 0.0 {
                w -= 1;
            }
        }
        w != 0
    };
    let mut out = Vec::with_capacity(count);
    let mut k = 0u32;
    while out.len() < count && k < (count as u32).saturating_mul(60).max(1000) {
        let x = b[0] + (b[2] - b[0]) * hash(seed, k, 101);
        let y = b[1] + (b[3] - b[1]) * hash(seed, k, 102);
        k += 1;
        if inside(x, y) {
            out.push([x as f32, y as f32]);
        }
    }
    out
}

/// The particles' homes for this node, cached by everything they depend on.
fn homes(
    id: &str,
    node: &Node,
    t: f64,
    fonts: &mut Fonts,
    base: &Path,
    cache: &mut HashMap<u64, Arc<Vec<[f32; 2]>>>,
) -> Result<Arc<Vec<[f32; 2]>>, RenderError> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for k in SHAPE_PARAMS {
        if let Some(p) = node.params.get(*k) {
            k.hash(&mut h);
            match p {
                Param::Text(s) => s.hash(&mut h),
                p => {
                    for v in p.eval(t).unwrap_or_default() {
                        v.to_bits().hash(&mut h);
                    }
                }
            }
        }
    }
    let key = h.finish();
    if let Some(v) = cache.get(&key) {
        return Ok(v.clone());
    }
    let count = num(node, "count", t, 4000.0).clamp(1.0, 200_000.0) as usize;
    let seed = num(node, "seed", t, 1.0) as u32;
    let paths: Vec<kurbo::BezPath> = if let Some(d) = text(node, "d") {
        let p = kurbo::BezPath::from_svg(d).map_err(|e| RenderError::BadParam {
            node: id.into(),
            param: "d".into(),
            reason: format!("invalid SVG path data: {e}"),
        })?;
        let pos = vec2(node, "position", t, [0.0, 0.0]);
        vec![kurbo::Affine::translate((pos[0], pos[1])) * p]
    } else {
        let fmt = ultrafast_graph::Format {
            width: 1,
            height: 1,
            fps: 1.0,
            duration: 1.0,
            motion_blur: None,
        };
        crate::geom::text_paths(id, node, &fmt, t, crate::ops::Affine::IDENTITY, fonts, base)?
            .0
            .into_iter()
            .map(|(p, _, _)| p)
            .collect()
    };
    let v = Arc::new(sample_inside(&paths, count, seed));
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(key, v.clone());
    Ok(v)
}

/// Evaluates the swarm at local time `t`.
pub fn evaluate(
    id: &str,
    node: &Node,
    t: f64,
    fonts: &mut Fonts,
    base: &Path,
    cache: &mut HashMap<u64, Arc<Vec<[f32; 2]>>>,
) -> Result<Particles, RenderError> {
    let homes = homes(id, node, t, fonts, base, cache)?;
    let seed = num(node, "seed", t, 1.0) as u32;
    let progress = num(node, "progress", t, 1.0).clamp(0.0, 1.0);
    let stagger = num(node, "stagger", t, 0.5).clamp(0.0, 0.95);
    let scatter = num(node, "scatter", t, 700.0);
    let origin = node
        .params
        .get("origin")
        .and_then(|p| p.eval(t))
        .filter(|v| v.len() >= 2);
    let turbulence = num(node, "turbulence", t, 40.0);
    let size = vec2(node, "particle_size", t, [2.0, 3.0]);
    let c_formed = color(node, "color", t, [1.0, 1.0, 1.0, 1.0]);
    let c_scatter = color(node, "color_scatter", t, c_formed);
    let shimmer = num(node, "shimmer", t, 0.0).clamp(0.0, 1.0);
    let mut data = Vec::with_capacity(homes.len() * STRIDE);
    let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (i, home) in homes.iter().enumerate() {
        let i32_ = i as u32;
        let h = |k: u32| hash(seed ^ 0x5bd1, i32_, k);
        let delay = h(1) * stagger;
        let p = ((progress - delay) / (1.0 - stagger)).clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - p).powi(3);
        let (hx, hy) = (home[0] as f64, home[1] as f64);
        let (sx, sy) = match &origin {
            Some(o) => (o[0] + (h(2) - 0.5) * 40.0, o[1] + (h(3) - 0.5) * 40.0),
            None => {
                let a = h(2) * std::f64::consts::TAU;
                let r = scatter * (0.25 + 0.75 * h(3).sqrt());
                (hx + a.cos() * r, hy + a.sin() * r)
            }
        };
        // Drift while away from home: two slow sine pairs per particle.
        let away = 1.0 - e;
        let (fx, fy, ph) = (0.4 + h(4) * 0.9, 0.3 + h(5) * 0.8, h(6) * std::f64::consts::TAU);
        let wx = (t * fx + ph).sin() + 0.5 * (t * fx * 2.3 + ph * 1.7).sin();
        let wy = (t * fy + ph * 0.7).cos() + 0.5 * (t * fy * 1.9 + ph).cos();
        let px = sx + (hx - sx) * e + turbulence * away * wx;
        let py = sy + (hy - sy) * e + turbulence * away * wy;
        // Velocity toward home, for the streak.
        let speed = 3.0 * (1.0 - p).powi(2);
        let (vx, vy) = ((hx - sx) * speed, (hy - sy) * speed);
        let s = (size[0] + (size[1] - size[0]) * e) * (0.7 + 0.6 * h(7));
        let tw = 1.0 - shimmer * 0.5 * (1.0 + (t * (2.0 + 4.0 * h(8)) + h(9) * std::f64::consts::TAU).sin());
        let c: Vec<f64> = (0..4)
            .map(|k| c_scatter[k] + (c_formed[k] - c_scatter[k]) * e)
            .collect();
        let alpha = (c[3] * tw * (0.25 + 0.75 * e.max(1.0 - progress))).clamp(0.0, 1.0);
        let lin = |x: f64| (srgb_to_linear(x.clamp(0.0, 1.0)) * alpha) as f32;
        let half = (s / 2.0).max(0.3);
        data.extend([
            px as f32,
            py as f32,
            half as f32,
            0.0,
            lin(c[0]),
            lin(c[1]),
            lin(c[2]),
            alpha as f32,
            vx as f32,
            vy as f32,
            (num(node, "stretch", t, 0.0) * away) as f32,
            0.0,
        ]);
        let reach = half + 2.0;
        bounds = [
            bounds[0].min(px - reach),
            bounds[1].min(py - reach),
            bounds[2].max(px + reach),
            bounds[3].max(py + reach),
        ];
    }
    let count = data.len() / STRIDE;
    if count == 0 {
        bounds = [0.0; 4];
    }
    Ok(Particles { data, count, bounds })
}
