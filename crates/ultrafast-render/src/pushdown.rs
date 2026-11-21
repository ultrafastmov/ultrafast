//! Transforms pushed down through composites, so a camera over a whole
//! composition keeps type and vectors sharp.
//!
//! A `transform` over a composite resamples the composite's pixels: zoom a
//! camera in on a scene and its type goes soft. But an affine transform of
//! a pixelwise composite equals the composite of the transformed layers
//! (T(over(a, b)) = over(T(a), T(b)), and likewise for blends, masks and
//! color ops), and a transform directly on a text, path or shape node is
//! absorbed into its geometry and drawn at full resolution. So before
//! planning, each such transform is copied down onto the layers of the
//! composite it moves. Blurs, shaders, footage and other images stop the
//! descent: those keep the transform over them, which resamples them as
//! before.
//!
//! A composite holding more footage (images, video) than one kernel can
//! bind is pushed through too, even without vector layers: the planner
//! has to split it into several passes, and a pass rendered below the
//! transform would be drawn where the layers sit before it moves them,
//! mostly outside the frame (a camera flying over many plates showed
//! nothing). With the transform on each layer every pass is drawn moved.

use std::collections::{BTreeMap, HashMap};
use ultrafast_graph::{Document, Flat, Node, NodeId, Timing};

/// Pixelwise ops: transforming their inputs is the same as transforming
/// their output.
fn distributes(op: &str) -> bool {
    matches!(
        op,
        "over"
            | "blend"
            | "composite"
            | "mix"
            | "mask"
            | "opacity"
            | "adjust"
            | "levels"
            | "tint"
            | "color_matrix"
            | "invert"
            | "threshold"
    )
}

/// Vector layers that draw a transform into their geometry.
fn vector(op: &str) -> bool {
    matches!(op, "text" | "path" | "shape")
}

fn has_vector(nodes: &BTreeMap<NodeId, Node>, id: &str, seen: &mut HashMap<String, bool>) -> bool {
    if let Some(&v) = seen.get(id) {
        return v;
    }
    seen.insert(id.to_string(), false);
    let Some(n) = nodes.get(id) else { return false };
    let v = vector(&n.op)
        || (distributes(&n.op) && n.inputs.values().any(|s| has_vector(nodes, s, seen)))
        || (n.op == "transform" && n.inputs.values().any(|s| has_vector(nodes, s, seen)));
    seen.insert(id.to_string(), v);
    v
}

/// Footage a kernel cannot hold under one transform: more images and
/// videos than its texture budget (one slot stays for its output).
const FOOTAGE_BUDGET: usize = crate::plan::MAX_TEXTURES - 1;

/// Images and videos reached from `id` through composites and transforms.
fn footage(nodes: &BTreeMap<NodeId, Node>, id: &str, memo: &mut HashMap<String, usize>) -> usize {
    if let Some(&v) = memo.get(id) {
        return v;
    }
    memo.insert(id.to_string(), 0);
    let Some(n) = nodes.get(id) else { return 0 };
    let v = if matches!(n.op.as_str(), "image" | "video") {
        1
    } else if distributes(&n.op) || n.op == "transform" {
        n.inputs.values().map(|s| footage(nodes, s, memo)).sum()
    } else {
        0
    };
    memo.insert(id.to_string(), v);
    v
}

/// Whether a transform over the composite `id` should move its layers:
/// it holds vector layers (drawn sharp when moved), or more footage than
/// one pass can bind.
fn worth_moving(
    nodes: &BTreeMap<NodeId, Node>,
    id: &str,
    seen: &mut HashMap<String, bool>,
    counts: &mut HashMap<String, usize>,
) -> bool {
    has_vector(nodes, id, seen) || footage(nodes, id, counts) > FOOTAGE_BUDGET
}

/// Whether any transform in `doc` moves a composite worth pushing through.
pub fn wanted(doc: &Document) -> bool {
    let mut seen = HashMap::new();
    let mut counts = HashMap::new();
    doc.nodes.values().any(|n| {
        n.op == "transform"
            && n.inputs.get("source").is_some_and(|s| {
                doc.nodes.get(s).is_some_and(|m| distributes(&m.op))
                    && worth_moving(&doc.nodes, s, &mut seen, &mut counts)
            })
    })
}

struct Push<'a> {
    nodes: &'a mut BTreeMap<NodeId, Node>,
    chains: &'a mut HashMap<NodeId, Vec<Timing>>,
    seen: HashMap<String, bool>,
    counts: HashMap<String, usize>,
}

impl Push<'_> {
    /// The id of a node that shows `n` moved by the transform `t` (a copy of
    /// `t` over `n`, or a copy of the composite `n` over moved inputs).
    fn push(&mut self, t: &str, tnode: &Node, n: &str, memo: &mut HashMap<String, String>) -> String {
        if let Some(id) = memo.get(n) {
            return id.clone();
        }
        let node = self.nodes[n].clone();
        let worth = distributes(&node.op) && worth_moving(self.nodes, n, &mut self.seen, &mut self.counts);
        let id = format!("{t}~{n}");
        if worth {
            let mut copy = node.clone();
            for (port, src) in node.inputs.iter() {
                let moved = self.push(t, tnode, src, memo);
                copy.inputs.insert(port.clone(), moved);
            }
            if let Some(c) = self.chains.get(n).cloned() {
                self.chains.insert(id.clone(), c);
            }
            self.nodes.insert(id.clone(), copy);
        } else {
            let mut copy = tnode.clone();
            copy.inputs.insert("source".into(), n.to_string());
            if let Some(c) = self.chains.get(t).cloned() {
                self.chains.insert(id.clone(), c);
            }
            self.nodes.insert(id.clone(), copy);
        }
        memo.insert(n.to_string(), id.clone());
        id
    }
}

/// Pushes every transform that moves a composite with vector layers (or
/// more footage than one pass binds) down onto that composite's layers. The transform's own id then holds the
/// moved composite, so its consumers are unchanged.
pub fn distribute(flat: &mut Flat) {
    let doc = &flat.doc;
    let mut seen = HashMap::new();
    let mut counts = HashMap::new();
    // Inner transforms first (sources before consumers), so an outer one
    // reaches layers an inner one has already moved.
    let order = doc
        .reachable_topo_order()
        .unwrap_or_else(|_| doc.nodes.keys().cloned().collect());
    let targets: Vec<(String, String)> = order
        .iter()
        .filter_map(|id| {
            let n = &doc.nodes[id];
            let s = n.inputs.get("source").filter(|_| n.op == "transform")?;
            (doc.nodes.get(s).is_some_and(|m| distributes(&m.op))
                && worth_moving(&doc.nodes, s, &mut seen, &mut counts))
            .then(|| (id.clone(), s.clone()))
        })
        .collect();
    let mut p = Push {
        nodes: &mut flat.doc.nodes,
        chains: &mut flat.chains,
        seen: HashMap::new(),
        counts: HashMap::new(),
    };
    for (t, s) in targets {
        let tnode = p.nodes[&t].clone();
        let mut memo = HashMap::new();
        let moved = p.push(&t, &tnode, &s, &mut memo);
        // The transform's id becomes the moved composite. Outside the
        // transform's window its copies pass their layers through, so the
        // result is the unmoved composite, as before.
        let root = p.nodes.remove(&moved).expect("pushed node");
        let root_chain = p.chains.remove(&moved);
        p.nodes.insert(t.clone(), root);
        match root_chain {
            Some(c) => {
                p.chains.insert(t.clone(), c);
            }
            None => {
                p.chains.remove(&t);
            }
        }
    }
}
