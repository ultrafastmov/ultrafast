//! ultrafast as Model Context Protocol tools.
//!
//! - [`rpc`]: the JSON-RPC 2.0 side of MCP (initialize, tools/list,
//!   tools/call), shared by every transport, and the stdio loop.
//! - [`tools`]: the tool definitions with their safety annotations: the
//!   local set (documents are files) and the remote set (documents travel
//!   inline in the call, media as asset ids or https URLs).
//! - [`core`]: what the tools do with a renderer and a document, so a local
//!   session and the self-hosted service answer the same way.
//! - [`http`]: Streamable HTTP for the remote set, behind the server's API
//!   key. The service mounts [`http::handle`] and implements
//!   [`http::Backend`]; a server that signs clients in with OAuth (ultrafast
//!   Cloud) adds an [`http::SignIn`].

pub mod core;
pub mod http;
pub mod remote;
pub mod rpc;
pub mod tools;
