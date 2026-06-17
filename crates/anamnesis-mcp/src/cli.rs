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
    /// Search memory and print the recall context block.
    ///
    /// By default this connects to the shared daemon as a client (the daemon owns
    /// the DB). `--embedded` (or `ANAMNESIS_NO_DAEMON=1`) opens the DB directly.
    Recall {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        namespace: Option<String>,
        /// Bypass the shared daemon and open the DB in-process.
        #[arg(long)]
        embedded: bool,
    },
    /// Store one insight and print its node id.
    ///
    /// By default this connects to the shared daemon as a client; `--embedded`
    /// (or `ANAMNESIS_NO_DAEMON=1`) opens the DB directly.
    Remember {
        text: String,
        #[arg(long)]
        namespace: Option<String>,
        /// Bypass the shared daemon and open the DB in-process.
        #[arg(long)]
        embedded: bool,
    },
    /// Link two remembered nodes with a typed reasoning relation.
    ///
    /// Pass node ids from a prior `recall` and a relation (causes, contradicts,
    /// supports, refutes, reason, rejected-alternative, belongs-to, related, or
    /// `custom:<label>`). By default this connects to the shared daemon as a
    /// client; `--embedded` (or `ANAMNESIS_NO_DAEMON=1`) opens the DB directly.
    Relate {
        from_id: u64,
        to_id: u64,
        relation: String,
        #[arg(long)]
        namespace: Option<String>,
        /// Bypass the shared daemon and open the DB in-process.
        #[arg(long)]
        embedded: bool,
    },
    /// Download/initialize the embedding model, then exit.
    Prewarm,
    /// Check the local setup (db path + lock, model cache, config) and print a
    /// checklist. Does NOT load the embedding model.
    Doctor,
    /// Print graph health/size stats for the default namespace.
    ///
    /// By default this connects to the shared daemon as a client; `--embedded`
    /// (or `ANAMNESIS_NO_DAEMON=1`) opens the DB directly.
    Stats {
        #[arg(long)]
        namespace: Option<String>,
        /// Bypass the shared daemon and open the DB in-process.
        #[arg(long)]
        embedded: bool,
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

/// Whether the `Daemon`-routed one-shots (`recall`/`remember`/`relate`/`stats`)
/// should bypass the shared daemon and open the DB in-process. True if the
/// subcommand carries `--embedded` OR `ANAMNESIS_NO_DAEMON` is set.
fn wants_embedded(cli: &Cli) -> bool {
    let flag = matches!(
        &cli.command,
        Some(Commands::Recall { embedded: true, .. })
            | Some(Commands::Remember { embedded: true, .. })
            | Some(Commands::Relate { embedded: true, .. })
            | Some(Commands::Stats { embedded: true, .. })
    );
    flag || crate::env_flag("ANAMNESIS_NO_DAEMON")
}

/// Outcome of attempting a synchronous one-shot.
pub enum Oneshot {
    /// Fully handled synchronously (printed its output).
    Done,
    /// `Serve`/no-subcommand: start the async stdio server.
    Serve,
    /// A daemon-routed one-shot that must run as an MCP client (async). The
    /// caller drives [`run_oneshot_client`] on a tokio runtime.
    Client,
}

/// Run the one-shots that are always synchronous and DB-direct (`prewarm`,
/// `doctor`) plus the `--embedded` variants of the daemon-routed commands.
///
/// Returns [`Oneshot::Client`] for a daemon-routed command in its default
/// (non-embedded) mode — the caller then dispatches [`run_oneshot_client`] on a
/// tokio runtime. Returns [`Oneshot::Serve`] for `Serve`/no-subcommand.
pub fn run_oneshot(cli: &Cli) -> Result<Oneshot> {
    crate::config::ensure_model_cache_dir();
    let cfg = Config::from_env();
    let embedded = wants_embedded(cli);
    match &cli.command {
        // `Serve`/no-subcommand → start the async stdio server. `Daemon` is
        // intercepted earlier in `main` (it runs the async socket daemon, not a
        // synchronous one-shot); it lands here only via the exhaustiveness check.
        None | Some(Commands::Serve { .. }) | Some(Commands::Daemon) => Ok(Oneshot::Serve),

        // Daemon-routed one-shots: default mode defers to the async client; only
        // the embedded/no-daemon mode runs here against the DB directly.
        Some(Commands::Recall { .. } | Commands::Remember { .. })
        | Some(Commands::Relate { .. } | Commands::Stats { .. })
            if !embedded =>
        {
            Ok(Oneshot::Client)
        }

        Some(Commands::Recall {
            query,
            limit,
            namespace,
            ..
        }) => {
            let mut reg = registry(&cfg);
            let packaged = reg.recall_packaged(query, *limit, namespace.as_deref())?;
            print_recall(&packaged);
            Ok(Oneshot::Done)
        }
        Some(Commands::Remember {
            text, namespace, ..
        }) => {
            let mut reg = registry(&cfg);
            let id = reg.remember(text, namespace.as_deref())?;
            println!("stored node {id}");
            Ok(Oneshot::Done)
        }
        Some(Commands::Relate {
            from_id,
            to_id,
            relation,
            namespace,
            ..
        }) => {
            let mut reg = registry(&cfg);
            let edge = reg.relate(*from_id, *to_id, relation, namespace.as_deref())?;
            println!("linked node {from_id} -> node {to_id} ({relation}) as edge {edge}");
            Ok(Oneshot::Done)
        }
        Some(Commands::Stats { namespace, .. }) => {
            let mut reg = registry(&cfg);
            let stats = reg.stats(namespace.as_deref())?;
            print!("{}", crate::server::format_stats(&stats));
            Ok(Oneshot::Done)
        }
        Some(Commands::Prewarm) => {
            let mut reg = registry(&cfg);
            reg.prewarm()?;
            eprintln!("anamnesis-mcp: embedding model ready");
            Ok(Oneshot::Done)
        }
        Some(Commands::Doctor) => {
            doctor(&cfg);
            Ok(Oneshot::Done)
        }
    }
}

/// Run a daemon-routed one-shot as an MCP client: ensure the shared daemon, issue
/// one `tools/call`, print the result text, and disconnect. Only the four
/// `Daemon`-routed commands reach here (the dispatcher sends everything else to
/// the synchronous path); other variants are unreachable by construction.
pub async fn run_oneshot_client(cli: &Cli) -> Result<()> {
    use crate::client::{args, call_tool_oneshot};
    use serde_json::Value;

    let cfg = Config::from_env();
    let (tool, arguments): (&'static str, _) = match &cli.command {
        Some(Commands::Recall {
            query,
            limit,
            namespace,
            ..
        }) => (
            "recall",
            args([
                ("query", Some(Value::from(query.clone()))),
                ("limit", Some(Value::from(*limit as u64))),
                ("namespace", namespace.clone().map(Value::from)),
            ]),
        ),
        Some(Commands::Remember {
            text, namespace, ..
        }) => (
            "remember",
            args([
                ("content", Some(Value::from(text.clone()))),
                ("namespace", namespace.clone().map(Value::from)),
            ]),
        ),
        Some(Commands::Relate {
            from_id,
            to_id,
            relation,
            namespace,
            ..
        }) => (
            "relate",
            args([
                ("from_id", Some(Value::from(*from_id))),
                ("to_id", Some(Value::from(*to_id))),
                ("relation", Some(Value::from(relation.clone()))),
                ("namespace", namespace.clone().map(Value::from)),
            ]),
        ),
        Some(Commands::Stats { namespace, .. }) => (
            "stats",
            args([("namespace", namespace.clone().map(Value::from))]),
        ),
        // The dispatcher only routes the four commands above here.
        _ => unreachable!("run_oneshot_client dispatched for a non-daemon-routed command"),
    };

    let text = call_tool_oneshot(&cfg, tool, arguments).await?;
    println!("{text}");
    Ok(())
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

/// Print an embedded `recall` result in the same shape the daemon's `recall`
/// tool returns: the readable context block followed by a compact `NODES` list of
/// `{node_id, score}` for `relate`. Keeping the two paths identical means a script
/// sees the same output whether or not a daemon is running.
fn print_recall(packaged: &crate::memory::PackagedRecall) {
    let refs: Vec<_> = packaged
        .hits
        .iter()
        .map(|h| serde_json::json!({ "node_id": h.node_id.0, "score": h.score }))
        .collect();
    let refs_json = serde_json::to_string(&refs).unwrap_or_else(|_| "[]".to_string());
    let context = if packaged.context.trim().is_empty() {
        "(no relevant memory)\n".to_string()
    } else {
        packaged.context.clone()
    };
    println!("{context}## NODES (for `relate`)\n{refs_json}");
}
