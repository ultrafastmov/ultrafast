//! A parameter's value at a time without a heap allocation: frames evaluate
//! every animated parameter of every live node (eight times per frame under
//! motion blur), and allocating a `Vec` for each dominated encoding.

use std::ops::{Deref, DerefMut};

const INLINE: usize = 16;

/// Up to 16 numbers inline, more on the heap. Derefs to `[f64]`.
#[derive(Debug, Clone)]
pub enum Vals {
    Inline { len: u8, v: [f64; INLINE] },
    Heap(Vec<f64>),
}

impl Vals {
    pub fn from_slice(s: &[f64]) -> Self {
        if s.len() <= INLINE {
            let mut v = [0.0; INLINE];
            v[..s.len()].copy_from_slice(s);
            Vals::Inline { len: s.len() as u8, v }
        } else {
            Vals::Heap(s.to_vec())
        }
    }

    pub fn filled(x: f64, n: usize) -> Self {
        if n <= INLINE {
            Vals::Inline {
                len: n as u8,
                v: [x; INLINE],
            }
        } else {
            Vals::Heap(vec![x; n])
        }
    }

    /// `n` values computed by `f(i)`.
    pub fn from_fn(n: usize, mut f: impl FnMut(usize) -> f64) -> Self {
        if n <= INLINE {
            let mut v = [0.0; INLINE];
            for (i, x) in v.iter_mut().take(n).enumerate() {
                *x = f(i);
            }
            Vals::Inline { len: n as u8, v }
        } else {
            Vals::Heap((0..n).map(f).collect())
        }
    }

    /// Pads with `fill` or truncates to `n` values.
    pub fn resize(&mut self, n: usize, fill: f64) {
        match self {
            Vals::Inline { len, v } if n <= INLINE => {
                for x in v.iter_mut().take(n).skip(*len as usize) {
                    *x = fill;
                }
                *len = n as u8;
            }
            Vals::Heap(h) => h.resize(n, fill),
            Vals::Inline { .. } => {
                let mut h = self.to_vec();
                h.resize(n, fill);
                *self = Vals::Heap(h);
            }
        }
    }
}

impl Deref for Vals {
    type Target = [f64];
    fn deref(&self) -> &[f64] {
        match self {
            Vals::Inline { len, v } => &v[..*len as usize],
            Vals::Heap(h) => h,
        }
    }
}

impl DerefMut for Vals {
    fn deref_mut(&mut self) -> &mut [f64] {
        match self {
            Vals::Inline { len, v } => &mut v[..*len as usize],
            Vals::Heap(h) => h,
        }
    }
}

impl IntoIterator for Vals {
    type Item = f64;
    type IntoIter = ValsIter;
    fn into_iter(self) -> ValsIter {
        ValsIter { vals: self, i: 0 }
    }
}

pub struct ValsIter {
    vals: Vals,
    i: usize,
}

impl Iterator for ValsIter {
    type Item = f64;
    fn next(&mut self) -> Option<f64> {
        let x = self.vals.get(self.i).copied();
        self.i += 1;
        x
    }
}
