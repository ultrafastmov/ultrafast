//! Inline styling inside a `text` param:
//!
//! - `*word*`: the emphasis style, set by the node's `em_*` params.
//! - `[words]{color=#ffd23f weight=800 font="Instrument Serif"}`: a span
//!   with its own attributes (color, font, weight, style, size, highlight,
//!   underline, rise).
//! - `\*`, `\[` and `\\` set the characters themselves. A `*` without a
//!   partner, or brackets without `{...}` after them, stay literal.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Style {
    Em,
    Attrs(BTreeMap<String, String>),
}

/// A styled byte range of the plain text.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// Whether `s` has anything markup would change.
pub fn has_markup(s: &str) -> bool {
    s.contains('*') || s.contains('[') || s.contains('\\')
}

fn attrs(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && (c[i].is_whitespace() || c[i] == ',' || c[i] == ';') {
            i += 1;
        }
        let k0 = i;
        while i < c.len() && c[i] != '=' && !c[i].is_whitespace() {
            i += 1;
        }
        let key: String = c[k0..i].iter().collect();
        if key.is_empty() {
            break;
        }
        if i >= c.len() || c[i] != '=' {
            out.insert(key, "true".into());
            continue;
        }
        i += 1;
        let val: String = if i < c.len() && (c[i] == '"' || c[i] == '\'') {
            let q = c[i];
            i += 1;
            let v0 = i;
            while i < c.len() && c[i] != q {
                i += 1;
            }
            let v = c[v0..i].iter().collect();
            i += 1;
            v
        } else {
            let v0 = i;
            while i < c.len() && !c[i].is_whitespace() && c[i] != ',' && c[i] != ';' {
                i += 1;
            }
            c[v0..i].iter().collect()
        };
        out.insert(key.trim().to_string(), val);
    }
    out
}

/// Index of the first `ch` at or after `from` that is not escaped.
fn unescaped(b: &[u8], from: usize, ch: u8) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == ch => return Some(j),
            _ => j += 1,
        }
    }
    None
}

/// Escapes `s` so markup sets it literally.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '[' | ']' | '\\' | '{') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The plain text and its styled spans.
pub fn parse(s: &str) -> (String, Vec<Span>) {
    let mut plain = String::with_capacity(s.len());
    let mut spans = Vec::new();
    let b = s.as_bytes();
    let mut em_open: Option<usize> = None;
    let mut i = 0;
    // Whether a later unescaped `*` closes one opened here.
    let closes = |from: usize| -> bool {
        let mut j = from;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'*' => return true,
                b'\n' => return false,
                _ => j += 1,
            }
        }
        false
    };
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() && matches!(b[i + 1], b'*' | b'[' | b']' | b'\\' | b'{') => {
                plain.push(b[i + 1] as char);
                i += 2;
            }
            b'*' => {
                match em_open.take() {
                    Some(start) => {
                        if plain.len() > start {
                            spans.push(Span {
                                start,
                                end: plain.len(),
                                style: Style::Em,
                            })
                        }
                    }
                    None if closes(i + 1) => em_open = Some(plain.len()),
                    None => plain.push('*'),
                }
                i += 1;
            }
            b'[' => {
                // `[text]{attrs}`: find the closing bracket (escapes
                // skipped), then braces.
                let close = unescaped(b, i + 1, b']');
                let brace = close.filter(|&c| b.get(c + 1) == Some(&b'{'));
                let end = brace.and_then(|c| s[c + 2..].find('}').map(|k| c + 2 + k));
                match (close, end) {
                    (Some(c), Some(e)) if !s[i + 1..c].contains('\n') => {
                        let (inner, inner_spans) = parse(&s[i + 1..c]);
                        let start = plain.len();
                        for sp in inner_spans {
                            spans.push(Span {
                                start: sp.start + start,
                                end: sp.end + start,
                                style: sp.style,
                            });
                        }
                        plain.push_str(&inner);
                        spans.push(Span {
                            start,
                            end: plain.len(),
                            style: Style::Attrs(attrs(&s[c + 2..e])),
                        });
                        i = e + 1;
                    }
                    _ => {
                        plain.push('[');
                        i += 1;
                    }
                }
            }
            _ => {
                let ch = s[i..].chars().next().unwrap();
                plain.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    (plain, spans)
}

/// The text a reader sees: markup removed.
pub fn plain(s: &str) -> String {
    if has_markup(s) { parse(s).0 } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_and_attribute_spans() {
        let (p, s) = parse("Make it *live* now");
        assert_eq!(p, "Make it live now");
        assert_eq!(
            s,
            vec![Span {
                start: 8,
                end: 12,
                style: Style::Em
            }]
        );
        let (p, s) = parse(r#"A [big idea]{color=#ff0 font="Instrument Serif" weight=800}."#);
        assert_eq!(p, "A big idea.");
        assert_eq!((s[0].start, s[0].end), (2, 10));
        let Style::Attrs(a) = &s[0].style else { panic!() };
        assert_eq!(a["color"], "#ff0");
        assert_eq!(a["font"], "Instrument Serif");
        assert_eq!(a["weight"], "800");
    }

    #[test]
    fn literals_stay() {
        assert_eq!(parse("5 * 3").0, "5 * 3");
        assert_eq!(parse("a [b] c").0, "a [b] c");
        assert_eq!(parse(r"\*not em\*").0, "*not em*");
        assert_eq!(parse("**").0, "");
        assert!(parse("*a*\n*b").1.len() == 1);
        assert_eq!(parse("*a*\n*b").0, "a\n*b");
    }

    #[test]
    fn escaped_brackets_inside_spans() {
        let src = format!("[{}]{{color=#f00}}x", escape("a[0]*b\\"));
        let (p, s) = parse(&src);
        assert_eq!(p, "a[0]*b\\x");
        assert_eq!((s[0].start, s[0].end), (0, 7));
    }

    #[test]
    fn nested_emphasis_inside_a_span() {
        let (p, s) = parse("[x *y* z]{underline=3}");
        assert_eq!(p, "x y z");
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end, &s[0].style), (2, 3, &Style::Em));
    }
}
