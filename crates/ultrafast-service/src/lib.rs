//! ultrafast's render service (docs/service/API.md): the v1 HTTP API that
//! `ultrafast serve` runs, as a library so tests can drive the engine
//! without HTTP and other crates can mount routes on the server.

pub mod api;
pub mod archive;
pub mod auth;
pub mod config;
pub mod inputs;
pub mod job;
pub mod mp4ranges;
pub mod records;
pub mod refs;
pub mod server;
pub mod store;
pub mod usage;
pub mod util;
