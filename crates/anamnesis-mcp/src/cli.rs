//! One-shot CLI (cold model load) + the `serve` subcommand dispatcher.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::memory::MemoryRegistry;

#[derive(Parser)]
#[command(name = "anamnesis-mcp", version, about = "Anamnesis MCP server + CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the MCP server over stdio (default when no subcommand is given).
    Serve,
    /// Search memory and print JSON hits.
    Recall {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Store one insight and print its node id.
    Remember {
        text: String,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Download/initialize the embedding model, then exit.
    Prewarm,
}

fn registry(cfg: &Config) -> MemoryRegistry {
    MemoryRegistry::file_backed(
        cfg.default_db.clone(),
        cfg.db_dir(),
        cfg.default_namespace.clone(),
        cfg.reinforce_on_recall,
    )
}

/// Run a one-shot command. Returns `Ok(true)` if handled here, `Ok(false)` if
/// the caller should start the async server (`Serve`/no subcommand).
pub fn run_oneshot(cli: &Cli) -> Result<bool> {
    crate::config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    match &cli.command {
        None | Some(Commands::Serve) => Ok(false),
        Some(Commands::Recall { query, limit, namespace }) => {
            let mut reg = registry(&cfg);
            let hits = reg.recall(query, *limit, namespace.as_deref())?;
            let body = serde_json::json!({
                "hits": hits.iter().map(|h| serde_json::json!({
                    "node_id": h.node_id.0, "text": h.text, "score": h.score,
                    "at_ms": h.at.0, "speaker": h.speaker, "session": h.session,
                })).collect::<Vec<_>>()
            });
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(true)
        }
        Some(Commands::Remember { text, namespace }) => {
            let mut reg = registry(&cfg);
            let id = reg.remember(text, namespace.as_deref())?;
            println!("stored node {id}");
            Ok(true)
        }
        Some(Commands::Prewarm) => {
            let mut reg = registry(&cfg);
            reg.prewarm()?;
            eprintln!("anamnesis-mcp: embedding model ready");
            Ok(true)
        }
    }
}
