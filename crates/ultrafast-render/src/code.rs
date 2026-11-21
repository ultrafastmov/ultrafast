//! `code`: source code set in a panel, syntax-highlighted, optionally typed
//! on with a caret, with line numbers, highlighted lines and window chrome.
//! It expands into a component of ordinary nodes: the code is one `text`
//! node whose colors are inline spans, so it stays editable and sharp.

use crate::geom::{self, Fonts};
use crate::markup::escape;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use ultrafast_graph::{Component, Document, Format, Node, Param};

pub fn used_by(doc: &Document) -> bool {
    doc.nodes.values().any(|n| n.op == "code")
}

/// Params a code instance keeps (the rest are consumed at expansion).
pub const PARAMS: &[&str] = &["opacity"];

pub const LANGUAGES: &[&str] = &[
    "rust", "python", "js", "ts", "json", "bash", "go", "swift", "c", "cpp", "slang", "sql", "css", "html", "text",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Plain,
    Keyword,
    Type,
    Func,
    Str,
    Num,
    Comment,
    Punct,
}

struct Theme {
    background: &'static str,
    border: &'static str,
    chrome: &'static str,
    highlight: &'static str,
    gutter: &'static str,
    caret: &'static str,
    colors: [&'static str; 8],
}

/// Plain, keyword, type, function, string, number, comment, punctuation.
fn theme(name: &str) -> Option<Theme> {
    Some(match name {
        "ultrafast" => Theme {
            background: "#121117",
            border: "#ffffff14",
            chrome: "#1a1920",
            highlight: "#211f2a",
            gutter: "#8a849c",
            caret: "#ff5c7a",
            colors: [
                "#e8e3d8", "#ff7aa2", "#c49cff", "#7cc5ff", "#a8e07a", "#ffb86c", "#6e6880", "#9a94a6",
            ],
        },
        "paper" => Theme {
            background: "#f7f3ea",
            border: "#16141a1f",
            chrome: "#efe9dd",
            highlight: "#f0e2d4",
            gutter: "#8a8274",
            caret: "#e2412b",
            colors: [
                "#1c1a22", "#c8153f", "#7a3fc4", "#1f5fbf", "#2f7d32", "#b35c00", "#8a8478", "#6b6577",
            ],
        },
        "mono" => Theme {
            background: "#0b0b0d",
            border: "#ffffff1a",
            chrome: "#141417",
            highlight: "#18181d",
            gutter: "#77777f",
            caret: "#f2f2f2",
            colors: [
                "#d8d8dc", "#ffffff", "#ffffff", "#f2f2f2", "#a0a0a8", "#c8c8d0", "#66666e", "#8a8a92",
            ],
        },
        _ => return None,
    })
}

fn keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false",
            "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
            "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        "python" => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else",
            "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None",
            "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while", "with", "yield", "self",
        ],
        "js" | "ts" => &[
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "default",
            "delete",
            "do",
            "else",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "from",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "interface",
            "let",
            "new",
            "null",
            "of",
            "return",
            "static",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "type",
            "typeof",
            "undefined",
            "var",
            "void",
            "while",
            "yield",
            "as",
            "implements",
            "enum",
            "readonly",
        ],
        "go" => &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "fallthrough",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "type",
            "var",
            "nil",
            "true",
            "false",
        ],
        "swift" => &[
            "actor",
            "async",
            "await",
            "break",
            "case",
            "class",
            "continue",
            "default",
            "defer",
            "do",
            "else",
            "enum",
            "extension",
            "false",
            "for",
            "func",
            "guard",
            "if",
            "import",
            "in",
            "init",
            "let",
            "nil",
            "private",
            "protocol",
            "public",
            "return",
            "self",
            "static",
            "struct",
            "switch",
            "throw",
            "throws",
            "true",
            "try",
            "var",
            "where",
            "while",
        ],
        "c" | "cpp" | "slang" => &[
            "auto",
            "bool",
            "break",
            "case",
            "char",
            "class",
            "const",
            "constexpr",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extern",
            "false",
            "float",
            "float2",
            "float3",
            "float4",
            "for",
            "if",
            "inline",
            "int",
            "long",
            "namespace",
            "nullptr",
            "return",
            "short",
            "sizeof",
            "static",
            "struct",
            "switch",
            "template",
            "this",
            "true",
            "typedef",
            "uint",
            "unsigned",
            "using",
            "void",
            "while",
            "in",
            "out",
            "inout",
            "uniform",
        ],
        "sql" => &[
            "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set", "delete",
            "create", "table", "join", "left", "right", "inner", "on", "group", "by", "order", "limit", "as", "having",
            "null", "distinct", "count", "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "INSERT", "INTO", "VALUES",
            "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "JOIN", "LEFT", "ON", "GROUP", "BY", "ORDER", "LIMIT", "AS",
            "HAVING", "NULL", "DISTINCT", "COUNT",
        ],
        "bash" => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "function", "in",
            "export", "local", "return", "echo", "cd", "sudo",
        ],
        "json" => &["true", "false", "null"],
        _ => &[],
    }
}

fn line_comment(lang: &str) -> Option<&'static str> {
    match lang {
        "python" | "bash" => Some("#"),
        "sql" => Some("--"),
        "json" | "text" | "html" | "css" => None,
        _ => Some("//"),
    }
}

/// Tokens of `src`, as (kind, text) pieces covering it exactly.
fn tokenize(src: &str, lang: &str) -> Vec<(Tok, String)> {
    let kw = keywords(lang);
    let lc = line_comment(lang);
    let block = !matches!(lang, "python" | "bash" | "json" | "text");
    let c: Vec<char> = src.chars().collect();
    let mut out: Vec<(Tok, String)> = Vec::new();
    let push = |t: Tok, s: String, out: &mut Vec<(Tok, String)>| match out.last_mut() {
        Some(last) if last.0 == t => last.1.push_str(&s),
        _ => out.push((t, s)),
    };
    let starts = |i: usize, p: &str| p.chars().enumerate().all(|(k, pc)| c.get(i + k) == Some(&pc));
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if lang == "text" {
            push(Tok::Plain, ch.to_string(), &mut out);
            i += 1;
            continue;
        }
        // Comments.
        if let Some(p) = lc
            && starts(i, p)
        {
            let j = (i..c.len()).find(|&j| c[j] == '\n').unwrap_or(c.len());
            push(Tok::Comment, c[i..j].iter().collect(), &mut out);
            i = j;
            continue;
        }
        if block && starts(i, "/*") {
            let j = (i + 2..c.len().saturating_sub(1))
                .find(|&j| c[j] == '*' && c[j + 1] == '/')
                .map(|j| j + 2)
                .unwrap_or(c.len());
            push(Tok::Comment, c[i..j].iter().collect(), &mut out);
            i = j;
            continue;
        }
        if lang == "html" && starts(i, "<!--") {
            let j = (i + 4..c.len())
                .find(|&j| starts(j, "-->"))
                .map(|j| j + 3)
                .unwrap_or(c.len());
            push(Tok::Comment, c[i..j].iter().collect(), &mut out);
            i = j;
            continue;
        }
        // Strings.
        if ch == '"' || ch == '\'' || (ch == '`' && matches!(lang, "js" | "ts" | "go")) {
            // A Rust lifetime ('a) is not a string.
            let lifetime = lang == "rust"
                && ch == '\''
                && c.get(i + 1).is_some_and(|n| n.is_alphabetic())
                && c.get(i + 2) != Some(&'\'');
            if !lifetime {
                let mut j = i + 1;
                while j < c.len() && c[j] != ch && !(c[j] == '\n' && ch != '`') {
                    if c[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
                let j = (j + 1).min(c.len());
                // JSON keys read as names, not values.
                let key = lang == "json" && (j..c.len()).find(|&k| c[k] != ' ').map(|k| c[k]) == Some(':');
                push(
                    if key { Tok::Func } else { Tok::Str },
                    c[i..j].iter().collect(),
                    &mut out,
                );
                i = j;
                continue;
            }
        }
        // Numbers.
        if ch.is_ascii_digit() && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_')) {
            let j = (i..c.len())
                .find(|&j| !(c[j].is_ascii_alphanumeric() || c[j] == '.' || c[j] == '_'))
                .unwrap_or(c.len());
            push(Tok::Num, c[i..j].iter().collect(), &mut out);
            i = j;
            continue;
        }
        // Words.
        if ch.is_alphabetic() || ch == '_' || (ch == '$' && lang == "bash") || (ch == '@' && lang != "text") {
            let j = (i + 1..c.len())
                .find(|&j| !(c[j].is_alphanumeric() || c[j] == '_' || (lang == "css" && c[j] == '-')))
                .unwrap_or(c.len());
            let w: String = c[i..j].iter().collect();
            let next = (j..c.len()).find(|&k| c[k] != ' ').map(|k| c[k]);
            let t = if kw.contains(&w.as_str()) || (lang == "rust" && next == Some('!')) {
                Tok::Keyword
            } else if next == Some('(') {
                Tok::Func
            } else if w.chars().next().is_some_and(|f| f.is_uppercase()) || ch == '@' {
                Tok::Type
            } else {
                Tok::Plain
            };
            push(t, w, &mut out);
            i = j;
            continue;
        }
        let t = if ch.is_whitespace() { Tok::Plain } else { Tok::Punct };
        push(t, ch.to_string(), &mut out);
        i += 1;
    }
    out
}

fn text<'a>(node: &'a Node, k: &str) -> Option<&'a str> {
    match node.params.get(k) {
        Some(Param::Text(s)) => Some(s),
        _ => None,
    }
}

fn num(node: &Node, k: &str, d: f64) -> f64 {
    node.params
        .get(k)
        .and_then(|p| p.eval(0.0))
        .and_then(|v| v.first().copied())
        .unwrap_or(d)
}

fn flag(node: &Node, k: &str, d: bool) -> bool {
    match node.params.get(k) {
        Some(Param::Bool(b)) => *b,
        _ => d,
    }
}

/// "3", "3-5", "2,4-6" as 1-based line numbers.
fn line_set(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
            out.extend(a.min(b)..=a.max(b));
        }
    }
    out
}

/// The markup of `src` colored by `kinds`, one span per token (spans do not
/// cross lines).
fn markup(tokens: &[(Tok, String)], th: &Theme) -> String {
    let mut out = String::new();
    for (t, s) in tokens {
        for (k, piece) in s.split('\n').enumerate() {
            if k > 0 {
                out.push('\n');
            }
            if piece.is_empty() {
                continue;
            }
            if *t == Tok::Plain || piece.trim().is_empty() {
                out.push_str(&escape(piece));
                continue;
            }
            let color = th.colors[*t as usize];
            let style = if *t == Tok::Comment { " style=italic" } else { "" };
            out.push_str(&format!("[{}]{{color={color}{style}}}", escape(piece)));
        }
    }
    out
}

pub fn build(node: &Node, fmt: &Format, fonts: &mut Fonts, base: &Path) -> Result<Component, String> {
    let src = text(node, "code").unwrap_or("").replace('\t', "    ");
    let src = src.trim_end_matches('\n').to_string();
    let lang = text(node, "language").unwrap_or("text");
    if !LANGUAGES.contains(&lang) {
        return Err(format!("unknown language `{lang}` ({})", LANGUAGES.join(", ")));
    }
    let th_name = text(node, "theme").unwrap_or("ultrafast");
    let th = theme(th_name).ok_or_else(|| format!("unknown theme `{th_name}` (ultrafast, paper, mono)"))?;
    let s = fmt.width.min(fmt.height) as f64 / 1080.0;
    let size = num(node, "size", 30.0 * s);
    let lh = num(node, "line_height", 1.55) * size;
    let font = text(node, "font").unwrap_or("ultrafast:mono").to_string();
    let pad = num(node, "padding", size * 1.4);
    let window = flag(node, "window", true);
    let numbers = flag(node, "line_numbers", true);
    let lines: Vec<&str> = src.split('\n').collect();
    let n_lines = lines.len().max(1);

    // Monospace metrics: one advance for every character.
    let probe: Node = serde_json::from_value(json!({"op": "text", "params": {
        "text": "MMMMMMMMMM", "font": font, "size": size, "features": "liga=0,calt=0"}}))
    .map_err(|e| e.to_string())?;
    let m = geom::measure("code", &probe, 0.0, fonts, base).map_err(|e| e.to_string())?;
    let adv = m.advance / 10.0;
    let (ascent, descent) = (m.ascent, m.descent);
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let gutter = if numbers {
        (n_lines.to_string().len() as f64 + 2.0) * adv
    } else {
        0.0
    };
    let bar = if window { size * 1.9 } else { 0.0 };
    let width = num(node, "width", gutter + cols as f64 * adv + 2.0 * pad);
    let height = bar + 2.0 * pad + lh * (n_lines as f64 - 1.0) + ascent + descent;
    let pos = node.params.get("position").and_then(|p| p.eval(0.0));
    let (x, y) = match pos {
        Some(p) if p.len() >= 2 => (p[0], p[1]),
        _ => ((fmt.width as f64 - width) / 2.0, (fmt.height as f64 - height) / 2.0),
    };

    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut layers: Vec<String> = Vec::new();
    let mut add = |id: &str, op: &str, params: Value, inputs: &[(&str, &str)], layer: bool| -> Result<(), String> {
        nodes.insert(
            id.to_string(),
            Node {
                op: op.into(),
                params: serde_json::from_value(params).map_err(|e| e.to_string())?,
                inputs: inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                ..Default::default()
            },
        );
        if layer {
            layers.push(id.to_string());
        }
        Ok(())
    };

    let radius = num(node, "radius", size * 0.55);
    if flag(node, "background", true) {
        add(
            "shadow_shape",
            "shape",
            json!({"kind": "rect", "center": [x + width / 2.0, y + height / 2.0 + size * 0.8],
                   "size": [width, height], "radius": radius, "color": "#00000099"}),
            &[],
            false,
        )?;
        add(
            "shadow",
            "blur",
            json!({"radius": size * 1.6}),
            &[("source", "shadow_shape")],
            true,
        )?;
        add(
            "panel",
            "shape",
            json!({"kind": "rect", "center": [x + width / 2.0, y + height / 2.0], "size": [width, height],
                   "radius": radius, "color": th.background}),
            &[],
            true,
        )?;
        add(
            "border",
            "shape",
            json!({"kind": "rect", "center": [x + width / 2.0, y + height / 2.0], "size": [width - 1.0, height - 1.0],
                   "radius": radius, "stroke": 1.5 * s, "color": th.border}),
            &[],
            true,
        )?;
    }
    if window {
        // The title bar: rounded on top, square where it meets the code.
        let cy = y + bar / 2.0 + size * 0.15;
        let bh = bar + size * 0.3;
        add(
            "bar_top",
            "shape",
            json!({"kind": "rect", "center": [x + width / 2.0, y + bh / 2.0], "size": [width - 2.0, bh - 1.0],
                   "radius": radius, "color": th.chrome}),
            &[],
            true,
        )?;
        add(
            "bar_bottom",
            "shape",
            json!({"kind": "rect", "center": [x + width / 2.0, y + bh * 0.75], "size": [width - 2.0, bh * 0.5],
                   "color": th.chrome}),
            &[],
            true,
        )?;
        add(
            "bar_rule",
            "shape",
            json!({"kind": "rect", "center": [x + width / 2.0, y + bh], "size": [width - 2.0, 1.5 * s], "color": th.border}),
            &[],
            true,
        )?;
        for (k, c) in ["#ff5f57", "#febc2e", "#28c840"].iter().enumerate() {
            add(
                &format!("dot{k}"),
                "shape",
                json!({"kind": "circle", "center": [x + pad * 0.75 + k as f64 * size * 0.85, cy],
                       "size": [size * 0.5, size * 0.5], "color": c}),
                &[],
                true,
            )?;
        }
        if let Some(title) = text(node, "title") {
            add(
                "title",
                "text",
                json!({"text": escape(title), "font": font, "size": (size * 0.9).max(24.0 * s), "color": th.gutter,
                       "align": "center", "valign": "middle", "position": [x + width / 2.0, cy]}),
                &[],
                true,
            )?;
        }
    }
    let x0 = x + pad + gutter;
    let top = y + bar + pad;
    // Highlighted lines: a band across the panel behind them.
    if let Some(hl) = text(node, "highlight") {
        let at = num(node, "highlight_at", 0.0);
        for (k, line) in line_set(hl).into_iter().filter(|&l| l >= 1 && l <= n_lines).enumerate() {
            let cy = top + ascent + (line as f64 - 1.0) * lh - size * 0.3;
            let id = format!("highlight{k}");
            add(
                &id,
                "shape",
                json!({"kind": "rect", "center": [x + width / 2.0, cy], "size": [width - 2.0, lh], "color": th.highlight}),
                &[],
                false,
            )?;
            add(
                &format!("{id}_in"),
                "opacity",
                json!({"opacity": {"keys": [{"t": at, "v": [0.0], "ease": "out_cubic"}, {"t": at + 0.4, "v": [1.0]}]}}),
                &[("source", &id)],
                true,
            )?;
        }
    }
    if numbers {
        let nums: Vec<String> = (1..=n_lines).map(|i| i.to_string()).collect();
        add(
            "line_numbers",
            "text",
            json!({"text": nums.join("\n"), "font": font, "size": size, "line_height": lh / size,
                   "color": th.gutter, "align": "right", "position": [x0 - adv * 1.5, top],
                   "features": "liga=0,calt=0", "skim": true}),
            &[],
            true,
        )?;
    }
    let tokens = tokenize(&src, lang);
    let mut p = json!({"text": markup(&tokens, &th), "font": font, "size": size, "line_height": lh / size,
                       "color": th.colors[0], "position": [x0, top], "features": "liga=0,calt=0", "skim": true});
    // Typing: characters appear at `type_speed` per second from `at`.
    let speed = num(node, "type_speed", 0.0);
    let at = num(node, "at", 0.0);
    let glyphs: Vec<(usize, usize)> = lines
        .iter()
        .enumerate()
        .flat_map(|(row, l)| (0..l.chars().count()).map(move |col| (row, col)))
        .collect();
    let total = glyphs.len();
    if speed > 0.0 {
        let end = at + total as f64 / speed;
        p["reveal"] = json!({"keys": [{"t": at, "v": [0.0]}, {"t": end, "v": [total as f64]}]});
        p["reveal_softness"] = json!(0.6);
    }
    // Lines longer than a given `width` are clipped at the panel's edge.
    add("code_text", "text", p, &[], false)?;
    add(
        "code",
        "crop",
        json!({"rect": [x, y + bar, width, height - bar], "softness": 0.0}),
        &[("source", "code_text")],
        true,
    )?;
    if speed > 0.0 && flag(node, "caret", true) {
        // The caret after the last typed character, then blinking.
        let end = at + total as f64 / speed;
        let place = |row: usize, col: usize| {
            json!([
                x0 + col as f64 * adv + adv * 0.1,
                top + ascent + row as f64 * lh - size * 0.3
            ])
        };
        // The caret runs along each line as it is typed (one linear key per
        // line, not one per character), then jumps to the next.
        let mut keys: Vec<Value> = Vec::new();
        let mut k = 0usize;
        for (row, l) in lines.iter().enumerate() {
            let n = l.chars().count();
            if n == 0 && row > 0 {
                continue;
            }
            let t0 = at + k as f64 / speed;
            keys.push(json!({"t": t0, "v": place(row, 0), "ease": "linear"}));
            if n > 0 {
                keys.push(json!({"t": at + (k + n) as f64 / speed - 1e-3, "v": place(row, n), "ease": "hold"}));
            }
            k += n;
        }
        let blink = format!("1 - (t > {end:.4}) * (fract((t - {end:.4}) * 1.1) > 0.55)");
        add(
            "caret_shape",
            "shape",
            json!({"kind": "rect", "size": [adv * 0.14 + 2.0 * s, size * 1.15], "color": th.caret,
                   "center": {"keys": keys}}),
            &[],
            false,
        )?;
        add(
            "caret",
            "opacity",
            json!({"opacity": {"expr": blink}}),
            &[("source", "caret_shape")],
            true,
        )?;
    }

    // Composite in order, then the instance's opacity.
    let mut acc: Option<String> = None;
    for (k, l) in layers.iter().enumerate() {
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
    let art = acc.ok_or("empty code block")?;
    nodes.insert(
        "code_out".into(),
        Node {
            op: "opacity".into(),
            params: [("opacity".to_string(), Param::Text("$opacity".into()))].into(),
            inputs: [("source".to_string(), art)].into(),
            ..Default::default()
        },
    );
    Ok(Component {
        nodes,
        output: "code_out".into(),
        params: [("opacity".to_string(), Param::Number(1.0))].into(),
        inputs: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_rust() {
        let t = tokenize("fn main() { let x = \"hi\"; // done\n}", "rust");
        let kinds: Vec<(Tok, &str)> = t.iter().map(|(k, s)| (*k, s.as_str())).collect();
        assert!(kinds.contains(&(Tok::Keyword, "fn")));
        assert!(kinds.contains(&(Tok::Func, "main")));
        assert!(kinds.contains(&(Tok::Str, "\"hi\"")));
        assert!(kinds.contains(&(Tok::Comment, "// done")));
        // Tokens cover the source exactly.
        assert_eq!(
            t.iter().map(|(_, s)| s.as_str()).collect::<String>(),
            "fn main() { let x = \"hi\"; // done\n}"
        );
    }

    #[test]
    fn markup_round_trips_to_the_source() {
        let src = "let a = v[0] * b; /* [x] */\nprint(\"\\\\\")";
        let th = theme("ultrafast").unwrap();
        let (plain, spans) = crate::markup::parse(&markup(&tokenize(src, "rust"), &th));
        assert_eq!(plain, src);
        assert!(!spans.is_empty());
    }
}
