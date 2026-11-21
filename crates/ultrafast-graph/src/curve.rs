use serde::{Deserialize, Deserializer, Serialize};

/// One keyframe. `ease` shapes the segment that starts at this key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub t: f64,
    /// A number, a vector, or a `#rrggbb[aa]` color (as 0..1 components).
    #[serde(deserialize_with = "key_value")]
    pub v: Vec<f64>,
    #[serde(default, deserialize_with = "ease_value")]
    pub ease: Ease,
}

/// An ease: a name (`out_cubic`, `out_back`, `spring`, ...),
/// `{"bezier": [x1, y1, x2, y2]}` or `{"spring": [damping, cycles]}`.
fn ease_value<'de, D: Deserializer<'de>>(d: D) -> Result<Ease, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum E {
        Name(String),
        Bezier { bezier: [f64; 4] },
        Spring { spring: Vec<f64> },
    }
    Ok(match E::deserialize(d)? {
        E::Name(n) if n == "spring" => Ease::Spring(0.45, 2.0),
        E::Name(n) => Ease::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&n)).map_err(|_| {
            serde::de::Error::custom(format!(
                "unknown ease `{n}` (linear, hold, in/out/in_out + quad, cubic, sine, expo, back; out_elastic, out_bounce, spring, or {{\"bezier\": [x1, y1, x2, y2]}})"
            ))
        })?,
        E::Bezier { bezier: [a, b, c, e] } => Ease::Bezier(a, b, c, e),
        E::Spring { spring } => Ease::Spring(spring.first().copied().unwrap_or(0.45), spring.get(1).copied().unwrap_or(2.0)),
    })
}

fn key_value<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(f64),
        Many(Vec<f64>),
        Hex(String),
    }
    match V::deserialize(d)? {
        V::One(x) => Ok(vec![x]),
        V::Many(v) => Ok(v),
        V::Hex(s) => {
            let h = s.strip_prefix('#').unwrap_or(&s);
            let byte = |i: usize| {
                h.get(i..i + 2)
                    .and_then(|b| u8::from_str_radix(b, 16).ok())
                    .map(|b| b as f64 / 255.0)
            };
            let parsed = match h.len() {
                6 => (0..3)
                    .map(|k| byte(k * 2))
                    .chain([Some(1.0)])
                    .collect::<Option<Vec<f64>>>(),
                8 => (0..4).map(|k| byte(k * 2)).collect(),
                _ => None,
            };
            parsed
                .ok_or_else(|| serde::de::Error::custom(format!("key value `{s}` is not a number, list or #hex color")))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    #[default]
    Linear,
    Hold,
    InQuad,
    OutQuad,
    InOutQuad,
    InCubic,
    OutCubic,
    InOutCubic,
    /// CSS-style cubic bezier with control points (x1, y1, x2, y2).
    Bezier(f64, f64, f64, f64),
    InSine,
    OutSine,
    InOutSine,
    InExpo,
    OutExpo,
    InOutExpo,
    /// Overshoots the target, then settles.
    OutBack,
    /// Pulls back before leaving.
    InBack,
    InOutBack,
    OutElastic,
    OutBounce,
    /// A damped spring that reaches the target by the next key:
    /// (damping ratio, oscillations within the segment). `"spring"` alone is
    /// (0.45, 2).
    Spring(f64, f64),
}

impl Ease {
    pub fn apply(self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Ease::Linear => u,
            Ease::Hold => 0.0,
            Ease::InQuad => u * u,
            Ease::OutQuad => 1.0 - (1.0 - u) * (1.0 - u),
            Ease::InOutQuad => {
                if u < 0.5 {
                    2.0 * u * u
                } else {
                    1.0 - (-2.0 * u + 2.0).powi(2) / 2.0
                }
            }
            Ease::InCubic => u * u * u,
            Ease::OutCubic => 1.0 - (1.0 - u).powi(3),
            Ease::InOutCubic => {
                if u < 0.5 {
                    4.0 * u * u * u
                } else {
                    1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
                }
            }
            Ease::Bezier(x1, y1, x2, y2) => bezier(u, x1, y1, x2, y2),
            Ease::InSine => 1.0 - (u * std::f64::consts::FRAC_PI_2).cos(),
            Ease::OutSine => (u * std::f64::consts::FRAC_PI_2).sin(),
            Ease::InOutSine => -((std::f64::consts::PI * u).cos() - 1.0) / 2.0,
            Ease::InExpo => {
                if u <= 0.0 {
                    0.0
                } else {
                    2f64.powf(10.0 * u - 10.0)
                }
            }
            Ease::OutExpo => {
                if u >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f64.powf(-10.0 * u)
                }
            }
            Ease::InOutExpo => match u {
                u if u <= 0.0 => 0.0,
                u if u >= 1.0 => 1.0,
                u if u < 0.5 => 2f64.powf(20.0 * u - 10.0) / 2.0,
                u => (2.0 - 2f64.powf(-20.0 * u + 10.0)) / 2.0,
            },
            Ease::OutBack => {
                let (c1, c3) = (1.70158, 2.70158);
                1.0 + c3 * (u - 1.0).powi(3) + c1 * (u - 1.0).powi(2)
            }
            Ease::InBack => {
                let (c1, c3) = (1.70158, 2.70158);
                c3 * u * u * u - c1 * u * u
            }
            Ease::InOutBack => {
                let c2 = 1.70158 * 1.525;
                if u < 0.5 {
                    ((2.0 * u).powi(2) * ((c2 + 1.0) * 2.0 * u - c2)) / 2.0
                } else {
                    ((2.0 * u - 2.0).powi(2) * ((c2 + 1.0) * (u * 2.0 - 2.0) + c2) + 2.0) / 2.0
                }
            }
            Ease::OutElastic => {
                if u <= 0.0 || u >= 1.0 {
                    u
                } else {
                    2f64.powf(-10.0 * u) * ((u * 10.0 - 0.75) * (2.0 * std::f64::consts::PI / 3.0)).sin() + 1.0
                }
            }
            Ease::OutBounce => {
                let (n1, d1) = (7.5625, 2.75);
                if u < 1.0 / d1 {
                    n1 * u * u
                } else if u < 2.0 / d1 {
                    let v = u - 1.5 / d1;
                    n1 * v * v + 0.75
                } else if u < 2.5 / d1 {
                    let v = u - 2.25 / d1;
                    n1 * v * v + 0.9375
                } else {
                    let v = u - 2.625 / d1;
                    n1 * v * v + 0.984375
                }
            }
            Ease::Spring(zeta, cycles) => spring(u, zeta, cycles),
        }
    }
}

/// Step response of a damped spring over one segment, corrected so it
/// lands exactly on the target at the end.
fn spring(u: f64, zeta: f64, cycles: f64) -> f64 {
    let zeta = zeta.clamp(0.05, 2.0);
    let w = 2.0 * std::f64::consts::PI * cycles.max(0.1);
    let x = |t: f64| {
        if zeta < 1.0 {
            let wd = w * (1.0 - zeta * zeta).sqrt();
            1.0 - (-zeta * w * t).exp() * ((wd * t).cos() + zeta * w / wd * (wd * t).sin())
        } else {
            1.0 - (-w * t).exp() * (1.0 + w * t)
        }
    };
    x(u) + (1.0 - x(1.0)) * u
}

fn bezier(x: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let coord = |t: f64, a: f64, b: f64| {
        let mt = 1.0 - t;
        3.0 * mt * mt * t * a + 3.0 * mt * t * t * b + t * t * t
    };
    // Newton iterations on x(t) = x, with bisection fallback.
    let mut t = x;
    for _ in 0..8 {
        let err = coord(t, x1, x2) - x;
        if err.abs() < 1e-7 {
            return coord(t, y1, y2);
        }
        let mt = 1.0 - t;
        let d = 3.0 * mt * mt * x1 + 6.0 * mt * t * (x2 - x1) + 3.0 * t * t * (1.0 - x2);
        if d.abs() < 1e-6 {
            break;
        }
        t -= err / d;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    t = x;
    for _ in 0..40 {
        let xv = coord(t, x1, x2);
        if (xv - x).abs() < 1e-7 {
            break;
        }
        if xv < x {
            lo = t;
        } else {
            hi = t;
        }
        t = 0.5 * (lo + hi);
    }
    coord(t, y1, y2)
}

/// [`sample`] without allocating for up to 16 values.
pub(crate) fn sample_vals(keys: &[Key], t: f64) -> Option<crate::Vals> {
    let first = keys.first()?;
    if keys.len() == 1 || t <= first.t {
        return Some(crate::Vals::from_slice(&first.v));
    }
    let last = keys.last().unwrap();
    if t >= last.t {
        return Some(crate::Vals::from_slice(&last.v));
    }
    let i = keys.partition_point(|k| k.t <= t) - 1;
    let (a, b) = (&keys[i], &keys[i + 1]);
    let span = b.t - a.t;
    let u = if span > 0.0 {
        a.ease.apply((t - a.t) / span)
    } else {
        1.0
    };
    let n = a.v.len().min(b.v.len());
    Some(crate::Vals::from_fn(n, |k| a.v[k] + (b.v[k] - a.v[k]) * u))
}

/// Samples a key list at `t`. Keys are assumed sorted by time.
pub(crate) fn sample(keys: &[Key], t: f64) -> Option<Vec<f64>> {
    let first = keys.first()?;
    if keys.len() == 1 || t <= first.t {
        return Some(first.v.clone());
    }
    let last = keys.last().unwrap();
    if t >= last.t {
        return Some(last.v.clone());
    }
    let i = keys.partition_point(|k| k.t <= t) - 1;
    let (a, b) = (&keys[i], &keys[i + 1]);
    let span = b.t - a.t;
    let u = if span > 0.0 {
        a.ease.apply((t - a.t) / span)
    } else {
        1.0
    };
    Some(a.v.iter().zip(b.v.iter()).map(|(x, y)| x + (y - x) * u).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_linear() {
        let e = Ease::Bezier(0.25, 0.25, 0.75, 0.75);
        assert!((e.apply(0.3) - 0.3).abs() < 1e-5);
        assert!((Ease::Bezier(0.4, 0.0, 0.2, 1.0).apply(1.0) - 1.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod ease_tests {
    use super::*;

    #[test]
    fn springs_and_backs_land_on_target_after_overshooting() {
        for e in [
            Ease::Spring(0.45, 2.0),
            Ease::Spring(0.2, 3.0),
            Ease::Spring(1.2, 1.0),
            Ease::OutBack,
            Ease::OutElastic,
            Ease::OutBounce,
        ] {
            assert!(e.apply(0.0).abs() < 1e-9, "{e:?} at 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-9, "{e:?} at 1");
        }
        let peak = (1..100)
            .map(|i| Ease::Spring(0.45, 2.0).apply(i as f64 / 100.0))
            .fold(0.0, f64::max);
        assert!(peak > 1.05, "spring overshoots: {peak}");
        assert!(Ease::OutBack.apply(0.7) > 1.0);
    }

    #[test]
    fn eases_parse_by_name_and_with_params() {
        let k: Key = serde_json::from_str(r#"{"t": 0, "v": 1, "ease": "spring"}"#).unwrap();
        assert_eq!(k.ease, Ease::Spring(0.45, 2.0));
        let k: Key = serde_json::from_str(r#"{"t": 0, "v": 1, "ease": {"spring": [0.3, 3]}}"#).unwrap();
        assert_eq!(k.ease, Ease::Spring(0.3, 3.0));
        let k: Key = serde_json::from_str(r#"{"t": 0, "v": 1, "ease": "out_back"}"#).unwrap();
        assert_eq!(k.ease, Ease::OutBack);
        let k: Key = serde_json::from_str(r#"{"t": 0, "v": 1, "ease": {"bezier": [0.16, 1, 0.3, 1]}}"#).unwrap();
        assert_eq!(k.ease, Ease::Bezier(0.16, 1.0, 0.3, 1.0));
        assert!(serde_json::from_str::<Key>(r#"{"t": 0, "v": 1, "ease": "wobbly"}"#).is_err());
    }
}
