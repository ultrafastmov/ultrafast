//! Slang code generation for fused kernels.
//!
//! A kernel is generated from a small expression tree (`SpecNode`s). Each
//! node calls its op function from `shaders/lib.slang`; warps change the
//! context their input is evaluated in, so a shape under a transform is
//! evaluated analytically at the transformed position instead of being
//! rasterized and resampled.
//!
//! Param offsets are baked into the code and depend only on structure, so a
//! param edit never changes the code and never triggers a compile.

use crate::ops::{Form, OpDef};

/// Name every generated kernel entry point is emitted with. The compiler
/// renames it to a unique symbol per kernel key.
pub const ENTRY: &str = "ENTRY_POINT";
/// Placeholder for a per-kernel prefix on module-level names, so several
/// kernels can share one compile unit.
const NSID: &str = "NSID";

/// Op library prepended to every generated module.
pub const LIBRARY: &str = include_str!("../shaders/lib.slang");

#[derive(Debug, Clone)]
pub enum Child {
    /// Another node of the same spec, evaluated inline.
    Node(usize),
    /// External texture, identified by the caller's external index.
    External(usize),
}

#[derive(Debug, Clone)]
pub struct SpecNode {
    pub def: &'static OpDef,
    /// One per connected port, in port order.
    pub children: Vec<Child>,
    /// External index of the resource texture (GenResource ops).
    pub resource: Option<usize>,
    /// Agent-written shader code (Custom ops).
    pub code: Option<String>,
    /// Floats of the code's constant tables, after the rest of its param
    /// block (see [`crate::tables`]).
    pub tables: usize,
    /// Caller's identifier for this node, carried through.
    pub tag: usize,
    /// Has a visibility window: one extra param float (after the op block)
    /// says whether it is active this frame; inactive nodes are skipped.
    pub gated: bool,
}

#[derive(Debug, Clone)]
pub struct Generated {
    /// Module-level declarations plus the entry point, without the library.
    pub body: String,
    /// External indices bound to T0, T1, ... in order.
    pub bindings: Vec<usize>,
    /// What fills P[4..], in order.
    pub layout: Vec<Slot>,
    /// The runs of layers `Slot::Run` refers to.
    pub runs: Vec<RunLayout>,
    /// Total param floats: 4-float header, node blocks, one 2-float origin
    /// per texture binding, then the blocks of repeated layers.
    pub param_len: usize,
    /// The kernel may read any of T0..T15 (a run of texture layers picks
    /// its texture per layer): every binding needs a texture.
    pub pad_textures: bool,
    /// Content hash of `body`.
    pub key: String,
}

/// What fills a kernel's params after the 4-float header, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// A spec node's param block: its op params, its window flag when it
    /// has one, a 4-float guard rect when it is guarded.
    Node(usize),
    /// Where run `r`'s layers start and how many there are this frame (two
    /// floats the packer writes; the layers' blocks follow everything else).
    Run(usize),
    /// A literal float.
    Value(f32),
    /// Zeros (padding a repeated layer to its stride).
    Pad(usize),
    /// Two floats per texture binding: where each placed texture sits.
    Origins,
}

/// Chains at least this long of one composite over one kind of layer are
/// emitted as a loop.
const MIN_RUN: usize = 4;

/// A run of layers composited one over the other, emitted as a loop over
/// param blocks, so the kernel is the same however many layers there are.
struct Run {
    /// Composite nodes in loop order: the bottom one first.
    links: Vec<usize>,
    /// What the bottom composite lays its layer over.
    base: Child,
}

/// A run of layers as the packer writes it each frame: the blocks of the
/// layers showing then (`[kind][composite params][guard rect][layer]`,
/// `stride` floats each), after everything else in the params.
#[derive(Debug, Clone)]
pub struct RunLayout {
    pub stride: usize,
    /// Floats in the layer part of a block (a generator's params, or a
    /// texture's binding), padded to this.
    pub payload: usize,
    pub layers: Vec<RunLayer>,
}

#[derive(Debug, Clone)]
pub struct RunLayer {
    /// 0: a texture; 1..: a generator kind.
    pub kind: usize,
    /// The composite (spec index).
    pub composite: usize,
    pub layer: LayerRef,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayerRef {
    /// Texture binding.
    Texture(usize),
    /// Generator (spec index).
    Gen(usize),
}

struct Emitter<'a> {
    spec: &'a [SpecNode],
    decls: String,
    code: String,
    vars: usize,
    bindings: Vec<usize>,
    layout: Vec<Slot>,
    /// Runs, their texture layers holding external indices until the end,
    /// when they bind after every other texture.
    runs: Vec<RunLayout>,
    offsets: Vec<Option<usize>>,
    next_offset: usize,
    /// Values already computed in the current scope, by (node, context,
    /// identity): a subtree reached twice (a gated layer's passthrough, a
    /// guarded composite's background) is emitted once.
    memo: std::collections::HashMap<(usize, String, bool), String>,
    /// Custom shader nodes whose namespace is already declared.
    declared: std::collections::HashSet<usize>,
}

impl Emitter<'_> {
    fn var(&mut self) -> String {
        self.vars += 1;
        format!("v{}", self.vars)
    }

    fn texture(&mut self, ext: usize) -> String {
        let i = self.bindings.iter().position(|&b| b == ext).unwrap_or_else(|| {
            self.bindings.push(ext);
            self.bindings.len() - 1
        });
        format!("T{i}")
    }

    /// Placeholder for the param offset of a texture's origin; resolved once
    /// all node params are laid out (origins follow them).
    fn origin(&self, texture: &str) -> String {
        format!("ORIGIN_{texture}_")
    }

    fn offset(&mut self, n: usize) -> usize {
        if let Some(o) = self.offsets[n] {
            return o;
        }
        let o = self.next_offset;
        self.offsets[n] = Some(o);
        self.layout.push(Slot::Node(n));
        self.next_offset += self.block(n) + self.spec[n].tables;
        o
    }

    /// Floats of node `n`'s param block before its tables: op params, window
    /// flag, guard rect.
    fn block(&self, n: usize) -> usize {
        self.spec[n].def.params
            + self.spec[n].gated as usize
            + if crate::ops::guarded(self.spec[n].def) { 4 } else { 0 }
    }

    fn child(&mut self, c: &Child, ctx: &str, identity: bool) -> String {
        match c {
            Child::Node(n) => self.node(*n, ctx, identity),
            Child::External(e) => {
                let t = self.texture(*e);
                let v = self.var();
                let o = self.origin(&t);
                if identity {
                    self.code += &format!("    float4 {v} = load_t({t}, gid.xy, {o});\n");
                } else {
                    self.code += &format!("    float4 {v} = sample_t({t}, {ctx}.pos, {o});\n");
                }
                v
            }
        }
    }

    /// Emits node `n` evaluated in context `ctx`; returns its value variable.
    /// `identity` means ctx is the unwarped pixel center, so textures can be
    /// loaded without resampling. Gated nodes (and everything inlined under
    /// them) only run while active.
    fn node(&mut self, n: usize, ctx: &str, identity: bool) -> String {
        let key = (n, ctx.to_string(), identity);
        if let Some(v) = self.memo.get(&key) {
            return v.clone();
        }
        if let Some(run) = self.run_at(n) {
            let v = self.emit_run(run, ctx, identity);
            self.memo.insert(key, v.clone());
            return v;
        }
        let v = if !self.spec[n].gated {
            self.node_inner(n, ctx, identity)
        } else {
            // Inactive: pass the destination/source input through unchanged.
            // It is computed before the branch (in scope for both sides), so
            // the active side reuses it instead of emitting it again.
            let pass = crate::ops::passthrough(self.spec[n].def, self.spec[n].children.len());
            let pv = pass.map(|i| {
                let child = self.spec[n].children[i].clone();
                self.child(&child, ctx, identity)
            });
            let active = self.offset(n) + self.spec[n].def.params;
            let v = self.var();
            self.code += &format!(
                "    float4 {v} = {};\n    if (P[{active}] > 0.5) {{\n",
                pv.as_deref().unwrap_or("float4(0.0)")
            );
            let saved = self.memo.clone();
            let inner = self.node_inner(n, ctx, identity);
            self.memo = saved;
            self.code += &format!("    {v} = {inner};\n    }}\n");
            v
        };
        self.memo.insert(key, v.clone());
        v
    }

    /// The run of layers whose top composite is `n`: guarded composites of
    /// one op with windows, chained through their backgrounds, each laying
    /// either a texture or a plain generator over the one below. Windowed
    /// layers (captions, beats) are what differs between chapters and what
    /// is mostly off at any moment, so looping over the ones showing costs
    /// less than unrolling them all; layers that are always on stay
    /// unrolled, where constant offsets are cheapest.
    fn run_at(&self, n: usize) -> Option<Run> {
        let top = &self.spec[n];
        if top.def.form != Form::Map || !crate::ops::guarded(top.def) {
            return None;
        }
        let layer_ok = |c: &Child| match c {
            Child::External(_) => true,
            Child::Node(g) => {
                let g = &self.spec[*g];
                g.def.form == Form::Gen && g.children.is_empty() && g.resource.is_none() && g.code.is_none()
            }
        };
        let mut links = Vec::new();
        let mut cur = n;
        let base = loop {
            let s = &self.spec[cur];
            if s.def.name != top.def.name || !s.gated || s.children.len() != 2 || !layer_ok(&s.children[0]) {
                break Child::Node(cur);
            }
            links.push(cur);
            match &s.children[1] {
                Child::Node(b) => cur = *b,
                c @ Child::External(_) => break c.clone(),
            }
        };
        if links.len() < MIN_RUN {
            return None;
        }
        links.reverse();
        Some(Run { links, base })
    }

    /// A run as a loop over the blocks of the layers showing this frame,
    /// which the packer writes after everything else in the params:
    /// `[kind][composite params][guard rect][layer]`, kind 0 a texture (its
    /// binding in the layer part), kind 1.. a generator (its params there).
    /// Layers out of their windows or empty are left out, so the loop runs
    /// only over what shows.
    fn emit_run(&mut self, run: Run, ctx: &str, identity: bool) -> String {
        let bgv = self.child(&run.base, ctx, identity);
        let cdef = self.spec[run.links[0]].def;
        // Generator kinds in a fixed order, so runs of the same kinds share
        // a kernel whatever their length or order.
        let mut kinds: Vec<&'static OpDef> = Vec::new();
        let mut textures = false;
        for &l in &run.links {
            match &self.spec[l].children[0] {
                Child::External(_) => textures = true,
                Child::Node(g) => {
                    // By function: one op can have several (gradient kinds).
                    let d = self.spec[*g].def;
                    if !kinds.iter().any(|k| k.func == d.func) {
                        kinds.push(d);
                    }
                }
            }
        }
        kinds.sort_by_key(|d| d.func);
        // A texture's layer part: its binding, width and height.
        let payload = kinds
            .iter()
            .map(|d| d.params)
            .chain(textures.then_some(3))
            .max()
            .unwrap_or(1)
            .max(1);
        let stride = 1 + cdef.params + 4 + payload;
        let layers = run
            .links
            .iter()
            .map(|&l| match &self.spec[l].children[0] {
                Child::External(e) => RunLayer {
                    kind: 0,
                    composite: l,
                    layer: LayerRef::Texture(*e),
                },
                Child::Node(g) => RunLayer {
                    kind: 1 + kinds
                        .iter()
                        .position(|d| d.func == self.spec[*g].def.func)
                        .expect("kind collected above"),
                    composite: l,
                    layer: LayerRef::Gen(*g),
                },
            })
            .collect();
        // In the main block: where the layers start and how many show.
        let slot = self.next_offset;
        self.next_offset += 2;
        self.layout.push(Slot::Run(self.runs.len()));
        self.runs.push(RunLayout {
            stride,
            payload,
            layers,
        });
        let v = self.var();
        let id = self.vars;
        let (i, o, q, k, f) = (
            format!("i{id}"),
            format!("o{id}"),
            format!("q{id}"),
            format!("k{id}"),
            format!("f{id}"),
        );
        let mut code = format!(
            "    float4 {v} = {bgv};\n    for (uint {i} = 0; {i} < uint(P[{count}]); {i}++) {{\n        \
             uint {o} = uint(P[{slot}]) + {i} * {stride};\n        \
             if (!in_rect({ctx}.pos, pf4({o} + {guard}))) continue;\n        \
             uint {k} = uint(P[{o}]);\n        uint {q} = {o} + {layer};\n        float4 {f} = float4(0.0);\n",
            count = slot + 1,
            guard = 1 + cdef.params,
            layer = 1 + cdef.params + 4
        );
        // One reader per kind; with a single kind, no dispatch at all.
        let mut reads: Vec<(usize, String)> = Vec::new();
        if textures {
            let pick = self.texture_picker(identity);
            reads.push((
                0,
                if identity {
                    format!(
                        "uint b = uint(P[{q}]); int2 p = int2(gid.xy) - int2(pf2(ORIGINS_BASE_ + 3 * b)); \
                         if (p.x >= 0 && p.y >= 0 && p.x < int(P[{q} + 1]) && p.y < int(P[{q} + 2])) {f} = {pick}(b, p);"
                    )
                } else {
                    format!("uint b = uint(P[{q}]); {f} = {pick}(b, ({ctx}.pos - pf2(ORIGINS_BASE_ + 3 * b)) / pf2({q} + 1));")
                },
            ));
        }
        for (x, d) in kinds.iter().enumerate() {
            reads.push((x + 1, format!("{f} = g_{}({ctx}, {q});", d.func)));
        }
        if reads.len() == 1 {
            code += &format!("        {}\n", reads[0].1);
        } else {
            for (x, (kind, read)) in reads.iter().enumerate() {
                let branch = if x == 0 { "if" } else { "else if" };
                code += &format!("        {branch} ({k} == {kind}) {{ {read} }}\n");
            }
        }
        code += &format!("        {v} = f_{}({ctx}, {f}, {v}, {o} + 1);\n    }}\n", cdef.func);
        self.code += &code;
        v
    }

    /// A function reading texture binding `b` (any of T0..T15), declared
    /// once per kernel: a texel load at a texture position, or a sample at
    /// texture coordinates (the caller places and bounds the texture).
    fn texture_picker(&mut self, identity: bool) -> String {
        let name = format!("{NSID}_layer_{}", if identity { "load" } else { "sample" });
        if !self.decls.contains(&format!("float4 {name}(")) {
            let mut cases = String::new();
            for t in 0..16 {
                let read = if identity {
                    format!("T{t}.Load(int3(p, 0))")
                } else {
                    format!("T{t}.SampleLevel(S, uv, 0)")
                };
                cases += &format!("    case {t}: return {read};\n");
            }
            let arg = if identity { "int2 p" } else { "float2 uv" };
            self.decls += &format!(
                "float4 {name}(uint b, {arg}) {{\n    switch (b) {{\n{cases}    default: return float4(0.0);\n    }}\n}}\n"
            );
        }
        name
    }

    fn node_inner(&mut self, n: usize, ctx: &str, identity: bool) -> String {
        let s = &self.spec[n];
        let def = s.def;
        match def.form {
            Form::Gen => {
                let o = self.offset(n);
                let v = self.var();
                self.code += &format!("    float4 {v} = g_{}({ctx}, {o});\n", def.func);
                v
            }
            Form::GenResource => {
                let o = self.offset(n);
                let t = self.texture(s.resource.expect("resource op without resource"));
                let v = self.var();
                self.code += &format!("    float4 {v} = g_{}({ctx}, {o}, {t});\n", def.func);
                v
            }
            Form::Map if crate::ops::guarded(def) => {
                // fg is only evaluated inside its bounds; elsewhere the
                // result is bg untouched (for over and normal blends with
                // transparent fg).
                let (fg, bg) = (s.children[0].clone(), s.children[1].clone());
                let bgv = self.child(&bg, ctx, identity);
                let o = self.offset(n);
                let guard = o + def.params + s.gated as usize;
                let v = self.var();
                self.code += &format!("    float4 {v} = {bgv};\n    if (in_rect({ctx}.pos, pf4({guard}))) {{\n");
                let saved = self.memo.clone();
                let fgv = self.child(&fg, ctx, identity);
                self.memo = saved;
                self.code += &format!("    {v} = f_{}({ctx}, {fgv}, {bgv}, {o});\n    }}\n", def.func);
                v
            }
            Form::Map => {
                let children = s.children.clone();
                let args: Vec<String> = children.iter().map(|c| self.child(c, ctx, identity)).collect();
                let o = self.offset(n);
                // Ops with a resource (LUTs) get its texture last.
                let res = match s.resource {
                    Some(r) => format!(", {}", self.texture(r)),
                    None => String::new(),
                };
                let v = self.var();
                self.code += &format!(
                    "    float4 {v} = f_{}({ctx}, {}, {o}{res});\n",
                    def.func,
                    args.join(", ")
                );
                v
            }
            Form::Warp => {
                let o = self.offset(n);
                let c = format!("c{}", self.vars + 1);
                self.vars += 1;
                self.code += &format!("    Ctx {c} = w_{}({ctx}, {o});\n", def.func);
                let child = s.children[0].clone();
                self.child(&child, &c, false)
            }
            Form::Displace => {
                let (source, map) = (s.children[0].clone(), s.children[1].clone());
                let m = self.child(&map, ctx, identity);
                let o = self.offset(n);
                let c = format!("c{}", self.vars + 1);
                self.vars += 1;
                self.code += &format!("    Ctx {c} = w_displace({ctx}, {m}, {o});\n");
                self.child(&source, &c, false)
            }
            Form::Custom => {
                let o = self.offset(n);
                let ns = format!("{NSID}_cs{n}");
                let mut cases = String::new();
                for (port, c) in s.children.clone().iter().enumerate() {
                    let Child::External(e) = c else {
                        unreachable!("custom shader inputs are always materialized")
                    };
                    let t = self.texture(*e);
                    let o = self.origin(&t);
                    cases += &format!("        case {port}: return sample_t({t}, pos, {o});\n");
                }
                let code = s.code.clone().unwrap_or_default();
                let tables = if s.tables > 0 {
                    crate::tables::declare_base(o + self.block(n))
                } else {
                    String::new()
                };
                if self.declared.insert(n) {
                    self.decls += &format!(
                        "namespace {ns} {{\n\
                     static const uint PO = {o};\n\
                     {tables}\
                     float param(int i) {{ return P[PO + uint(i)]; }}\n\
                     float time() {{ return P[PO + 16]; }}\n\
                     float2 size() {{ return resolution(); }}\n\
                     float4 input(int i, float2 pos) {{\n    switch (i) {{\n{cases}        default: return float4(0.0);\n    }}\n}}\n\
                     float4 from_srgb(float4 c) {{ return float4(srgb_to_linear(saturate(c.rgb)) * c.a, c.a); }}\n\
                     #line 1 \"shader\"\n\
                     {code}\n\
                     }}\n"
                    );
                }
                let v = self.var();
                self.code += &format!("    float4 {v} = {ns}::shade({ctx}.pos, {ctx}.px);\n");
                v
            }
            Form::Blur | Form::Raster | Form::Object | Form::Render3D | Form::Particles | Form::Sim => {
                unreachable!("barriers and scene objects are never inlined")
            }
        }
    }
}

/// Generates the kernel for `spec`, rooted at node `root`.
pub fn generate(spec: &[SpecNode], root: usize) -> Generated {
    let mut e = Emitter {
        spec,
        decls: String::new(),
        code: String::new(),
        vars: 0,
        bindings: Vec::new(),
        layout: Vec::new(),
        runs: Vec::new(),
        offsets: vec![None; spec.len()],
        next_offset: 4,
        memo: std::collections::HashMap::new(),
        declared: std::collections::HashSet::new(),
    };
    let v = e.node(root, "c0", true);
    // Runs' textures bind after every other texture, so the others keep
    // their bindings however long the runs are.
    let mut pad_textures = false;
    let mut runs = std::mem::take(&mut e.runs);
    for run in &mut runs {
        for layer in &mut run.layers {
            if let LayerRef::Texture(ext) = layer.layer {
                pad_textures = true;
                let b = e.bindings.iter().position(|&x| x == ext).unwrap_or_else(|| {
                    e.bindings.push(ext);
                    e.bindings.len() - 1
                });
                layer.layer = LayerRef::Texture(b);
            }
        }
    }
    // Texture origins and texel sizes (3 floats each) follow the node params.
    let origins = e.next_offset;
    for k in 0..e.bindings.len() {
        let off = origins + 3 * k;
        let ph = format!("ORIGIN_T{k}_");
        e.code = e.code.replace(&ph, &off.to_string());
        e.decls = e.decls.replace(&ph, &off.to_string());
    }
    e.code = e.code.replace("ORIGINS_BASE_", &origins.to_string());
    e.layout.push(Slot::Origins);
    // Then, at most, every layer of every run.
    let param_len = origins + 3 * e.bindings.len() + runs.iter().map(|r| r.stride * r.layers.len()).sum::<usize>();
    let body = format!(
        "{}\n[shader(\"compute\")]\n[numthreads(16, 16, 1)]\nvoid {ENTRY}(uint3 gid: SV_DispatchThreadID) {{\n    \
         if (outside_frame(gid.xy)) return;\n    Ctx c0 = root_ctx(gid.xy);\n{}    OUT.Store(gid.xy, {v});\n}}\n",
        e.decls, e.code
    );
    let key = blake3::hash(body.as_bytes()).to_hex()[..24].to_string();
    Generated {
        body,
        bindings: e.bindings,
        layout: e.layout,
        runs,
        param_len,
        pad_textures,
        key,
    }
}

/// Generates the standalone single-op kernel for `def` (tier 0): all inputs
/// are textures. Returns None for ops without one (blur, custom shaders).
pub fn single_op(def: &'static OpDef) -> Option<Generated> {
    if matches!(
        def.form,
        Form::Blur | Form::Custom | Form::Raster | Form::Object | Form::Render3D | Form::Particles | Form::Sim
    ) {
        return None;
    }
    let spec = [SpecNode {
        def,
        children: (0..def.ports.len()).map(Child::External).collect(),
        resource: crate::ops::uses_resource(def).then_some(def.ports.len()),
        code: None,
        tables: 0,
        tag: 0,
        gated: false,
    }];
    Some(generate(&spec, 0))
}

/// Renames the generated entry point to a unique symbol.
pub fn entry_name(key: &str) -> String {
    format!("k_{key}")
}

/// Assembles a compile unit: the op library plus each kernel body with its
/// entry point and module-level names made unique.
pub fn module(kernels: &[(&str, &str)]) -> String {
    let mut s = String::with_capacity(LIBRARY.len() + kernels.iter().map(|k| k.1.len() + 64).sum::<usize>());
    s += LIBRARY;
    // Groups with the same structure share one kernel: emit each key once.
    let mut seen = std::collections::HashSet::new();
    for (key, body) in kernels {
        if !seen.insert(*key) {
            continue;
        }
        let name = entry_name(key);
        s += "\n";
        s += &body.replace(ENTRY, &name).replace(NSID, &format!("k{key}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::OPS;

    fn def(func: &str) -> &'static OpDef {
        OPS.iter().find(|d| d.func == func).unwrap()
    }

    #[test]
    fn warp_over_generator_is_analytic() {
        let spec = vec![
            SpecNode {
                def: def("shape"),
                children: vec![],
                resource: None,
                code: None,
                tables: 0,
                tag: 0,
                gated: false,
            },
            SpecNode {
                def: def("transform"),
                children: vec![Child::Node(0)],
                resource: None,
                code: None,
                tables: 0,
                tag: 1,
                gated: false,
            },
            SpecNode {
                def: def("over"),
                children: vec![Child::Node(1), Child::External(7)],
                resource: None,
                code: None,
                tables: 0,
                tag: 2,
                gated: false,
            },
        ];
        let g = generate(&spec, 2);
        assert!(g.body.contains("w_transform(c0, 10)"), "{}", g.body);
        // The fg subtree only runs inside its bounds.
        assert!(g.body.contains("if (in_rect(c0.pos, pf4(6)))"), "{}", g.body);
        assert!(g.body.contains("g_shape(c"), "{}", g.body);
        // Origin of T0 follows the 26 node param floats (over carries a 4-float guard).
        assert!(g.body.contains("load_t(T0, gid.xy, 30)"), "{}", g.body);
        assert_eq!(g.bindings, vec![7]);
        // transform(7) + shape(13) + over(2) after the header, plus one
        // origin and texel size.
        assert_eq!(g.param_len, 4 + 7 + 13 + 2 + 4 + 3);
    }

    fn chain(n: usize, texture_every: usize) -> Vec<SpecNode> {
        // Layers over External(0): shapes, with a texture every few layers.
        let mut spec = Vec::new();
        let mut below = Child::External(0);
        for i in 0..n {
            let layer = if texture_every > 0 && i % texture_every == 1 {
                Child::External(10 + i)
            } else {
                spec.push(SpecNode {
                    def: def("shape"),
                    children: vec![],
                    resource: None,
                    code: None,
                    tables: 0,
                    tag: spec.len(),
                    gated: false,
                });
                Child::Node(spec.len() - 1)
            };
            spec.push(SpecNode {
                def: def("over"),
                children: vec![layer, below],
                resource: None,
                code: None,
                tables: 0,
                tag: spec.len(),
                gated: true,
            });
            below = Child::Node(spec.len() - 1);
        }
        spec
    }

    #[test]
    fn repeated_layers_loop_and_share_a_kernel_at_any_count() {
        let a = chain(6, 0);
        let b = chain(9, 0);
        let (ga, gb) = (generate(&a, a.len() - 1), generate(&b, b.len() - 1));
        assert!(ga.body.contains("for (uint"), "{}", ga.body);
        assert_eq!(ga.key, gb.key);
        assert_ne!(ga.param_len, gb.param_len);
        assert!(!ga.pad_textures);
        // Textures among the layers loop too, bound after the base.
        let c = chain(8, 3);
        let d = chain(11, 3);
        let (gc, gd) = (generate(&c, c.len() - 1), generate(&d, d.len() - 1));
        assert_eq!(gc.key, gd.key);
        assert!(gc.pad_textures);
        assert_eq!(gc.bindings[0], 0);
        // Short chains stay unrolled.
        let e = chain(3, 0);
        assert!(!generate(&e, e.len() - 1).body.contains("for (uint"));
    }

    #[test]
    fn same_structure_same_key() {
        let a = single_op(def("over")).unwrap();
        let b = single_op(def("over")).unwrap();
        assert_eq!(a.key, b.key);
        assert_ne!(a.key, single_op(def("mask")).unwrap().key);
    }
}

/// Param layout of a `feedback` step kernel (after the 4-float header,
/// whose [1] is the state texel size in frame px).
pub mod feedback_params {
    /// u0..u15.
    pub const U: usize = 4;
    pub const TIME: usize = 20;
    pub const DT: usize = 21;
    /// Steps taken since the simulation started (0 on the first).
    pub const STEP: usize = 22;
    /// Origins of in0..in3 (2 floats each).
    pub const ORIGINS: usize = 24;
    pub const LEN: usize = 32;
}

/// Kernel for one step of a `feedback` node: `update(pos, px)` from the
/// agent's code, with the previous state (T0) and up to four placed inputs
/// (T1..T4), and the code's constant tables after the params when it has
/// them. Returns (key, body).
pub fn feedback(code: &str, tables: bool) -> (String, String) {
    use feedback_params::*;
    let mut cases = String::new();
    for i in 0..4 {
        cases += &format!(
            "        case {i}: return sample_o(T{}, pos, {});\n",
            i + 1,
            ORIGINS + 2 * i
        );
    }
    let tables = if tables {
        crate::tables::declare_base(LEN)
    } else {
        String::new()
    };
    let body = format!(
        "namespace {NSID}_fb {{\n\
         {tables}\
         float param(int i) {{ return P[{U} + uint(i)]; }}\n\
         float time() {{ return P[{TIME}]; }}\n\
         float dt() {{ return P[{DT}]; }}\n\
         int step_index() {{ return int(P[{STEP}] + 0.5); }}\n\
         float2 size() {{ return resolution(); }}\n\
         float4 prev(float2 pos) {{ return sample_region(T0, pos, float2(0.0), P[1]); }}\n\
         float4 prev_sharp(float2 pos) {{ return sample_sharp(T0, pos / P[1]); }}\n\
         float4 input(int i, float2 pos) {{\n    switch (i) {{\n{cases}        default: return float4(0.0);\n    }}\n}}\n\
         float4 from_srgb(float4 c) {{ return float4(srgb_to_linear(saturate(c.rgb)) * c.a, c.a); }}\n\
         #line 1 \"shader\"\n\
         {code}\n\
         }}\n\
         [shader(\"compute\")]\n[numthreads(16, 16, 1)]\nvoid {ENTRY}(uint3 gid: SV_DispatchThreadID) {{\n    \
         if (outside_grid(gid.xy)) return;\n    Ctx c0 = root_ctx(gid.xy);\n    \
         OUT.Store(gid.xy, {NSID}_fb::update(c0.pos, c0.px));\n}}\n"
    );
    let key = blake3::hash(body.as_bytes()).to_hex()[..24].to_string();
    (key, body)
}
