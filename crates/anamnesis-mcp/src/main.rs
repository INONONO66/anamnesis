mod cli;
mod config;
mod memory;
mod server;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::cli::Cli;
use crate::config::Config;
use crate::memory::MemoryRegistry;
use crate::server::AnamnesisServer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    // One-shot CLI commands run synchronously (cold model load) and exit.
    if cli::run_oneshot(&cli)? {
        return Ok(());
    }
    // Otherwise start the async stdio server.
    serve()
}

#[tokio::main]
async fn serve() -> Result<()> {
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
