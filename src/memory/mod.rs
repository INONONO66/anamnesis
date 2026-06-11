//! Memory — the Framework API for Anamnesis.
//!
//! # Overview
//!
//! This module is the **validated consumer layer** of the Anamnesis crate. It
//! implements the bench-proven ingest recipe currently living in
//! `benches/eval_common/real_bench/graph.rs` and exposes it as the official
//! front door: `anamnesis::Memory`.
//!
//! # Recipe origin
//!
//! The encoding strategy (speaker-prefixed Episodic turn + ±1-window Semantic
//! view, `ExtractedFrom` and `Temporal` edges, session/speaker entity tags,
//! ingest-everything engine config) is the exact recipe validated by the
//! LoCoMo and LongMemEval benchmark harness. Consuming `Memory` gives you
//! those numbers out of the box.
//!
//! # Buffering semantics
//!
//! `Memory` is incremental — the "+1 future turn" of each window doesn't
//! exist yet at `add` time. The recipe is replicated exactly via
//! **one-turn buffering** per session:
//!
//! - `add(session, speaker, text, at)` ingests the Episodic node immediately
//!   and finalizes the *previous* turn's Semantic node (now that its `+1` is
//!   known). Temporal edges are wired as each turn arrives.
//! - `flush_session` / `flush_all` finalize the last buffered turn with window
//!   `(prev?, last)` — no `+1` to append.
//!
//! The resulting node set, content, timestamps and edges are **identical to the
//! batch recipe for uninterrupted sessions**. A flush/search boundary finalizes
//! the pending turn without its future neighbor (one-sided window), which is the
//! one unavoidable divergence from the batch recipe. Node-ID *ordering* may also
//! differ (semantics land one step later), which can flip retrieval ties broken
//! by node id.
//!
//! # Drop and explicit flush
//!
//! `Memory` implements `Drop`, which calls `flush_all()` in a best-effort
//! manner (errors are swallowed). For reliable error handling, call
//! `flush_all()` explicitly before dropping.
//!
//! # Escape hatch
//!
//! `engine()` / `engine_mut()` expose the underlying [`Engine`] directly. Below
//! this line the recipe's conventions (node types, edge topology, entity tags,
//! embedding approach) **do not apply** — you are responsible for correctness.
//! Mix framework calls and raw engine calls only if you know what you are doing.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::Engine;
use crate::api::{EngineConfig, IngestResult, Observation, TickReport};
use crate::embedding::EmbeddingProvider;
use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::types::PeerId;
use crate::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use crate::mechanics::social::ConfidenceLevel;
use crate::peer::SourceKind;
use crate::query::{ContextPackage, SearchInput};
use crate::storage::{SqliteStorage, StorageAdapter};

/// Per-session state for incremental window finalization.
#[derive(Debug, Default)]
struct SessionBuffer {
    /// The buffered turn waiting for its `+1` context (to build the Semantic window).
    pending: Option<PendingTurn>,
    /// 1-based turn index (incremented each `add`).
    turn_index: usize,
    /// The last episodic NodeId from this session (retained across flush boundaries
    /// to wire Temporal edges to the next `add`).
    last_episodic_id: Option<NodeId>,
    /// Speaker-prefixed text of the last finalized turn (retained across flush
    /// boundaries to include as `prev` context in the next turn's window).
    last_speaker_text: Option<String>,
}

/// A buffered turn waiting for the next turn (to complete its context window).
#[derive(Debug, Clone)]
struct PendingTurn {
    /// The episodic node already ingested for this turn.
    episodic_id: NodeId,
    /// Timestamp of this turn (Semantic node will carry this timestamp).
    at: Timestamp,
    /// Speaker-prefixed text of the previous turn (for window building), if any.
    prev_speaker_text: Option<String>,
    /// Speaker-prefixed text of this turn.
    speaker_text: String,
    /// Session id (for entity tags).
    session_id: String,
    /// Speaker (for entity tags / summary).
    speaker: String,
    /// 1-based turn index.
    turn_index: usize,
}

/// Receipt returned by [`Memory::add`] and [`Memory::add_note`].
///
/// Contains the episodic [`NodeId`] of the current turn and, when the
/// previous turn's Semantic node was finalized in the same call, its id.
#[derive(Debug, Clone)]
pub struct AddReceipt {
    /// Episodic node created for this turn.
    pub episodic: NodeId,
    /// Semantic node finalized for the *previous* buffered turn, if any.
    ///
    /// `None` when this was the first turn in a session (no prior turn to
    /// finalize) or when called via `add_note` (not applicable — `add_note`
    /// finalizes its own semantic and returns it here instead).
    pub finalized_semantic: Option<NodeId>,
}

/// The framework API — bench-proven ingest recipe with incremental window
/// finalization.
///
/// `Memory<S>` wraps an [`Engine<S>`] and manages per-session buffering so
/// that each `add` call produces the same graph topology as the batch benchmark
/// recipe. The default storage type is [`SqliteStorage`] (in-memory SQLite).
///
/// See the [module docs](self) for design and buffering semantics.
pub struct Memory<S: StorageAdapter + Clone = SqliteStorage> {
    engine: Engine<S>,
    provider: Arc<dyn EmbeddingProvider>,
    sessions: HashMap<String, SessionBuffer>,
}

// ── Engine config used by Memory (bench defaults) ────────────────────────────

fn memory_engine_config() -> EngineConfig {
    EngineConfig {
        dedup_enabled: false,
        novelty_threshold: 0.0,
        confidence_threshold: 0.0,
        ..EngineConfig::default()
    }
}

// ── Drop ─────────────────────────────────────────────────────────────────────

impl<S: StorageAdapter + Clone> Drop for Memory<S> {
    /// Best-effort flush of all pending session buffers on drop.
    ///
    /// Errors are swallowed. Call [`flush_all`](Memory::flush_all) explicitly
    /// before dropping if you need to observe errors.
    fn drop(&mut self) {
        let _ = self.flush_all();
    }
}

// ── Constructors — `embed` feature ───────────────────────────────────────────

impl Memory<SqliteStorage> {
    /// Open (or create) a file-backed `Memory` using the built-in FastEmbed
    /// provider (BAAI/bge-base-en-v1.5).
    ///
    /// Requires the `embed` feature flag.
    #[cfg(feature = "embed")]
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        use crate::embedding::fastembed::FastEmbedProvider;
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(FastEmbedProvider::new()?);
        Self::with_provider(path, provider)
    }

    /// Create an in-memory `Memory` using the built-in FastEmbed provider
    /// (BAAI/bge-base-en-v1.5).
    ///
    /// Requires the `embed` feature flag.
    #[cfg(feature = "embed")]
    pub fn in_memory() -> Result<Self, Error> {
        use crate::embedding::fastembed::FastEmbedProvider;
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(FastEmbedProvider::new()?);
        Self::in_memory_with_provider(provider)
    }

    /// Open (or create) a file-backed `Memory` using a caller-supplied
    /// embedding provider. No feature flag required.
    pub fn with_provider(
        path: impl AsRef<Path>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self, Error> {
        let storage = SqliteStorage::open(path)?;
        let engine = Engine::with_storage(memory_engine_config(), storage);
        Ok(Memory {
            engine,
            provider,
            sessions: HashMap::new(),
        })
    }

    /// Create an in-memory `Memory` using a caller-supplied embedding provider.
    /// No feature flag required.
    pub fn in_memory_with_provider(provider: Arc<dyn EmbeddingProvider>) -> Result<Self, Error> {
        let engine = Engine::with_config(memory_engine_config());
        Ok(Memory {
            engine,
            provider,
            sessions: HashMap::new(),
        })
    }
}

// ── Core API (generic over S) ─────────────────────────────────────────────────

impl<S: StorageAdapter + Clone> Memory<S> {
    /// Add a conversational turn using the bench recipe.
    ///
    /// Steps (per the incremental window finalization design):
    /// 1. Embed and ingest an `Episodic` node for `speaker: text`.
    /// 2. If a buffered turn `t(i-1)` exists, finalize its `Semantic` window
    ///    (now complete with `t(i)` as the `+1` context) and link
    ///    `ExtractedFrom`.
    /// 3. Wire a `Temporal` edge from `epi(i-1)` to `epi(i)`.
    /// 4. Buffer `t(i)` for the next call.
    ///
    /// Returns an [`AddReceipt`] with `episodic` = the new episodic node id and
    /// `finalized_semantic` = the previous turn's semantic node id (if any).
    ///
    /// # Buffering and the final turn
    ///
    /// The last buffered turn's Semantic view is written at the next `add`,
    /// `flush_session`, `flush_all`, `search`/`search_at`, or on `Drop`.
    /// Call [`flush_all`](Memory::flush_all) explicitly to observe any errors
    /// from that finalization (Drop swallows them).
    ///
    /// # Error safety
    ///
    /// If this method returns `Err`, the session buffer is left exactly as it
    /// was before the call — the previously-pending turn is never silently lost.
    /// `turn_index` is only incremented on success.
    pub fn add(
        &mut self,
        session: &str,
        speaker: &str,
        text: &str,
        at: Timestamp,
    ) -> Result<AddReceipt, Error> {
        let session_buf = self.sessions.entry(session.to_string()).or_default();

        // Snapshot continuity state BEFORE any mutation so we can use it
        // throughout without borrowing `session_buf` again mid-sequence.
        let pending_snapshot = session_buf.pending.clone();
        let next_turn_index = session_buf.turn_index + 1;
        let continuity_prev_epi = session_buf.last_episodic_id;

        let speaker_text = format!("{}: {}", speaker, text);

        // ── Phase A: all fallible work (NO buffer mutation yet) ──────────────

        // (a) Embed the current turn's episodic text.
        let epi_embedding = embed_one(&*self.provider, &speaker_text)?;

        // (b) If pending: build window and embed it. Both are fallible.
        let pending_window_result: Option<(String, Vec<f64>)> =
            if let Some(ref pending) = pending_snapshot {
                let window = build_window(
                    pending.prev_speaker_text.as_deref(),
                    &pending.speaker_text,
                    Some(&speaker_text),
                );
                let sem_embedding = embed_one(&*self.provider, &window)?;
                Some((window, sem_embedding))
            } else {
                None
            };

        // ── Phase B: ingest operations (also fallible, but ordered so buffer
        //    mutation only happens after all ingests succeed) ─────────────────

        // (c) Ingest current episodic node.
        let epi_id = ingest_node(
            &mut self.engine,
            &speaker_text,
            speaker_text.clone(),
            epi_embedding,
            KnowledgeType::Episodic,
            at,
            entity_tags_for(session, speaker),
            Some(format!("{} turn {}", speaker, next_turn_index)),
            session,
        )?;

        // (d) If pending: ingest its semantic, then wire ExtractedFrom + Temporal.
        let finalized_semantic = if let (Some(pending), Some((window, sem_embedding))) =
            (pending_snapshot, pending_window_result)
        {
            let sem_id = ingest_node(
                &mut self.engine,
                &window,
                window.clone(),
                sem_embedding,
                KnowledgeType::Semantic,
                pending.at,
                entity_tags_for(&pending.session_id, &pending.speaker),
                Some(format!("{} turn {}", pending.speaker, pending.turn_index)),
                &pending.session_id,
            )?;
            self.engine
                .link(sem_id, pending.episodic_id, EdgeType::ExtractedFrom)?;
            // Temporal: epi(i-1) → epi(i). Use pending.episodic_id as the prior
            // episodic (from the pending turn, not the continuity state, since a
            // pending turn means we are in the normal mid-session flow).
            self.engine
                .link(pending.episodic_id, epi_id, EdgeType::Temporal)?;
            Some((sem_id, pending.speaker_text))
        } else {
            // No pending in the buffer. But if we have a cross-flush continuity
            // episodic, wire its Temporal edge now.
            if let Some(prev_epi_id) = continuity_prev_epi {
                self.engine.link(prev_epi_id, epi_id, EdgeType::Temporal)?;
            }
            None
        };

        // ── Phase E: ALL fallible work done — now mutate buffer state ─────────

        let buf = self.sessions.get_mut(session)
            // SAFETY: we inserted the entry at the top of this function via
            // `or_default()`, so it is guaranteed to be present.
            .expect("session buffer must exist after or_default()");

        buf.turn_index = next_turn_index;

        // Update cross-flush continuity fields.
        buf.last_episodic_id = Some(epi_id);

        // The `prev_speaker_text` for the NEW pending is the finalized turn's
        // speaker_text (if we just finalized one), otherwise the retained
        // cross-flush prev (if any), otherwise None.
        let prev_for_new_pending = finalized_semantic
            .as_ref()
            .map(|(_, prev_text)| prev_text.clone())
            .or_else(|| buf.last_speaker_text.clone());

        buf.last_speaker_text = Some(speaker_text.clone());

        buf.pending = Some(PendingTurn {
            episodic_id: epi_id,
            at,
            prev_speaker_text: prev_for_new_pending,
            speaker_text: speaker_text.clone(),
            session_id: session.to_string(),
            speaker: speaker.to_string(),
            turn_index: next_turn_index,
        });

        Ok(AddReceipt {
            episodic: epi_id,
            finalized_semantic: finalized_semantic.map(|(sem_id, _)| sem_id),
        })
    }

    /// Single-shot note — its own session, window = itself, finalized immediately.
    ///
    /// Creates both an `Episodic` and `Semantic` node, linked with
    /// `ExtractedFrom`. The `Semantic` window contains only the note text
    /// (no context neighbors). Returns an `AddReceipt` with both ids set.
    pub fn add_note(&mut self, text: &str, at: Timestamp) -> Result<AddReceipt, Error> {
        let session_id = format!("note-{}", at.0);
        let speaker = "note";
        let speaker_text = text.to_string();

        let epi_embedding = embed_one(&*self.provider, &speaker_text)?;
        let epi_id = ingest_node(
            &mut self.engine,
            &speaker_text,
            speaker_text.clone(),
            epi_embedding,
            KnowledgeType::Episodic,
            at,
            entity_tags_for(&session_id, speaker),
            None,
            &session_id,
        )?;

        // Window = just itself (no prev, no next).
        let window = speaker_text.clone();
        let sem_embedding = embed_one(&*self.provider, &window)?;
        let sem_id = ingest_node(
            &mut self.engine,
            &window,
            window.clone(),
            sem_embedding,
            KnowledgeType::Semantic,
            at,
            entity_tags_for(&session_id, speaker),
            None,
            &session_id,
        )?;
        self.engine.link(sem_id, epi_id, EdgeType::ExtractedFrom)?;

        Ok(AddReceipt {
            episodic: epi_id,
            finalized_semantic: Some(sem_id),
        })
    }

    /// Finalize the last buffered turn for `session`.
    ///
    /// Writes the pending turn's Semantic node (one-sided window — no `+1`
    /// neighbor, because this is a flush boundary) and removes it from the
    /// per-session buffer. Continuity state (`last_episodic_id`,
    /// `last_speaker_text`) is retained so that a subsequent `add` on the
    /// same session still produces a Temporal edge and includes the prev-turn
    /// text in the new window.
    ///
    /// Returns the `NodeId` of the semantic node created for the last turn,
    /// or `None` if the session had no buffered turn (already flushed or
    /// never existed).
    ///
    /// # Note
    ///
    /// The final turn's semantic view is written here (or at `flush_all` /
    /// `search` / `Drop`). Call this explicitly before dropping if you need
    /// to observe errors — `Drop` swallows them.
    pub fn flush_session(&mut self, session: &str) -> Result<Option<NodeId>, Error> {
        let pending = match self.sessions.get_mut(session) {
            Some(buf) => buf.pending.take(),
            None => None,
        };
        let Some(pending) = pending else {
            return Ok(None);
        };
        let window = build_window(
            pending.prev_speaker_text.as_deref(),
            &pending.speaker_text,
            None,
        );
        let sem_embedding = embed_one(&*self.provider, &window)?;
        let sem_id = ingest_node(
            &mut self.engine,
            &window,
            window.clone(),
            sem_embedding,
            KnowledgeType::Semantic,
            pending.at,
            entity_tags_for(&pending.session_id, &pending.speaker),
            Some(format!("{} turn {}", pending.speaker, pending.turn_index)),
            &pending.session_id,
        )?;
        self.engine
            .link(sem_id, pending.episodic_id, EdgeType::ExtractedFrom)?;

        // Retain continuity state so the next `add` on this session can wire
        // a Temporal edge and include this turn's text as prev-window context.
        if let Some(buf) = self.sessions.get_mut(session) {
            buf.last_episodic_id = Some(pending.episodic_id);
            buf.last_speaker_text = Some(pending.speaker_text);
        }

        Ok(Some(sem_id))
    }

    /// Finalize all pending sessions.
    ///
    /// The final turn's semantic view for each session is written here.
    /// Call this explicitly before dropping if you need to observe errors —
    /// `Drop` swallows them.
    pub fn flush_all(&mut self) -> Result<(), Error> {
        let sessions: Vec<String> = self.sessions.keys().cloned().collect();
        for session in sessions {
            self.flush_session(&session)?;
        }
        Ok(())
    }

    /// Read-only access to the underlying [`Engine`].
    ///
    /// **Escape hatch** — below this line the recipe's conventions do not apply.
    pub fn engine(&self) -> &Engine<S> {
        &self.engine
    }

    /// Mutable access to the underlying [`Engine`].
    ///
    /// **Escape hatch** — below this line the recipe's conventions do not apply.
    pub fn engine_mut(&mut self) -> &mut Engine<S> {
        &mut self.engine
    }
}

// ── Search / recall / used / tick ─────────────────────────────────────────────

/// A single ranked memory hit from a [`Recall`].
///
/// Returned by [`Memory::search`] and [`Memory::search_at`] from the engine's
/// pre-packaging readout surface — the same surface the benchmarks measure.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Id of the retrieved node.
    pub node_id: NodeId,
    /// Full content of the node (L2).
    pub text: String,
    /// Readout score (ranking key; higher = more relevant).
    pub score: f64,
    /// Timestamp when the node was created.
    pub at: Timestamp,
    /// Normalized speaker extracted from the node's `speaker-<norm>` entity tag, if any.
    pub speaker: Option<String>,
    /// Normalized session extracted from the node's `session-<norm>` entity tag, if any.
    pub session: Option<String>,
}

/// Output of [`Memory::search`] / [`Memory::search_at`].
///
/// `hits` are the ranked results from the pre-packaging readout surface.
/// `package` is the assembled [`ContextPackage`] — pass it to [`Memory::used`]
/// when you actually use the results (commit-gated reinforcement).
#[derive(Debug, Clone)]
pub struct Recall {
    /// Top-`limit` hits ranked by readout score, highest first.
    pub hits: Vec<Hit>,
    /// Assembled context package — consume via [`Memory::used`] to reinforce.
    pub package: ContextPackage,
}

impl<S: StorageAdapter + Clone> Memory<S> {
    /// Search memory at wall-clock `now`.
    ///
    /// Equivalent to `search_at(query, limit, Timestamp::now())`. For deterministic
    /// or time-travel queries use [`search_at`](Memory::search_at) instead.
    pub fn search(&mut self, query: &str, limit: usize) -> Result<Recall, Error> {
        self.search_at(query, limit, Timestamp::now())
    }

    /// Search memory at an explicit `now` timestamp.
    ///
    /// First flushes all pending session buffers so that every previously added
    /// turn is searchable (even the last unfinalized one). Then embeds the query,
    /// runs the bench-default `SearchInput` through the engine, and maps the
    /// `trace.readout` top-`limit` candidates to [`Hit`]s.
    ///
    /// The [`Recall`] contains both the ranked hits and the assembled
    /// [`ContextPackage`]; pass the `Recall` to [`used`](Memory::used) when the
    /// results are actually consumed.
    pub fn search_at(
        &mut self,
        query: &str,
        limit: usize,
        now: Timestamp,
    ) -> Result<Recall, Error> {
        // Flush pending buffers so the last buffered turn is searchable.
        self.flush_all()?;

        // Embed the query via the provider.
        let embedding = embed_one(&*self.provider, query)?;

        // Build bench-default SearchInput: text + query_embedding + limit +
        // seed_limit = Some(limit.max(1)); speaker cues OFF; now = explicit.
        let input = SearchInput {
            text: query.to_string(),
            query_embedding: Some(embedding),
            limit,
            seed_limit: Some(limit.max(1)),
            now,
            entity_tags: Vec::new(), // speaker cues OFF (bench default)
            ..SearchInput::default()
        };

        let result = self.engine.search(input)?;

        // Map trace.readout top-limit to Hits. Skip entries whose node lookup fails.
        let hits: Vec<Hit> = result
            .trace
            .readout
            .iter()
            .take(limit)
            .filter_map(|candidate| {
                let node = self.engine.graph().get_node(candidate.node_id).ok()?;
                let (speaker, session) = parse_entity_tags(&node.entity_tags);
                Some(Hit {
                    node_id: candidate.node_id,
                    text: node.content.clone(),
                    score: candidate.score,
                    at: node.created_at,
                    speaker,
                    session,
                })
            })
            .collect();

        Ok(Recall {
            hits,
            package: result.package,
        })
    }

    /// Commit a [`Recall`]'s context package with [`ConfidenceLevel::Medium`] reinforcement.
    ///
    /// Call this **only** for results you actually used — reinforcement is
    /// commit-gated. Calling `used` strengthens the accessed nodes' retained-action
    /// reservoirs, making them more salient in future retrievals.
    ///
    /// Note: reinforcement is anchored to wall-clock time internally
    /// (`Engine::commit` uses the real clock), so callers using logical-time
    /// (`search_at` with a synthetic `now`) should be aware that the decay
    /// applied to committed nodes is wall-clock anchored, not logical-time
    /// anchored.
    pub fn used(&mut self, recall: Recall) -> Result<(), Error> {
        self.engine
            .commit(recall.package, Some(ConfidenceLevel::Medium))?;
        Ok(())
    }

    /// Advance the engine's decay clock to `now`.
    ///
    /// Decays the retained-action reservoir `A_i` for all nodes and re-projects
    /// salience. Returns the tick report for observability.
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error> {
        self.engine.tick(now)
    }
}

// ── Search helpers ────────────────────────────────────────────────────────────

/// Extract `(speaker, session)` from a node's entity tags.
///
/// Looks for `speaker-<norm>` and `session-<norm>` tags (the convention used
/// by the bench recipe). Returns `None` for each if the corresponding tag is
/// absent.
fn parse_entity_tags(tags: &[String]) -> (Option<String>, Option<String>) {
    let mut speaker = None;
    let mut session = None;
    for tag in tags {
        if let Some(s) = tag.strip_prefix("speaker-") {
            speaker = Some(s.to_string());
        } else if let Some(s) = tag.strip_prefix("session-") {
            session = Some(s.to_string());
        }
    }
    (speaker, session)
}

// ── Recipe helpers ────────────────────────────────────────────────────────────

/// Join `[prev?, cur, next?]` into the context-window string.
fn build_window(prev: Option<&str>, cur: &str, next: Option<&str>) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(3);
    if let Some(p) = prev {
        parts.push(p);
    }
    parts.push(cur);
    if let Some(n) = next {
        parts.push(n);
    }
    parts.join("\n")
}

/// Entity tags: `session-<norm>` and `speaker-<norm>` (no dataset tag).
fn entity_tags_for(session: &str, speaker: &str) -> Vec<String> {
    vec![
        format!("session-{}", normalize_tag(session)),
        format!("speaker-{}", normalize_tag(speaker)),
    ]
}

/// Normalize a tag component: trim, lowercase, replace ` `, `:`, `_` with `-`.
fn normalize_tag(value: &str) -> String {
    value.trim().to_lowercase().replace([' ', ':', '_'], "-")
}

/// First 50 chars of `content` as the node name (bench `make_name`).
fn make_name(content: &str) -> String {
    let name: String = content.chars().take(50).collect();
    if name.trim().is_empty() {
        "empty turn".to_string()
    } else {
        name
    }
}

/// Embed a single text string via the provider.
fn embed_one(provider: &dyn EmbeddingProvider, text: &str) -> Result<Vec<f64>, Error> {
    let results = provider.embed_f64(&[text])?;
    results
        .into_iter()
        .next()
        .ok_or_else(|| Error::InvalidInput("provider returned empty embedding".to_string()))
}

/// Ingest a node via the public `Engine::ingest` API and return its `NodeId`.
#[allow(clippy::too_many_arguments)]
fn ingest_node<S: StorageAdapter + Clone>(
    engine: &mut Engine<S>,
    content_for_name: &str,
    content: String,
    embedding: Vec<f64>,
    node_type: KnowledgeType,
    timestamp: Timestamp,
    entity_tags: Vec<String>,
    summary: Option<String>,
    session_id: &str,
) -> Result<NodeId, Error> {
    let observation = Observation {
        name: make_name(content_for_name),
        summary,
        content,
        embedding: Some(embedding),
        confidence: 0.95,
        node_type,
        entity_tags,
        origin: Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: session_id.to_string(),
            scope: ScopePath::universal(),
            confidence: 0.95,
        },
        timestamp,
        valid_from: None,
        valid_until: None,
    };
    match engine.ingest(observation)? {
        IngestResult::Created(ids) => ids
            .first()
            .copied()
            .ok_or_else(|| Error::InvalidInput("ingest created no node".to_string())),
        IngestResult::Reinforced { existing_id, .. } => Ok(existing_id),
    }
}
