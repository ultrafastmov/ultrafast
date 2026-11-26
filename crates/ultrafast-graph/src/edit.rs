use crate::{Component, Document, DocumentError, Format, Node, NodeId, Param, Timing};
use serde::{Deserialize, Serialize};

/// A single serializable mutation of a [`Document`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum Edit {
    SetParam {
        node: NodeId,
        param: String,
        value: Param,
    },
    RemoveParam {
        node: NodeId,
        param: String,
    },
    AddNode {
        id: NodeId,
        node: Node,
    },
    /// Adds a node and lays it over the current output (an `over` named
    /// `<id>_over` becomes the output): the quickest way to put something
    /// on screen.
    AddLayer {
        id: NodeId,
        node: Node,
    },
    RemoveNode {
        #[serde(alias = "node")]
        id: NodeId,
    },
    Connect {
        node: NodeId,
        port: String,
        #[serde(alias = "from")]
        source: NodeId,
    },
    Disconnect {
        node: NodeId,
        port: String,
    },
    SetOutput {
        node: NodeId,
    },
    SetFormat {
        format: Format,
    },
    /// Sets (or with `null`, clears) a node's timing.
    SetTiming {
        node: NodeId,
        timing: Option<Timing>,
    },
    /// Defines, replaces, or with `null` removes a component.
    SetComponent {
        name: String,
        component: Option<Component>,
    },
}

/// What an applied batch changed. Drives how much work the renderer redoes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing changed.
    None,
    /// Only param values changed: plan and pipelines are reused as is.
    Params,
    /// Graph shape, ops, format or animation-ness changed: plan is recompiled.
    Structure,
}

/// Reads an edit batch. When an edit is malformed, the message names it
/// (its place in the batch and its kind) and the fields that kind takes, so
/// one fix gets the batch through.
pub fn parse_edits(v: &serde_json::Value) -> Result<Vec<Edit>, String> {
    let items = v.as_array().ok_or("bad edits: `edits` is a list of edit objects")?;
    items
        .iter()
        .enumerate()
        .map(|(i, e)| {
            serde_json::from_value(e.clone()).map_err(|err| {
                let kind = e.get("edit").and_then(serde_json::Value::as_str).unwrap_or("");
                let takes = match kind {
                    "set_param" => "{node, param, value}",
                    "remove_param" => "{node, param}",
                    "add_node" | "add_layer" => "{id, node}",
                    "remove_node" => "{id}",
                    "connect" => "{node, port, source}",
                    "disconnect" => "{node, port}",
                    "set_output" => "{node}",
                    "set_format" => "{format}",
                    "set_timing" => "{node, timing}",
                    "set_component" => "{name, component}",
                    _ => "",
                };
                let takes = if takes.is_empty() {
                    String::new()
                } else {
                    format!("; {kind} takes {takes}")
                };
                format!("bad edits: edit {} of {} ({kind}): {err}{takes}", i + 1, items.len())
            })
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("node `{0}` does not exist")]
    NoSuchNode(NodeId),
    #[error("node `{0}` already exists")]
    NodeExists(NodeId),
    #[error("edit leaves document invalid: {0}")]
    Invalid(#[from] DocumentError),
}

impl Document {
    /// Applies a batch atomically: either every edit lands, or none does.
    pub fn apply(&mut self, edits: &[Edit]) -> Result<Change, EditError> {
        let mut next = self.clone();
        let mut change = Change::None;
        for e in edits {
            let c = next.apply_one(e)?;
            change = change.max(c);
        }
        if change == Change::Structure {
            next.validate()?;
            if next.needs_flatten() {
                next.flatten()?;
            }
        }
        if change != Change::None {
            next.revision = self.revision + 1;
            if change == Change::Structure {
                next.structure_revision = self.structure_revision + 1;
            }
        }
        *self = next;
        Ok(change)
    }

    fn node_mut(&mut self, id: &str) -> Result<&mut Node, EditError> {
        self.nodes
            .get_mut(id)
            .ok_or_else(|| EditError::NoSuchNode(id.to_string()))
    }

    fn apply_one(&mut self, e: &Edit) -> Result<Change, EditError> {
        Ok(match e {
            Edit::SetParam { node, param, value } => {
                let n = self.node_mut(node)?;
                // Toggling static <-> animated changes cacheability, which the
                // plan depends on, so it counts as structural.
                let was_animated = n.params.get(param).is_some_and(Param::is_animated);
                let prev = n.params.insert(param.clone(), value.clone());
                if prev.as_ref() == Some(value) {
                    Change::None
                } else if was_animated != value.is_animated() {
                    Change::Structure
                } else {
                    Change::Params
                }
            }
            Edit::RemoveParam { node, param } => {
                let n = self.node_mut(node)?;
                match n.params.remove(param) {
                    Some(p) if p.is_animated() => Change::Structure,
                    Some(_) => Change::Params,
                    None => Change::None,
                }
            }
            Edit::AddNode { id, node } => {
                if self.nodes.contains_key(id) {
                    return Err(EditError::NodeExists(id.clone()));
                }
                self.nodes.insert(id.clone(), node.clone());
                Change::Structure
            }
            Edit::AddLayer { id, node } => {
                let over = format!("{id}_over");
                for k in [id, &over] {
                    if self.nodes.contains_key(k) {
                        return Err(EditError::NodeExists(k.clone()));
                    }
                }
                self.nodes.insert(id.clone(), node.clone());
                self.nodes.insert(
                    over.clone(),
                    Node {
                        op: "over".into(),
                        inputs: [("fg".to_string(), id.clone()), ("bg".to_string(), self.output.clone())].into(),
                        ..Default::default()
                    },
                );
                self.output = over;
                Change::Structure
            }
            Edit::RemoveNode { id } => {
                self.nodes.remove(id).ok_or_else(|| EditError::NoSuchNode(id.clone()))?;
                Change::Structure
            }
            Edit::Connect { node, port, source } => {
                if !self.nodes.contains_key(source) {
                    return Err(EditError::NoSuchNode(source.clone()));
                }
                self.node_mut(node)?.inputs.insert(port.clone(), source.clone());
                Change::Structure
            }
            Edit::Disconnect { node, port } => {
                self.node_mut(node)?.inputs.remove(port);
                Change::Structure
            }
            Edit::SetOutput { node } => {
                if !self.nodes.contains_key(node) {
                    return Err(EditError::NoSuchNode(node.clone()));
                }
                self.output = node.clone();
                Change::Structure
            }
            Edit::SetTiming { node, timing } => {
                let n = self.node_mut(node)?;
                let had_window = n.timing.is_some_and(|t| t.has_window());
                let prev = std::mem::replace(&mut n.timing, *timing);
                if prev == *timing {
                    Change::None
                } else if had_window != timing.is_some_and(|t| t.has_window()) {
                    Change::Structure
                } else {
                    Change::Params
                }
            }
            Edit::SetComponent { name, component } => {
                let prev = match component {
                    Some(c) => self.components.insert(name.clone(), c.clone()),
                    None => self.components.remove(name),
                };
                if prev.as_ref() == component.as_ref() {
                    Change::None
                } else {
                    Change::Structure
                }
            }
            Edit::SetFormat { format } => {
                if *format == self.format {
                    Change::None
                } else {
                    self.format = *format;
                    Change::Structure
                }
            }
        })
    }
}

impl Document {
    /// The edit batch that turns this document into `to`: what changed
    /// between two versions of a file, as edits. Applying it leaves a
    /// document equal to `to`, and when only values changed the batch is
    /// params-only, so a renderer keeps its plan and pipelines.
    pub fn diff(&self, to: &Document) -> Vec<Edit> {
        let mut out = Vec::new();
        if self.format != to.format {
            out.push(Edit::SetFormat { format: to.format });
        }
        for (name, c) in &to.components {
            if self.components.get(name) != Some(c) {
                out.push(Edit::SetComponent {
                    name: name.clone(),
                    component: Some(c.clone()),
                });
            }
        }
        for name in self.components.keys().filter(|n| !to.components.contains_key(*n)) {
            out.push(Edit::SetComponent {
                name: name.clone(),
                component: None,
            });
        }
        // New nodes (and nodes whose op changed) first, so every later
        // connect finds its source.
        for (id, n) in &to.nodes {
            match self.nodes.get(id) {
                None => out.push(Edit::AddNode {
                    id: id.clone(),
                    node: n.clone(),
                }),
                Some(o) if o.op != n.op => {
                    out.push(Edit::RemoveNode { id: id.clone() });
                    out.push(Edit::AddNode {
                        id: id.clone(),
                        node: n.clone(),
                    });
                }
                _ => {}
            }
        }
        for (id, n) in &to.nodes {
            let Some(o) = self.nodes.get(id).filter(|o| o.op == n.op) else {
                continue;
            };
            for (k, v) in &n.params {
                if o.params.get(k) != Some(v) {
                    out.push(Edit::SetParam {
                        node: id.clone(),
                        param: k.clone(),
                        value: v.clone(),
                    });
                }
            }
            for k in o.params.keys().filter(|k| !n.params.contains_key(*k)) {
                out.push(Edit::RemoveParam {
                    node: id.clone(),
                    param: k.clone(),
                });
            }
            for (port, src) in &n.inputs {
                if o.inputs.get(port) != Some(src) {
                    out.push(Edit::Connect {
                        node: id.clone(),
                        port: port.clone(),
                        source: src.clone(),
                    });
                }
            }
            for port in o.inputs.keys().filter(|p| !n.inputs.contains_key(*p)) {
                out.push(Edit::Disconnect {
                    node: id.clone(),
                    port: port.clone(),
                });
            }
            if o.timing != n.timing {
                out.push(Edit::SetTiming {
                    node: id.clone(),
                    timing: n.timing,
                });
            }
        }
        if self.output != to.output {
            out.push(Edit::SetOutput {
                node: to.output.clone(),
            });
        }
        for id in self.nodes.keys().filter(|id| !to.nodes.contains_key(*id)) {
            out.push(Edit::RemoveNode { id: id.clone() });
        }
        out
    }
}

impl PartialOrd for Change {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Change {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let rank = |c: &Change| match c {
            Change::None => 0,
            Change::Params => 1,
            Change::Structure => 2,
        };
        rank(self).cmp(&rank(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_edits_takes_common_field_names_and_names_the_bad_edit() {
        let v = serde_json::json!([
            { "edit": "remove_node", "node": "a" },
            { "edit": "connect", "node": "b", "port": "source", "from": "c" }
        ]);
        let e = parse_edits(&v).unwrap();
        assert_eq!(e[0], Edit::RemoveNode { id: "a".into() });
        assert_eq!(
            e[1],
            Edit::Connect {
                node: "b".into(),
                port: "source".into(),
                source: "c".into()
            }
        );
        let bad =
            serde_json::json!([{ "edit": "remove_node", "id": "a" }, { "edit": "set_param", "node": "b", "value": 1 }]);
        let msg = parse_edits(&bad).unwrap_err();
        assert!(msg.contains("edit 2 of 2 (set_param)"), "{msg}");
        assert!(msg.contains("set_param takes {node, param, value}"), "{msg}");
    }

    #[test]
    fn diff_turns_one_version_into_the_next() {
        let a = Document::from_json(
            r##"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":2},"output":"o",
            "nodes":{"bg":{"op":"solid","params":{"color":"#000000"}},
                     "fg":{"op":"shape","params":{"color":"#ff0000","size":[4,4]}},
                     "gone":{"op":"solid"},
                     "o":{"op":"over","inputs":{"fg":"fg","bg":"bg"}}}}"##,
        )
        .unwrap();
        // Values only: a params-only batch.
        let mut b = a.clone();
        b.nodes
            .get_mut("fg")
            .unwrap()
            .params
            .insert("color".into(), Param::Text("#00ff00".into()));
        let edits = a.diff(&b);
        assert_eq!(edits.len(), 1);
        let mut c = a.clone();
        assert_eq!(c.apply(&edits).unwrap(), Change::Params);
        assert_eq!(c, b);
        // Shape: a new node wired in, one removed, a timing, a new output.
        let b = Document::from_json(
            r##"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":2},"output":"o2",
            "nodes":{"bg":{"op":"gradient","params":{"a":"#000000","b":"#222222"}},
                     "fg":{"op":"shape","params":{"color":"#ff0000"},"timing":{"in":0.5}},
                     "glow":{"op":"glow","inputs":{"source":"fg"}},
                     "o":{"op":"over","inputs":{"fg":"glow","bg":"bg"}},
                     "o2":{"op":"opacity","params":{"opacity":0.5},"inputs":{"source":"o"}}}}"##,
        )
        .unwrap();
        let mut c = a.clone();
        assert_eq!(c.apply(&a.diff(&b)).unwrap(), Change::Structure);
        assert_eq!(c, b);
        assert!(b.diff(&b).is_empty());
    }

    fn doc() -> Document {
        Document::from_json(
            r#"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":1},
            "output":"a","nodes":{"a":{"op":"solid","params":{"color":[1,0,0,1]}}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn param_edit_is_not_structural() {
        let mut d = doc();
        let c = d
            .apply(&[Edit::SetParam {
                node: "a".into(),
                param: "color".into(),
                value: Param::Vector(vec![0.0, 1.0, 0.0, 1.0]),
            }])
            .unwrap();
        assert_eq!(c, Change::Params);
        assert_eq!((d.revision, d.structure_revision), (1, 0));
    }

    #[test]
    fn failed_batch_is_atomic() {
        let mut d = doc();
        let r = d.apply(&[
            Edit::SetParam {
                node: "a".into(),
                param: "color".into(),
                value: Param::Number(1.0),
            },
            Edit::RemoveNode { id: "a".into() },
        ]);
        assert!(r.is_err());
        assert_eq!(d, doc());
    }
}
