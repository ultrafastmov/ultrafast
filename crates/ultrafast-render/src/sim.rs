//! Stateful simulations: `fluid` (ink and smoke) and `feedback` (agent
//! code that reads its own previous frame).
//!
//! A simulation's image at frame F is what stepping it once per frame from
//! the start produces, so it stays a pure function of the document and the
//! frame like every other op. Exports are exact; seeking resumes from the
//! nearest checkpoint and re-simulates the frames in between (with their
//! own inputs). In the live player an edit keeps the running state, so the
//! simulation reacts instead of restarting, and long jumps catch up a few
//! frames per displayed frame instead of stalling it.

use crate::plan::{GroupKind, Plan};
use crate::{Active, FrameReport, Mode, Placed, RenderError, Renderer, header, local_time};
use ultrafast_gpu::{CommandList, PixelFormat, Texture, TextureDesc};
use ultrafast_graph::{Document, Node};

/// Frames between checkpoints.
const CHECK_EVERY: i64 = 15;
/// Checkpoints kept per simulation; past this, every other one is dropped.
const MAX_CHECKPOINTS: usize = 24;
/// Frames the live player simulates per displayed frame when behind.
const LIVE_CATCHUP: i64 = 4;

pub(crate) struct SimState {
    /// Identity of everything the result depends on (the node, its
    /// upstream, the format).
    key: u64,
    /// Last composition frame processed; -1 before the first.
    frame: i64,
    /// Steps taken since the simulation started.
    steps: u64,
    /// Matches what an export would compute.
    exact: bool,
    /// fluid: [velocity, dye, pressure]; feedback: [state]. Empty = blank.
    fields: Vec<Texture>,
    checkpoints: Vec<Checkpoint>,
}

struct Checkpoint {
    frame: i64,
    steps: u64,
    exact: bool,
    fields: Vec<Texture>,
}

impl SimState {
    fn new(key: u64) -> Self {
        Self {
            key,
            frame: -1,
            steps: 0,
            exact: true,
            fields: Vec::new(),
            checkpoints: Vec::new(),
        }
    }
}

fn num(node: &Node, name: &str, t: f64, default: f64) -> f64 {
    node.params
        .get(name)
        .and_then(|p| p.eval(t))
        .and_then(|v| v.first().copied())
        .unwrap_or(default)
}

fn vec2(node: &Node, name: &str, t: f64, default: [f64; 2]) -> [f64; 2] {
    match node.params.get(name).and_then(|p| p.eval(t)) {
        Some(v) if v.len() >= 2 => [v[0], v[1]],
        Some(v) if v.len() == 1 => [v[0], v[0]],
        _ => default,
    }
}

/// Composition frame of time `t`.
fn frame_of(t: f64, fps: f64) -> i64 {
    (t * fps + 1e-6).floor() as i64
}

fn grid(doc: &Document, scale: f64, format: PixelFormat) -> TextureDesc {
    let s = scale.clamp(0.05, 1.0);
    TextureDesc {
        width: ((doc.format.width as f64 * s).ceil() as u32).max(1),
        height: ((doc.format.height as f64 * s).ceil() as u32).max(1),
        format,
        cpu_readable: false,
    }
}

/// Field layouts of a simulation node at time `t`.
fn layout(doc: &Document, node: &Node, t: f64) -> Vec<TextureDesc> {
    // State that accumulates small increments every step (velocity,
    // pressure, agent state such as reaction-diffusion) needs full floats;
    // the dye is an image.
    if node.op == "fluid" {
        let v = grid(doc, num(node, "resolution", t, 0.25), PixelFormat::Rgba32Float);
        let d = grid(doc, num(node, "dye_resolution", t, 0.5), PixelFormat::Rgba16Float);
        vec![v, d, v]
    } else {
        vec![grid(doc, num(node, "resolution", t, 1.0), PixelFormat::Rgba32Float)]
    }
}

/// Frame px per texel of a grid.
fn texel(doc: &Document, d: &TextureDesc) -> f32 {
    doc.format.width as f32 / d.width as f32
}

impl Renderer {
    fn sim_key(&self, plan: &Plan, doc: &Document, root: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (doc.format.width, doc.format.height, doc.format.fps.to_bits()).hash(&mut h);
        let mut stack = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(i) = stack.pop() {
            if !seen.insert(i) {
                continue;
            }
            let pn = &plan.nodes[i];
            pn.id.hash(&mut h);
            format!("{:?}", doc.nodes[&pn.id]).hash(&mut h);
            format!("{:?}", pn.chain).hash(&mut h);
            stack.extend(pn.inputs.iter().copied());
        }
        h.finish()
    }

    fn release(&mut self, fields: Vec<Texture>) {
        for f in fields {
            self.pool.give_back(f);
        }
    }

    fn copy_fields(&mut self, fields: &[Texture], list: &mut CommandList) -> Vec<Texture> {
        let k = self.kernels.utility("k_copy");
        fields
            .iter()
            .map(|f| {
                let c = self.pool.lease(&self.device, f.desc());
                list.dispatch(&k, &c, &[f], &[0.0, 1.0, 0.0, 0.0]);
                c
            })
            .collect()
    }

    /// Brings every simulation in the plan to the frame before `t`: reconciles
    /// edits, restores a checkpoint when seeking back, and renders the
    /// frames in between (only what the simulations read) so the frame at `t`
    /// takes a single step.
    pub(crate) fn sim_catch_up(
        &mut self,
        active: &mut Active,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
        report: &mut FrameReport,
    ) -> Result<(), RenderError> {
        let sims: Vec<usize> = (0..active.plan.groups.len())
            .filter(|&g| active.plan.groups[g].kind == GroupKind::Sim)
            .collect();
        if sims.is_empty() {
            return Ok(());
        }
        let fps = doc.format.fps;
        let target = frame_of(t, fps);
        let mut need: Vec<(usize, i64, i64)> = Vec::new();
        for &gi in &sims {
            let root = active.plan.groups[gi].root;
            let id = active.plan.nodes[root].id.clone();
            let key = self.sim_key(&active.plan, doc, root);
            let mut st = self.sims.remove(&id).unwrap_or_else(|| SimState::new(key));
            if st.key != key {
                // Live: keep running with the new settings. Exact: start over.
                let old = std::mem::take(&mut st.checkpoints);
                for c in old {
                    self.release(c.fields);
                }
                st.key = key;
                st.exact = false;
            }
            if self.mode == Mode::Exact && !st.exact {
                self.release(std::mem::take(&mut st.fields));
                st = SimState::new(key);
            }
            if target <= st.frame {
                // Seeking back (or re-rendering the same frame, which must not
                // step twice): resume from the latest usable checkpoint.
                if target < st.frame {
                    let usable = |c: &Checkpoint| c.frame < target && (c.exact || self.mode == Mode::Live);
                    let found = st.checkpoints.iter().rposition(usable);
                    self.release(std::mem::take(&mut st.fields));
                    match found {
                        Some(i) => {
                            let c = &st.checkpoints[i];
                            let (frame, steps, exact) = (c.frame, c.steps, c.exact);
                            let src = c.fields.clone();
                            st.fields = self.copy_fields(&src, list);
                            st.frame = frame;
                            st.steps = steps;
                            st.exact = exact;
                        }
                        None => {
                            st.frame = -1;
                            st.steps = 0;
                            st.exact = true;
                        }
                    }
                }
            }
            let from = st.frame + 1;
            let mut to = target - 1;
            if self.mode == Mode::Live {
                to = to.min(st.frame + LIVE_CATCHUP);
            }
            if from <= to {
                need.push((gi, from, to));
            }
            self.sims.insert(id, st);
        }
        if need.is_empty() {
            return Ok(());
        }
        let first = need.iter().map(|n| n.1).min().unwrap();
        let last = need.iter().map(|n| n.2).max().unwrap();
        let mb = doc.format.motion_blur.filter(|m| m.samples > 1);
        for f in first..=last {
            let roots: Vec<usize> = need
                .iter()
                .filter(|n| n.1 <= f && f <= n.2)
                .map(|n| active.plan.groups[n.0].root)
                .collect();
            let tf = doc.format.time_of(f.max(0) as u64);
            // The same instant the frame's own render steps at: the first
            // motion-blur sample.
            let ts = match mb {
                Some(m) => {
                    let span = m.shutter / fps;
                    tf - span / 2.0 + span * 0.5 / m.samples as f64
                }
                None => tf,
            };
            // Frames where none of them runs advance without rendering.
            let running = roots.iter().any(|&r| {
                let pn = &active.plan.nodes[r];
                let (lt, on) = local_time(pn, tf);
                on && lt >= num(&doc.nodes[&pn.id], "start", lt, 0.0)
            });
            if !running {
                for &r in &roots {
                    let pn = &active.plan.nodes[r];
                    let (lt, _) = local_time(pn, tf);
                    let id = pn.id.clone();
                    let mut st = self.sims.remove(&id).unwrap();
                    // Blank before its start; held while hidden.
                    if lt < num(&doc.nodes[&id], "start", lt, 0.0) {
                        self.release(std::mem::take(&mut st.fields));
                        st.steps = 0;
                    }
                    st.frame = f;
                    self.sims.insert(id, st);
                }
                continue;
            }
            let mask = crate::live_from(&active.plan, ts, &roots);
            self.frame_t = tf;
            self.encode_groups_masked(
                active,
                doc,
                ts,
                list,
                report,
                crate::Pass {
                    only: Some(&mask),
                    ..Default::default()
                },
            )?;
            report.sim_catchup_frames += 1;
        }
        self.frame_t = t;
        Ok(())
    }

    /// Runs a simulation group for this frame: steps it if it is exactly one
    /// frame behind, then upsamples its image into `out`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_sim(
        &mut self,
        plan: &Plan,
        gi: usize,
        inputs: &[Placed],
        out: &Texture,
        doc: &Document,
        t: f64,
        list: &mut CommandList,
        report: &mut FrameReport,
    ) -> Result<(), RenderError> {
        let g = &plan.groups[gi];
        let pn = &plan.nodes[g.root];
        let id = pn.id.clone();
        let node = &doc.nodes[&id];
        let fps = doc.format.fps;
        let frame = frame_of(self.frame_t, fps);
        let key = self.sim_key(plan, doc, g.root);
        let mut st = self.sims.remove(&id).unwrap_or_else(|| SimState::new(key));
        if st.key != key {
            st.key = key;
            st.exact = false;
        }
        let (lt, on) = local_time(pn, t);
        if st.frame == frame - 1 {
            let start = num(node, "start", lt, 0.0);
            if !on || lt < start {
                // Before its start the simulation is blank; while hidden it
                // holds still.
                if lt < start {
                    self.release(std::mem::take(&mut st.fields));
                    st.steps = 0;
                }
            } else {
                let ports = crate::ops::connected_ports(pn.def, &id, node)?;
                let port = |name: &str| ports.iter().position(|p| p == name).map(|i| &inputs[i]);
                let stepped = if node.op == "fluid" {
                    self.fluid_step(&mut st, doc, node, lt, port("dye"), port("stir"), list)?;
                    true
                } else {
                    let ins: Vec<Option<&Placed>> = (0..4).map(|i| port(&format!("in{i}"))).collect();
                    self.feedback_step(&mut st, &g.key, pn.tables.as_deref(), doc, node, lt, &ins, list, &id)?
                };
                if stepped {
                    st.steps += 1;
                    report.sim_steps += 1;
                } else {
                    // The step kernel is still compiling (live only).
                    st.exact = false;
                }
            }
            st.frame = frame;
            // An export only moves forward: checkpoints (for seeking back)
            // would only hold memory.
            if frame % CHECK_EVERY == 0 && !st.fields.is_empty() && !self.sequential {
                let fields = self.copy_fields(&st.fields.clone(), list);
                st.checkpoints.retain(|c| c.frame < frame);
                st.checkpoints.push(Checkpoint {
                    frame,
                    steps: st.steps,
                    exact: st.exact,
                    fields,
                });
                if st.checkpoints.len() > MAX_CHECKPOINTS {
                    let old = std::mem::take(&mut st.checkpoints);
                    for (i, c) in old.into_iter().enumerate() {
                        if i % 2 == 1 || i == 0 {
                            st.checkpoints.push(c);
                        } else {
                            self.release(c.fields);
                        }
                    }
                }
            }
        }
        // The image: the dye (fluid) or the state (feedback), upsampled;
        // nothing while the node is outside its window.
        let image = match (on, node.op.as_str()) {
            (false, _) => None,
            (true, "fluid") => st.fields.get(1),
            (true, _) => st.fields.first(),
        };
        match image {
            Some(img) => {
                let mut p = header(doc, t).to_vec();
                p.extend([1.0, 0.0, 0.0, 0.0, 0.0, texel(doc, &img.desc()), 1.0]);
                list.dispatch(&self.kernels.utility("k_resample"), out, &[img], &p);
            }
            None => list.dispatch(&self.kernels.utility("k_clear"), out, &[], &header(doc, t)),
        }
        self.sims.insert(id, st);
        Ok(())
    }

    /// Fresh zeroed fields when the simulation is blank or changed size.
    fn ensure_fields(&mut self, st: &mut SimState, descs: &[TextureDesc], doc: &Document, list: &mut CommandList) {
        let ok = st.fields.len() == descs.len() && st.fields.iter().zip(descs).all(|(f, d)| f.desc() == *d);
        if ok {
            return;
        }
        self.release(std::mem::take(&mut st.fields));
        let clear = self.kernels.utility("k_clear");
        for d in descs {
            let f = self.pool.lease(&self.device, *d);
            let s = texel(doc, d);
            list.dispatch(
                &clear,
                &f,
                &[],
                &[0.0, s, doc.format.width as f32, doc.format.height as f32],
            );
            st.fields.push(f);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fluid_step(
        &mut self,
        st: &mut SimState,
        doc: &Document,
        node: &Node,
        t: f64,
        dye_in: Option<&Placed>,
        stir_in: Option<&Placed>,
        list: &mut CommandList,
    ) -> Result<(), RenderError> {
        let descs = layout(doc, node, t);
        self.ensure_fields(st, &descs, doc, list);
        let (vd, dd) = (descs[0], descs[1]);
        let (sv, sd) = (texel(doc, &vd), texel(doc, &dd));
        let (w, h) = (doc.format.width as f32, doc.format.height as f32);
        let substeps = num(node, "substeps", t, 1.0).clamp(1.0, 8.0) as usize;
        let dt = (1.0 / doc.format.fps / substeps as f64) as f32;
        let iters = num(node, "pressure_iterations", t, 24.0).clamp(1.0, 200.0) as usize;
        let vorticity = num(node, "vorticity", t, 4.0) as f32;
        let turbulence = num(node, "turbulence", t, 0.0) as f32;
        let turb_scale = num(node, "turbulence_scale", t, 260.0) as f32;
        let seed = num(node, "seed", t, 1.0) as f32;
        let buoyancy = num(node, "buoyancy", t, 0.0) as f32;
        let wind = vec2(node, "wind", t, [0.0, 0.0]);
        let push = num(node, "stir", t, 2500.0) as f32;
        let swirl = num(node, "swirl", t, 0.0) as f32;
        let emit = num(node, "emit", t, 6.0);
        let diss = num(node, "dissipation", t, 0.15);
        let vdiss = num(node, "velocity_dissipation", t, 0.4);
        let k = |name: &str| self.kernels.utility(name);
        let (k_curl, k_force, k_div, k_jac, k_proj, k_adv) = (
            k("k_fl_curl"),
            k("k_fl_force"),
            k("k_fl_div"),
            k("k_fl_jacobi"),
            k("k_fl_project"),
            k("k_fl_advect"),
        );
        let none = self.placeholder.clone();
        let (stir_tex, stir_origin) = match stir_in {
            Some(p) => (p.tex.clone(), p.origin),
            None => (none.clone(), [0.0, 0.0]),
        };
        let (dye_tex, dye_origin) = match dye_in {
            Some(p) => (p.tex.clone(), p.origin),
            None => (none.clone(), [0.0, 0.0]),
        };
        for s in 0..substeps {
            let ts = t + s as f64 * dt as f64;
            let base = |texel: f32| vec![ts as f32, texel, w, h, dt, sv, sd];
            // Forces.
            let curl = self.pool.lease(&self.device, vd);
            list.dispatch(&k_curl, &curl, &[&st.fields[0]], &base(sv));
            let v = self.pool.lease(&self.device, vd);
            let mut p = base(sv);
            p.extend([
                vorticity,
                turbulence,
                turb_scale,
                (ts * 0.12) as f32 + seed * 17.0,
                buoyancy,
                stir_origin[0],
                stir_origin[1],
                push,
                swirl,
                stir_in.is_some() as u8 as f32,
            ]);
            list.dispatch(&k_force, &v, &[&st.fields[0], &curl, &st.fields[1], &stir_tex], &p);
            self.pool.give_back(curl);
            let old = std::mem::replace(&mut st.fields[0], v);
            self.pool.give_back(old);
            // Pressure projection.
            let div = self.pool.lease(&self.device, vd);
            list.dispatch(&k_div, &div, &[&st.fields[0]], &base(sv));
            for i in 0..iters {
                let pn = self.pool.lease(&self.device, vd);
                let mut p = base(sv);
                p.push(if i == 0 { 0.8 } else { 1.0 });
                list.dispatch(&k_jac, &pn, &[&st.fields[2], &div], &p);
                let old = std::mem::replace(&mut st.fields[2], pn);
                self.pool.give_back(old);
            }
            self.pool.give_back(div);
            let v = self.pool.lease(&self.device, vd);
            list.dispatch(&k_proj, &v, &[&st.fields[0], &st.fields[2]], &base(sv));
            let old = std::mem::replace(&mut st.fields[0], v);
            self.pool.give_back(old);
            // Advection: velocity through itself, then the dye, with the
            // source laid in.
            let v = self.pool.lease(&self.device, vd);
            let mut p = base(sv);
            p.extend([sv, wind[0] as f32, wind[1] as f32, 0.0, 0.0]);
            p.extend([(-vdiss * dt as f64).exp() as f32, 0.0, 0.0]);
            list.dispatch(&k_adv, &v, &[&st.fields[0], &st.fields[0], &none], &p);
            let d = self.pool.lease(&self.device, dd);
            let mut p = base(sd);
            p.extend([sd, wind[0] as f32, wind[1] as f32, dye_origin[0], dye_origin[1]]);
            p.extend([
                (-diss * dt as f64).exp() as f32,
                (1.0 - (-emit * dt as f64).exp()) as f32,
                dye_in.is_some() as u8 as f32,
            ]);
            list.dispatch(&k_adv, &d, &[&st.fields[0], &st.fields[1], &dye_tex], &p);
            let old = std::mem::replace(&mut st.fields[0], v);
            self.pool.give_back(old);
            let old = std::mem::replace(&mut st.fields[1], d);
            self.pool.give_back(old);
        }
        Ok(())
    }

    /// One frame of a feedback node. Returns false when its kernel is not
    /// compiled yet (live mode only; export waits).
    #[allow(clippy::too_many_arguments)]
    fn feedback_step(
        &mut self,
        st: &mut SimState,
        key: &str,
        tables: Option<&[f32]>,
        doc: &Document,
        node: &Node,
        t: f64,
        inputs: &[Option<&Placed>],
        list: &mut CommandList,
        id: &str,
    ) -> Result<bool, RenderError> {
        use crate::codegen::feedback_params as fp;
        use crate::kernels::KernelState;
        let kernel = match self.mode {
            Mode::Exact => self.kernels.wait(key),
            Mode::Live => self.kernels.get(key).unwrap_or(KernelState::Pending),
        };
        let kernel = match kernel {
            KernelState::Ready(k) => k,
            KernelState::Pending => return Ok(false),
            KernelState::Failed(e) => {
                self.diagnose(id, e);
                return Ok(false);
            }
        };
        let descs = layout(doc, node, t);
        self.ensure_fields(st, &descs, doc, list);
        let d = descs[0];
        let s = texel(doc, &d);
        let substeps = num(node, "substeps", t, 1.0).clamp(1.0, 64.0) as usize;
        let dt = 1.0 / doc.format.fps / substeps as f64;
        let mut u = [0.0f32; 16];
        if let Some(v) = node.params.get("u").and_then(|p| p.eval(t)) {
            for (i, x) in v.iter().take(16).enumerate() {
                u[i] = *x as f32;
            }
        }
        for (i, slot) in u.iter_mut().enumerate() {
            if let Some(v) = node.params.get(&format!("u{i}")).and_then(|p| p.eval(t)) {
                *slot = v.first().copied().unwrap_or(0.0) as f32;
            }
        }
        let none = self.placeholder.clone();
        let texs: Vec<Texture> = inputs
            .iter()
            .map(|p| p.map(|p| p.tex.clone()).unwrap_or(none.clone()))
            .collect();
        for k in 0..substeps {
            let mut p = vec![0.0f32; fp::LEN];
            p[0] = t as f32;
            p[1] = s;
            p[2] = doc.format.width as f32;
            p[3] = doc.format.height as f32;
            p[fp::U..fp::U + 16].copy_from_slice(&u);
            p[fp::TIME] = (t + k as f64 * dt) as f32;
            p[fp::DT] = dt as f32;
            p[fp::STEP] = (st.steps * substeps as u64 + k as u64) as f32;
            for (i, inp) in inputs.iter().enumerate() {
                if let Some(pl) = inp {
                    p[fp::ORIGINS + 2 * i] = pl.origin[0];
                    p[fp::ORIGINS + 2 * i + 1] = pl.origin[1];
                }
            }
            p.extend_from_slice(tables.unwrap_or_default());
            let next = self.pool.lease(&self.device, d);
            list.dispatch(
                &kernel,
                &next,
                &[&st.fields[0], &texs[0], &texs[1], &texs[2], &texs[3]],
                &p,
            );
            let old = std::mem::replace(&mut st.fields[0], next);
            self.pool.give_back(old);
        }
        Ok(true)
    }
}
