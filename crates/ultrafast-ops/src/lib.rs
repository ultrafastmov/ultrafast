//! ultrafast's op vocabulary on the CPU: the op registry, which maps document
//! ops onto the Slang op library and packs their params ([`ops`]), the
//! built-in components ([`builtins`]), and the machine-readable schema of
//! every op with the document check built on it ([`schema`]).
//!
//! The renderer (ultrafast-render) compiles documents with these and re-exports
//! the modules at their old paths; tools that only need the vocabulary or a
//! document's problems, such as a composer that turns another tool's data
//! into documents, use them without a GPU.

// As in the renderer this code grew in: indexed loops over parallel arrays
// (the homography solve) are the clearest form here.
#![allow(clippy::needless_range_loop)]

pub mod batch;
pub mod builtins;
pub mod json_schema;
pub mod ops;
pub mod schema;
pub mod templates;

use ultrafast_graph::NodeId;

/// Why a node's op could not be resolved, wired or packed (ultrafast-render
/// reports each as the `RenderError` variant of the same name).
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("node `{node}` has unknown op `{op}`")]
    UnknownOp { node: NodeId, op: String },
    #[error("node `{node}` is missing required input `{port}`")]
    MissingPort { node: NodeId, port: String },
    #[error("node `{node}` param `{param}`: {reason}")]
    BadParam {
        node: NodeId,
        param: String,
        reason: String,
    },
}
