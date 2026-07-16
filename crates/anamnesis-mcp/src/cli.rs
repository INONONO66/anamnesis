//! One-shot CLI (cold model load) + the `serve` subcommand dispatcher.

use std::sync::{Arc, Mutex};

use anamnesis::embedding::EmbeddingProvider;
use anyhow::Result;
use clap::{ArgGroup, Parser, Subcommand};

use crate::extract::audit::{ExtractionAuditResult, resolve_reviewer};
use crate::extract::types::{AuditSupport, ContaminationCategory, RelationVerdict};
use crate::extract::worker::{WorkerError, WorkerNoop, WorkerOutcome, run_worker};

use crate::config::Config;
use crate::memory::{
    EmbeddingMigrationRequest, MemoryRegistry, MigrationLockLease,
    PendingEmbeddingMigrationRequest, acquire_namespace_migration_lock,
};

pub(crate) struct ManualEmbeddingMigration {
    namespace: String,
    db_path: std::path::PathBuf,
    lock_lease: MigrationLockLease,
}

impl ManualEmbeddingMigration {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn into_request(self, provider: Arc<dyn EmbeddingProvider>) -> EmbeddingMigrationRequest {
        (
            PendingEmbeddingMigrationRequest {
                namespace: self.namespace,
                db_path: self.db_path,
                provider,
            },
            self.lock_lease,
        )
            .into()
    }
}

pub(crate) fn prepare_manual_migration(
    registry: &MemoryRegistry,
    namespace: Option<&str>,
) -> Result<ManualEmbeddingMigration, anamnesis::Error> {
    let namespace = registry.canonical_ns_key(namespace);
    let db_path = registry.namespace_db_path(&namespace)?.ok_or_else(|| {
        anamnesis::Error::InvalidInput(
            "manual embedding migration requires a file-backed database".into(),
        )
    })?;
    let metadata = std::fs::metadata(&db_path).map_err(|error| {
        anamnesis::Error::StorageError(format!(
            "namespace {namespace:?} database {} must already exist before migration: {error}",
            db_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(anamnesis::Error::InvalidInput(format!(
            "namespace {namespace:?} database {} is not a regular file",
            db_path.display()
        )));
    }
    let lock_lease = acquire_namespace_migration_lock(&db_path).map_err(|error| {
        anamnesis::Error::StorageError(format!(
            "cannot migrate namespace {namespace:?} while its database is in use; stop the \
             anamnesis daemon or other owner, then retry: {error}"
        ))
    })?;
    let db_path = std::fs::canonicalize(&db_path).map_err(|error| {
        anamnesis::Error::StorageError(format!(
            "resolve namespace {namespace:?} database {}: {error}",
            db_path.display()
        ))
    })?;
    Ok(ManualEmbeddingMigration {
        namespace,
        db_path,
        lock_lease,
    })
}

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
    #[command(group(
        ArgGroup::new("audit_update")
            .args(["candidate", "relation"])
            .multiple(false)
    ))]
    /// Review staged extraction candidates and relations without writing graph content.
    Extract {
        /// Retained for explicit audit invocation compatibility.
        #[arg(long)]
        audit: bool,
        /// Namespace to audit (defaults to the configured namespace).
        #[arg(long)]
        namespace: Option<String>,
        /// Maximum audit rows to return for a list request.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Candidate row to review.
        #[arg(long, conflicts_with = "relation", requires = "support")]
        candidate: Option<u64>,
        /// Candidate support verdict.
        #[arg(long, requires = "candidate", value_parser = parse_audit_support)]
        support: Option<AuditSupport>,
        /// Optional contamination finding for a candidate review.
        #[arg(long, requires = "candidate", value_parser = parse_contamination_category)]
        contamination: Option<ContaminationCategory>,
        /// Relation row to review.
        #[arg(long, conflicts_with = "candidate", requires = "relation_verdict")]
        relation: Option<u64>,
        /// Relation review verdict.
        #[arg(long, requires = "relation", value_parser = parse_relation_verdict)]
        relation_verdict: Option<RelationVerdict>,
        /// Reviewer identity; defaults through audit environment variables.
        #[arg(long, requires = "audit_update")]
        reviewer: Option<String>,
    },
    /// Download/initialize the embedding model, then exit.
    Prewarm,
    /// Re-embed an existing namespace database with the configured model.
    ///
    /// The daemon must be stopped because migration owns the namespace database
    /// lock for the entire operation.
    MigrateEmbeddings {
        /// Namespace to migrate (defaults to the configured namespace).
        #[arg(long)]
        namespace: Option<String>,
    },
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
        /// Include data-minimized recall eligibility telemetry.
        #[arg(long)]
        recall: bool,
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
fn parse_audit_support(value: &str) -> Result<AuditSupport, String> {
    match value {
        "supported" => Ok(AuditSupport::Supported),
        "partial" => Ok(AuditSupport::Partial),
        "unsupported" => Ok(AuditSupport::Unsupported),
        _ => Err("must be supported, partial, or unsupported".to_owned()),
    }
}

fn parse_contamination_category(value: &str) -> Result<ContaminationCategory, String> {
    match value {
        "unsupported-claim" => Ok(ContaminationCategory::UnsupportedClaim),
        "prompt-injection" => Ok(ContaminationCategory::PromptInjection),
        "secret-reexposure" => Ok(ContaminationCategory::SecretReexposure),
        "foreign-scope" => Ok(ContaminationCategory::ForeignScope),
        "contradicts-source" => Ok(ContaminationCategory::ContradictsSource),
        _ => Err(
            "must be unsupported-claim, prompt-injection, secret-reexposure, foreign-scope, or contradicts-source"
                .to_owned(),
        ),
    }
}

fn parse_relation_verdict(value: &str) -> Result<RelationVerdict, String> {
    match value {
        "correct" => Ok(RelationVerdict::Correct),
        "wrong-type" => Ok(RelationVerdict::WrongType),
        "wrong-direction" => Ok(RelationVerdict::WrongDirection),
        "invalid" => Ok(RelationVerdict::Invalid),
        _ => Err("must be correct, wrong-type, wrong-direction, or invalid".to_owned()),
    }
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
/// Whether an extract invocation is an audit request rather than a worker run.
pub fn is_extract_audit(cli: &Cli) -> bool {
    matches!(
        &cli.command,
        Some(Commands::Extract { audit: true, .. })
            | Some(Commands::Extract {
                candidate: Some(_),
                ..
            })
            | Some(Commands::Extract {
                relation: Some(_),
                ..
            })
    )
}

/// Render the extraction worker result without performing process I/O.
struct ExtractRender {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn render_extract_outcome(result: Result<WorkerOutcome, WorkerError>) -> ExtractRender {
    match result {
        Ok(WorkerOutcome::Staged {
            run_id,
            candidate_count,
            relation_count,
        }) => ExtractRender {
            exit_code: 0,
            stdout: format!(
                "extraction staged: run_id={run_id} candidate_count={candidate_count} relation_count={relation_count}"
            ),
            stderr: String::new(),
        },
        Ok(WorkerOutcome::AlreadyStaged {
            run_id,
            candidate_count,
            relation_count,
        }) => ExtractRender {
            exit_code: 0,
            stdout: format!(
                "extraction already staged: run_id={run_id} candidate_count={candidate_count} relation_count={relation_count}"
            ),
            stderr: String::new(),
        },
        Ok(WorkerOutcome::Noop(WorkerNoop::ModeOff)) => ExtractRender {
            exit_code: 0,
            stdout: "extraction disabled".to_owned(),
            stderr: String::new(),
        },
        Ok(WorkerOutcome::Noop(WorkerNoop::WorkerBusy)) => ExtractRender {
            exit_code: 0,
            stdout: "extraction already running".to_owned(),
            stderr: String::new(),
        },
        Ok(WorkerOutcome::Noop(WorkerNoop::BelowThreshold)) => ExtractRender {
            exit_code: 0,
            stdout: "extraction skipped: insufficient captured turns".to_owned(),
            stderr: String::new(),
        },
        Err(error) => ExtractRender {
            exit_code: 1,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

/// Run the explicit, opt-in extraction worker. Audit requests are dispatched to
/// the daemon instead and therefore never parse provider configuration.
pub fn run_extract_worker(cli: &Cli) -> Result<()> {
    let namespace = match &cli.command {
        Some(Commands::Extract { namespace, .. }) => namespace.as_deref(),
        _ => None,
    };
    let cfg = Config::from_env();
    let rendered = render_extract_outcome(run_worker(&cfg, namespace));
    if rendered.exit_code == 0 {
        write_oneshot_stdout(cli, &rendered.stdout);
        return Ok(());
    }
    tracing::error!(error = %rendered.stderr, "extraction worker failed");
    Err(anyhow::anyhow!(rendered.stderr))
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
        None
        | Some(
            Commands::Serve { .. }
            | Commands::Daemon
            | Commands::Dashboard { .. }
            | Commands::MigrateEmbeddings { .. },
        ) => Ok(Oneshot::Serve),

        // The `hook` family always talks to the warm daemon as an async client
        // (there is no embedded hook mode); defer to `run_oneshot_client`.
        Some(Commands::Hook { .. }) => Ok(Oneshot::Client),

        Some(Commands::Extract { .. }) => Ok(Oneshot::Client),
        // Daemon-routed one-shots: default mode defers to the async client; only
        // the embedded/no-daemon mode runs here against the DB directly.
        Some(Commands::Recall { .. } | Commands::Remember { .. })
        | Some(Commands::Relate { .. } | Commands::Stats { .. })
            if !embedded =>
        {
            Ok(Oneshot::Client)
        }

        Some(Commands::Recall { .. }) => {
            let registry = Arc::new(Mutex::new(registry(&cfg)));
            let text = run_embedded_oneshot(cli, &registry)?;
            write_oneshot_stdout(cli, &text);
            Ok(Oneshot::Done)
        }
        Some(Commands::Stats { recall: true, .. }) => {
            let registry = Arc::new(Mutex::new(registry(&cfg)));
            let text = run_embedded_oneshot(cli, &registry)?;
            write_oneshot_stdout(cli, &text);
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

pub fn run_migrate_embeddings(namespace: Option<&str>) -> Result<()> {
    let cfg = Config::from_env();
    let reg = registry(&cfg);
    let prepared = prepare_manual_migration(&reg, namespace)?;
    crate::config::ensure_model_cache_dir();
    let model = crate::memory::embed_model_from_name(&cfg.embed_model)?;
    let provider: Arc<dyn EmbeddingProvider> =
        Arc::new(anamnesis::embedding::fastembed::FastEmbedProvider::with_model(model)?);
    run_prepared_migration(prepared, provider, &mut std::io::stdout().lock())
}

fn run_prepared_migration(
    prepared: ManualEmbeddingMigration,
    provider: Arc<dyn EmbeddingProvider>,
    output: &mut dyn std::io::Write,
) -> Result<()> {
    let canonical_namespace = prepared.namespace().to_string();
    let outcome = crate::memory::migration::migrate_embeddings(
        prepared.into_request(provider),
        &mut |event| {
            tracing::info!(
                namespace = %event.namespace,
                committed = event.committed,
                total = event.total,
                batch = event.batch,
                source_model = ?event.source_model,
                source_dimensions = ?event.source_dimensions,
                target_model = %event.target_model,
                target_dimensions = event.target_dimensions,
                "embedding migration batch committed"
            );
        },
    )?;
    write_migration_summary(output, &canonical_namespace, &outcome)?;
    Ok(())
}

fn write_migration_summary(
    output: &mut dyn std::io::Write,
    namespace: &str,
    outcome: &crate::memory::EmbeddingMigrationOutcome,
) -> std::io::Result<()> {
    match outcome {
        crate::memory::EmbeddingMigrationOutcome::NoOp { model, dimensions } => writeln!(
            output,
            "embedding migration no-op: namespace={namespace} model={model} dimensions={dimensions} already compatible"
        ),
        crate::memory::EmbeddingMigrationOutcome::Migrated(report) => writeln!(
            output,
            "embedding migration complete: namespace={namespace} migrated={} resumed={} backup={}",
            report.migrated,
            report.resumed,
            report.backup_path.display()
        ),
    }
}

/// Run a daemon-routed one-shot as an MCP client: ensure the shared daemon, issue
/// one `tools/call`, print the result text, and disconnect. The main dispatcher
/// routes extraction audits here; other commands reach here only when their
/// default daemon mode is selected.
pub async fn run_oneshot_client(cli: &Cli) -> Result<()> {
    use crate::client::call_oneshot;

    // The `hook` family has a different flow (read stdin, gated read-only recall,
    // emit hook JSON, fail-open) and never bails — handle it up front.
    if let Some(Commands::Hook { event }) = &cli.command {
        return crate::hook::run(event).await;
    }

    let cfg = Config::from_env();
    let req =
        oneshot_request(cli).ok_or_else(|| anyhow::anyhow!("invalid one-shot client command"))?;
    let text = call_oneshot(&cfg, req).await?;
    write_oneshot_stdout(cli, &text);
    Ok(())
}

fn format_oneshot_stdout(cli: &Cli, text: &str) -> String {
    if matches!(
        &cli.command,
        Some(Commands::Extract {
            candidate: None,
            relation: None,
            ..
        })
    ) {
        match serde_json::from_str::<ExtractionAuditResult>(text) {
            Ok(result) => crate::extract::audit::render_audit_report(&result),
            Err(_) => format!("{text}\n"),
        }
    } else if matches!(&cli.command, Some(Commands::Stats { recall: true, .. })) {
        text.to_owned()
    } else {
        format!("{text}\n")
    }
}

fn write_oneshot_stdout(cli: &Cli, text: &str) {
    let output = format_oneshot_stdout(cli, text);
    print!("{output}");
}

fn oneshot_request(cli: &Cli) -> Option<crate::proto::Request> {
    use crate::proto::Request;

    match &cli.command {
        Some(Commands::Recall {
            query,
            limit,
            namespace,
            ..
        }) => Some(Request::Recall {
            query: query.clone(),
            limit: Some(*limit as u32),
            namespace: namespace.clone(),
            reinforce: None,
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        }),
        Some(Commands::Remember {
            text, namespace, ..
        }) => Some(Request::Remember {
            content: text.clone(),
            namespace: namespace.clone(),
            tags: None,
            metadata: None,
            scope: None,
        }),
        Some(Commands::Relate {
            from_id,
            to_id,
            relation,
            namespace,
            ..
        }) => Some(Request::Relate {
            from_id: *from_id,
            to_id: *to_id,
            relation: relation.clone(),
            namespace: namespace.clone(),
        }),
        Some(Commands::Stats {
            namespace, recall, ..
        }) => Some(Request::Stats {
            namespace: namespace.clone(),
            recall: (*recall).then_some(true),
        }),
        Some(Commands::Extract {
            namespace,
            limit,
            candidate: None,
            relation: None,
            ..
        }) => Some(Request::ExtractionAuditList {
            namespace: namespace.clone(),
            limit: Some(*limit),
        }),
        Some(Commands::Extract {
            namespace,
            candidate: Some(candidate_id),
            support: Some(support),
            contamination,
            reviewer,
            ..
        }) => Some(Request::UpdateExtractionCandidateAudit {
            namespace: namespace.clone(),
            candidate_id: *candidate_id,
            support: *support,
            contamination: *contamination,
            reviewer: resolve_reviewer(reviewer.as_deref()),
        }),
        Some(Commands::Extract {
            namespace,
            relation: Some(relation_id),
            relation_verdict: Some(verdict),
            reviewer,
            ..
        }) => Some(Request::UpdateExtractionRelationAudit {
            namespace: namespace.clone(),
            relation_id: *relation_id,
            verdict: *verdict,
            reviewer: resolve_reviewer(reviewer.as_deref()),
        }),
        _ => None,
    }
}

fn run_embedded_oneshot(cli: &Cli, registry: &Arc<Mutex<MemoryRegistry>>) -> Result<String> {
    let request =
        oneshot_request(cli).ok_or_else(|| anyhow::anyhow!("invalid embedded one-shot command"))?;
    match crate::dispatch::dispatch(registry, request) {
        crate::proto::Response::Ok { text } => Ok(text),
        crate::proto::Response::Err { message, .. } => Err(anyhow::anyhow!(message)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryRegistry, PolicyStoreState, StubProvider};
    use crate::proto::{RecallEventKind, Request, Response};
    use std::sync::{Arc, Mutex};

    fn recall_cli(embedded: bool) -> Cli {
        use clap::Parser;

        let mut args = vec!["anamnesis", "recall", "caller provenance"];
        if embedded {
            args.push("--embedded");
        }
        Cli::try_parse_from(args).expect("parse recall CLI")
    }
    fn stats_cli(recall: bool, embedded: bool) -> Cli {
        use clap::Parser;

        let mut args = vec!["anamnesis", "stats"];
        if recall {
            args.push("--recall");
        }
        if embedded {
            args.push("--embedded");
        }
        Cli::try_parse_from(args).expect("parse stats CLI")
    }

    fn stub_registry() -> (Arc<Mutex<MemoryRegistry>>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temporary directory");
        let registry = MemoryRegistry::file_backed_unlocked_with(
            Arc::new(StubProvider),
            dir.path().join("memory.db"),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        (Arc::new(Mutex::new(registry)), dir)
    }

    fn seed_recall(registry: &Arc<Mutex<MemoryRegistry>>) {
        let handles = {
            let mut guard = registry.lock().expect("lock registry");
            guard
                .namespace_handles(None)
                .expect("resolve default namespace handles")
        };
        handles
            .memory
            .lock()
            .expect("lock default memory")
            .add_note(
                "caller provenance belongs to the request boundary",
                anamnesis::graph::Timestamp(1),
            )
            .expect("seed recall fixture with deterministic note origin");
    }

    fn recall_events(registry: &Arc<Mutex<MemoryRegistry>>) -> Vec<crate::memory::RecallEvent> {
        let handles = {
            let mut guard = registry.lock().expect("lock registry");
            guard
                .namespace_handles(None)
                .expect("resolve default namespace handles")
        };
        let _memory_guard = handles.memory.lock().expect("lock default memory");
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("open default policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("recall must initialize the policy store");
        };
        store
            .read_recall_events_for_test()
            .expect("read persisted recall events")
    }

    #[test]
    fn raw_cli_recall_builds_an_unclassified_daemon_request() {
        let request = oneshot_request(&recall_cli(false))
            .expect("raw daemon-routed recall must build a request");
        let wire = serde_json::to_string(&request).expect("serialize daemon request");
        assert!(
            !wire.contains("\"event_kind\""),
            "raw CLI recall must omit unclassified provenance from daemon wire: {wire}"
        );
        assert!(matches!(
            request,
            Request::Recall {
                query,
                limit: Some(20),
                namespace: None,
                event_kind: None,
                ..
            } if query == "caller provenance"
        ));
    }

    #[test]
    fn embedded_cli_recall_uses_local_dispatch_with_unknown_provenance_and_daemon_bytes() {
        let (daemon_registry, _daemon_dir) = stub_registry();
        let (embedded_registry, _embedded_dir) = stub_registry();
        seed_recall(&daemon_registry);
        seed_recall(&embedded_registry);

        let daemon_text = match crate::dispatch::dispatch(
            &daemon_registry,
            oneshot_request(&recall_cli(false))
                .expect("raw daemon-routed recall must build a request"),
        ) {
            Response::Ok { text } => text,
            response => panic!("daemon recall must succeed: {response:?}"),
        };
        let embedded_text = run_embedded_oneshot(&recall_cli(true), &embedded_registry)
            .expect("embedded recall must dispatch locally");

        let daemon_stdout = format_oneshot_stdout(&recall_cli(false), &daemon_text);
        let embedded_stdout = format_oneshot_stdout(&recall_cli(true), &embedded_text);
        assert_eq!(
            embedded_stdout, daemon_stdout,
            "embedded and daemon recall must render byte-identical stdout"
        );
        assert!(
            daemon_stdout.ends_with('\n') && !daemon_stdout.ends_with("\n\n"),
            "recall stdout must end with exactly one trailing newline: {daemon_stdout:?}"
        );
        let events = recall_events(&embedded_registry);
        assert_eq!(events.len(), 1, "embedded recall must persist one event");
        assert_eq!(
            events[0].event_kind,
            RecallEventKind::Unknown,
            "an embedded raw CLI recall has no richer caller provenance"
        );
    }
    #[test]
    fn stats_recall_cli_parses_and_sends_true_to_daemon() {
        let request = oneshot_request(&stats_cli(true, false))
            .expect("stats --recall must build a daemon request");
        let wire = serde_json::to_string(&request).expect("serialize stats request");

        assert!(
            wire.contains("\"recall\":true"),
            "stats --recall must carry recall=true on the daemon wire: {wire}"
        );
        assert!(matches!(
            request,
            Request::Stats {
                namespace: None,
                recall: Some(true),
            }
        ));
    }

    #[test]
    fn stats_without_recall_preserves_none_on_daemon_request() {
        let request = oneshot_request(&stats_cli(false, false))
            .expect("stats without --recall must build a daemon request");

        assert!(matches!(
            request,
            Request::Stats {
                namespace: None,
                recall: None,
            }
        ));
        assert_eq!(
            format_oneshot_stdout(&stats_cli(false, false), "graph stats"),
            "graph stats\n",
            "the existing daemon stats path must keep its one trailing newline"
        );
    }

    #[test]
    fn embedded_and_daemon_stats_recall_render_byte_identically() {
        let (daemon_registry, _daemon_dir) = stub_registry();
        let (embedded_registry, _embedded_dir) = stub_registry();

        let daemon_text = match crate::dispatch::dispatch(
            &daemon_registry,
            oneshot_request(&stats_cli(true, false))
                .expect("stats --recall must build a daemon request"),
        ) {
            Response::Ok { text } => text,
            response => panic!("daemon stats --recall must succeed: {response:?}"),
        };
        let embedded_text = run_embedded_oneshot(&stats_cli(true, true), &embedded_registry)
            .expect("embedded stats --recall must dispatch locally");

        let daemon_stdout = format_oneshot_stdout(&stats_cli(true, false), &daemon_text);
        let embedded_stdout = format_oneshot_stdout(&stats_cli(true, true), &embedded_text);
        assert_eq!(
            embedded_stdout, daemon_stdout,
            "embedded and daemon stats --recall must have byte-identical stdout framing"
        );
    }

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

    #[test]
    fn migrate_embeddings_parses_optional_namespace() {
        use clap::Parser;

        let with_namespace = Cli::try_parse_from([
            "anamnesis",
            "migrate-embeddings",
            "--namespace",
            "Team Memory",
        ])
        .expect("parse migration namespace");
        assert!(matches!(
            with_namespace.command,
            Some(Commands::MigrateEmbeddings {
                namespace: Some(namespace)
            }) if namespace == "Team Memory"
        ));

        let default_namespace = Cli::try_parse_from(["anamnesis", "migrate-embeddings"])
            .expect("parse migration default namespace");
        assert!(matches!(
            default_namespace.command,
            Some(Commands::MigrateEmbeddings { namespace: None })
        ));
    }
    #[test]
    fn extract_audit_cli_accepts_list_candidate_and_relation_forms() {
        use clap::Parser;

        for args in [
            vec!["anamnesis", "extract", "--audit"],
            vec!["anamnesis", "extract", "--audit", "--limit", "25"],
            vec![
                "anamnesis",
                "extract",
                "--audit",
                "--candidate",
                "7",
                "--support",
                "partial",
                "--contamination",
                "unsupported-claim",
                "--reviewer",
                "reviewer",
                "--namespace",
                "project/anamnesis",
            ],
            vec![
                "anamnesis",
                "extract",
                "--audit",
                "--relation",
                "9",
                "--relation-verdict",
                "wrong-direction",
                "--reviewer",
                "reviewer",
            ],
        ] {
            assert!(
                Cli::try_parse_from(args).is_ok(),
                "valid extraction audit form must parse"
            );
        }
    }

    #[test]
    fn extract_audit_cli_rejects_incomplete_or_mixed_update_forms() {
        use clap::Parser;

        for args in [
            vec!["anamnesis", "extract", "--audit", "--support", "supported"],
            vec!["anamnesis", "extract", "--audit", "--candidate", "7"],
            vec![
                "anamnesis",
                "extract",
                "--audit",
                "--relation-verdict",
                "correct",
            ],
            vec![
                "anamnesis",
                "extract",
                "--audit",
                "--candidate",
                "7",
                "--support",
                "supported",
                "--relation",
                "9",
                "--relation-verdict",
                "correct",
            ],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "incomplete or mixed extraction audit form must be rejected"
            );
        }
    }

    #[test]
    fn already_compatible_cli_outcome_is_noop_without_backup_path() {
        struct CompatibleProvider;

        impl EmbeddingProvider for CompatibleProvider {
            fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, anamnesis::Error> {
                Ok(texts.iter().map(|_| vec![0.5; 384]).collect())
            }

            fn dimensions(&self) -> usize {
                384
            }

            fn model_name(&self) -> &str {
                "intfloat/multilingual-e5-small"
            }
        }

        let dir = tempfile::tempdir().expect("temporary directory");
        let db = dir.path().join("compatible.db");
        let mut storage =
            anamnesis::storage::SqliteStorage::open(&db).expect("create compatible database");
        storage
            .set_embedding_model_name("intfloat/multilingual-e5-small")
            .expect("stamp compatible model");
        drop(storage);
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(CompatibleProvider);
        let registry = crate::memory::MemoryRegistry::file_backed_with(
            Arc::clone(&provider),
            db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        let prepared = prepare_manual_migration(&registry, None).expect("prepare migration");
        let mut output = Vec::new();

        run_prepared_migration(prepared, provider, &mut output).expect("run no-op migration");

        let summary = String::from_utf8(output).expect("UTF-8 summary");
        assert_eq!(
            summary,
            "embedding migration no-op: namespace=default model=intfloat/multilingual-e5-small dimensions=384 already compatible\n"
        );
        assert!(!summary.contains("backup="));
        assert!(
            !crate::memory::backup_path_for_database(&db)
                .expect("derive backup path")
                .exists()
        );
    }
    #[test]
    fn extract_has_no_embedded_flag() {
        use clap::Parser;
        assert!(
            Cli::try_parse_from(["anamnesis", "extract", "--embedded"]).is_err(),
            "extract must never offer an embedded mode"
        );
    }
    #[test]
    fn audit_request_is_daemon_only_and_carries_no_worker_configuration() {
        use clap::Parser;

        let cli = Cli::try_parse_from([
            "anamnesis",
            "extract",
            "--audit",
            "--namespace",
            "project/resolved-namespace",
        ])
        .expect("audit CLI parses without any provider command");
        assert!(matches!(
            oneshot_request(&cli),
            Some(Request::ExtractionAuditList {
                namespace: Some(namespace),
                ..
            }) if namespace == "project/resolved-namespace"
        ));
    }

    #[test]
    fn extract_outcome_rendering_keeps_successes_concise_and_failures_typed() {
        use crate::extract::worker::{WorkerError, WorkerNoop, WorkerOutcome};

        for outcome in [
            WorkerOutcome::Staged {
                run_id: 41,
                candidate_count: 3,
                relation_count: 2,
            },
            WorkerOutcome::AlreadyStaged {
                run_id: 42,
                candidate_count: 5,
                relation_count: 4,
            },
        ] {
            let rendered = render_extract_outcome(Ok(outcome));
            assert_eq!(rendered.exit_code, 0);
            assert!(rendered.stderr.is_empty());
            assert!(
                rendered.stdout.contains("run_id="),
                "successful extraction must identify its run: {}",
                rendered.stdout
            );
            assert!(
                rendered.stdout.contains("candidate_count=")
                    && rendered.stdout.contains("relation_count="),
                "successful extraction must report candidate and relation counts: {}",
                rendered.stdout
            );
        }

        for outcome in [
            WorkerOutcome::Noop(WorkerNoop::ModeOff),
            WorkerOutcome::Noop(WorkerNoop::WorkerBusy),
            WorkerOutcome::Noop(WorkerNoop::BelowThreshold),
        ] {
            let rendered = render_extract_outcome(Ok(outcome));
            assert_eq!(rendered.exit_code, 0);
            assert!(rendered.stderr.is_empty());
            assert!(!rendered.stdout.contains('\n'));
        }

        let failed = render_extract_outcome(Err(WorkerError::Runtime("boom".into())));
        assert_ne!(failed.exit_code, 0);
        assert!(failed.stdout.is_empty());
        assert!(failed.stderr.contains("runtime"));
    }
}
