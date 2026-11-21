//! Font matching: which installed font (family, weight, style) renders a
//! line of text the way the reference shows it, and at what size, position,
//! tracking and color.
//!
//! 1. The reference line becomes a coverage map: each pixel's position
//!    between the line's background and text colors, in linear light.
//! 2. Every installed face that has glyphs for the text is screened by the
//!    aspect ratio of the shaped line's ink box (font tables only, no
//!    rasterizing), and the closest ones are rendered by ultrafast itself, sized
//!    and placed so their ink box lands on the reference's.
//! 3. The best few are refined (size, position, tracking) by search, and
//!    the text color is solved by least squares against the coverage.

use crate::metrics::Frame;
use crate::{Reference, nelder_mead};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;
use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Edit, Param};
use ultrafast_render::ops::srgb_to_linear;
use ultrafast_render::{Mode, Renderer};

#[derive(Debug, Clone)]
pub struct MatchOptions {
    /// Most faces rendered after the shape screen.
    pub screen: usize,
    /// Faces whose line would need more tracking than this (in ems) to
    /// match the reference's width are screened out.
    pub max_tracking: f64,
    /// Best rendered faces refined by search.
    pub refine: usize,
    /// Extra font folders to consider besides the installed fonts.
    pub font_dirs: Vec<PathBuf>,
    /// The background behind the text when it is known (RGBA8, the
    /// reference's size): coverage is measured against it instead of
    /// rows sampled above and below the line.
    pub background: Option<std::sync::Arc<Vec<u8>>>,
    /// The line's ink box [x0, y0, x1, y1] (frame px, exclusive end) when
    /// it is known, instead of searching around the node for it.
    pub ink: Option<[f64; 4]>,
    /// Only this face (font, weight, italic): to place a part of a line
    /// whose font is already known.
    pub only: Option<(String, u16, bool)>,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            screen: 400,
            max_tracking: 0.35,
            refine: 3,
            font_dirs: Vec::new(),
            background: None,
            ink: None,
            only: None,
        }
    }
}

#[derive(Debug, Clone)]
struct Face {
    /// Family name, or a font file path for faces from `font_dirs`.
    font: String,
    family: String,
    weight: u16,
    italic: bool,
    upem: f64,
    /// Ink box of the shaped line in font units, y up: x0, y0, x1, y1.
    ink: [f64; 4],
    /// Shaped glyph count (tracking applies between each).
    glyphs: usize,
    /// A variable font: any weight its axis spans.
    variable: bool,
    /// A variable face's data and index, to measure it at other weights.
    source: Option<(std::sync::Arc<Vec<u8>>, u32)>,
    /// The optical size axis (min, default, max), if it has one.
    opsz_axis: Option<[f64; 3]>,
    /// Optical size pinned to a value; none follows the size (ultrafast's and
    /// browsers' default).
    opsz: Option<f64>,
    /// Its other variation axes (tag, [min, default, max]), and those set
    /// off their defaults.
    axes: Vec<([u8; 4], [f64; 3])>,
    pins: Vec<([u8; 4], f64)>,
}

#[derive(Debug, Clone)]
struct Scored {
    face: Face,
    /// size, baseline x, baseline y (crop pixels), tracking.
    x: [f64; 4],
    mse: f64,
}

fn lin3(c: [u8; 3]) -> [f64; 3] {
    c.map(|v| srgb_to_linear(v as f64 / 255.0))
}

fn linear_to_srgb(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// The background behind a text line, per pixel: each column interpolated
/// between a band above the line and a band below it (median of 5 rows by
/// 3 columns each). Covers uniform fills, gradients and vertical structure
/// such as color bars or footage edges.
struct BgField {
    above: Vec<[f64; 3]>,
    below: Vec<[f64; 3]>,
    ya: f64,
    yb: f64,
    /// A known background (linear), width, instead of the two bands.
    known: Option<(std::sync::Arc<Vec<u8>>, i64)>,
}

impl BgField {
    fn new(frame: &Frame, ya: i64, yb: i64) -> Self {
        let (fw, fh) = (frame.width as i64, frame.height as i64);
        let band = |yc: i64| -> Vec<[f64; 3]> {
            (0..fw)
                .map(|x| {
                    let mut v: [Vec<f64>; 3] = Default::default();
                    for y in (yc - 2..=yc + 2).map(|y| y.clamp(0, fh - 1)) {
                        for xx in (x - 1..=x + 1).map(|x| x.clamp(0, fw - 1)) {
                            let k = ((y * fw + xx) * 4) as usize;
                            for c in 0..3 {
                                v[c].push(srgb_to_linear(frame.rgba[k + c] as f64 / 255.0));
                            }
                        }
                    }
                    [0, 1, 2].map(|c| {
                        v[c].sort_by(|a, b| a.partial_cmp(b).unwrap());
                        v[c][v[c].len() / 2]
                    })
                })
                .collect()
        };
        let (ya, yb) = (ya.clamp(0, fh - 1), yb.clamp(0, fh - 1));
        Self {
            above: band(ya),
            below: band(yb),
            ya: ya as f64,
            yb: yb.max(ya + 1) as f64,
            known: None,
        }
    }

    fn known(plate: std::sync::Arc<Vec<u8>>, width: i64) -> Self {
        Self {
            above: Vec::new(),
            below: Vec::new(),
            ya: 0.0,
            yb: 1.0,
            known: Some((plate, width)),
        }
    }

    fn at(&self, x: i64, y: i64) -> [f64; 3] {
        if let Some((p, w)) = &self.known {
            let i = ((y * w + x) * 4) as usize;
            return lin3([p[i], p[i + 1], p[i + 2]]);
        }
        let t = ((y as f64 - self.ya) / (self.yb - self.ya)).clamp(0.0, 1.0);
        let (a, b) = (self.above[x as usize], self.below[x as usize]);
        [0, 1, 2].map(|c| a[c] + (b[c] - a[c]) * t)
    }
}

/// Coverage of `fg` over the background field for the pixels of `rect`
/// (x, y, w, h): each pixel's place between its background and `fg`.
fn coverage(frame: &Frame, rect: [i64; 4], fg: [f64; 3], bg: &BgField) -> Vec<f32> {
    let mut out = Vec::with_capacity((rect[2] * rect[3]) as usize);
    for y in rect[1]..rect[1] + rect[3] {
        for x in rect[0]..rect[0] + rect[2] {
            let b = bg.at(x, y);
            let d = [fg[0] - b[0], fg[1] - b[1], fg[2] - b[2]];
            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            let i = ((y * frame.width as i64 + x) * 4) as usize;
            let c = lin3([frame.rgba[i], frame.rgba[i + 1], frame.rgba[i + 2]]);
            // Where text and background colors coincide, nothing is known.
            let a = if dd < 1e-4 {
                0.0
            } else {
                ((c[0] - b[0]) * d[0] + (c[1] - b[1]) * d[1] + (c[2] - b[2]) * d[2]) / dd
            };
            out.push(a.clamp(0.0, 1.0) as f32);
        }
    }
    out
}

/// The text color: the median of the pixels in `rect` that differ most
/// from their background (glyph interiors).
fn text_color(frame: &Frame, rect: [i64; 4], bg: &BgField) -> [f64; 3] {
    let mut px: Vec<(f64, [f64; 3])> = Vec::new();
    for y in rect[1]..rect[1] + rect[3] {
        for x in rect[0]..rect[0] + rect[2] {
            let i = ((y * frame.width as i64 + x) * 4) as usize;
            let c = lin3([frame.rgba[i], frame.rgba[i + 1], frame.rgba[i + 2]]);
            let b = bg.at(x, y);
            let d = (0..3).map(|k| (c[k] - b[k]).powi(2)).sum::<f64>();
            px.push((d, c));
        }
    }
    px.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let top = &px[..(px.len() / 6).max(1)];
    [0, 1, 2].map(|k| {
        let mut v: Vec<f64> = top.iter().map(|p| p.1[k]).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    })
}

/// The contiguous run of `true` around `seed` in `on`, bridging gaps up to
/// `gap` long.
fn run_around(on: &[bool], seed: std::ops::Range<usize>, gap: usize) -> Option<(usize, usize)> {
    let start = (seed.start..seed.end.min(on.len())).find(|&i| on[i])?;
    let (mut lo, mut hi) = (start, start);
    let mut i = start;
    let mut miss = 0;
    while i + 1 < on.len() {
        i += 1;
        if on[i] {
            hi = i;
            miss = 0;
        } else {
            miss += 1;
            if miss > gap && i >= seed.end {
                break;
            }
        }
    }
    let mut i = start;
    miss = 0;
    while i > 0 {
        i -= 1;
        if on[i] {
            lo = i;
            miss = 0;
        } else {
            miss += 1;
            if miss > gap {
                break;
            }
        }
    }
    Some((lo, hi + 1))
}

/// Installed (and `dirs`) faces that can set `text`, with their ink boxes.
fn faces(text: &str, dirs: &[PathBuf]) -> Vec<Face> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let system: HashSet<fontdb::ID> = db.faces().map(|f| f.id).collect();
    for d in dirs {
        db.load_fonts_dir(d);
    }
    // ultrafast's bundled faces, by family name like installed ones.
    let before: HashSet<fontdb::ID> = db.faces().map(|f| f.id).collect();
    for data in ultrafast_render::geom::bundled_fonts() {
        db.load_font_data(data.to_vec());
    }
    let bundled: HashSet<fontdb::ID> = db.faces().map(|f| f.id).filter(|id| !before.contains(id)).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for info in db.faces() {
        let Some((family, _)) = info.families.first() else {
            continue;
        };
        let italic = info.style != fontdb::Style::Normal;
        let font = if system.contains(&info.id) || bundled.contains(&info.id) {
            family.clone()
        } else {
            match &info.source {
                // Collections other than their first face cannot be named by path.
                fontdb::Source::File(p) if info.index == 0 => p.to_string_lossy().into_owned(),
                _ => continue,
            }
        };
        if !seen.insert((font.clone(), info.weight.0, italic)) {
            continue;
        }
        // A variable font is every weight its axis spans.
        let weights: Vec<u16> = db
            .with_face_data(info.id, |data, index| {
                let f = rustybuzz::ttf_parser::Face::parse(data, index).ok()?;
                let axis = f
                    .variation_axes()
                    .into_iter()
                    .find(|a| a.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"wght"))?;
                let (lo, hi) = (axis.min_value.ceil() as u16, axis.max_value.floor() as u16);
                Some((1..=9).map(|k| k * 100).filter(|w| *w >= lo && *w <= hi).collect())
            })
            .flatten()
            .unwrap_or_else(|| vec![info.weight.0]);
        // Variable faces are screened at their default weight, regular and
        // bold (a thin default would rank the family out); the best are
        // tried at the other weights after scoring.
        let variable = weights.len() > 1 || weights.first() != Some(&info.weight.0);
        let all_axes: Vec<([u8; 4], [f64; 3])> = db
            .with_face_data(info.id, |data, index| {
                let f = rustybuzz::ttf_parser::Face::parse(data, index).ok()?;
                Some(
                    f.variation_axes()
                        .into_iter()
                        .map(|a| {
                            (
                                a.tag.to_bytes(),
                                [a.min_value as f64, a.def_value as f64, a.max_value as f64],
                            )
                        })
                        .collect(),
                )
            })
            .flatten()
            .unwrap_or_default();
        let opsz_axis = all_axes.iter().find(|a| &a.0 == b"opsz").map(|a| a.1);
        let axes: Vec<([u8; 4], [f64; 3])> = all_axes
            .into_iter()
            .filter(|a| &a.0 != b"wght" && &a.0 != b"opsz" && a.1[0] < a.1[2])
            .collect();
        let source = if variable {
            db.with_face_data(info.id, |data, index| (std::sync::Arc::new(data.to_vec()), index))
        } else {
            None
        };
        let mut screen_at = vec![info.weight.0];
        if variable {
            screen_at.extend(
                [400, 700]
                    .into_iter()
                    .filter(|w| *w != info.weight.0 && weights.contains(w)),
            );
        }
        for weight in screen_at {
            if weight != info.weight.0 && !seen.insert((font.clone(), weight, italic)) {
                continue;
            }
            // Shape and measure at this weight: a variable font's glyphs
            // widen as it gets bolder.
            let shaped = db.with_face_data(info.id, |data, index| {
                let mut f = rustybuzz::Face::from_slice(data, index)?;
                if text.chars().any(|c| !c.is_whitespace() && f.glyph_index(c).is_none()) {
                    return None;
                }
                if variable {
                    f.set_variation(rustybuzz::ttf_parser::Tag::from_bytes(b"wght"), weight as f32);
                }
                let mut buf = rustybuzz::UnicodeBuffer::new();
                buf.push_str(text);
                buf.guess_segment_properties();
                let out = rustybuzz::shape(&f, &[], buf);
                struct Nop;
                impl rustybuzz::ttf_parser::OutlineBuilder for Nop {
                    fn move_to(&mut self, _: f32, _: f32) {}
                    fn line_to(&mut self, _: f32, _: f32) {}
                    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
                    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
                    fn close(&mut self) {}
                }
                let mut pen = 0.0f64;
                let mut ink = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for (gi, gp) in out.glyph_infos().iter().zip(out.glyph_positions()) {
                    let id = rustybuzz::ttf_parser::GlyphId(gi.glyph_id as u16);
                    // Static faces: the glyph's own box; variable ones: the
                    // outline at this weight.
                    let b = if variable {
                        f.outline_glyph(id, &mut Nop)
                    } else {
                        f.glyph_bounding_box(id)
                    };
                    if let Some(b) = b {
                        let ox = pen + gp.x_offset as f64;
                        let oy = gp.y_offset as f64;
                        ink = [
                            ink[0].min(ox + b.x_min as f64),
                            ink[1].min(oy + b.y_min as f64),
                            ink[2].max(ox + b.x_max as f64),
                            ink[3].max(oy + b.y_max as f64),
                        ];
                    }
                    pen += gp.x_advance as f64;
                }
                (ink[2] > ink[0] && ink[3] > ink[1]).then_some((f.units_per_em() as f64, ink, out.len()))
            });
            if let Some(Some((upem, ink, glyphs))) = shaped {
                out.push(Face {
                    font: font.clone(),
                    family: family.clone(),
                    weight,
                    italic,
                    upem,
                    ink,
                    glyphs,
                    variable,
                    source: source.clone(),
                    opsz_axis,
                    opsz: None,
                    axes: axes.clone(),
                    pins: Vec::new(),
                });
            }
        }
    }
    out
}

/// Upem, ink box and glyph count of `text` set in a face at `weight`
/// (its `wght` axis set when it is variable) and optical size `opsz`.
fn measure_at(
    data: &[u8],
    index: u32,
    text: &str,
    weight: Option<u16>,
    opsz: Option<f64>,
) -> Option<(f64, [f64; 4], usize)> {
    let mut f = rustybuzz::Face::from_slice(data, index)?;
    if let Some(w) = weight {
        f.set_variation(rustybuzz::ttf_parser::Tag::from_bytes(b"wght"), w as f32);
    }
    if let Some(v) = opsz {
        f.set_variation(rustybuzz::ttf_parser::Tag::from_bytes(b"opsz"), v as f32);
    }
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    let out = rustybuzz::shape(&f, &[], buf);
    struct Nop;
    impl rustybuzz::ttf_parser::OutlineBuilder for Nop {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }
    let mut pen = 0.0f64;
    let mut ink = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (gi, gp) in out.glyph_infos().iter().zip(out.glyph_positions()) {
        let id = rustybuzz::ttf_parser::GlyphId(gi.glyph_id as u16);
        let b = if weight.is_some() || opsz.is_some() {
            f.outline_glyph(id, &mut Nop)
        } else {
            f.glyph_bounding_box(id)
        };
        if let Some(b) = b {
            let ox = pen + gp.x_offset as f64;
            let oy = gp.y_offset as f64;
            ink = [
                ink[0].min(ox + b.x_min as f64),
                ink[1].min(oy + b.y_min as f64),
                ink[2].max(ox + b.x_max as f64),
                ink[3].max(oy + b.y_max as f64),
            ];
        }
        pen += gp.x_advance as f64;
    }
    (ink[2] > ink[0] && ink[3] > ink[1]).then_some((f.units_per_em() as f64, ink, out.len()))
}

fn style_name(italic: bool) -> &'static str {
    if italic { "italic" } else { "normal" }
}

/// Finds the font of text node `node` in the reference frame at `t` and
/// writes it (with size, baseline position, tracking and color) into a copy
/// of `doc`. The node should be an untransformed single-line `text` placed
/// roughly over the reference text (as `ultrafast ocr --draft` makes them).
pub fn match_font(
    device: &Device,
    doc: &Document,
    base_dir: &Path,
    reference: &Path,
    node: &str,
    t: f64,
    opts: &MatchOptions,
) -> Result<(Document, Value), String> {
    let started = Instant::now();
    let n = doc.nodes.get(node).ok_or(format!("no node `{node}`"))?;
    if n.op != "text" {
        return Err(format!("`{node}` is a `{}`, not a text node", n.op));
    }
    for p in ["rotation", "scale", "anchor", "skew", "width"] {
        if n.params.contains_key(p) {
            return Err(format!(
                "`{node}` sets `{p}`; font matching needs an untransformed single line"
            ));
        }
    }
    let text = ultrafast_render::ops::text(n, "text").unwrap_or("").trim().to_string();
    if text.is_empty() || text.contains('\n') {
        return Err(format!("`{node}` needs a single non-empty line of text"));
    }

    // Where the draft puts the line.
    let mut renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    renderer.set_base_dir(base_dir);
    renderer.set_mode(Mode::Exact);
    renderer.render_rgba8(doc, t).map_err(|e| e.to_string())?;
    let b = renderer
        .node_bounds(node)
        .ok_or(format!("`{node}` drew nothing at t={t}"))?
        .map(|v| v as f64);
    let mut r = Reference::open(device, reference, Some(doc.format.width), Some(doc.format.height))?;
    let frame = r.frame(t)?;
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    let bh = (b[3] - b[1]).max(4.0);
    let box_rect = {
        let x0 = (b[0].floor() as i64).clamp(0, fw - 1);
        let y0 = (b[1].floor() as i64).clamp(0, fh - 1);
        [
            x0,
            y0,
            ((b[2].ceil() as i64).min(fw) - x0).max(1),
            ((b[3].ceil() as i64).min(fh) - y0).max(1),
        ]
    };

    // The reference line's ink box, searched in a generous window.
    let clamp_rect = |x0: f64, y0: f64, x1: f64, y1: f64| -> [i64; 4] {
        let x0 = (x0.floor() as i64).clamp(0, fw - 1);
        let y0 = (y0.floor() as i64).clamp(0, fh - 1);
        let x1 = (x1.ceil() as i64).clamp(x0 + 1, fw);
        let y1 = (y1.ceil() as i64).clamp(y0 + 1, fh);
        [x0, y0, x1 - x0, y1 - y0]
    };
    // Background bands half a line above and below the draft's box; if the
    // ink reaches them (the draft was too small), move them out once.
    let (mut ya, mut yb) = ((b[1] - 0.5 * bh).floor() as i64, (b[3] + 0.5 * bh).ceil() as i64);
    let mut attempt = 0;
    let (bg, fg, ink) = loop {
        let bg = match &opts.background {
            Some(p) => BgField::known(p.clone(), frame.width as i64),
            None => BgField::new(&frame, ya, yb),
        };
        let fg = text_color(&frame, box_rect, &bg);
        if let Some(known) = opts.ink {
            break (bg, fg, known);
        }
        // Full width: the draft's font may be much narrower or wider than
        // the reference's, and the run search below stops at the line's ends.
        let win = clamp_rect(0.0, (ya + 3) as f64, fw as f64, (yb - 2) as f64);
        let cov = coverage(&frame, win, fg, &bg);
        let (ww, wh) = (win[2] as usize, win[3] as usize);
        // Rows are judged over the draft's span (full-width rows pick up
        // every edge in a busy background); isolated pixels are noise.
        let (sx0, sx1) = (
            ((b[0] - bh - win[0] as f64).max(0.0) as usize).min(ww - 1),
            ((b[2] + bh - win[0] as f64).max(1.0) as usize).min(ww),
        );
        let rows: Vec<bool> = (0..wh)
            .map(|y| (sx0..sx1).filter(|&x| cov[y * ww + x] > 0.5).count() >= 2)
            .collect();
        let seed_y = ((b[1] - win[1] as f64).max(0.0) as usize)..((b[3] - win[1] as f64).max(1.0) as usize);
        let (ry0, ry1) = run_around(&rows, seed_y, (bh * 0.25) as usize).ok_or("no text ink near the node")?;
        let cols: Vec<bool> = (0..ww)
            .map(|x| (ry0..ry1).filter(|&y| cov[y * ww + x] > 0.5).count() >= 2)
            .collect();
        let seed_x = ((b[0] - win[0] as f64).max(0.0) as usize)..((b[2] - win[0] as f64).max(1.0) as usize);
        // Word gaps stay well under the line's own ink height; gaps between
        // separate elements on the same row are usually wider.
        let (rx0, rx1) =
            run_around(&cols, seed_x, ((ry1 - ry0) as f64 * 0.8) as usize).ok_or("no text ink near the node")?;
        let ink = [
            (win[0] + rx0 as i64) as f64,
            (win[1] + ry0 as i64) as f64,
            (win[0] + rx1 as i64) as f64,
            (win[1] + ry1 as i64) as f64,
        ];
        let touching = ry0 <= 1 || ry1 + 1 >= wh;
        if !touching || attempt == 1 {
            break (bg, fg, ink);
        }
        attempt += 1;
        let ih = ink[3] - ink[1];
        (ya, yb) = (
            (ink[1] - 0.4 * ih).floor() as i64 - 3,
            (ink[3] + 0.4 * ih).ceil() as i64 + 2,
        );
    };
    let (iw, ih) = (ink[2] - ink[0], ink[3] - ink[1]);
    let pad = (ih * 0.3).max(4.0);
    let crop = clamp_rect(ink[0] - pad, ink[1] - pad, ink[2] + pad, ink[3] + pad);
    let target = coverage(&frame, crop, fg, &bg);
    let (cw, ch) = (crop[2] as u32, crop[3] as u32);

    // Each face sized so its ink height matches, with the tracking that
    // makes its ink width match; faces needing implausible tracking are out.
    let t_faces = Instant::now();
    let mut all = faces(&text, &opts.font_dirs);
    if std::env::var_os("ULTRAFAST_DEBUG_MATCH").is_some() {
        eprintln!(
            "match {text:?}: {} faces in {:?} (since start {:?})",
            all.len(),
            t_faces.elapsed(),
            started.elapsed()
        );
    }
    if let Some((font, weight, italic)) = &opts.only {
        all = all
            .into_iter()
            .filter(|f| &f.font == font && f.italic == *italic && (f.variable || f.weight == *weight))
            .filter_map(|f| match &f.source {
                Some((data, index)) if f.variable => {
                    let (upem, ink, glyphs) = measure_at(data, *index, &text, Some(*weight), None)?;
                    Some(Face {
                        weight: *weight,
                        upem,
                        ink,
                        glyphs,
                        ..f
                    })
                }
                _ => Some(f),
            })
            .take(1)
            .collect();
    }
    if all.is_empty() {
        return Err(format!("no installed font has glyphs for `{text}`"));
    }
    let start_of = |f: &Face| {
        let size = ih * f.upem / (f.ink[3] - f.ink[1]);
        let s = size / f.upem;
        let natural = (f.ink[2] - f.ink[0]) * s;
        let tracking = if f.glyphs > 1 {
            (iw - natural) / (f.glyphs - 1) as f64
        } else {
            0.0
        };
        // Baseline origin that puts the ink box's top-left on the reference's.
        [
            size,
            ink[0] - crop[0] as f64 - f.ink[0] * s,
            ink[1] - crop[1] as f64 + f.ink[3] * s,
            tracking,
        ]
    };
    // Faces whose optical size follows the size: measured at the size the
    // line's height asks for (proportions change with optical size).
    for f in all.iter_mut().filter(|f| f.opsz_axis.is_some() && f.opsz.is_none()) {
        let Some((data, index)) = f.source.clone() else {
            continue;
        };
        let [lo, _, hi] = f.opsz_axis.unwrap();
        let size = start_of(f)[0].clamp(lo, hi);
        if let Some((upem, ink, glyphs)) = measure_at(&data, index, &text, Some(f.weight), Some(size)) {
            (f.upem, f.ink, f.glyphs) = (upem, ink, glyphs);
        }
    }
    let mut screened: Vec<(f64, Face)> = all
        .iter()
        .map(|f| {
            let x = start_of(f);
            ((x[3] / x[0]).abs(), f.clone())
        })
        .filter(|(em, _)| *em <= opts.max_tracking)
        .collect();
    screened.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    screened.truncate(opts.screen.max(1));
    if screened.is_empty() {
        return Err(format!("no font sets `{text}` at the reference's proportions"));
    }

    // Render the survivors with their ink box on the reference's.
    let mut work = Document::from_json(
        &json!({
            "version": 1,
            "format": { "width": cw, "height": ch, "fps": 30, "duration": 1 },
            "output": "t",
            "nodes": { "t": { "op": "text", "params": {
                "text": text, "align": "left", "valign": "baseline", "color": "#ffffff",
                "size": 10, "position": [0, 0], "tracking": 0
            } } }
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    let mut crop_renderer = Renderer::new(device.clone()).map_err(|e| e.to_string())?;
    crop_renderer.set_base_dir(base_dir);
    crop_renderer.set_mode(Mode::Exact);
    let mut renders = 0usize;
    // Axis params on the work node, to take off for faces without them.
    let mut axis_params: HashSet<String> = HashSet::new();
    let mut eval = |face: &Face, x: &[f64; 4], work: &mut Document, keep: Option<&mut Vec<f32>>| {
        renders += 1;
        let set = |param: &str, value: Param| Edit::SetParam {
            node: "t".into(),
            param: param.into(),
            value,
        };
        let mut axis_edits = Vec::new();
        let mut now = HashSet::new();
        for (tag, r) in &face.axes {
            let name = format!("axis_{}", String::from_utf8_lossy(tag).trim_end());
            let v = face.pins.iter().find(|p| &p.0 == tag).map_or(r[1], |p| p.1);
            axis_edits.push(set(&name, Param::Number(v)));
            now.insert(name);
        }
        for stale in axis_params.difference(&now) {
            axis_edits.push(Edit::RemoveParam {
                node: "t".into(),
                param: stale.clone(),
            });
        }
        axis_params = now;
        work.apply(&axis_edits).map_err(|e| e.to_string())?;
        work.apply(&[
            set("font", Param::Text(face.font.clone())),
            set("weight", Param::Number(face.weight as f64)),
            set("style", Param::Text(style_name(face.italic).into())),
            set("size", Param::Number(x[0].max(1.0))),
            set("position", Param::Vector(vec![x[1], x[2]])),
            set("tracking", Param::Number(x[3])),
            // Pinned, or following the size as it does by default.
            set("axis_opsz", Param::Number(face.opsz.unwrap_or(x[0].max(1.0)))),
        ])
        .map_err(|e| e.to_string())?;
        let (px, _) = crop_renderer.render_rgba8(work, 0.0).map_err(|e| e.to_string())?;
        let mut err = 0.0f64;
        for (i, &want) in target.iter().enumerate() {
            let got = px[i * 4 + 3] as f32 / 255.0;
            err += ((got - want) as f64).powi(2);
        }
        if let Some(k) = keep {
            *k = px.chunks_exact(4).map(|p| p[3] as f32 / 255.0).collect();
        }
        Ok::<f64, String>(err / target.len().max(1) as f64)
    };
    let mut scored: Vec<Scored> = Vec::new();
    for (_, face) in &screened {
        let x = start_of(face);
        let mse = eval(face, &x, &mut work, None)?;
        scored.push(Scored {
            face: face.clone(),
            x,
            mse,
        });
    }
    scored.sort_by(|a, b| a.mse.partial_cmp(&b.mse).unwrap());
    // Variable faces near the top at every hundred of their weight axis,
    // each measured at that weight (glyphs widen as they get bolder).
    let mut more = Vec::new();
    for s in scored
        .iter()
        .take(12)
        .filter(|s| s.face.variable && opts.only.is_none())
    {
        let Some((data, index)) = &s.face.source else { continue };
        // At the optical size it is set at (pinned, or following the size).
        let opsz = s
            .face
            .opsz_axis
            .map(|[lo, _, hi]| s.face.opsz.unwrap_or(s.x[0].clamp(lo, hi)));
        for w in (1..=9).map(|k| k * 100u16).filter(|w| *w != s.face.weight) {
            let Some((upem, ink, glyphs)) = measure_at(data, *index, &text, Some(w), opsz) else {
                continue;
            };
            let face = Face {
                weight: w,
                upem,
                ink,
                glyphs,
                ..s.face.clone()
            };
            let x = start_of(&face);
            if (x[3] / x[0]).abs() > opts.max_tracking {
                continue;
            }
            let mse = eval(&face, &x, &mut work, None)?;
            more.push(Scored { face, x, mse });
        }
    }
    scored.extend(more);
    scored.sort_by(|a, b| a.mse.partial_cmp(&b.mse).unwrap());
    // Faces with an optical size axis near the top, measured at the size
    // they would be set at (glyph proportions change with it) and also
    // pinned to its default and its ends (type set at a fixed instance).
    let mut more = Vec::new();
    for s in scored
        .iter()
        .take(12)
        .filter(|s| s.face.opsz_axis.is_some() && s.face.opsz.is_none())
    {
        let Some((data, index)) = &s.face.source else { continue };
        let [lo, def, hi] = s.face.opsz_axis.unwrap();
        let mut pins = vec![None, Some(def), Some(lo), Some(hi)];
        pins.dedup();
        // A pinned optical size changes how heavy the strokes look: the
        // neighboring weights too.
        let w0 = s.face.weight as i32;
        for v in pins {
            for dw in [0, -100, 100] {
                let weight = (w0 + dw).clamp(100, 900) as u16;
                if dw != 0 && (v.is_none() || !s.face.variable) {
                    continue;
                }
                let at = v.unwrap_or(s.x[0].clamp(lo, hi));
                let Some((upem, ink, glyphs)) = measure_at(data, *index, &text, Some(weight), Some(at)) else {
                    continue;
                };
                let face = Face {
                    opsz: v,
                    weight,
                    upem,
                    ink,
                    glyphs,
                    ..s.face.clone()
                };
                let x = start_of(&face);
                let mse = eval(&face, &x, &mut work, None)?;
                more.push(Scored { face, x, mse });
            }
        }
    }
    scored.extend(more);
    scored.sort_by(|a, b| a.mse.partial_cmp(&b.mse).unwrap());
    // One of each variant (a family installed twice scores twice).
    let mut seen_variant = HashSet::new();
    scored.retain(|s| {
        seen_variant.insert((
            s.face.font.clone(),
            s.face.weight,
            s.face.italic,
            s.face.opsz.map(|v| (v * 10.0) as i64),
            s.face
                .pins
                .iter()
                .map(|(t, v)| (*t, (v * 10.0) as i64))
                .collect::<Vec<_>>(),
        ))
    });
    if std::env::var_os("ULTRAFAST_DEBUG_MATCH").is_some() {
        for s in scored.iter().take(10) {
            eprintln!(
                "  {} {} {} opsz {:?} pins {:?}: {:.5} at {:?}",
                s.face.font, s.face.weight, s.face.italic, s.face.opsz, s.face.pins, s.mse, s.x
            );
        }
    }

    // Where a face starts (sized and spaced from the ink box) misleads
    // between close weights: a quick fit of place, size and tracking for
    // twice as many before choosing the few to refine.
    let quick = (opts.refine.max(1) * 2).min(scored.len());
    for s in scored.iter_mut().take(quick) {
        let steps = [s.x[0] * 0.03, 1.0, 1.0, (s.x[0] * 0.01).max(0.2)];
        let face = s.face.clone();
        let (best, err) = nelder_mead(&s.x, &steps, 30, |x| {
            eval(&face, &[x[0], x[1], x[2], x[3]], &mut work, None)
        })?;
        if err < s.mse {
            s.x = [best[0], best[1], best[2], best[3]];
            s.mse = err;
        }
    }
    scored.sort_by(|a, b| a.mse.partial_cmp(&b.mse).unwrap());

    // Refine the best few: size, position and tracking.
    let refine = opts.refine.max(1).min(scored.len());
    for s in scored.iter_mut().take(refine) {
        let steps = [s.x[0] * 0.03, 1.0, 1.0, (s.x[0] * 0.01).max(0.2)];
        let face = s.face.clone();
        let (best, err) = nelder_mead(&s.x, &steps, 90, |x| {
            eval(&face, &[x[0], x[1], x[2], x[3]], &mut work, None)
        })?;
        if err < s.mse {
            s.x = [best[0], best[1], best[2], best[3]];
            s.mse = err;
        }
        // Other axes (softness, width, wonkiness...): each tried at its
        // ends and middle, kept where it fits better.
        for (tag, r) in face.axes.clone() {
            for v in [r[0], (r[0] + r[2]) / 2.0, r[2]] {
                if (v - r[1]).abs() < 1e-6 {
                    continue;
                }
                let mut f = s.face.clone();
                f.pins.retain(|p| p.0 != tag);
                f.pins.push((tag, v));
                let (x, err) = nelder_mead(&s.x, &steps, 40, |x| {
                    eval(&f, &[x[0], x[1], x[2], x[3]], &mut work, None)
                })?;
                if err < s.mse {
                    s.mse = err;
                    s.x = [x[0], x[1], x[2], x[3]];
                    s.face = f;
                }
            }
        }
        // A variable font's weight between the hundreds, each weight with
        // its own placement (glyphs widen as it gets bolder).
        if face.variable && opts.only.is_none() {
            let w0 = face.weight as i32;
            for dw in [-50, 50, -25, 25] {
                let mut f = s.face.clone();
                f.weight = (w0 + dw).clamp(1, 1000) as u16;
                let (x, err) = nelder_mead(&s.x, &steps, 40, |x| {
                    eval(&f, &[x[0], x[1], x[2], x[3]], &mut work, None)
                })?;
                if err < s.mse {
                    s.mse = err;
                    s.x = [x[0], x[1], x[2], x[3]];
                    s.face.weight = f.weight;
                }
            }
        }
    }
    scored.sort_by(|a, b| a.mse.partial_cmp(&b.mse).unwrap());
    let best = scored[0].clone();

    // Text color by least squares against the reference, given the best
    // face's coverage: ref = a * fg + (1 - a) * bg.
    let mut a = Vec::new();
    eval(&best.face, &best.x, &mut work, Some(&mut a))?;
    let mut num = [0.0f64; 3];
    let mut den = 0.0f64;
    for (i, &ai) in a.iter().enumerate() {
        let (x, y) = (crop[0] + (i as i64 % crop[2]), crop[1] + (i as i64 / crop[2]));
        let k = ((y * fw + x) * 4) as usize;
        let c = lin3([frame.rgba[k], frame.rgba[k + 1], frame.rgba[k + 2]]);
        let ai = ai as f64;
        let bgx = bg.at(x, y);
        for j in 0..3 {
            num[j] += ai * (c[j] - (1.0 - ai) * bgx[j]);
        }
        den += ai * ai;
    }
    let mut color = if den > 1e-6 { num.map(|v| v / den) } else { fg };
    // Edges carry codec ringing and chroma bleed; where the glyphs have
    // solid interiors, their median is the truer color.
    let mut inside: Vec<[u8; 3]> = Vec::new();
    for (i, &ai) in a.iter().enumerate() {
        if ai >= 0.99 {
            let (x, y) = (crop[0] + (i as i64 % crop[2]), crop[1] + (i as i64 / crop[2]));
            let k = ((y * fw + x) * 4) as usize;
            inside.push([frame.rgba[k], frame.rgba[k + 1], frame.rgba[k + 2]]);
        }
    }
    let solid = inside.len() >= 24;
    if solid {
        let med = |c: usize| {
            let mut v: Vec<u8> = inside.iter().map(|p| p[c]).collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        color = lin3([med(0), med(1), med(2)]);
    }
    let hex = format!(
        "#{:02x}{:02x}{:02x}",
        (linear_to_srgb(color[0]) * 255.0).round() as u8,
        (linear_to_srgb(color[1]) * 255.0).round() as u8,
        (linear_to_srgb(color[2]) * 255.0).round() as u8
    );

    let set = |param: &str, value: Param| Edit::SetParam {
        node: node.into(),
        param: param.into(),
        value,
    };
    let round = |v: f64, k: f64| (v * k).round() / k;
    let mut edits = vec![
        set("font", Param::Text(best.face.font.clone())),
        set("weight", Param::Number(best.face.weight as f64)),
        set("style", Param::Text(style_name(best.face.italic).into())),
        set("size", Param::Number(round(best.x[0], 100.0))),
        set(
            "position",
            Param::Vector(vec![
                round(best.x[1] + crop[0] as f64, 100.0),
                round(best.x[2] + crop[1] as f64, 100.0),
            ]),
        ),
        set("align", Param::Text("left".into())),
        set("valign", Param::Text("baseline".into())),
        set("color", Param::Text(hex.clone())),
    ];
    if best.x[3].abs() >= 0.05 {
        edits.push(set("tracking", Param::Number(round(best.x[3], 100.0))));
    }
    if let Some(v) = best.face.opsz {
        edits.push(set("axis_opsz", Param::Number(round(v, 10.0))));
    }
    for (tag, v) in &best.face.pins {
        edits.push(set(
            &format!("axis_{}", String::from_utf8_lossy(tag).trim_end()),
            Param::Number(round(*v, 10.0)),
        ));
    }
    let mut out = doc.clone();
    out.apply(&edits).map_err(|e| e.to_string())?;
    let psnr = |mse: f64| if mse <= 0.0 { 100.0 } else { 10.0 * (1.0 / mse).log10() };
    let describe = |s: &Scored| {
        json!({
            "font": s.face.font, "family": s.face.family, "weight": s.face.weight,
            "style": style_name(s.face.italic), "coverage_psnr": round(psnr(s.mse), 100.0),
        })
    };
    Ok((
        out,
        json!({
            "node": node,
            "text": text,
            "ink": ink,
            "best": {
                "font": best.face.font, "family": best.face.family, "weight": best.face.weight,
                "style": style_name(best.face.italic),
                "size": round(best.x[0], 100.0),
                "position": [round(best.x[1] + crop[0] as f64, 100.0), round(best.x[2] + crop[1] as f64, 100.0)],
                "tracking": round(best.x[3], 100.0),
                "optical_size": best.face.opsz.map_or(json!("auto"), |v| json!(v)),
                "axes": best.face.pins.iter().map(|(t, v)| (String::from_utf8_lossy(t).trim_end().to_string(), json!(round(*v, 10.0)))).collect::<serde_json::Map<_, _>>(),
                "color": hex,
                "color_from": if solid { "glyph interiors" } else { "least squares" },
                "coverage_psnr": round(psnr(best.mse), 100.0),
            },
            "runners_up": scored.iter().skip(1).take(7).map(describe).collect::<Vec<_>>(),
            "faces_considered": all.len(),
            "faces_rendered": screened.len(),
            "renders": renders,
            "ms": started.elapsed().as_millis() as u64,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_bridge_small_gaps() {
        let on = [false, true, true, false, true, false, false, false, true];
        assert_eq!(run_around(&on, 1..2, 1), Some((1, 5)));
        assert_eq!(run_around(&on, 1..2, 3), Some((1, 9)));
        assert_eq!(run_around(&on, 5..6, 0), None);
    }
}
