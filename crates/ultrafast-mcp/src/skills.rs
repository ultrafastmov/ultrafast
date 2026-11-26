//! The ultrafast skill over MCP, through the `io.modelcontextprotocol/skills`
//! extension, so a host that loads skills from MCP servers gets it with the
//! tools, with nothing else to install. The skill is the repository's
//! `skills/ultrafast`, copied into this crate by `tools/sync-plugin-skill.sh`
//! (a published crate can't reach files outside its folder) and built in, so
//! it always describes the binary that serves it.
//!
//! - Each file is a resource, `skill://ultrafast/<path>`, listed by
//!   `resources/list` and read with `resources/read`. Clients without the
//!   extension can read it that way too: the server's instructions end with
//!   [`POINTER`].
//! - `skills/list` and `skills/get` return the skill's entry: its
//!   frontmatter as JSON and every file's SHA-256 digest and size, which
//!   hosts check what they read against.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// The extension's identifier, declared under `capabilities.extensions`.
pub const EXTENSION: &str = "io.modelcontextprotocol/skills";

/// The URI of the skill's `SKILL.md`, which names the skill in `skills/get`.
pub const URI: &str = "skill://ultrafast/SKILL.md";

/// The sentence the server's instructions end with while it serves the
/// skill.
pub const POINTER: &str = "If the ultrafast skill isn't loaded already, read it from this server before your first document: the resource skill://ultrafast/SKILL.md, with the files it links to beside it.";

/// The variable that turns the skill off (`0`, `off`, `false` or `no`), for
/// a plugin that ships the skill itself.
pub const ENV: &str = "ULTRAFAST_MCP_SKILL";

/// The skill's files: the path inside the skill, and the contents.
const FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../skills/ultrafast/SKILL.md")),
    (
        "references/document.md",
        include_str!("../skills/ultrafast/references/document.md"),
    ),
    (
        "references/recipes.md",
        include_str!("../skills/ultrafast/references/recipes.md"),
    ),
    (
        "references/setup.md",
        include_str!("../skills/ultrafast/references/setup.md"),
    ),
    (
        "examples/explainer-chart.json",
        include_str!("../skills/ultrafast/examples/explainer-chart.json"),
    ),
    (
        "examples/vertical-promo.json",
        include_str!("../skills/ultrafast/examples/vertical-promo.json"),
    ),
];

/// Whether [`ENV`] leaves the skill on: unset, or anything but `0`, `off`,
/// `false` and `no`.
pub fn wanted() -> bool {
    wanted_by(std::env::var(ENV).ok().as_deref())
}

fn wanted_by(value: Option<&str>) -> bool {
    value.is_none_or(|v| {
        !["0", "off", "false", "no"]
            .iter()
            .any(|o| v.trim().eq_ignore_ascii_case(o))
    })
}

struct Skill {
    /// The `skills/list` and `skills/get` entry.
    entry: Value,
    /// The files as `resources/list` entries.
    resources: Vec<Value>,
}

/// The skill, read once. `None` if its frontmatter can't be served (the
/// tests make sure it can).
fn skill() -> Option<&'static Skill> {
    static SKILL: OnceLock<Option<Skill>> = OnceLock::new();
    SKILL
        .get_or_init(|| match build() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("ultrafast: not serving the skill over MCP: {e}");
                None
            }
        })
        .as_ref()
}

/// The `skills/list` entries: the one skill.
pub fn list() -> Vec<Value> {
    skill().map(|s| s.entry.clone()).into_iter().collect()
}

/// The entry for the skill whose `SKILL.md` is at `uri`.
pub fn get(uri: &str) -> Option<Value> {
    skill().filter(|_| uri == URI).map(|s| s.entry.clone())
}

/// The skill's files as `resources/list` entries.
pub fn resources() -> Vec<Value> {
    skill().map(|s| s.resources.clone()).unwrap_or_default()
}

/// The `resources/read` contents of the file at `uri`.
pub fn read(uri: &str) -> Option<Value> {
    skill()?;
    let path = uri.strip_prefix("skill://ultrafast/")?;
    let (path, text) = FILES.iter().find(|(p, _)| *p == path)?;
    Some(json!({ "uri": uri, "mimeType": mime(path), "text": text }))
}

fn uri(path: &str) -> String {
    format!("skill://ultrafast/{path}")
}

fn mime(path: &str) -> &'static str {
    if path.ends_with(".md") {
        "text/markdown"
    } else if path.ends_with(".json") {
        "application/json"
    } else {
        "text/plain"
    }
}

fn sha256(text: &str) -> String {
    let hex: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn build() -> Result<Skill, String> {
    let fm = frontmatter(FILES[0].1)?;
    let text = |k: &str| fm.get(k).and_then(Value::as_str).map(str::to_string);
    // The URI's last segment is the skill's name.
    let name = text("name").ok_or("SKILL.md has no `name`")?;
    if name != "ultrafast" {
        return Err(format!("SKILL.md names `{name}`, but it is served as `ultrafast`"));
    }
    let description = text("description").ok_or("SKILL.md has no `description`")?;
    let files: Vec<Value> = FILES
        .iter()
        .map(|(path, text)| json!({ "uri": uri(path), "digest": sha256(text), "size": text.len() }))
        .collect();
    let resources = FILES
        .iter()
        .map(|(path, text)| {
            let mut r = json!({ "uri": uri(path), "name": path, "mimeType": mime(path), "size": text.len() });
            if *path == "SKILL.md" {
                r["name"] = json!(name);
                r["title"] = json!("The ultrafast skill");
                r["description"] = json!(description);
            }
            r
        })
        .collect();
    Ok(Skill {
        entry: json!({ "uri": URI, "frontmatter": fm, "resources": files }),
        resources,
    })
}

/// A `SKILL.md`'s YAML frontmatter as JSON, which hosts compare with the
/// frontmatter they parse. Reads the YAML a skill's frontmatter needs and
/// refuses the rest: `key: value` lines and one level of nested keys
/// (`metadata:`), each value plain, 'single-' or "double-quoted" on its
/// line, and `#` comments. A plain value YAML reads as something other than
/// text (a number, a date, `true`, `null`, ...) is refused too, so every
/// value is text that any YAML parser reads the same way.
fn frontmatter(md: &str) -> Result<Map<String, Value>, String> {
    let mut lines = md.lines().enumerate();
    if lines.next().map(|(_, l)| l.trim_end()) != Some("---") {
        return Err("the file doesn't start with a `---` line".into());
    }
    let mut out = Map::new();
    // The key whose nested keys are being read, and their indent.
    let mut nested: Option<(String, Option<usize>)> = None;
    for (i, line) in lines {
        let at = |e: String| format!("line {}: {e}", i + 1);
        let body = line.trim_start_matches(' ');
        let indent = line.len() - body.len();
        let body = body.trim_end();
        if body == "---" && indent == 0 {
            if let Some((key, None)) = nested {
                return Err(at(format!("`{key}:` has nothing under it")));
            }
            return Ok(out);
        }
        if body.starts_with('\t') {
            return Err(at("a tab in the indent".into()));
        }
        if body.chars().any(char::is_control) {
            return Err(at(
                "a control character (a tab, say): write it as an escape in double quotes".into(),
            ));
        }
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let (key, rest) = key_value(body).map_err(at)?;
        if indent == 0 {
            if let Some((parent, None)) = &nested {
                return Err(at(format!("`{parent}:` has nothing under it")));
            }
            nested = None;
            if out.contains_key(key) {
                return Err(at(format!("`{key}` twice")));
            }
            if rest.is_empty() {
                out.insert(key.to_string(), Value::Object(Map::new()));
                nested = Some((key.to_string(), None));
            } else {
                out.insert(key.to_string(), scalar(rest).map_err(at)?);
            }
            continue;
        }
        let Some((parent, want)) = nested.as_mut() else {
            return Err(at("an indented line that isn't under a key".into()));
        };
        if *want.get_or_insert(indent) != indent {
            return Err(at("indented unlike the line before".into()));
        }
        if rest.is_empty() {
            return Err(at("keys nest one level deep at most".into()));
        }
        let map = out.get_mut(parent.as_str()).and_then(Value::as_object_mut).unwrap();
        if map.contains_key(key) {
            return Err(at(format!("`{key}` twice under `{parent}`")));
        }
        map.insert(key.to_string(), scalar(rest).map_err(at)?);
    }
    Err("the frontmatter has no closing `---` line".into())
}

/// A `key: value` line's key and value (empty for `key:`, whose keys
/// follow, indented).
fn key_value(body: &str) -> Result<(&str, &str), String> {
    let Some((key, rest)) = body.split_once(':') else {
        return Err(format!("`{body}` isn't `key: value`"));
    };
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(format!("`{key}` isn't a plain key"));
    }
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return Err(format!("`{key}:` needs a space after the colon"));
    }
    let rest = rest.trim_start();
    Ok((key, if rest.starts_with('#') { "" } else { rest }))
}

fn scalar(s: &str) -> Result<Value, String> {
    let (text, after) = match s.as_bytes()[0] {
        b'"' => double_quoted(s)?,
        b'\'' => single_quoted(s)?,
        _ => return plain(s).map(Value::String),
    };
    // After the closing quote: nothing, or a comment.
    let trimmed = after.trim_start();
    if !(trimmed.is_empty() || (trimmed.starts_with('#') && trimmed.len() < after.len())) {
        return Err(format!("`{after}` after a quoted value"));
    }
    Ok(Value::String(text))
}

/// A "double-quoted" value and what follows it. Escapes are JSON's, which
/// mean the same in YAML.
fn double_quoted(s: &str) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = s.char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &s[i + 1..])),
            '\\' => {
                let (_, e) = chars.next().ok_or("a value ends in `\\`")?;
                out.push(match e {
                    '"' | '\\' | '/' => e,
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'u' => {
                        let hex: String = (0..4).filter_map(|_| chars.next().map(|(_, h)| h)).collect();
                        u32::from_str_radix(&hex, 16)
                            .ok()
                            .filter(|_| hex.len() == 4)
                            .and_then(char::from_u32)
                            .ok_or(format!("`\\u{hex}` isn't a character"))?
                    }
                    _ => return Err(format!("`\\{e}`: use JSON's escapes in a double-quoted value")),
                });
            }
            _ => out.push(c),
        }
    }
    Err("a double-quoted value doesn't end on its line".into())
}

/// A 'single-quoted' value (`''` is a quote) and what follows it.
fn single_quoted(s: &str) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = s.char_indices().skip(1).peekable();
    while let Some((i, c)) = chars.next() {
        if c != '\'' {
            out.push(c);
        } else if chars.peek().is_some_and(|(_, n)| *n == '\'') {
            chars.next();
            out.push('\'');
        } else {
            return Ok((out, &s[i + 1..]));
        }
    }
    Err("a single-quoted value doesn't end on its line".into())
}

/// A plain (unquoted) value, up to a comment.
fn plain(s: &str) -> Result<String, String> {
    let mut cs = s.chars();
    let (first, second) = (cs.next().unwrap_or(' '), cs.next());
    if ",[]{}#&*!|>%@`".contains(first) || ("-?:".contains(first) && second.is_none_or(|c| c == ' ')) {
        return Err(format!("`{s}` starts with a YAML indicator: quote it"));
    }
    // A `#` after a space starts a comment.
    let end = s
        .char_indices()
        .find(|&(i, c)| c == '#' && s[..i].ends_with(' '))
        .map_or(s.len(), |(i, _)| i);
    let v = s[..end].trim_end();
    if v.contains(": ") || v.ends_with(':') {
        return Err(format!("`{v}` holds `: `: quote it"));
    }
    if typed(v) {
        return Err(format!(
            "YAML reads `{v}` as a number, date, boolean or null, not text: quote it"
        ));
    }
    Ok(v.to_string())
}

/// Whether some YAML parser reads the plain value `v` as something other
/// than text: null, a boolean (YAML 1.1's yes, no, on and off included), a
/// number, a date or a time. Errs toward yes (`3D` is text, but is refused).
fn typed(v: &str) -> bool {
    const WORDS: &[&str] = &[
        "null", "~", "true", "false", "yes", "no", "on", "off", "y", "n", "=", "<<",
    ];
    if WORDS.iter().any(|w| v.eq_ignore_ascii_case(w)) {
        return true;
    }
    let unsigned = v.strip_prefix(['+', '-']).unwrap_or(v);
    unsigned.starts_with(|c: char| c.is_ascii_digit() || c == '.') && (!v.contains(' ') || v.contains(':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn files_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out.sort();
        out
    }

    fn here() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/ultrafast")
    }

    #[test]
    fn the_skill_is_served_and_describes_this_build() {
        let fm = frontmatter(FILES[0].1).unwrap();
        assert_eq!(fm["name"], "ultrafast");
        assert!(fm["description"].as_str().unwrap().len() > 100);
        assert_eq!(fm["license"], "Apache-2.0 OR MIT");
        // packaging/version.py writes the release's version here.
        assert_eq!(fm["metadata"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(skill().is_some());
        let entry = &list()[0];
        assert_eq!(entry["uri"], URI);
        assert_eq!(entry["frontmatter"], Value::Object(fm));
        assert_eq!(get(URI).as_ref(), Some(entry));
        assert_eq!(get("skill://ultrafast/references/setup.md"), None);
        assert_eq!(get("skill://other/SKILL.md"), None);
    }

    #[test]
    fn every_file_is_listed_with_its_digest_and_size() {
        // Every file in the crate's copy is built in, and nothing else.
        let mut built: Vec<String> = FILES.iter().map(|(p, _)| p.to_string()).collect();
        built.sort();
        assert_eq!(built, files_under(&here()));
        let entry = &list()[0];
        let listed = entry["resources"].as_array().unwrap();
        assert_eq!(listed.len(), FILES.len());
        assert!(FILES.len() <= 512 && FILES.iter().map(|(_, t)| t.len()).sum::<usize>() <= 16 << 20);
        for (r, (path, text)) in listed.iter().zip(FILES) {
            let bytes = std::fs::read(here().join(path)).unwrap();
            assert_eq!(r["uri"], format!("skill://ultrafast/{path}"));
            assert_eq!(r["size"], bytes.len());
            let digest = r["digest"].as_str().unwrap();
            assert_eq!(digest.len(), "sha256:".len() + 64);
            assert_eq!(digest, sha256(std::str::from_utf8(&bytes).unwrap()));
            assert_eq!(read(r["uri"].as_str().unwrap()).unwrap()["text"], *text);
        }
        // The known digest of a short text, against a slip in the hashing.
        assert_eq!(
            sha256("abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let res = resources();
        assert_eq!(res.len(), FILES.len());
        assert_eq!(res[0]["name"], "ultrafast");
        assert_eq!(res[0]["mimeType"], "text/markdown");
        assert_eq!(res[0]["description"], entry["frontmatter"]["description"]);
        assert!(res.iter().any(|r| r["mimeType"] == "application/json"));
        assert_eq!(read("skill://ultrafast/nope.md"), None);
        assert_eq!(read("skill://ultrafast/"), None);
        assert_eq!(read("file:///etc/passwd"), None);
    }

    /// The crate's copy is the repository's skill (when built in the
    /// repository; `tools/sync-plugin-skill.sh` copies it).
    #[test]
    fn the_copy_is_the_repository_skill() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ultrafast");
        if !repo.is_dir() {
            return;
        }
        assert_eq!(
            files_under(&repo),
            files_under(&here()),
            "run tools/sync-plugin-skill.sh"
        );
        for f in files_under(&repo) {
            assert!(
                std::fs::read(repo.join(&f)).unwrap() == std::fs::read(here().join(&f)).unwrap(),
                "{f} differs: run tools/sync-plugin-skill.sh"
            );
        }
    }

    #[test]
    fn frontmatter_reads_the_yaml_skills_use() {
        let fm = frontmatter(
            "---\r\nname: a-skill   # its name\r\ndescription: Does things: well, \"mostly\" (see #3).\r\n\
             license: 'It''s MIT'\r\n# a comment\r\n\r\nmetadata:  # nested\r\n  version: \"0.2.2\"\r\n\
             \x20 url: https://example.com/a#b\r\n  note: \"tab\\tand \\u00e9\"\r\nother: x\r\n---\r\n# Body\r\n",
        );
        // `: ` inside a plain value is refused (YAML can't read it).
        assert!(fm.unwrap_err().contains("line 3"));
        let fm = frontmatter(
            "---\nname: a-skill   # its name\ndescription: Does things, \"mostly\" (C#, F#).\n\
             license: 'It''s MIT'\n# a comment\n\nmetadata:  # nested\n  version: \"0.2.2\"\n\
             \x20 url: https://example.com/a#b\n  note: \"tab\\tand \\u00e9\"\nother: x\n---\n# Body\n",
        )
        .unwrap();
        assert_eq!(
            Value::Object(fm),
            json!({
                "name": "a-skill",
                "description": "Does things, \"mostly\" (C#, F#).",
                "license": "It's MIT",
                "metadata": { "version": "0.2.2", "url": "https://example.com/a#b", "note": "tab\tand é" },
                "other": "x",
            })
        );
        // A comment cuts a plain value, as in YAML.
        let fm = frontmatter("---\nname: x #y\ndescription: see #3\n---\n").unwrap();
        assert_eq!((&fm["name"], &fm["description"]), (&json!("x"), &json!("see")));
    }

    #[test]
    fn frontmatter_refuses_what_it_cannot_read_exactly() {
        let refused = [
            "name: x\n---\n",                  // no opening line
            "---\nname: x\n",                  // no closing line
            "---\nversion: 1.0\n---\n",        // a number
            "---\nversion: 2026-11-26\n---\n", // a date
            "---\nlicensed: yes\n---\n",       // YAML 1.1's boolean
            "---\nx: null\n---\n",             // null
            "---\nx: ~\n---\n",                // null
            "---\nx: 12:30\n---\n",            // a time
            "---\nx: [a, b]\n---\n",           // a list
            "---\nx: {a: b}\n---\n",           // a map
            "---\nx: |\n  text\n---\n",        // a block
            "---\nx: >\n  text\n---\n",        // a folded block
            "---\nx:\n  - a\n---\n",           // a list
            "---\nx: &a b\n---\n",             // an anchor
            "---\nx: *a\n---\n",               // an alias
            "---\nx: !tag b\n---\n",           // a tag
            "---\nx: a: b\n---\n",             // `: ` in a value
            "---\nx: a:\n---\n",               // ends in a colon
            "---\nx: long\n  line\n---\n",     // a value over two lines
            "---\nx:\ny: b\n---\n",            // nothing under a key
            "---\nx:\n---\n",                  // nothing under a key
            "---\nx:\n  a: 1\n---\n",          // a nested number
            "---\nx:\n  a:\n    b: c\n---\n",  // two levels
            "---\nx:\n  a: b\n   c: d\n---\n", // uneven indent
            "---\nx: a\nx: b\n---\n",          // a key twice
            "---\nx:\n  a: b\n  a: c\n---\n",  // a key twice
            "---\n\"x\": a\n---\n",            // a quoted key
            "---\nx:a\n---\n",                 // no space after the colon
            "---\nx: \"open\n---\n",           // an open quote
            "---\nx: 'open\n---\n",            // an open quote
            "---\nx: \"a\" b\n---\n",          // text after a quote
            "---\nx: \"a\"# b\n---\n",         // a comment without a space
            "---\nx: \"\\x41\"\n---\n",        // YAML's own escape
            "---\nx: \"\\ud83d\"\n---\n",      // half a surrogate pair
            "---\n\tx: a\n---\n",              // a tab
            "---\nx: a\tb\n---\n",             // a tab
            "---\nx: 'a\tb'\n---\n",           // a tab
            "---\nx: - a\n---\n",              // a list marker
            "---\nx: `a`\n---\n",              // a reserved indicator
            "---\nx: @a\n---\n",               // a reserved indicator
            "---\nx: %a\n---\n",               // a directive
        ];
        for y in refused {
            assert!(frontmatter(y).is_err(), "{y:?} was read as {:?}", frontmatter(y));
        }
        // Text that only looks close.
        let fm = frontmatter("---\na: 3 ways to cut\nb: -x\nc: v1.0\nd: '1.0'\ne: \"yes\"\nf: C#\n---\n").unwrap();
        assert_eq!(
            Value::Object(fm),
            json!({ "a": "3 ways to cut", "b": "-x", "c": "v1.0", "d": "1.0", "e": "yes", "f": "C#" })
        );
    }

    #[test]
    fn the_variable_turns_it_off() {
        for off in ["0", " Off ", "false", "NO"] {
            assert!(!wanted_by(Some(off)), "{off}");
        }
        for on in [None, Some("1"), Some("on"), Some("")] {
            assert!(wanted_by(on), "{on:?}");
        }
    }
}
