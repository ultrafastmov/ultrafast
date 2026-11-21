//! Components whose nodes expand from their params before rendering
//! (captions, blocks, code, clip cameras, SVG and Lottie files) can only
//! expand once an instance gives those params values. Such a component gets
//! a copy per instance with the instance's values substituted in, so its
//! nodes expand like the document's own, wherever it is used.

use std::collections::{BTreeMap, BTreeSet};
use ultrafast_graph::{Component, Document, Param};

/// Nesting deeper than this is left to flattening (which rejects cycles).
const DEPTH: usize = 32;

/// The document with every instance of a component that holds a node of one
/// of `ops` (itself, or through the components it instances) given a copy
/// of its own, named `component@instance/path`; None when there is none.
pub fn per_instance(doc: &Document, ops: &[&str]) -> Option<Document> {
    let mut holds: BTreeSet<String> = BTreeSet::new();
    loop {
        let before = holds.len();
        for (name, c) in &doc.components {
            if !holds.contains(name)
                && c.nodes
                    .values()
                    .any(|n| ops.contains(&n.op.as_str()) || holds.contains(&n.op))
            {
                holds.insert(name.clone());
            }
        }
        if holds.len() == before {
            break;
        }
    }
    if holds.is_empty() {
        return None;
    }
    let mut out = doc.clone();
    let mut made = BTreeMap::new();
    for (id, node) in out.nodes.iter_mut() {
        if holds.contains(&node.op) {
            node.op = copy_for(doc, &holds, &node.op, &node.params, id, 0, &mut made);
        }
    }
    // Every use now has its own copy; the originals' nodes could not expand.
    for name in &holds {
        out.components.remove(name);
    }
    out.components.extend(made);
    Some(out)
}

fn copy_for(
    doc: &Document,
    holds: &BTreeSet<String>,
    name: &str,
    params: &BTreeMap<String, Param>,
    path: &str,
    depth: usize,
    made: &mut BTreeMap<String, Component>,
) -> String {
    let Some(c) = doc.components.get(name) else {
        return name.to_string();
    };
    if depth > DEPTH {
        return name.to_string();
    }
    let mut values = c.params.clone();
    values.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mut copy = c.clone();
    for (id, n) in copy.nodes.iter_mut() {
        n.params = substitute(&n.params, &values);
        if holds.contains(&n.op) {
            let op = n.op.clone();
            n.op = copy_for(doc, holds, &op, &n.params, &format!("{path}/{id}"), depth + 1, made);
        }
    }
    let named = format!("{name}@{path}");
    made.insert(named.clone(), copy);
    named
}

/// `"$name"` params given their value (as flattening gives them; `$$`
/// escapes stay for flattening to undo).
fn substitute(params: &BTreeMap<String, Param>, values: &BTreeMap<String, Param>) -> BTreeMap<String, Param> {
    let is_ref = |s: &str| {
        s.strip_prefix('$').is_some_and(|r| {
            r.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    params
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Param::Text(s) if is_ref(s) => values.get(&s[1..]).cloned().unwrap_or_else(|| v.clone()),
                v => v.clone(),
            };
            (k.clone(), v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_of_expanding_components_get_their_own_copies() {
        let doc = Document::from_json(
            r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":2},"output":"b",
            "components":{
              "subs":{"output":"c","params":{"words":"","tint":"#fff"},
                "nodes":{"c":{"op":"captions","params":{"words":"$words","color":"$tint"}}}},
              "card":{"output":"s","params":{"line":""},
                "nodes":{"s":{"op":"subs","params":{"words":"$line"}}}},
              "plain":{"output":"x","nodes":{"x":{"op":"solid"}}}},
            "nodes":{
              "a":{"op":"subs","params":{"words":"0 1 hello"}},
              "b":{"op":"card","params":{"line":"0 1 world"}},
              "p":{"op":"plain"}}}"##,
        )
        .unwrap();
        let d = per_instance(&doc, &["captions"]).unwrap();
        assert_eq!(d.nodes["a"].op, "subs@a");
        assert_eq!(d.nodes["b"].op, "card@b");
        assert_eq!(d.nodes["p"].op, "plain");
        let a = &d.components["subs@a"].nodes["c"];
        assert_eq!(a.params["words"], Param::Text("0 1 hello".into()));
        assert_eq!(a.params["color"], Param::Text("#fff".into()));
        let inner = &d.components["card@b"].nodes["s"];
        assert_eq!(inner.op, "subs@b/s");
        assert_eq!(
            d.components["subs@b/s"].nodes["c"].params["words"],
            Param::Text("0 1 world".into())
        );
        assert!(!d.components.contains_key("subs") && !d.components.contains_key("card"));
        assert!(d.components.contains_key("plain"));
        assert!(per_instance(&doc, &["block"]).is_none());
    }
}
