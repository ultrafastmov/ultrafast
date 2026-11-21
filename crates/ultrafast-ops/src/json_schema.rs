//! The document format as JSON Schema (draft 2020-12), generated from the
//! op vocabulary ([`crate::schema::vocabulary`]) so the two never drift:
//! `ultrafast schema` prints it, and it is published as
//! `https://ultrafast.mov/schema/v1.json` for editor completion,
//! validation and structured outputs.
//!
//! It accepts what the engine accepts (scalars broadcast to vectors, colours
//! as hex or 0..1 lists, any param animated); [`crate::schema::check`] goes
//! further (misspelled params with suggestions, missing files, broken
//! shaders) and stays the authority.

use serde_json::{Map, Value, json};

/// Where the schema is published.
pub const ID: &str = "https://ultrafast.mov/schema/v1.json";

/// Ease names a key accepts (besides `{"bezier": [...]}` and
/// `{"spring": [damping, cycles]}`).
pub const EASES: &[&str] = &[
    "linear",
    "hold",
    "in_quad",
    "out_quad",
    "in_out_quad",
    "in_cubic",
    "out_cubic",
    "in_out_cubic",
    "in_sine",
    "out_sine",
    "in_out_sine",
    "in_expo",
    "out_expo",
    "in_out_expo",
    "out_back",
    "in_back",
    "in_out_back",
    "out_elastic",
    "out_bounce",
    "spring",
];

/// The JSON Schema of a version-1 document.
pub fn json_schema() -> Value {
    let vocab = crate::schema::vocabulary();
    let ops: Vec<&Value> = vocab.as_array().into_iter().flatten().collect();
    let names: Vec<&str> = ops.iter().filter_map(|o| o["op"].as_str()).collect();

    let mut rules = Vec::new();
    for o in &ops {
        let mut props = Map::new();
        let mut patterns = Map::new();
        for p in o["params"].as_array().into_iter().flatten() {
            let name = p["name"].as_str().unwrap_or("");
            let ty = p["type"].as_str().unwrap_or("");
            let mut desc = p["doc"].as_str().unwrap_or("").to_string();
            match p["default"].as_str() {
                Some(d) if !d.is_empty() => {
                    if !desc.is_empty() {
                        desc.push_str(". ");
                    }
                    desc.push_str(&format!("Default: {d}."));
                }
                _ => {}
            }
            let mut s = json!({ "anyOf": [param_type(ty), { "$ref": "#/$defs/animated" }, { "$ref": "#/$defs/component_param" }] });
            if !desc.is_empty() {
                s["description"] = json!(desc);
            }
            // `axis_<tag>`: one param per variable-font axis.
            if let Some(prefix) = name.strip_suffix("<tag>") {
                patterns.insert(format!("^{prefix}[A-Za-z0-9]{{4}}$"), s);
            } else {
                props.insert(name.to_string(), s);
            }
        }
        let mut params = json!({ "type": "object", "properties": props });
        if !patterns.is_empty() {
            params["patternProperties"] = Value::Object(patterns);
        }
        let mut then = json!({ "properties": { "params": params } });
        let ports: Vec<&str> = o["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str())
            .collect();
        if !ports.is_empty() {
            let ports: Map<String, Value> = ports
                .iter()
                .map(|p| {
                    (
                        p.to_string(),
                        json!({ "type": "string", "description": "id of the node on this port" }),
                    )
                })
                .collect();
            then["properties"]["inputs"] = json!({ "type": "object", "properties": ports });
        }
        rules.push(json!({
            "if": { "properties": { "op": { "const": o["op"] } }, "required": ["op"] },
            "then": then,
        }));
    }

    let num = json!({ "type": "number" });
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": ID,
        "title": "ultrafast document, version 1",
        "description": "A video as one JSON document: a format, a graph of nodes and the node to output. Generated from `ultrafast ops` by `ultrafast schema`; see https://ultrafast.mov/docs/documents/. `ultrafast check` validates further (misspelled params, missing files, broken shaders).",
        "type": "object",
        "required": ["version", "format", "nodes", "output"],
        "properties": {
            "version": { "const": 1, "description": "the document format's version; changes within a version are additive" },
            "format": { "$ref": "#/$defs/format" },
            "nodes": {
                "type": "object",
                "description": "every node by id",
                "additionalProperties": { "$ref": "#/$defs/node" },
            },
            "components": {
                "type": "object",
                "description": "reusable sub-graphs: a node whose op names one is an instance",
                "additionalProperties": { "$ref": "#/$defs/component" },
            },
            "output": { "type": "string", "description": "id of the node to render" },
            "meta": { "type": "object", "description": "free-form notes and tool data; not rendered" },
        },
        "$defs": {
            "format": {
                "type": "object",
                "required": ["width", "height", "fps", "duration"],
                "properties": {
                    "width": { "type": "integer", "minimum": 1, "description": "px" },
                    "height": { "type": "integer", "minimum": 1, "description": "px" },
                    "fps": { "type": "number", "exclusiveMinimum": 0 },
                    "duration": { "type": "number", "exclusiveMinimum": 0, "description": "seconds" },
                    "motion_blur": {
                        "type": "object",
                        "required": ["samples"],
                        "description": "each moving layer averages samples over the shutter",
                        "properties": {
                            "samples": { "type": "integer", "minimum": 1 },
                            "shutter": { "type": "number", "minimum": 0, "description": "frames the shutter is open (0.5 = 180 degrees). Default: 0.5." },
                            "max_samples": { "type": "integer", "minimum": 1, "description": "more samples on fast moves, up to this many" },
                        },
                    },
                },
            },
            "node": {
                "type": "object",
                "required": ["op"],
                "properties": {
                    "op": {
                        "description": "an op or the name of a component",
                        "anyOf": [{ "enum": names }, { "type": "string" }],
                    },
                    "params": { "type": "object" },
                    "inputs": {
                        "type": "object",
                        "description": "port name to the id of the node feeding it",
                        "additionalProperties": { "type": "string" },
                    },
                    "timing": { "$ref": "#/$defs/timing" },
                },
                "allOf": rules,
            },
            "timing": {
                "type": "object",
                "properties": {
                    "in": { "type": "number", "description": "on screen from this time (parent seconds)" },
                    "out": { "type": "number", "description": "on screen until this time" },
                    "offset": { "type": "number", "description": "where local time zero sits in parent time. Default: 0." },
                    "speed": { "type": "number", "description": "local seconds per parent second; negative plays backwards. Default: 1." },
                    "loop": { "type": "number", "exclusiveMinimum": 0, "description": "local time wraps at this duration" },
                },
            },
            "component": {
                "type": "object",
                "required": ["nodes", "output"],
                "properties": {
                    "nodes": { "type": "object", "additionalProperties": { "$ref": "#/$defs/node" } },
                    "output": { "type": "string" },
                    "params": {
                        "type": "object",
                        "description": "exposed params and their defaults; \"$name\" inside the component refers to one",
                        "additionalProperties": { "$ref": "#/$defs/value" },
                    },
                    "inputs": { "type": "array", "items": { "type": "string" }, "description": "image input port names" },
                },
            },
            "component_param": {
                "type": "string",
                "pattern": "^\\$[A-Za-z_][A-Za-z0-9_]*$",
                "description": "inside a component: the value of the component's param of this name",
            },
            "color": {
                "anyOf": [
                    { "type": "string", "pattern": "^#([0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$", "description": "sRGB hex" },
                    { "type": "array", "items": num, "minItems": 3, "maxItems": 4, "description": "0..1 components, straight alpha" },
                ],
            },
            "value": {
                "description": "a param: a number, a list of numbers, text (or a #hex colour), a bool, or animated",
                "anyOf": [
                    num,
                    { "type": "array", "items": num },
                    { "type": "string" },
                    { "type": "boolean" },
                    { "$ref": "#/$defs/animated" },
                ],
            },
            "ease": {
                "anyOf": [
                    { "enum": EASES },
                    {
                        "type": "object",
                        "required": ["bezier"],
                        "properties": { "bezier": { "type": "array", "items": num, "minItems": 4, "maxItems": 4, "description": "CSS cubic-bezier x1, y1, x2, y2" } },
                    },
                    {
                        "type": "object",
                        "required": ["spring"],
                        "properties": { "spring": { "type": "array", "items": num, "maxItems": 2, "description": "damping ratio, oscillations" } },
                    },
                ],
            },
            "key": {
                "type": "object",
                "required": ["t", "v"],
                "properties": {
                    "t": { "type": "number", "description": "seconds" },
                    "v": {
                        "description": "a number, a list or a #hex colour",
                        "anyOf": [num, { "type": "array", "items": num }, { "type": "string", "pattern": "^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$" }],
                    },
                    "ease": { "$ref": "#/$defs/ease", "description": "the ease into the next key. Default: linear." },
                },
            },
            "animated": {
                "type": "object",
                "description": "keyframes, an expression of t, or a driver",
                "oneOf": [
                    {
                        "required": ["keys"],
                        "properties": {
                            "keys": { "type": "array", "items": { "$ref": "#/$defs/key" }, "minItems": 1 },
                            "repeat": { "enum": ["cycle", "pingpong"], "description": "after the last key" },
                        },
                    },
                    {
                        "required": ["expr"],
                        "properties": {
                            "expr": {
                                "description": "a formula of t (seconds), or one per component",
                                "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }],
                            },
                        },
                    },
                    {
                        "required": ["wiggle"],
                        "properties": {
                            "wiggle": {
                                "type": "object",
                                "properties": {
                                    "frequency": { "type": "number", "description": "wiggles per second. Default: 1." },
                                    "amplitude": { "anyOf": [num, { "type": "array", "items": num }], "description": "peak offset. Default: 1." },
                                    "base": { "$ref": "#/$defs/value", "description": "the value it wiggles around" },
                                    "seed": { "type": "integer", "minimum": 0 },
                                    "octaves": { "type": "integer", "minimum": 1, "description": "detail layers. Default: 1." },
                                },
                            },
                        },
                    },
                    {
                        "required": ["oscillate"],
                        "properties": {
                            "oscillate": {
                                "type": "object",
                                "properties": {
                                    "frequency": { "type": "number", "description": "cycles per second. Default: 1." },
                                    "amplitude": { "anyOf": [num, { "type": "array", "items": num }], "description": "Default: 1." },
                                    "base": { "$ref": "#/$defs/value" },
                                    "phase": { "anyOf": [num, { "type": "array", "items": num }], "description": "in cycles, 0..1" },
                                    "wave": { "enum": ["sine", "triangle", "square", "saw"], "description": "Default: sine." },
                                },
                            },
                        },
                    },
                    {
                        "required": ["audio"],
                        "properties": {
                            "audio": {
                                "type": "object",
                                "required": ["src"],
                                "properties": {
                                    "src": { "type": "string", "description": "audio or video file, relative to the document" },
                                    "band": { "enum": ["all", "low", "mid", "high"], "description": "Default: all." },
                                    "range": { "type": "array", "items": num, "minItems": 2, "maxItems": 2, "description": "output for silence and for the loudest moment. Default: [0, 1]." },
                                    "smooth": { "type": "number", "minimum": 0, "description": "release seconds" },
                                    "start": { "type": "number", "description": "seconds into the file at document time 0" },
                                },
                            },
                        },
                    },
                ],
            },
        },
    })
}

/// The schema of one param type as the vocabulary spells it (`number`,
/// `vec2|number`, `enum(a|b)`, `color`...). Vectors take a scalar too,
/// which the engine broadcasts.
fn param_type(ty: &str) -> Value {
    let num = json!({ "type": "number" });
    let vec = |n: usize| json!({ "anyOf": [num, { "type": "array", "items": num, "minItems": n, "maxItems": n }] });
    if let Some(options) = ty.strip_prefix("enum(").and_then(|t| t.strip_suffix(')')) {
        return json!({ "enum": options.split('|').collect::<Vec<_>>() });
    }
    let one = |t: &str| match t {
        "number" => num.clone(),
        "bool" => json!({ "type": "boolean" }),
        "text" => json!({ "type": "string" }),
        "color" => json!({ "$ref": "#/$defs/color" }),
        "vec2" => vec(2),
        "vec3" => vec(3),
        "vec4" => vec(4),
        "number[]" | "vector" => json!({ "type": "array", "items": num }),
        _ => json!({ "$ref": "#/$defs/value" }),
    };
    let parts: Vec<Value> = ty.split('|').map(one).collect();
    if parts.len() == 1 {
        parts.into_iter().next().unwrap()
    } else {
        json!({ "anyOf": parts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_every_op() {
        let s = json_schema();
        let rules = s["$defs"]["node"]["allOf"].as_array().unwrap();
        for d in crate::ops::OPS {
            assert!(
                rules.iter().any(|r| r["if"]["properties"]["op"]["const"] == d.name),
                "no rule for {}",
                d.name
            );
        }
        // Variable-font axes are a pattern, not a literal `axis_<tag>`.
        let text = rules
            .iter()
            .find(|r| r["if"]["properties"]["op"]["const"] == "text")
            .unwrap();
        let params = &text["then"]["properties"]["params"];
        assert!(params["properties"].get("axis_<tag>").is_none());
        assert!(params["patternProperties"].get("^axis_[A-Za-z0-9]{4}$").is_some());
    }

    #[test]
    fn published_copy_is_current() {
        // docs/schema/v1.json is what the site publishes; regenerate it with
        // `ultrafast schema > docs/schema/v1.json`.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/schema/v1.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            // Not in a packaged crate.
            return;
        };
        let published: Value = serde_json::from_str(&text).unwrap();
        assert!(
            published == json_schema(),
            "docs/schema/v1.json is stale: run `ultrafast schema > docs/schema/v1.json`"
        );
    }
}
