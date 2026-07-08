//! One-shot CLI (cold model load) + the `serve` subcommand dispatcher.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::memory::MemoryRegistry;

#[derive(Parser)]
#[command(
    name = "anamnesis",
    version,
    about = "Anamnesis cognitive memory — daemon, MCP server, hooks, CLI"
)]
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
    /// Serve a local read-only observability/management dashboard over HTTP.
    ///
    /// Binds `127.0.0.1:<port>` (local-only, no auth) and connects to the shared
    /// daemon as a client — it never opens the DB directly. Browse memories, view
    /// graph stats, and soft-retract nodes from a browser. Prints the URL on
    /// startup; runs until interrupted.
    Dashboard {
        /// TCP port to bind on `127.0.0.1`. `0` (default) picks a free port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Namespace to browse (defaults to the configured namespace). A
        /// per-request `?namespace=` query can still override this.
        #[arg(long)]
        namespace: Option<String>,
    },
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
    /// Claude Code hook entrypoint: read the hook JSON on stdin, do a gated,
    /// read-only recall via the warm daemon, and emit the hook JSON on stdout.
    ///
    /// Always exits 0 (fail-open): any error injects nothing rather than blocking
    /// or erasing the user's prompt. Always routes through the shared daemon (no
    /// `--embedded` mode) so it shares the warm model.
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
}

/// The Claude Code hook events anamnesis handles (v1). clap renders these as
/// `hook session-start` / `hook user-prompt` (kebab-cased), matching the plugin's
/// `hooks.json` commands.
#[derive(Subcommand, Clone, Debug)]
pub enum HookEvent {
    /// `SessionStart`: seed the session with high-salience project memories.
    SessionStart,
    /// `UserPromptSubmit`: activation-gated, read-only recall on the prompt.
    UserPrompt,
    /// `Stop`: capture the just-finished turn (user+assistant) as raw Episodic.
    Stop,
    /// `PreCompact`: flush recent turns before context compaction. (The
    /// extraction signal is emitted by `SessionStart` only — PreCompact stdout
    /// is not injected into context by the host.)
    PreCompact,
    /// `SessionEnd`: flush remaining turns at session close (Claude Code only;
    /// omitted from codex-hooks.json — Codex lacks this event).
    SessionEnd,
}

fn registry(cfg: &Config) -> MemoryRegistry {
    MemoryRegistry::file_backed_with_model(
        cfg.default_db.clone(),
        cfg.db_dir(),
        cfg.default_namespace.clone(),
        cfg.reinforce_on_recall,
        cfg.embed_model.clone(),
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
        // `Serve`/no-subcommand → start the async stdio server. `Daemon` and
        // `Dashboard` are intercepted earlier in `main` (each runs its own
        // long-lived server, not a synchronous one-shot); they land here only via
        // the exhaustiveness check.
        None | Some(Commands::Serve { .. } | Commands::Daemon | Commands::Dashboard { .. }) => {
            Ok(Oneshot::Serve)
        }

        // The `hook` family always talks to the warm daemon as an async client
        // (there is no embedded hook mode); defer to `run_oneshot_client`.
        Some(Commands::Hook { .. }) => Ok(Oneshot::Client),

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
            print!("{}", crate::dispatch::format_stats(&stats));
            Ok(Oneshot::Done)
        }
        Some(Commands::Prewarm) => {
            let mut reg = registry(&cfg);
            reg.prewarm()?;
            eprintln!("anamnesis: embedding model ready");
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
    use crate::client::call_oneshot;
    use crate::proto::Request;

    // The `hook` family has a different flow (read stdin, gated read-only recall,
    // emit hook JSON, fail-open) and never bails — handle it up front.
    if let Some(Commands::Hook { event }) = &cli.command {
        return crate::hook::run(event).await;
    }

    let cfg = Config::from_env();
    let req = match &cli.command {
        Some(Commands::Recall {
            query,
            limit,
            namespace,
            ..
        }) => Request::Recall {
            query: query.clone(),
            limit: Some(*limit as u32),
            namespace: namespace.clone(),
            reinforce: None,
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
        },
        Some(Commands::Remember {
            text, namespace, ..
        }) => Request::Remember {
            content: text.clone(),
            namespace: namespace.clone(),
            tags: None,
            metadata: None,
            scope: None,
        },
        Some(Commands::Relate {
            from_id,
            to_id,
            relation,
            namespace,
            ..
        }) => Request::Relate {
            from_id: *from_id,
            to_id: *to_id,
            relation: relation.clone(),
            namespace: namespace.clone(),
        },
        Some(Commands::Stats { namespace, .. }) => Request::Stats {
            namespace: namespace.clone(),
        },
        // The dispatcher only routes the four commands above here.
        _ => unreachable!("run_oneshot_client dispatched for a non-daemon-routed command"),
    };

    let text = call_oneshot(&cfg, req).await?;
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
    println!("anamnesis doctor");
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
    // release immediately. A failure means another anamnesis process holds it.
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
                "{} is held by another anamnesis process",
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
/// `{node_id, score, cosine}` for `relate`. Keeping the two paths identical means a script
/// sees the same output whether or not a daemon is running.
fn print_recall(packaged: &crate::memory::PackagedRecall) {
    let refs: Vec<_> = packaged
        .hits
        .iter()
        .map(
            |h| serde_json::json!({ "node_id": h.node_id.0, "score": h.score, "cosine": h.cosine }),
        )
        .collect();
    let refs_json = serde_json::to_string(&refs).unwrap_or_else(|_| "[]".to_string());
    let context = if packaged.context.trim().is_empty() {
        "(no relevant memory)\n".to_string()
    } else {
        packaged.context.clone()
    };
    println!("{context}## NODES (for `relate`)\n{refs_json}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_capture_events_parse() {
        use clap::Parser;
        // The binary name + `hook stop` must parse to HookEvent::Stop.
        let cli = Cli::try_parse_from(["anamnesis", "hook", "stop"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Hook {
                event: HookEvent::Stop
            })
        ));
        assert!(Cli::try_parse_from(["anamnesis", "hook", "pre-compact"]).is_ok());
        assert!(Cli::try_parse_from(["anamnesis", "hook", "session-end"]).is_ok());
    }
}
