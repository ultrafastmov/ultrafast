//! Procedural param drivers: values computed from time instead of written
//! as keyframes. They evaluate like any param, so every numeric param can
//! wiggle, oscillate, repeat its keys, or follow a soundtrack.

use crate::Param;
use serde::{Deserialize, Deserializer, Serialize};

/// A number (broadcast) or a list.
fn num_or_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(f64),
        Many(Vec<f64>),
    }
    Ok(match V::deserialize(d)? {
        V::One(x) => vec![x],
        V::Many(v) => v,
    })
}

fn one() -> f64 {
    1.0
}

/// Smooth random motion around `base`: fractal value noise, one independent
/// stream per component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wiggle {
    /// Wiggles per second.
    #[serde(default = "one")]
    pub frequency: f64,
    /// Peak offset; a number (all components) or one per component.
    #[serde(default = "amplitude_default", deserialize_with = "num_or_vec")]
    pub amplitude: Vec<f64>,
    /// The value it wiggles around (number, vector or keyframes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Box<Param>>,
    #[serde(default)]
    pub seed: u32,
    /// Detail layers, each twice as fast and half as strong.
    #[serde(default = "octaves_default")]
    pub octaves: u32,
}

fn amplitude_default() -> Vec<f64> {
    vec![1.0]
}
fn octaves_default() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    #[default]
    Sine,
    Triangle,
    Square,
    Saw,
}

/// Periodic motion around `base`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Oscillate {
    /// Cycles per second.
    #[serde(default = "one")]
    pub frequency: f64,
    #[serde(default = "amplitude_default", deserialize_with = "num_or_vec")]
    pub amplitude: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Box<Param>>,
    /// Phase in cycles (0..1); a number or one per component.
    #[serde(default, deserialize_with = "num_or_vec")]
    pub phase: Vec<f64>,
    #[serde(default)]
    pub wave: Wave,
}

/// Follows a soundtrack's loudness in a frequency band. The renderer bakes
/// it into keyframes when it loads the document (it needs the audio file);
/// unbaked it evaluates to `range[0]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioDriver {
    /// Audio or video file (relative to the document).
    pub src: String,
    /// `all`, `low` (kick and bass), `mid`, or `high` (hats, air).
    #[serde(default = "band_default")]
    pub band: String,
    /// Output for silence and for the loudest moment of the track.
    #[serde(default = "range_default")]
    pub range: Vec<f64>,
    /// Release time in seconds (attack is immediate).
    #[serde(default = "smooth_default")]
    pub smooth: f64,
    /// Seconds into the file at document time 0.
    #[serde(default)]
    pub start: f64,
}

fn band_default() -> String {
    "all".into()
}
fn range_default() -> Vec<f64> {
    vec![0.0, 1.0]
}
fn smooth_default() -> f64 {
    0.12
}

fn hash(seed: u32, i: i64, k: u32) -> f64 {
    let mut x = seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add((i as u32).wrapping_mul(0x85EB_CA6B))
        .wrapping_add(((i >> 32) as u32).wrapping_mul(0x27D4_EB2F))
        ^ k.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x as f64 / u32::MAX as f64 * 2.0 - 1.0
}

/// Smooth 1D value noise in [-1, 1].
fn noise(seed: u32, k: u32, x: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let (a, b) = (hash(seed, i as i64, k), hash(seed, i as i64 + 1, k));
    a + (b - a) * u
}

fn broadcast(v: &[f64], n: usize, default: f64) -> Vec<f64> {
    match v.len() {
        0 => vec![default; n],
        1 => vec![v[0]; n],
        _ => (0..n).map(|i| v.get(i).copied().unwrap_or(default)).collect(),
    }
}

impl Wiggle {
    pub fn eval(&self, t: f64) -> Vec<f64> {
        let base = self
            .base
            .as_ref()
            .and_then(|b| b.eval(t))
            .unwrap_or_else(|| vec![0.0; self.amplitude.len()]);
        let n = base.len().max(self.amplitude.len());
        let base = broadcast(&base, n, 0.0);
        let amp = broadcast(&self.amplitude, n, 0.0);
        (0..n)
            .map(|k| {
                let (mut sum, mut a, mut f, mut norm) = (0.0, 1.0, self.frequency, 0.0);
                for o in 0..self.octaves.clamp(1, 8) {
                    sum += a * noise(self.seed.wrapping_add(o * 101), k as u32, t * f);
                    norm += a;
                    a *= 0.5;
                    f *= 2.0;
                }
                base[k] + amp[k] * sum / norm
            })
            .collect()
    }
}

impl Oscillate {
    pub fn eval(&self, t: f64) -> Vec<f64> {
        let base = self
            .base
            .as_ref()
            .and_then(|b| b.eval(t))
            .unwrap_or_else(|| vec![0.0; self.amplitude.len()]);
        let n = base.len().max(self.amplitude.len());
        let base = broadcast(&base, n, 0.0);
        let amp = broadcast(&self.amplitude, n, 0.0);
        let phase = broadcast(&self.phase, n, 0.0);
        (0..n)
            .map(|k| {
                let c = (t * self.frequency + phase[k]).rem_euclid(1.0);
                let w = match self.wave {
                    Wave::Sine => (c * std::f64::consts::TAU).sin(),
                    Wave::Triangle => 1.0 - 4.0 * (c - 0.5).abs(),
                    Wave::Square => {
                        if c < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    Wave::Saw => 2.0 * c - 1.0,
                };
                base[k] + amp[k] * w
            })
            .collect()
    }
}

/// `repeat` for keyframes: what happens after the last key.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// Start over from the first key.
    Cycle,
    /// Play back and forth.
    Pingpong,
}

/// Maps `t` into the keys' span for a repeat mode.
pub fn repeat_time(t: f64, first: f64, last: f64, mode: Repeat) -> f64 {
    let span = last - first;
    if span <= 0.0 || t <= first {
        return t;
    }
    let u = (t - first) / span;
    match mode {
        Repeat::Cycle => first + u.fract() * span,
        Repeat::Pingpong => {
            let k = u.floor() as i64;
            let f = u.fract();
            first + if k % 2 == 0 { f } else { 1.0 - f } * span
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiggle_is_smooth_bounded_and_deterministic() {
        let w: Param =
            serde_json::from_str(r#"{"wiggle": {"frequency": 2, "amplitude": 10, "base": [100, 50]}}"#).unwrap();
        let a = w.eval(1.234).unwrap();
        assert_eq!(a, w.eval(1.234).unwrap());
        assert_eq!(a.len(), 2);
        assert!((a[0] - 100.0).abs() <= 10.0 && (a[1] - 50.0).abs() <= 10.0);
        // Components move independently; nearby times are close.
        assert_ne!(a[0] - 100.0, a[1] - 50.0);
        let b = w.eval(1.235).unwrap();
        assert!((a[0] - b[0]).abs() < 0.5);
    }

    #[test]
    fn oscillate_and_repeat() {
        let o: Param = serde_json::from_str(r#"{"oscillate": {"frequency": 1, "amplitude": 2, "base": 5}}"#).unwrap();
        assert!((o.eval(0.25).unwrap()[0] - 7.0).abs() < 1e-9);
        let k: Param =
            serde_json::from_str(r#"{"keys": [{"t": 0, "v": 0}, {"t": 1, "v": 10}], "repeat": "pingpong"}"#).unwrap();
        assert!((k.eval(1.25).unwrap()[0] - 7.5).abs() < 1e-9);
        let c: Param =
            serde_json::from_str(r#"{"keys": [{"t": 0, "v": 0}, {"t": 1, "v": 10}], "repeat": "cycle"}"#).unwrap();
        assert!((c.eval(2.5).unwrap()[0] - 5.0).abs() < 1e-9);
    }
}
