mod config;
mod memory;
mod server;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::config::Config;
use crate::memory::MemoryRegistry;
use crate::server::AnamnesisServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Logs MUST go to stderr — stdout is the JSON-RPC wire.
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).init();

    config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    let registry = MemoryRegistry::file_backed(
        cfg.default_db.clone(),
        cfg.db_dir(),
        cfg.default_namespace.clone(),
        cfg.reinforce_on_recall,
    );
    let server = AnamnesisServer::new(Arc::new(Mutex::new(registry)));

    tracing::info!("anamnesis-mcp serving over stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
