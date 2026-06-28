mod cli;
mod client;
mod config;
mod daemon;
mod dispatch;
mod hook;
mod launcher;
mod memory;
mod proto;
mod server;
mod transcript;

#[cfg(test)]
mod eval;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::cli::{Cli, Commands, Oneshot};
use crate::config::Config;
use crate::memory::MemoryRegistry;
use crate::server::AnamnesisServer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    // The shared daemon serves the bespoke protocol over a Unix socket (async,
    // long-lived). It is NOT a one-shot, so dispatch it before the synchronous CLI.
    if matches!(cli.command, Some(Commands::Daemon)) {
        return run_daemon();
    }
    // One-shot CLI commands. The synchronous path handles `prewarm`/`doctor` and
    // the `--embedded` (DB-direct) variants of the daemon-routed commands; the
    // default daemon-routed commands run as async MCP clients.
    match cli::run_oneshot(&cli)? {
        Oneshot::Done => return Ok(()),
        Oneshot::Client => return run_oneshot_client(&cli),
        Oneshot::Serve => {}
    }
    // `serve` (or no subcommand) → the async stdio entry point. By default this
    // is the MCP adapter: serve MCP over stdio and forward every tool call to the
    // shared daemon over the bespoke client. The `--embedded` flag or
    // `ANAMNESIS_NO_DAEMON=1` selects the in-process server that owns the DB directly.
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
pub(crate) fn env_flag(var: &str) -> bool {
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

/// Drive a daemon-routed one-shot (`recall`/`remember`/`relate`/`stats` in their
/// default mode) as a bespoke daemon client on a tokio runtime: ensure the daemon,
/// issue one request, print the reply, disconnect.
#[tokio::main]
async fn run_oneshot_client(cli: &Cli) -> Result<()> {
    cli::run_oneshot_client(cli).await
}

/// The MCP adapter (default `serve`): the agent speaks MCP over stdio to this
/// `AnamnesisServer`, whose every tool call is forwarded to the shared daemon over
/// the bespoke client. MCP lives here and in `server.rs` ONLY — the daemon, hook,
/// and CLI clients are MCP-free (ADR-0012). We exit hard once the session ends
/// because `tokio::io::stdin()` does an uncancelable blocking read on a hidden
/// thread that could otherwise hang a graceful runtime drain.
#[tokio::main]
async fn serve_launcher() -> Result<()> {
    config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    let daemon_client = client::DaemonClient::connect(&cfg).await?;
    tracing::info!("anamnesis: MCP adapter over stdio → shared daemon");
    let service = AnamnesisServer::daemon(daemon_client)
        .serve(stdio())
        .await?;
    // The daemon owns the registry (and flushes on its own shutdown); we just
    // relay, so there is nothing to flush here. Run until the agent disconnects
    // (stdin EOF) or a shutdown signal arrives.
    tokio::select! {
        res = service.waiting() => { res?; }
        _ = shutdown_signal() => { tracing::info!("shutdown signal received"); }
    }
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
    let server = AnamnesisServer::local(registry.clone());
    tracing::info!("anamnesis serving over stdio (embedded mode)");
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
