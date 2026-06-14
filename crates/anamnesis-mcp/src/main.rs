mod cli;
mod config;
mod memory;
mod server;

#[cfg(test)]
mod eval;

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
    let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed(
        cfg.default_db.clone(),
        cfg.db_dir(),
        cfg.default_namespace.clone(),
        cfg.reinforce_on_recall,
    )));
    let server = AnamnesisServer::new(registry.clone());
    tracing::info!("anamnesis-mcp serving over stdio");
    let service = server.serve(stdio()).await?;

    // Run until the client disconnects (stdin EOF) OR a shutdown signal arrives.
    // On a signal we flush explicitly, because `process::exit` skips `Drop` (the
    // graceful EOF path flushes via `Memory`'s `Drop`).
    tokio::select! {
        res = service.waiting() => { res?; }
        _ = shutdown_signal() => {
            tracing::info!("shutdown signal received; flushing memory");
            if let Ok(mut guard) = registry.lock()
                && let Err(e) = guard.flush_all_open()
            {
                tracing::error!("flush on shutdown failed: {e}");
            }
        }
    }
    Ok(())
}

/// Resolves when the process receives SIGTERM (or Ctrl-C / SIGINT).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
