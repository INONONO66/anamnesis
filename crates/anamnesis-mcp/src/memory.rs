//! Namespace-aware wrapper over `anamnesis::Memory`.
//!
//! Owns one `Memory<SqliteStorage>` per namespace, all sharing a single
//! embedding provider that is built lazily on first use. All access is
//! single-threaded by construction (the server holds this behind a Mutex).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::{KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::memory::{Hit, ListFilter, MemoryStats, MemoryView, NoteOptions, Relation};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};

use crate::capture::{
    META_EXTRACTED, META_TURN_KEY, extract_redelivery_ms, scan_extraction_state, turn_key,
};

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

/// Map an agent-facing relation label to the curated [`Relation`] vocabulary.
///
/// Accepts the canonical names (case-insensitive, `-`/`_`/space-insensitive).
/// An unrecognized label is **not** silently coerced to `Custom`: it returns a
/// clear error listing the accepted relations, so an agent typo surfaces instead
/// of quietly authoring a `Custom("typo")` edge. Use an explicit `custom:<label>`
/// prefix to author a consumer-defined relation on purpose.
pub fn parse_relation(label: &str) -> Result<Relation, Error> {
    let norm = label.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    if let Some(custom) = norm.strip_prefix("custom:") {
        let custom = custom.trim();
        if custom.is_empty() {
            return Err(Error::InvalidInput(
                "relation \"custom:\" requires a non-empty label (e.g. \"custom:blocks\")"
                    .to_string(),
            ));
        }
        // Preserve the caller's original (untrimmed-of-case) custom label after
        // the prefix, rather than the normalized form, so labels are faithful.
        let original = label.trim();
        let original_custom = original[original.find(':').map(|i| i + 1).unwrap_or(0)..].trim();
        return Ok(Relation::Custom(original_custom.to_string()));
    }
    let relation = match norm.as_str() {
        "causes" | "causal" => Relation::Causes,
        "contradicts" => Relation::Contradicts,
        "supports" => Relation::Supports,
        "refutes" => Relation::Refutes,
        "reason" => Relation::Reason,
        "rejected-alternative" | "rejectedalternative" => Relation::RejectedAlternative,
        "belongs-to" | "belongsto" => Relation::BelongsTo,
        "related" | "semantic" => Relation::Related,
        "supersedes" | "supersede" => Relation::Supersedes,
        _ => {
            return Err(Error::InvalidInput(format!(
                "unknown relation {label:?}; expected one of: causes, contradicts, supports, \
                 refutes, reason, rejected-alternative, belongs-to, related, supersedes (or \
                 \"custom:<label>\")"
            )));
        }
    };
    Ok(relation)
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

/// Build [`NoteOptions`] for `remember` from the wire-level tags/metadata/scope.
///
/// Empty-string tags are dropped rather than stored (adversarial input, not a
/// caller error). An invalid `scope` string (e.g. empty) surfaces
/// [`ScopePath::new`]'s error so `dispatch` can map it to `invalid_params`.
pub(crate) fn build_note_options(
    tags: Option<Vec<String>>,
    metadata: Option<HashMap<String, String>>,
    scope: Option<String>,
) -> Result<NoteOptions, Error> {
    let tags = tags
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    let metadata = metadata.unwrap_or_default().into_iter().collect();
    let scope = scope.map(ScopePath::new).transpose()?;
    Ok(NoteOptions {
        scope,
        tags,
        metadata,
    })
}

/// Parse a `list` metadata filter's `"key=value"` wire format into a
/// `(key, value)` pair. Splits on the first `=`; a missing `=` or an empty
/// key is a caller error.
pub(crate) fn parse_metadata_filter(raw: &str) -> Result<(String, String), Error> {
    let (key, value) = raw.split_once('=').ok_or_else(|| {
        Error::InvalidInput(format!(
            "malformed metadata filter {raw:?}; expected \"key=value\""
        ))
    })?;
    if key.is_empty() {
        return Err(Error::InvalidInput(format!(
            "malformed metadata filter {raw:?}; key must not be empty"
        )));
    }
    Ok((key.to_string(), value.to_string()))
}

/// How the shared embedding provider is obtained.
enum ProviderSource {
    /// Pre-built provider (tests, or a caller-supplied embedder).
    #[cfg(test)]
    Ready(Arc<dyn EmbeddingProvider>),
    /// Build the FastEmbed (bge-base-en-v1.5) provider on first use.
    FastEmbedLazy,
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
        Self {
            provider: None,
            source: ProviderSource::FastEmbedLazy,
            backend: Backend::File {
                default_db,
                dir,
                default_namespace: default_namespace.clone(),
            },
            reinforce_on_recall,
            open: HashMap::new(),
            default_namespace,
            locks: Vec::new(),
            lock_on_open: true,
            seen_turn_keys: HashSet::new(),
            unextracted: HashMap::new(),
            ops: OpCounters::default(),
        }
    }

    /// Daemon constructor: file-backed, FastEmbed provider built lazily, but the
    /// registry does **not** take per-namespace locks.
    ///
    /// The daemon already holds the single exclusive lock on the resolved DB (via
    /// [`crate::daemon::acquire_daemon`]) and is the sole process that opens it,
    /// so re-locking inside the registry is both redundant and would deadlock the
    /// default namespace against the daemon's own held lock.
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

    /// Test/embeddable constructor: in-memory graphs + a caller-supplied provider.
    #[cfg(test)]
    pub fn in_memory_with(provider: Arc<dyn EmbeddingProvider>, reinforce_on_recall: bool) -> Self {
        Self {
            provider: Some(provider.clone()),
            source: ProviderSource::Ready(provider),
            backend: Backend::Memory,
            reinforce_on_recall,
            open: HashMap::new(),
            default_namespace: "default".to_string(),
            locks: Vec::new(),
            lock_on_open: true,
            seen_turn_keys: HashSet::new(),
            unextracted: HashMap::new(),
            ops: OpCounters::default(),
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
            default_namespace,
            locks: Vec::new(),
            lock_on_open: true,
            seen_turn_keys: HashSet::new(),
            unextracted: HashMap::new(),
            ops: OpCounters::default(),
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
            ProviderSource::FastEmbedLazy => {
                Arc::new(anamnesis::embedding::fastembed::FastEmbedProvider::new()?)
            }
        };
        self.provider = Some(p.clone());
        Ok(p)
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

    fn open_namespace(&mut self, ns: &str) -> Result<Memory<SqliteStorage>, Error> {
        let provider = self.provider()?;
        // Resolve where this namespace lives WITHOUT holding a borrow of
        // `self.backend`, so we can push to `self.locks` below. `None` = the
        // in-memory test backend.
        let path: Option<PathBuf> = match &self.backend {
            #[cfg(test)]
            Backend::Memory => None,
            Backend::File {
                default_db,
                dir,
                default_namespace,
            } => {
                if ns == default_namespace {
                    Some(default_db.clone())
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
                    Some(path)
                }
            }
        };

        let Some(path) = path else {
            return Memory::in_memory_with_provider(provider);
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::StorageError(format!("create db dir: {e}")))?;
        }

        // The daemon already holds the single exclusive lock on the resolved DB
        // and is the sole opener, so it skips the per-namespace lock entirely.
        // Every other path takes an exclusive lock on a sibling `<db>.lock` so a
        // second anamnesis-mcp process can't open the same database (two
        // processes => two in-memory caches over one file => corruption). The OS
        // releases the lock when this process exits or is killed.
        if self.lock_on_open {
            let mut lock_path = path.clone().into_os_string();
            lock_path.push(".lock");
            let lock_path = PathBuf::from(lock_path);
            let lock_file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .map_err(|e| Error::StorageError(format!("open lock file {lock_path:?}: {e}")))?;
            // UFCS: on rustc >= 1.89 `File` has an inherent `try_lock` (different
            // signature) that would shadow the trait method; pin to fs4's so the
            // behavior is identical on our 1.88 MSRV and on newer toolchains.
            if fs4::FileExt::try_lock(&lock_file).is_err() {
                return Err(Error::StorageError(format!(
                    "database {path:?} is already in use by another anamnesis process; \
                     use a different ANAMNESIS_DB or namespace"
                )));
            }
            self.locks.push(lock_file);
        }

        let mem = Memory::with_provider(path, provider)?;
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

    /// Search; on success optionally auto-commit (reinforce) the returned package.
    /// A single lazy `tick(now)` after the search keeps forgetting current
    /// without a background thread and persists the reinforcement.
    ///
    /// Returns the raw de-duplicated [`Hit`] list. The CLI/server paths use
    /// [`recall_packaged`](Self::recall_packaged) (which also renders the context
    /// block), so in a non-test build this primitive has only test consumers.
    ///
    /// Ticks the engine exactly ONCE (flagship bug #2): an earlier revision also
    /// ticked before the search for same-call ranking freshness, but `tick` is
    /// not a no-op to call twice per recall — idle-edge leakage and node decay
    /// both key off elapsed time since the last tick, so a second tick a few
    /// milliseconds later doubled decay/leak pressure on every single read (and
    /// this method already runs on every recall, so the doubling compounded
    /// per-call, not just per session). One tick per recall restores
    /// call-frequency independence; the trade-off is that ranking for THIS
    /// call's own `search` uses decay as of the previous tick, not this instant.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn recall(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<Vec<Hit>, Error> {
        let reinforce = self.reinforce_on_recall;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        // `seed_limit` tracks `limit` inside `search`, and the RWR is noisier with
        // more seeds, so do NOT oversample to refill the top-k — that measurably
        // hurts ranking. Instead search `limit`, then collapse the Episodic+Semantic
        // copies `add_note` creates. This collapse alone lifts insight Recall@5 from
        // 0.375 to 0.94 (see src/eval.rs); the trade-off is that a heavily-duplicated
        // result can return fewer than `limit` distinct hits.
        let recall = mem.search(query, limit)?;
        let raw = recall.hits.clone();
        if reinforce {
            mem.used(recall)?;
        }
        // `Engine::commit` does not flush storage, so this `tick` persists any
        // reinforcement to SQLite (without it a CLI one-shot `recall`, or
        // `serve`'s last recall before shutdown, would lose it) and advances the
        // decay clock the NEXT recall's `search` will rank against.
        mem.tick(Timestamp::now())?;
        #[cfg(test)]
        record_tick();
        Ok(dedup_hits(raw))
    }

    /// Like [`recall`](Self::recall), but also returns the readable context block
    /// rendered from the assembled package (`Recall::as_context`).
    ///
    /// The `context` string is the primary, human-readable `recall` payload; the
    /// `hits` carry the same de-duplicated ranked list so the agent can pass
    /// `node_id`s on to `relate`. Reinforcement / tick semantics are identical to
    /// [`recall`](Self::recall).
    pub fn recall_packaged(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<PackagedRecall, Error> {
        // The classic path: reinforce per the registry default, no gate.
        self.recall_packaged_gated(query, limit, ns, None, None)
    }

    /// Gated, optionally read-only variant of [`recall_packaged`](Self::recall_packaged)
    /// for the Claude Code hook path.
    ///
    /// - `reinforce`: `None` ⇒ use the registry's configured default; `Some(false)`
    ///   ⇒ a pure read (skip the reinforcing `used()` commit); `Some(true)` ⇒ force
    ///   reinforcement.
    /// - `gate`: the need-odds threshold `τ`. After ranking, if there are no hits OR
    ///   the top hit's score is `< τ`, return an **empty** [`PackagedRecall`] (empty
    ///   `context`, empty `hits`) so the caller injects nothing. `None` ⇒ no gate.
    ///
    /// Tick semantics match [`recall`](Self::recall): exactly ONE tick per call
    /// (see its doc for why not two), after the search, on every branch
    /// (gated-out or not) — durability of any reinforcement (or of the gated-out
    /// read) never depends on how the call resolved. When the gate trips, the
    /// read is pure (never reinforces) regardless of `reinforce`, since there is
    /// nothing relevant to mark as used.
    pub fn recall_packaged_gated(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
        reinforce: Option<bool>,
        gate: Option<f64>,
    ) -> Result<PackagedRecall, Error> {
        // Count every recall; a recall is "reinforcing" per the SAME resolution
        // the method uses below (`reinforce.unwrap_or(self.reinforce_on_recall)`).
        // Counted on intent, before the gate can turn a would-be reinforce into a
        // pure read — the metric tracks how the caller asked to recall.
        self.ops.recalls += 1;
        if reinforce == Some(true) || (reinforce.is_none() && self.reinforce_on_recall) {
            self.ops.reinforcing_recalls += 1;
        }
        let reinforce = reinforce.unwrap_or(self.reinforce_on_recall);
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_recall_packaged_gated(&mut mem, query, limit, reinforce, gate)
    }

    /// Author a typed reasoning-chain edge between two existing nodes.
    ///
    /// `relation` is parsed via [`parse_relation`] (unknown labels error clearly).
    /// The node ids typically come from a prior `recall`. Returns the new edge id.
    pub fn relate(
        &mut self,
        from_id: u64,
        to_id: u64,
        relation: &str,
        ns: Option<&str>,
    ) -> Result<u64, Error> {
        self.ops.relates += 1;
        let relation = parse_relation(relation)?;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_relate(&mut mem, from_id, to_id, relation)
    }

    /// Read-only health/size snapshot for a namespace (`Memory::stats`).
    ///
    /// Flushes pending buffers first so the counts reflect live state.
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

    /// Store one distilled insight (`add_note`). Returns the episodic node id.
    pub fn remember(&mut self, text: &str, ns: Option<&str>) -> Result<u64, Error> {
        self.ops.remembers += 1;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_remember(&mut mem, text)
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
        let decisions = filter_capture_decisions(&self.seen_turn_keys, session, turns, capture);
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

    /// Replace a node's content and re-embed it. `pub(crate)` [`mem_update`]
    /// does the namespace-locked work; `crate::dispatch` calls it directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn update(&mut self, id: u64, new_content: &str, ns: Option<&str>) -> Result<(), Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_update(&mut mem, NodeId(id), new_content)
    }

    /// Soft- (`hard = false`) or hard-delete (`hard = true`, irreversible) a node.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn forget(
        &mut self,
        id: u64,
        reason: &str,
        hard: bool,
        ns: Option<&str>,
    ) -> Result<(), Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        if hard {
            mem_forget_hard(&mut mem, NodeId(id))
        } else {
            mem_forget(&mut mem, NodeId(id), reason)
        }
    }

    /// Mark `new_id` as superseding `old_id`. Returns the new edge id.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn supersede(&mut self, new_id: u64, old_id: u64, ns: Option<&str>) -> Result<u64, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_supersede(&mut mem, NodeId(new_id), NodeId(old_id))
    }

    /// List nodes matching `filter`, ordered by salience (highest first).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn list(
        &mut self,
        filter: &ListFilter,
        ns: Option<&str>,
    ) -> Result<Vec<MemoryView>, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_list(&mut mem, filter)
    }

    /// Read a single node as a [`MemoryView`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get(&mut self, id: u64, ns: Option<&str>) -> Result<MemoryView, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_get(&mut mem, NodeId(id))
    }

    /// Force the embedding provider to build (download the model). For `prewarm`.
    pub fn prewarm(&mut self) -> Result<(), Error> {
        let provider = self.provider()?;
        provider.embed_single("warm")?;
        Ok(())
    }
}

// ── Namespace-locked primitives (phase-2 work) ───────────────────────────────
//
// Each function below operates on an already-resolved `&mut Memory` — no
// registry access, no global lock. `crate::dispatch` calls these directly
// between acquiring and releasing a namespace's `Mutex`, and the
// `MemoryRegistry` convenience methods above call the SAME functions after
// locking their own resolved handle, so the two call paths can never diverge.

/// Namespace-locked body of [`MemoryRegistry::recall_packaged_gated`].
pub(crate) fn mem_recall_packaged_gated(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    limit: usize,
    reinforce: bool,
    gate: Option<f64>,
) -> Result<PackagedRecall, Error> {
    let recall = mem.search(query, limit)?;

    // Gate on the top readout score (hits are rank-ordered, highest first).
    // No hits ⇒ below any threshold. Below `τ` ⇒ inject nothing.
    let gated_out = match (gate, recall.hits.first().map(|h| h.score)) {
        (Some(tau), Some(top)) => top < tau,
        (Some(_), None) => true,
        (None, _) => false,
    };
    if gated_out {
        // Pure read, no reinforcement, nothing to inject. Still tick once for
        // durability / decay-clock advancement (single tick per call).
        mem.tick(Timestamp::now())?;
        #[cfg(test)]
        record_tick();
        return Ok(PackagedRecall {
            context: String::new(),
            hits: Vec::new(),
        });
    }

    // Render the context block from the package BEFORE `used` consumes it.
    let context = recall.as_context();
    let raw = recall.hits.clone();
    if reinforce {
        mem.used(recall)?;
    }
    // Single tick per call (see `MemoryRegistry::recall` for why not two).
    mem.tick(Timestamp::now())?;
    #[cfg(test)]
    record_tick();
    let hits = dedup_hits(raw);
    Ok(PackagedRecall { context, hits })
}

/// Like [`mem_recall_packaged_gated`], with a post-filter dropping hits whose
/// node origin scope or entity tags don't match. Applied to the already
/// gated/ranked/de-duplicated `hits` list — the readable `context` block is
/// left as rendered (simplest correct approach; does not rewrite the
/// activation pipeline).
pub(crate) fn mem_recall_packaged_gated_filtered(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    limit: usize,
    reinforce: bool,
    gate: Option<f64>,
    scope: Option<&str>,
    tag: Option<&str>,
) -> Result<PackagedRecall, Error> {
    let mut packaged = mem_recall_packaged_gated(mem, query, limit, reinforce, gate)?;
    if scope.is_some() || tag.is_some() {
        packaged.hits.retain(|h| {
            let Ok(node) = mem.engine().graph().get_node(h.node_id) else {
                return false;
            };
            let scope_ok = scope.is_none_or(|s| node.origin.scope.as_str() == s);
            let tag_ok = tag.is_none_or(|t| node.entity_tags.iter().any(|et| et == t));
            scope_ok && tag_ok
        });
    }
    Ok(packaged)
}

/// Namespace-locked body of [`MemoryRegistry::relate`].
pub(crate) fn mem_relate(
    mem: &mut Memory<SqliteStorage>,
    from_id: u64,
    to_id: u64,
    relation: Relation,
) -> Result<u64, Error> {
    // Flush so a just-added turn (its semantic still buffered) is a valid
    // endpoint, mirroring `search`.
    mem.flush_all()?;
    let edge = mem.relate(NodeId(from_id), NodeId(to_id), relation)?;
    mem.flush_all()?;
    Ok(edge.0)
}

/// Namespace-locked body of [`MemoryRegistry::remember`].
pub(crate) fn mem_remember(mem: &mut Memory<SqliteStorage>, text: &str) -> Result<u64, Error> {
    mem_remember_with(mem, text, NoteOptions::default())
}

/// Like [`mem_remember`], with scope/tags/metadata routed through
/// [`Memory::add_note_with`].
pub(crate) fn mem_remember_with(
    mem: &mut Memory<SqliteStorage>,
    text: &str,
    opts: NoteOptions,
) -> Result<u64, Error> {
    let receipt = mem.add_note_with(text, Timestamp::now(), opts)?;
    mem.flush_all()?;
    Ok(receipt.episodic.0)
}

/// Namespace-locked body of [`MemoryRegistry::update`].
pub(crate) fn mem_update(
    mem: &mut Memory<SqliteStorage>,
    id: NodeId,
    new_content: &str,
) -> Result<(), Error> {
    // Flush so a just-added node (its semantic still buffered) is a valid
    // target, mirroring `mem_relate`.
    mem.flush_all()?;
    mem.update_content(id, new_content, Timestamp::now())?;
    mem.flush_all()?;
    Ok(())
}

/// Namespace-locked body of [`MemoryRegistry::get`].
pub(crate) fn mem_get(mem: &mut Memory<SqliteStorage>, id: NodeId) -> Result<MemoryView, Error> {
    mem.flush_all()?;
    mem.get(id)
}

/// Namespace-locked body of [`MemoryRegistry::list`].
pub(crate) fn mem_list(
    mem: &mut Memory<SqliteStorage>,
    filter: &ListFilter,
) -> Result<Vec<MemoryView>, Error> {
    mem.flush_all()?;
    mem.list(filter)
}

/// Namespace-locked soft-delete body of [`MemoryRegistry::forget`].
pub(crate) fn mem_forget(
    mem: &mut Memory<SqliteStorage>,
    id: NodeId,
    reason: &str,
) -> Result<(), Error> {
    mem.flush_all()?;
    mem.forget(id, reason, Timestamp::now())?;
    mem.flush_all()?;
    Ok(())
}

/// Namespace-locked hard-delete body of [`MemoryRegistry::forget`] (`hard = true`).
pub(crate) fn mem_forget_hard(mem: &mut Memory<SqliteStorage>, id: NodeId) -> Result<(), Error> {
    mem.flush_all()?;
    mem.delete_hard(id)?;
    mem.flush_all()?;
    Ok(())
}

/// Namespace-locked body of [`MemoryRegistry::supersede`]. Returns the new
/// `Supersedes` edge id.
pub(crate) fn mem_supersede(
    mem: &mut Memory<SqliteStorage>,
    new_id: NodeId,
    old_id: NodeId,
) -> Result<u64, Error> {
    mem.flush_all()?;
    let edge = mem.supersede(new_id, old_id)?;
    mem.flush_all()?;
    Ok(edge.0)
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
pub(crate) fn filter_capture_decisions(
    seen_turn_keys: &HashSet<String>,
    session: &str,
    turns: &[Turn],
    capture: bool,
) -> Vec<(String, String, Timestamp, Option<String>)> {
    let base = Timestamp::now().0;
    turns
        .iter()
        .enumerate()
        .filter_map(|(i, turn)| {
            let at = Timestamp(turn.at_ms.unwrap_or(base + i as u64));
            let key = capture.then(|| turn_key(session, &turn.speaker, &turn.text, at.0));
            // Dedup gate: skip turns already seen in this capture stream.
            if let Some(k) = &key
                && seen_turn_keys.contains(k)
            {
                return None; // deduplicated
            }
            Some((turn.speaker.clone(), turn.text.clone(), at, key))
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
    decisions: Vec<(String, String, Timestamp, Option<String>)>,
) -> IngestPhase2 {
    let mut episodic = 0usize;
    let mut semantic = 0usize;
    let mut newly_captured: Vec<(NodeId, String)> = Vec::new();

    for (speaker, text, at, key) in decisions {
        let receipt = match mem.add(session, &speaker, &text, at) {
            Ok(r) => r,
            Err(e) => {
                return IngestPhase2 {
                    committed: Vec::new(),
                    outcome: Err(e),
                };
            }
        };
        episodic += 1;
        if receipt.finalized_semantic.is_some() {
            semantic += 1;
        }
        if let Some(k) = key {
            newly_captured.push((receipt.episodic, k));
        }
    }

    match mem.flush_session(session) {
        Ok(Some(_)) => semantic += 1,
        Ok(None) => {}
        Err(e) => {
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

/// Deterministic 64-dim embedding provider for tests — no network, no model
/// download. Shared by this module's tests and the daemon lifecycle test.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(reinforce: bool) -> MemoryRegistry {
        MemoryRegistry::in_memory_with(Arc::new(StubProvider), reinforce)
    }

    // ── Single-tick-per-recall (flagship bug #2) ────────────────────────────

    #[test]
    fn recall_ticks_engine_exactly_once() {
        let mut reg = registry(true);
        reg.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let before = TICK_CALLS.with(|c| c.get());
        reg.recall("auth race condition", 5, None).unwrap();
        assert_eq!(
            TICK_CALLS.with(|c| c.get()) - before,
            1,
            "recall must tick the engine exactly once, not twice"
        );
    }

    #[test]
    fn recall_packaged_gated_ticks_engine_exactly_once() {
        let mut reg = registry(true);
        reg.remember("the cache key omitted the lockfile hash", None)
            .unwrap();
        let before = TICK_CALLS.with(|c| c.get());
        reg.recall_packaged_gated("cache key lockfile", 5, None, None, None)
            .unwrap();
        assert_eq!(
            TICK_CALLS.with(|c| c.get()) - before,
            1,
            "recall_packaged_gated must tick the engine exactly once, not twice"
        );
    }

    #[test]
    fn recall_packaged_gated_gated_out_still_ticks_exactly_once() {
        let mut reg = registry(true);
        reg.remember("unrelated note", None).unwrap();
        let before = TICK_CALLS.with(|c| c.get());
        // An impossibly high gate threshold forces the gated-out early-return
        // branch, which has its own tick call site.
        reg.recall_packaged_gated("unrelated note", 5, None, None, Some(1_000.0))
            .unwrap();
        assert_eq!(
            TICK_CALLS.with(|c| c.get()) - before,
            1,
            "the gated-out branch must also tick the engine exactly once"
        );
    }

    #[test]
    fn second_registry_on_same_db_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        let mut a = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            db.clone(),
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        a.remember("first writer holds the lock", None).unwrap();

        let mut b = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            db,
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        let err = b.remember("second writer must be rejected", None);
        assert!(
            err.is_err(),
            "a second registry on the same DB file must be rejected by the lock"
        );
    }

    #[test]
    fn remember_then_recall_returns_a_hit() {
        let mut reg = registry(true);
        // `remember` returns the episodic node id; `.unwrap()` already proves it
        // stored without error (any u64 is a valid id).
        let _id = reg
            .remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let hits = reg.recall("auth race condition", 5, None).unwrap();
        assert!(!hits.is_empty(), "expected at least one hit after remember");
    }

    #[test]
    fn recall_collapses_duplicate_text_nodes() {
        let mut reg = registry(false);
        // One note = an Episodic + a Semantic node with identical text. recall must
        // return that text once, not twice.
        reg.remember("the cache key omitted the lockfile hash", None)
            .unwrap();
        let hits = reg.recall("cache key lockfile", 5, None).unwrap();
        let mut texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
        texts.sort_unstable();
        let unique = {
            let mut t = texts.clone();
            t.dedup();
            t.len()
        };
        assert_eq!(
            texts.len(),
            unique,
            "recall returned duplicate-text hits: {texts:?}"
        );
    }

    #[test]
    fn ingest_conversation_counts_turns() {
        let mut reg = registry(true);
        let turns = vec![
            Turn {
                speaker: "alice".into(),
                text: "we picked postgres".into(),
                at_ms: None,
            },
            Turn {
                speaker: "bob".into(),
                text: "because of jsonb".into(),
                at_ms: None,
            },
            Turn {
                speaker: "alice".into(),
                text: "and row-level security".into(),
                at_ms: None,
            },
        ];
        let summary = reg
            .ingest_conversation("design-chat", &turns, None, false)
            .unwrap();
        assert_eq!(summary.episodic, 3);
        assert!(summary.semantic >= 1);
    }

    #[test]
    fn namespaces_are_isolated() {
        let mut reg = registry(true);
        reg.remember("alpha-only secret", Some("alpha")).unwrap();
        let beta_hits = reg.recall("alpha-only secret", 5, Some("beta")).unwrap();
        assert!(
            beta_hits.is_empty(),
            "namespace beta must not see alpha's memory"
        );
    }

    #[test]
    fn sanitize_blocks_path_traversal() {
        assert_eq!(
            MemoryRegistry::sanitize("../../etc/passwd"),
            "------etc-passwd"
        );
        assert_eq!(MemoryRegistry::sanitize(""), "default");
        assert_eq!(MemoryRegistry::sanitize("Work Project:1"), "work-project-1");
    }

    /// A non-default namespace whose sanitized stem equals the default DB file
    /// stem must be rejected, not silently aliased onto the default file.
    #[test]
    fn namespace_colliding_with_default_db_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let default_db = dir.path().join("memory.db");
        let mut reg = MemoryRegistry::file_backed(
            default_db,
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        reg.provider = Some(Arc::new(StubProvider));
        // ns "memory" sanitizes to "memory" → <dir>/memory.db == default_db.
        let err = reg.remember("leak attempt", Some("memory")).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    }

    /// Raw namespaces that sanitize to the same stem must collapse to ONE
    /// instance over ONE file, not two instances racing over the same file.
    #[test]
    fn sanitize_equal_namespaces_share_one_instance() {
        let dir = tempfile::tempdir().unwrap();
        let default_db = dir.path().join("memory.db");
        let mut reg = MemoryRegistry::file_backed(
            default_db,
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        reg.provider = Some(Arc::new(StubProvider));
        reg.remember("shared via Alpha", Some("Alpha")).unwrap();
        // "alpha" sanitizes to the same stem as "Alpha"; it must see the write.
        let hits = reg.recall("shared via Alpha", 5, Some("alpha")).unwrap();
        assert!(
            !hits.is_empty(),
            "alpha must see Alpha's write (same canonical namespace)"
        );
        // Exactly one open instance for both raw spellings.
        assert_eq!(reg.open.len(), 1, "Alpha and alpha must share one instance");
    }

    // ── parse_relation ────────────────────────────────────────────────────────

    #[test]
    fn parse_relation_canonical_and_aliases() {
        use anamnesis::memory::Relation;
        assert_eq!(parse_relation("causes").unwrap(), Relation::Causes);
        assert_eq!(parse_relation("CAUSAL").unwrap(), Relation::Causes);
        assert_eq!(
            parse_relation("contradicts").unwrap(),
            Relation::Contradicts
        );
        assert_eq!(parse_relation("supports").unwrap(), Relation::Supports);
        assert_eq!(parse_relation("refutes").unwrap(), Relation::Refutes);
        assert_eq!(parse_relation("reason").unwrap(), Relation::Reason);
        assert_eq!(
            parse_relation("rejected-alternative").unwrap(),
            Relation::RejectedAlternative
        );
        // space/underscore are normalized to `-`.
        assert_eq!(
            parse_relation("Rejected Alternative").unwrap(),
            Relation::RejectedAlternative
        );
        assert_eq!(parse_relation("belongs_to").unwrap(), Relation::BelongsTo);
        assert_eq!(parse_relation("related").unwrap(), Relation::Related);
        assert_eq!(parse_relation("semantic").unwrap(), Relation::Related);
    }

    #[test]
    fn parse_relation_accepts_supersedes() {
        assert_eq!(parse_relation("supersedes").unwrap(), Relation::Supersedes);
        assert_eq!(parse_relation("supersede").unwrap(), Relation::Supersedes);
        assert_eq!(parse_relation("SUPERSEDES").unwrap(), Relation::Supersedes);
    }

    #[test]
    fn parse_relation_custom_preserves_label() {
        use anamnesis::memory::Relation;
        // Custom label keeps its original case after the `custom:` prefix.
        assert_eq!(
            parse_relation("custom:Blocks").unwrap(),
            Relation::Custom("Blocks".to_string())
        );
        assert_eq!(
            parse_relation("  custom:depends-on ").unwrap(),
            Relation::Custom("depends-on".to_string())
        );
    }

    #[test]
    fn parse_relation_rejects_unknown_and_empty_custom() {
        let err = parse_relation("frobnicate").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
        let err = parse_relation("custom:").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    }

    // ── management API (registry-level convenience wrappers) ───────────────────

    #[test]
    fn registry_update_edits_content() {
        let mut reg = registry(false);
        let id = reg.remember("the deploy script is bash", None).unwrap();
        reg.update(id, "the deploy script is python", None).unwrap();
        let view = reg.get(id, None).unwrap();
        assert_eq!(view.content, "the deploy script is python");
    }

    #[test]
    fn registry_forget_soft_then_hard() {
        let mut reg = registry(false);
        let id = reg.remember("a stale credential note", None).unwrap();
        reg.forget(id, "rotated", false, None).unwrap();
        let view = reg.get(id, None).unwrap();
        assert!(view.retracted, "soft-forgotten node must show retracted");

        reg.forget(id, "", true, None).unwrap();
        assert!(
            reg.get(id, None).is_err(),
            "hard-forgotten node must no longer be readable"
        );
    }

    #[test]
    fn registry_supersede_sets_validity_window() {
        let mut reg = registry(false);
        let old = reg.remember("we use postgres", None).unwrap();
        let new = reg.remember("we use sqlite", None).unwrap();
        reg.supersede(new, old, None).unwrap();
        let old_view = reg.get(old, None).unwrap();
        assert!(old_view.valid_until.is_some());
    }

    #[test]
    fn registry_list_orders_by_salience_and_filters() {
        let mut reg = registry(false);
        reg.remember("a note about apples", None).unwrap();
        let filter = ListFilter {
            min_salience: 0.0,
            limit: 10,
            node_type: None,
            tag: None,
            scope: None,
            metadata: None,
        };
        let views = reg.list(&filter, None).unwrap();
        assert!(!views.is_empty());
        for w in views.windows(2) {
            assert!(w[0].salience >= w[1].salience);
        }
    }

    // ── relate ────────────────────────────────────────────────────────────────

    #[test]
    fn relate_links_two_remembered_nodes() {
        let mut reg = registry(false);
        let a = reg.remember("the deploy failed", None).unwrap();
        let b = reg.remember("the disk was full", None).unwrap();
        // b causes a. Returns a valid (non-panicking) edge id.
        let _edge = reg.relate(b, a, "causes", None).unwrap();
        // A contradiction edge must show up in stats.
        let _edge2 = reg.relate(a, b, "contradicts", None).unwrap();
        let stats = reg.stats(None).unwrap();
        assert!(
            stats.contradiction_count >= 1,
            "expected a contradiction edge, got {}",
            stats.contradiction_count
        );
    }

    #[test]
    fn relate_unknown_relation_errors() {
        let mut reg = registry(false);
        let a = reg.remember("x", None).unwrap();
        let b = reg.remember("y", None).unwrap();
        let err = reg.relate(a, b, "not-a-relation", None).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    }

    #[test]
    fn relate_missing_endpoint_errors() {
        let mut reg = registry(false);
        let a = reg.remember("only node", None).unwrap();
        // u64::MAX is not a real node id.
        let result = reg.relate(a, u64::MAX, "related", None);
        assert!(
            result.is_err(),
            "linking to a missing node must error: {result:?}"
        );
    }

    // ── recall_packaged ───────────────────────────────────────────────────────

    #[test]
    fn recall_packaged_returns_context_and_dedup_hits() {
        let mut reg = registry(true);
        reg.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let packaged = reg.recall_packaged("auth race condition", 5, None).unwrap();
        // hits are de-duplicated (the Episodic+Semantic copies collapse).
        let mut texts: Vec<&str> = packaged.hits.iter().map(|h| h.text.as_str()).collect();
        texts.sort_unstable();
        let mut unique = texts.clone();
        unique.dedup();
        assert_eq!(
            texts.len(),
            unique.len(),
            "packaged hits had duplicate text"
        );
        // Context is a string (may be empty if nothing packaged, but with a hit it
        // should carry a section header).
        if !packaged.hits.is_empty() {
            assert!(
                packaged.context.contains("##"),
                "expected a section header in context:\n{}",
                packaged.context
            );
        }
    }

    // ── recall_packaged_gated (the hook recall path) ───────────────────────────

    /// A gate `τ` above the top hit's score ⇒ empty context AND empty hits
    /// (the hook injects nothing).
    #[test]
    fn gated_recall_below_threshold_is_empty() {
        let mut reg = registry(false);
        reg.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        // First read the true top score with no gate, then set τ just above it.
        let ungated = reg
            .recall_packaged_gated("auth race condition", 5, None, Some(false), None)
            .unwrap();
        let top = ungated
            .hits
            .first()
            .map(|h| h.score)
            .expect("a relevant hit exists");
        let tau = top + 1.0; // strictly above the best score ⇒ gate trips.

        let gated = reg
            .recall_packaged_gated("auth race condition", 5, None, Some(false), Some(tau))
            .unwrap();
        assert!(
            gated.context.is_empty(),
            "above-τ gate must yield empty context, got:\n{}",
            gated.context
        );
        assert!(
            gated.hits.is_empty(),
            "above-τ gate must yield no hits, got {} hits",
            gated.hits.len()
        );
    }

    /// No hits at all ⇒ gated out (treated as below any threshold).
    #[test]
    fn gated_recall_with_no_hits_is_empty() {
        let mut reg = registry(false);
        // Empty graph: nothing to retrieve, so any gate (even 0.0) yields empty.
        let gated = reg
            .recall_packaged_gated("nothing here", 5, None, Some(false), Some(0.0))
            .unwrap();
        assert!(gated.context.is_empty());
        assert!(gated.hits.is_empty());
    }

    /// A gate `τ` at/below the top score ⇒ the rendered top-k context block.
    #[test]
    fn gated_recall_at_or_above_threshold_renders_top_k() {
        let mut reg = registry(false);
        reg.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        // τ = 0.0 admits every positive-scored hit.
        let gated = reg
            .recall_packaged_gated("auth race condition", 5, None, Some(false), Some(0.0))
            .unwrap();
        assert!(!gated.hits.is_empty(), "τ=0.0 must admit the relevant hit");
        assert!(
            gated.context.contains("##"),
            "expected a rendered section header, got:\n{}",
            gated.context
        );
    }

    /// `gate = None` means no gating: the rendered block comes back even with a
    /// huge would-be threshold, exactly as the classic `recall_packaged`.
    #[test]
    fn gated_recall_none_gate_never_filters() {
        let mut reg = registry(false);
        reg.remember("postgres was chosen for jsonb", None).unwrap();
        let gated = reg
            .recall_packaged_gated("postgres jsonb", 5, None, Some(false), None)
            .unwrap();
        assert!(!gated.hits.is_empty());
        assert!(gated.context.contains("##"));
    }

    /// `reinforce = false` is a pure read: repeated reads never lift base-level
    /// salience (it only decays under the ticks), while `reinforce = true` does
    /// lift it via the `used()` commit.
    #[test]
    fn read_only_recall_does_not_reinforce_but_reinforcing_does() {
        // Read-only: salience must not climb across repeated reads.
        let mut ro = registry(false);
        ro.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let ro_before = ro.stats(None).unwrap().avg_salience;
        for _ in 0..3 {
            let pkg = ro
                .recall_packaged_gated("auth race condition", 5, None, Some(false), None)
                .unwrap();
            assert!(
                !pkg.hits.is_empty(),
                "each read should still return the hit"
            );
        }
        let ro_after = ro.stats(None).unwrap().avg_salience;
        assert!(
            ro_after <= ro_before,
            "read-only recall must not increase salience: {ro_before} -> {ro_after}"
        );

        // Reinforcing: salience should climb under the same reads.
        let mut rw = registry(false);
        rw.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let rw_before = rw.stats(None).unwrap().avg_salience;
        for _ in 0..3 {
            rw.recall_packaged_gated("auth race condition", 5, None, Some(true), None)
                .unwrap();
        }
        let rw_after = rw.stats(None).unwrap().avg_salience;
        assert!(
            rw_after > rw_before,
            "reinforcing recall must increase salience: {rw_before} -> {rw_after}"
        );
    }

    /// A gated read-out that trips `τ` is a pure read regardless of `reinforce`:
    /// nothing relevant ⇒ nothing reinforced.
    #[test]
    fn gated_out_recall_never_reinforces_even_when_asked() {
        let mut reg = registry(false);
        reg.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let before = reg.stats(None).unwrap().avg_salience;
        // τ astronomically high ⇒ always gated out, even with reinforce=true.
        for _ in 0..3 {
            let pkg = reg
                .recall_packaged_gated("auth race condition", 5, None, Some(true), Some(1e9))
                .unwrap();
            assert!(pkg.hits.is_empty(), "gate must trip at τ=1e9");
        }
        let after = reg.stats(None).unwrap().avg_salience;
        assert!(
            after <= before,
            "a gated-out recall must not reinforce: {before} -> {after}"
        );
    }

    /// `recall_packaged` (the classic entry) still behaves exactly as before:
    /// it delegates to the gated method with the registry's reinforce default
    /// and no gate. With `reinforce_on_recall = true` it lifts salience.
    #[test]
    fn recall_packaged_preserves_classic_reinforcing_behavior() {
        let mut reg = registry(true); // reinforce_on_recall = true
        reg.remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let before = reg.stats(None).unwrap().avg_salience;
        for _ in 0..3 {
            let pkg = reg.recall_packaged("auth race condition", 5, None).unwrap();
            assert!(!pkg.hits.is_empty());
            assert!(pkg.context.contains("##"));
        }
        let after = reg.stats(None).unwrap().avg_salience;
        assert!(
            after > before,
            "classic recall_packaged with reinforce default on must lift salience: {before} -> {after}"
        );
    }

    // ── stats ─────────────────────────────────────────────────────────────────

    #[test]
    fn stats_counts_remembered_nodes() {
        let mut reg = registry(false);
        let empty = reg.stats(None).unwrap();
        assert_eq!(empty.node_count, 0);
        reg.remember("one fact", None).unwrap();
        reg.remember("another fact", None).unwrap();
        let s = reg.stats(None).unwrap();
        // Each `remember` is an Episodic + Semantic node (2 per note).
        assert!(
            s.node_count >= 4,
            "expected >= 4 nodes, got {}",
            s.node_count
        );
    }

    // ── usage_report (dogfood metrics) ─────────────────────────────────────────

    #[test]
    fn usage_report_counts_ops_and_backlog() {
        let mut reg = registry(true);
        // 1 remember, 1 relate, 2 recalls (1 reinforcing), 1 captured turn, 1 pull.
        let a = reg.remember("the deploy failed", None).unwrap();
        let b = reg.remember("the disk was full", None).unwrap();
        reg.relate(b, a, "causes", None).unwrap();
        let _ = reg
            .recall_packaged_gated("deploy", 5, None, Some(false), None)
            .unwrap();
        let _ = reg
            .recall_packaged_gated("deploy", 5, None, Some(true), None)
            .unwrap();
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "capture me".into(),
            at_ms: Some(1),
        }];
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        let _ = reg.pull_pending(Some(10), None).unwrap();

        let report = reg.usage_report(None).unwrap();
        assert!(
            report.contains("recalls: 2 (1 reinforcing)"),
            "got: {report}"
        );
        assert!(report.contains("remembers: 2"), "got: {report}");
        assert!(report.contains("relates: 1"), "got: {report}");
        assert!(report.contains("captured turns: 1"), "got: {report}");
        assert!(report.contains("extraction pulls: 1"), "got: {report}");
        assert!(
            report.contains("extraction backlog: 0"),
            "drained: {report}"
        );
        assert!(report.contains("captured total: 1"), "got: {report}");
        assert!(report.contains("stale ratio"), "got: {report}");
    }
}
