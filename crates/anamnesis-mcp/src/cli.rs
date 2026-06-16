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
    ///
    /// Default mode is the *launcher*: ensure the shared daemon is up and act as
    /// a transparent stdio↔socket proxy. `--embedded` (or `ANAMNESIS_NO_DAEMON=1`)
    /// runs the old in-process server that owns the DB directly — a fallback for
    /// debugging or environments without Unix sockets / detached spawns.
    Serve {
        /// Run in-process (own the DB directly) instead of proxying to the shared
        /// daemon. Equivalent to setting `ANAMNESIS_NO_DAEMON=1`.
        #[arg(long)]
        embedded: bool,
    },
    /// Run the shared on-demand daemon: own the resolved DB and serve MCP over a
    /// per-DB Unix socket to many clients. Auto-spawned; not usually run by hand.
    Daemon,
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
    /// Check the local setup (db path + lock, model cache, config) and print a
    /// checklist. Does NOT load the embedding model.
    Doctor,
    /// Print graph health/size stats for the default namespace.
    ///
    /// Opens the registry (loads the embedding model) and prints
    /// `Memory::stats()`.
    Stats {
        #[arg(long)]
        namespace: Option<String>,
    },
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
        // `Serve`/no-subcommand → start the async stdio server. `Daemon` is
        // intercepted earlier in `main` (it runs the async socket daemon, not a
        // synchronous one-shot); it lands here only via the exhaustiveness check.
        None | Some(Commands::Serve { .. }) | Some(Commands::Daemon) => Ok(false),
        Some(Commands::Recall {
            query,
            limit,
            namespace,
        }) => {
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
        Some(Commands::Doctor) => {
            doctor(&cfg);
            Ok(true)
        }
        Some(Commands::Stats { namespace }) => {
            let mut reg = registry(&cfg);
            let stats = reg.stats(namespace.as_deref())?;
            print_stats(&stats);
            Ok(true)
        }
    }
}

/// `[ok]` / `[!!]` checklist line.
fn check(label: &str, ok: bool, detail: impl std::fmt::Display) {
    let mark = if ok { "[ok]" } else { "[!!]" };
    println!("{mark} {label}: {detail}");
}

/// Print a setup checklist without loading the embedding model.
///
/// Verifies: the resolved default DB path and that its directory is creatable;
/// that the sibling `<db>.lock` can be acquired (no other process holding it);
/// the model cache directory; and the resolved config values.
fn doctor(cfg: &Config) {
    println!("anamnesis-mcp doctor");
    println!("====================");

    // Config.
    check("default namespace", true, &cfg.default_namespace);
    check("reinforce on recall", true, cfg.reinforce_on_recall);

    // DB path + directory.
    let db = &cfg.default_db;
    println!("[..] default db: {}", db.display());
    let dir = cfg.db_dir();
    let dir_ok = std::fs::create_dir_all(&dir).is_ok();
    check(
        "db directory writable",
        dir_ok,
        if dir_ok {
            dir.display().to_string()
        } else {
            format!("cannot create {}", dir.display())
        },
    );

    // Lock availability: try to acquire the sibling `<db>.lock` exclusively, then
    // release immediately. A failure means another anamnesis-mcp process holds it.
    let (lock_ok, lock_detail) = probe_lock(db);
    check("db lock available", lock_ok, lock_detail);

    // Model cache dir (env or default). Existence is informational — the model is
    // downloaded lazily on first real use.
    let cache = crate::config::model_cache_dir();
    let cached = cache.is_dir();
    check(
        "model cache dir",
        true,
        format!(
            "{} ({})",
            cache.display(),
            if cached {
                "present"
            } else {
                "not yet created — model downloads on first use"
            }
        ),
    );
}

/// Try to acquire-then-release an exclusive lock on `<db>.lock`. Returns
/// `(available, human-readable detail)`.
fn probe_lock(db: &std::path::Path) -> (bool, String) {
    let mut lock_path = db.to_path_buf().into_os_string();
    lock_path.push(".lock");
    let lock_path = std::path::PathBuf::from(lock_path);
    // Read-only probe: do NOT create the lock file. Its absence means no `serve`
    // has ever held it, so the DB is free.
    let file = match std::fs::OpenOptions::new().write(true).open(&lock_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                true,
                format!("{} (free — no lock file yet)", lock_path.display()),
            );
        }
        Err(e) => return (false, format!("cannot open {}: {e}", lock_path.display())),
    };
    // UFCS to fs4's `try_lock` for the same reason as memory.rs (avoid the
    // inherent `File::try_lock` on rustc >= 1.89).
    if fs4::FileExt::try_lock(&file).is_err() {
        return (
            false,
            format!(
                "{} is held by another anamnesis-mcp process",
                lock_path.display()
            ),
        );
    }
    // Release so a subsequent `serve` can take it.
    let _ = fs4::FileExt::unlock(&file);
    (true, format!("{} (free)", lock_path.display()))
}

/// Pretty-print a `MemoryStats` snapshot to stdout.
fn print_stats(s: &anamnesis::memory::MemoryStats) {
    println!("nodes:                {}", s.node_count);
    println!("edges:                {}", s.edge_count);
    println!(
        "orphans:              {} ({:.1}%)",
        s.orphan_count,
        s.orphan_ratio * 100.0
    );
    println!(
        "contradictions:       {} ({:.1}%)",
        s.contradiction_count,
        s.contradiction_ratio * 100.0
    );
    println!("supersedes:           {}", s.supersede_count);
    println!("retracted:            {}", s.retracted_count);
    println!("missing embeddings:   {}", s.missing_embedding_count);
    println!("avg salience:         {:.3}", s.avg_salience);
    println!("avg degree:           {:.2}", s.average_degree);
    println!("stale (>30d):         {:.1}%", s.stale_ratio * 100.0);
    println!("salience entropy:     {:.3} bits", s.salience_entropy);
    println!("peers:                {}", s.peer_count);
    println!("health grade:         {:?}", s.grade);
    if !s.scope_distribution.is_empty() {
        println!("scope distribution:");
        for (scope, count) in &s.scope_distribution {
            println!("  {scope}: {count}");
        }
    }
}
