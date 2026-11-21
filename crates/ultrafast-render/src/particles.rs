//! Deterministic particle systems: every particle's state is a closed-form
//! function of (seed, index, time), so any frame renders in any order and
//! scrubbing or editing never needs a simulation history.

use crate::RenderError;
use crate::ops::{parse_hex, srgb_to_linear, text};
use ultrafast_graph::{Node, Param};

/// GPU layout per particle (12 floats); see shaders/particles.slang.
pub const STRIDE: usize = 12;

pub struct Particles {
    pub data: Vec<f32>,
    pub count: usize,
    /// Frame-space bounds of everything drawn.
    pub bounds: [f64; 4],
}

pub(crate) fn hash(seed: u32, i: u32, k: u32) -> f64 {
    // PCG-style integer hash to [0, 1).
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ i.wrapping_mul(0x85EB_CA6B) ^ k.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x as f64 / 4_294_967_296.0
}

struct R<'a> {
    node: &'a Node,
    t: f64,
}

impl R<'_> {
    fn v(&self, name: &str, d: &[f64]) -> Vec<f64> {
        let mut v = self
            .node
            .params
            .get(name)
            .and_then(|p| p.eval(self.t))
            .unwrap_or_else(|| d.to_vec());
        if v.len() == 1 && d.len() > 1 {
            v = vec![v[0]; d.len()];
        }
        v.resize(d.len(), 0.0);
        v
    }
    fn n(&self, name: &str, d: f64) -> f64 {
        self.v(name, &[d])[0]
    }
    fn color(&self, name: &str, d: [f64; 4]) -> [f64; 4] {
        match self.node.params.get(name) {
            Some(Param::Text(s)) => parse_hex(s).unwrap_or(d),
            Some(p) => match p.eval(self.t) {
                Some(v) if v.len() == 4 => [v[0], v[1], v[2], v[3]],
                Some(v) if v.len() == 3 => [v[0], v[1], v[2], 1.0],
                _ => d,
            },
            None => d,
        }
    }
}

/// Evaluates every particle alive at local time `t`.
pub fn evaluate(id: &str, node: &Node, t: f64) -> Result<Particles, RenderError> {
    let r = R { node, t };
    let bad = |param: &str, reason: String| RenderError::BadParam {
        node: id.into(),
        param: param.into(),
        reason,
    };
    let seed = r.n("seed", 1.0) as u32;
    let rate = r.n("rate", 60.0).max(0.0);
    let max = r.n("max", 5000.0).clamp(0.0, 200_000.0) as usize;
    let life = r.v("life", &[1.5, 2.5]);
    let pos = r.v("position", &[960.0, 540.0]);
    let area = r.v("emitter_size", &[0.0, 0.0]);
    let dir = r.n("direction", -90.0).to_radians();
    let spread = r.n("spread", 30.0).to_radians();
    let speed = r.v("speed", &[200.0, 400.0]);
    let gravity = r.v("gravity", &[0.0, 300.0]);
    let drag = r.n("drag", 0.0).max(0.0);
    let size = r.v("size", &[8.0, 2.0]);
    let jitter = r.n("size_jitter", 0.3).clamp(0.0, 1.0);
    let spin = r.v("spin", &[0.0, 0.0]);
    let c0 = r.color("color_start", [1.0, 1.0, 1.0, 1.0]);
    let c1 = r.color("color_end", c0);
    let fade_in = r.n("fade_in", 0.1).max(1e-6);
    let fade_out = r.n("fade_out", 0.3).max(1e-6);
    let emit_start = r.n("emit_start", 0.0);
    let emit_end = r.n("emit_end", f64::INFINITY);
    let stretch = r.n("stretch", 0.0).max(0.0);
    let shape = match text(node, "shape").unwrap_or("circle") {
        "circle" => 0.0,
        "square" => 1.0,
        "spark" => 2.0,
        "sprite" => 3.0,
        o => {
            return Err(bad(
                "shape",
                format!("unknown shape `{o}` (circle | square | spark | sprite)"),
            ));
        }
    };
    let max_life = life[0].max(life[1]);
    let mut data = Vec::new();
    let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    if rate <= 0.0 {
        return Ok(Particles {
            data,
            count: 0,
            bounds: [0.0, 0.0, 0.0, 0.0],
        });
    }
    let first = (((t - max_life - emit_start) * rate).floor().max(0.0)) as u64;
    let last_emitted = ((t.min(emit_end) - emit_start) * rate).floor();
    if last_emitted < 0.0 {
        return Ok(Particles {
            data,
            count: 0,
            bounds: [0.0, 0.0, 0.0, 0.0],
        });
    }
    let last = last_emitted as u64;
    let mut count = 0;
    for i in first..=last {
        if count >= max {
            break;
        }
        let ii = i as u32;
        let h = |k: u32| hash(seed, ii, k);
        let spawn = emit_start + (i as f64 + h(0)) / rate;
        let lifetime = life[0] + (life[1] - life[0]) * h(1);
        let age = t - spawn;
        if age < 0.0 || age >= lifetime || spawn > emit_end {
            continue;
        }
        let u = age / lifetime;
        let a = dir + (h(2) - 0.5) * spread;
        let sp = speed[0] + (speed[1] - speed[0]) * h(3);
        let (v0x, v0y) = (a.cos() * sp, a.sin() * sp);
        let p0x = pos[0] + (h(4) - 0.5) * area[0];
        let p0y = pos[1] + (h(5) - 0.5) * area[1];
        // Linear drag has an exact closed form; without drag it is ballistic.
        let (px, py, vx, vy) = if drag > 1e-6 {
            let e = (-drag * age).exp();
            let f = (1.0 - e) / drag;
            let g = (age - f) / drag;
            (
                p0x + v0x * f + gravity[0] * g,
                p0y + v0y * f + gravity[1] * g,
                v0x * e + gravity[0] * f,
                v0y * e + gravity[1] * f,
            )
        } else {
            (
                p0x + v0x * age + 0.5 * gravity[0] * age * age,
                p0y + v0y * age + 0.5 * gravity[1] * age * age,
                v0x + gravity[0] * age,
                v0y + gravity[1] * age,
            )
        };
        let s = (size[0] + (size[1] - size[0]) * u) * (1.0 - jitter * h(6));
        let half = (s / 2.0).max(0.05);
        let rot = (h(7) * 360.0 + (spin[0] + (spin[1] - spin[0]) * h(8)) * age).to_radians();
        let fade = (u / fade_in).min(1.0) * ((1.0 - u) / fade_out).min(1.0);
        let col: Vec<f64> = (0..4).map(|k| c0[k] + (c1[k] - c0[k]) * u).collect();
        let alpha = (col[3] * fade).clamp(0.0, 1.0);
        let lin = |x: f64| srgb_to_linear(x.clamp(0.0, 1.0)) * alpha;
        let speed_now = (vx * vx + vy * vy).sqrt();
        let st = stretch * speed_now / 400.0;
        data.extend([
            px as f32,
            py as f32,
            half as f32,
            rot as f32,
            lin(col[0]) as f32,
            lin(col[1]) as f32,
            lin(col[2]) as f32,
            alpha as f32,
            vx as f32,
            vy as f32,
            st as f32,
            shape,
        ]);
        let reach = half * (1.0 + st) + 2.0;
        bounds = [
            bounds[0].min(px - reach),
            bounds[1].min(py - reach),
            bounds[2].max(px + reach),
            bounds[3].max(py + reach),
        ];
        count += 1;
    }
    if count == 0 {
        bounds = [0.0, 0.0, 0.0, 0.0];
    }
    Ok(Particles { data, count, bounds })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_bounded() {
        let node: Node = serde_json::from_str(r#"{"op":"particles","params":{"rate":100,"life":[1,1]}}"#).unwrap();
        let a = evaluate("p", &node, 2.0).unwrap();
        let b = evaluate("p", &node, 2.0).unwrap();
        assert_eq!(a.data, b.data);
        // About one second of particles alive at 100/s.
        assert!((90..=110).contains(&a.count), "{}", a.count);
    }
}
