//! `ultrafast review`: an automatic second pair of eyes for agents. Renders the
//! film and reports what a viewer would trip over: text that never becomes
//! readable, is on screen too briefly to read, runs off the frame or into
//! the platform UI, collides with other text, or is too small; and flashing
//! that risks photosensitive viewers.
//!
//! Every check is measured on the rendered frames (not guessed from
//! params): a text's contrast is the final image under its glyphs against
//! the final image around them, so masks, crops, fades and layers on top
//! all count.

use crate::{Placed, RenderError, Renderer, local_time};
use serde::Serialize;
use ultrafast_gpu::{PixelFormat, TextureDesc};
use ultrafast_graph::Document;

/// What one text looked like in one rendered frame.
#[derive(Debug, Clone)]
pub struct TextProbe {
    pub node: String,
    pub text: String,
    /// Glyph bounds in frame px (may extend past the frame).
    pub bounds: [f32; 4],
    /// Sampled glyph pixels, and how many of them fall outside the frame.
    pub ink: usize,
    pub clipped: usize,
    /// WCAG contrast ratio of the final frame under the glyphs against
    /// around them: 1 = invisible, 3 = readable large text, 21 = max.
    pub contrast: f32,
    /// Type size in frame px (size param times absorbed scale).
    pub size_px: f32,
    /// Frame indices of sampled glyph pixels, sorted (overlap tests).
    pub pixels: Vec<u32>,
    /// Glanced at rather than read (`skim` on the text: code, data
    /// labels): no reading-time check.
    pub skim: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SafeArea {
    /// Keep text this fraction of the short side away from the edges.
    Margin(f32),
    /// Vertical social video: clear of the caption and button overlays
    /// (bottom 20 %, right 12 %, top 8 %).
    Social,
}

impl SafeArea {
    /// The rectangle text should stay inside, `[x0, y0, x1, y1]` in frame
    /// px. Captions keep to the same one by default (see `captions`).
    pub fn rect(self, fw: f64, fh: f64) -> [f64; 4] {
        let short = fw.min(fh);
        match self {
            SafeArea::Margin(m) => {
                let m = m as f64 * short;
                [m, m, fw - m, fh - m]
            }
            SafeArea::Social => [0.04 * short, 0.08 * fh, fw - 0.12 * fw, fh - 0.2 * fh],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReviewOptions {
    /// Seconds between text probes (flashes are checked every frame).
    pub step: f64,
    pub safe: SafeArea,
}

impl Default for ReviewOptions {
    fn default() -> Self {
        Self {
            step: 0.1,
            safe: SafeArea::Margin(0.04),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub severity: &'static str,
    pub kind: &'static str,
    pub node: String,
    pub from: f64,
    pub to: f64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Review {
    pub issues: Vec<Issue>,
    pub frames: u64,
    pub texts: usize,
    pub seconds: f64,
}

/// Relative luminance of an sRGB8 pixel.
fn luminance(p: &[u8]) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2])
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let m = v.len() / 2;
    v.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
    Some(v[m])
}

/// Sampling stride over glyph texels.
const STRIDE: usize = 2;

impl Renderer {
    /// Renders frame `t` and measures every visible text in it. Returns the
    /// frame (sRGB RGBA8, opaque) and the probes.
    pub fn probe_texts(&mut self, doc: &Document, t: f64) -> Result<(Vec<u8>, Vec<TextProbe>), RenderError> {
        // Text rasters drawn whole, not stopped at the frame: what runs off
        // it is what the probe measures.
        let framed = std::mem::replace(&mut self.no_framed, true);
        let rendered = self.render_rgba8(doc, t);
        self.no_framed = framed;
        let (frame, _) = rendered?;
        let flat = if crate::needs_flat(doc) {
            Some(self.flattened(doc)?)
        } else {
            None
        };
        let doc: &Document = flat.as_ref().map(|f| &f.doc).unwrap_or(doc);
        let (fw, fh) = (doc.format.width as i64, doc.format.height as i64);
        let active = self.active.take().expect("plan");
        let live = crate::live_groups(&active.plan, t);
        let mut found: Vec<(String, Placed, f32, Option<Hom>)> = Vec::new();
        let plan = &active.plan;
        let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); plan.nodes.len()];
        for (i, n) in plan.nodes.iter().enumerate() {
            for &s in &n.inputs {
                consumers[s].push(i);
            }
        }
        for (gi, g) in active.plan.groups.iter().enumerate() {
            let Some(rc) = &g.raster else { continue };
            let pn = &active.plan.nodes[rc.node];
            if pn.def.name != "text" || !live[gi] || !local_time(pn, t).1 {
                continue;
            }
            // Skip type hidden on its way to the output (cropped away,
            // faded out): it is not on screen, whatever its raster holds.
            if let Some(b) = self.bounds.get(&pn.id) {
                let rect = [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64];
                if !reaches_screen(plan, doc, &consumers, g.root, rect, 1.0, t) {
                    continue;
                }
            }
            let Some(placed) = active.raster[gi].clone() else {
                continue;
            };
            // Pins and transforms after the raster move its type on screen:
            // it is measured where they put it.
            let map = to_screen(plan, doc, &consumers, g.root, &rc.transforms, t);
            let node = &doc.nodes[&pn.id];
            let (lt, _) = local_time(pn, t);
            let outer = crate::absorbed(&active.plan, doc, &rc.transforms, t);
            let m = outer.mul(crate::geom::placement_affine(node, lt)).0;
            let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
            let size = node
                .params
                .get("size")
                .and_then(|p| p.eval(lt))
                .and_then(|v| v.first().copied())
                .unwrap_or(64.0);
            found.push((pn.id.clone(), placed, (size * scale) as f32, map));
        }
        self.active = Some(active);

        // Luminance of the whole frame, once.
        let lut: Vec<f32> = (0..256u32)
            .map(|c| {
                let c = c as f32 / 255.0;
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            })
            .collect();
        let lum: Vec<f32> = frame
            .chunks_exact(4)
            .map(|p| 0.2126 * lut[p[0] as usize] + 0.7152 * lut[p[1] as usize] + 0.0722 * lut[p[2] as usize])
            .collect();
        let at = |x: i64, y: i64| -> Option<f32> {
            (x >= 0 && y >= 0 && x < fw && y < fh).then(|| lum[(y * fw + x) as usize])
        };
        let mut probes = Vec::new();
        for (id, placed, raster_px, map) in found {
            let d = placed.tex.desc();
            let rgba = self.device.create_texture(TextureDesc {
                width: d.width,
                height: d.height,
                format: PixelFormat::Rgba8Unorm,
                cpu_readable: true,
            });
            let mut list = self.device.begin();
            list.dispatch(
                &self.kernels.utility("k_encode_srgb8"),
                &rgba,
                &[&placed.tex],
                &[0.0, 1.0, d.width as f32, d.height as f32, 0.0, 0.0, 0.0],
            );
            list.submit().wait()?;
            let px = self.device.readback(&rgba);
            let (ox, oy) = (placed.origin[0] as i64, placed.origin[1] as i64);
            let (tw, th) = (d.width as usize, d.height as usize);
            // Frame px of the raster to screen px, and how much it scales the
            // type there (the pin's local scale at the raster's middle).
            let to = |x: i64, y: i64| -> Option<(i64, i64)> {
                match &map {
                    None => Some((x, y)),
                    Some(m) => hom_apply(m, x as f64, y as f64).map(|(x, y)| (x.round() as i64, y.round() as i64)),
                }
            };
            let zoom = map
                .as_ref()
                .and_then(|m| {
                    let (cx, cy) = (ox as f64 + tw as f64 / 2.0, oy as f64 + th as f64 / 2.0);
                    let (a, b, c) = (
                        hom_apply(m, cx, cy)?,
                        hom_apply(m, cx + 1.0, cy)?,
                        hom_apply(m, cx, cy + 1.0)?,
                    );
                    let det = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
                    Some(det.abs().sqrt())
                })
                .unwrap_or(1.0) as f32;
            let size_px = raster_px * zoom;
            // Glyph texels (solid ones: edges mix with the ground) and the
            // ground between and around them, in screen px.
            let mut solid = Vec::new();
            let mut edge = Vec::new();
            let mut ground = Vec::new();
            let mut edge_ground = Vec::new();
            let near_r = ((raster_px * 0.05).round() as i64).clamp(2, 8);
            // The ink's bounds in the raster's frame px.
            let mut rb = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
            let mut pixels = Vec::new();
            let (mut ink, mut clipped) = (0usize, 0usize);
            let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            let inside = |x: i64, y: i64| x >= 0 && y >= 0 && x < fw && y < fh;
            // Coverage relative to the text's own opacity (translucent type
            // is still type).
            let peak = px.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0).max(1) as u32;
            let cov = |a: u8| (a as u32 * 255 / peak).min(255) as u8;
            for y in (0..th).step_by(STRIDE) {
                for x in (0..tw).step_by(STRIDE) {
                    let a = cov(px[(y * tw + x) * 4 + 3]);
                    let (rx, ry) = (ox + x as i64, oy + y as i64);
                    let Some((fx, fy)) = to(rx, ry) else {
                        if a > 128 {
                            ink += 1;
                            clipped += 1;
                        }
                        continue;
                    };
                    if a > 128 {
                        ink += 1;
                        rb = [rb[0].min(rx), rb[1].min(ry), rb[2].max(rx), rb[3].max(ry)];
                        b = [
                            b[0].min(fx as f32),
                            b[1].min(fy as f32),
                            b[2].max(fx as f32),
                            b[3].max(fy as f32),
                        ];
                        if inside(fx, fy) {
                            pixels.push((fy * fw + fx) as u32);
                            if a > 220 {
                                solid.push((fx, fy))
                            } else {
                                edge.push((fx, fy))
                            }
                        } else {
                            clipped += 1;
                        }
                    } else if a < 5 && inside(fx, fy) {
                        // The ground right at the letters' edges is what
                        // they are read against (an outline or a shadow
                        // there counts); farther ground only if none.
                        let alpha = |x: i64, y: i64| {
                            if x < 0 || y < 0 || x >= tw as i64 || y >= th as i64 {
                                0
                            } else {
                                cov(px[(y as usize * tw + x as usize) * 4 + 3])
                            }
                        };
                        let r = near_r;
                        let (xi, yi) = (x as i64, y as i64);
                        let near = [(r, 0), (-r, 0), (0, r), (0, -r), (r, r), (-r, -r), (r, -r), (-r, r)]
                            .iter()
                            .any(|&(dx, dy)| alpha(xi + dx, yi + dy) > 128);
                        if near {
                            edge_ground.push((fx, fy));
                        } else {
                            ground.push((fx, fy));
                        }
                    }
                }
            }
            if ink == 0 {
                continue;
            }
            let pad = (size_px * 0.15).clamp(4.0, 40.0) as i64;
            let (x0, y0) = (b[0] as i64 - pad, b[1] as i64 - pad);
            // The ground past the raster's edges, out to `pad` screen px
            // around the ink: walked in the raster's frame px, then put on
            // screen.
            let rpad = (pad as f32 / zoom.max(1e-3)).ceil() as i64;
            for y in (rb[1] - rpad..=rb[3] + rpad).step_by(STRIDE) {
                for x in (rb[0] - rpad..=rb[2] + rpad).step_by(STRIDE) {
                    let within = x >= ox && y >= oy && x < ox + tw as i64 && y < oy + th as i64;
                    if within {
                        continue;
                    }
                    if let Some((sx, sy)) = to(x, y).filter(|&(x, y)| inside(x, y)) {
                        ground.push((sx, sy));
                    }
                }
            }
            if solid.len() < 30 {
                solid.extend(edge);
            }
            // Effects downstream (lens warps, refraction, glitches) move the
            // glyphs a little, by different amounts along a line: each tile
            // of the text is measured at the offset where it lines up best.
            let reach = (size_px * 0.3).clamp(3.0, 28.0) as i64;
            let stride = (reach / 5).max(1);
            let tile = (size_px * 1.5).max(48.0) as i64;
            let key = |(x, y): (i64, i64)| ((x - x0).div_euclid(tile), (y - y0).div_euclid(tile));
            let contrast_against = |ground: &[(i64, i64)]| -> f32 {
                let mut tiles: std::collections::HashMap<(i64, i64), (Vec<(i64, i64)>, Vec<(i64, i64)>)> =
                    Default::default();
                for &p in &solid {
                    tiles.entry(key(p)).or_default().0.push(p);
                }
                for &p in ground {
                    if let Some(t) = tiles.get_mut(&key(p)) {
                        t.1.push(p);
                    }
                }
                let mut ink_l = Vec::new();
                let mut ground_l = Vec::new();
                for (ink_px, ground_px) in tiles.values() {
                    if ground_px.is_empty() {
                        continue;
                    }
                    let sample = |dx: i64, dy: i64, every: usize| -> (Vec<f32>, Vec<f32>) {
                        (
                            ink_px
                                .iter()
                                .step_by(every)
                                .filter_map(|&(x, y)| at(x + dx, y + dy))
                                .collect(),
                            ground_px
                                .iter()
                                .step_by(every)
                                .filter_map(|&(x, y)| at(x + dx, y + dy))
                                .collect(),
                        )
                    };
                    let score = |(a, mut g): (Vec<f32>, Vec<f32>)| -> f32 {
                        let m = if a.is_empty() {
                            return 1.0;
                        } else {
                            a.iter().sum::<f32>() / a.len() as f32
                        };
                        match median(&mut g) {
                            Some(g) => (m.max(g) + 0.05) / (m.min(g) + 0.05),
                            None => 1.0,
                        }
                    };
                    let every = ((ink_px.len() + ground_px.len()) / 400).max(1);
                    let mut best = (0, 0, score(sample(0, 0, every)));
                    let mut dy = -reach;
                    while dy <= reach {
                        let mut dx = -reach;
                        while dx <= reach {
                            let c = score(sample(dx, dy, every));
                            if c > best.2 + 1e-4 {
                                best = (dx, dy, c);
                            }
                            dx += stride;
                        }
                        dy += stride;
                    }
                    let (a, g) = sample(best.0, best.1, 1);
                    ink_l.extend(a);
                    ground_l.extend(g);
                }
                // Ink at its typical level (mean: the eye integrates scanlines
                // and halftones), ground at its median (stray highlights around
                // the type do not count).
                if ink_l.is_empty() {
                    1.0
                } else {
                    let m = ink_l.iter().sum::<f32>() / ink_l.len() as f32;
                    match median(&mut ground_l) {
                        Some(g) => (m.max(g) + 0.05) / (m.min(g) + 0.05),
                        None => 1.0,
                    }
                }
            };
            // Read against the ground right at the letters' edges (an outline
            // or shadow there counts) or the ground beyond (a glow around
            // them does not hide them): whichever reads better.
            let far: Vec<(i64, i64)> = ground.clone();
            let contrast = if edge_ground.len() >= 30 {
                contrast_against(&edge_ground).max(contrast_against(&far))
            } else {
                let mut all = far;
                all.extend(edge_ground);
                contrast_against(&all)
            };
            pixels.sort_unstable();
            let text = crate::geom::display_text(&doc.nodes[&id], t);
            let skim = matches!(
                doc.nodes[&id].params.get("skim"),
                Some(ultrafast_graph::Param::Bool(true))
            );
            probes.push(TextProbe {
                node: id,
                text,
                bounds: b,
                ink,
                clipped,
                contrast,
                size_px,
                pixels,
                skim,
            });
        }
        Ok((frame, probes))
    }
}

/// A 3x3 projective map of frame px, row-major (`x' = (m0 x + m1 y + m2) /
/// (m6 x + m7 y + m8)`).
type Hom = [f64; 9];

fn hom_mul(a: &Hom, b: &Hom) -> Hom {
    let mut m = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            m[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    m
}

fn hom_apply(m: &Hom, x: f64, y: f64) -> Option<(f64, f64)> {
    let w = m[6] * x + m[7] * y + m[8];
    // Behind the pin's horizon: nowhere on screen.
    (w > 1e-9).then(|| ((m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w))
}

/// Where a geometric op moves frame px, active at `t`: a `transform` (an
/// affine) or a `corner_pin` (its source rect onto its corners). None for
/// everything else.
fn op_map(pn: &crate::plan::PlanNode, doc: &Document, t: f64) -> Option<Hom> {
    let node = &doc.nodes[&pn.id];
    let (lt, on) = local_time(pn, t);
    if !on {
        return None;
    }
    match pn.def.name {
        "transform" => {
            let m = crate::ops::transform_affine(node, &doc.format, lt).0;
            Some([m[0], m[2], m[4], m[1], m[3], m[5], 0.0, 0.0, 1.0])
        }
        "corner_pin" => {
            let (w, h) = (doc.format.width as f64, doc.format.height as f64);
            let vec = |k: &str, d: [f64; 8]| -> Vec<f64> {
                node.params
                    .get(k)
                    .and_then(|p| p.eval(lt))
                    .filter(|v| v.len() >= d.len().min(4))
                    .unwrap_or_else(|| d.to_vec())
            };
            let c = vec("corners", [0.0, 0.0, w, 0.0, w, h, 0.0, h]);
            let s = vec("source_rect", [0.0, 0.0, w, h, 0.0, 0.0, 0.0, 0.0]);
            if c.len() < 8 || s.len() < 4 {
                return None;
            }
            let from = [
                [s[0], s[1]],
                [s[0] + s[2], s[1]],
                [s[0] + s[2], s[1] + s[3]],
                [s[0], s[1] + s[3]],
            ];
            let to = [[c[0], c[1]], [c[2], c[3]], [c[4], c[5]], [c[6], c[7]]];
            crate::ops::homography(&from, &to)
        }
        _ => None,
    }
}

/// The bounding rect of `rect` mapped by `m` (None if part of it falls
/// behind a pin's horizon).
fn map_rect(m: &Hom, r: [f64; 4]) -> Option<[f64; 4]> {
    let pts: Vec<(f64, f64)> = [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])]
        .iter()
        .map(|&(x, y)| hom_apply(m, x, y))
        .collect::<Option<_>>()?;
    Some([
        pts.iter().map(|p| p.0).fold(f64::MAX, f64::min),
        pts.iter().map(|p| p.1).fold(f64::MAX, f64::min),
        pts.iter().map(|p| p.0).fold(f64::MIN, f64::max),
        pts.iter().map(|p| p.1).fold(f64::MIN, f64::max),
    ])
}

/// Work enough for any real document's walk to its output; past it a
/// text is taken to be on screen.
const WALK_STEPS: usize = 200_000;

/// Whether node `i`'s content, covering `rect` (frame px) at opacity
/// `alpha`, survives the crops, fades and windows between it and the
/// output, on some path to it: the rect moves with the transforms and
/// corner pins on the way. However deep the stack (the walk once gave up
/// after 48 levels and took the rest for visible).
fn reaches_screen(
    plan: &crate::plan::Plan,
    doc: &Document,
    consumers: &[Vec<usize>],
    i: usize,
    rect: [f64; 4],
    alpha: f64,
    t: f64,
) -> bool {
    let out = plan.nodes.len() - 1;
    // What reached each node already: a later arrival no more opaque and
    // no larger is not walked again.
    let mut seen: Vec<Vec<(f64, [f64; 4])>> = vec![Vec::new(); plan.nodes.len()];
    let mut stack = vec![(i, rect, alpha)];
    let mut steps = 0;
    while let Some((i, r, a)) = stack.pop() {
        if a < 0.05 || r[0] >= r[2] || r[1] >= r[3] {
            continue;
        }
        if i == out {
            return true;
        }
        steps += 1;
        if steps > WALK_STEPS {
            return true;
        }
        let inside = |o: &[f64; 4]| r[0] >= o[0] && r[1] >= o[1] && r[2] <= o[2] && r[3] <= o[3];
        if seen[i].iter().any(|(sa, sr)| a <= *sa && inside(sr)) {
            continue;
        }
        seen[i].push((a, r));
        for &c in &consumers[i] {
            let pn = &plan.nodes[c];
            let node = &doc.nodes[&pn.id];
            let (lt, on) = local_time(pn, t);
            let port = pn.inputs.iter().position(|&x| x == i).unwrap_or(0);
            if !on {
                // An inactive node passes one input through, or nothing.
                if crate::ops::passthrough(pn.def, pn.inputs.len()) == Some(port) {
                    stack.push((c, r, a));
                }
                continue;
            }
            let num = |k: &str, d: f64| {
                node.params
                    .get(k)
                    .and_then(|p| p.eval(lt))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(d)
            };
            let (mut r, mut a) = (r, a);
            match pn.def.name {
                "crop" => {
                    let cr = node.params.get("rect").and_then(|p| p.eval(lt)).unwrap_or_default();
                    if cr.len() >= 4 {
                        r = [
                            r[0].max(cr[0]),
                            r[1].max(cr[1]),
                            r[2].min(cr[0] + cr[2]),
                            r[3].min(cr[1] + cr[3]),
                        ];
                    }
                }
                "opacity" => a *= num("opacity", 1.0),
                "over" | "blend" if port == 0 => a *= num("opacity", 1.0),
                "transform" | "corner_pin" => {
                    if let Some(m) = op_map(pn, doc, t) {
                        match map_rect(&m, r) {
                            Some(m) => r = m,
                            None => continue,
                        }
                    }
                }
                _ => {}
            }
            stack.push((c, r, a));
        }
    }
    false
}

/// Where node `i`'s frame px land on screen: the transforms and corner pins
/// between it and the output (but those in `absorbed`, already drawn into
/// its raster), composed along its first path to the output. None when
/// nothing on the way moves it.
fn to_screen(
    plan: &crate::plan::Plan,
    doc: &Document,
    consumers: &[Vec<usize>],
    i: usize,
    absorbed: &[usize],
    t: f64,
) -> Option<Hom> {
    let out = plan.nodes.len() - 1;
    let identity: Hom = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut seen = vec![false; plan.nodes.len()];
    let mut stack = vec![(i, identity, false)];
    let mut steps = 0;
    while let Some((i, m, moved)) = stack.pop() {
        if i == out {
            return moved.then_some(m);
        }
        steps += 1;
        if seen[i] || steps > WALK_STEPS {
            continue;
        }
        seen[i] = true;
        // Pushed in reverse, so the first consumer is walked first.
        for &c in consumers[i].iter().rev() {
            let step = (!absorbed.contains(&c))
                .then(|| op_map(&plan.nodes[c], doc, t))
                .flatten();
            match step {
                Some(s) => stack.push((c, hom_mul(&s, &m), true)),
                None => stack.push((c, m, moved)),
            }
        }
    }
    None
}

/// Visible: distinguishable from what is around it. Readable: WCAG AA for
/// large text.
const VISIBLE: f32 = 1.5;
const READABLE: f32 = 3.0;

fn mean_luminance(frame: &[u8]) -> f32 {
    let n = frame.len() / 4;
    let step = (n / 20000).max(1);
    let mut s = 0.0;
    let mut k = 0;
    for i in (0..n).step_by(step) {
        s += luminance(&frame[i * 4..i * 4 + 4]);
        k += 1;
    }
    s / k.max(1) as f32
}

fn words(text: &str) -> usize {
    text.split_whitespace().count().max(1)
}

/// Seconds a viewer needs to read `text`: about 270 words a minute plus a
/// moment to find it (a single bold word reads in about half a second).
fn reading_time(text: &str) -> f64 {
    0.25 + 0.22 * words(text) as f64
}

/// Longest run of consecutive samples satisfying `ok`, as (from, to).
fn longest_run(samples: &[(f64, bool)], step: f64) -> Option<(f64, f64)> {
    let mut best: Option<(f64, f64)> = None;
    let mut start: Option<f64> = None;
    let mut last = 0.0;
    for &(t, ok) in samples {
        if ok {
            if start.is_none() {
                start = Some(t);
            }
            last = t;
        } else if let Some(s) = start.take()
            && best.is_none_or(|(a, b)| last - s > b - a)
        {
            best = Some((s, last));
        }
    }
    if let Some(s) = start
        && best.is_none_or(|(a, b)| last - s > b - a)
    {
        best = Some((s, last));
    }
    best.map(|(a, b)| (a, b + step))
}

/// Spans (at least `min` long) where `bad` holds, as (from, to).
fn spans(samples: &[(f64, bool)], step: f64, min: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start: Option<f64> = None;
    let mut last = 0.0;
    for &(t, bad) in samples.iter().chain(std::iter::once(&(f64::MAX, false))) {
        if bad {
            start.get_or_insert(t);
            last = t;
        } else if let Some(s) = start.take()
            && last + step - s >= min - 1e-9
        {
            out.push((s, last + step));
        }
    }
    out
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let i = w * h;
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    i / (area(a) + area(b) - i).max(1e-6)
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Renders the whole film and reports problems.
pub fn review(r: &mut Renderer, doc: &Document, opts: &ReviewOptions) -> Result<Review, RenderError> {
    let start = std::time::Instant::now();
    let fmt = doc.format;
    let total = (fmt.duration * fmt.fps).round() as u64;
    let every = ((opts.step * fmt.fps).round() as u64).max(1);
    let step = every as f64 / fmt.fps;
    let (fw, fh) = (fmt.width as f32, fmt.height as f32);
    let mut lumas = Vec::with_capacity(total as usize);
    // Per text node: (t, probe) samples.
    let mut per: std::collections::BTreeMap<String, Vec<(f64, TextProbe)>> = Default::default();
    let mut overlaps: std::collections::BTreeMap<(String, String), Vec<f64>> = Default::default();
    for f in 0..total {
        let t = fmt.time_of(f);
        if f % every == 0 {
            let (frame, probes) = r.probe_texts(doc, t)?;
            lumas.push(mean_luminance(&frame));
            for i in 0..probes.len() {
                for j in i + 1..probes.len() {
                    let (a, b) = (&probes[i], &probes[j]);
                    // Text crossing in a transition (fading, half there)
                    // is not a collision: only both fully legible is.
                    if a.contrast < READABLE || b.contrast < READABLE {
                        continue;
                    }
                    // Stacked copies of one text overlap by design.
                    if a.text == b.text && iou(a.bounds, b.bounds) > 0.6 {
                        continue;
                    }
                    let (small, big) = if a.pixels.len() < b.pixels.len() {
                        (a, b)
                    } else {
                        (b, a)
                    };
                    let shared = small
                        .pixels
                        .iter()
                        .filter(|p| big.pixels.binary_search(p).is_ok())
                        .count();
                    if shared as f32 > 0.03 * small.pixels.len() as f32 {
                        let key = if a.node < b.node {
                            (a.node.clone(), b.node.clone())
                        } else {
                            (b.node.clone(), a.node.clone())
                        };
                        overlaps.entry(key).or_default().push(t);
                    }
                }
            }
            for p in probes {
                per.entry(p.node.clone()).or_default().push((t, p));
            }
        } else {
            let (frame, _) = r.render_rgba8(doc, t)?;
            lumas.push(mean_luminance(&frame));
        }
    }

    let mut issues = Vec::new();
    let mut push = |severity, kind, node: &str, from: f64, to: f64, message: String| {
        issues.push(Issue {
            severity,
            kind,
            node: node.to_string(),
            from: r2(from),
            to: r2(to),
            message,
        })
    };
    let short = fw.min(fh);
    // Stacked copies (a shadow, an outline, a glow made from the same text)
    // are judged together: a copy is fine wherever a more legible copy of
    // the same text sits on it.
    let mut at_time: std::collections::HashMap<u64, Vec<&TextProbe>> = Default::default();
    for samples in per.values() {
        for (t, p) in samples {
            at_time.entry(t.to_bits()).or_default().push(p);
        }
    }
    let backed = |t: f64, p: &TextProbe| {
        at_time.get(&t.to_bits()).is_some_and(|ps| {
            ps.iter().any(|q| {
                q.node != p.node && q.text == p.text && q.contrast > p.contrast && iou(q.bounds, p.bounds) > 0.6
            })
        })
    };
    let per: std::collections::BTreeMap<String, Vec<(f64, TextProbe)>> = per
        .iter()
        .map(|(n, samples)| {
            let own: Vec<(f64, TextProbe)> = samples.iter().filter(|(t, p)| !backed(*t, p)).cloned().collect();
            (n.clone(), own)
        })
        .filter(|(_, v)| !v.is_empty())
        .collect();
    for (node, samples) in &per {
        let text = samples[0].1.text.clone();
        let label = if text.chars().count() > 40 {
            format!("\"{}…\"", text.chars().take(40).collect::<String>())
        } else {
            format!("\"{text}\"")
        };
        // Type set wider than the frame on both sides is texture (marquees,
        // patterns), not copy to be read.
        if samples
            .iter()
            .any(|(_, p)| (p.bounds[0] < 0.0 && p.bounds[2] >= fw) || (p.bounds[1] < 0.0 && p.bounds[3] >= fh))
        {
            continue;
        }
        let best = samples.iter().map(|(_, p)| p.contrast).fold(1.0f32, f32::max);
        if std::env::var("ULTRAFAST_DEBUG_REVIEW").is_ok() {
            let sz = samples.iter().map(|(_, p)| p.size_px).fold(f32::MAX, f32::min);
            eprintln!(
                "{node}: {} samples, best contrast {best:.2}, size {sz:.1}",
                samples.len()
            );
        }
        let first = samples[0].0;
        let last = samples.last().unwrap().0 + step;
        if best < VISIBLE {
            push(
                "info",
                "never_visible",
                node,
                first,
                last,
                format!("{label} never shows in the final image (fine if it is only a matte or source)"),
            );
            continue;
        }
        if best < READABLE {
            push(
                "warning",
                "low_contrast",
                node,
                first,
                last,
                format!(
                    "{label} never reaches readable contrast (best {best:.1}:1, want 3:1 or more): darken/lighten it, add a shadow or a backing panel"
                ),
            );
        } else {
            let readable: Vec<(f64, bool)> = samples.iter().map(|(t, p)| (*t, p.contrast >= READABLE)).collect();
            if let Some((a, b)) = longest_run(&readable, step) {
                let need = reading_time(&text);
                if b - a < need - 1e-9 && !samples[0].1.skim {
                    push(
                        if words(&text) <= 2 { "info" } else { "warning" },
                        "too_brief",
                        node,
                        a,
                        b,
                        format!(
                            "{label} is readable for {:.1} s; {} word(s) need about {:.1} s",
                            b - a,
                            words(&text),
                            need
                        ),
                    );
                }
            }
            // Dim stretches while it is otherwise readable.
            let dim: Vec<(f64, bool)> = samples
                .iter()
                .map(|(t, p)| (*t, p.contrast >= VISIBLE && p.contrast < READABLE))
                .collect();
            for (a, b) in spans(&dim, step, 0.6) {
                // A fade in or out (dim next to invisible) is a transition,
                // not a problem, unless it drags on.
                let before = samples
                    .iter()
                    .rev()
                    .find(|(t, _)| *t < a - 1e-9)
                    .map(|(_, p)| p.contrast);
                let after = samples.iter().find(|(t, _)| *t >= b - 1e-9).map(|(_, p)| p.contrast);
                let fading = before.is_none_or(|c| c < VISIBLE) || after.is_none_or(|c| c < VISIBLE);
                if fading && b - a < 1.2 {
                    continue;
                }
                let worst = samples
                    .iter()
                    .filter(|(t, _)| *t >= a && *t < b)
                    .map(|(_, p)| p.contrast)
                    .fold(f32::MAX, f32::min);
                push(
                    "warning",
                    "low_contrast",
                    node,
                    a,
                    b,
                    format!("{label} is hard to read here (down to {worst:.1}:1)"),
                );
            }
        }
        // Off the frame while visible.
        let off: Vec<(f64, bool)> = samples
            .iter()
            .map(|(t, p)| (*t, p.contrast >= VISIBLE && p.clipped as f32 > 0.02 * p.ink as f32))
            .collect();
        for (a, b) in spans(&off, step, 0.5) {
            let p = &samples.iter().find(|(t, _)| *t >= a).unwrap().1;
            let mut edges = Vec::new();
            if p.bounds[0] < 0.0 {
                edges.push(format!("left by {:.0} px", -p.bounds[0]));
            }
            if p.bounds[2] >= fw {
                edges.push(format!("right by {:.0} px", p.bounds[2] - fw + 1.0));
            }
            if p.bounds[1] < 0.0 {
                edges.push(format!("top by {:.0} px", -p.bounds[1]));
            }
            if p.bounds[3] >= fh {
                edges.push(format!("bottom by {:.0} px", p.bounds[3] - fh + 1.0));
            }
            push(
                "warning",
                "clipped",
                node,
                a,
                b,
                format!(
                    "{label} runs off the frame ({}; {:.0}% of the glyphs): make it smaller or move it in",
                    edges.join(", "),
                    100.0 * p.clipped as f32 / p.ink as f32
                ),
            );
        }
        // Safe area.
        let [l, t, rr, bb] = opts.safe.rect(fw as f64, fh as f64).map(|v| v as f32);
        let why = match opts.safe {
            SafeArea::Margin(_) => "the safe margin",
            SafeArea::Social => "the social UI area (captions and buttons cover it)",
        };
        let unsafe_: Vec<(f64, bool)> = samples
            .iter()
            .map(|(tt, p)| {
                let b = p.bounds;
                (
                    *tt,
                    p.contrast >= READABLE && p.clipped == 0 && (b[0] < l || b[1] < t || b[2] > rr || b[3] > bb),
                )
            })
            .collect();
        for (a, b) in spans(&unsafe_, step, 0.5) {
            let p = &samples.iter().find(|(tt, _)| *tt >= a).unwrap().1;
            push(
                if opts.safe == SafeArea::Social {
                    "warning"
                } else {
                    "info"
                },
                "unsafe_area",
                node,
                a,
                b,
                format!(
                    "{label} sits in {why} (bounds {:.0},{:.0} to {:.0},{:.0})",
                    p.bounds[0], p.bounds[1], p.bounds[2], p.bounds[3]
                ),
            );
        }
        // Size.
        let small = samples
            .iter()
            .filter(|(_, p)| p.contrast >= READABLE)
            .map(|(_, p)| p.size_px)
            .fold(f32::MAX, f32::min);
        if small < 0.022 * short && small > 0.0 {
            push(
                "info",
                "small_text",
                node,
                first,
                last,
                format!(
                    "{label} is set at {small:.0} px; below about {:.0} px it is hard to read on a phone",
                    0.022 * short
                ),
            );
        }
    }
    for ((a, b), times) in &overlaps {
        let samples: Vec<(f64, bool)> = times.iter().map(|t| (*t, true)).collect();
        // Consecutive probe times form spans.
        // Collisions last; text crossing in a transition (a second or so) does not.
        for (from, to) in spans(&samples, step, 1.5) {
            push(
                "warning",
                "overlap",
                a,
                from,
                to,
                format!("text `{a}` and `{b}` overlap on screen"),
            );
        }
    }
    // Flashes (WCAG 2.3.1): more than three pairs of opposing changes in
    // mean luminance of 10% or more (the darker state below 0.8) within any
    // second.
    let mut transitions = Vec::new();
    let (mut lo, mut hi, mut dir) = (lumas[0], lumas[0], 0i32);
    for (i, &l) in lumas.iter().enumerate().skip(1) {
        match dir {
            1 if l > hi => hi = l,
            -1 if l < lo => lo = l,
            1 if hi - l >= 0.1 && l < 0.8 => {
                dir = -1;
                lo = l;
                transitions.push(i);
            }
            -1 if l - lo >= 0.1 && lo < 0.8 => {
                dir = 1;
                hi = l;
                transitions.push(i);
            }
            0 => {
                if l - lo >= 0.1 && lo < 0.8 {
                    dir = 1;
                    hi = l;
                    transitions.push(i);
                } else if hi - l >= 0.1 && l < 0.8 {
                    dir = -1;
                    lo = l;
                    transitions.push(i);
                } else {
                    lo = lo.min(l);
                    hi = hi.max(l);
                }
            }
            _ => {}
        }
    }
    let win = fmt.fps.round() as usize;
    let mut flashes = Vec::new();
    let mut j = 0;
    for i in 0..transitions.len() {
        while transitions[i] - transitions[j] >= win {
            j += 1;
        }
        if (i - j).div_ceil(2) > 3 {
            flashes.push(transitions[i]);
        }
    }
    if let (Some(&a), Some(&b)) = (flashes.first(), flashes.last()) {
        push(
            "error",
            "flashing",
            "",
            fmt.time_of(a as u64) - 1.0,
            fmt.time_of(b as u64),
            "more than 3 flashes per second: a photosensitive seizure risk (WCAG 2.3.1); slow or soften them".into(),
        );
    }
    issues.sort_by(|a, b| a.from.total_cmp(&b.from));
    // Many labels at one small size are one finding.
    let mut merged: Vec<Issue> = Vec::new();
    for i in issues {
        if let Some(last) = merged.last_mut() {
            let size = |m: &str| m.split(" set at ").nth(1).map(|s| s.to_string());
            if i.kind == "small_text"
                && last.kind == "small_text"
                && last.from == i.from
                && last.to == i.to
                && size(&last.message) == size(&i.message)
            {
                last.node = format!("{}, {}", last.node, i.node);
                let n = last.node.split(", ").count();
                last.message = format!("{n} texts are set at {}", size(&i.message).unwrap_or_default());
                continue;
            }
        }
        merged.push(i);
    }
    let issues = merged;
    Ok(Review {
        issues,
        frames: total,
        texts: per.len(),
        seconds: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_spans() {
        let s = [(0.0, false), (0.1, true), (0.2, true), (0.3, false), (0.4, true)];
        assert_eq!(longest_run(&s, 0.1), Some((0.1, 0.30000000000000004)));
        assert_eq!(spans(&s, 0.1, 0.15).len(), 1);
        assert!((reading_time("one two three") - 0.91).abs() < 1e-9);
    }
}
