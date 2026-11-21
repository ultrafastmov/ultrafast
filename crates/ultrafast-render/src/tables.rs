//! Big constant tables in agent shader code, read from the params instead.
//!
//! On Vulkan a `static const float ENV[3863] = {...};` in a shader is an
//! array in every GPU thread's own memory (SPIR-V has no constant address
//! space for it). When NVIDIA's compiler does not move it to its constant
//! bank, every pixel fills its copy before reading from it: one 1080p frame
//! of a production film took more than two seconds and Windows reset the
//! GPU. So tables of at least [`MIN_FLOATS`] floats move into the node's
//! params, uploaded with its other params, and `ENV[i]` reads them through
//! a subscript. Their values are then no longer in the kernel, so editing
//! them compiles nothing.

use std::sync::Arc;

/// Tables smaller than this stay in the code, where the compiler keeps
/// them in registers or folds their reads.
pub const MIN_FLOATS: usize = 64;

/// The param offset the rewritten code reads its tables from. The code
/// that hosts it defines it ([`declare_base`]).
pub const BASE: &str = "ULTRAFAST_TABLES";

/// Declares [`BASE`]: the tables start at param `at`.
pub fn declare_base(at: usize) -> String {
    format!("static const uint {BASE} = {at};\n")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hoisted {
    /// The code with each moved table replaced by an accessor of the same
    /// name, on as many lines as the table took (errors keep their lines).
    pub code: String,
    /// The tables' values, one table after another.
    pub data: Arc<[f32]>,
}

/// Moves the big constant tables out of `code`; None when it has none.
pub fn hoist(code: &str) -> Option<Hoisted> {
    let toks = lex(code);
    let mut found: Vec<Table> = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < toks.len() {
        match toks[i].tok {
            Tok::Punct(b'{') => depth += 1,
            Tok::Punct(b'}') => depth -= 1,
            _ if depth == 0 => {
                if let Some(t) = table_at(&toks, i) {
                    i = t.last + 1;
                    found.push(t);
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found.retain(|t| t.values.len() >= MIN_FLOATS && only_indexed(&toks, t));
    if found.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(code.len());
    let mut data = Vec::new();
    let mut from = 0;
    for t in &found {
        let (start, end) = (toks[t.first].start, toks[t.last].end);
        out += &code[from..start];
        out += &accessor(t, data.len());
        out.extend(std::iter::repeat_n('\n', code[start..end].matches('\n').count()));
        data.extend_from_slice(&t.values);
        from = end;
    }
    out += &code[from..];
    Some(Hoisted {
        code: out,
        data: data.into(),
    })
}

/// A `static const` array declaration at the top level of the code.
struct Table {
    name: String,
    /// Scalar type of its elements and how many each has (float3: 3).
    scalar: &'static str,
    width: usize,
    count: usize,
    values: Vec<f32>,
    /// Its tokens, `static` to `;`.
    first: usize,
    last: usize,
}

const SCALARS: [&str; 4] = ["float", "half", "int", "uint"];

fn element_type(name: &str) -> Option<(&'static str, usize)> {
    let scalar = SCALARS.iter().find(|s| name.starts_with(**s))?;
    match &name[scalar.len()..] {
        "" => Some((scalar, 1)),
        w @ ("2" | "3" | "4") => Some((scalar, w.parse().unwrap())),
        _ => None,
    }
}

/// The table declared at token `i`, if one is: `static const T NAME[N] =
/// { numbers };`, N a number, a constant or left out, the numbers plain
/// literals (constructors like `float2(1, 2)` and nested braces are fine).
fn table_at(toks: &[Token], i: usize) -> Option<Table> {
    let ident = |k: usize| match toks.get(k)?.tok {
        Tok::Ident(s) => Some(s),
        _ => None,
    };
    let punct = |k: usize, c: u8| toks.get(k).is_some_and(|t| t.tok == Tok::Punct(c));
    let mut k = i;
    match (ident(k)?, ident(k + 1)?) {
        ("static", "const") | ("const", "static") => k += 2,
        _ => return None,
    }
    let (scalar, width) = element_type(ident(k)?)?;
    let name = ident(k + 1)?;
    k += 2;
    if !punct(k, b'[') {
        return None;
    }
    let size = match toks.get(k + 1)?.tok {
        Tok::Punct(b']') => {
            k += 2;
            None
        }
        Tok::Num(n) if punct(k + 2, b']') => {
            k += 3;
            Some(parse_int(n)?)
        }
        Tok::Ident(c) if punct(k + 2, b']') => {
            k += 3;
            constant(toks, c)
        }
        _ => return None,
    };
    if !punct(k, b'=') || !punct(k + 1, b'{') {
        return None;
    }
    k += 1;
    // The initializer: numbers, commas, signs, braces, constructors.
    let mut values = Vec::new();
    let mut nest: Vec<u8> = Vec::new();
    let mut sign = 1.0f64;
    loop {
        let t = toks.get(k)?;
        match t.tok {
            Tok::Punct(c @ (b'{' | b'(')) => nest.push(c),
            Tok::Punct(b'}') => {
                if nest.pop()? != b'{' {
                    return None;
                }
                if nest.is_empty() {
                    break;
                }
            }
            Tok::Punct(b')') => {
                if nest.pop()? != b'(' {
                    return None;
                }
            }
            Tok::Punct(b',') => {}
            // Signs, only on numbers.
            Tok::Punct(c @ (b'-' | b'+')) => {
                if !matches!(toks.get(k + 1)?.tok, Tok::Num(_) | Tok::Punct(b'-' | b'+')) {
                    return None;
                }
                if c == b'-' {
                    sign = -sign;
                }
                k += 1;
                continue;
            }
            // A constructor: `float2(`.
            Tok::Ident(s) if element_type(s).is_some() && punct(k + 1, b'(') => {}
            Tok::Num(n) => values.push(number(n, scalar, sign)?),
            _ => return None,
        }
        sign = 1.0;
        k += 1;
    }
    if !punct(k + 1, b';') {
        return None;
    }
    let count = size.unwrap_or(values.len() / width);
    if count == 0 || values.len() != count * width {
        return None;
    }
    Some(Table {
        name: name.to_string(),
        scalar,
        width,
        count,
        values,
        first: i,
        last: k + 1,
    })
}

/// A literal as the table's scalar type holds it: floats as f32 (rounded
/// once, as the shader compiler would), integers exactly or not at all.
fn number(lit: &str, scalar: &str, sign: f64) -> Option<f32> {
    if scalar == "float" || scalar == "half" {
        if lit.starts_with("0x") || lit.starts_with("0X") {
            return None;
        }
        let v: f32 = lit.trim_end_matches(['f', 'F', 'h', 'H', 'l', 'L']).parse().ok()?;
        return Some(if sign < 0.0 { -v } else { v });
    }
    let v = parse_int(lit)? as f64 * sign;
    // Every integer up to 2^24 is an f32 exactly.
    (v.abs() <= (1u32 << 24) as f64 && (scalar == "int" || v >= 0.0)).then_some(v as f32)
}

fn parse_int(lit: &str) -> Option<usize> {
    let s = lit.trim_end_matches(['u', 'U', 'l', 'L']);
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// The value of `static const int NAME = 123;` at the top level.
fn constant(toks: &[Token], name: &str) -> Option<usize> {
    toks.windows(7).find_map(|w| {
        let is = |k: usize, s: &str| w[k].tok == Tok::Ident(s);
        let ok = is(0, "static")
            && is(1, "const")
            && (is(2, "int") || is(2, "uint"))
            && is(3, name)
            && w[4].tok == Tok::Punct(b'=')
            && w[6].tok == Tok::Punct(b';');
        match w[5].tok {
            Tok::Num(n) if ok => parse_int(n),
            _ => None,
        }
    })
}

/// Whether every use of the table outside its declaration indexes it inside
/// a function. A table passed whole to a function, a name used for
/// something else, or a global initialized from the table (which must be a
/// compile-time constant) keeps the table in the code.
fn only_indexed(toks: &[Token], t: &Table) -> bool {
    // Open braces, true for an initializer list (`= {`).
    let mut open: Vec<bool> = Vec::new();
    for (k, tok) in toks.iter().enumerate() {
        match tok.tok {
            Tok::Punct(b'{') => open.push(k > 0 && toks[k - 1].tok == Tok::Punct(b'=')),
            Tok::Punct(b'}') => {
                open.pop();
            }
            Tok::Ident(name) if name == t.name && !(t.first..=t.last).contains(&k) => {
                let indexed = toks.get(k + 1).is_some_and(|n| n.tok == Tok::Punct(b'['));
                if !indexed || open.first() != Some(&false) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// A struct with the table's name whose subscript reads its values from
/// the params, starting at float `at` of the node's tables.
fn accessor(t: &Table, at: usize) -> String {
    let (name, s, w, last) = (&t.name, t.scalar, t.width, t.count - 1);
    let read = |k: usize| {
        let p = if k == 0 {
            "P[b]".to_string()
        } else {
            format!("P[b + {k}u]")
        };
        if s == "float" { p } else { format!("{s}({p})") }
    };
    let (ty, value) = if w == 1 {
        (s.to_string(), read(0))
    } else {
        (
            format!("{s}{w}"),
            format!("{s}{w}({})", (0..w).map(read).collect::<Vec<_>>().join(", ")),
        )
    };
    format!(
        "struct UltrafastTable_{name} {{ __subscript(int i) -> {ty} {{ get {{ \
         uint b = {BASE} + {at}u + {w}u * uint(clamp(i, 0, {last})); return {value}; }} }} }}; \
         static const UltrafastTable_{name} {name} = {{}};"
    )
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok<'a> {
    Ident(&'a str),
    Num(&'a str),
    Punct(u8),
}

struct Token<'a> {
    tok: Tok<'a>,
    start: usize,
    end: usize,
}

/// Identifiers, numbers and punctuation, without comments, preprocessor
/// lines and string literals.
fn lex(code: &str) -> Vec<Token<'_>> {
    let b = code.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    let line_end = |i: usize| b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p);
    // Nothing but whitespace yet on this line (a `#` there starts a
    // preprocessor line).
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c.is_ascii_whitespace() {
            line_start |= c == b'\n';
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") || (c == b'#' && line_start) {
            i = line_end(i);
        } else if b[i..].starts_with(b"/*") {
            i = code[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
        } else if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            toks.push(Token {
                tok: Tok::Punct(b'"'),
                start,
                end: i,
            });
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(Token {
                tok: Tok::Ident(&code[start..i]),
                start,
                end: i,
            });
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let hex = b[i..].starts_with(b"0x") || b[i..].starts_with(b"0X");
            i += 1;
            while i < b.len() {
                let d = b[i];
                let exp_sign = !hex && (d == b'+' || d == b'-') && matches!(b[i - 1], b'e' | b'E');
                if d.is_ascii_alphanumeric() || d == b'.' || exp_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            toks.push(Token {
                tok: Tok::Num(&code[start..i]),
                start,
                end: i,
            });
        } else {
            // One token per byte; a multi-byte character outside a comment
            // is only ever punctuation here, never part of a table.
            i += code[i..].chars().next().map_or(1, char::len_utf8);
            toks.push(Token {
                tok: Tok::Punct(c),
                start,
                end: i,
            });
        }
        line_start = false;
    }
    toks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(n: usize) -> String {
        (0..n)
            .map(|i| format!("{:.3}", i as f32 / 8.0))
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[test]
    fn moves_a_big_table_and_keeps_lines() {
        let code = format!(
            "static const int ENV_N = 100;\n// The envelope.\nstatic const float ENV[100] = {{\n  {}\n}};\nstatic const float SEG[3] = {{1.0, 2.0, 3.0}};\nfloat4 shade(float2 pos, float px) {{\n  int i = int(pos.x);\n  return float4(ENV[i] + SEG[1], ENV [ min(i, ENV_N - 1) ], 0.0, 1.0);\n}}\n",
            floats(100)
        );
        let h = hoist(&code).expect("hoisted");
        assert_eq!(h.data.len(), 100);
        assert_eq!(h.data[9], 9.0 / 8.0);
        assert_eq!(h.code.lines().count(), code.lines().count());
        assert!(h.code.contains("static const UltrafastTable_ENV ENV = {};"));
        assert!(h.code.contains("uint(clamp(i, 0, 99))"));
        // Small tables stay, and so does everything after the big one.
        assert!(h.code.contains("static const float SEG[3] = {1.0, 2.0, 3.0};"));
        assert!(h.code.contains("ENV [ min(i, ENV_N - 1) ]"));
        assert!(!h.code.contains("12.375"));
    }

    #[test]
    fn vectors_signs_comments_and_sizes() {
        let pts: Vec<String> = (0..40).map(|i| format!("float2(-{i}.5, +{i}e1) /* p{i} */")).collect();
        let code = format!(
            "static const uint N = 40;\nstatic const float2 PTS[N] = {{ {} }};\nconst static int IDX[] = {{ {} }};\nfloat4 shade(float2 p, float px) {{ return float4(PTS[0], float(IDX[3]), 1.0); }}",
            pts.join(",\n"),
            (0..70).map(|i| format!("-{i}")).collect::<Vec<_>>().join(", ")
        );
        let h = hoist(&code).expect("hoisted");
        assert_eq!(h.data.len(), 80 + 70);
        assert_eq!(&h.data[..4], &[-0.5, 0.0, -1.5, 10.0]);
        assert_eq!(h.data[80 + 3], -3.0);
        assert!(h.code.contains("-> float2"));
        assert!(h.code.contains("float2(P[b], P[b + 1u])"));
        assert!(h.code.contains("ULTRAFAST_TABLES + 80u + 1u * uint(clamp(i, 0, 69))"));
        assert!(h.code.contains("-> int { get"));
        assert_eq!(h.code.lines().count(), code.lines().count());
    }

    #[test]
    fn leaves_what_it_cannot_read() {
        let big = floats(100);
        for code in [
            // Used whole, not indexed.
            format!(
                "static const float T[100] = {{{big}}};\nfloat f(float t[100]) {{ return t[0]; }}\nfloat g() {{ return f(T); }}"
            ),
            // An expression in the initializer.
            format!(
                "static const float PI2 = 6.28;\nstatic const float T[101] = {{PI2, {big}}};\nfloat g(int i) {{ return T[i]; }}"
            ),
            // Inside a function: the compiler's to place.
            format!("float g(int i) {{ static const float T[100] = {{{big}}}; return T[i]; }}"),
            // A global initialized from it.
            format!(
                "static const float T[100] = {{{big}}};\nstatic const float FIRST = T[0];\nfloat g(int i) {{ return T[i] + FIRST; }}"
            ),
            format!(
                "static const float T[100] = {{{big}}};\nstatic const float2 AB[2] = {{ float2(T[0], 1.0), float2(T[1], 2.0) }};\nfloat g(int i) {{ return T[i]; }}"
            ),
            // Size and values disagree.
            format!("static const float T[99] = {{{big}}};\nfloat g(int i) {{ return T[i]; }}"),
            // Too small.
            "static const float T[8] = {0, 1, 2, 3, 4, 5, 6, 7};\nfloat g(int i) { return T[i]; }".to_string(),
            // An integer an f32 cannot hold exactly.
            format!(
                "static const uint T[65] = {{4294967295u, {}}};\nuint g(int i) {{ return T[i]; }}",
                vec!["1"; 64].join(", ")
            ),
        ] {
            assert_eq!(hoist(&code), None, "{code}");
        }
    }
}
