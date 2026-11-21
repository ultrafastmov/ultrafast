//! Expressions: a param computed from time, e.g.
//! `{"expr": "540 + 200 * sin(t * 3)"}` or, for vectors,
//! `{"expr": ["540 + 80 * noise(t)", "960"]}`. Parsed once when the
//! document loads (errors point at the expression), evaluated per frame.
//!
//! Variables: `t` (the node's local time, seconds), `pi`, `tau`, `e`.
//! Operators: `+ - * / %`, `^` (power), unary `-`, comparisons `< > <= >=`
//! (1 or 0), parentheses. Functions: sin cos tan asin acos atan atan2 abs
//! sign floor ceil round fract sqrt exp ln log10 pow min max clamp mix
//! smoothstep step saw tri square noise (smooth value noise, about -1..1)
//! hash (0..1) wiggle(freq, amp[, seed]).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq)]
enum Ast {
    Num(f64),
    Var(char),
    Neg(Box<Ast>),
    Bin(char, Box<Ast>, Box<Ast>),
    Call(String, Vec<Ast>),
}

/// One or more expressions (a vector param has one per component). A
/// malformed expression still loads (so the document opens); it evaluates
/// to 0 and `ultrafast check` reports it.
#[derive(Debug, Clone)]
pub struct Expr {
    src: Vec<String>,
    ast: Vec<Result<Ast, String>>,
    /// A single source string rather than a list.
    scalar: bool,
}

impl PartialEq for Expr {
    fn eq(&self, o: &Self) -> bool {
        self.src == o.src
    }
}

impl Serialize for Expr {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.scalar {
            self.src[0].serialize(s)
        } else {
            self.src.serialize(s)
        }
    }
}

impl<'de> Deserialize<'de> for Expr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum S {
            One(String),
            Many(Vec<String>),
        }
        let (src, scalar) = match S::deserialize(d)? {
            S::One(s) => (vec![s], true),
            S::Many(v) => (v, false),
        };
        let ast = src.iter().map(|s| parse(s)).collect();
        Ok(Expr { src, ast, scalar })
    }
}

impl Expr {
    pub fn eval(&self, t: f64) -> Vec<f64> {
        self.ast
            .iter()
            .map(|a| a.as_ref().map(|a| eval(a, t)).unwrap_or(0.0))
            .collect()
    }

    /// Problems with the expressions, one line each.
    pub fn errors(&self) -> Vec<String> {
        self.src
            .iter()
            .zip(&self.ast)
            .filter_map(|(s, a)| a.as_ref().err().map(|e| format!("expression `{s}`: {e}")))
            .collect()
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Result<Ast, String> {
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Ast, String> {
        let mut a = self.sum()?;
        loop {
            let op = match self.peek() {
                Some(b'<') if self.s.get(self.i + 1) == Some(&b'=') => 'l',
                Some(b'>') if self.s.get(self.i + 1) == Some(&b'=') => 'g',
                Some(b'<') => '<',
                Some(b'>') => '>',
                _ => return Ok(a),
            };
            self.i += if op == 'l' || op == 'g' { 2 } else { 1 };
            a = Ast::Bin(op, Box::new(a), Box::new(self.sum()?));
        }
    }
    fn sum(&mut self) -> Result<Ast, String> {
        let mut a = self.product()?;
        loop {
            let op = match self.peek() {
                Some(b'+') => '+',
                Some(b'-') => '-',
                _ => return Ok(a),
            };
            self.i += 1;
            a = Ast::Bin(op, Box::new(a), Box::new(self.product()?));
        }
    }
    fn product(&mut self) -> Result<Ast, String> {
        let mut a = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(b'*') => '*',
                Some(b'/') => '/',
                Some(b'%') => '%',
                _ => return Ok(a),
            };
            self.i += 1;
            a = Ast::Bin(op, Box::new(a), Box::new(self.unary()?));
        }
    }
    fn unary(&mut self) -> Result<Ast, String> {
        if self.eat(b'-') {
            return Ok(Ast::Neg(Box::new(self.unary()?)));
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }
    fn power(&mut self) -> Result<Ast, String> {
        let base = self.atom()?;
        if self.eat(b'^') {
            // Right-associative, binds tighter than unary minus on the left.
            return Ok(Ast::Bin('^', Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }
    fn atom(&mut self) -> Result<Ast, String> {
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                let e = self.expr()?;
                if !self.eat(b')') {
                    return Err(format!("expected `)` at {}", self.i));
                }
                Ok(e)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let start = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                    self.i += 1;
                }
                if self.i < self.s.len() && (self.s[self.i] == b'e' || self.s[self.i] == b'E') {
                    let save = self.i;
                    self.i += 1;
                    if self.i < self.s.len() && (self.s[self.i] == b'-' || self.s[self.i] == b'+') {
                        self.i += 1;
                    }
                    if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                            self.i += 1;
                        }
                    } else {
                        self.i = save;
                    }
                }
                let txt = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                txt.parse().map(Ast::Num).map_err(|_| format!("bad number `{txt}`"))
            }
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                    self.i += 1;
                }
                let name = std::str::from_utf8(&self.s[start..self.i]).unwrap().to_string();
                if self.eat(b'(') {
                    let mut args = Vec::new();
                    if !self.eat(b')') {
                        loop {
                            args.push(self.expr()?);
                            if self.eat(b')') {
                                break;
                            }
                            if !self.eat(b',') {
                                return Err(format!("expected `,` or `)` in {name}(...)"));
                            }
                        }
                    }
                    let (lo, hi) = arity(&name).ok_or_else(|| format!("unknown function `{name}`"))?;
                    if args.len() < lo || args.len() > hi {
                        return Err(format!("`{name}` takes {lo}..{hi} arguments, got {}", args.len()));
                    }
                    return Ok(Ast::Call(name, args));
                }
                match name.as_str() {
                    "t" => Ok(Ast::Var('t')),
                    "pi" => Ok(Ast::Num(std::f64::consts::PI)),
                    "tau" => Ok(Ast::Num(std::f64::consts::TAU)),
                    "e" => Ok(Ast::Num(std::f64::consts::E)),
                    _ => Err(format!("unknown name `{name}` (variables: t, pi, tau, e)")),
                }
            }
            Some(c) => Err(format!("unexpected `{}` at {}", c as char, self.i)),
            None => Err("unexpected end".into()),
        }
    }
}

fn arity(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "abs" | "sign" | "floor" | "ceil" | "round" | "fract"
        | "sqrt" | "exp" | "ln" | "log10" | "saw" | "tri" | "square" => (1, 1),
        "noise" | "hash" => (1, 2),
        "atan2" | "pow" | "step" => (2, 2),
        "min" | "max" => (2, 8),
        "clamp" | "mix" | "smoothstep" => (3, 3),
        "wiggle" => (2, 3),
        _ => return None,
    })
}

fn parse(s: &str) -> Result<Ast, String> {
    let mut p = P { s: s.as_bytes(), i: 0 };
    let e = p.expr()?;
    if p.peek().is_some() {
        return Err(format!("unexpected `{}` at {}", p.s[p.i] as char, p.i));
    }
    Ok(e)
}

fn hash(x: f64, seed: f64) -> f64 {
    let v = (x * 127.1 + seed * 311.7).sin() * 43758.5453;
    v - v.floor()
}

/// Smooth value noise in about -1..1.
fn noise(x: f64, seed: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash(i, seed) * 2.0 - 1.0;
    let b = hash(i + 1.0, seed) * 2.0 - 1.0;
    a + (b - a) * u
}

fn eval(a: &Ast, t: f64) -> f64 {
    match a {
        Ast::Num(v) => *v,
        Ast::Var(_) => t,
        Ast::Neg(x) => -eval(x, t),
        Ast::Bin(op, x, y) => {
            let (x, y) = (eval(x, t), eval(y, t));
            match op {
                '+' => x + y,
                '-' => x - y,
                '*' => x * y,
                '/' => x / y,
                '%' => x.rem_euclid(y),
                '^' => x.powf(y),
                '<' => (x < y) as u8 as f64,
                '>' => (x > y) as u8 as f64,
                'l' => (x <= y) as u8 as f64,
                _ => (x >= y) as u8 as f64,
            }
        }
        Ast::Call(f, args) => {
            let v: Vec<f64> = args.iter().map(|a| eval(a, t)).collect();
            let a0 = v[0];
            match f.as_str() {
                "sin" => a0.sin(),
                "cos" => a0.cos(),
                "tan" => a0.tan(),
                "asin" => a0.asin(),
                "acos" => a0.acos(),
                "atan" => a0.atan(),
                "atan2" => a0.atan2(v[1]),
                "abs" => a0.abs(),
                "sign" => a0.signum(),
                "floor" => a0.floor(),
                "ceil" => a0.ceil(),
                "round" => a0.round(),
                "fract" => a0 - a0.floor(),
                "sqrt" => a0.max(0.0).sqrt(),
                "exp" => a0.exp(),
                "ln" => a0.ln(),
                "log10" => a0.log10(),
                "pow" => a0.powf(v[1]),
                "min" => v.iter().cloned().fold(f64::INFINITY, f64::min),
                "max" => v.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                "clamp" => a0.clamp(v[1].min(v[2]), v[2].max(v[1])),
                "mix" => a0 + (v[1] - a0) * v[2],
                "smoothstep" => {
                    let u = ((v[2] - a0) / (v[1] - a0)).clamp(0.0, 1.0);
                    u * u * (3.0 - 2.0 * u)
                }
                "step" => (v[1] >= a0) as u8 as f64,
                "saw" => a0 - a0.floor(),
                "tri" => 1.0 - (2.0 * (a0 - a0.floor()) - 1.0).abs(),
                "square" => {
                    if a0 - a0.floor() < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                "noise" => noise(a0, v.get(1).copied().unwrap_or(0.0)),
                "hash" => hash(a0, v.get(1).copied().unwrap_or(0.0)),
                "wiggle" => {
                    let seed = v.get(2).copied().unwrap_or(0.0);
                    let x = t * a0;
                    v[1] * (noise(x, seed) * 0.7 + noise(x * 2.1, seed + 9.0) * 0.3)
                }
                _ => 0.0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str, t: f64) -> f64 {
        eval(&parse(s).unwrap(), t)
    }

    #[test]
    fn evaluates() {
        assert_eq!(e("1 + 2 * 3", 0.0), 7.0);
        assert_eq!(e("(1 + 2) * 3", 0.0), 9.0);
        assert_eq!(e("-2 ^ 2", 0.0), -4.0);
        assert_eq!(e("2 ^ 3 ^ 2", 0.0), 512.0);
        assert!((e("540 + 200 * sin(t * pi)", 0.5) - 740.0).abs() < 1e-9);
        assert_eq!(e("clamp(t, 0, 1)", 3.0), 1.0);
        assert_eq!(e("max(1, 5, 3)", 0.0), 5.0);
        assert_eq!(e("t > 2", 3.0), 1.0);
        assert_eq!(e("mix(10, 20, 0.25)", 0.0), 12.5);
        assert!(e("noise(t)", 1.3).abs() <= 1.0);
        assert_eq!(e("1e3", 0.0), 1000.0);
        assert!(parse("sin(").is_err());
        assert!(parse("foo(1)").is_err());
        assert!(parse("x + 1").is_err());
        assert!(parse("min(1)").is_err());
    }
}
