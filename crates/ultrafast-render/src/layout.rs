//! `stack`: lays its inputs out in a column or a row by their measured
//! bounds, so agents compose blocks of type, artwork and shapes without
//! computing positions. Each child is wrapped in a translation (absorbed by
//! vector content, so it stays sharp), and the stack becomes their
//! composite. Children keep their own animation, relative to the layout.

use crate::geom::{self, Fonts};
use crate::ops::Affine;
use std::collections::BTreeMap;
use std::path::Path;
use ultrafast_graph::{Document, Node, Param};

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "stack")
}

fn num(node: &Node, k: &str, d: f64) -> f64 {
    node.params
        .get(k)
        .and_then(|p| p.eval(0.0))
        .and_then(|v| v.first().copied())
        .unwrap_or(d)
}

fn text<'a>(node: &'a Node, k: &str) -> Option<&'a str> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s),
        _ => None,
    }
}

/// Frame bounds [x0, y0, x1, y1] of a child as placed by its own params,
/// at time `t`.
fn bounds(
    id: &str,
    t: f64,
    doc: &Document,
    done: &BTreeMap<String, [f64; 4]>,
    fonts: &mut Fonts,
    base: &Path,
) -> Result<[f64; 4], String> {
    if let Some(b) = done.get(id) {
        return Ok(*b);
    }
    let node = doc.nodes.get(id).ok_or_else(|| format!("no node `{id}`"))?;
    let geo = |g: geom::Geometry| {
        [
            g.bounds[0] as f64,
            g.bounds[1] as f64,
            g.bounds[2] as f64,
            g.bounds[3] as f64,
        ]
    };
    match node.op.as_str() {
        "text" => geom::text_node(id, node, &doc.format, t, Affine::IDENTITY, fonts, base)
            .map(geo)
            .map_err(|e| e.to_string()),
        "path" => geom::path(id, node, &doc.format, t, Affine::IDENTITY)
            .map(geo)
            .map_err(|e| e.to_string()),
        "shape" => {
            let c = node.params.get("center").and_then(|p| p.eval(t)).unwrap_or_default();
            let s = node.params.get("size").and_then(|p| p.eval(t)).unwrap_or_default();
            let k = node
                .params
                .get("scale")
                .and_then(|p| p.eval(t))
                .unwrap_or_else(|| vec![1.0]);
            let (cx, cy) = (c.first().copied().unwrap_or(0.0), c.get(1).copied().unwrap_or(0.0));
            let (kx, ky) = (
                k.first().copied().unwrap_or(1.0).abs(),
                k.get(1).or(k.first()).copied().unwrap_or(1.0).abs(),
            );
            let (w, h) = (
                s.first().copied().unwrap_or(0.0) * kx,
                s.get(1).or(s.first()).copied().unwrap_or(0.0) * ky,
            );
            Ok([cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0])
        }
        "svg" => {
            let imp = crate::svg::import_node(node, base)?;
            let p = node.params.get("position").and_then(|p| p.eval(t)).unwrap_or_default();
            let a = node.params.get("anchor").and_then(|p| p.eval(t)).unwrap_or_default();
            let x = p.first().copied().unwrap_or(0.0) - a.first().copied().unwrap_or(0.0);
            let y = p.get(1).copied().unwrap_or(0.0) - a.get(1).copied().unwrap_or(0.0);
            Ok([x, y, x + imp.size[0], y + imp.size[1]])
        }
        // A spacer: `size` [w, h] of empty room.
        "solid" if node.params.contains_key("size") => {
            let s = node.params.get("size").and_then(|p| p.eval(0.0)).unwrap_or_default();
            Ok([
                0.0,
                0.0,
                s.first().copied().unwrap_or(0.0),
                s.get(1).copied().unwrap_or(0.0),
            ])
        }
        other => Err(format!(
            "`stack` lays out text, path, shape, svg and stack nodes; `{id}` is a `{other}` (wrap it in a stack with a fixed-size spacer, or place it by hand)"
        )),
    }
}

/// `doc` with every `stack` expanded into translated children and their
/// composite. Problems are reported per node; a failed stack shows nothing.
pub fn expand(doc: &Document, fonts: &mut Fonts, base: &Path) -> (Document, Vec<(String, String)>) {
    let mut d = doc.clone();
    let mut problems = Vec::new();
    let mut done: BTreeMap<String, [f64; 4]> = BTreeMap::new();
    // Per expanded stack, the translations that place its leaves: an outer
    // stack moves those instead of wrapping (and resampling) the composite.
    let mut leaves: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Inner stacks first, so outer ones can measure them.
    let order = match doc.reachable_topo_order() {
        Ok(o) => o,
        Err(e) => return (d, vec![(String::new(), e.to_string())]),
    };
    let mut stacks: Vec<String> = order.into_iter().filter(|id| doc.nodes[id].op == "stack").collect();
    // Stacks not reachable from the output are still expanded (they may be
    // previewed); they go last.
    for (id, n) in &doc.nodes {
        if n.op == "stack" && !stacks.contains(id) {
            stacks.push(id.clone());
        }
    }
    for id in stacks {
        let node = d.nodes[&id].clone();
        let mut ports: Vec<(&String, &String)> = node.inputs.iter().collect();
        ports.sort_by_key(|(p, _)| {
            p.trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        });
        let children: Vec<String> = ports.iter().map(|(_, src)| (*src).clone()).collect();
        let row = text(&node, "direction") == Some("row");
        let gap = num(&node, "gap", 24.0);
        let align = text(&node, "align").unwrap_or("start").to_string();
        let anchor = text(&node, "anchor").unwrap_or("top_left").replace('-', "_");
        let pos = node
            .params
            .get("position")
            .and_then(|p| p.eval(0.0))
            .unwrap_or_default();
        let pos = [pos.first().copied().unwrap_or(0.0), pos.get(1).copied().unwrap_or(0.0)];
        let mut bs = Vec::new();
        let mut failed = None;
        // Children are measured at `at` seconds: when entrances have landed.
        let at = num(&node, "at", 0.0);
        for c in &children {
            match bounds(c, at, &d, &done, fonts, base) {
                Ok(b) => bs.push(b),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        let empty = || Node {
            op: "solid".into(),
            params: [("color".to_string(), Param::Text("#00000000".into()))].into(),
            ..Default::default()
        };
        if let Some(e) = failed {
            problems.push((id.clone(), e));
            d.nodes.insert(id.clone(), empty());
            continue;
        }
        if children.is_empty() {
            d.nodes.insert(id.clone(), empty());
            continue;
        }
        let size = |b: &[f64; 4]| (b[2] - b[0], b[3] - b[1]);
        let main: f64 =
            bs.iter().map(|b| if row { size(b).0 } else { size(b).1 }).sum::<f64>() + gap * (bs.len() - 1) as f64;
        let cross = bs
            .iter()
            .map(|b| if row { size(b).1 } else { size(b).0 })
            .fold(0.0, f64::max);
        let (w, h) = if row { (main, cross) } else { (cross, main) };
        // Where the block's top-left goes, from its anchor.
        let fx = if anchor.ends_with("left") {
            0.0
        } else if anchor.ends_with("right") {
            1.0
        } else {
            0.5
        };
        let fy = if anchor.starts_with("top") {
            0.0
        } else if anchor.starts_with("bottom") {
            1.0
        } else {
            0.5
        };
        let (bx, by) = (pos[0] - fx * w, pos[1] - fy * h);
        let along = match align.as_str() {
            "center" => 0.5,
            "end" => 1.0,
            _ => 0.0,
        };
        let mut cursor = 0.0;
        let mut layers = Vec::new();
        let mut mine = Vec::new();
        for (k, (c, b)) in children.iter().zip(&bs).enumerate() {
            let (cw, ch) = size(b);
            let (tx, ty) = if row {
                (bx + cursor, by + (h - ch) * along)
            } else {
                (bx + (w - cw) * along, by + cursor)
            };
            cursor += if row { cw } else { ch } + gap;
            let (dx, dy) = (tx - b[0], ty - b[1]);
            if let Some(inner) = leaves.get(c).cloned() {
                // A stack inside: move its leaves.
                for l in &inner {
                    if let Some(Param::Vector(v)) = d.nodes.get_mut(l).and_then(|n| n.params.get_mut("position")) {
                        v[0] += dx;
                        v[1] += dy;
                    }
                }
                mine.extend(inner);
                layers.push(c.clone());
                continue;
            }
            let tid = format!("{id}/{k}");
            let mut p = BTreeMap::new();
            p.insert("anchor".to_string(), Param::Vector(vec![0.0, 0.0]));
            p.insert("position".to_string(), Param::Vector(vec![dx, dy]));
            d.nodes.insert(
                tid.clone(),
                Node {
                    op: "transform".into(),
                    params: p,
                    inputs: [("source".to_string(), c.clone())].into(),
                    ..Default::default()
                },
            );
            mine.push(tid.clone());
            layers.push(tid);
        }
        let mut acc = layers[0].clone();
        for (k, l) in layers.iter().enumerate().skip(1) {
            let oid = if k == layers.len() - 1 {
                id.clone()
            } else {
                format!("{id}/over{k}")
            };
            d.nodes.insert(
                oid.clone(),
                Node {
                    op: "over".into(),
                    inputs: [("fg".to_string(), l.clone()), ("bg".to_string(), acc)].into(),
                    timing: if k == layers.len() - 1 { node.timing } else { None },
                    ..Default::default()
                },
            );
            acc = oid;
        }
        if layers.len() == 1 {
            // The stack is its only child, moved.
            let only = layers[0].clone();
            let mut t = d.nodes.get(&only).cloned().unwrap();
            if mine.contains(&only) {
                d.nodes.remove(&only);
                mine.retain(|m| *m != only);
                mine.push(id.clone());
            } else {
                // A single inner stack: the outer is just a pass-through.
                t = Node {
                    op: "opacity".into(),
                    params: [("opacity".to_string(), Param::Number(1.0))].into(),
                    inputs: [("source".to_string(), only)].into(),
                    ..Default::default()
                };
            }
            t.timing = node.timing;
            d.nodes.insert(id.clone(), t);
        }
        done.insert(id.clone(), [bx, by, bx + w, by + h]);
        leaves.insert(id.clone(), mine);
    }
    (d, problems)
}
