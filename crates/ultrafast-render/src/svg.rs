//! SVG import: an SVG file becomes ordinary ultrafast nodes, one `path` per
//! filled or stroked shape (gradients as masked `gradient`s), composited in
//! paint order. The result is a component, so every part of a logo or icon
//! is an editable, animatable node, and everything stays vector: placement
//! goes into each path's own params, so it is sharp at any scale.
//!
//! An `svg` node (`src`, `size`) expands into that component when the
//! document is flattened; `ultrafast import-svg` writes it out for editing.

use std::collections::BTreeMap;
use std::path::Path;
use ultrafast_graph::{Component, Document, Node, Param};
use usvg::tiny_skia_path::{PathSegment, Transform};

pub struct Imported {
    pub component: Component,
    /// Size of the placed artwork in px (after fitting).
    pub size: [f64; 2],
    /// SVG features that were skipped (clip paths, masks, filters, images).
    pub warnings: Vec<String>,
}

/// Whether `doc` has `svg` nodes to expand.
pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "svg")
        || doc.components.values().any(|c| c.nodes.values().any(|n| n.op == "svg"))
}

/// Parameters a placed SVG instance exposes.
pub const PLACEMENT: &[&str] = &["position", "anchor", "scale", "rotation", "opacity"];

fn text_param(s: &str) -> Param {
    Param::Text(s.to_string())
}

fn num(v: f64) -> Param {
    Param::Number(v)
}

fn vecp(v: &[f64]) -> Param {
    Param::Vector(v.to_vec())
}

fn hex(c: usvg::Color, alpha: f32) -> String {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", c.red, c.green, c.blue, a)
}

fn map(t: &Transform, x: f32, y: f32) -> (f64, f64) {
    let mut p = [usvg::tiny_skia_path::Point::from_xy(x, y)];
    t.map_points(&mut p);
    (p[0].x as f64, p[0].y as f64)
}

/// Path data in placed px, absolute commands.
fn path_data(path: &usvg::tiny_skia_path::Path, t: &Transform) -> String {
    use std::fmt::Write;
    let mut d = String::new();
    let pt = |d: &mut String, x: f32, y: f32| {
        let (x, y) = map(t, x, y);
        let _ = write!(d, " {:.2} {:.2}", x, y);
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                d.push_str(" M");
                pt(&mut d, p.x, p.y);
            }
            PathSegment::LineTo(p) => {
                d.push_str(" L");
                pt(&mut d, p.x, p.y);
            }
            PathSegment::QuadTo(a, p) => {
                d.push_str(" Q");
                pt(&mut d, a.x, a.y);
                pt(&mut d, p.x, p.y);
            }
            PathSegment::CubicTo(a, b, p) => {
                d.push_str(" C");
                pt(&mut d, a.x, a.y);
                pt(&mut d, b.x, b.y);
                pt(&mut d, p.x, p.y);
            }
            PathSegment::Close => d.push_str(" Z"),
        }
    }
    d.trim().to_string()
}

fn scale_of(t: &Transform) -> f64 {
    ((t.sx * t.sy - t.kx * t.ky) as f64).abs().sqrt()
}

struct Builder {
    nodes: BTreeMap<String, Node>,
    used: std::collections::HashSet<String>,
    warnings: Vec<String>,
    fit: Transform,
    n: usize,
}

impl Builder {
    fn name(&mut self, id: &str, suffix: &str) -> String {
        let base: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let base = if base.is_empty() {
            self.n += 1;
            format!("shape{}", self.n)
        } else {
            base
        };
        let mut name = format!("{base}{suffix}");
        let mut k = 2;
        while !self.used.insert(name.clone()) {
            name = format!("{base}{suffix}_{k}");
            k += 1;
        }
        name
    }

    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    fn placed(params: &mut BTreeMap<String, Param>) {
        for p in ["position", "anchor", "scale", "rotation"] {
            params.insert(p.into(), text_param(&format!("${p}")));
        }
    }

    /// A paint over `shape` (a path node id painting white): the color goes
    /// on the path itself; gradients become a gradient masked by it.
    fn paint(
        &mut self,
        id: &str,
        suffix: &str,
        mut path: Node,
        paint: &usvg::Paint,
        opacity: f32,
        to_px: &Transform,
    ) -> Option<String> {
        match paint {
            usvg::Paint::Color(c) => {
                path.params.insert("color".into(), text_param(&hex(*c, opacity)));
                let name = self.name(id, suffix);
                self.nodes.insert(name.clone(), path);
                Some(name)
            }
            usvg::Paint::LinearGradient(g) => {
                let gt = to_px.pre_concat(g.transform());
                let (sx, sy) = map(&gt, g.x1(), g.y1());
                // The gradient parameter is affine in x: rebuild the same
                // field as a plain linear gradient (exact under any affine).
                let (ux, uy) = (g.x2() - g.x1(), g.y2() - g.y1());
                let len2 = (ux * ux + uy * uy).max(1e-12);
                let inv = gt.invert().unwrap_or_default();
                // Gradient of t in px space: J^T * u / |u|^2 with J = d(gradient space)/d(px).
                let (jx0, jy0) = map(&inv, 0.0, 0.0);
                let (jx1, jy1) = map(&inv, 1.0, 0.0);
                let (jx2, jy2) = map(&inv, 0.0, 1.0);
                let gx = ((jx1 - jx0) * ux as f64 + (jy1 - jy0) * uy as f64) / len2 as f64;
                let gy = ((jx2 - jx0) * ux as f64 + (jy2 - jy0) * uy as f64) / len2 as f64;
                let gl2 = (gx * gx + gy * gy).max(1e-12);
                let end = [sx + gx / gl2, sy + gy / gl2];
                let mut p = BTreeMap::new();
                p.insert("kind".into(), text_param("linear"));
                p.insert("start".into(), vecp(&[sx, sy]));
                p.insert("end".into(), vecp(&end));
                p.insert("stops".into(), text_param(&stops(g.stops(), opacity)));
                p.insert("space".into(), text_param("srgb"));
                Some(self.masked(id, suffix, path, p))
            }
            usvg::Paint::RadialGradient(g) => {
                let gt = to_px.pre_concat(g.transform());
                let (cx, cy) = map(&gt, g.cx(), g.cy());
                let mut p = BTreeMap::new();
                p.insert("kind".into(), text_param("radial"));
                p.insert("center".into(), vecp(&[cx, cy]));
                p.insert("radius".into(), num(g.r().get() as f64 * scale_of(&gt)));
                p.insert("stops".into(), text_param(&stops(g.stops(), opacity)));
                p.insert("space".into(), text_param("srgb"));
                if (g.fx() - g.cx()).abs() > 1e-3 || (g.fy() - g.cy()).abs() > 1e-3 {
                    self.warn("radial gradient focal points are drawn centered".into());
                }
                Some(self.masked(id, suffix, path, p))
            }
            usvg::Paint::Pattern(_) => {
                self.warn("pattern fills are skipped".into());
                None
            }
        }
    }

    fn masked(&mut self, id: &str, suffix: &str, mut path: Node, grad: BTreeMap<String, Param>) -> String {
        path.params.insert("color".into(), text_param("#ffffff"));
        let shape = self.name(id, &format!("{suffix}_shape"));
        self.nodes.insert(shape.clone(), path);
        let g = self.name(id, &format!("{suffix}_gradient"));
        self.nodes.insert(
            g.clone(),
            Node {
                op: "gradient".into(),
                params: grad,
                ..Default::default()
            },
        );
        // Placement moves the gradient with the shape.
        let gp = self.name(id, &format!("{suffix}_gradient_placed"));
        let mut tp = BTreeMap::new();
        Self::placed(&mut tp);
        self.nodes.insert(
            gp.clone(),
            Node {
                op: "transform".into(),
                params: tp,
                inputs: [("source".to_string(), g)].into(),
                ..Default::default()
            },
        );
        let name = self.name(id, suffix);
        self.nodes.insert(
            name.clone(),
            Node {
                op: "mask".into(),
                inputs: [("source".to_string(), gp), ("matte".to_string(), shape)].into(),
                ..Default::default()
            },
        );
        name
    }

    fn path(&mut self, p: &usvg::Path, fallback: &str, layers: &mut Vec<String>) {
        if !p.is_visible() {
            return;
        }
        let id = if p.id().is_empty() { fallback } else { p.id() };
        let to_px = self.fit.pre_concat(p.abs_transform());
        let d = path_data(p.data(), &to_px);
        let base = |extra: &[(&str, Param)]| {
            let mut params = BTreeMap::new();
            params.insert("d".to_string(), text_param(&d));
            Self::placed(&mut params);
            for (k, v) in extra {
                params.insert(k.to_string(), v.clone());
            }
            Node {
                op: "path".into(),
                params,
                ..Default::default()
            }
        };
        let fill = p.fill().and_then(|f| {
            let rule = match f.rule() {
                usvg::FillRule::NonZero => "nonzero",
                usvg::FillRule::EvenOdd => "evenodd",
            };
            let node = base(&[("fill_rule", text_param(rule))]);
            self.paint(id, "", node, f.paint(), f.opacity().get(), &to_px)
        });
        let stroke = p.stroke().and_then(|s| {
            let w = s.width().get() as f64 * scale_of(&to_px);
            let mut extra = vec![
                ("stroke", num(w)),
                (
                    "join",
                    text_param(match s.linejoin() {
                        usvg::LineJoin::Round => "round",
                        usvg::LineJoin::Bevel => "bevel",
                        _ => "miter",
                    }),
                ),
                (
                    "cap",
                    text_param(match s.linecap() {
                        usvg::LineCap::Round => "round",
                        usvg::LineCap::Square => "square",
                        usvg::LineCap::Butt => "butt",
                    }),
                ),
            ];
            if let Some(dash) = s.dasharray() {
                let k = scale_of(&to_px);
                extra.push(("dash", vecp(&dash.iter().map(|v| *v as f64 * k).collect::<Vec<_>>())));
                extra.push(("dash_offset", num(s.dashoffset() as f64 * k)));
            }
            let node = base(&extra);
            self.paint(id, "_stroke", node, s.paint(), s.opacity().get(), &to_px)
        });
        let ordered = match p.paint_order() {
            usvg::PaintOrder::FillAndStroke => [fill, stroke],
            usvg::PaintOrder::StrokeAndFill => [stroke, fill],
        };
        layers.extend(ordered.into_iter().flatten());
    }

    /// The layers of `g`'s children in paint order, each with the blend mode
    /// it composites with. A group without effects of its own (opacity,
    /// clip, mask, filter, blend mode, isolation) is no layer: its children
    /// join the list, so a blend mode among them blends with everything
    /// under the group, as in a browser. Paths carry their full transform.
    fn layers_of(&mut self, g: &usvg::Group, gid: &str, layers: &mut Vec<(String, usvg::BlendMode)>) {
        for child in g.children() {
            // Text as outlines, named after the text element.
            let (c, cid) = match child {
                usvg::Node::Group(c) => (c.as_ref(), if c.id().is_empty() { gid } else { c.id() }),
                usvg::Node::Text(t) => (t.flattened(), if t.id().is_empty() { gid } else { t.id() }),
                usvg::Node::Path(p) => {
                    let mut own = Vec::new();
                    self.path(p, gid, &mut own);
                    layers.extend(own.into_iter().map(|l| (l, usvg::BlendMode::Normal)));
                    continue;
                }
                usvg::Node::Image(_) => {
                    self.warn("embedded images are skipped".into());
                    continue;
                }
            };
            if c.should_isolate() {
                layers.extend(self.group(c, cid).map(|l| (l, c.blend_mode())));
            } else {
                self.layers_of(c, cid, layers);
            }
        }
    }

    /// Layers of a group's children, composited; returns the node id.
    fn group(&mut self, g: &usvg::Group, fallback: &str) -> Option<String> {
        let gid = if g.id().is_empty() { fallback } else { g.id() };
        let mut layers = Vec::new();
        self.layers_of(g, gid, &mut layers);
        let mut acc = layers.first()?.0.clone();
        // Composited in sRGB, as browsers do: a red circle at 50% over white
        // is (255, 128, 128), not linear light's (255, 188, 188).
        for (l, mode) in &layers[1..] {
            let (op, mut params) = match blend_mode(*mode) {
                Some(m) => {
                    let mut p = BTreeMap::new();
                    p.insert("mode".to_string(), text_param(m));
                    ("blend", p)
                }
                None => ("over", BTreeMap::new()),
            };
            params.insert("space".to_string(), text_param("srgb"));
            let name = self.name(gid, if op == "blend" { "_blend" } else { "_over" });
            self.nodes.insert(
                name.clone(),
                Node {
                    op: op.into(),
                    params,
                    inputs: [("fg".to_string(), l.clone()), ("bg".to_string(), acc)].into(),
                    ..Default::default()
                },
            );
            acc = name;
        }
        // Blur filters become ultrafast blurs; other filters are skipped.
        if !g.filters().is_empty() {
            let blur = match (g.filters(), g.filters().first().map(|f| f.primitives())) {
                ([_], Some([p])) => match p.kind() {
                    usvg::filter::Kind::GaussianBlur(b) => Some((b.std_dev_x().get() + b.std_dev_y().get()) / 2.0),
                    _ => None,
                },
                _ => None,
            };
            match blur {
                Some(sd) => {
                    let to_px = self.fit.pre_concat(g.abs_transform());
                    let name = self.name(gid, "_blur");
                    let mut params = BTreeMap::new();
                    params.insert("radius".to_string(), num(sd as f64 * scale_of(&to_px)));
                    self.nodes.insert(
                        name.clone(),
                        Node {
                            op: "blur".into(),
                            params,
                            inputs: [("source".to_string(), acc)].into(),
                            ..Default::default()
                        },
                    );
                    acc = name;
                }
                None => self.warn("filters other than a plain blur are skipped: add ultrafast effects instead".into()),
            }
        }
        // Clip paths: the clip shapes become a matte.
        if let Some(clip) = g.clip_path() {
            if clip.clip_path().is_some() {
                self.warn("nested clip paths are applied one level deep".into());
            }
            let base = self.fit.pre_concat(g.abs_transform()).pre_concat(clip.transform());
            let mut shapes = Vec::new();
            self.clip_shapes(clip.root(), &base, gid, &mut shapes);
            if let Some(first) = shapes.first().cloned() {
                let mut matte = first;
                for sh in &shapes[1..] {
                    let name = self.name(gid, "_clip_union");
                    self.nodes.insert(
                        name.clone(),
                        Node {
                            op: "over".into(),
                            inputs: [("fg".to_string(), sh.clone()), ("bg".to_string(), matte)].into(),
                            ..Default::default()
                        },
                    );
                    matte = name;
                }
                let name = self.name(gid, "_clipped");
                self.nodes.insert(
                    name.clone(),
                    Node {
                        op: "mask".into(),
                        inputs: [("source".to_string(), acc), ("matte".to_string(), matte)].into(),
                        ..Default::default()
                    },
                );
                acc = name;
            }
        }
        // Masks: the mask's own artwork (in the group's coordinates) as a
        // luminance or alpha matte.
        if let Some(mask) = g.mask() {
            if mask.mask().is_some() {
                self.warn("nested masks are applied one level deep".into());
            }
            let saved = self.fit;
            self.fit = self.fit.pre_concat(g.abs_transform());
            let art = self.group(mask.root(), &format!("{gid}_mask"));
            self.fit = saved;
            if let Some(art) = art {
                let name = self.name(gid, "_masked");
                let mode = match mask.kind() {
                    usvg::MaskType::Luminance => "luma",
                    usvg::MaskType::Alpha => "alpha",
                };
                self.nodes.insert(
                    name.clone(),
                    Node {
                        op: "mask".into(),
                        params: [("mode".to_string(), text_param(mode))].into(),
                        inputs: [("source".to_string(), acc), ("matte".to_string(), art)].into(),
                        ..Default::default()
                    },
                );
                acc = name;
            }
        }
        let o = g.opacity().get();
        if o < 0.999 {
            let name = self.name(gid, "_opacity");
            let mut params = BTreeMap::new();
            params.insert("opacity".to_string(), num(o as f64));
            self.nodes.insert(
                name.clone(),
                Node {
                    op: "opacity".into(),
                    params,
                    inputs: [("source".to_string(), acc)].into(),
                    ..Default::default()
                },
            );
            acc = name;
        }
        Some(acc)
    }
}

impl Builder {
    /// White path nodes for the shapes of a clip path (in the clipped
    /// group's coordinates, mapped by `base`).
    fn clip_shapes(&mut self, g: &usvg::Group, base: &Transform, gid: &str, out: &mut Vec<String>) {
        for child in g.children() {
            match child {
                usvg::Node::Group(c) => self.clip_shapes(c, base, gid, out),
                usvg::Node::Text(t) => self.clip_shapes(t.flattened(), base, gid, out),
                usvg::Node::Path(p) => {
                    let to_px = base.pre_concat(p.abs_transform());
                    let rule = match p.fill().map(|f| f.rule()) {
                        Some(usvg::FillRule::EvenOdd) => "evenodd",
                        _ => "nonzero",
                    };
                    let mut params = BTreeMap::new();
                    params.insert("d".to_string(), text_param(&path_data(p.data(), &to_px)));
                    params.insert("color".to_string(), text_param("#ffffff"));
                    params.insert("fill_rule".to_string(), text_param(rule));
                    Self::placed(&mut params);
                    let name = self.name(gid, "_clip");
                    self.nodes.insert(
                        name.clone(),
                        Node {
                            op: "path".into(),
                            params,
                            ..Default::default()
                        },
                    );
                    out.push(name);
                }
                usvg::Node::Image(_) => {}
            }
        }
    }
}

/// The `blend` op's mode for an SVG `mix-blend-mode` (`None`: normal).
fn blend_mode(m: usvg::BlendMode) -> Option<&'static str> {
    use usvg::BlendMode as B;
    Some(match m {
        B::Normal => return None,
        B::Multiply => "multiply",
        B::Screen => "screen",
        B::Overlay => "overlay",
        B::Darken => "darken",
        B::Lighten => "lighten",
        B::ColorDodge => "color_dodge",
        B::ColorBurn => "color_burn",
        B::HardLight => "hard_light",
        B::SoftLight => "soft_light",
        B::Difference => "difference",
        B::Exclusion => "exclusion",
        B::Hue => "hue",
        B::Saturation => "saturation",
        B::Color => "color",
        B::Luminosity => "luminosity",
    })
}

fn stops(s: &[usvg::Stop], opacity: f32) -> String {
    // ultrafast gradients take up to 8 stops: keep the ends and spread the rest.
    let idx: Vec<usize> = if s.len() <= 8 {
        (0..s.len()).collect()
    } else {
        (0..8).map(|i| i * (s.len() - 1) / 7).collect()
    };
    idx.iter()
        .map(|&i| {
            let st = &s[i];
            format!(
                "{:.4}:{}",
                st.offset().get(),
                hex(st.color(), st.opacity().get() * opacity)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether inputs (SVG, Lottie, glTF files) may read other files they
/// name. A service rendering documents from many callers turns this off
/// (`ULTRAFAST_NO_EXTERNAL_FILES=1`): an input then reads only what it embeds,
/// never another path on the machine.
pub fn external_files_allowed() -> bool {
    static ALLOWED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALLOWED.get_or_init(|| std::env::var("ULTRAFAST_NO_EXTERNAL_FILES").as_deref() != Ok("1"))
}

/// Imports SVG data. `fit` scales it to fit inside [w, h] (keeping its
/// aspect); `resources` resolves relative references.
pub fn import(data: &[u8], resources: Option<&Path>, fit: Option<[f64; 2]>) -> Result<Imported, String> {
    let mut opt = usvg::Options {
        resources_dir: resources.map(|p| p.to_path_buf()),
        ..Default::default()
    };
    if !external_files_allowed() {
        // Embedded images (data URLs) only.
        opt.resources_dir = None;
        opt.image_href_resolver = usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        };
    }
    // Text becomes outlines, set from the faces the text op uses: installed
    // and bundled ones, generic families mapped to faces this machine has,
    // and a family it lacks fetched from Google Fonts. (Loaded only when
    // there is text: without fonts usvg drops it while parsing.)
    let substituted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    if data.windows(5).any(|w| w == b"<text") {
        static BASE: std::sync::OnceLock<usvg::fontdb::Database> = std::sync::OnceLock::new();
        opt.fontdb = std::sync::Arc::new(BASE.get_or_init(crate::geom::base_database).clone());
        let notes = substituted.clone();
        let select = usvg::FontResolver::default_font_selector();
        opt.font_resolver = usvg::FontResolver {
            select_font: Box::new(move |font, db| {
                let has = |db: &usvg::fontdb::Database, name: &str| {
                    db.faces()
                        .any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
                };
                // As in CSS, the first family in the list this machine has
                // (or can fetch) is the one; a generic family always is.
                let mut missing = Vec::new();
                for family in font.families() {
                    let usvg::FontFamily::Named(name) = family else {
                        break;
                    };
                    if !has(db, name) {
                        let files = crate::fontfetch::fetch(name);
                        if !files.is_empty() {
                            let db = std::sync::Arc::make_mut(db);
                            for f in files {
                                let _ = db.load_font_file(f);
                            }
                        }
                    }
                    if has(db, name) {
                        missing.clear();
                        break;
                    }
                    missing.push(format!("`{name}`"));
                }
                let id = select(font, db);
                if !missing.is_empty() {
                    let set = id
                        .and_then(|id| db.face(id))
                        .and_then(|f| f.families.first())
                        .map(|(n, _)| format!("is set in `{n}`"))
                        .unwrap_or_else(|| "is left out".into());
                    let note = format!(
                        "text in {} {set}: {} on this machine or Google Fonts",
                        missing.join(", "),
                        if missing.len() == 1 {
                            "that font isn't"
                        } else {
                            "none of them is"
                        }
                    );
                    let mut notes = notes.lock().unwrap();
                    if !notes.contains(&note) {
                        notes.push(note);
                    }
                }
                id
            }),
            select_fallback: usvg::FontResolver::default_fallback_selector(),
        };
    }
    let tree = usvg::Tree::from_data(data, &opt).map_err(|e| format!("SVG: {e}"))?;
    let (w, h) = (tree.size().width() as f64, tree.size().height() as f64);
    let k = match fit {
        Some([fw, fh]) if w > 0.0 && h > 0.0 => (fw / w).min(fh / h),
        _ => 1.0,
    };
    let mut b = Builder {
        nodes: BTreeMap::new(),
        used: Default::default(),
        warnings: std::mem::take(&mut *substituted.lock().unwrap()),
        fit: Transform::from_scale(k as f32, k as f32),
        n: 0,
    };
    let root = b.group(tree.root(), "");
    let art = match root {
        Some(r) => r,
        None => {
            let mut params = BTreeMap::new();
            params.insert("color".to_string(), text_param("#00000000"));
            b.nodes.insert(
                "empty".into(),
                Node {
                    op: "solid".into(),
                    params,
                    ..Default::default()
                },
            );
            "empty".to_string()
        }
    };
    let mut params = BTreeMap::new();
    params.insert("opacity".to_string(), text_param("$opacity"));
    b.nodes.insert(
        "svg_out".into(),
        Node {
            op: "opacity".into(),
            params,
            inputs: [("source".to_string(), art)].into(),
            ..Default::default()
        },
    );
    let mut cparams = BTreeMap::new();
    cparams.insert("position".to_string(), vecp(&[0.0, 0.0]));
    cparams.insert("anchor".to_string(), vecp(&[0.0, 0.0]));
    cparams.insert("scale".to_string(), vecp(&[1.0, 1.0]));
    cparams.insert("rotation".to_string(), num(0.0));
    cparams.insert("opacity".to_string(), num(1.0));
    Ok(Imported {
        component: Component {
            nodes: b.nodes,
            output: "svg_out".into(),
            params: cparams,
            inputs: Vec::new(),
        },
        size: [w * k, h * k],
        warnings: b.warnings,
    })
}

/// Reads the `svg` node's file and imports it.
pub fn import_node(node: &Node, base: &Path) -> Result<Imported, String> {
    let src = match node.params.get("src") {
        Some(Param::Text(s)) => s.clone(),
        _ => return Err("`svg` needs `src` (an .svg file)".into()),
    };
    let path = if Path::new(&src).is_absolute() {
        Path::new(&src).to_path_buf()
    } else {
        base.join(&src)
    };
    let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fit = node
        .params
        .get("size")
        .and_then(|p| p.eval(0.0))
        .and_then(|v| match v.len() {
            0 => None,
            1 => Some([v[0], v[0]]),
            _ => Some([v[0], v[1]]),
        });
    import(&data, path.parent(), fit)
}

/// `doc` with every `svg` node replaced by an instance of its imported
/// component. `load` imports one node (cached by the caller).
pub fn expand(
    doc: &Document,
    mut load: impl FnMut(&str, &Node) -> Result<std::sync::Arc<Imported>, String>,
    mut diagnose: impl FnMut(&str, String),
) -> Document {
    let mut d = doc.clone();
    let mut add: Vec<(String, Component)> = Vec::new();
    let mut fix = |scope: &str, nodes: &mut BTreeMap<String, Node>| {
        for (id, node) in nodes.iter_mut() {
            if node.op != "svg" {
                continue;
            }
            let path = if scope.is_empty() {
                id.clone()
            } else {
                format!("{scope}/{id}")
            };
            match load(&path, node) {
                Ok(imp) => {
                    for w in &imp.warnings {
                        diagnose(&path, format!("svg: {w}"));
                    }
                    let name = format!("svg:{path}");
                    add.push((name.clone(), imp.component.clone()));
                    node.op = name;
                    node.params.retain(|k, _| PLACEMENT.contains(&k.as_str()));
                }
                Err(e) => {
                    diagnose(&path, e);
                    node.op = "solid".into();
                    node.params = [("color".to_string(), Param::Text("#00000000".into()))].into();
                }
            }
        }
    };
    fix("", &mut d.nodes);
    let names: Vec<String> = d.components.keys().cloned().collect();
    for n in names {
        let mut c = d.components.remove(&n).unwrap();
        fix(&n, &mut c.nodes);
        d.components.insert(n, c);
    }
    for (name, c) in add {
        d.components.insert(name, c);
    }
    d
}
