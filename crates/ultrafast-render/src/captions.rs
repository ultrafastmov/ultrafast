//! Animated captions from word timings: the karaoke, pop-on and reveal
//! styles of social video, as ordinary text nodes.
//!
//! Words (from a transcript file or inline) are laid out into pages that
//! fit `width` and `max_lines`, breaking at sentence ends and pauses. Every
//! word is its own `text` node, colored, scaled and faded by keyframes at
//! its own time, and every page is a layer with a visibility window, so a
//! caption track costs only the page on screen, and any word can be
//! edited afterwards (`ultrafast captions` writes the component).

use crate::geom::{Fonts, measure};
use std::collections::BTreeMap;
use std::path::Path;
use ultrafast_graph::{Component, Document, Format, Node, Param};

#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// Parses word timings: JSON (a list of `{word|text, start, end}`, or
/// `{words: [...]}`, or Whisper's `{segments: [{words: [...]}]}`), SRT /
/// WebVTT (each cue's words spread over the cue by length), or plain
/// lines of `start end word`.
pub fn parse(src: &str) -> Result<Vec<Word>, String> {
    let s = src.trim_start_matches('\u{feff}').trim();
    let words = if s.starts_with('[') || s.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| format!("captions JSON: {e}"))?;
        let mut list = Vec::new();
        collect_json(&v, &mut list);
        list
    } else if s.contains("-->") {
        parse_cues(s)?
    } else {
        let mut list = Vec::new();
        for (i, line) in s.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.splitn(3, char::is_whitespace);
            let (a, b, w) = (it.next(), it.next(), it.next());
            match (a.and_then(|a| a.parse().ok()), b.and_then(|b| b.parse().ok()), w) {
                (Some(start), Some(end), Some(w)) => list.push(Word {
                    text: w.trim().to_string(),
                    start,
                    end,
                }),
                _ => return Err(format!("captions line {}: expected `start end word`", i + 1)),
            }
        }
        list
    };
    let mut words: Vec<Word> = words.into_iter().filter(|w| !w.text.trim().is_empty()).collect();
    for w in &mut words {
        w.text = w.text.trim().to_string();
        if w.end < w.start {
            w.end = w.start;
        }
    }
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    if words.is_empty() {
        return Err("captions: no words".into());
    }
    Ok(words)
}

fn collect_json(v: &serde_json::Value, out: &mut Vec<Word>) {
    match v {
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_json(x, out)),
        serde_json::Value::Object(o) => {
            if let Some(ws) = o.get("words").or_else(|| o.get("segments")) {
                return collect_json(ws, out);
            }
            let text = o.get("word").or_else(|| o.get("text")).and_then(|x| x.as_str());
            let num = |k: &str| o.get(k).and_then(|x| x.as_f64());
            if let (Some(t), Some(s), Some(e)) = (text, num("start"), num("end")) {
                out.push(Word {
                    text: t.to_string(),
                    start: s,
                    end: e,
                });
            }
        }
        _ => {}
    }
}

fn parse_time(t: &str) -> Option<f64> {
    // hh:mm:ss,mmm | mm:ss.mmm | ss.mmm
    let t = t.trim().replace(',', ".");
    let parts: Vec<&str> = t.split(':').collect();
    let mut secs = 0.0;
    for p in &parts {
        secs = secs * 60.0 + p.trim().parse::<f64>().ok()?;
    }
    Some(secs)
}

fn parse_cues(s: &str) -> Result<Vec<Word>, String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = s.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some((a, b)) = lines[i].split_once("-->") {
            let start = parse_time(a).ok_or_else(|| format!("captions: bad time `{a}`"))?;
            // VTT cue settings follow the end time.
            let end = parse_time(b.split_whitespace().next().unwrap_or(""))
                .ok_or_else(|| format!("captions: bad time `{b}`"))?;
            let mut text = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().is_empty() {
                text.push(' ');
                text.push_str(lines[i].trim());
                i += 1;
            }
            // Strip simple markup (<i>, {\an8}).
            let mut clean = String::new();
            let mut skip = None;
            for c in text.chars() {
                match (skip, c) {
                    (None, '<') => skip = Some('>'),
                    (None, '{') => skip = Some('}'),
                    (Some(e), c) if c == e => skip = None,
                    (Some(_), _) => {}
                    (None, c) => clean.push(c),
                }
            }
            let ws: Vec<&str> = clean.split_whitespace().collect();
            let total: usize = ws.iter().map(|w| w.chars().count() + 1).sum();
            let mut t = start;
            for w in ws {
                let d = (end - start) * (w.chars().count() + 1) as f64 / total.max(1) as f64;
                out.push(Word {
                    text: w.to_string(),
                    start: t,
                    end: t + d,
                });
                t += d;
            }
        } else {
            i += 1;
        }
    }
    Ok(out)
}

/// Caption style knobs, read from the node's params.
struct Style {
    font: Param,
    size: f64,
    weight: f64,
    color: String,
    highlight: String,
    spoken: String,
    pill: Option<String>,
    /// A rounded box behind each page.
    backing: Option<String>,
    mode: String,
    width: f64,
    max_lines: usize,
    line_height: f64,
    position: [f64; 2],
    align: String,
    uppercase: bool,
    tracking: f64,
    linger: f64,
    /// `[x0, y0, x1, y1]` every page stays inside (`None`: exactly at
    /// `position`).
    safe: Option<[f64; 4]>,
}

/// The area captions keep to, from the `safe` param: `auto` (the default)
/// is clear of the social apps' buttons and captions on a vertical frame
/// (the area `review --safe social` checks) and inside a 4% margin
/// otherwise; `social` and `margin` choose one; `none` places pages exactly
/// at `position`.
fn safe_area(node: &Node, fmt: &Format) -> Option<[f64; 4]> {
    use crate::review::SafeArea;
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    let area = match text(node, "safe").as_deref() {
        Some("none") => return None,
        Some("social") => SafeArea::Social,
        Some("margin") => SafeArea::Margin(0.04),
        _ if h > w => SafeArea::Social,
        _ => SafeArea::Margin(0.04),
    };
    // A little inside it: ink overshoots the font's ascent and descent, and
    // the spoken word swells.
    let inset = 0.012 * w.min(h);
    let [x0, y0, x1, y1] = area.rect(w, h);
    Some([x0 + inset, y0 + inset, x1 - inset, y1 - inset])
}

fn text(node: &Node, k: &str) -> Option<String> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn number(node: &Node, k: &str, d: f64) -> f64 {
    node.params
        .get(k)
        .and_then(|p| p.eval(0.0))
        .and_then(|v| v.first().copied())
        .unwrap_or(d)
}

fn style(node: &Node, fmt: &Format) -> Style {
    let (w, h) = (fmt.width as f64, fmt.height as f64);
    let size = number(node, "size", (w.min(h) * 0.065).round());
    let color = text(node, "color").unwrap_or_else(|| "#ffffff".into());
    let pos = node
        .params
        .get("position")
        .and_then(|p| p.eval(0.0))
        .unwrap_or_default();
    let safe = safe_area(node, fmt);
    // A page's backing box reaches past its words.
    let pad_x = if text(node, "background").is_some() {
        size * 0.42
    } else {
        0.0
    };
    let mut width = number(node, "width", w * 0.82);
    let mut position = [
        pos.first().copied().unwrap_or(w / 2.0),
        pos.get(1).copied().unwrap_or(h * 0.72),
    ];
    if let Some([x0, _, x1, _]) = safe {
        // Lines no wider than the area, and the line box inside it (pages
        // are kept inside vertically as they are laid out).
        width = width.min(x1 - x0 - 2.0 * pad_x).max(size);
        let (lo, hi) = (x0 + pad_x + width / 2.0, x1 - pad_x - width / 2.0);
        position[0] = if lo <= hi {
            position[0].clamp(lo, hi)
        } else {
            (x0 + x1) / 2.0
        };
    }
    Style {
        font: node
            .params
            .get("font")
            .cloned()
            .unwrap_or(Param::Text("Helvetica Neue".into())),
        size,
        weight: number(node, "weight", 800.0),
        highlight: text(node, "highlight").unwrap_or_else(|| "#ffd23f".into()),
        spoken: text(node, "spoken").unwrap_or_else(|| color.clone()),
        color,
        pill: text(node, "highlight_background"),
        backing: text(node, "background"),
        mode: text(node, "style").unwrap_or_else(|| "karaoke".into()),
        width,
        max_lines: number(node, "max_lines", 2.0).max(1.0) as usize,
        line_height: number(node, "line_height", 1.18),
        position,
        align: text(node, "align").unwrap_or_else(|| "center".into()),
        uppercase: matches!(node.params.get("uppercase"), Some(Param::Bool(true))),
        tracking: number(node, "tracking", 0.0),
        linger: number(node, "linger", 0.6),
        safe,
    }
}

/// Params every caption node exposes on its instance.
pub const PARAMS: &[&str] = &["opacity"];

/// Lays the words out into pages of text nodes.
pub fn build(words: &[Word], node: &Node, fmt: &Format, fonts: &mut Fonts, base: &Path) -> Result<Component, String> {
    let st = style(node, fmt);
    let shown = |w: &Word| {
        if st.uppercase {
            w.text.to_uppercase()
        } else {
            w.text.clone()
        }
    };
    let word_node = |s: &str| {
        let mut p = BTreeMap::new();
        p.insert("text".to_string(), Param::Text(s.to_string()));
        p.insert("font".to_string(), st.font.clone());
        p.insert("size".to_string(), Param::Number(st.size));
        p.insert("weight".to_string(), Param::Number(st.weight));
        p.insert("tracking".to_string(), Param::Number(st.tracking));
        for k in ["style", "variations", "features"] {
            if let Some(v) = node.params.get(k)
                && (k != "style" || matches!(v, Param::Text(s) if s == "italic" || s == "normal"))
            {
                p.insert(k.to_string(), v.clone());
            }
        }
        Node {
            op: "text".into(),
            params: p,
            ..Default::default()
        }
    };
    let advance = |fonts: &mut Fonts, s: &str| -> Result<(f64, f64, f64), String> {
        let m = measure("captions", &word_node(s), 0.0, fonts, base).map_err(|e| e.to_string())?;
        Ok((m.advance, m.ascent, m.descent))
    };
    let (space, ascent, descent) = advance(fonts, " ")?;
    // Room for the highlight pill between words.
    let space = if st.pill.is_some() {
        space + st.size * 0.16
    } else {
        space
    };
    let all: String = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let line_px = st.size * st.line_height * fonts.script_leading(&all);
    // A right-to-left line starts at the right, and "left" means its start.
    let rtl = fonts.reads_rtl(&all);

    // Pages of lines of (word index, advance).
    let mut pages: Vec<Vec<Vec<(usize, f64)>>> = Vec::new();
    let mut lines: Vec<Vec<(usize, f64)>> = vec![Vec::new()];
    let mut line_w = 0.0;
    for (i, w) in words.iter().enumerate() {
        let (adv, _, _) = advance(fonts, &shown(w))?;
        let prev = i.checked_sub(1).map(|p| &words[p]);
        let pause = prev.is_some_and(|p| w.start - p.end > 0.8);
        let sentence = prev.is_some_and(|p| p.text.ends_with(['.', '?', '!']))
            && lines.iter().map(|l| l.len()).sum::<usize>() >= 3;
        let fits = lines.last().unwrap().is_empty() || line_w + space + adv <= st.width;
        if (pause || sentence) && !lines[0].is_empty() {
            pages.push(std::mem::replace(&mut lines, vec![Vec::new()]));
            line_w = 0.0;
        } else if !fits {
            if lines.len() >= st.max_lines {
                pages.push(std::mem::replace(&mut lines, vec![Vec::new()]));
            } else {
                lines.push(Vec::new());
            }
            line_w = 0.0;
        }
        let l = lines.last_mut().unwrap();
        if !l.is_empty() {
            line_w += space;
        }
        line_w += adv;
        l.push((i, adv));
    }
    if !lines[0].is_empty() {
        pages.push(lines);
    }

    let first_of = |pi: usize| words[pages[pi][0][0].0].start;
    let last_of = |pi: usize| words[pages[pi].last().unwrap().last().unwrap().0].end;
    // When page `pi` hands over to the next: 0.04 s before the next page's
    // first word, or when this page's last word ends if that is later
    // (speech runs on), and no later than the next first word starts. The
    // one time ends this page and starts the next, so two pages never show
    // at once.
    let handover = |pi: usize| {
        (pi + 1 < pages.len()).then(|| {
            let next = first_of(pi + 1);
            (next - 0.04).max(last_of(pi)).min(next)
        })
    };
    let mut nodes = BTreeMap::new();
    let mut page_layers = Vec::new();
    for (pi, page) in pages.iter().enumerate() {
        let t_in = match pi.checked_sub(1) {
            Some(prev) => handover(prev).unwrap_or(0.0),
            None => (first_of(pi) - 0.04).max(0.0),
        };
        let t_out = match handover(pi) {
            Some(h) => (last_of(pi) + st.linger).min(h),
            None => last_of(pi) + st.linger,
        };
        // Block centered on position: lines stacked, each aligned, and
        // moved into the safe area if it would cross it.
        let block_h = ascent + descent + line_px * (page.len() - 1) as f64;
        let mut cy = st.position[1];
        if let Some([_, y0, _, y1]) = st.safe {
            let pad_y = if st.backing.is_some() { st.size * 0.7 } else { 0.0 };
            let half = block_h / 2.0 + pad_y;
            if y1 - y0 >= 2.0 * half {
                cy = cy.clamp(y0 + half, y1 - half);
            }
        }
        let top = cy - block_h / 2.0;
        let mut acc: Option<String> = None;
        if let Some(bg) = &st.backing {
            let widest = page
                .iter()
                .map(|l| l.iter().map(|(_, a)| a).sum::<f64>() + space * (l.len() - 1) as f64)
                .fold(0.0, f64::max);
            let pad = st.size * 0.42;
            let cx = match st.align.as_str() {
                "left" => st.position[0] - st.width / 2.0 + widest / 2.0,
                "right" => st.position[0] + st.width / 2.0 - widest / 2.0,
                _ => st.position[0],
            };
            let id = format!("p{pi}_background");
            let mut p = BTreeMap::new();
            p.insert("kind".to_string(), Param::Text("rect".into()));
            p.insert("center".to_string(), Param::Vector(vec![cx, cy]));
            p.insert(
                "size".to_string(),
                Param::Vector(vec![widest + 2.0 * pad, block_h + 1.4 * pad]),
            );
            p.insert("radius".to_string(), Param::Number(st.size * 0.35));
            p.insert("color".to_string(), Param::Text(bg.clone()));
            nodes.insert(
                id.clone(),
                Node {
                    op: "shape".into(),
                    params: p,
                    ..Default::default()
                },
            );
            acc = Some(id);
        }
        for (li, line) in page.iter().enumerate() {
            let lw: f64 = line.iter().map(|(_, a)| a).sum::<f64>() + space * (line.len() - 1) as f64;
            let mut x = match st.align.as_str() {
                "left" if rtl => st.position[0] + st.width / 2.0 - lw,
                "left" => st.position[0] - st.width / 2.0,
                "right" => st.position[0] + st.width / 2.0 - lw,
                _ => st.position[0] - lw / 2.0,
            };
            let baseline = top + ascent + line_px * li as f64;
            let in_place: Vec<(usize, f64)> = if rtl {
                line.iter().rev().copied().collect()
            } else {
                line.clone()
            };
            for &(wi, adv) in &in_place {
                let w = &words[wi];
                let id = format!("p{pi}_w{wi}");
                let mut n = word_node(&shown(w));
                let cx = x + adv / 2.0;
                let cy = baseline - st.size * 0.35;
                n.params.insert("valign".into(), Param::Text("baseline".into()));
                n.params.insert("position".into(), Param::Vector(vec![cx, cy]));
                n.params
                    .insert("anchor".into(), Param::Vector(vec![adv / 2.0, -st.size * 0.35]));
                let key = |t: f64, v: &str| serde_json::json!({"t": t, "v": v, "ease": "hold"});
                let color = match st.mode.as_str() {
                    "karaoke" | "pop" => {
                        serde_json::json!({"keys": [key(0.0, &st.color), key(w.start, &st.highlight), key(w.end.max(w.start + 0.08), &st.spoken)]})
                    }
                    _ => serde_json::json!(st.color),
                };
                n.params.insert(
                    "color".into(),
                    serde_json::from_value(color).map_err(|e| e.to_string())?,
                );
                let anim = |ks: serde_json::Value| -> Result<Param, String> {
                    serde_json::from_value(serde_json::json!({ "keys": ks })).map_err(|e| e.to_string())
                };
                match st.mode.as_str() {
                    "pop" => {
                        n.params.insert(
                            "scale".into(),
                            anim(serde_json::json!([
                                {"t": w.start, "v": [0.55, 0.55], "ease": {"bezier": [0.2, 1.6, 0.4, 1.0]}},
                                {"t": w.start + 0.2, "v": [1.0, 1.0]}
                            ]))?,
                        );
                    }
                    "karaoke" => {
                        // The spoken word swells by up to 8%, never past
                        // 40% of a space on either side (long words would
                        // run into their neighbours).
                        let s = (1.0 + 0.8 * space / adv.max(1.0)).min(1.08);
                        n.params.insert(
                            "scale".into(),
                            anim(serde_json::json!([
                                {"t": w.start, "v": [1.0, 1.0], "ease": "out_cubic"},
                                {"t": w.start + 0.08, "v": [s, s], "ease": "in_out_cubic"},
                                {"t": w.end.max(w.start + 0.16), "v": [1.0, 1.0]}
                            ]))?,
                        );
                    }
                    _ => {}
                }
                let mut layer = id.clone();
                nodes.insert(id.clone(), n);
                // Words that are not there yet: pop and reveal bring them in
                // at their time.
                if matches!(st.mode.as_str(), "pop" | "reveal") {
                    let o = format!("{id}_in");
                    let mut p = BTreeMap::new();
                    p.insert(
                        "opacity".to_string(),
                        anim(serde_json::json!([
                            {"t": w.start, "v": [0.0], "ease": "out_cubic"},
                            {"t": w.start + if st.mode == "pop" { 0.06 } else { 0.14 }, "v": [1.0]}
                        ]))?,
                    );
                    nodes.insert(
                        o.clone(),
                        Node {
                            op: "opacity".into(),
                            params: p,
                            inputs: [("source".to_string(), layer)].into(),
                            ..Default::default()
                        },
                    );
                    layer = o;
                }
                // A rounded highlight behind the word being spoken.
                if let Some(pill) = &st.pill {
                    let pid = format!("{id}_pill");
                    let mut p = BTreeMap::new();
                    p.insert("kind".to_string(), Param::Text("rect".into()));
                    p.insert(
                        "center".to_string(),
                        Param::Vector(vec![cx, baseline - (ascent - descent) / 2.0 + descent * 0.1]),
                    );
                    p.insert(
                        "size".to_string(),
                        Param::Vector(vec![adv + st.size * 0.28, (ascent + descent) * 0.92]),
                    );
                    p.insert("radius".to_string(), Param::Number(st.size * 0.18));
                    p.insert("color".to_string(), Param::Text(pill.clone()));
                    nodes.insert(
                        pid.clone(),
                        Node {
                            op: "shape".into(),
                            params: p,
                            ..Default::default()
                        },
                    );
                    let ov = format!("{id}_on_pill");
                    nodes.insert(
                        ov.clone(),
                        Node {
                            op: "over".into(),
                            inputs: [("fg".to_string(), layer.clone()), ("bg".to_string(), pid)].into(),
                            ..Default::default()
                        },
                    );
                    // The pill shows only while the word is spoken.
                    let lit = format!("{id}_lit");
                    nodes.insert(
                        lit.clone(),
                        Node {
                            op: "over".into(),
                            inputs: [("fg".to_string(), ov), ("bg".to_string(), layer.clone())].into(),
                            timing: Some(
                                serde_json::from_value(
                                    serde_json::json!({"in": w.start, "out": w.end.max(w.start + 0.08)}),
                                )
                                .map_err(|e| e.to_string())?,
                            ),
                            ..Default::default()
                        },
                    );
                    layer = lit;
                }
                acc = Some(match acc {
                    None => layer,
                    Some(prev) => {
                        let o = format!("{id}_over");
                        nodes.insert(
                            o.clone(),
                            Node {
                                op: "over".into(),
                                inputs: [("fg".to_string(), layer), ("bg".to_string(), prev)].into(),
                                ..Default::default()
                            },
                        );
                        o
                    }
                });
                x += adv + space;
            }
        }
        page_layers.push((acc.unwrap(), t_in, t_out));
    }
    // Pages, each visible in its window.
    let mut nodes_out = nodes;
    nodes_out.insert(
        "empty".into(),
        Node {
            op: "solid".into(),
            params: [("color".to_string(), Param::Text("#00000000".into()))].into(),
            ..Default::default()
        },
    );
    let mut acc = "empty".to_string();
    for (pi, (layer, t_in, t_out)) in page_layers.into_iter().enumerate() {
        let id = format!("page{pi}");
        nodes_out.insert(
            id.clone(),
            Node {
                op: "over".into(),
                inputs: [("fg".to_string(), layer), ("bg".to_string(), acc)].into(),
                timing: Some(
                    serde_json::from_value(serde_json::json!({"in": t_in, "out": t_out})).map_err(|e| e.to_string())?,
                ),
                ..Default::default()
            },
        );
        acc = id;
    }
    nodes_out.insert(
        "captions_out".into(),
        Node {
            op: "opacity".into(),
            params: [("opacity".to_string(), Param::Text("$opacity".into()))].into(),
            inputs: [("source".to_string(), acc)].into(),
            ..Default::default()
        },
    );
    Ok(Component {
        nodes: nodes_out,
        output: "captions_out".into(),
        params: [("opacity".to_string(), Param::Number(1.0))].into(),
        inputs: Vec::new(),
    })
}

/// Whether `doc` has `captions` nodes to expand.
pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "captions")
}

/// The words of a `captions` node: `src` file or inline `words`.
pub fn words_of(node: &Node, base: &Path) -> Result<Vec<Word>, String> {
    if let Some(src) = text(node, "src") {
        let p = if Path::new(&src).is_absolute() {
            Path::new(&src).to_path_buf()
        } else {
            base.join(&src)
        };
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        return parse(&s);
    }
    match text(node, "words") {
        Some(w) => parse(&w),
        None => Err("`captions` needs `src` (a transcript file) or `words`".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_lists_srt_and_lines() {
        let j = r#"{"segments":[{"words":[{"word":" Hello","start":0.0,"end":0.4},{"word":"world.","start":0.5,"end":0.9}]}]}"#;
        let w = parse(j).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].text, "Hello");
        let srt = "1\n00:00:01,000 --> 00:00:02,000\nfast <i>type</i>\n\n2\n00:00:02,500 --> 00:00:03,000\nnow\n";
        let w = parse(srt).unwrap();
        assert_eq!(
            w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["fast", "type", "now"]
        );
        assert!((w[0].start - 1.0).abs() < 1e-9 && (w[1].end - 2.0).abs() < 1e-9);
        let w = parse("0 0.3 one\n0.3 0.7 two words").unwrap();
        assert_eq!(w[1].text, "two words");
    }

    /// Every word's box, `[x0, y0, x1, y1]`, from the expanded component.
    fn word_boxes(params: &str, fmt: &Format) -> Vec<[f64; 4]> {
        let node: Node = serde_json::from_str(&format!(r#"{{"op":"captions","params":{params}}}"#)).unwrap();
        let words = words_of(&node, Path::new(".")).unwrap();
        let mut fonts = Fonts::new();
        let c = build(&words, &node, fmt, &mut fonts, Path::new(".")).unwrap();
        c.nodes
            .values()
            .filter(|n| n.op == "text")
            .map(|n| {
                let m = measure("w", n, 0.0, &mut fonts, Path::new(".")).unwrap();
                let pos = n.params["position"].eval(0.0).unwrap();
                let anchor = n.params["anchor"].eval(0.0).unwrap();
                let x0 = pos[0] - anchor[0];
                let base = pos[1] - anchor[1];
                [x0, base - m.ascent, x0 + m.advance, base + m.descent]
            })
            .collect()
    }

    #[test]
    fn captions_keep_clear_of_social_buttons_on_vertical_frames() {
        let fmt = Format {
            width: 1080,
            height: 1920,
            fps: 30.0,
            duration: 8.0,
            motion_blur: None,
        };
        let words: String =
            "Captions that run the whole width of a vertical frame used to slide under the share buttons"
                .split(' ')
                .enumerate()
                .map(|(i, w)| format!("{} {} {w}\\n", i as f64 * 0.3, i as f64 * 0.3 + 0.3))
                .collect();
        let [x0, y0, x1, y1] = crate::review::SafeArea::Social.rect(1080.0, 1920.0);
        // By default, and when placed low: inside the area `review --safe
        // social` checks.
        for at in ["", r#","position":[540,1800]"#, r#","position":[900,1000]"#] {
            let boxes = word_boxes(
                &format!(r#"{{"words":"{words}","font":"ultrafast:sans","size":84{at}}}"#),
                &fmt,
            );
            assert!(!boxes.is_empty());
            for b in &boxes {
                assert!(b[0] >= x0 && b[2] <= x1 && b[1] >= y0 && b[3] <= y1, "{at}: {b:?}");
            }
        }
        // `safe: none` places it exactly where asked.
        let boxes = word_boxes(
            &format!(r#"{{"words":"{words}","font":"ultrafast:sans","size":84,"position":[540,1800],"safe":"none"}}"#),
            &fmt,
        );
        assert!(boxes.iter().any(|b| b[3] > y1));
    }

    /// Each page's window, `(in, out)`, and the indices of its first and
    /// last words, in page order.
    fn page_windows(words: &str, fmt: &Format) -> Vec<(f64, f64, usize, usize)> {
        let node: Node = serde_json::from_str(&format!(
            r#"{{"op":"captions","params":{{"words":"{words}","font":"ultrafast:sans","size":84}}}}"#
        ))
        .unwrap();
        let words = words_of(&node, Path::new(".")).unwrap();
        let c = build(&words, &node, fmt, &mut Fonts::new(), Path::new(".")).unwrap();
        (0..)
            .map_while(|pi| Some((pi, c.nodes.get(&format!("page{pi}"))?)))
            .map(|(pi, n)| {
                let t = n.timing.unwrap();
                let prefix = format!("p{pi}_w");
                let ws: Vec<usize> = c
                    .nodes
                    .iter()
                    .filter(|(id, n)| n.op == "text" && id.starts_with(&prefix))
                    .map(|(id, _)| id[prefix.len()..].parse().unwrap())
                    .collect();
                let (lo, hi) = (*ws.iter().min().unwrap(), *ws.iter().max().unwrap());
                (t.in_point.unwrap(), t.out_point.unwrap(), lo, hi)
            })
            .collect()
    }

    #[test]
    fn pages_hand_over_without_showing_together() {
        let fmt = Format {
            width: 1080,
            height: 1920,
            fps: 30.0,
            duration: 30.0,
            motion_blur: None,
        };
        let text = "When speech runs on the next page comes in as the last one goes and never on top of it \
                    however the words were timed";
        // Words every `step` s lasting `len`: abutting, as speech-to-text
        // gives them, overlapping by 0.05 s, 0.02 s apart and 0.3 s apart.
        for (step, len) in [(0.3, 0.3), (0.3, 0.35), (0.3, 0.28), (0.5, 0.2)] {
            let words: String = text
                .split(' ')
                .enumerate()
                .map(|(i, w)| format!("{} {} {w}\\n", i as f64 * step, i as f64 * step + len))
                .collect();
            let pages = page_windows(&words, &fmt);
            assert!(pages.len() >= 3, "{pages:?}");
            let e = 1e-9;
            for (k, &(t_in, t_out, lo, hi)) in pages.iter().enumerate() {
                let (first, last) = (lo as f64 * step, hi as f64 * step + len);
                // In 0.04 s before its first word, or as the page before
                // goes if that is later; out after its last word and the
                // linger, or as the next comes in.
                assert!(
                    t_in <= first + e && t_in >= first - 0.04 - e,
                    "({step}, {len}) {k}: {pages:?}"
                );
                match pages.get(k + 1) {
                    Some(&(next_in, ..)) => {
                        assert!(t_out <= next_in + e, "({step}, {len}) {k}: {pages:?}");
                        // Shown while its words are spoken, up to the next
                        // page's first.
                        let next_first = (hi + 1) as f64 * step;
                        assert!(t_out >= last.min(next_first) - e, "({step}, {len}) {k}: {pages:?}");
                        // After a pause, as before: out after the linger or
                        // as the next comes in, 0.04 s before its first word.
                        if next_first - last >= 0.04 {
                            assert!(
                                (next_in - (next_first - 0.04)).abs() < e,
                                "({step}, {len}) {k}: {pages:?}"
                            );
                            assert!(
                                (t_out - (last + 0.6).min(next_in)).abs() < e,
                                "({step}, {len}) {k}: {pages:?}"
                            );
                        }
                    }
                    None => assert!((t_out - (last + 0.6)).abs() < e),
                }
            }
        }
    }
}
