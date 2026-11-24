//! Ready-made animated blocks: a title card, a lower third, a pull quote,
//! a stat, a chapter marker and an end card, in one house style (masked
//! rises on an expo ease, staggered; a masked exit). A block is one node in
//! the document; it expands into a component of ordinary text, shape and
//! path nodes measured and laid out for the frame, so give it a timing
//! `offset` to place it in time, and `ultrafast` treats every part as editable.

use crate::geom::{self, Fonts};
use crate::ops::Affine;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use ultrafast_graph::{Component, Document, Format, Node, Param};

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "block")
}

/// Params a block instance keeps (the rest are consumed at expansion).
pub const PARAMS: &[&str] = &["opacity"];

pub(crate) fn text<'a>(node: &'a Node, k: &str) -> Option<&'a str> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s),
        _ => None,
    }
}

pub(crate) fn num(node: &Node, k: &str, d: f64) -> f64 {
    node.params
        .get(k)
        .and_then(|p| p.eval(0.0))
        .and_then(|v| v.first().copied())
        .unwrap_or(d)
}

pub(crate) fn param(v: Value) -> Result<Param, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

pub(crate) const OUT: [f64; 4] = [0.16, 1.0, 0.3, 1.0];
pub(crate) const IN: [f64; 4] = [0.7, 0.0, 0.84, 0.0];

pub(crate) struct Builder<'a> {
    pub(crate) nodes: BTreeMap<String, Node>,
    pub(crate) layers: Vec<String>,
    pub(crate) fmt: Format,
    pub(crate) fonts: &'a mut Fonts,
    pub(crate) base: &'a Path,
    /// When the exit starts (local seconds); None: stays.
    pub(crate) exit: Option<f64>,
    pub(crate) n: usize,
    /// The color of *emphasis*.
    pub(crate) accent: String,
}

/// A laid-out line of type: its node id and ink bounds relative to its
/// baseline origin.
pub(crate) struct Line {
    pub(crate) id: String,
    pub(crate) top: f64,
    pub(crate) bottom: f64,
    pub(crate) left: f64,
    pub(crate) right: f64,
}

impl Builder<'_> {
    pub(crate) fn id(&mut self, base: &str) -> String {
        self.n += 1;
        format!("{base}{}", self.n)
    }

    /// A text node at baseline origin (0, 0), measured.
    pub(crate) fn line(&mut self, name: &str, params: Value) -> Result<Line, String> {
        let mut p: BTreeMap<String, Param> = serde_json::from_value(params).map_err(|e| e.to_string())?;
        // *Emphasis* in a block's text is set in the accent, italic.
        if matches!(p.get("text"), Some(Param::Text(t)) if t.contains('*')) {
            p.entry("em_color".into())
                .or_insert_with(|| Param::Text(self.accent.clone()));
            p.entry("em_style".into())
                .or_insert_with(|| Param::Text("italic".into()));
        }
        p.insert("valign".into(), Param::Text("baseline".into()));
        p.insert("position".into(), Param::Vector(vec![0.0, 0.0]));
        let node = Node {
            op: "text".into(),
            params: p,
            ..Default::default()
        };
        let g = geom::text_node(name, &node, &self.fmt, 0.0, Affine::IDENTITY, self.fonts, self.base)
            .map_err(|e| e.to_string())?;
        let id = self.id(name);
        self.nodes.insert(id.clone(), node);
        Ok(Line {
            id,
            top: g.bounds[1] as f64,
            bottom: g.bounds[3] as f64,
            left: g.bounds[0] as f64,
            right: g.bounds[2] as f64,
        })
    }

    /// Places a line at baseline (x, y) with a masked rise in at `t_in` and
    /// a masked rise out at the exit (+ `t_out` stagger).
    pub(crate) fn reveal(&mut self, l: &Line, x: f64, y: f64, t_in: f64, t_out: f64) -> Result<(), String> {
        let h = (l.bottom - l.top).max(1.0);
        let rise = h * 1.15;
        let mut ks = vec![
            json!({"t": t_in, "v": [x, y + rise], "ease": {"bezier": OUT}}),
            json!({"t": t_in + 0.75, "v": [x, y]}),
        ];
        if let Some(e) = self.exit {
            ks.push(json!({"t": e + t_out, "v": [x, y], "ease": {"bezier": IN}}));
            ks.push(json!({"t": e + t_out + 0.45, "v": [x, y - rise]}));
        }
        self.nodes
            .get_mut(&l.id)
            .unwrap()
            .params
            .insert("position".into(), param(json!({ "keys": ks }))?);
        let pad = h * 0.12;
        let crop = self.id("mask");
        self.nodes.insert(
            crop.clone(),
            Node {
                op: "crop".into(),
                params: [(
                    "rect".to_string(),
                    Param::Vector(vec![
                        x + l.left - 400.0,
                        y + l.top - pad,
                        l.right - l.left + 800.0,
                        h + 2.0 * pad,
                    ]),
                )]
                .into(),
                inputs: [("source".to_string(), l.id.clone())].into(),
                ..Default::default()
            },
        );
        self.layers.push(crop);
        Ok(())
    }

    pub(crate) fn shape(&mut self, params: Value) -> Result<String, String> {
        let id = self.id("shape");
        let p: BTreeMap<String, Param> = serde_json::from_value(params).map_err(|e| e.to_string())?;
        self.nodes.insert(
            id.clone(),
            Node {
                op: "shape".into(),
                params: p,
                ..Default::default()
            },
        );
        Ok(id)
    }

    /// Fades a layer out over the exit.
    pub(crate) fn fade_out(&mut self, layer: String, t_out: f64) -> String {
        let Some(e) = self.exit else { return layer };
        let id = self.id("fade");
        let o =
            json!({"keys": [{"t": e + t_out, "v": [1.0], "ease": {"bezier": IN}}, {"t": e + t_out + 0.4, "v": [0.0]}]});
        self.nodes.insert(
            id.clone(),
            Node {
                op: "opacity".into(),
                params: [("opacity".to_string(), serde_json::from_value(o).unwrap())].into(),
                inputs: [("source".to_string(), layer)].into(),
                ..Default::default()
            },
        );
        id
    }
}

/// Expands a `block` node into a component.
/// Local seconds until a block has fully exited (None: it stays).
pub fn lifetime(node: &Node) -> Option<f64> {
    let kind = text(node, "kind").unwrap_or("title");
    let duration = num(node, "duration", default_duration(kind));
    (kind != "end_card" && duration > 0.0).then_some(duration + 1.5)
}

fn default_duration(kind: &str) -> f64 {
    match kind {
        "lower_third" => 4.0,
        "quote" => 5.0,
        "chapter" => 2.5,
        "bar_chart" | "line_chart" | "donut" => 5.0,
        _ => 3.5,
    }
}

pub fn build(node: &Node, fmt: &Format, fonts: &mut Fonts, base: &Path) -> Result<Component, String> {
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    let s = w.min(h) / 1080.0 * num(node, "scale", 1.0);
    let kind = text(node, "kind").unwrap_or("title");
    let ink = text(node, "ink").unwrap_or("#f6f3ee").to_string();
    // Dark type means a light page: secondary type and the accent need
    // more weight there to stay readable.
    let dark_ink = {
        let h = ink.trim_start_matches('#');
        let c = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(255) as f64;
        0.2126 * c(0) + 0.7152 * c(2) + 0.0722 * c(4) < 128.0
    };
    let muted = text(node, "muted").map(String::from).unwrap_or_else(|| {
        if ink.len() == 7 {
            format!("{ink}{}", if dark_ink { "e0" } else { "b8" })
        } else {
            ink.clone()
        }
    });
    let accent = text(node, "accent")
        .unwrap_or(if dark_ink { "#c8153f" } else { "#ff5c7a" })
        .to_string();
    let display = text(node, "display_font").unwrap_or("ultrafast:display").to_string();
    let sans = text(node, "text_font").unwrap_or("ultrafast:sans").to_string();
    let mono = text(node, "mono_font").unwrap_or("ultrafast:mono").to_string();
    let center = text(node, "align") == Some("center");
    let width = num(node, "width", w * 0.8);
    let duration = num(node, "duration", default_duration(kind));
    let exit = (kind != "end_card" && duration > 0.0).then_some(duration);
    let pos = node.params.get("position").and_then(|p| p.eval(0.0));
    let field = |k: &str| text(node, k).unwrap_or("").to_string();
    let mut b = Builder {
        nodes: BTreeMap::new(),
        layers: Vec::new(),
        fmt: *fmt,
        fonts,
        base,
        exit,
        n: 0,
        accent: accent.clone(),
    };
    let anchor_x = |l: &Line, x: f64| {
        if center {
            x - (l.left + l.right) / 2.0
        } else {
            x - l.left
        }
    };

    match kind {
        "title" | "chapter" => {
            let (x, top) = pos
                .map(|p| (p[0], p[1]))
                .unwrap_or((if center { w / 2.0 } else { w * 0.1 }, h * 0.38));
            let mut lines: Vec<(Line, f64)> = Vec::new();
            let kicker = if kind == "chapter" {
                field("number")
            } else {
                field("kicker")
            };
            if !kicker.is_empty() {
                let l = b.line(
                    "kicker",
                    json!({"text": kicker, "font": mono, "size": 28.0 * s, "tracking": 4.0 * s,
                    "weight": 500, "color": accent}),
                )?;
                lines.push((l, 30.0 * s));
            }
            let title = field("title");
            let size = if kind == "chapter" { 104.0 } else { 120.0 } * s;
            let l = b.line(
                "title",
                json!({"text": title, "font": display, "size": size, "weight": 650, "axis_opsz": 144,
                "axis_SOFT": 50, "tracking": -2.0 * s, "width": width, "line_height": 1.04, "color": ink,
                "align": if center { "center" } else { "left" }}),
            )?;
            lines.push((l, 34.0 * s));
            let sub = field("subtitle");
            if !sub.is_empty() {
                let l = b.line(
                    "subtitle",
                    json!({"text": sub, "font": sans, "size": 40.0 * s, "width": width,
                    "line_height": 1.35, "color": muted, "align": if center { "center" } else { "left" }}),
                )?;
                lines.push((l, 0.0));
            }
            let mut y = top;
            for (k, (l, gap)) in lines.iter().enumerate() {
                let baseline = y - l.top;
                b.reveal(l, anchor_x(l, x), baseline, 0.1 + 0.12 * k as f64, 0.06 * k as f64)?;
                y = baseline + l.bottom + gap;
            }
            if kind == "chapter" {
                // A rule that draws across under the block.
                let rx = if center { x - width / 2.0 } else { x };
                let rule = b.shape(json!({"kind": "rect", "color": accent,
                    "center": {"keys": [{"t": 0.25, "v": [rx, y + 10.0 * s], "ease": {"bezier": OUT}}, {"t": 1.05, "v": [rx + width / 2.0, y + 10.0 * s]}]},
                    "size": {"keys": [{"t": 0.25, "v": [0.0, 4.0 * s], "ease": {"bezier": OUT}}, {"t": 1.05, "v": [width, 4.0 * s]}]}}))?;
                let rule = b.fade_out(rule, 0.1);
                b.layers.push(rule);
            }
        }
        "lower_third" => {
            let (x, y) = pos.map(|p| (p[0], p[1])).unwrap_or((w * 0.07, h - h.min(w) * 0.2));
            let name = b.line(
                "name",
                json!({"text": field("name"), "font": sans, "size": 52.0 * s, "weight": 700,
                "tracking": -0.5 * s, "color": ink}),
            )?;
            let role = b.line(
                "role",
                json!({"text": field("role"), "font": mono, "size": 26.0 * s, "weight": 500,
                "tracking": 3.0 * s, "color": muted}),
            )?;
            let bar_h = (name.bottom - name.top) + (role.bottom - role.top) + 26.0 * s;
            let bar = b.shape(json!({"kind": "rect", "color": accent, "radius": 3.0 * s,
                "center": [x + 4.0 * s, y - bar_h / 2.0],
                "size": {"keys": [{"t": 0.0, "v": [8.0 * s, 0.0], "ease": {"bezier": OUT}}, {"t": 0.6, "v": [8.0 * s, bar_h]}]}}))?;
            let bar = b.fade_out(bar, 0.15);
            b.layers.push(bar);
            let tx = x + 30.0 * s;
            let name_base = y - bar_h - name.top;
            b.reveal(&name, tx - name.left, name_base, 0.15, 0.0)?;
            let role_base = name_base + name.bottom + 26.0 * s - role.top;
            b.reveal(&role, tx - role.left, role_base, 0.27, 0.06)?;
        }
        "quote" => {
            let (x, top) = pos
                .map(|p| (p[0], p[1]))
                .unwrap_or((if center { w / 2.0 } else { w * 0.1 }, h * 0.3));
            let mark = b.line(
                "mark",
                json!({"text": "\u{201C}", "font": display, "size": 220.0 * s, "weight": 900, "color": accent}),
            )?;
            let q = b.line("quote", json!({"text": field("quote"), "font": "ultrafast:serif", "style": "italic",
                "size": 72.0 * s, "width": width, "line_height": 1.12, "color": ink, "align": if center { "center" } else { "left" }}))?;
            let author = field("author");
            let mark_base = top - mark.top;
            b.reveal(&mark, anchor_x(&mark, x), mark_base, 0.0, 0.0)?;
            let q_base = mark_base + mark.bottom + 6.0 * s - q.top;
            b.reveal(&q, anchor_x(&q, x), q_base, 0.15, 0.05)?;
            if !author.is_empty() {
                let a = b.line(
                    "author",
                    json!({"text": format!("\u{2014} {author}"), "font": mono, "size": 28.0 * s,
                    "tracking": 3.0 * s, "weight": 500, "color": muted}),
                )?;
                let a_base = q_base + q.bottom + 40.0 * s - a.top;
                b.reveal(&a, anchor_x(&a, x), a_base, 0.4, 0.1)?;
            }
        }
        "stat" => {
            let (x, top) = pos
                .map(|p| (p[0], p[1]))
                .unwrap_or((if center { w / 2.0 } else { w * 0.1 }, h * 0.38));
            let value = num(node, "value", 0.0);
            let mut p = json!({"text": "0", "font": text(node, "number_font").unwrap_or("ultrafast:wide"), "size": 200.0 * s, "weight": 800,
                "tracking": -6.0 * s, "color": ink, "decimals": num(node, "decimals", 0.0),
                "prefix": field("prefix"), "suffix": field("suffix"), "align": if center { "center" } else { "left" }});
            // Measure at the final value, then count up to it.
            p["number"] = json!(value);
            let n = b.line("value", p)?;
            let count = json!({"keys": [{"t": 0.1, "v": [0.0], "ease": {"bezier": OUT}}, {"t": 1.4, "v": [value]}]});
            b.nodes
                .get_mut(&n.id)
                .unwrap()
                .params
                .insert("number".into(), param(count)?);
            let base_v = top - n.top;
            b.reveal(&n, anchor_x(&n, x), base_v, 0.05, 0.0)?;
            let label = b.line(
                "label",
                json!({"text": field("label"), "font": mono, "size": 30.0 * s, "weight": 500,
                "tracking": 4.0 * s, "color": muted, "align": if center { "center" } else { "left" }}),
            )?;
            let lb = base_v + n.bottom + 34.0 * s - label.top;
            b.reveal(&label, anchor_x(&label, x), lb, 0.25, 0.06)?;
            let uw = n.right - n.left;
            let ux = if center { x - uw / 2.0 } else { x };
            let uy = lb + label.bottom + 30.0 * s;
            let line = b.id("underline");
            b.nodes.insert(
                line.clone(),
                Node {
                    op: "path".into(),
                    params: serde_json::from_value(json!({"d": format!("M {ux} {uy} L {} {uy}", ux + uw), "stroke": 5.0 * s,
                        "cap": "round", "color": accent,
                        "trim_end": {"keys": [{"t": 0.35, "v": [0.0], "ease": {"bezier": OUT}}, {"t": 1.3, "v": [1.0]}]}}))
                    .map_err(|e| e.to_string())?,
                    ..Default::default()
                },
            );
            let line = b.fade_out(line, 0.12);
            b.layers.push(line);
        }
        "end_card" => {
            let (x, cy) = pos.map(|p| (p[0], p[1])).unwrap_or((w / 2.0, h * 0.45));
            let mut y = cy;
            if let Some(src) = text(node, "logo") {
                let size = 220.0 * s;
                let id = b.id("logo");
                b.nodes.insert(
                    id.clone(),
                    Node {
                        op: "svg".into(),
                        params: serde_json::from_value(json!({"src": src, "size": [size, size], "anchor": [size / 2.0, size / 2.0],
                            "position": [x, y - size / 2.0],
                            "scale": {"keys": [{"t": 0.0, "v": [0.7, 0.7], "ease": {"bezier": OUT}}, {"t": 0.7, "v": [1.0, 1.0]}]},
                            "opacity": {"keys": [{"t": 0.0, "v": [0.0]}, {"t": 0.25, "v": [1.0]}]}}))
                        .map_err(|e| e.to_string())?,
                        ..Default::default()
                    },
                );
                b.layers.push(id);
                y += 60.0 * s;
            }
            // Title and tagline wrap at the block's width, like every block's.
            let t = b.line(
                "title",
                json!({"text": field("title"), "font": text(node, "title_font").unwrap_or("ultrafast:wide"),
                "size": 120.0 * s, "weight": 800, "tracking": -4.0 * s, "width": width, "line_height": 1.04,
                "color": ink, "align": "center"}),
            )?;
            let tb = y - t.top;
            b.reveal(&t, x - (t.left + t.right) / 2.0, tb, 0.2, 0.0)?;
            y = tb + t.bottom + 26.0 * s;
            let tag = field("tagline");
            if !tag.is_empty() {
                let l = b.line(
                    "tagline",
                    json!({"text": tag, "font": "ultrafast:serif", "style": "italic", "size": 56.0 * s,
                    "width": width, "line_height": 1.25, "color": muted, "align": "center"}),
                )?;
                let base = y - l.top;
                b.reveal(&l, x - (l.left + l.right) / 2.0, base, 0.35, 0.0)?;
                y = base + l.bottom + 50.0 * s;
            }
            let cta = field("cta");
            if !cta.is_empty() {
                let l = b.line("cta", json!({"text": cta, "font": sans, "size": 36.0 * s, "weight": 650, "color": "#101014", "align": "center"}))?;
                let (pw, ph) = (l.right - l.left + 80.0 * s, l.bottom - l.top + 44.0 * s);
                let pill = b.shape(
                    json!({"kind": "rect", "center": [x, y + ph / 2.0], "size": [pw, ph], "radius": ph / 2.0,
                    "color": accent}),
                )?;
                let pin = b.id("pill_in");
                b.nodes.insert(
                    pin.clone(),
                    Node {
                        op: "opacity".into(),
                        params: [(
                            "opacity".to_string(),
                            param(json!({"keys": [{"t": 0.5, "v": [0.0]}, {"t": 0.8, "v": [1.0]}]}))?,
                        )]
                        .into(),
                        inputs: [("source".to_string(), pill)].into(),
                        ..Default::default()
                    },
                );
                b.layers.push(pin);
                let base = y + ph / 2.0 - (l.top + l.bottom) / 2.0;
                b.reveal(&l, x - (l.left + l.right) / 2.0, base, 0.55, 0.0)?;
            }
        }
        "bar_chart" | "line_chart" | "donut" => {
            let look = crate::charts::Look {
                s,
                w,
                h,
                ink: ink.clone(),
                muted: muted.clone(),
                accent: accent.clone(),
                sans: sans.clone(),
                mono: mono.clone(),
                center,
                width,
                pos: pos.clone(),
            };
            match kind {
                "bar_chart" => crate::charts::bar_chart(&mut b, node, &look)?,
                "line_chart" => crate::charts::line_chart(&mut b, node, &look)?,
                _ => crate::charts::donut(&mut b, node, &look)?,
            }
        }
        other => {
            return Err(format!(
                "unknown block kind `{other}` (title, lower_third, quote, stat, chapter, end_card, bar_chart, line_chart, donut)"
            ));
        }
    }

    // Composite in order, then the instance's opacity.
    let mut nodes = b.nodes;
    let mut acc: Option<String> = None;
    for (k, l) in b.layers.iter().enumerate() {
        acc = Some(match acc {
            None => l.clone(),
            Some(prev) => {
                let id = format!("over{k}");
                nodes.insert(
                    id.clone(),
                    Node {
                        op: "over".into(),
                        inputs: [("fg".to_string(), l.clone()), ("bg".to_string(), prev)].into(),
                        ..Default::default()
                    },
                );
                id
            }
        });
    }
    let art = acc.ok_or("empty block")?;
    nodes.insert(
        "block_out".into(),
        Node {
            op: "opacity".into(),
            params: [("opacity".to_string(), Param::Text("$opacity".into()))].into(),
            inputs: [("source".to_string(), art)].into(),
            ..Default::default()
        },
    );
    Ok(Component {
        nodes,
        output: "block_out".into(),
        params: [("opacity".to_string(), Param::Number(1.0))].into(),
        inputs: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ink bounds of each text in a built block once it has settled.
    fn settled_text(params: Value, width: u32, height: u32) -> Vec<(String, [f32; 4])> {
        let node: Node = serde_json::from_value(json!({"op": "block", "params": params})).unwrap();
        let fmt = Format {
            width,
            height,
            fps: 30.0,
            duration: 6.0,
            motion_blur: None,
        };
        let mut fonts = Fonts::new();
        let c = build(&node, &fmt, &mut fonts, Path::new(".")).unwrap();
        c.nodes
            .iter()
            .filter(|(_, n)| n.op == "text")
            .map(|(id, n)| {
                let g = geom::text_node(id, n, &fmt, 3.0, Affine::IDENTITY, &mut fonts, Path::new(".")).unwrap();
                (id.clone(), g.bounds)
            })
            .collect()
    }

    #[test]
    fn an_end_card_title_wraps_inside_a_vertical_frame() {
        // 25 characters in the wide face ran 1740 px across a 1080 px frame.
        let texts = settled_text(
            json!({"kind": "end_card", "title": "Seeds that suit your soil",
                   "tagline": "Planting guides for every garden", "cta": "fernway.example"}),
            1080,
            1920,
        );
        assert_eq!(texts.len(), 3, "{texts:?}");
        for (id, b) in &texts {
            assert!(b[0] >= 108.0 - 1.0 && b[2] <= 972.0 + 1.0, "{id} {b:?}");
        }
        // Stacked in order, not overlapping.
        let y = |p: &str| texts.iter().find(|(id, _)| id.starts_with(p)).unwrap().1;
        assert!(
            y("title")[3] < y("tagline")[1] && y("tagline")[3] < y("cta")[1],
            "{texts:?}"
        );
    }
}
