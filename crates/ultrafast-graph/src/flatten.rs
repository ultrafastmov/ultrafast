//! Components (precomps) and flattening.
//!
//! A component is a reusable sub-graph with exposed params and image
//! inputs. A node whose `op` names a component is an *instance*. Before
//! planning, instances are expanded into the parent graph with ids prefixed
//! by the instance path (`intro/title`), so fusion works across component
//! boundaries and nothing is rendered twice.
//!
//! Inside a component, a param written as the string `"$name"` takes the
//! instance's value for `name` (or the component's default), and an input
//! written as `"$port"` connects to whatever the instance's `port` input is
//! wired to. The instance's `timing` applies to every node inside it.

use crate::timing::Timing;
use crate::{Document, DocumentError, Node, NodeId, Param};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub nodes: BTreeMap<NodeId, Node>,
    pub output: NodeId,
    /// Exposed params and their defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Param>,
    /// Image input port names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
}

/// A document with every component instance expanded.
#[derive(Debug, Clone)]
pub struct Flat {
    pub doc: Document,
    /// Timing chain (outermost first) of every node that has one.
    pub chains: HashMap<NodeId, Vec<Timing>>,
}

impl Document {
    /// Whether rendering needs [`Document::flatten`] (components or timing).
    pub fn needs_flatten(&self) -> bool {
        // Audio drivers are baked into keyframes on the flattened copy.
        !self.components.is_empty()
            || self
                .nodes
                .values()
                .any(|n| n.timing.is_some() || n.params.values().any(|p| matches!(p, Param::Audio { .. })))
    }

    pub fn flatten(&self) -> Result<Flat, DocumentError> {
        let mut nodes = BTreeMap::new();
        let mut chains = HashMap::new();
        let mut stack = Vec::new();
        let ids = self.expand(
            &self.nodes,
            "",
            &[],
            &HashMap::new(),
            &BTreeMap::new(),
            &mut stack,
            &mut nodes,
            &mut chains,
        )?;
        let output = ids
            .get(&self.output)
            .cloned()
            .ok_or_else(|| DocumentError::MissingOutput(self.output.clone()))?;
        let mut doc = Document {
            version: self.version,
            format: self.format,
            output,
            nodes,
            components: BTreeMap::new(),
            revision: self.revision,
            structure_revision: self.structure_revision,
            instance: self.instance,
        };
        // Flattened nodes carry no timing of their own: it lives in chains.
        for n in doc.nodes.values_mut() {
            n.timing = None;
        }
        doc.validate()?;
        Ok(Flat { doc, chains })
    }

    /// Flat id of the node that produces `id`'s image, for a node in a graph
    /// expanded under `prefix`.
    fn output_id(
        &self,
        graph: &BTreeMap<NodeId, Node>,
        id: &str,
        prefix: &str,
        depth: usize,
    ) -> Result<String, DocumentError> {
        if depth > 64 {
            return Err(DocumentError::Component(format!(
                "components nest too deeply at `{id}`"
            )));
        }
        let node = graph
            .get(id)
            .ok_or_else(|| DocumentError::MissingOutput(format!("{prefix}{id}")))?;
        match self.components.get(&node.op) {
            // A timed instance shows through its gate (see `expand`).
            Some(_) if node.timing.is_some_and(|t| t.has_window()) => Ok(format!("{prefix}{id}/__gate")),
            Some(c) => self.output_id(&c.nodes, &c.output, &format!("{prefix}{id}/"), depth + 1),
            None => Ok(format!("{prefix}{id}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn expand(
        &self,
        graph: &BTreeMap<NodeId, Node>,
        prefix: &str,
        chain: &[Timing],
        ports: &HashMap<String, NodeId>,
        params: &BTreeMap<String, Param>,
        stack: &mut Vec<String>,
        out: &mut BTreeMap<NodeId, Node>,
        chains: &mut HashMap<NodeId, Vec<Timing>>,
    ) -> Result<HashMap<NodeId, NodeId>, DocumentError> {
        let mut ids = HashMap::new();
        for id in graph.keys() {
            ids.insert(id.clone(), self.output_id(graph, id, prefix, stack.len())?);
        }
        let resolve_input = |src: &str| -> Result<NodeId, DocumentError> {
            if let Some(port) = src.strip_prefix('$') {
                ports.get(port).cloned().ok_or_else(|| {
                    DocumentError::Component(format!(
                        "input `${port}` is not connected on the instance at `{prefix}`"
                    ))
                })
            } else {
                ids.get(src)
                    .cloned()
                    .ok_or_else(|| DocumentError::MissingOutput(format!("{prefix}{src}")))
            }
        };
        for (id, node) in graph {
            let mut node_chain = chain.to_vec();
            if let Some(t) = node.timing {
                node_chain.push(t);
            }
            let substituted = substitute(&node.params, params, prefix)?;
            match self.components.get(&node.op) {
                Some(comp) => {
                    // A timed instance acts as one layer: inside its window it
                    // shows its output; outside it shows its input as it is
                    // (an effect) or nothing (a generator). Its inner nodes run
                    // on its clock but are not gated one by one, which would
                    // pass each inner node's own input through instead.
                    let windowed = node.timing.filter(|t| t.has_window());
                    if let Some(t) = windowed {
                        let mut clock = chain.to_vec();
                        clock.push(Timing {
                            in_point: None,
                            out_point: None,
                            ..t
                        });
                        node_chain = clock;
                    }
                    if stack.contains(&node.op) {
                        return Err(DocumentError::Component(format!(
                            "component `{}` contains itself",
                            node.op
                        )));
                    }
                    let mut inner_ports = HashMap::new();
                    for p in &comp.inputs {
                        if let Some(src) = node.inputs.get(p) {
                            inner_ports.insert(p.clone(), resolve_input(src)?);
                        }
                    }
                    let mut inner_params = comp.params.clone();
                    inner_params.extend(substituted);
                    stack.push(node.op.clone());
                    self.expand(
                        &comp.nodes,
                        &format!("{prefix}{id}/"),
                        &node_chain,
                        &inner_ports,
                        &inner_params,
                        stack,
                        out,
                        chains,
                    )?;
                    stack.pop();
                    if let Some(t) = windowed {
                        let inner =
                            self.output_id(&comp.nodes, &comp.output, &format!("{prefix}{id}/"), stack.len())?;
                        let through = comp
                            .inputs
                            .iter()
                            .find(|p| p.as_str() == "source" && inner_ports.contains_key(*p))
                            .or_else(|| comp.inputs.iter().find(|p| inner_ports.contains_key(*p)))
                            .map(|p| inner_ports[p].clone());
                        let a = match through {
                            Some(src) => src,
                            None => {
                                let clear = format!("{prefix}{id}/__clear");
                                out.insert(
                                    clear.clone(),
                                    Node {
                                        op: "solid".into(),
                                        params: [("color".to_string(), Param::Text("#00000000".into()))].into(),
                                        ..Default::default()
                                    },
                                );
                                clear
                            }
                        };
                        let gate = format!("{prefix}{id}/__gate");
                        out.insert(
                            gate.clone(),
                            Node {
                                op: "mix".into(),
                                params: [("t".to_string(), Param::Number(1.0))].into(),
                                inputs: [("a".to_string(), a), ("b".to_string(), inner)].into(),
                                ..Default::default()
                            },
                        );
                        let mut gate_chain = chain.to_vec();
                        gate_chain.push(t);
                        chains.insert(gate, gate_chain);
                    }
                }
                None => {
                    let flat_id = format!("{prefix}{id}");
                    let mut n = node.clone();
                    n.params = substituted;
                    n.inputs = node
                        .inputs
                        .iter()
                        .map(|(port, src)| Ok((port.clone(), resolve_input(src)?)))
                        .collect::<Result<_, DocumentError>>()?;
                    if !node_chain.is_empty() {
                        chains.insert(flat_id.clone(), node_chain);
                    }
                    out.insert(flat_id, n);
                }
            }
        }
        Ok(ids)
    }
}

fn substitute(
    params: &BTreeMap<String, Param>,
    values: &BTreeMap<String, Param>,
    prefix: &str,
) -> Result<BTreeMap<String, Param>, DocumentError> {
    // Only `$identifier` inside a component refers to a param; anything else
    // (a shell prompt in a text layer, prices) is literal text.
    let is_ref = |s: &str| {
        !prefix.is_empty()
            && s.strip_prefix('$').is_some_and(|r| {
                r.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
    };
    params
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Param::Text(s) if is_ref(s) => values
                    .get(&s[1..])
                    .cloned()
                    .ok_or_else(|| DocumentError::Component(format!("param `{s}` is not defined at `{prefix}`")))?,
                // `$$` escapes a literal leading dollar sign.
                Param::Text(s) if !prefix.is_empty() && s.starts_with("$$") => Param::Text(s[1..].to_string()),
                v => v.clone(),
            };
            Ok((k.clone(), v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::resolve_chain;

    #[test]
    fn instances_expand_with_params_ports_and_timing() {
        let doc = Document::from_json(
            r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":4},"output":"out",
            "components":{
              "badge":{"output":"comp","params":{"tint":"#ff0000"},"inputs":["under"],
                "nodes":{
                  "dot":{"op":"shape","params":{"color":"$tint"}},
                  "comp":{"op":"over","inputs":{"fg":"dot","bg":"$under"}}}}},
            "nodes":{
              "bg":{"op":"solid"},
              "a":{"op":"badge","inputs":{"under":"bg"},"timing":{"in":1,"offset":1}},
              "b":{"op":"badge","inputs":{"under":"a"},"params":{"tint":"#00ff00"}},
              "out":{"op":"opacity","inputs":{"source":"b"}}}}"##,
        )
        .unwrap();
        let flat = doc.flatten().unwrap();
        let n = &flat.doc.nodes;
        assert_eq!(n["out"].inputs["source"], "b/comp");
        // `a` is timed: it shows through a gate that passes its input on
        // outside the window, and its inner nodes run on its clock.
        assert_eq!(n["b/comp"].inputs["bg"], "a/__gate");
        assert_eq!(n["a/__gate"].inputs["a"], "bg");
        assert_eq!(n["a/__gate"].inputs["b"], "a/comp");
        assert_eq!(n["a/comp"].inputs["bg"], "bg");
        assert_eq!(n["a/dot"].params["color"], Param::Text("#ff0000".into()));
        assert_eq!(n["b/dot"].params["color"], Param::Text("#00ff00".into()));
        let gate = &flat.chains["a/__gate"];
        assert_eq!(resolve_chain(gate, 0.5), None);
        assert_eq!(resolve_chain(gate, 2.0), Some(1.0));
        assert_eq!(resolve_chain(&flat.chains["a/dot"], 2.0), Some(1.0));
        assert!(!flat.chains.contains_key("b/dot"));
    }

    #[test]
    fn recursive_components_are_rejected() {
        let doc = Document::from_json(
            r##"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":1},"output":"x",
            "components":{"loop":{"output":"y","nodes":{"y":{"op":"loop"}}}},
            "nodes":{"x":{"op":"loop"}}}"##,
        );
        // Either parse-time validation or flattening must refuse it.
        match doc {
            Err(_) => {}
            Ok(d) => assert!(d.flatten().is_err()),
        }
    }
}
