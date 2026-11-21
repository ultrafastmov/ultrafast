//! Charts as blocks: `bar_chart` (bars rise in turn, values count up),
//! `line_chart` (the line draws on, points pop as it reaches them, the area
//! under it fills) and `donut` (segments sweep round in turn, with a legend
//! and the share of one in the middle). Like every block they expand into
//! ordinary shape, path and text nodes in the house style.

use crate::blocks::{Builder, Line, OUT, num, param, text};
use serde_json::{Value, json};
use ultrafast_graph::Node;

/// What a chart shares with the other blocks: scale, colors, faces,
/// alignment and where it goes.
pub(crate) struct Look {
    pub s: f64,
    pub w: f64,
    pub h: f64,
    pub ink: String,
    pub muted: String,
    pub accent: String,
    pub sans: String,
    pub mono: String,
    pub center: bool,
    pub width: f64,
    pub pos: Option<Vec<f64>>,
}

/// Colors a donut takes in turn after the accent.
const PALETTE: &[&str] = &[
    "#ffd23f", "#4d7cff", "#36d6a0", "#b18cff", "#ff9f43", "#5ad1ff", "#f6f3ee",
];

fn values(node: &Node) -> Result<Vec<f64>, String> {
    let v = node.params.get("values").and_then(|p| p.eval(0.0)).unwrap_or_default();
    if v.is_empty() {
        return Err("a chart needs `values` (a list of numbers)".into());
    }
    Ok(v)
}

/// `labels`: "Q1, Q2, Q3", or separated by `|` when labels hold commas.
fn labels(node: &Node) -> Vec<String> {
    let Some(l) = text(node, "labels") else {
        return Vec::new();
    };
    let sep = if l.contains('|') { '|' } else { ',' };
    l.split(sep).map(|s| s.trim().to_string()).collect()
}

fn add(b: &mut Builder, base: &str, op: &str, params: Value, inputs: &[(&str, &str)]) -> Result<String, String> {
    let id = b.id(base);
    b.nodes.insert(
        id.clone(),
        Node {
            op: op.into(),
            params: serde_json::from_value(params).map_err(|e| e.to_string())?,
            inputs: inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Default::default()
        },
    );
    Ok(id)
}

/// Fades a layer in from `t` over `d` seconds.
fn fade_in(b: &mut Builder, layer: &str, t: f64, d: f64) -> Result<String, String> {
    add(
        b,
        "fade_in",
        "opacity",
        json!({"opacity": {"keys": [{"t": t, "v": [0.0]}, {"t": t + d, "v": [1.0]}]}}),
        &[("source", layer)],
    )
}

/// Pushes a layer that fades out over the exit.
fn layer(b: &mut Builder, id: String, t_out: f64) {
    let l = b.fade_out(id, t_out);
    b.layers.push(l);
}

/// The value formatting a text node with `number` uses.
fn number_params(node: &Node) -> Value {
    json!({"decimals": num(node, "decimals", 0.0), "prefix": text(node, "prefix").unwrap_or(""),
           "suffix": text(node, "suffix").unwrap_or("")})
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

/// Kicker and title above a chart, from `top`; returns the y under them.
fn heading(b: &mut Builder, node: &Node, k: &Look, left: f64, top: f64) -> Result<f64, String> {
    let s = k.s;
    let mut y = top;
    let at = |l: &Line| {
        if k.center {
            left + k.width / 2.0 - (l.left + l.right) / 2.0
        } else {
            left - l.left
        }
    };
    if let Some(kicker) = text(node, "kicker").filter(|t| !t.is_empty()) {
        let l = b.line(
            "kicker",
            json!({"text": kicker, "font": k.mono, "size": 26.0 * s, "tracking": 4.0 * s, "weight": 500, "color": k.accent}),
        )?;
        let base = y - l.top;
        b.reveal(&l, at(&l), base, 0.0, 0.0)?;
        y = base + l.bottom + 22.0 * s;
    }
    if let Some(title) = text(node, "title").filter(|t| !t.is_empty()) {
        let l = b.line(
            "title",
            json!({"text": title, "font": k.sans, "size": 56.0 * s, "weight": 650, "tracking": -s,
                   "width": k.width, "line_height": 1.1, "color": k.ink,
                   "align": if k.center { "center" } else { "left" }}),
        )?;
        let base = y - l.top;
        b.reveal(&l, at(&l), base, 0.08, 0.04)?;
        y = base + l.bottom + 56.0 * s;
    }
    Ok(y)
}

/// The chart's box: left edge, top, and the plot height.
fn frame(node: &Node, k: &Look) -> (f64, f64, f64) {
    let (x, top) = k
        .pos
        .as_ref()
        .map(|p| (p[0], p[1]))
        .unwrap_or((if k.center { k.w / 2.0 } else { k.w * 0.1 }, k.h * 0.2));
    let left = if k.center { x - k.width / 2.0 } else { x };
    (left, top, num(node, "height", k.h * 0.34))
}

/// The thin rule the chart stands on, drawn left to right.
fn baseline(b: &mut Builder, k: &Look, left: f64, y: f64) -> Result<(), String> {
    let d = format!("M {left} {y} L {} {y}", left + k.width);
    let id = add(
        b,
        "axis",
        "path",
        json!({"d": d, "stroke": (2.0 * k.s).max(1.0), "cap": "round", "color": k.muted,
               "trim_end": {"keys": [{"t": 0.1, "v": [0.0], "ease": {"bezier": OUT}}, {"t": 0.9, "v": [1.0]}]}}),
        &[],
    )?;
    layer(b, id, 0.0);
    Ok(())
}

/// Category labels centered under x positions, every `step`-th one.
fn x_labels(b: &mut Builder, k: &Look, labels: &[String], xs: &[f64], y: f64, times: &[f64]) -> Result<(), String> {
    let step = labels.len().div_ceil(9).max(1);
    for (i, (label, &x)) in labels.iter().zip(xs).enumerate() {
        if i % step != 0 && i + 1 != labels.len() || label.is_empty() {
            continue;
        }
        let l = b.line(
            "label",
            json!({"text": label, "font": k.mono, "size": 24.0 * k.s, "weight": 500, "tracking": 1.0 * k.s, "color": k.muted}),
        )?;
        // One baseline for all, whatever their ascenders.
        b.reveal(
            &l,
            x - (l.left + l.right) / 2.0,
            y + 24.0 * k.s * 0.75,
            times[i],
            0.02 * i as f64,
        )?;
    }
    Ok(())
}

pub(crate) fn bar_chart(b: &mut Builder, node: &Node, k: &Look) -> Result<(), String> {
    let vals = values(node)?;
    let labels = labels(node);
    let s = k.s;
    let n = vals.len();
    let (left, top, plot_h) = frame(node, k);
    let y = heading(b, node, k, left, top)?;
    // Room above the tallest bar for its value.
    let base_y = y + 34.0 * s * 1.7 + plot_h;
    let max = num(node, "max", vals.iter().cloned().fold(0.0, f64::max)).max(1e-9);
    let gap = 0.38;
    let bw = (k.width / (n as f64 + (n as f64 - 1.0) * gap)).min(260.0 * s);
    let span = bw * (n as f64 + (n as f64 - 1.0) * gap);
    let x0 = left + (k.width - span) / 2.0;
    let argmax = (0..n).max_by(|&a, &b| vals[a].total_cmp(&vals[b])).unwrap_or(0);
    let hi = num(node, "highlight", argmax as f64).round() as i64;
    let bar_color = text(node, "color").unwrap_or(&k.muted).to_string();
    let stagger = (0.7 / n as f64).min(0.09);
    let grow = 0.9;
    baseline(b, k, left, base_y)?;
    let mut xs = Vec::new();
    let mut times = Vec::new();
    for (i, &v) in vals.iter().enumerate() {
        let hgt = (v.max(0.0) / max * plot_h).max(0.0);
        let cx = x0 + bw / 2.0 + i as f64 * bw * (1.0 + gap);
        let ti = 0.3 + stagger * i as f64;
        let color = if i as i64 == hi {
            k.accent.as_str()
        } else {
            bar_color.as_str()
        };
        let bar = b.shape(json!({"kind": "rect", "radius": (bw * 0.14).min(14.0 * s), "color": color,
            "center": {"keys": [{"t": ti, "v": [cx, base_y], "ease": {"bezier": OUT}}, {"t": ti + grow, "v": [cx, base_y - hgt / 2.0]}]},
            "size": {"keys": [{"t": ti, "v": [bw, 0.0], "ease": {"bezier": OUT}}, {"t": ti + grow, "v": [bw, hgt]}]}}))?;
        layer(b, bar, 0.02 * i as f64);
        // The value rides the bar's top, counting up with it.
        let ink = if i as i64 == hi {
            k.accent.as_str()
        } else {
            k.ink.as_str()
        };
        let label = b.line(
            "value",
            merge(
                json!({"text": "0", "number": v, "font": k.sans, "size": 34.0 * s, "weight": 650, "color": ink, "align": "center"}),
                number_params(node),
            ),
        )?;
        let pad = 16.0 * s;
        let p = &mut b.nodes.get_mut(&label.id).unwrap().params;
        p.insert(
            "number".into(),
            param(json!({"keys": [{"t": ti, "v": [0.0], "ease": {"bezier": OUT}}, {"t": ti + grow, "v": [v]}]}))?,
        );
        p.insert(
            "position".into(),
            param(
                json!({"keys": [{"t": ti, "v": [cx, base_y - pad], "ease": {"bezier": OUT}},
                                  {"t": ti + grow, "v": [cx, base_y - hgt - pad]}]}),
            )?,
        );
        let shown = fade_in(b, &label.id, ti, 0.25)?;
        layer(b, shown, 0.02 * i as f64);
        xs.push(cx);
        times.push(ti + 0.1);
    }
    x_labels(b, k, &labels, &xs, base_y + 20.0 * s, &times)
}

/// in_out_cubic and its inverse.
fn ease_io(u: f64) -> f64 {
    if u < 0.5 {
        4.0 * u * u * u
    } else {
        1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
    }
}
fn ease_io_inv(p: f64) -> f64 {
    if p < 0.5 {
        (p / 4.0).cbrt()
    } else {
        1.0 - (2.0 * (1.0 - p)).cbrt() / 2.0
    }
}

/// A smooth path through points (Catmull-Rom as cubic Béziers), and the
/// arc length along it at each point.
fn smooth(pts: &[(f64, f64)]) -> (String, Vec<f64>) {
    use kurbo::ParamCurveArclen;
    let mut d = format!("M {} {}", pts[0].0, pts[0].1);
    let mut lens = vec![0.0];
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0);
        let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0);
        d.push_str(&format!(" C {} {} {} {} {} {}", c1.0, c1.1, c2.0, c2.1, p2.0, p2.1));
        let seg = kurbo::CubicBez::new(p1, c1, c2, p2);
        lens.push(lens[i] + seg.arclen(1e-3));
    }
    (d, lens)
}

pub(crate) fn line_chart(b: &mut Builder, node: &Node, k: &Look) -> Result<(), String> {
    let vals = values(node)?;
    if vals.len() < 2 {
        return Err("a line_chart needs at least two `values`".into());
    }
    let labels = labels(node);
    let s = k.s;
    let n = vals.len();
    let (left, top, plot_h) = frame(node, k);
    let y = heading(b, node, k, left, top)?;
    let base_y = y + 34.0 * s * 1.7 + plot_h;
    let max = num(node, "max", vals.iter().cloned().fold(f64::MIN, f64::max));
    let min = num(node, "min", 0.0_f64.min(vals.iter().cloned().fold(f64::MAX, f64::min)));
    let range = (max - min).max(1e-9);
    let pad_x = 14.0 * s;
    let pts: Vec<(f64, f64)> = vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            (
                left + pad_x + (k.width - 2.0 * pad_x) * i as f64 / (n - 1) as f64,
                base_y - (v - min) / range * plot_h,
            )
        })
        .collect();
    let (d, lens) = smooth(&pts);
    let total = lens[n - 1].max(1e-9);
    let color = text(node, "color").unwrap_or(&k.accent).to_string();
    let (t0, dur) = (0.35, num(node, "draw_time", 1.6));
    let arrive: Vec<f64> = lens.iter().map(|l| t0 + dur * ease_io_inv(l / total)).collect();
    baseline(b, k, left, base_y)?;
    // The area under the line, uncovered left to right with it.
    if !matches!(node.params.get("area"), Some(ultrafast_graph::Param::Bool(false))) {
        let area_d = format!("{d} L {} {base_y} L {} {base_y} Z", pts[n - 1].0, pts[0].0);
        let tint = match crate::ops::parse_hex(&color) {
            Some(c) => format!(
                "#{:02x}{:02x}{:02x}2e",
                (c[0] * 255.0).round() as u8,
                (c[1] * 255.0).round() as u8,
                (c[2] * 255.0).round() as u8
            ),
            None => color.clone(),
        };
        let area = add(b, "area", "path", json!({"d": area_d, "color": tint}), &[])?;
        let ax = pts[0].0;
        let mut keys: Vec<Value> = Vec::new();
        for j in 0..=12 {
            let u = j as f64 / 12.0;
            let p = ease_io(u);
            // The crop edge follows the line's tip: x at this share of length.
            let l = p * total;
            let i = lens.iter().position(|&x| x >= l).unwrap_or(n - 1).max(1);
            let f = ((l - lens[i - 1]) / (lens[i] - lens[i - 1]).max(1e-9)).clamp(0.0, 1.0);
            let x = pts[i - 1].0 + (pts[i].0 - pts[i - 1].0) * f;
            keys.push(json!({"t": t0 + dur * u, "v": [ax - 2.0, top, (x - ax + 2.0).max(0.0), base_y - top + 2.0]}));
        }
        let crop = add(
            b,
            "area_reveal",
            "crop",
            json!({"rect": {"keys": keys}, "softness": 8.0 * s}),
            &[("source", &area)],
        )?;
        layer(b, crop, 0.0);
    }
    let line = add(
        b,
        "line",
        "path",
        json!({"d": d, "stroke": 5.0 * s, "cap": "round", "join": "round", "color": color,
               "trim_end": {"keys": [{"t": t0, "v": [0.0], "ease": "in_out_cubic"}, {"t": t0 + dur, "v": [1.0]}]}}),
        &[],
    )?;
    layer(b, line, 0.0);
    let argmax = n - 1;
    let hi = num(node, "highlight", argmax as f64).round() as i64;
    for (i, &(x, py)) in pts.iter().enumerate() {
        let big = i as i64 == hi;
        let r = if big { 13.0 } else { 8.0 } * s;
        let ta = arrive[i];
        let dot = b.shape(json!({"kind": "circle", "center": [x, py], "color": if big { &k.ink } else { &color },
            "size": {"keys": [{"t": ta - 0.05, "v": [0.0, 0.0], "ease": "out_back"}, {"t": ta + 0.35, "v": [2.0 * r, 2.0 * r]}]}}))?;
        layer(b, dot, 0.0);
        if big {
            // Its value above it, once the line arrives.
            let label = b.line(
                "value",
                merge(
                    json!({"text": "0", "number": vals[i], "font": k.sans, "size": 40.0 * s, "weight": 700, "color": k.ink}),
                    number_params(node),
                ),
            )?;
            let lx = (x - (label.left + label.right) / 2.0)
                .min(left + k.width - label.right)
                .max(left - label.left);
            b.reveal(&label, lx, py - 30.0 * s - label.bottom, ta + 0.05, 0.0)?;
        }
    }
    let xs: Vec<f64> = pts.iter().map(|p| p.0).collect();
    x_labels(b, k, &labels, &xs, base_y + 20.0 * s, &arrive)
}

/// SVG arc along a circle of radius `r` about (cx, cy), degrees clockwise
/// from 3 o'clock.
fn arc(cx: f64, cy: f64, r: f64, a0: f64, a1: f64) -> String {
    let p = |a: f64| (cx + r * a.to_radians().cos(), cy + r * a.to_radians().sin());
    let (x0, y0) = p(a0);
    // A full circle is two half arcs.
    if a1 - a0 >= 359.99 {
        let (xm, ym) = p(a0 + 180.0);
        return format!("M {x0} {y0} A {r} {r} 0 0 1 {xm} {ym} A {r} {r} 0 0 1 {x0} {y0}");
    }
    let (x1, y1) = p(a1);
    let large = (a1 - a0 > 180.0) as u8;
    format!("M {x0} {y0} A {r} {r} 0 {large} 1 {x1} {y1}")
}

pub(crate) fn donut(b: &mut Builder, node: &Node, k: &Look) -> Result<(), String> {
    let vals: Vec<f64> = values(node)?.into_iter().map(|v| v.max(0.0)).collect();
    let total: f64 = vals.iter().sum::<f64>().max(1e-9);
    let labels = labels(node);
    let s = k.s;
    let n = vals.len();
    let (left, top, plot_h) = frame(node, k);
    let y = heading(b, node, k, left, top)?;
    let wide = k.w >= k.h;
    let r = num(
        node,
        "radius",
        if wide {
            (k.width * 0.22).min(plot_h * 0.62)
        } else {
            (k.width * 0.36).min(plot_h * 0.62)
        },
    );
    let thick = num(node, "thickness", r * 0.3);
    let (cx, cy) = if wide {
        (left + r, y + r)
    } else {
        (left + k.width / 2.0, y + r)
    };
    let colors: Vec<String> = match text(node, "colors") {
        Some(c) => c.split(',').map(|s| s.trim().to_string()).collect(),
        None => std::iter::once(k.accent.clone())
            .chain(PALETTE.iter().map(|s| s.to_string()))
            .collect(),
    };
    let (t0, dur) = (0.3, 1.5);
    // One sweep round the ring (out cubic); each segment draws its share.
    let at = |p: f64| t0 + dur * (1.0 - (1.0 - p).max(0.0).cbrt());
    let gap = if n > 1 { 1.2 } else { 0.0 };
    let mut cum = 0.0;
    let mut shares = Vec::new();
    for (i, &v) in vals.iter().enumerate() {
        let (f0, f1) = (cum / total, (cum + v) / total);
        cum += v;
        shares.push(f1 - f0);
        if f1 - f0 <= 0.0 {
            continue;
        }
        let (a0, a1) = (-90.0 + 360.0 * f0 + gap, -90.0 + 360.0 * f1 - gap);
        if a1 <= a0 {
            continue;
        }
        let keys: Vec<Value> = (0..=6)
            .map(|j| {
                let q = j as f64 / 6.0;
                json!({"t": at(f0 + (f1 - f0) * q), "v": [q]})
            })
            .collect();
        let seg = add(
            b,
            "segment",
            "path",
            json!({"d": arc(cx, cy, r - thick / 2.0, a0, a1), "stroke": thick, "cap": "butt",
                   "color": colors[i % colors.len()], "trim_end": {"keys": keys}}),
            &[],
        )?;
        layer(b, seg, 0.03 * i as f64);
    }
    // The share of one segment in the middle.
    let argmax = (0..n).max_by(|&a, &b| vals[a].total_cmp(&vals[b])).unwrap_or(0);
    let hi = (num(node, "highlight", argmax as f64).round().max(0.0) as usize).min(n - 1);
    let pct = shares[hi] * 100.0;
    let big = b.line(
        "share",
        json!({"text": "0", "number": pct, "suffix": "%", "font": text(node, "number_font").unwrap_or("ultrafast:wide"),
               "size": r * 0.42, "weight": 800, "tracking": -2.0 * s, "color": k.ink}),
    )?;
    b.nodes.get_mut(&big.id).unwrap().params.insert(
        "number".into(),
        param(json!({"keys": [{"t": t0, "v": [0.0], "ease": {"bezier": OUT}}, {"t": t0 + dur, "v": [pct]}]}))?,
    );
    let name = labels.get(hi).cloned().unwrap_or_default();
    let below = if name.is_empty() {
        None
    } else {
        Some(b.line(
            "share_label",
            json!({"text": name, "font": k.mono, "size": (r * 0.1).max(20.0 * s), "weight": 500,
                   "tracking": 2.0 * s, "color": k.muted}),
        )?)
    };
    let gap_y = 12.0 * s;
    let block_h = (big.bottom - big.top) + below.as_ref().map(|l| l.bottom - l.top + gap_y).unwrap_or(0.0);
    let big_base = cy - block_h / 2.0 - big.top;
    b.reveal(&big, cx - (big.left + big.right) / 2.0, big_base, 0.35, 0.0)?;
    if let Some(l) = below {
        b.reveal(
            &l,
            cx - (l.left + l.right) / 2.0,
            big_base + big.bottom + gap_y - l.top,
            0.45,
            0.02,
        )?;
    }
    // Legend: a dot, the label and its share per row.
    if !labels.is_empty() {
        let row = 58.0 * s;
        let (lx, ly) = if wide {
            (cx + r + 90.0 * s, cy - row * n as f64 / 2.0 + row / 2.0)
        } else {
            (left + k.width * 0.12, cy + r + 80.0 * s)
        };
        for (i, label) in labels.iter().enumerate().take(n) {
            let yy = ly + row * i as f64;
            let ti = 0.5 + 0.08 * i as f64;
            let dot = b.shape(json!({"kind": "circle", "center": [lx + 9.0 * s, yy], "color": colors[i % colors.len()],
                "size": {"keys": [{"t": ti, "v": [0.0, 0.0], "ease": "out_back"}, {"t": ti + 0.4, "v": [18.0 * s, 18.0 * s]}]}}))?;
            layer(b, dot, 0.03 * i as f64);
            let l = b.line(
                "legend",
                json!({"text": label, "font": k.sans, "size": 32.0 * s, "weight": 500, "color": k.ink}),
            )?;
            b.reveal(
                &l,
                lx + 34.0 * s - l.left,
                yy - (l.top + l.bottom) / 2.0,
                ti,
                0.03 * i as f64,
            )?;
            let p = b.line(
                "legend_share",
                json!({"text": format!("{:.0}%", shares[i] * 100.0), "font": k.mono, "size": 26.0 * s,
                       "weight": 500, "color": k.muted}),
            )?;
            let px = lx + 34.0 * s + (l.right - l.left) + 18.0 * s;
            b.reveal(
                &p,
                px - p.left,
                yy - (l.top + l.bottom) / 2.0,
                ti + 0.05,
                0.03 * i as f64,
            )?;
        }
    }
    Ok(())
}
