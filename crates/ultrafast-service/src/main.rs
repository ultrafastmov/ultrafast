//! The render service on its own (`ultrafast serve` runs the same server
//! with the remote MCP server mounted): see docs/service/SELF_HOST.md.

fn main() -> anyhow::Result<()> {
    let cfg = ultrafast_service::config::Config::from_env()?;
    ultrafast_service::server::run(cfg, Default::default())
}
