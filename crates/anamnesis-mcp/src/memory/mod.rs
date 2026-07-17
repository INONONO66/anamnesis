//! Namespace-aware wrapper over `anamnesis::Memory`.
//!
//! Owns one `Memory<SqliteStorage>` per namespace, all sharing a single
//! embedding provider that is built lazily on first use. All access is
//! single-threaded by construction (the server holds this behind a Mutex).
//!
//! Split into submodules (behavior-preserving move only, no logic changes):
//! this file owns the [`MemoryRegistry`] struct, its constructors, namespace
//! resolution/opening (including the first-open extraction-queue rebuild),
//! `stats`/`usage_report`/`ingest_conversation`, and the shared test
//! infrastructure. [`mgmt`] owns the single-node management primitives
//! (`update`/`forget`/`supersede`/`list`/`get`/`remember`/`relate`); [`recall`]
//! owns the gated recall primitives. Both submodules' `pub(crate)` items are
//! re-exported here so every existing `crate::memory::X` call site (in
//! `dispatch.rs`, `capture.rs`, …) keeps working unchanged.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::memory::{Hit, MemoryStats};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};

use crate::capture::{extract_redelivery_ms, scan_extraction_state};

mod mgmt;
pub(crate) mod migration;
mod policy;
mod recall;
#[cfg(test)]
mod tests;

pub(crate) use mgmt::*;
pub(crate) use policy::*;
pub(crate) use recall::*;

pub(crate) fn embed_model_from_name(
    name: &str,
) -> Result<anamnesis::embedding::fastembed::EmbeddingModel, Error> {
    anamnesis::embedding::fastembed::embed_model_from_name(name)
}

pub(crate) struct PendingEmbeddingMigrationRequest {
    pub namespace: String,
    pub db_path: PathBuf,
    pub provider: Arc<dyn EmbeddingProvider>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamespaceMigrationState {
    Running,
    Failed { message: String },
    Completed,
}

pub(crate) type NamespaceHandle = Arc<Mutex<Memory<SqliteStorage>>>;
/// The phase-1 namespace handles returned to dispatch callers. The policy
/// handle uses the same canonical key as the graph handle and remains
/// uninitialized until phase 2.
pub(crate) struct NamespaceHandles {
    pub(crate) key: String,
    pub(crate) memory: NamespaceHandle,
    pub(crate) policy: PolicyStoreHandle,
}

pub(crate) enum NamespaceResolution {
    Ready(NamespaceHandle),
    StartMigration(PendingEmbeddingMigrationRequest),
    Migrating,
    MigrationFailed(String),
}

pub(crate) enum NamespaceProbe {
    Resolved(NamespaceResolution),
    Inspect {
        key: String,
        pending: PendingEmbeddingMigrationRequest,
    },
}

pub(crate) struct EmbeddingMigrationRequest {
    pending: PendingEmbeddingMigrationRequest,
    lock_lease: MigrationLockLease,
}

impl From<(PendingEmbeddingMigrationRequest, MigrationLockLease)> for EmbeddingMigrationRequest {
    fn from((pending, lock_lease): (PendingEmbeddingMigrationRequest, MigrationLockLease)) -> Self {
        Self {
            pending,
            lock_lease,
        }
    }
}

pub(crate) struct MigrationLockLease {
    kind: MigrationLockLeaseKind,
}

enum MigrationLockLeaseKind {
    Acquired(std::fs::File),
    DaemonDefault(Arc<std::fs::File>),
}

impl From<std::fs::File> for MigrationLockLease {
    fn from(file: std::fs::File) -> Self {
        Self {
            kind: MigrationLockLeaseKind::Acquired(file),
        }
    }
}

impl From<Arc<std::fs::File>> for MigrationLockLease {
    fn from(file: Arc<std::fs::File>) -> Self {
        Self {
            kind: MigrationLockLeaseKind::DaemonDefault(file),
        }
    }
}

impl MigrationLockLease {
    fn file(&self) -> &std::fs::File {
        match &self.kind {
            MigrationLockLeaseKind::Acquired(file) => file,
            MigrationLockLeaseKind::DaemonDefault(file) => file,
        }
    }

    fn into_daemon_default(self) -> Result<Arc<std::fs::File>, Error> {
        match self.kind {
            MigrationLockLeaseKind::DaemonDefault(file) => Ok(file),
            MigrationLockLeaseKind::Acquired(_) => Err(Error::InvalidInput(
                "migration runtime requires the daemon's default database lease".to_string(),
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmbeddingProgress {
    pub namespace: String,
    pub committed: usize,
    pub total: usize,
    pub batch: usize,
    pub source_model: Option<String>,
    pub source_dimensions: Option<usize>,
    pub target_model: String,
    pub target_dimensions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmbeddingMigrationReport {
    pub scanned: usize,
    pub migrated: usize,
    pub resumed: usize,
    pub batches: usize,
    pub backup_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EmbeddingMigrationOutcome {
    NoOp { model: String, dimensions: usize },
    Migrated(EmbeddingMigrationReport),
}

pub(crate) fn backup_path_for_database(db_path: &std::path::Path) -> Result<PathBuf, Error> {
    let output = std::process::Command::new("date")
        .arg("+%Y%m%d")
        .output()
        .map_err(|error| Error::StorageError(format!("read local calendar date: {error}")))?;
    if !output.status.success() {
        return Err(Error::StorageError(format!(
            "read local calendar date: date exited with {}",
            output.status
        )));
    }
    let stamp = String::from_utf8(output.stdout)
        .map_err(|error| Error::StorageError(format!("decode local calendar date: {error}")))?;
    let stamp = stamp.trim();
    if stamp.len() != 8 || !stamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::StorageError(format!(
            "local calendar date returned invalid YYYYMMDD value {stamp:?}"
        )));
    }
    let mut backup = std::ffi::OsString::from(db_path.as_os_str());
    backup.push(format!(".bak-{stamp}"));
    Ok(PathBuf::from(backup))
}

fn namespace_lock_path(path: &std::path::Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn acquire_namespace_lock(path: &std::path::Path) -> Result<std::fs::File, Error> {
    let lock_path = namespace_lock_path(path);
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            Error::StorageError(format!("open namespace lock file {lock_path:?}: {error}"))
        })?;
    if fs4::FileExt::try_lock(&lock_file).is_err() {
        return Err(Error::StorageError(format!(
            "database {path:?} is already in use by another anamnesis process; use a different \
             ANAMNESIS_DB or namespace"
        )));
    }
    Ok(lock_file)
}

pub(crate) fn acquire_namespace_migration_lock(
    path: &std::path::Path,
) -> Result<MigrationLockLease, Error> {
    acquire_namespace_lock(path).map(MigrationLockLease::from)
}

/// One conversational turn for `ingest_conversation`.
#[derive(Debug, Clone)]
pub struct Turn {
    pub speaker: String,
    pub text: String,
    /// Unix-millis timestamp; if `None`, a monotonic value is assigned.
    pub at_ms: Option<u64>,
}

/// Summary returned by `ingest_conversation`.
#[derive(Debug, Clone, Copy)]
pub struct IngestSummary {
    pub episodic: usize,
    pub semantic: usize,
}

/// Output of [`MemoryRegistry::recall_packaged`].
///
/// `context` is the human-readable context block rendered from the assembled
/// package (the primary `recall` payload); `hits` is the compact, de-duplicated
/// ranked list whose `node_id`s the agent can feed back to `relate`.
#[derive(Debug, Clone)]
pub struct PackagedRecall {
    /// Readable context block from `Recall::as_context()`.
    pub context: String,
    /// De-duplicated ranked hits (id reference for `relate`).
    pub hits: Vec<Hit>,
}
/// Gate decision and data-minimized result attribution for one recall.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallGateTrace {
    pub has_hits: bool,
    pub readout_pass: bool,
    pub cosine_pass: bool,
    pub eligible: bool,
    pub top_score: Option<f64>,
    pub top_cosine: Option<f64>,
    pub gate_threshold: Option<f64>,
    pub cosine_gate: Option<f64>,
    pub result_node_ids: Vec<u64>,
    pub auto_extract_node_count: usize,
}

/// Full result of a gated recall, separating rendered content from its gate trace.
#[derive(Debug, Clone)]
pub struct RecallOutcome {
    pub packaged: PackagedRecall,
    pub trace: RecallGateTrace,
}

/// Render a [`KnowledgeType`] as the short label `list`/`get` use on the wire
/// (the inverse of [`parse_knowledge_type`]).
pub(crate) fn knowledge_type_label(kt: &KnowledgeType) -> String {
    match kt {
        KnowledgeType::Identity => "identity".to_string(),
        KnowledgeType::Semantic => "semantic".to_string(),
        KnowledgeType::Episodic => "episodic".to_string(),
        KnowledgeType::Custom(label) => label.clone(),
    }
}

/// Render an [`EdgeType`] as the wire label `graph` uses, mirroring
/// [`knowledge_type_label`]'s snake_case convention.
pub(crate) fn edge_type_label(et: &EdgeType) -> String {
    match et {
        EdgeType::Semantic => "semantic".to_string(),
        EdgeType::Causal => "causal".to_string(),
        EdgeType::Temporal => "temporal".to_string(),
        EdgeType::Reason => "reason".to_string(),
        EdgeType::ReinforcedBy => "reinforced_by".to_string(),
        EdgeType::ConsolidatedFrom => "consolidated_from".to_string(),
        EdgeType::ExtractedFrom => "extracted_from".to_string(),
        EdgeType::Entity => "entity".to_string(),
        EdgeType::Supersedes => "supersedes".to_string(),
        EdgeType::RejectedAlternative => "rejected_alternative".to_string(),
        EdgeType::Supports => "supports".to_string(),
        EdgeType::Refutes => "refutes".to_string(),
        EdgeType::BelongsTo => "belongs_to".to_string(),
        EdgeType::Contradicts => "contradicts".to_string(),
        EdgeType::Custom(label) => label.clone(),
    }
}

/// Parse a `list` filter's `node_type` label into a [`KnowledgeType`]. Any
/// label outside the fixed vocabulary becomes `Custom(label)` — `list`'s
/// filter is advisory narrowing, not a validated enum, so an unrecognized
/// label simply matches nothing rather than erroring the whole call.
pub(crate) fn parse_knowledge_type(label: &str) -> KnowledgeType {
    match label.trim().to_ascii_lowercase().as_str() {
        "identity" => KnowledgeType::Identity,
        "semantic" => KnowledgeType::Semantic,
        "episodic" => KnowledgeType::Episodic,
        _ => KnowledgeType::Custom(label.trim().to_string()),
    }
}

/// Whether a [`Error`] is the caller's fault (bad/missing id) vs. an internal
/// fault, for the management tools' `invalid_params` vs `internal` split.
pub(crate) fn is_caller_error(err: &Error) -> bool {
    matches!(
        err,
        Error::NodeNotFound(_) | Error::EdgeNotFound(_) | Error::InvalidInput(_)
    )
}

/// How the shared embedding provider is obtained.
enum ProviderSource {
    /// Pre-built provider supplied by an in-process test.
    #[cfg(test)]
    Ready(Arc<dyn EmbeddingProvider>),
    /// Build the configured FastEmbed provider on first use.
    FastEmbedLazy { model_name: String },
}
/// Storage backend for opened namespaces.
enum Backend {
    /// File-backed: `<dir>/<namespace>.db` (the default namespace uses `default_db`).
    File {
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
    },
    /// In-memory (tests): every namespace is a fresh in-memory graph.
    #[cfg(test)]
    Memory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamespaceCompatibility {
    Ready,
    DimensionMismatch {
        stored_model: Option<String>,
        db_dimensions: Vec<Option<usize>>,
        target_model: String,
        target_dimensions: usize,
    },
    ModelMismatch {
        stored_model: String,
        target_model: String,
        target_dimensions: usize,
    },
    IncompleteMigration {
        target_model: String,
        target_dimensions: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MigrationBackupState {
    NoBackupCreated,
    BackupPreserved { backup_path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MigrationFailureContext {
    FixedCategory,
    BackupCreation {
        backup_path: PathBuf,
        destination_exists: bool,
    },
    CheckpointBackupValidation {
        backup_path: PathBuf,
    },
    CheckpointBackupVerification {
        backup_path: PathBuf,
    },
    BackupVerificationCleanup {
        backup_path: PathBuf,
        validation_path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MigrationFailureCause {
    MissingNode,
    MissingEdge,
    Storage,
    Rejected,
    InvalidConfiguration,
    InvalidInput,
    NonFiniteState,
    BudgetExhausted,
}

impl From<&Error> for MigrationFailureCause {
    fn from(source: &Error) -> Self {
        match source {
            Error::NodeNotFound(_) => Self::MissingNode,
            Error::EdgeNotFound(_) => Self::MissingEdge,
            Error::StorageError(_) => Self::Storage,
            Error::Rejected(_) => Self::Rejected,
            Error::InvalidConfig(_) => Self::InvalidConfiguration,
            Error::InvalidInput(_) => Self::InvalidInput,
            Error::NonFinite(_) => Self::NonFiniteState,
            Error::BudgetExhausted => Self::BudgetExhausted,
        }
    }
}

impl std::fmt::Display for MigrationFailureCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MissingNode => "missing node",
            Self::MissingEdge => "missing edge",
            Self::Storage => "storage",
            Self::Rejected => "rejected",
            Self::InvalidConfiguration => "invalid configuration",
            Self::InvalidInput => "invalid input",
            Self::NonFiniteState => "non-finite state",
            Self::BudgetExhausted => "budget exhausted",
        })
    }
}

pub(crate) struct EmbeddingMigrationFailure {
    backup_state: MigrationBackupState,
    failure_context: MigrationFailureContext,
    source: Error,
}

impl std::fmt::Debug for EmbeddingMigrationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "EmbeddingMigrationFailure({self})")
    }
}

impl EmbeddingMigrationFailure {
    pub(crate) fn new(
        backup_state: MigrationBackupState,
        failure_context: MigrationFailureContext,
        source: Error,
    ) -> Self {
        Self {
            backup_state,
            failure_context,
            source,
        }
    }

    pub(crate) fn retained_source(&self) -> &Error {
        &self.source
    }
}

impl std::fmt::Display for EmbeddingMigrationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            &EmbeddingMigrationError::Failed {
                backup_state: self.backup_state.clone(),
                failure_context: self.failure_context.clone(),
                cause: MigrationFailureCause::from(self.retained_source()),
            }
            .render(),
        )
    }
}

impl std::error::Error for EmbeddingMigrationFailure {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EmbeddingMigrationError {
    Incompatible(NamespaceCompatibility),
    AutomaticMigrationDisabled(NamespaceCompatibility),
    Failed {
        backup_state: MigrationBackupState,
        failure_context: MigrationFailureContext,
        cause: MigrationFailureCause,
    },
}

impl EmbeddingMigrationError {
    pub(crate) fn render(&self) -> String {
        match self {
            Self::Incompatible(compatibility) => compatibility.render(),
            Self::AutomaticMigrationDisabled(compatibility) => format!(
                "{}. Automatic migration is disabled by \
                 ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS; no database mutation occurred.",
                compatibility.message()
            ),
            Self::Failed {
                failure_context:
                    MigrationFailureContext::BackupCreation {
                        backup_path,
                        destination_exists: true,
                    },
                cause,
                ..
            } => format!(
                "embedding migration failed while creating the required backup at {} (cause \
                 category: {cause}); no verified backup was created. An existing item blocks \
                 that path. Preserve or move it before rerunning `{}`.",
                backup_path.display(),
                migration_command()
            ),
            Self::Failed {
                failure_context:
                    MigrationFailureContext::BackupCreation {
                        backup_path,
                        destination_exists: false,
                    },
                cause,
                ..
            } => format!(
                "embedding migration failed while creating the required backup at {} (cause \
                 category: {cause}); no verified backup was created. Resolve access or \
                 free-space issues for that backup path, then rerun `{}`.",
                backup_path.display(),
                migration_command()
            ),
            Self::Failed {
                failure_context: MigrationFailureContext::CheckpointBackupValidation { backup_path },
                cause,
                ..
            } => format!(
                "embedding migration cannot safely validate the checkpoint backup at {} \
                 (cause category: {cause}). Do not overwrite that path; reconcile the checkpoint \
                 with the live database before rerunning `{}`.",
                backup_path.display(),
                migration_command()
            ),
            Self::Failed {
                failure_context:
                    MigrationFailureContext::CheckpointBackupVerification { backup_path },
                cause,
                ..
            } => format!(
                "embedding migration could not re-verify the preserved checkpoint backup at {} \
                 (cause category: {cause}). Preserve it, resolve the verification issue, then \
                 rerun `{}`.",
                backup_path.display(),
                migration_command()
            ),
            Self::Failed {
                failure_context:
                    MigrationFailureContext::BackupVerificationCleanup {
                        backup_path,
                        validation_path,
                    },
                cause,
                ..
            } => format!(
                "embedding migration failed while removing the validation copy at {} after the \
                 checkpoint backup at {} was verified (cause category: {cause}); the checkpoint \
                 backup is preserved. Remove the validation copy if it remains, then rerun `{}`.",
                validation_path.display(),
                backup_path.display(),
                migration_command()
            ),
            Self::Failed {
                backup_state: MigrationBackupState::NoBackupCreated,
                cause,
                ..
            } => format!(
                "embedding migration failed (cause category: {cause}) before a verified backup \
                 was created; no backup was created. Resolve the migration issue, then run \
                 `{}`.",
                migration_command()
            ),
            Self::Failed {
                backup_state: MigrationBackupState::BackupPreserved { backup_path },
                cause,
                ..
            } => format!(
                "embedding migration failed (cause category: {cause}) after a verified backup \
                 was created; the backup at {} is preserved. Resolve the migration issue, then \
                 run `{}`.",
                backup_path.display(),
                migration_command()
            ),
        }
    }
}

impl NamespaceCompatibility {
    pub(crate) fn message(&self) -> String {
        EmbeddingMigrationError::Incompatible(self.clone()).render()
    }

    fn render(&self) -> String {
        match self {
            Self::Ready => "embedding space is compatible".to_string(),
            Self::DimensionMismatch {
                stored_model,
                db_dimensions,
                target_model,
                target_dimensions,
            } => {
                let fallback = stored_model_fallback(stored_model.as_deref());
                let stored_model_text = stored_model.as_deref().map_or_else(String::new, |model| {
                    format!(" The stored model is '{model}'.")
                });
                format!(
                    "embedding dimension mismatch: DB has {} embeddings.{} Configured model \
                     '{target_model}' produces {target_dimensions}-d embeddings. Preferred \
                     recovery: `{}`.{}",
                    format_db_dimensions(db_dimensions),
                    stored_model_text,
                    migration_command(),
                    fallback
                )
            }
            Self::ModelMismatch {
                stored_model,
                target_model,
                target_dimensions,
            } => format!(
                "embedding model mismatch: DB has {target_dimensions}-d embeddings created by \
                 '{stored_model}', but configured model '{target_model}' produces \
                 {target_dimensions}-d embeddings. Preferred recovery: `{}`.{}",
                migration_command(),
                stored_model_fallback(Some(stored_model))
            ),
            Self::IncompleteMigration {
                target_model,
                target_dimensions,
            } => format!(
                "embedding migration checkpoint is incomplete for configured model \
                 '{target_model}' ({target_dimensions}-d). Resume it with `{}`.",
                migration_command()
            ),
        }
    }
}

fn migration_command() -> &'static str {
    "anamnesis migrate-embeddings [--namespace NS]"
}

fn stored_model_fallback(stored_model: Option<&str>) -> String {
    match stored_model {
        Some(model) => format!(
            " To keep the stored embedding space instead, set \
             ANAMNESIS_EMBED_MODEL={model}."
        ),
        None => " To keep the stored embedding space instead, set ANAMNESIS_EMBED_MODEL to the \
             model that created the DB."
            .to_string(),
    }
}

fn format_db_dimensions(db_dimensions: &[Option<usize>]) -> String {
    let mut dimensions: Vec<usize> = db_dimensions.iter().flatten().copied().collect();
    dimensions.sort_unstable();
    dimensions.dedup();
    let formatted = dimensions
        .iter()
        .map(|dimension| format!("{dimension}-d"))
        .collect::<Vec<_>>();

    match (
        formatted.is_empty(),
        db_dimensions.iter().any(Option::is_none),
    ) {
        (true, _) => "unknown-dimension".to_string(),
        (false, true) => format!("{} and unknown-dimension", formatted.join(", ")),
        (false, false) => formatted.join(", "),
    }
}

/// Daemon-lifetime operation counters (in-memory; reset when the daemon
/// exits). A working-session observability window, not a persisted metric.
///
/// Fields are `pub(crate)` so the capture pipeline (`crate::capture`) can bump
/// `extraction_pulls` from `pull_pending`, mirroring the `seen_turn_keys` /
/// `unextracted` cross-module field pattern.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct OpCounters {
    pub(crate) recalls: u64,
    pub(crate) reinforcing_recalls: u64,
    pub(crate) remembers: u64,
    pub(crate) relates: u64,
    pub(crate) captured_turns: u64,
    pub(crate) extraction_pulls: u64,
    // ── Failure / anomaly counters (O1: silent-failure observability) ────────
    // Bumped by `crate::dispatch` at the request boundary, via the same
    // `pub(crate)` cross-module pattern as `extraction_pulls`. Counting is
    // fail-open: a `u64 += 1` cannot fail, so it never alters request behavior
    // or introduces a failure path.
    //
    // These cover ONLY failures the daemon directly observes at dispatch. The
    // hook/client-process classes — daemon-call timeout, hook-side τ gate,
    // transcript parse-fail (all in `hook.rs`), and the shell binary-fetch —
    // occur in a SEPARATE process and are invisible here; surfacing them would
    // need a cross-process reporting channel (a proto message or shared file).
    // TODO(O1): add that channel + a `hook_*` counter set when a consumer needs it.
    /// Requests that returned an error `Response` (any tool): total tool-call
    /// failures the daemon handled.
    pub(crate) dispatch_errors: u64,
    /// Ingest (capture-path) requests that errored — the subset of
    /// `dispatch_errors` that means captured turns were dropped.
    pub(crate) ingest_errors: u64,
    /// Recalls whose package was empty ("nothing to inject": τ-gate trip or no
    /// hits) — a daemon-side proxy for the hook's `(no relevant memory)` no-op.
    pub(crate) empty_recalls: u64,
}

pub struct MemoryRegistry {
    provider: Option<Arc<dyn EmbeddingProvider>>,
    source: ProviderSource,
    backend: Backend,
    /// `pub(crate)` so `crate::dispatch`'s phase-1 (brief global lock) can read
    /// the registry's default without needing a whole-registry method call.
    pub(crate) reinforce_on_recall: bool,
    /// One `Memory` per namespace, each behind its OWN `Mutex` so different
    /// namespaces never block each other (registry-lock-starvation fix, O2).
    /// See [`namespace_handle`](Self::namespace_handle) for the two-phase
    /// locking discipline this enables.
    open: HashMap<String, Arc<Mutex<Memory<SqliteStorage>>>>,
    /// One policy side-store state per namespace, keyed identically to `open`.
    /// Phase 1 only creates `Uninitialized`; phase 2 opens it after locking the
    /// corresponding graph handle.
    policy: HashMap<String, PolicyStoreHandle>,
    default_namespace: String,
    /// Exclusive locks on each opened file-backed DB. Held for the process
    /// lifetime so a second process can't open the same database; the OS
    /// releases them automatically on exit or crash.
    locks: Vec<std::fs::File>,
    /// Whether `open_namespace` takes the per-namespace `<db>.lock`.
    ///
    /// `true` for the embedded one-shot/stdio path (each process owns the DB and
    /// must guard it). `false` for the **daemon**, which already holds the single
    /// exclusive lock on the resolved DB and is the only process that opens it —
    /// re-locking each namespace there would be redundant (and the daemon's own
    /// lock would otherwise deadlock the default namespace against itself).
    lock_on_open: bool,
    /// Persisted-turn dedup keys seen for the default-namespace capture stream.
    /// Loaded from node metadata at daemon start; gates multi-hook duplicates.
    /// `pub(crate)` so the capture pipeline (`crate::capture`) can maintain it.
    pub(crate) seen_turn_keys: HashSet<String>,
    /// Episodic node ids captured but not yet reasoning-extracted (the
    /// queue), keyed by the SAME canonical namespace key
    /// [`canonical_key`](Self::canonical_key) uses for the `open` map — each
    /// namespace's un-extracted turns are isolated from every other
    /// namespace's. `pub(crate)` so the capture pipeline (`crate::capture`)
    /// and `crate::dispatch` can maintain it.
    pub(crate) unextracted: HashMap<String, Vec<NodeId>>,
    /// Daemon-lifetime op counters surfaced by [`usage_report`](Self::usage_report).
    /// `pub(crate)` so the capture pipeline (`crate::capture`) can increment
    /// `captured_turns` / `extraction_pulls` from its impl block there.
    pub(crate) ops: OpCounters,
    migration_states: HashMap<String, NamespaceMigrationState>,
    auto_migrate_embeddings: bool,
}

impl MemoryRegistry {
    /// Production constructor: file-backed, FastEmbed provider built lazily.
    ///
    /// Each opened namespace takes its own `<db>.lock` (single-writer guard for
    /// the embedded one-shot / stdio path, where this process owns the DB).
    pub fn file_backed(
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
    ) -> Self {
        Self::file_backed_with_model(
            default_db,
            dir,
            default_namespace,
            reinforce_on_recall,
            crate::config::DEFAULT_EMBED_MODEL.to_string(),
        )
    }

    pub fn file_backed_with_model(
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
        embed_model: String,
    ) -> Self {
        Self {
            provider: None,
            source: ProviderSource::FastEmbedLazy {
                model_name: embed_model,
            },
            backend: Backend::File {
                default_db,
                dir,
                default_namespace: default_namespace.clone(),
            },
            reinforce_on_recall,
            open: HashMap::new(),
            policy: HashMap::new(),
            default_namespace,
            locks: Vec::new(),
            lock_on_open: true,
            seen_turn_keys: HashSet::new(),
            unextracted: HashMap::new(),
            ops: OpCounters::default(),
            migration_states: HashMap::new(),
            auto_migrate_embeddings: true,
        }
    }

    /// Daemon constructor: file-backed, FastEmbed provider built lazily, with
    /// the default database lock supplied externally.
    ///
    /// The daemon does not re-lock its default database, but named namespace
    /// databases retain their own process-lifetime sibling locks.
    #[allow(dead_code)]
    pub fn file_backed_unlocked(
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
    ) -> Self {
        Self {
            lock_on_open: false,
            ..Self::file_backed(default_db, dir, default_namespace, reinforce_on_recall)
        }
    }

    pub fn file_backed_unlocked_with_model(
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
        embed_model: String,
    ) -> Self {
        Self {
            lock_on_open: false,
            ..Self::file_backed_with_model(
                default_db,
                dir,
                default_namespace,
                reinforce_on_recall,
                embed_model,
            )
        }
    }

    pub(crate) fn set_auto_migrate_embeddings(&mut self, enabled: bool) {
        self.auto_migrate_embeddings = enabled;
    }

    /// Test/embeddable constructor: in-memory graphs + a caller-supplied provider.
    #[cfg(test)]
    pub fn in_memory_with(provider: Arc<dyn EmbeddingProvider>, reinforce_on_recall: bool) -> Self {
        Self {
            provider: Some(provider.clone()),
            source: ProviderSource::Ready(provider),
            backend: Backend::Memory,
            reinforce_on_recall,
            open: HashMap::new(),
            policy: HashMap::new(),
            default_namespace: "default".to_string(),
            locks: Vec::new(),
            lock_on_open: true,
            seen_turn_keys: HashSet::new(),
            unextracted: HashMap::new(),
            ops: OpCounters::default(),
            migration_states: HashMap::new(),
            auto_migrate_embeddings: true,
        }
    }

    /// Test constructor: file-backed (exercises real locking) with a supplied provider.
    #[cfg(test)]
    pub fn file_backed_with(
        provider: Arc<dyn EmbeddingProvider>,
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
    ) -> Self {
        Self {
            provider: Some(provider.clone()),
            source: ProviderSource::Ready(provider),
            backend: Backend::File {
                default_db,
                dir,
                default_namespace: default_namespace.clone(),
            },
            reinforce_on_recall,
            open: HashMap::new(),
            policy: HashMap::new(),
            default_namespace,
            locks: Vec::new(),
            lock_on_open: true,
            seen_turn_keys: HashSet::new(),
            unextracted: HashMap::new(),
            ops: OpCounters::default(),
            migration_states: HashMap::new(),
            auto_migrate_embeddings: true,
        }
    }

    /// Test constructor: file-backed but UNLOCKED (daemon mode) with a supplied
    /// provider — exercises the real socket/file path with a stub embedder.
    #[cfg(test)]
    pub fn file_backed_unlocked_with(
        provider: Arc<dyn EmbeddingProvider>,
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
    ) -> Self {
        Self {
            lock_on_open: false,
            ..Self::file_backed_with(
                provider,
                default_db,
                dir,
                default_namespace,
                reinforce_on_recall,
            )
        }
    }

    fn provider(&mut self) -> Result<Arc<dyn EmbeddingProvider>, Error> {
        if let Some(p) = &self.provider {
            return Ok(p.clone());
        }
        // anamnesis-mcp depends on `anamnesis` with features = ["embed"]
        // unconditionally, so FastEmbedProvider is always available here.
        let p: Arc<dyn EmbeddingProvider> = match &self.source {
            #[cfg(test)]
            ProviderSource::Ready(p) => p.clone(),
            ProviderSource::FastEmbedLazy { model_name } => {
                let model = embed_model_from_name(model_name)?;
                Arc::new(anamnesis::embedding::fastembed::FastEmbedProvider::with_model(model)?)
            }
        };
        self.provider = Some(p.clone());
        Ok(p)
    }

    pub(crate) fn prepare_namespace_probe(
        &mut self,
        namespace: Option<&str>,
    ) -> Result<NamespaceProbe, Error> {
        let key = self.canonical_ns_key(namespace);
        match self.migration_states.get(&key).cloned() {
            Some(NamespaceMigrationState::Running) => {
                return Ok(NamespaceProbe::Resolved(NamespaceResolution::Migrating));
            }
            Some(NamespaceMigrationState::Failed { message }) => {
                return Ok(NamespaceProbe::Resolved(
                    NamespaceResolution::MigrationFailed(message),
                ));
            }
            Some(NamespaceMigrationState::Completed) => {
                self.migration_states.remove(&key);
            }
            None => {}
        }
        if let Some(handle) = self.open.get(&key) {
            return Ok(NamespaceProbe::Resolved(NamespaceResolution::Ready(
                Arc::clone(handle),
            )));
        }

        let Some(db_path) = self.namespace_db_path(&key)? else {
            return self
                .namespace_handle(Some(&key))
                .map(NamespaceResolution::Ready)
                .map(NamespaceProbe::Resolved);
        };
        if !db_path.exists() {
            return self
                .namespace_handle(Some(&key))
                .map(NamespaceResolution::Ready)
                .map(NamespaceProbe::Resolved);
        }

        let provider = self.provider()?;
        Ok(NamespaceProbe::Inspect {
            key: key.clone(),
            pending: PendingEmbeddingMigrationRequest {
                namespace: key,
                db_path,
                provider,
            },
        })
    }

    pub(crate) fn schedule_namespace_migration(
        &mut self,
        key: String,
        pending: PendingEmbeddingMigrationRequest,
        mismatch: NamespaceCompatibility,
    ) -> NamespaceResolution {
        match self.migration_states.get(&key).cloned() {
            Some(NamespaceMigrationState::Running) => NamespaceResolution::Migrating,
            Some(NamespaceMigrationState::Failed { message }) => {
                NamespaceResolution::MigrationFailed(message)
            }
            Some(NamespaceMigrationState::Completed) | None if !self.auto_migrate_embeddings => {
                NamespaceResolution::MigrationFailed(
                    EmbeddingMigrationError::AutomaticMigrationDisabled(mismatch).render(),
                )
            }
            Some(NamespaceMigrationState::Completed) | None => {
                self.migration_states
                    .insert(key, NamespaceMigrationState::Running);
                NamespaceResolution::StartMigration(pending)
            }
        }
    }

    pub(crate) fn finish_namespace_migration(&mut self, key: &str, result: Result<(), String>) {
        let state = match result {
            Ok(()) => NamespaceMigrationState::Completed,
            Err(message) => NamespaceMigrationState::Failed { message },
        };
        self.migration_states.insert(key.to_string(), state);
    }

    #[cfg(test)]
    pub(crate) fn namespace_migration_state(&self, key: &str) -> Option<NamespaceMigrationState> {
        self.migration_states.get(key).cloned()
    }

    /// Sanitize a namespace into a safe file stem (no path traversal).
    fn sanitize(ns: &str) -> String {
        let s: String = ns
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if s.is_empty() {
            "default".to_string()
        } else {
            s.to_lowercase()
        }
    }

    fn resolve_namespace<'a>(&'a self, ns: Option<&'a str>) -> &'a str {
        ns.unwrap_or(&self.default_namespace)
    }

    /// Canonical identity for a namespace: the key under which its single
    /// `Memory` instance (and, for the file backend, its on-disk file) lives.
    ///
    /// For the file backend this is the sanitized stem, so raw namespaces that
    /// sanitize to the same stem (e.g. `"alpha"`/`"Alpha"`, `"a/b"`/`"a-b"`)
    /// collapse to ONE instance over ONE file instead of two instances racing
    /// over the same file. The default namespace keeps its raw key so it stays
    /// distinct from any sanitized collision (and an explicit request for the
    /// default namespace name still routes to `default_db`).
    fn canonical_key(&self, ns: &str) -> String {
        match &self.backend {
            Backend::File {
                default_namespace, ..
            } if ns == default_namespace => ns.to_string(),
            Backend::File { .. } => Self::sanitize(ns),
            #[cfg(test)]
            Backend::Memory => ns.to_string(),
        }
    }

    /// [`canonical_key`](Self::canonical_key) for an `Option<&str>` namespace
    /// arg, resolving `None` to the registry default first — the single key
    /// every per-namespace collection (`open`, `unextracted`) is keyed by.
    /// Pure/cheap (no I/O, no locking), so `crate::capture` and
    /// `crate::dispatch` can call it as many times as needed without
    /// affecting the two-phase locking discipline.
    pub(crate) fn canonical_ns_key(&self, ns: Option<&str>) -> String {
        self.canonical_key(self.resolve_namespace(ns))
    }

    pub(crate) fn namespace_db_path(&self, ns: &str) -> Result<Option<PathBuf>, Error> {
        match &self.backend {
            #[cfg(test)]
            Backend::Memory => Ok(None),
            Backend::File {
                default_db,
                dir,
                default_namespace,
            } => {
                if ns == default_namespace {
                    Ok(Some(default_db.clone()))
                } else {
                    let path = dir.join(format!("{}.db", Self::sanitize(ns)));
                    // A non-default namespace whose sanitized file resolves to the
                    // default namespace's DB would silently share one SQLite file
                    // under two HashMap keys (divergent caches, data leak). Reject it.
                    if path == *default_db {
                        return Err(Error::InvalidInput(format!(
                            "namespace {ns:?} collides with the default namespace's database file; \
                             choose a different namespace name"
                        )));
                    }
                    Ok(Some(path))
                }
            }
        }
    }

    fn open_policy_store(path: Option<&Path>) -> Result<PolicyStore, Error> {
        match path {
            Some(path) => PolicyStore::open(path),
            #[cfg(test)]
            None => PolicyStore::in_memory(),
            #[cfg(not(test))]
            None => Err(Error::StorageError(
                "in-memory policy store is unavailable outside tests".to_string(),
            )),
        }
    }

    fn open_namespace(&mut self, ns: &str) -> Result<Memory<SqliteStorage>, Error> {
        let provider = self.provider()?;
        let provider_dim = provider.dimensions();
        let provider_model = provider.model_name().to_string();
        let path = self.namespace_db_path(ns)?;

        let Some(path) = path else {
            let mut mem = Memory::in_memory_with_provider(provider)?;
            verify_embedding_compatibility(&mut mem, provider_dim, &provider_model)?;
            return Ok(mem);
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::StorageError(format!("create db dir: {e}")))?;
        }

        // The daemon reuses its externally held default DB lock. Named namespace
        // DBs and embedded callers take their own sibling `<db>.lock`, preventing
        // two in-memory caches in different processes from opening one SQLite DB.
        let daemon_named_namespace = match &self.backend {
            Backend::File { default_db, .. } => path != *default_db,
            #[cfg(test)]
            Backend::Memory => false,
        };
        if self.lock_on_open || daemon_named_namespace {
            self.locks.push(acquire_namespace_lock(&path)?);
        }

        let mut mem = Memory::with_provider(path, provider)?;
        verify_embedding_compatibility(&mut mem, provider_dim, &provider_model)?;
        Ok(mem)
    }

    /// Open-or-fetch the per-namespace handle, returning a CLONED `Arc` so the
    /// caller can release the registry's global lock before doing any
    /// expensive `Memory` work (search/embed/ingest). `pub(crate)` so
    /// `crate::dispatch` and `crate::capture` can drive the same two-phase
    /// discipline this registry's own methods use below.
    ///
    /// LOCK-ORDERING INVARIANT (deadlock-freedom): always acquire the global
    /// registry lock (the `Arc<Mutex<MemoryRegistry>>` a caller like
    /// `dispatch`/`daemon` holds to call this) THEN a per-namespace lock
    /// (`handle.lock()` on the returned `Arc`), NEVER the reverse, and NEVER
    /// hold both locks across blocking work (embed/ingest/search). Every
    /// caller in this crate resolves a handle, drops the global lock, THEN
    /// locks the handle — so two requests against different namespaces can
    /// never wait on each other, and the same namespace's `Mutex` still
    /// serializes writers within it (single-writer-per-namespace).
    pub(crate) fn namespace_handle(
        &mut self,
        ns: Option<&str>,
    ) -> Result<Arc<Mutex<Memory<SqliteStorage>>>, Error> {
        let raw = self.resolve_namespace(ns).to_string();
        let key = self.canonical_key(&raw);
        if !self.open.contains_key(&key) {
            let mem = self.open_namespace(&key)?;
            // First open of this namespace THIS process: rebuild its capture
            // queue from durable node metadata, same scan `load_extraction_state`
            // runs for the default namespace at daemon startup (daemon.rs).
            // Only non-default namespaces need this here — the default is
            // already rebuilt by that startup call before any handle is
            // requested. Extend (never clear) the shared dedup set and this
            // namespace's own queue bucket so concurrently-open namespaces'
            // state is untouched.
            let (keys, pending) =
                scan_extraction_state(&mem, Timestamp::now().0, extract_redelivery_ms());
            self.seen_turn_keys.extend(keys);
            self.unextracted
                .entry(key.clone())
                .or_default()
                .extend(pending);
            self.open.insert(key.clone(), Arc::new(Mutex::new(mem)));
        }
        Ok(self.open.get(&key).expect("just inserted").clone())
    }
    /// Resolve graph and policy handles under the caller's brief registry-lock
    /// phase. This creates only an `Uninitialized` policy state; it never opens,
    /// migrates, or issues SQL against the policy side schema.
    pub(crate) fn namespace_handles(
        &mut self,
        ns: Option<&str>,
    ) -> Result<NamespaceHandles, Error> {
        let key = self.canonical_ns_key(ns);
        let memory = self.namespace_handle(ns)?;
        let path = self.namespace_db_path(&key)?;
        let policy = match self.policy.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => Arc::clone(entry.get()),
            std::collections::hash_map::Entry::Vacant(entry) => Arc::clone(entry.insert(Arc::new(
                Mutex::new(PolicyStoreState::Uninitialized { path }),
            ))),
        };

        Ok(NamespaceHandles {
            key,
            memory,
            policy,
        })
    }

    /// Lazily open the resolved policy side store. Callers must hold the
    /// corresponding namespace `Memory` lock before calling this function, so
    /// the lock order is always Memory then PolicyStore. It never accesses the
    /// registry/global lock.
    pub(crate) fn policy_store(
        policy: &PolicyStoreHandle,
    ) -> Result<MutexGuard<'_, PolicyStoreState>, Error> {
        let mut state = policy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = match &*state {
            PolicyStoreState::Uninitialized { path } => path.clone(),
            PolicyStoreState::Ready(_) => return Ok(state),
            PolicyStoreState::Disabled { reason } => {
                return Err(Error::StorageError(reason.clone()));
            }
        };

        match Self::open_policy_store(path.as_deref()) {
            Ok(store) => *state = PolicyStoreState::Ready(store),
            Err(error) => {
                let reason = error.to_string();
                *state = PolicyStoreState::Disabled { reason };
                return Err(error);
            }
        }

        Ok(state)
    }

    /// Flush every open namespace's pending state to disk. Called on graceful
    /// shutdown (SIGTERM), where `process::exit` would otherwise skip `Drop`.
    /// Locks each namespace (poison-recovering) rather than assuming
    /// exclusive access, so a flush during shutdown can never panic even if a
    /// dispatch elsewhere left a namespace's lock poisoned.
    pub fn flush_all_open(&mut self) -> Result<(), Error> {
        for mem in self.open.values() {
            mem.lock().unwrap_or_else(|p| p.into_inner()).flush_all()?;
        }
        Ok(())
    }

    /// Read-only health/size snapshot for a namespace (`Memory::stats`).
    ///
    /// Flushes pending buffers first so the counts reflect live state.
    ///
    /// Production dispatch reads the namespace handle directly for its phase split,
    /// so this convenience wrapper has only test consumers in non-test builds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stats(&mut self, ns: Option<&str>) -> Result<MemoryStats, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem.flush_all()?;
        mem.stats()
    }

    /// Usage/capture section appended to the `stats` tool output.
    /// Counters are daemon-lifetime; backlog/captured/stale are live reads.
    ///
    /// `crate::dispatch`'s `Stats` arm reuses [`mem_usage_totals`] /
    /// [`format_usage_report`] directly (its own phase split), so in a
    /// non-test build this convenience wrapper has only test consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn usage_report(&mut self, ns: Option<&str>) -> Result<String, Error> {
        let ns_key = self.canonical_ns_key(ns);
        let backlog = self.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0);
        let handle = self.namespace_handle(ns)?;
        let (total, stale) = {
            let mem = handle.lock().unwrap_or_else(|p| p.into_inner());
            mem_usage_totals(&mem)
        };
        Ok(format_usage_report(
            &self.ops,
            backlog,
            self.seen_turn_keys.len(),
            total,
            stale,
        ))
    }

    /// Ingest a batch of conversational turns via the bench windowing recipe.
    ///
    /// When `capture` is `true`, each turn is deduplicated by a stable content hash
    /// (`seen_turn_keys`), stamped with `anamnesis:turn_key` and `anamnesis:extracted`
    /// metadata, and enqueued in `unextracted` for downstream reasoning extraction.
    /// When `capture` is `false`, the path is unchanged from the pre-capture behaviour.
    ///
    /// `crate::dispatch`'s `Ingest` arm reuses [`filter_capture_decisions`] /
    /// [`mem_ingest_conversation`] directly (its own phase split), so in a
    /// non-test build this convenience wrapper has only test consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ingest_conversation(
        &mut self,
        session: &str,
        turns: &[Turn],
        ns: Option<&str>,
        capture: bool,
    ) -> Result<IngestSummary, Error> {
        // Resolve the handle FIRST: on first open this rebuilds `seen_turn_keys`
        // for this namespace, so the dedup filter below sees restart-durable state.
        let ns_key = self.canonical_ns_key(ns);
        let handle = self.namespace_handle(ns)?;
        let decisions = filter_capture_decisions(
            &self.seen_turn_keys,
            session,
            turns,
            capture,
            ScopePath::universal(),
        );
        let phase2 = {
            let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
            mem_ingest_conversation(&mut mem, session, decisions)
        };
        // Commit the shared registry state for exactly the turns that were
        // durably marked, regardless of whether the overall ingest ultimately
        // errored (mirrors the pre-split behavior, where each turn's dedup key
        // and queue slot were committed immediately after its own successful
        // `set_metadata_pairs`, not gated on the whole batch succeeding).
        // Enqueued under the SAME canonical key the ingest actually wrote
        // into, so the backlog is isolated per namespace (P1-T4).
        self.ops.captured_turns += phase2.committed.len() as u64;
        let queue = self.unextracted.entry(ns_key).or_default();
        for (epi_id, key) in phase2.committed {
            self.seen_turn_keys.insert(key);
            queue.push(epi_id);
        }
        phase2.outcome
    }

    /// Force the embedding provider to build (download the model). For `prewarm`.
    pub fn prewarm(&mut self) -> Result<(), Error> {
        let provider = self.provider()?;
        provider.embed_single("warm")?;
        Ok(())
    }
}

/// Namespace-scoped `(total_nodes, stale_nodes)` scan for the `usage_report`
/// capture section (14-day staleness window). Read-only.
pub(crate) fn mem_usage_totals(mem: &Memory<SqliteStorage>) -> (usize, usize) {
    const STALE_MS: u64 = 14 * 24 * 60 * 60 * 1000; // 14 days
    let now = Timestamp::now().0;
    let graph = mem.engine().graph();
    let mut total = 0usize;
    let mut stale = 0usize;
    for id in graph.all_node_ids() {
        if let Ok(node) = graph.get_node(id) {
            total += 1;
            if now.saturating_sub(node.accessed_at.0) > STALE_MS {
                stale += 1;
            }
        }
    }
    (total, stale)
}

pub(crate) fn verify_embedding_dim(
    mem: &Memory<SqliteStorage>,
    provider_dim: usize,
    model: &str,
) -> Result<(), Error> {
    for id in mem.engine().graph().all_node_ids() {
        let node = mem.engine().graph().get_node(id)?;
        let Some(embedding) = node.embedding.as_ref() else {
            continue;
        };
        let db_dim = embedding.len();
        if db_dim != provider_dim {
            return Err(Error::InvalidInput(
                EmbeddingMigrationError::Incompatible(NamespaceCompatibility::DimensionMismatch {
                    stored_model: mem.engine().graph().storage().embedding_model_name()?,
                    db_dimensions: vec![Some(db_dim)],
                    target_model: model.to_string(),
                    target_dimensions: provider_dim,
                })
                .render(),
            ));
        }
        return Ok(());
    }
    Ok(())
}

fn inspect_namespace_compatibility(
    db_path: &std::path::Path,
    provider: &dyn EmbeddingProvider,
) -> Result<NamespaceCompatibility, Error> {
    let inspection = SqliteStorage::inspect_embedding_migration(db_path)?;
    let target_model = provider.model_name().to_string();
    let target_dimensions = provider.dimensions();
    if inspection.checkpoint.is_some() {
        return Ok(NamespaceCompatibility::IncompleteMigration {
            target_model,
            target_dimensions,
        });
    }
    if inspection
        .embedding_dimensions
        .iter()
        .any(|dimension| *dimension != Some(target_dimensions))
    {
        return Ok(NamespaceCompatibility::DimensionMismatch {
            stored_model: inspection.embedding_model,
            db_dimensions: inspection.embedding_dimensions,
            target_model,
            target_dimensions,
        });
    }
    match inspection.embedding_model {
        Some(stored_model) if stored_model != target_model => {
            Ok(NamespaceCompatibility::ModelMismatch {
                stored_model,
                target_model,
                target_dimensions,
            })
        }
        Some(_) | None => Ok(NamespaceCompatibility::Ready),
    }
}

pub(crate) fn inspect_pending_embedding_compatibility(
    pending: &PendingEmbeddingMigrationRequest,
) -> Result<NamespaceCompatibility, Error> {
    inspect_namespace_compatibility(&pending.db_path, pending.provider.as_ref())
}

fn verify_embedding_compatibility(
    mem: &mut Memory<SqliteStorage>,
    provider_dim: usize,
    current_model: &str,
) -> Result<(), Error> {
    verify_embedding_dim(mem, provider_dim, current_model)?;

    match mem.engine().graph().storage().embedding_model_name()? {
        Some(stored_model) if stored_model == current_model => Ok(()),
        Some(stored_model) => Err(Error::InvalidInput(
            EmbeddingMigrationError::Incompatible(NamespaceCompatibility::ModelMismatch {
                stored_model,
                target_model: current_model.to_string(),
                target_dimensions: provider_dim,
            })
            .render(),
        )),
        None => mem
            .engine_mut()
            .graph_mut()
            .storage_mut()
            .set_embedding_model_name(current_model),
    }
}

/// Render the `usage_report` text from registry-wide counters plus a
/// namespace's `(total, stale)` scan. Shared by [`MemoryRegistry::usage_report`]
/// and `crate::dispatch`'s `Stats` phase-3 commit so both render byte-identical
/// output.
///
/// `extraction_backlog` is the caller-resolved namespace's own queue length
/// (see [`MemoryRegistry::canonical_ns_key`]) — the extraction backlog line is
/// per-namespace, not a single count summed across every namespace.
pub(crate) fn format_usage_report(
    ops: &OpCounters,
    extraction_backlog: usize,
    seen_turn_keys_len: usize,
    total: usize,
    stale: usize,
) -> String {
    let stale_ratio = if total == 0 {
        0.0
    } else {
        stale as f64 / total as f64
    };
    format!(
        "usage (this daemon):\n  recalls: {} ({} reinforcing)\n  remembers: {}\n  relates: {}\n  captured turns: {}\n  extraction pulls: {}\nfailures (this daemon):\n  dispatch errors: {} ({} ingest)\n  empty recalls: {}\ncapture:\n  extraction backlog: {}\n  captured total: {}\n  stale ratio (14d): {:.2}",
        ops.recalls,
        ops.reinforcing_recalls,
        ops.remembers,
        ops.relates,
        ops.captured_turns,
        ops.extraction_pulls,
        ops.dispatch_errors,
        ops.ingest_errors,
        ops.empty_recalls,
        extraction_backlog,
        seen_turn_keys_len,
        stale_ratio,
    )
}

/// Pre-lock (registry-state-only) dedup filter shared by
/// [`MemoryRegistry::ingest_conversation`] and `crate::dispatch`'s `Ingest`
/// phase-1: decide which turns are new against `seen_turn_keys` BEFORE any
/// `Memory` access, so the global lock never overlaps the per-namespace one.
pub(crate) struct IngestDecision {
    pub(crate) speaker: String,
    pub(crate) text: String,
    pub(crate) at: Timestamp,
    pub(crate) key: Option<String>,
    pub(crate) scope: ScopePath,
}

pub(crate) fn filter_capture_decisions(
    seen_turn_keys: &HashSet<String>,
    session: &str,
    turns: &[Turn],
    capture: bool,
    scope: ScopePath,
) -> Vec<IngestDecision> {
    let base = Timestamp::now().0;
    turns
        .iter()
        .enumerate()
        .filter_map(|(i, turn)| {
            let at = Timestamp(turn.at_ms.unwrap_or(base + i as u64));
            let key =
                capture.then(|| crate::capture::turn_key(session, &turn.speaker, &turn.text, at.0));
            // Dedup gate: skip turns already seen in this capture stream.
            if let Some(k) = &key
                && seen_turn_keys.contains(k)
            {
                return None; // deduplicated
            }
            Some(IngestDecision {
                speaker: turn.speaker.clone(),
                text: turn.text.clone(),
                at,
                key,
                scope: scope.clone(),
            })
        })
        .collect()
}

/// Outcome of the namespace-locked phase of `ingest_conversation`: the
/// registry-state updates (`seen_turn_keys`/`unextracted`) a caller must
/// commit, plus the overall result. `committed` is populated up to (but not
/// including) the first `set_metadata_pairs` failure, and is returned
/// regardless of whether `outcome` is `Ok` or `Err` — mirroring the pre-split
/// code's interleaved per-turn commit (each turn's registry-state update
/// happened immediately after its own successful durable write, not gated on
/// the whole batch succeeding).
pub(crate) struct IngestPhase2 {
    pub(crate) committed: Vec<(NodeId, String)>,
    pub(crate) outcome: Result<IngestSummary, Error>,
}

/// Namespace-locked body of [`MemoryRegistry::ingest_conversation`]: given the
/// already dedup-filtered `decisions` (from [`filter_capture_decisions`]),
/// add each turn, flush the session, and durably stamp the capture metadata.
pub(crate) fn mem_ingest_conversation(
    mem: &mut Memory<SqliteStorage>,
    session: &str,
    decisions: Vec<IngestDecision>,
) -> IngestPhase2 {
    use crate::capture::{META_CAPTURE, META_EXTRACTED, META_TURN_KEY};

    let mut episodic = 0usize;
    let mut semantic = 0usize;
    let mut newly_captured: Vec<(NodeId, String)> = Vec::new();
    let mut capture_nodes: Vec<NodeId> = Vec::new();

    for decision in decisions {
        let receipt = match mem.add_in_scope(
            session,
            &decision.speaker,
            &decision.text,
            decision.at,
            decision.scope,
        ) {
            Ok(r) => r,
            Err(e) => {
                return IngestPhase2 {
                    committed: Vec::new(),
                    outcome: Err(e),
                };
            }
        };
        episodic += 1;
        if let Some(semantic_id) = receipt.finalized_semantic {
            semantic += 1;
            if decision.key.is_some() {
                capture_nodes.push(semantic_id);
            }
        }
        if let Some(k) = decision.key {
            capture_nodes.push(receipt.episodic);
            newly_captured.push((receipt.episodic, k));
        }
    }

    match mem.flush_session(session) {
        Ok(Some(semantic_id)) => {
            semantic += 1;
            if !newly_captured.is_empty() {
                capture_nodes.push(semantic_id);
            }
        }
        Ok(None) => {}
        Err(e) => {
            return IngestPhase2 {
                committed: Vec::new(),
                outcome: Err(e),
            };
        }
    }

    for id in capture_nodes {
        if let Err(e) = mem.set_metadata(id, META_CAPTURE, "true") {
            return IngestPhase2 {
                committed: Vec::new(),
                outcome: Err(e),
            };
        }
    }

    // Stamp captured turns (after flush so nodes are durable). Both keys go in
    // ONE durable write (`set_metadata_pairs`): a turn can never end up
    // deduped (turn_key present) but invisible to the extraction queue
    // (extracted missing) via a partial failure.
    let mut committed = Vec::with_capacity(newly_captured.len());
    for (epi_id, key) in newly_captured {
        if let Err(e) =
            mem.set_metadata_pairs(epi_id, &[(META_TURN_KEY, &key), (META_EXTRACTED, "false")])
        {
            return IngestPhase2 {
                committed,
                outcome: Err(e),
            };
        }
        committed.push((epi_id, key));
    }

    IngestPhase2 {
        committed,
        outcome: Ok(IngestSummary { episodic, semantic }),
    }
}

// Test-only counter of internal `Memory::tick` invocations made by `recall` /
// `recall_packaged_gated`. Lets tests assert the single-tick-per-recall
// invariant (flagship bug #2: MCP `recall` ticked the engine TWICE per call,
// doubling idle-edge leak/decay pressure on every read) directly, rather than
// inferring it from a decay/leak side effect that the per-edge leak checkpoint
// fix (`anamnesis::Engine::tick`) makes idempotent across near-simultaneous
// ticks — and therefore unobservable that way.
#[cfg(test)]
thread_local! {
    static TICK_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_tick() {
    TICK_CALLS.with(|c| c.set(c.get() + 1));
}

/// Deterministic 64-dim embedding provider for in-process tests — no network or
/// model download.
#[cfg(test)]
pub(crate) struct StubProvider;

#[cfg(test)]
impl EmbeddingProvider for StubProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts
            .iter()
            .map(|t| {
                let len = t.len() as f32;
                (0..64)
                    .map(|i| ((len * (i as f32 + 1.0)) % 100.0) / 100.0)
                    .collect()
            })
            .collect())
    }
    fn dimensions(&self) -> usize {
        64
    }
    fn model_name(&self) -> &str {
        "stub-64"
    }
}

/// Collapse duplicate-text hits (the Episodic + Semantic copy `add_note` creates),
/// keeping the first (highest-scored, since `hits` is already rank-ordered)
/// occurrence of each distinct text.
fn dedup_hits(hits: Vec<Hit>) -> Vec<Hit> {
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter(|h| seen.insert(h.text.clone()))
        .collect()
}
