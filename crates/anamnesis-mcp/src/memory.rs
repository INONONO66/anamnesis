//! Namespace-aware wrapper over `anamnesis::Memory`.
//!
//! Owns one `Memory<SqliteStorage>` per namespace, all sharing a single
//! embedding provider that is built lazily on first use. All access is
//! single-threaded by construction (the server holds this behind a Mutex).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::{NodeId, Timestamp};
use anamnesis::memory::{Hit, MemoryStats, Relation};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};

/// Metadata key: the stable dedup hash stored on each captured episodic node.
const META_TURN_KEY: &str = "anamnesis:turn_key";
/// Metadata key: whether a captured episodic node has been reasoning-extracted.
const META_EXTRACTED: &str = "anamnesis:extracted";

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
        _ => {
            return Err(Error::InvalidInput(format!(
                "unknown relation {label:?}; expected one of: causes, contradicts, supports, \
                 refutes, reason, rejected-alternative, belongs-to, related (or \"custom:<label>\")"
            )));
        }
    };
    Ok(relation)
}

/// Stable dedup key for a captured turn: lowercase hex Sha256 of
/// `session \0 speaker \0 text \0 at_ms`. Sha256 (not DefaultHasher) because the
/// key is persisted in `node.metadata` and re-derived after daemon restarts /
/// rebuilds — it must be identical across runs and toolchain versions.
pub(crate) fn turn_key(session: &str, speaker: &str, text: &str, at_ms: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(session.as_bytes());
    h.update([0u8]);
    h.update(speaker.as_bytes());
    h.update([0u8]);
    h.update(text.as_bytes());
    h.update([0u8]);
    h.update(at_ms.to_le_bytes());
    format!("{:x}", h.finalize())
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

pub struct MemoryRegistry {
    provider: Option<Arc<dyn EmbeddingProvider>>,
    source: ProviderSource,
    backend: Backend,
    reinforce_on_recall: bool,
    open: HashMap<String, Memory<SqliteStorage>>,
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
    seen_turn_keys: HashSet<String>,
    /// Episodic node ids captured but not yet reasoning-extracted (the queue).
    unextracted: Vec<NodeId>,
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
            unextracted: Vec::new(),
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
            unextracted: Vec::new(),
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
            unextracted: Vec::new(),
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

    fn get(&mut self, ns: Option<&str>) -> Result<&mut Memory<SqliteStorage>, Error> {
        let raw = self.resolve_namespace(ns).to_string();
        let key = self.canonical_key(&raw);
        if !self.open.contains_key(&key) {
            let mem = self.open_namespace(&key)?;
            self.open.insert(key.clone(), mem);
        }
        Ok(self.open.get_mut(&key).expect("just inserted"))
    }

    /// Flush every open namespace's pending state to disk. Called on graceful
    /// shutdown (SIGTERM), where `process::exit` would otherwise skip `Drop`.
    pub fn flush_all_open(&mut self) -> Result<(), Error> {
        for mem in self.open.values_mut() {
            mem.flush_all()?;
        }
        Ok(())
    }

    /// Search; on success optionally auto-commit (reinforce) the returned package.
    /// A lazy `tick(now)` keeps forgetting current without a background thread and
    /// persists the reinforcement.
    ///
    /// Returns the raw de-duplicated [`Hit`] list. The CLI/server paths use
    /// [`recall_packaged`](Self::recall_packaged) (which also renders the context
    /// block), so in a non-test build this primitive has only test consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn recall(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<Vec<Hit>, Error> {
        let reinforce = self.reinforce_on_recall;
        let mem = self.get(ns)?;
        // Tick BEFORE searching so a cold recall after a long idle gap ranks on
        // current decay, not stale cached salience: `search` reads the cached
        // `salience` projection, which is only refreshed by a write or `tick`.
        mem.tick(Timestamp::now())?;
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
        // Tick again AFTER the reinforcing commit: `Engine::commit` does not flush
        // storage, so this `tick` persists the reinforcement to SQLite (without it
        // a CLI one-shot `recall`, or `serve`'s last recall before shutdown, would
        // lose it). The pre-search tick handles ranking freshness; this one handles
        // durability.
        mem.tick(Timestamp::now())?;
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
    /// Tick semantics match [`recall_packaged`](Self::recall_packaged): a tick before
    /// the search (ranking freshness) and one after (durability of any reinforcement).
    /// When the gate trips, the read is pure (never reinforces) regardless of
    /// `reinforce`, since there is nothing relevant to mark as used.
    pub fn recall_packaged_gated(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
        reinforce: Option<bool>,
        gate: Option<f64>,
    ) -> Result<PackagedRecall, Error> {
        let reinforce = reinforce.unwrap_or(self.reinforce_on_recall);
        let mem = self.get(ns)?;
        // Tick BEFORE searching so a cold recall after a long idle gap ranks on
        // current decay (same rationale as `recall`).
        mem.tick(Timestamp::now())?;
        let recall = mem.search(query, limit)?;

        // Gate on the top readout score (hits are rank-ordered, highest first).
        // No hits ⇒ below any threshold. Below `τ` ⇒ inject nothing.
        let gated_out = match (gate, recall.hits.first().map(|h| h.score)) {
            (Some(tau), Some(top)) => top < tau,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if gated_out {
            // Pure read, no reinforcement, nothing to inject. Still tick for
            // durability of the pre-search decay refresh.
            mem.tick(Timestamp::now())?;
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
        // Persist the reinforcement (see `recall` for why a post-commit tick).
        mem.tick(Timestamp::now())?;
        let hits = dedup_hits(raw);
        Ok(PackagedRecall { context, hits })
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
        let relation = parse_relation(relation)?;
        let mem = self.get(ns)?;
        // Flush so a just-added turn (its semantic still buffered) is a valid
        // endpoint, mirroring `consolidate`/`search`.
        mem.flush_all()?;
        let edge = mem.relate(NodeId(from_id), NodeId(to_id), relation)?;
        mem.flush_all()?;
        Ok(edge.0)
    }

    /// Read-only health/size snapshot for a namespace (`Memory::stats`).
    ///
    /// Flushes pending buffers first so the counts reflect live state.
    pub fn stats(&mut self, ns: Option<&str>) -> Result<MemoryStats, Error> {
        let mem = self.get(ns)?;
        mem.flush_all()?;
        mem.stats()
    }

    /// Store one distilled insight (`add_note`). Returns the episodic node id.
    pub fn remember(&mut self, text: &str, ns: Option<&str>) -> Result<u64, Error> {
        let mem = self.get(ns)?;
        let receipt = mem.add_note(text, Timestamp::now())?;
        mem.flush_all()?;
        Ok(receipt.episodic.0)
    }

    /// Ingest a batch of conversational turns via the bench windowing recipe.
    ///
    /// When `capture` is `true`, each turn is deduplicated by a stable content hash
    /// (`seen_turn_keys`), stamped with `anamnesis:turn_key` and `anamnesis:extracted`
    /// metadata, and enqueued in `unextracted` for downstream reasoning extraction.
    /// When `capture` is `false`, the path is unchanged from the pre-capture behaviour.
    pub fn ingest_conversation(
        &mut self,
        session: &str,
        turns: &[Turn],
        ns: Option<&str>,
        capture: bool,
    ) -> Result<IngestSummary, Error> {
        let base = Timestamp::now().0;
        let mut episodic = 0usize;
        let mut semantic = 0usize;
        // Collect (speaker, text, at, Option<key>) decisions BEFORE any mem borrow,
        // so the dedup gate (which reads self.seen_turn_keys) and the mem borrow
        // do not overlap — the borrow checker would reject the interleaved pattern.
        let decisions: Vec<(String, String, Timestamp, Option<String>)> = turns
            .iter()
            .enumerate()
            .filter_map(|(i, turn)| {
                let at = Timestamp(turn.at_ms.unwrap_or(base + i as u64));
                let key = capture.then(|| turn_key(session, &turn.speaker, &turn.text, at.0));
                // Dedup gate: skip turns already seen in this capture stream.
                if let Some(k) = &key {
                    if self.seen_turn_keys.contains(k) {
                        return None; // deduplicated
                    }
                }
                Some((turn.speaker.clone(), turn.text.clone(), at, key))
            })
            .collect();

        // Now do all mem.add calls (borrows &mut Memory from self).
        let mut newly_captured: Vec<(NodeId, String)> = Vec::new();
        for (speaker, text, at, key) in decisions {
            let mem = self.get(ns)?;
            let receipt = mem.add(session, &speaker, &text, at)?;
            episodic += 1;
            if receipt.finalized_semantic.is_some() {
                semantic += 1;
            }
            if let Some(k) = key {
                newly_captured.push((receipt.episodic, k));
            }
        }

        if self.get(ns)?.flush_session(session)?.is_some() {
            semantic += 1;
        }

        // Stamp + enqueue captured turns (after flush so nodes are durable).
        for (epi_id, key) in newly_captured {
            let mem = self.get(ns)?;
            mem.set_metadata(epi_id, META_TURN_KEY, &key)?;
            mem.set_metadata(epi_id, META_EXTRACTED, "false")?;
            self.seen_turn_keys.insert(key);
            self.unextracted.push(epi_id);
        }

        Ok(IngestSummary { episodic, semantic })
    }

    /// Drain up to `limit` un-extracted turns, optimistically marking each
    /// `extracted=true` (fail-open: raw survives even if the agent never emits).
    /// Returns a JSON array `[{"node_id":N,"content":"..."}]` for the agent.
    pub fn pull_pending(&mut self, limit: Option<usize>, ns: Option<&str>) -> Result<String, Error> {
        let take = limit.unwrap_or(self.unextracted.len()).min(self.unextracted.len());
        let ids: Vec<NodeId> = self.unextracted.drain(0..take).collect();
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            let mem = self.get(ns)?;
            let content = mem.engine().graph().get_node(id).map(|n| n.content.clone()).unwrap_or_default();
            mem.set_metadata(id, META_EXTRACTED, "true")?;
            items.push(serde_json::json!({ "node_id": id.0, "content": content }));
        }
        Ok(serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string()))
    }

    /// Pending-queue size + the configured threshold (read by the hook signal).
    pub fn extraction_status(&mut self, _ns: Option<&str>) -> Result<String, Error> {
        let threshold: usize = std::env::var("ANAMNESIS_EXTRACT_THRESHOLD_N")
            .ok().and_then(|v| v.trim().parse().ok()).unwrap_or(20);
        Ok(serde_json::json!({ "pending": self.unextracted.len(), "threshold": threshold }).to_string())
    }

    /// Number of captured episodic nodes awaiting reasoning extraction.
    /// Test accessor — production code uses the field directly.
    #[cfg(test)]
    pub(crate) fn unextracted_len(&self) -> usize {
        self.unextracted.len()
    }

    /// Rebuild the capture indexes (`seen_turn_keys`, `unextracted`) from node
    /// metadata. Called once at daemon startup so the queue + dedup survive
    /// restarts. Idempotent: clears then repopulates.
    pub fn load_extraction_state(&mut self, ns: Option<&str>) -> Result<(), Error> {
        self.seen_turn_keys.clear();
        self.unextracted.clear();
        let mem = self.get(ns)?;
        let graph = mem.engine().graph();
        let mut keys = Vec::new();
        let mut pending = Vec::new();
        for id in graph.all_node_ids() {
            if let Ok(node) = graph.get_node(id) {
                if let Some(k) = node.metadata.get(META_TURN_KEY) {
                    keys.push(k.clone());
                }
                if node.metadata.get(META_EXTRACTED).map(String::as_str) == Some("false") {
                    pending.push(id);
                }
            }
        }
        self.seen_turn_keys.extend(keys);
        self.unextracted = pending;
        Ok(())
    }

    /// Force the embedding provider to build (download the model). For `prewarm`.
    pub fn prewarm(&mut self) -> Result<(), Error> {
        let provider = self.provider()?;
        provider.embed_single("warm")?;
        Ok(())
    }
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
        let dir = std::env::temp_dir().join("anamnesis-ns-collision-test");
        let default_db = dir.join("memory.db");
        let mut reg = MemoryRegistry::file_backed(default_db, dir, "default".to_string(), false);
        reg.provider = Some(Arc::new(StubProvider));
        // ns "memory" sanitizes to "memory" → <dir>/memory.db == default_db.
        let err = reg.remember("leak attempt", Some("memory")).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    }

    /// Raw namespaces that sanitize to the same stem must collapse to ONE
    /// instance over ONE file, not two instances racing over the same file.
    #[test]
    fn sanitize_equal_namespaces_share_one_instance() {
        let dir = std::env::temp_dir().join("anamnesis-ns-canonical-test");
        let _ = std::fs::remove_dir_all(&dir);
        let default_db = dir.join("memory.db");
        let mut reg = MemoryRegistry::file_backed(default_db, dir, "default".to_string(), false);
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

    #[test]
    fn turn_key_is_deterministic_and_field_sensitive() {
        let a = super::turn_key("s1", "user", "hello", 1000);
        let b = super::turn_key("s1", "user", "hello", 1000);
        assert_eq!(a, b, "same inputs ⇒ same key");
        assert_eq!(a.len(), 64, "sha256 hex is 64 chars");
        assert_ne!(a, super::turn_key("s1", "user", "hello", 1001), "at_ms matters");
        assert_ne!(a, super::turn_key("s1", "assistant", "hello", 1000), "speaker matters");
        assert_ne!(a, super::turn_key("s2", "user", "hello", 1000), "session matters");
    }

    #[test]
    fn pull_pending_returns_once_and_marks_extracted() {
        let mut reg = registry(true);
        let turns = vec![
            Turn { speaker: "user".into(), text: "why x".into(), at_ms: Some(10) },
            Turn { speaker: "assistant".into(), text: "because y".into(), at_ms: Some(20) },
        ];
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        assert_eq!(reg.unextracted_len(), 2);
        let json = reg.pull_pending(None, None).unwrap();
        assert!(json.contains("\"node_id\""), "got: {json}");
        assert!(json.contains("because y"), "content included: {json}");
        assert_eq!(reg.unextracted_len(), 0, "drained");
        // Second pull is empty (optimistic mark held).
        let json2 = reg.pull_pending(None, None).unwrap();
        assert_eq!(json2.trim(), "[]");
    }

    #[test]
    fn extraction_status_reports_pending_and_threshold() {
        let mut reg = registry(true);
        let turns = vec![Turn { speaker: "user".into(), text: "z".into(), at_ms: Some(99) }];
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        let s = reg.extraction_status(None).unwrap();
        assert!(s.contains("\"pending\":1"), "got: {s}");
        assert!(s.contains("\"threshold\":"), "got: {s}");
    }

    #[test]
    fn capture_dedups_identical_turns() {
        let mut reg = registry(true);
        let turns = vec![Turn { speaker: "user".into(), text: "ship it".into(), at_ms: Some(1000) }];
        let s1 = reg.ingest_conversation("sess", &turns, None, true).unwrap();
        assert_eq!(s1.episodic, 1, "first capture creates one episodic");
        // Same turn again (multi-hook) ⇒ deduped, no new node.
        let s2 = reg.ingest_conversation("sess", &turns, None, true).unwrap();
        assert_eq!(s2.episodic, 0, "identical turn is skipped");
    }

    #[test]
    fn capture_enqueues_unextracted_but_plain_ingest_does_not() {
        let mut reg = registry(true);
        let turns = vec![Turn { speaker: "user".into(), text: "a decision".into(), at_ms: Some(2000) }];
        reg.ingest_conversation("s", &turns, None, false).unwrap();
        assert_eq!(reg.unextracted_len(), 0, "non-capture ingest does not enqueue");
        let turns2 = vec![Turn { speaker: "user".into(), text: "another".into(), at_ms: Some(3000) }];
        reg.ingest_conversation("s", &turns2, None, true).unwrap();
        assert_eq!(reg.unextracted_len(), 1, "capture ingest enqueues");
    }

    #[test]
    fn extraction_state_rebuilds_from_metadata_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let turns = vec![Turn { speaker: "user".into(), text: "persist me".into(), at_ms: Some(5000) }];
        {
            let mut reg = MemoryRegistry::file_backed_with(
                Arc::new(StubProvider), db.clone(), dir.path().to_path_buf(), "default".into(), false);
            reg.ingest_conversation("s", &turns, None, true).unwrap();
            assert_eq!(reg.unextracted_len(), 1);
        } // drop → releases lock
        // Fresh registry on the same DB: index empty until loaded.
        let mut reg2 = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider), db.clone(), dir.path().to_path_buf(), "default".into(), false);
        assert_eq!(reg2.unextracted_len(), 0, "index empty before load");
        reg2.load_extraction_state(None).unwrap();
        assert_eq!(reg2.unextracted_len(), 1, "unextracted rebuilt from metadata");
        // And dedup still holds after reload (same turn is skipped).
        let s = reg2.ingest_conversation("s", &turns, None, true).unwrap();
        assert_eq!(s.episodic, 0, "turn_key reloaded ⇒ dedup holds across restart");
    }
}
