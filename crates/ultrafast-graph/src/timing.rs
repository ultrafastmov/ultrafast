//! Node timing: visibility windows and local time.
//!
//! Any node may carry a `timing` object. Its window (`in`, `out`) is in the
//! time of the graph the node lives in. Outside it the node is inactive:
//! composites pass their background through, effects their source, and
//! generators go transparent (layer semantics).
//! Its params (keyframes, video playheads) are evaluated in *local* time:
//! `(t - offset) * speed`, wrapped by `loop` if set.
//!
//! Components (precomps) apply their instance's timing to every node inside
//! them, so a node's effective timing is a chain from the outermost instance
//! to the node itself.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    /// Visible from this time (inclusive). None: from the start.
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub in_point: Option<f64>,
    /// Visible until this time (exclusive). None: to the end.
    #[serde(rename = "out", default, skip_serializing_if = "Option::is_none")]
    pub out_point: Option<f64>,
    /// Local time zero, in parent time.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f64,
    /// Local seconds per parent second (negative plays backwards).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub speed: f64,
    /// Local time wraps at this duration.
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub loop_duration: Option<f64>,
}

fn one() -> f64 {
    1.0
}
fn is_one(v: &f64) -> bool {
    *v == 1.0
}
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            in_point: None,
            out_point: None,
            offset: 0.0,
            speed: 1.0,
            loop_duration: None,
        }
    }
}

impl Timing {
    pub fn has_window(&self) -> bool {
        self.in_point.is_some() || self.out_point.is_some()
    }

    pub fn contains(&self, t: f64) -> bool {
        self.in_point.is_none_or(|i| t >= i) && self.out_point.is_none_or(|o| t < o)
    }

    pub fn local(&self, t: f64) -> f64 {
        let l = (t - self.offset) * self.speed;
        match self.loop_duration {
            Some(d) if d > 0.0 => l.rem_euclid(d),
            _ => l,
        }
    }
}

/// Evaluates a timing chain (outermost first) at parent time `t`. Returns
/// the node's local time, or None if any level's window excludes it.
pub fn resolve_chain(chain: &[Timing], t: f64) -> Option<f64> {
    let mut tau = t;
    for timing in chain {
        if !timing.contains(tau) {
            return None;
        }
        tau = timing.local(tau);
    }
    Some(tau)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_composes_windows_and_offsets() {
        let outer = Timing {
            in_point: Some(2.0),
            out_point: Some(6.0),
            offset: 2.0,
            ..Default::default()
        };
        let inner = Timing {
            in_point: Some(1.0),
            speed: 2.0,
            ..Default::default()
        };
        assert_eq!(resolve_chain(&[outer, inner], 1.0), None); // before outer in
        assert_eq!(resolve_chain(&[outer, inner], 2.5), None); // outer local 0.5 < inner in
        assert_eq!(resolve_chain(&[outer, inner], 4.0), Some(4.0)); // local 2.0 * 2
        assert_eq!(resolve_chain(&[outer, inner], 6.0), None); // outer out
    }

    #[test]
    fn loop_wraps() {
        let t = Timing {
            loop_duration: Some(2.0),
            ..Default::default()
        };
        assert!((t.local(5.5) - 1.5).abs() < 1e-12);
    }
}
