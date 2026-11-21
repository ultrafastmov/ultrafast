//! Vector geometry and text on the CPU: paths (SVG data, presets, strokes,
//! trims, morphs) and text (font lookup, shaping, layout, measuring) built
//! into flattened geometry for the GPU rasterizer ([`geom`]), inline text
//! markup ([`markup`]) and fonts fetched by name ([`fontfetch`]).
//!
//! The renderer (ultrafast-render) rasterizes this geometry and re-exports
//! these modules at their old paths; tools that only measure or lay out
//! type use them without a GPU.

// As in the renderer this code grew in: glyph runs travel as tuples, and
// `Affine::mul` is a plain method.
#![allow(clippy::type_complexity, clippy::should_implement_trait)]

pub mod fontfetch;
pub mod geom;
pub mod lru;
pub mod markup;

use ultrafast_graph::{Node, NodeId, Param};

/// Why a node's geometry could not be built (ultrafast-render reports it as
/// the `RenderError` variant of the same name).
#[derive(Debug, thiserror::Error)]
pub enum GeomError {
    #[error("node `{node}` param `{param}`: {reason}")]
    BadParam {
        node: NodeId,
        param: String,
        reason: String,
    },
}

// Helpers shared with the renderer, which re-exports them from `ops` and
// `images`.

pub fn text<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    match node.params.get(name) {
        Some(Param::Text(s)) => Some(s.as_str()),
        _ => None,
    }
}

pub fn parse_hex(s: &str) -> Option<[f64; 4]> {
    let h = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|b| b as f64 / 255.0);
    let nib = |i: usize| u8::from_str_radix(h.get(i..i + 1)?, 16).ok().map(|b| b as f64 / 15.0);
    match h.len() {
        3 => Some([nib(0)?, nib(1)?, nib(2)?, 1.0]),
        4 => Some([nib(0)?, nib(1)?, nib(2)?, nib(3)?]),
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 1.0]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

pub fn srgb_to_linear(x: f64) -> f64 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// 2D affine `[a, b, c, d, e, f]`: x' = a x + c y + e, y' = b x + d y + f.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f64, y: f64) -> Self {
        Affine([1.0, 0.0, 0.0, 1.0, x, y])
    }
    pub fn scale(x: f64, y: f64) -> Self {
        Affine([x, 0.0, 0.0, y, 0.0, 0.0])
    }
    pub fn rotate(r: f64) -> Self {
        let (s, c) = r.sin_cos();
        Affine([c, s, -s, c, 0.0, 0.0])
    }
    pub fn skew_x(r: f64) -> Self {
        Affine([1.0, 0.0, r.tan(), 1.0, 0.0, 0.0])
    }
    pub fn mul(self, o: Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = o.0;
        Affine([
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ])
    }
    pub fn det(self) -> f64 {
        self.0[0] * self.0[3] - self.0[1] * self.0[2]
    }
    pub fn inverse(self) -> Option<Affine> {
        let det = self.det();
        if det.abs() < 1e-12 {
            return None;
        }
        let [a, b, c, d, e, f] = self.0;
        let inv = 1.0 / det;
        Some(Affine([
            d * inv,
            -b * inv,
            -c * inv,
            a * inv,
            (c * f - d * e) * inv,
            (b * e - a * f) * inv,
        ]))
    }
    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }
}

/// IEEE half from f32 (round to nearest even).
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32 - 127 + 15;
    let mant = x & 0x7f_ffff;
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - exp);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    if exp >= 31 {
        return sign | 0x7c00;
    }
    let h = sign as u32 | ((exp as u32) << 10) | (mant >> 13);
    let rem = mant & 0x1fff;
    (if rem > 0x1000 || (rem == 0x1000 && h & 1 == 1) {
        h + 1
    } else {
        h
    }) as u16
}
