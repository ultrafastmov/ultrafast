//! Graph -> render plan compiler.
//!
//! The plan partitions the nodes reachable from the output into **groups**.
//! Each group becomes one GPU kernel: its root node plus every upstream node
//! that can be evaluated inline (generators, per-pixel maps, warps) and has no
//! other consumer. Nodes that must exist as textures (the output, nodes with
//! several consumers, inputs of multi-tap ops like blur, agent shaders) are
//! group roots. This is operation fusion: a stack of 30 animated layers over a
//! background is one dispatch with no intermediate textures.
//!
//! Plans are rebuilt only when the document's *structure signature* changes
//! (ops, wiring, kernel variants, which params are animated). Param edits
//! reuse the plan and every compiled kernel unchanged.

use crate::RenderError;
use crate::codegen::{self, Child, SpecNode};
use crate::ops::{self, Form, OpDef};
use std::collections::HashMap;
use std::sync::Arc;
use ultrafast_graph::{Document, NodeId, Timing};

/// A static subtree at least this heavy is cached as its own texture rather
/// than re-evaluated inside an animated consumer every frame.
const HEAVY_STATIC: usize = 4;
/// Texture bindings available to one kernel (T0..T15).
pub const MAX_TEXTURES: usize = 16;

#[derive(Debug, Clone)]
pub struct PlanNode {
    pub id: NodeId,
    pub def: &'static OpDef,
    /// Timing chain (outermost first): local time and visibility windows.
    pub chain: Vec<Timing>,
    /// Has a visibility window somewhere in its chain.
    pub gated: bool,
    /// Connected input nodes, in port order.
    pub inputs: Vec<usize>,
    pub is_static: bool,
    pub variant: String,
    /// Agent code's big constant tables, packed after the node's params
    /// (its `variant` reads them there; see [`crate::tables`]).
    pub tables: Option<Arc<[f32]>>,
}

/// Where a kernel texture binding comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TexSource {
    /// Output of an earlier group.
    Group(usize),
    /// An external resource (image file, ...), keyed by its source string.
    Resource(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// One generated kernel.
    Fused,
    /// Two-pass separable blur of its single input.
    Blur,
    /// GPU coverage rasterization of vector geometry (path, text).
    Raster,
    /// 3D pass: meshes, lights and a camera rendered with depth and MSAA.
    Render3D,
    /// Particle sprites, drawn into a texture sized to their bounds.
    Particles,
    /// A simulation stepped once per frame from its own persistent state.
    Sim,
}

/// A raster node plus the transforms stacked directly on it, which are
/// applied to the geometry before rasterization instead of resampling.
#[derive(Debug, Clone)]
pub struct RasterChain {
    pub node: usize,
    /// Absorbed `transform` nodes, innermost first.
    pub transforms: Vec<usize>,
}

/// A run of layers ([`codegen::RunLayout`]) with plan node indices.
#[derive(Debug, Clone)]
pub struct PlanRun {
    pub stride: usize,
    pub payload: usize,
    /// (kind, composite node, layer): a texture binding or a generator node.
    pub layers: Vec<(usize, usize, codegen::LayerRef)>,
}

/// One piece of a group's param block ([`codegen::Slot`], with plan node
/// indices).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamSlot {
    /// A node's block: its op params, its window flag, its guard rect.
    Node(usize),
    /// Where run `r` (of [`Group::runs`]) starts and how many of its layers
    /// show this frame.
    Run(usize),
    Value(f32),
    /// Zeros.
    Pad(usize),
    /// Two floats per texture binding.
    Origins,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub kind: GroupKind,
    /// Plan node index of the root.
    pub root: usize,
    /// Every node evaluated in this group, root last.
    pub nodes: Vec<usize>,
    /// Texture bindings T0.., in kernel order.
    pub textures: Vec<TexSource>,
    /// What fills the param block after the 4-float frame header, in order.
    pub params: Vec<ParamSlot>,
    pub param_len: usize,
    /// The kernel may read any texture binding: bind all 16.
    pub pad_textures: bool,
    /// Runs of layers composited in a loop: their layers, with plan node
    /// indices.
    pub runs: Vec<PlanRun>,
    pub is_static: bool,
    /// Output texture slot.
    pub output: usize,
    /// Kernel identity: hash of the generated code. Equal keys share kernels.
    pub key: String,
    /// Generated Slang for the kernel body (entry named by `codegen::ENTRY`).
    pub body: String,
    pub raster: Option<RasterChain>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub signature: u64,
    pub nodes: Vec<PlanNode>,
    pub groups: Vec<Group>,
    /// For each node, the group it is the root of (usize::MAX if inlined).
    pub group_of: Vec<usize>,
    /// For each node, the one group it is part of ([`SHARED`] when several
    /// groups evaluate it, usize::MAX when none): an O(1) membership test.
    pub owner: Vec<usize>,
    pub slot_count: usize,
    pub output_slot: usize,
    /// With motion blur: the group holding every moving layer, after which
    /// only finishing looks follow, and which groups are those looks (run
    /// once per frame on the blurred image).
    pub mb_split: Option<usize>,
    pub post: Vec<bool>,
    /// Per group: bitset of the plan nodes its root depends on.
    pub group_up: Vec<Vec<u64>>,
    /// Per group: the groups its root depends on are exactly those before it.
    pub prefix: Vec<bool>,
    /// Per node: a generator only used as an overlay texture.
    pub overlay: Vec<bool>,
    /// Nodes present in the document but not reachable from the output.
    pub eliminated: Vec<NodeId>,
    /// Per group: its output is only ever read where it lands in the frame
    /// (per-pixel ops all the way to the output), so content outside the
    /// frame is never seen and need not be drawn.
    pub framed: Vec<bool>,
}

/// Hash of everything that shapes the plan. Cheap enough to compute after
/// every edit batch.
/// Timing chains of flattened nodes (see [`ultrafast_graph::Flat`]).
pub type Chains = HashMap<NodeId, Vec<Timing>>;

fn gated(chains: Option<&Chains>, id: &str) -> bool {
    chains
        .and_then(|c| c.get(id))
        .is_some_and(|c| c.iter().any(Timing::has_window))
}

pub fn signature(doc: &Document, chains: Option<&Chains>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    doc.output.hash(&mut h);
    (doc.format.width, doc.format.height).hash(&mut h);
    doc.format.motion_blur.is_some_and(|m| m.samples > 1).hash(&mut h);
    for (id, n) in &doc.nodes {
        id.hash(&mut h);
        n.op.hash(&mut h);
        n.inputs.hash(&mut h);
        // Kernel variant (gradient kind, shader code, image source) and which
        // params are animated shape the plan; param values do not.
        if let Ok(def) = ops::resolve(id, n) {
            def.func.hash(&mut h);
            ops::variant_key(def, n).hash(&mut h);
        }
        for (k, p) in &n.params {
            if p.is_animated() {
                k.hash(&mut h);
            }
        }
        // A reduced-resolution shader is its own group.
        ops::render_scale(n).is_some().hash(&mut h);
        gated(chains, id).hash(&mut h);
    }
    h.finish()
}

pub fn compile(doc: &Document, chains: Option<&Chains>) -> Result<Plan, RenderError> {
    let order = schedule(doc, &doc.reachable_topo_order()?);
    let index: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();

    let mut nodes: Vec<PlanNode> = Vec::with_capacity(order.len());
    for id in &order {
        let node = &doc.nodes[id];
        let def = ops::resolve(id, node)?;
        let ports = ops::connected_ports(def, id, node)?;
        let inputs: Vec<usize> = ports.iter().map(|p| index[node.inputs[p].as_str()]).collect();
        let gated = gated(chains, id);
        let is_static = !node.is_animated()
            && !ops::time_varying(def, node)
            && !gated
            && inputs.iter().all(|&i| nodes[i].is_static);
        // Video decoders and image sequences are per node (each has its own
        // playhead: timing, first_frame, fps), so their resource identity
        // includes the node id.
        let mut variant = if def.name == "video" || (def.name == "image" && ops::time_varying(def, node)) {
            format!("{}\u{1f}{id}", ops::variant_key(def, node))
        } else {
            ops::variant_key(def, node)
        };
        // Agent code's big tables are read from the params, not compiled in.
        let tables = if matches!(def.form, Form::Custom | Form::Sim) {
            crate::tables::hoist(&variant).map(|h| {
                variant = h.code;
                h.data
            })
        } else {
            None
        };
        nodes.push(PlanNode {
            id: id.clone(),
            def,
            chain: chains.and_then(|c| c.get(id)).cloned().unwrap_or_default(),
            gated,
            inputs,
            is_static,
            variant,
            tables,
        });
    }
    let n = nodes.len();

    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in nodes.iter().enumerate() {
        for &s in &node.inputs {
            consumers[s].push(i);
        }
    }
    // Rough cost: nodes in the subtree, with agent shaders and fractal
    // generators counting as heavy on their own (one node, many noise
    // evaluations per pixel), so a still one is cached rather than fused
    // into a consumer that changes every frame.
    let mut weight = vec![1usize; n];
    for i in 0..n {
        if matches!(nodes[i].def.name, "shader" | "noise" | "marble") {
            weight[i] = HEAVY_STATIC;
        }
        weight[i] += nodes[i].inputs.iter().map(|&s| weight[s]).sum::<usize>();
    }

    // Raster nodes absorb the chain of transforms stacked on them. Chain
    // members other than the head are never materialized or inlined.
    let mut chain_head = vec![usize::MAX; n];
    let mut in_chain = vec![false; n];
    for i in 0..n {
        if nodes[i].def.form != Form::Raster {
            continue;
        }
        let mut head = i;
        while consumers[head].len() == 1 && nodes[consumers[head][0]].def.func == "transform" {
            in_chain[head] = true;
            head = consumers[head][0];
        }
        chain_head[head] = i;
    }

    // Which nodes must exist as textures.
    if nodes.last().is_some_and(|o| o.def.form == Form::Object) {
        let o = nodes.last().unwrap();
        return Err(RenderError::BadParam {
            node: o.id.clone(),
            param: "output".into(),
            reason: format!(
                "`{}` is a 3D object, not an image: render it with a `render3d` node",
                o.def.name
            ),
        });
    }
    let mut materialize: Vec<bool> = (0..n)
        .map(|i| {
            // Scene objects are data, never textures; 3D passes always are.
            match nodes[i].def.form {
                Form::Object => return false,
                Form::Render3D | Form::Particles | Form::Sim => return true,
                _ => {}
            }
            if ops::render_scale(&doc.nodes[&nodes[i].id]).is_some() {
                return true;
            }
            // An agent shader with a timing window is its own pass: fused into
            // a composite, it would run whenever anything else in that kernel
            // shows (its window only switches its result off), and a raymarch
            // or a simulation-sized shader is far costlier than the texture
            // it takes to keep it apart.
            if nodes[i].def.name == "shader" && nodes[i].gated {
                return true;
            }
            if chain_head[i] != usize::MAX {
                return true;
            }
            if in_chain[i] {
                return false;
            }
            if i == n - 1 || consumers[i].len() != 1 || nodes[i].def.form == Form::Blur {
                return true;
            }
            let c = &nodes[consumers[i][0]];
            if matches!(
                c.def.form,
                Form::Blur | Form::Custom | Form::Object | Form::Render3D | Form::Particles | Form::Sim
            ) {
                return true;
            }
            nodes[i].is_static && !c.is_static && weight[i] >= HEAVY_STATIC
        })
        .collect();

    // Motion blur: materialize the image every moving layer reaches, so the
    // finishing looks after it can run once on the blurred result.
    let split = if doc.format.motion_blur.is_some_and(|m| m.samples > 1) {
        finishing_split(&nodes).filter(|&x| !in_chain[x] && chain_head[x] == usize::MAX)
    } else {
        None
    };
    if let Some(x) = split {
        materialize[x] = true;
    }

    // Kernels bind at most 16 textures. Walking in dependency order, count
    // the textures each inlined subtree would bring into its group; when a
    // node would exceed the budget, materialize its heaviest inlined child
    // (which then becomes its own group, already within budget).
    let budget = MAX_TEXTURES - 1;
    let mut contrib = vec![0usize; n];
    for i in 0..n {
        if nodes[i].def.form == Form::Object {
            continue;
        }
        let resource = ops::uses_resource(nodes[i].def) as usize;
        let child = |c: usize, m: &[bool], contrib: &[usize]| if m[c] { 1 } else { contrib[c] };
        let mut own: usize = resource
            + nodes[i]
                .inputs
                .iter()
                .map(|&c| child(c, &materialize, &contrib))
                .sum::<usize>();
        while own > budget {
            let heaviest = nodes[i]
                .inputs
                .iter()
                .copied()
                .filter(|&c| !materialize[c] && nodes[c].def.form != Form::Object)
                .max_by_key(|&c| contrib[c]);
            let Some(c) = heaviest else { break };
            own = own - contrib[c] + 1;
            materialize[c] = true;
        }
        contrib[i] = own;
    }

    let mut group_of = vec![usize::MAX; n];
    let mut groups: Vec<Group> = Vec::new();
    for root in 0..n {
        if !materialize[root] {
            continue;
        }
        let g = groups.len();
        let group = if nodes[root].def.form == Form::Particles {
            Group {
                kind: GroupKind::Particles,
                root,
                is_static: false,
                nodes: vec![root],
                textures: nodes[root]
                    .inputs
                    .iter()
                    .map(|&s| TexSource::Group(group_of[s]))
                    .collect(),
                params: Vec::new(),
                pad_textures: false,
                runs: Vec::new(),
                param_len: 4,
                output: usize::MAX,
                key: String::new(),
                body: String::new(),
                raster: None,
            }
        } else if nodes[root].def.form == Form::Sim {
            // Feedback steps run agent code: one generated kernel.
            let (key, body) = if nodes[root].def.name == "feedback" {
                crate::codegen::feedback(&nodes[root].variant, nodes[root].tables.is_some())
            } else {
                (String::new(), String::new())
            };
            Group {
                kind: GroupKind::Sim,
                root,
                is_static: false,
                nodes: vec![root],
                textures: nodes[root]
                    .inputs
                    .iter()
                    .map(|&s| TexSource::Group(group_of[s]))
                    .collect(),
                params: Vec::new(),
                pad_textures: false,
                runs: Vec::new(),
                param_len: 4,
                output: usize::MAX,
                key,
                body,
                raster: None,
            }
        } else if nodes[root].def.form == Form::Render3D {
            // Objects connected to the pass, and the images their materials use.
            let objects: Vec<usize> = nodes[root]
                .inputs
                .iter()
                .copied()
                .filter(|&o| nodes[o].def.form == Form::Object)
                .collect();
            let mut textures = Vec::new();
            // Images on the pass itself (an environment map), then the ones
            // object materials use.
            let images = nodes[root]
                .inputs
                .iter()
                .filter(|&&s| nodes[s].def.form != Form::Object);
            for &src in images.chain(objects.iter().flat_map(|&o| nodes[o].inputs.iter())) {
                let ts = TexSource::Group(group_of[src]);
                if !textures.contains(&ts) {
                    textures.push(ts);
                }
            }
            let mut members = objects.clone();
            members.push(root);
            Group {
                kind: GroupKind::Render3D,
                root,
                is_static: members.iter().all(|&m| nodes[m].is_static),
                nodes: members,
                textures,
                params: Vec::new(),
                pad_textures: false,
                runs: Vec::new(),
                param_len: 4,
                output: usize::MAX,
                key: String::new(),
                body: String::new(),
                raster: None,
            }
        } else if chain_head[root] != usize::MAX {
            let r = chain_head[root];
            let mut transforms = Vec::new();
            let mut cur = r;
            while cur != root {
                cur = consumers[cur][0];
                transforms.push(cur);
            }
            let mut members = vec![r];
            members.extend(&transforms);
            Group {
                kind: GroupKind::Raster,
                root,
                is_static: members.iter().all(|&m| nodes[m].is_static),
                nodes: members,
                textures: Vec::new(),
                params: vec![ParamSlot::Node(r)],
                pad_textures: false,
                runs: Vec::new(),
                param_len: 4 + nodes[r].def.params,
                output: usize::MAX,
                key: String::new(),
                body: String::new(),
                raster: Some(RasterChain { node: r, transforms }),
            }
        } else if nodes[root].def.form == Form::Blur {
            let src = nodes[root].inputs[0];
            Group {
                kind: GroupKind::Blur,
                root,
                nodes: vec![root],
                textures: vec![TexSource::Group(group_of[src])],
                params: vec![ParamSlot::Node(root)],
                pad_textures: false,
                runs: Vec::new(),
                param_len: 4 + nodes[root].def.params,
                is_static: nodes[root].is_static,
                output: usize::MAX,
                key: String::new(),
                body: String::new(),
                raster: None,
            }
        } else {
            build_fused(&nodes, &materialize, &group_of, root)?
        };
        group_of[root] = g;
        groups.push(group);
    }

    // Motion blur: every group the finishing split depends on goes first,
    // so the groups after it can run once after the samples without reading
    // slots the sampled groups reuse.
    if let Some(x) = split {
        let up = upstream(&nodes, x);
        let order: Vec<usize> = (0..groups.len())
            .filter(|&g| up[groups[g].root])
            .chain((0..groups.len()).filter(|&g| !up[groups[g].root]))
            .collect();
        let mut new_index = vec![0usize; groups.len()];
        for (ni, &og) in order.iter().enumerate() {
            new_index[og] = ni;
        }
        let mut old: Vec<Option<Group>> = std::mem::take(&mut groups).into_iter().map(Some).collect();
        groups = order.iter().map(|&og| old[og].take().unwrap()).collect();
        for g in &mut groups {
            for t in &mut g.textures {
                if let TexSource::Group(s) = t {
                    *s = new_index[*s];
                }
            }
        }
        for v in group_of.iter_mut() {
            if *v != usize::MAX {
                *v = new_index[*v];
            }
        }
    }
    let (slot_count, output_slot) = assign_slots(&mut groups);
    let (mb_split, post) = match split {
        Some(x) => {
            let up = upstream(&nodes, x);
            (Some(group_of[x]), groups.iter().map(|g| !up[g.root]).collect())
        }
        None => (None, vec![false; groups.len()]),
    };
    // Per group: the nodes its root depends on, and whether those span
    // exactly the groups before it (so the groups up to it can be sampled
    // for motion blur and the rest run once).
    let words = n.div_ceil(64);
    let mut up_nodes = vec![vec![0u64; words]; n];
    for i in 0..n {
        up_nodes[i][i / 64] |= 1 << (i % 64);
        for k in 0..nodes[i].inputs.len() {
            let s = nodes[i].inputs[k];
            let (a, b) = up_nodes.split_at_mut(i);
            for (w, v) in b[0].iter_mut().zip(&a[s]) {
                *w |= *v;
            }
        }
    }
    let group_up: Vec<Vec<u64>> = groups.iter().map(|g| up_nodes[g.root].clone()).collect();
    let prefix: Vec<bool> = (0..groups.len())
        .map(|k| (0..groups.len()).all(|j| (j <= k) == bit(&group_up[k], groups[j].root)))
        .collect();
    let overlay = overlays(&nodes);
    let eliminated = doc
        .nodes
        .keys()
        .filter(|id| !index.contains_key(id.as_str()))
        .cloned()
        .collect();
    let mut owner = vec![usize::MAX; nodes.len()];
    for (gi, g) in groups.iter().enumerate() {
        for &m in &g.nodes {
            owner[m] = if owner[m] == usize::MAX || owner[m] == gi {
                gi
            } else {
                SHARED
            };
        }
    }
    let framed = framed_groups(&nodes, &groups, &group_of);
    Ok(Plan {
        signature: signature(doc, chains),
        framed,
        nodes,
        groups,
        group_of,
        owner,
        slot_count,
        output_slot,
        eliminated,
        mb_split,
        post,
        group_up,
        prefix,
        overlay,
    })
}

/// [`Plan::framed`]: which groups' outputs are read only at their own frame
/// positions. The output is; a group read by a fused group's per-pixel ops
/// (inputs evaluated where the output pixel is) inherits that group's
/// answer; any warp, displacement, blur tap, shader, sprite or 3D material
/// reading it may look outside the frame.
fn framed_groups(nodes: &[PlanNode], groups: &[Group], group_of: &[usize]) -> Vec<bool> {
    let n = groups.len();
    // Per producer group: (consumer group, read at the consumer's own pixel).
    let mut reads: Vec<Vec<(usize, bool)>> = vec![Vec::new(); n];
    for (ci, c) in groups.iter().enumerate() {
        if c.kind != GroupKind::Fused {
            for t in &c.textures {
                if let TexSource::Group(p) = t {
                    reads[*p].push((ci, false));
                }
            }
            continue;
        }
        // Whether each node of the group is evaluated at the output pixel.
        let mut here: HashMap<usize, bool> = HashMap::new();
        here.insert(c.root, true);
        for &m in c.nodes.iter().rev() {
            let at = *here.get(&m).unwrap_or(&false);
            let pn = &nodes[m];
            for (port, &src) in pn.inputs.iter().enumerate() {
                let passes = at
                    && match pn.def.form {
                        Form::Map => true,
                        // The map is read in place; the source displaced.
                        Form::Displace => port == 1,
                        _ => false,
                    };
                if c.nodes.contains(&src) {
                    // Evaluated inline: at this node's pixel only if every
                    // consumer here passes its own.
                    let e = here.entry(src).or_insert(true);
                    *e &= passes;
                } else if group_of[src] < n {
                    reads[group_of[src]].push((ci, passes));
                }
            }
        }
    }
    let mut framed = vec![false; n];
    for g in (0..n).rev() {
        framed[g] = if g + 1 == n {
            true
        } else {
            !reads[g].is_empty() && reads[g].iter().all(|&(c, here)| here && framed[c])
        };
    }
    framed
}

/// [`Plan::owner`] of a node several groups evaluate.
pub const SHARED: usize = usize::MAX - 1;

impl Plan {
    /// Whether group `gi` evaluates node `ni`.
    pub fn in_group(&self, gi: usize, ni: usize) -> bool {
        match self.owner[ni] {
            SHARED => self.groups[gi].nodes.contains(&ni),
            o => o == gi,
        }
    }
}

/// Whether bit `i` is set in a node set.
pub fn bit(set: &[u64], i: usize) -> bool {
    set[i / 64] >> (i % 64) & 1 == 1
}

/// Generators that only ever lie over the picture as a texture (grain, a
/// light leak): consumed only as the `fg` of blends.
fn overlays(nodes: &[PlanNode]) -> Vec<bool> {
    let n = nodes.len();
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in nodes.iter().enumerate() {
        for &s in &node.inputs {
            consumers[s].push(i);
        }
    }
    (0..n)
        .map(|i| {
            matches!(nodes[i].def.form, Form::Custom | Form::Gen)
                && nodes[i].inputs.is_empty()
                && !consumers[i].is_empty()
                && consumers[i]
                    .iter()
                    .all(|&c| nodes[c].def.name == "blend" && nodes[c].inputs.first() == Some(&i))
        })
        .collect()
}

/// Nodes `x` depends on, itself included.
fn upstream(nodes: &[PlanNode], x: usize) -> Vec<bool> {
    let mut up = vec![false; nodes.len()];
    let mut stack = vec![x];
    while let Some(i) = stack.pop() {
        if !std::mem::replace(&mut up[i], true) {
            stack.extend(nodes[i].inputs.iter().copied());
        }
    }
    up
}

/// The node whose image already holds every moving layer, when everything
/// after it is a finishing look: color, blur and glow, shaders, still or
/// procedural overlays. With motion blur those run once on the blurred
/// image (as on a camera's exposure). None when there is nothing to gain.
fn finishing_split(nodes: &[PlanNode]) -> Option<usize> {
    let n = nodes.len();
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in nodes.iter().enumerate() {
        for &s in &node.inputs {
            consumers[s].push(i);
        }
    }
    let finishing = |i: usize| {
        let d = nodes[i].def;
        let generator = nodes[i].inputs.is_empty();
        match d.form {
            Form::Map | Form::Blur => true,
            // Filters, and still generators.
            Form::Custom | Form::Gen if !generator || nodes[i].is_static => true,
            // A changing generator is moving content, unless it only ever
            // lies over the picture as a texture (grain, a light leak).
            Form::Custom | Form::Gen => {
                !consumers[i].is_empty()
                    && consumers[i]
                        .iter()
                        .all(|&c| nodes[c].def.name == "blend" && nodes[c].inputs.first() == Some(&i))
            }
            _ => false,
        }
    };
    // Upstream sets, in topological order (inputs come first).
    let words = n.div_ceil(64);
    let mut up = vec![vec![0u64; words]; n];
    for i in 0..n {
        up[i][i / 64] |= 1 << (i % 64);
        for k in 0..nodes[i].inputs.len() {
            let s = nodes[i].inputs[k];
            let (a, b) = if s < i { up.split_at_mut(i) } else { continue };
            for (w, v) in b[0].iter_mut().zip(&a[s]) {
                *w |= *v;
            }
        }
    }
    let has = |set: &Vec<u64>, j: usize| set[j / 64] >> (j % 64) & 1 == 1;
    let scene: Vec<usize> = (0..n).filter(|&i| !finishing(i)).collect();
    let mut best: Option<(usize, u32)> = None;
    for x in 0..n.saturating_sub(1) {
        if !scene.iter().all(|&s| has(&up[x], s)) {
            continue;
        }
        if !(0..n).all(|j| has(&up[x], j) || finishing(j)) {
            continue;
        }
        let size: u32 = up[x].iter().map(|w| w.count_ones()).sum();
        if best.is_none_or(|(_, b)| size < b) {
            best = Some((x, size));
        }
    }
    best.map(|(x, _)| x)
}

fn build_fused(
    nodes: &[PlanNode],
    materialize: &[bool],
    group_of: &[usize],
    root: usize,
) -> Result<Group, RenderError> {
    // Build a codegen spec of the inlined subtree. Materialized inputs and
    // resources become external texture references.
    let mut spec: Vec<SpecNode> = Vec::new();
    let mut externals: Vec<TexSource> = Vec::new();
    let mut member: Vec<usize> = Vec::new();
    fn ext(externals: &mut Vec<TexSource>, s: TexSource) -> usize {
        externals.iter().position(|e| *e == s).unwrap_or_else(|| {
            externals.push(s);
            externals.len() - 1
        })
    }
    fn walk(
        nodes: &[PlanNode],
        materialize: &[bool],
        group_of: &[usize],
        i: usize,
        spec: &mut Vec<SpecNode>,
        externals: &mut Vec<TexSource>,
        member: &mut Vec<usize>,
    ) -> usize {
        let n = &nodes[i];
        let children = n
            .inputs
            .iter()
            .map(|&s| {
                if materialize[s] {
                    Child::External(ext(externals, TexSource::Group(group_of[s])))
                } else {
                    Child::Node(walk(nodes, materialize, group_of, s, spec, externals, member))
                }
            })
            .collect();
        let resource = ops::uses_resource(n.def).then(|| ext(externals, TexSource::Resource(n.variant.clone())));
        let code = (n.def.form == Form::Custom).then(|| n.variant.clone());
        spec.push(SpecNode {
            def: n.def,
            children,
            resource,
            code,
            tables: n.tables.as_ref().map_or(0, |t| t.len()),
            tag: i,
            gated: n.gated,
        });
        member.push(i);
        spec.len() - 1
    }
    let top = walk(
        nodes,
        materialize,
        group_of,
        root,
        &mut spec,
        &mut externals,
        &mut member,
    );
    let k = codegen::generate(&spec, top);
    if k.bindings.len() > MAX_TEXTURES {
        return Err(RenderError::TooManyInputs {
            node: nodes[root].id.clone(),
            count: k.bindings.len(),
        });
    }
    Ok(Group {
        kind: GroupKind::Fused,
        root,
        // Inputs' staticness is folded in by assign_slots.
        is_static: member.iter().all(|&m| nodes[m].is_static),
        nodes: member,
        textures: k.bindings.iter().map(|&b| externals[b].clone()).collect(),
        params: k
            .layout
            .iter()
            .map(|s| match s {
                codegen::Slot::Node(i) => ParamSlot::Node(spec[*i].tag),
                codegen::Slot::Run(r) => ParamSlot::Run(*r),
                codegen::Slot::Value(v) => ParamSlot::Value(*v),
                codegen::Slot::Pad(n) => ParamSlot::Pad(*n),
                codegen::Slot::Origins => ParamSlot::Origins,
            })
            .collect(),
        param_len: k.param_len,
        pad_textures: k.pad_textures,
        runs: k
            .runs
            .iter()
            .map(|r| PlanRun {
                stride: r.stride,
                payload: r.payload,
                layers: r
                    .layers
                    .iter()
                    .map(|l| {
                        let layer = match l.layer {
                            codegen::LayerRef::Gen(g) => codegen::LayerRef::Gen(spec[g].tag),
                            ref t => t.clone(),
                        };
                        (l.kind, spec[l.composite].tag, layer)
                    })
                    .collect(),
            })
            .collect(),
        output: usize::MAX,
        key: k.key,
        body: k.body,
        raster: None,
    })
}

/// Assigns texture slots by lifetime. Static group outputs are read on frames
/// where their producer does not run, so they keep dedicated slots. Dynamic
/// outputs share slots once their last reader has run.
fn assign_slots(groups: &mut [Group]) -> (usize, usize) {
    let n = groups.len();
    // A group is only static if every group it reads is static.
    for g in 0..n {
        let inputs_static = groups[g].textures.iter().all(|t| match t {
            TexSource::Group(s) => groups[*s].is_static,
            TexSource::Resource(_) => true,
        });
        groups[g].is_static &= inputs_static;
    }
    let mut last_use: Vec<usize> = (0..n).collect();
    for g in 0..n {
        for t in &groups[g].textures {
            if let TexSource::Group(s) = t {
                last_use[*s] = last_use[*s].max(g);
            }
        }
    }
    let mut count = 0;
    let mut free: Vec<usize> = Vec::new();
    let mut release_at: Vec<Vec<usize>> = vec![Vec::new(); n];
    let new_slot = |count: &mut usize| {
        *count += 1;
        *count - 1
    };
    for g in 0..n {
        // 3D passes keep their own slot: with motion blur a still scene is
        // rendered once and read by every sample.
        let slot = if groups[g].is_static || groups[g].kind == GroupKind::Render3D {
            new_slot(&mut count)
        } else {
            free.pop().unwrap_or_else(|| new_slot(&mut count))
        };
        groups[g].output = slot;
        if !groups[g].is_static && groups[g].kind != GroupKind::Render3D && g != n - 1 {
            release_at[last_use[g]].push(slot);
        }
        let released = std::mem::take(&mut release_at[g]);
        free.extend(released);
    }
    (count, groups.last().map(|g| g.output).unwrap_or(0))
}

/// Reorders a valid topological order to keep few intermediates alive at once:
/// at each node, the input with the heaviest subtree is evaluated first, so
/// light branches are produced just before they are consumed (Sethi-Ullman).
fn schedule(doc: &Document, topo: &[NodeId]) -> Vec<NodeId> {
    let mut weight: HashMap<&str, usize> = HashMap::new();
    for id in topo {
        let w = 1 + doc.nodes[id].inputs.values().map(|s| weight[s.as_str()]).sum::<usize>();
        weight.insert(id.as_str(), w);
    }
    let mut out = Vec::with_capacity(topo.len());
    let mut done: HashMap<&str, ()> = HashMap::new();
    let mut stack: Vec<(&str, bool)> = vec![(doc.output.as_str(), false)];
    while let Some((id, expanded)) = stack.pop() {
        if done.contains_key(id) {
            continue;
        }
        if expanded {
            done.insert(id, ());
            out.push(id.to_string());
            continue;
        }
        stack.push((id, true));
        let mut inputs: Vec<&str> = doc.nodes[id].inputs.values().map(String::as_str).collect();
        // Stack is LIFO: push lightest first so the heaviest is expanded first.
        inputs.sort_by_key(|s| weight[s]);
        for s in inputs {
            if !done.contains_key(s) {
                stack.push((s, false));
            }
        }
    }
    out
}

impl Plan {
    /// Human/agent readable description of the plan.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "plan: {} nodes in {} kernels, {} texture slots, output slot {}\n",
            self.nodes.len(),
            self.groups.len(),
            self.slot_count,
            self.output_slot
        );
        for (gi, g) in self.groups.iter().enumerate() {
            let ids: Vec<&str> = g.nodes.iter().map(|&i| self.nodes[i].id.as_str()).collect();
            let tex: Vec<String> = g
                .textures
                .iter()
                .map(|t| match t {
                    TexSource::Group(x) => format!("g{x}"),
                    TexSource::Resource(r) => format!("res:{r}"),
                })
                .collect();
            s += &format!(
                "  g{gi:<3} {:<6} {} -> slot {}{}  [{}]\n",
                match g.kind {
                    GroupKind::Fused => "fused",
                    GroupKind::Blur => "blur",
                    GroupKind::Raster => "raster",
                    GroupKind::Render3D => "3d",
                    GroupKind::Particles => "particles",
                    GroupKind::Sim => "sim",
                },
                if tex.is_empty() {
                    String::new()
                } else {
                    format!("reads {} ", tex.join(","))
                },
                g.output,
                if g.is_static { " static/cached" } else { "" },
                ids.join(" "),
            );
        }
        if let Some(sg) = self.mb_split {
            let post: Vec<String> = (0..self.groups.len())
                .filter(|&g| self.post[g])
                .map(|g| format!("g{g}"))
                .collect();
            s += &format!(
                "  motion blur: samples up to g{sg}; once per frame on the blurred image: {}\n",
                post.join(" ")
            );
        }
        if !self.eliminated.is_empty() {
            s += &format!("  eliminated (unreachable): {}\n", self.eliminated.join(", "));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layered(n: usize, animated: bool) -> Document {
        let anim = r#"{"keys":[{"t":0,"v":[0,0]},{"t":1,"v":[9,9]}]}"#;
        let center = if animated { anim } else { "[4,4]" };
        let mut nodes = format!(r#""s0": {{"op":"shape","params":{{"center":{center}}}}}"#);
        for i in 1..n {
            nodes += &format!(
                r#","s{i}": {{"op":"shape","params":{{"center":{center}}}}}
                ,"o{i}": {{"op":"over","inputs":{{"fg":"s{i}","bg":"{}"}}}}"#,
                if i == 1 {
                    "s0".to_string()
                } else {
                    format!("o{}", i - 1)
                }
            );
        }
        Document::from_json(&format!(
            r#"{{"version":1,"format":{{"width":8,"height":8,"fps":30,"duration":1}},"output":"o{}","nodes":{{{nodes}}}}}"#,
            n - 1
        ))
        .unwrap()
    }

    #[test]
    fn layer_stack_fuses_into_one_kernel() {
        let plan = compile(&layered(20, true), None).unwrap();
        assert_eq!(plan.nodes.len(), 39);
        assert_eq!(plan.groups.len(), 1, "{}", plan.describe());
        assert_eq!(plan.slot_count, 1);
    }

    #[test]
    fn shared_nodes_materialize_and_blur_is_a_barrier() {
        let doc = Document::from_json(
            r#"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":1},"output":"out","nodes":{
            "s": {"op":"shape"},
            "b": {"op":"blur","inputs":{"source":"s"}},
            "m": {"op":"mask","inputs":{"source":"b","matte":"s"}},
            "out": {"op":"transform","inputs":{"source":"m"}}}}"#,
        )
        .unwrap();
        let plan = compile(&doc, None).unwrap();
        // s (2 consumers), b (blur), then m + out fused.
        assert_eq!(plan.groups.len(), 3, "{}", plan.describe());
        assert_eq!(plan.groups[1].kind, GroupKind::Blur);
        assert_eq!(plan.groups[2].nodes.len(), 2);
        for g in &plan.groups {
            for t in &g.textures {
                if let TexSource::Group(s) = t {
                    assert_ne!(plan.groups[*s].output, g.output);
                }
            }
        }
    }

    #[test]
    fn param_values_do_not_change_signature_but_animation_does() {
        let mut a = layered(3, false);
        let s0 = signature(&a, None);
        a.nodes
            .get_mut("s1")
            .unwrap()
            .params
            .insert("size".into(), ultrafast_graph::Param::Number(3.0));
        assert_eq!(signature(&a, None), s0);
        let b = layered(3, true);
        assert_ne!(signature(&b, None), s0);
    }
}
