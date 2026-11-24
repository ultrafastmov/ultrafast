//! ultrafast document model.
//!
//! A [`Document`] is a procedural node graph addressed by stable string ids.
//! It has no GPU dependency: documents can be parsed, validated, edited and
//! diffed anywhere. All mutation goes through [`Edit`] so that every change is
//! serializable and classifiable as param-only or structural.

mod canonical;
mod curve;
pub mod driver;
mod edit;
pub mod expr;
mod flatten;
mod timing;
mod vals;

pub use canonical::canonical_json;
pub use curve::{Ease, Key};
pub use edit::{Change, Edit, EditError};
pub use flatten::{Component, Flat};
pub use timing::{Timing, resolve_chain};
pub use vals::Vals;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type NodeId = String;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Seconds.
    pub duration: f64,
    /// Temporal supersampling: each output frame averages `samples` renders
    /// spread over `shutter` frames (0.5 = a 180 degree shutter), centered on
    /// the frame time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<MotionBlur>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MotionBlur {
    pub samples: u32,
    #[serde(default = "half")]
    pub shutter: f64,
    /// More samples where things move fast, up to this many: a frame takes
    /// one per [`MotionBlur::SPACING`] px its fastest layer moves over the
    /// shutter, never fewer than `samples`, so a whip pan blurs smooth
    /// instead of in steps while slow frames stay cheap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_samples: Option<u32>,
}

fn half() -> f64 {
    0.5
}

impl MotionBlur {
    /// Px of motion one sample covers when the count adapts.
    pub const SPACING: f64 = 2.0;

    /// Samples for a frame whose fastest layer moves `px` over the shutter.
    pub fn samples_for(&self, px: f64) -> u32 {
        match self.max_samples {
            Some(max) if max > self.samples => {
                let want = (px / Self::SPACING - 1e-6).ceil();
                if want.is_nan() {
                    self.samples
                } else {
                    (want.min(max as f64) as u32).clamp(self.samples, max)
                }
            }
            _ => self.samples,
        }
    }
}

impl Format {
    pub fn frame_count(&self) -> u64 {
        (self.duration * self.fps).round().max(0.0) as u64
    }

    pub fn time_of(&self, frame: u64) -> f64 {
        frame as f64 / self.fps
    }
}

/// A parameter value as written in the document.
///
/// Numbers and vectors are plain JSON. An animated param is an object with a
/// `keys` array; any param can become animated without changing its node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Param {
    Number(f64),
    Vector(Vec<f64>),
    Text(String),
    Bool(bool),
    Animated {
        keys: Vec<Key>,
        /// After the last key: start over, or play back and forth.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeat: Option<driver::Repeat>,
    },
    Wiggle {
        wiggle: driver::Wiggle,
    },
    Oscillate {
        oscillate: driver::Oscillate,
    },
    Audio {
        audio: driver::AudioDriver,
    },
    /// Computed from time: `{"expr": "540 + 200 * sin(t * 3)"}`, or a list
    /// of expressions for a vector.
    Expr {
        expr: expr::Expr,
    },
}

impl Param {
    /// Whether the value changes with time.
    pub fn is_animated(&self) -> bool {
        match self {
            Param::Animated { keys, .. } => keys.len() > 1,
            Param::Wiggle { .. } | Param::Oscillate { .. } | Param::Audio { .. } | Param::Expr { .. } => true,
            _ => false,
        }
    }

    /// Evaluates the param at time `t` (seconds) into a numeric vector.
    /// Returns `None` for text and bool params.
    /// Like [`Param::eval`], without allocating for up to 16 values.
    pub fn eval_vals(&self, t: f64) -> Option<Vals> {
        match self {
            Param::Number(n) => Some(Vals::from_slice(&[*n])),
            Param::Vector(v) => Some(Vals::from_slice(v)),
            Param::Animated { keys, repeat } => match (repeat, keys.first(), keys.last()) {
                (Some(r), Some(a), Some(b)) => curve::sample_vals(keys, driver::repeat_time(t, a.t, b.t, *r)),
                _ => curve::sample_vals(keys, t),
            },
            Param::Text(_) | Param::Bool(_) => None,
            other => other.eval(t).map(Vals::Heap),
        }
    }

    pub fn eval(&self, t: f64) -> Option<Vec<f64>> {
        match self {
            Param::Number(n) => Some(vec![*n]),
            Param::Vector(v) => Some(v.clone()),
            Param::Animated { keys, repeat } => match (repeat, keys.first(), keys.last()) {
                (Some(r), Some(a), Some(b)) => curve::sample(keys, driver::repeat_time(t, a.t, b.t, *r)),
                _ => curve::sample(keys, t),
            },
            Param::Wiggle { wiggle } => Some(wiggle.eval(t)),
            Param::Oscillate { oscillate } => Some(oscillate.eval(t)),
            Param::Audio { audio } => Some(vec![audio.range.first().copied().unwrap_or(0.0)]),
            Param::Expr { expr } => Some(expr.eval(t)),
            Param::Text(_) | Param::Bool(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Node {
    /// Operation name, for example `solid`, `shape`, `over`.
    pub op: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Param>,
    /// Named image inputs, mapping port name to source node id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, NodeId>,
    /// Visibility window and local time (see [`Timing`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<Timing>,
}

impl Node {
    pub fn is_animated(&self) -> bool {
        self.params.values().any(Param::is_animated)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub version: u32,
    pub format: Format,
    pub output: NodeId,
    pub nodes: BTreeMap<NodeId, Node>,
    /// Reusable sub-graphs; a node whose `op` names one is an instance.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, Component>,
    /// Bumped on every applied edit batch. Not serialized.
    #[serde(skip)]
    pub revision: u64,
    /// Bumped only on structural edits. Plans are keyed by this.
    #[serde(skip)]
    pub structure_revision: u64,
    /// Identity of this document instance (unique per process, not
    /// serialized). Together with `revision` it tells a renderer whether it
    /// has already seen exactly this content. Mutate documents through
    /// [`Document::apply`] so the revision tracks every change.
    #[serde(skip, default = "next_instance")]
    pub instance: u64,
}

/// Content equality: version, format, output and nodes. Revisions and
/// instance identity are bookkeeping and do not take part.
impl PartialEq for Document {
    fn eq(&self, o: &Self) -> bool {
        self.version == o.version
            && self.format == o.format
            && self.output == o.output
            && self.nodes == o.nodes
            && self.components == o.components
    }
}

fn next_instance() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("output node `{0}` does not exist")]
    MissingOutput(NodeId),
    #[error("node `{node}` input `{port}` references missing node `{target}`")]
    MissingInput { node: NodeId, port: String, target: NodeId },
    #[error("cycle through node `{0}`")]
    Cycle(NodeId),
    #[error("component: {0}")]
    Component(String),
    /// A param value that is none of the param forms, located.
    #[error(
        "{at} param `{param}`: {value} is not a param value (a number, vector, color or other text, bool, keys, expr, wiggle, oscillate or audio)"
    )]
    BadParam { at: String, param: String, value: String },
}

impl Document {
    pub fn from_json(s: &str) -> Result<Self, DocumentError> {
        let doc: Document = match serde_json::from_str(s) {
            Ok(d) => d,
            // serde's message for a bad param names neither the node nor the
            // param (and a one-line document's column says little): find it.
            Err(e) => return Err(locate_bad_param(s).unwrap_or(DocumentError::Parse(e))),
        };
        doc.validate()?;
        Ok(doc)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("document serializes")
    }

    /// A copy that renderers treat as a document of its own. A plain clone
    /// keeps the instance, so two clones edited the same number of times
    /// would look to a renderer like content it has already seen: make
    /// variants of one document (a video per row) with this.
    pub fn fork(&self) -> Document {
        let mut d = self.clone();
        d.instance = next_instance();
        d
    }
}

/// The first param in a document's text (its nodes', or its components')
/// that does not parse as a [`Param`].
/// Keys given twice in one object of a JSON document, as paths
/// (`nodes.title.params.position`). Parsing keeps the last of them without
/// a word, so a param written twice loses its first value.
pub fn duplicate_keys(json: &str) -> Vec<String> {
    use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
    use std::cell::RefCell;
    struct Walk<'a> {
        path: String,
        found: &'a RefCell<Vec<String>>,
    }
    impl<'de> DeserializeSeed<'de> for Walk<'_> {
        type Value = ();
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Walk<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("JSON")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<(), A::Error> {
            let mut seen = std::collections::HashSet::new();
            while let Some(k) = m.next_key::<String>()? {
                let path = if self.path.is_empty() {
                    k.clone()
                } else {
                    format!("{}.{k}", self.path)
                };
                if !seen.insert(k) {
                    self.found.borrow_mut().push(path.clone());
                }
                m.next_value_seed(Walk {
                    path,
                    found: self.found,
                })?;
            }
            Ok(())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<(), A::Error> {
            let mut i = 0;
            while s
                .next_element_seed(Walk {
                    path: format!("{}[{i}]", self.path),
                    found: self.found,
                })?
                .is_some()
            {
                i += 1;
            }
            Ok(())
        }
        fn visit_bool<E>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E>(self) -> Result<(), E> {
            Ok(())
        }
    }
    let found = RefCell::new(Vec::new());
    let mut de = serde_json::Deserializer::from_str(json);
    let _ = Walk {
        path: String::new(),
        found: &found,
    }
    .deserialize(&mut de);
    found.into_inner()
}

fn locate_bad_param(s: &str) -> Option<DocumentError> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let bad = |at: String, nodes: &serde_json::Value| -> Option<DocumentError> {
        for (id, node) in nodes.as_object()? {
            for (k, p) in node
                .get("params")
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
            {
                if serde_json::from_value::<Param>(p.clone()).is_err() {
                    let mut value = p.to_string();
                    if value.len() > 120 {
                        value = format!("{}...", &value[..value.floor_char_boundary(117)]);
                    }
                    return Some(DocumentError::BadParam {
                        at: format!("{at}node `{id}`"),
                        param: k.clone(),
                        value,
                    });
                }
            }
        }
        None
    };
    if let Some(e) = v.get("nodes").and_then(|n| bad(String::new(), n)) {
        return Some(e);
    }
    for (name, c) in v
        .get("components")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(e) = c.get("nodes").and_then(|n| bad(format!("component `{name}` "), n)) {
            return Some(e);
        }
    }
    None
}

impl Document {
    pub fn validate(&self) -> Result<(), DocumentError> {
        if !self.nodes.contains_key(&self.output) {
            return Err(DocumentError::MissingOutput(self.output.clone()));
        }
        for (id, node) in &self.nodes {
            for (port, target) in &node.inputs {
                if !self.nodes.contains_key(target) {
                    return Err(DocumentError::MissingInput {
                        node: id.clone(),
                        port: port.clone(),
                        target: target.clone(),
                    });
                }
            }
        }
        self.reachable_topo_order().map(|_| ())
    }

    /// Nodes reachable from the output, dependencies first. Unreachable nodes
    /// are omitted: this is dead-node elimination.
    pub fn reachable_topo_order(&self) -> Result<Vec<NodeId>, DocumentError> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Visiting,
            Done,
        }
        let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();
        let mut order = Vec::new();
        // Iterative DFS so deep graphs cannot overflow the stack.
        let mut stack: Vec<(&str, bool)> = vec![(self.output.as_str(), false)];
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                marks.insert(id, Mark::Done);
                order.push(id.to_string());
                continue;
            }
            match marks.get(id) {
                Some(Mark::Done) => continue,
                Some(Mark::Visiting) => return Err(DocumentError::Cycle(id.to_string())),
                None => {}
            }
            marks.insert(id, Mark::Visiting);
            stack.push((id, true));
            let Some(node) = self.nodes.get(id) else {
                return Err(DocumentError::MissingOutput(id.to_string()));
            };
            for target in node.inputs.values() {
                match marks.get(target.as_str()) {
                    Some(Mark::Done) => {}
                    Some(Mark::Visiting) => return Err(DocumentError::Cycle(target.clone())),
                    None => stack.push((target.as_str(), false)),
                }
            }
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_given_twice_are_found() {
        let doc = r#"{"version": 1, "nodes": {"title": {"op": "text", "params": {"position": [1, 2], "text": "a",
            "position": [3, 4]}}, "list": {"op": "path", "params": {"points": [{"x": 1, "x": 2}]}}}}"#;
        assert_eq!(
            duplicate_keys(doc),
            ["nodes.title.params.position", "nodes.list.params.points[0].x"]
        );
        assert!(duplicate_keys(r#"{"a": {"b": 1}, "c": {"b": 2}}"#).is_empty());
        assert!(duplicate_keys("not json").is_empty());
    }

    #[test]
    fn a_bad_param_is_located() {
        let e = Document::from_json(
            r#"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":1},"output":"a",
            "nodes":{"a":{"op":"shape","params":{"scale":{"keys":[{"t":0,"v":[[1,1]]}]}}}}}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("node `a` param `scale`"), "{e}");
    }

    fn doc() -> Document {
        Document::from_json(
            r#"{
            "version": 1,
            "format": {"width": 64, "height": 64, "fps": 30, "duration": 1},
            "output": "comp",
            "nodes": {
                "bg": {"op": "solid", "params": {"color": [0,0,0,1]}},
                "unused": {"op": "solid"},
                "dot": {"op": "shape", "params": {"x": {"keys": [{"t":0,"v":[0]},{"t":1,"v":[10]}]}}},
                "comp": {"op": "over", "inputs": {"fg": "dot", "bg": "bg"}}
            }}"#,
        )
        .unwrap()
    }

    #[test]
    fn dead_nodes_are_not_planned() {
        let order = doc().reachable_topo_order().unwrap();
        assert!(!order.contains(&"unused".to_string()));
        assert_eq!(order.last().unwrap(), "comp");
    }

    #[test]
    fn cycles_are_rejected() {
        let mut d = doc();
        d.nodes.get_mut("bg").unwrap().inputs.insert("x".into(), "comp".into());
        assert!(matches!(d.validate(), Err(DocumentError::Cycle(_))));
    }

    #[test]
    fn animated_params_sample() {
        let d = doc();
        let p = &d.nodes["dot"].params["x"];
        assert_eq!(p.eval(0.5).unwrap(), vec![5.0]);
        assert!(d.nodes["dot"].is_animated());
    }
}
