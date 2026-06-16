mod cli;
mod config;
mod daemon;
mod launcher;
mod memory;
mod server;

#[cfg(test)]
mod eval;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::cli::{Cli, Commands};
use crate::config::Config;
use crate::memory::MemoryRegistry;
use crate::server::AnamnesisServer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    // The shared daemon runs the rmcp server over a Unix socket (async, long-
    // lived). It is NOT a one-shot, so dispatch it before the synchronous CLI.
    if matches!(cli.command, Some(Commands::Daemon)) {
        return run_daemon();
    }
    // One-shot CLI commands run synchronously (cold model load) and exit.
    if cli::run_oneshot(&cli)? {
        return Ok(());
    }
    // `serve` (or no subcommand) → the async stdio entry point. By default this
    // is the *launcher* (ensure the shared daemon, then proxy stdio↔socket). The
    // `--embedded` flag or `ANAMNESIS_NO_DAEMON=1` selects the old in-process
    // server that owns the DB directly.
    let embedded = matches!(cli.command, Some(Commands::Serve { embedded: true }))
        || env_flag("ANAMNESIS_NO_DAEMON");
    if embedded {
        serve_embedded()
    } else {
        serve_launcher()
    }
}

/// `true` when `var` is set to a truthy value (anything other than unset/empty/
/// `0`/`false`/`no`).
fn env_flag(var: &str) -> bool {
    match std::env::var(var) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no"
        ),
        Err(_) => false,
    }
}

#[tokio::main]
async fn run_daemon() -> Result<()> {
    config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    daemon::run(cfg).await
}

/// Launcher: ensure the shared daemon for the resolved DB is up, then run a
/// transparent stdio↔socket proxy. Does NO engine work — Claude speaks MCP over
/// stdio to us, we relay the bytes to/from the daemon. Once the proxy returns
/// (either side closed) we exit immediately, because `tokio::io::stdin()` does an
/// uncancelable blocking read on a hidden thread that could otherwise hang a
/// graceful runtime drain.
#[tokio::main]
async fn serve_launcher() -> Result<()> {
    config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    let stream = launcher::ensure_daemon(&cfg.default_db).await?;
    tracing::info!("anamnesis-mcp launcher: proxying stdio to the shared daemon");
    launcher::proxy_stdio(stream).await?;
    std::process::exit(0);
}

/// The original in-process server: this process owns the DB and serves MCP over
/// stdio directly. Selected by `--embedded` / `ANAMNESIS_NO_DAEMON=1`.
#[tokio::main]
async fn serve_embedded() -> Result<()> {
    config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed(
        cfg.default_db.clone(),
        cfg.db_dir(),
        cfg.default_namespace.clone(),
        cfg.reinforce_on_recall,
    )));
    let server = AnamnesisServer::new(registry.clone());
    tracing::info!("anamnesis-mcp serving over stdio (embedded mode)");
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
pub(crate) async fn shutdown_signal() {
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
