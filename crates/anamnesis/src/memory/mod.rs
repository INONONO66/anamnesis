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

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

mod manage;
mod view;
pub use view::{ListFilter, MemoryView};

use crate::Engine;
use crate::api::{CommitReport, EngineConfig, HealthGrade, IngestResult, Observation, TickReport};
use crate::embedding::EmbeddingProvider;
use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::types::SourceKind;
use crate::graph::types::{EdgeId, PeerId};
use crate::graph::{Edge, EdgeType, KnowledgeType, Node, NodeId, ScopePath, Timestamp};
use crate::mechanics::social::ConfidenceLevel;
use crate::query::{ContextPackage, Fragment, SearchInput, SearchResult, Tension};
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

/// Options for [`Memory::add_note_with`] — optional scope, extra entity tags,
/// and metadata applied to the ingested note beyond the default recipe.
///
/// Both nodes the note creates (Episodic + Semantic) receive the same
/// `scope`, extra `tags`, and `metadata`.
#[derive(Debug, Clone, Default)]
pub struct NoteOptions {
    /// Origin scope for the note. `None` (default) ⇒ universal scope — the
    /// same default [`Memory::add_note`] uses.
    pub scope: Option<ScopePath>,
    /// Extra entity tags appended to the recipe's default session/speaker tags.
    pub tags: Vec<String>,
    /// Consumer-defined metadata key-value pairs stamped on both ingested nodes.
    pub metadata: Vec<(String, String)>,
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

// ── Agent-facing relations ───────────────────────────────────────────────────

/// A curated, agent-facing subset of the engine's [`EdgeType`] relations.
///
/// This is the relation vocabulary exposed at the [`Memory`] front door for
/// hand-authoring typed reasoning-chain edges via [`Memory::relate`]. It
/// deliberately excludes engine-internal edge types (`Temporal`, `ExtractedFrom`,
/// `ConsolidatedFrom`, `ReinforcedBy`, `Entity`) — those are wired automatically
/// by the recipe and should not be authored by hand — and `Supersedes`, which is
/// directional *and* mutates the validity window of its endpoints. Reach for
/// [`Memory::engine_mut`] if you genuinely need those.
///
/// Each variant maps to exactly one engine [`EdgeType`]:
///
/// | `Relation`           | engine [`EdgeType`]            | meaning                         |
/// |----------------------|--------------------------------|---------------------------------|
/// | [`Causes`]           | [`EdgeType::Causal`]           | cause → effect                  |
/// | [`Contradicts`]      | [`EdgeType::Contradicts`]      | conflicting assertions          |
/// | [`Supports`]         | [`EdgeType::Supports`]         | positive evidential support     |
/// | [`Refutes`]          | [`EdgeType::Refutes`]          | refuting evidence (weak)        |
/// | [`Reason`]           | [`EdgeType::Reason`]           | decision rationale              |
/// | [`RejectedAlternative`] | [`EdgeType::RejectedAlternative`] | considered & discarded option |
/// | [`BelongsTo`]        | [`EdgeType::BelongsTo`]        | hierarchical / containment      |
/// | [`Related`]          | [`EdgeType::Semantic`]         | generic conceptual relationship |
/// | [`Custom`]           | [`EdgeType::Custom`]           | consumer-defined relation       |
///
/// [`Causes`]: Relation::Causes
/// [`Contradicts`]: Relation::Contradicts
/// [`Supports`]: Relation::Supports
/// [`Refutes`]: Relation::Refutes
/// [`Reason`]: Relation::Reason
/// [`RejectedAlternative`]: Relation::RejectedAlternative
/// [`BelongsTo`]: Relation::BelongsTo
/// [`Related`]: Relation::Related
/// [`Custom`]: Relation::Custom
///
/// # Note on `Contradicts`
///
/// `Contradicts` is a *constraint* edge: it is excluded from spreading-activation
/// propagation and instead surfaces query-local frustration stress between its
/// active endpoints (ADR-0006). It is never inhibitory and is never auto-deleted.
/// `Refutes`, despite the name, *is* a weak supportive propagating edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive] // deliberately-growable vocabulary; future relations stay additive
pub enum Relation {
    /// Cause → effect ([`EdgeType::Causal`]).
    Causes,
    /// Conflicting assertions ([`EdgeType::Contradicts`]). Surfaces frustration
    /// stress rather than propagating activation; never inhibitory.
    Contradicts,
    /// Positive evidential support ([`EdgeType::Supports`]).
    Supports,
    /// Refuting evidence ([`EdgeType::Refutes`]). Weak supportive propagation,
    /// *not* inhibitory despite the name.
    Refutes,
    /// Decision rationale ([`EdgeType::Reason`]).
    Reason,
    /// A considered-and-discarded option ([`EdgeType::RejectedAlternative`]).
    RejectedAlternative,
    /// Hierarchical / containment relationship ([`EdgeType::BelongsTo`]).
    BelongsTo,
    /// Generic conceptual relationship ([`EdgeType::Semantic`]).
    Related,
    /// Replaces outdated knowledge ([`EdgeType::Supersedes`]).
    Supersedes,
    /// A consumer-defined relation, carrying its label through to
    /// [`EdgeType::Custom`].
    Custom(String),
}

impl Relation {
    /// Map this agent-facing relation to the engine's [`EdgeType`].
    fn to_edge_type(&self) -> EdgeType {
        match self {
            Relation::Causes => EdgeType::Causal,
            Relation::Contradicts => EdgeType::Contradicts,
            Relation::Supports => EdgeType::Supports,
            Relation::Refutes => EdgeType::Refutes,
            Relation::Reason => EdgeType::Reason,
            Relation::RejectedAlternative => EdgeType::RejectedAlternative,
            Relation::BelongsTo => EdgeType::BelongsTo,
            Relation::Related => EdgeType::Semantic,
            Relation::Supersedes => EdgeType::Supersedes,
            Relation::Custom(label) => EdgeType::Custom(label.clone()),
        }
    }
}

impl From<Relation> for EdgeType {
    fn from(relation: Relation) -> Self {
        relation.to_edge_type()
    }
}

/// Direction of an edge relative to the node it was read from.
///
/// Returned as part of a [`Neighbor`] by [`Memory::neighbors`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// The edge points *away* from the queried node (queried node is the source).
    Outgoing,
    /// The edge points *toward* the queried node (queried node is the target).
    Incoming,
}

/// A typed neighbor of a node, as returned by [`Memory::neighbors`].
///
/// Carries the other endpoint, the edge id and type, the edge weight, and the
/// direction of the edge relative to the queried node. The `edge_type` is the
/// raw engine [`EdgeType`] (so engine-internal edges like `Temporal` /
/// `ExtractedFrom` are visible) — agents can map the agent-facing subset back to
/// [`Relation`] themselves if desired.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive] // may gain fields (e.g. validity window); keep additive
pub struct Neighbor {
    /// The other endpoint of the edge (not the queried node).
    pub node: NodeId,
    /// The edge connecting the two nodes.
    pub edge: EdgeId,
    /// The engine relationship type of the edge.
    pub edge_type: EdgeType,
    /// Edge strength [0, 1] (the bounded projection of conductance).
    pub weight: f64,
    /// Direction of the edge relative to the queried node.
    pub direction: Direction,
}

// ── Stats / health snapshot ──────────────────────────────────────────────────

/// Strongly-typed read-only snapshot of graph size, structure, and decay/health,
/// returned by [`Memory::stats`].
///
/// Combines the engine's structural grade report ([`Engine::health`]) and its
/// nine-metric observability report ([`Engine::graph_health`]) into one summary.
///
/// # Buffering caveat
///
/// `stats` reflects only **flushed** (persisted) graph state. Pending per-session
/// buffers (the not-yet-finalized last turn of each open session) are *not*
/// counted. Call [`Memory::flush_all`] first if exact live counts matter.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive] // report may gain metrics; keep additive
pub struct MemoryStats {
    /// Total number of live nodes.
    pub node_count: usize,
    /// Total number of live edges.
    pub edge_count: usize,
    /// Number of nodes with no edges (orphans).
    pub orphan_count: usize,
    /// Fraction of nodes that are orphans `[0, 1]` (fragmentation signal).
    pub orphan_ratio: f64,
    /// Number of `Contradicts` edges (knowledge conflicts).
    pub contradiction_count: usize,
    /// Fraction of edges that are `Contradicts` `[0, 1]`.
    pub contradiction_ratio: f64,
    /// Number of `Supersedes` edges.
    pub supersede_count: usize,
    /// Number of retracted nodes.
    pub retracted_count: usize,
    /// Number of nodes without an embedding vector.
    pub missing_embedding_count: usize,
    /// Average salience across all nodes.
    pub avg_salience: f64,
    /// Mean graph degree `2 * edge_count / node_count`.
    pub average_degree: f64,
    /// Fraction of nodes not accessed within the 30-day stale window `[0, 1]` —
    /// the closest structural signal to a "forgetting"/decay summary the engine
    /// exposes.
    pub stale_ratio: f64,
    /// Shannon entropy (bits) of the salience distribution (diagnostic; diversity
    /// of salience across nodes).
    pub salience_entropy: f64,
    /// Node count by origin scope (`"universal"` keys the universal scope).
    pub scope_distribution: BTreeMap<String, usize>,
    /// Number of registered peers.
    pub peer_count: usize,
    /// Overall structural health grade (A/B/C/D).
    pub grade: HealthGrade,
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
            ScopePath::universal(),
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
                ScopePath::universal(),
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
    ///
    /// Equivalent to `add_note_with(text, at, NoteOptions::default())` —
    /// universal scope, no extra tags, no metadata.
    pub fn add_note(&mut self, text: &str, at: Timestamp) -> Result<AddReceipt, Error> {
        self.add_note_with(text, at, NoteOptions::default())
    }

    /// Like [`add_note`](Memory::add_note), with an optional scope, extra
    /// entity tags, and metadata applied to both ingested nodes.
    ///
    /// `opts.scope` sets both nodes' `Origin.scope` (default: universal).
    /// `opts.tags` are appended to the recipe's default session/speaker
    /// entity tags. `opts.metadata` is stamped via
    /// [`set_metadata_pairs`](Memory::set_metadata_pairs) after ingest (the
    /// same durable write path `set_metadata`/`set_metadata_pairs` already use).
    pub fn add_note_with(
        &mut self,
        text: &str,
        at: Timestamp,
        opts: NoteOptions,
    ) -> Result<AddReceipt, Error> {
        let session_id = format!("note-{}", at.0);
        let speaker = "note";
        let speaker_text = text.to_string();
        let scope = opts.scope.unwrap_or_else(ScopePath::universal);

        let mut entity_tags = entity_tags_for(&session_id, speaker);
        entity_tags.extend(opts.tags.iter().cloned());

        let epi_embedding = embed_one(&*self.provider, &speaker_text)?;
        let epi_id = ingest_node(
            &mut self.engine,
            &speaker_text,
            speaker_text.clone(),
            epi_embedding,
            KnowledgeType::Episodic,
            at,
            entity_tags.clone(),
            None,
            &session_id,
            scope.clone(),
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
            entity_tags,
            None,
            &session_id,
            scope,
        )?;
        self.engine.link(sem_id, epi_id, EdgeType::ExtractedFrom)?;

        if !opts.metadata.is_empty() {
            let pairs: Vec<(&str, &str)> = opts
                .metadata
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            self.set_metadata_pairs(epi_id, &pairs)?;
            self.set_metadata_pairs(sem_id, &pairs)?;
        }

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
            ScopePath::universal(),
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

    /// Set one metadata key on an existing node and persist it durably.
    ///
    /// Metadata is written via `set_node` (INSERT OR REPLACE) — a bare
    /// `get_node_mut` + `flush` would only persist hot fields, so the value
    /// would silently vanish on reopen. Used by the capture pipeline to stamp
    /// `anamnesis:turn_key` / `anamnesis:extracted` on Episodic nodes.
    pub fn set_metadata(&mut self, id: NodeId, key: &str, value: &str) -> Result<(), Error> {
        self.set_metadata_pairs(id, &[(key, value)])
    }

    /// Set several metadata keys on an existing node in **one** durable write.
    ///
    /// A single `set_node` (one `INSERT OR REPLACE` row write) carries all pairs,
    /// so callers that must stamp related keys together (e.g. the capture
    /// pipeline's `anamnesis:turn_key` + `anamnesis:extracted`) cannot be split
    /// by a partial failure between two writes.
    pub fn set_metadata_pairs(&mut self, id: NodeId, pairs: &[(&str, &str)]) -> Result<(), Error> {
        let mut node = self.engine.graph().get_node(id)?.clone();
        for (key, value) in pairs {
            node.metadata
                .insert((*key).to_string(), (*value).to_string());
        }
        self.engine.graph_mut().storage_mut().set_node(node)
    }
}

// ── Relate / neighbors — typed reasoning-chain edges ─────────────────────────

impl<S: StorageAdapter + Clone> Memory<S> {
    /// Author a typed edge between two existing nodes.
    ///
    /// This is the front-door path for filling typed reasoning-chain edges — the
    /// agent passes node ids (e.g. from a prior [`recall`](Memory::search)) and a
    /// curated [`Relation`]. The relation maps to an engine [`EdgeType`] and the
    /// edge is created via [`Engine::link`].
    ///
    /// Returns the new [`EdgeId`].
    ///
    /// # Edge strength
    ///
    /// The edge's strength is **not** caller-supplied: the engine derives a
    /// cold-start conductance seed and projects the weight itself (ADR-0002). You
    /// cannot hand-author edge strength through this API.
    ///
    /// # Errors
    ///
    /// Returns an error if either endpoint does not exist in the graph
    /// ([`Engine::link`] resolves both up front).
    ///
    /// # Note
    ///
    /// [`Relation::Contradicts`] creates a constraint edge that surfaces
    /// query-local frustration stress rather than propagating activation; it is
    /// never inhibitory. Engine-internal edge types (`Temporal`, `ExtractedFrom`,
    /// etc.) and the time-mutating `Supersedes` are intentionally *not* reachable
    /// here — use [`engine_mut`](Memory::engine_mut) if you genuinely need them.
    ///
    /// # Additive
    ///
    /// `relate` does not de-duplicate: calling it twice for the same
    /// `(from, to, relation)` creates two distinct edges (and inflates
    /// [`stats`](Memory::stats)'s `edge_count`). Re-asserting a known relation
    /// stacks edges; the caller owns idempotency.
    pub fn relate(
        &mut self,
        from: NodeId,
        to: NodeId,
        relation: Relation,
    ) -> Result<EdgeId, Error> {
        self.engine.link(from, to, relation.into())
    }

    /// Read a node's typed edges (both outgoing and incoming).
    ///
    /// Returns a [`Neighbor`] for every edge touching `node`: outgoing edges first
    /// (where `node` is the source, neighbor is the target), then incoming edges
    /// (where `node` is the target, neighbor is the source). Each carries the
    /// other endpoint, edge id, engine [`EdgeType`], weight, and [`Direction`].
    ///
    /// This is a read-only view supporting future graph-expansion use-cases.
    ///
    /// # Errors
    ///
    /// Returns an error if any edge referenced by the node cannot be resolved (a
    /// storage inconsistency). A node with no edges yields an empty vector.
    pub fn neighbors(&self, node: NodeId) -> Result<Vec<Neighbor>, Error> {
        let graph = self.engine.graph();
        // edges_from/edges_to borrow `graph`, and get_edge also borrows it
        // immutably — both are shared borrows, so iterating the slices while
        // calling get_edge inside the loop compiles without a double-borrow.
        let mut out = Vec::with_capacity(graph.edges_from(node).len() + graph.edges_to(node).len());
        for &edge_id in graph.edges_from(node) {
            let edge = graph.get_edge(edge_id)?;
            out.push(Neighbor {
                node: edge.target,
                edge: edge_id,
                edge_type: edge.edge_type.clone(),
                weight: edge.weight,
                direction: Direction::Outgoing,
            });
        }
        for &edge_id in graph.edges_to(node) {
            let edge = graph.get_edge(edge_id)?;
            out.push(Neighbor {
                node: edge.source,
                edge: edge_id,
                edge_type: edge.edge_type.clone(),
                weight: edge.weight,
                direction: Direction::Incoming,
            });
        }
        Ok(out)
    }

    /// Extract a bounded, multi-seed k-hop subgraph as owned snapshots.
    ///
    /// Runs an undirected breadth-first search from every id in `seeds`
    /// simultaneously (each seed starts at depth 0; a node's recorded depth is
    /// its distance from the *nearest* seed). Traversal follows both outgoing
    /// and incoming edges, mirroring [`Memory::neighbors`]. Expansion stops
    /// past `depth` hops, and once the visited-node count reaches
    /// `node_budget` no further nodes are enqueued.
    /// [`Subgraph::truncated`](Subgraph) is set **only** when that budget cutoff
    /// actually discarded a still-unvisited node reachable within `depth` — a
    /// fully-exhausted BFS frontier (nothing left to visit) never sets it, even
    /// when the wider graph holds unrelated, unreachable nodes.
    ///
    /// The returned edge set is **induced**: every edge whose endpoints are
    /// both in the visited set is included exactly once, even if one endpoint
    /// was only reached through a different seed's BFS branch.
    ///
    /// # Empty seeds
    ///
    /// `seeds == &[]` returns an empty, non-truncated [`Subgraph`] — there is
    /// nothing to expand from, so no edges or nodes can qualify.
    ///
    /// # Errors
    ///
    /// Returns an error if any id in `seeds` does not exist in the graph
    /// (mirrors [`Memory::neighbors`] / [`Engine::link`] resolving endpoints
    /// up front).
    pub fn subgraph(
        &self,
        seeds: &[NodeId],
        depth: usize,
        node_budget: usize,
    ) -> Result<Subgraph, Error> {
        let graph = self.engine.graph();
        for &seed in seeds {
            graph.get_node(seed)?;
        }

        let (depths, truncated) = bfs_depths(graph, seeds, depth, node_budget);
        let visited: HashSet<NodeId> = depths.keys().copied().collect();

        let mut edges: Vec<Edge> = Vec::new();
        let mut seen_edges: HashSet<EdgeId> = HashSet::new();
        for &nid in &visited {
            for &eid in graph.edges_from(nid) {
                if let Ok(edge) = graph.get_edge(eid)
                    && visited.contains(&edge.target)
                    && seen_edges.insert(eid)
                {
                    edges.push(edge.clone());
                }
            }
        }

        let nodes: Vec<Node> = depths
            .keys()
            .filter_map(|&nid| graph.get_node(nid).ok().cloned())
            .collect();
        let depth_pairs: Vec<(NodeId, usize)> = depths.into_iter().collect();

        Ok(Subgraph {
            nodes,
            edges,
            depths: depth_pairs,
            truncated,
        })
    }
}

/// Undirected multi-seed BFS, bounded by `max_depth` and `node_budget`.
///
/// Returns each visited node's distance from the nearest seed, plus whether
/// the budget cutoff actually discarded a genuinely new (not-yet-visited)
/// node that was still within `max_depth` — i.e. real truncation of the
/// reachable set, not merely "the graph has more nodes than the budget."
/// Stops enqueuing once `node_budget` nodes have been visited (a
/// `node_budget` of 0 visits nothing, and is truncated iff there was a seed
/// to visit). Extracted from [`Memory::subgraph`] to keep that method within
/// the file's LOC guidance.
fn bfs_depths<S: StorageAdapter + Clone>(
    graph: &crate::graph::Graph<S>,
    seeds: &[NodeId],
    max_depth: usize,
    node_budget: usize,
) -> (HashMap<NodeId, usize>, bool) {
    let mut depths = HashMap::new();
    let mut queue = VecDeque::new();
    let mut truncated = false;

    for &seed in seeds {
        if depths.contains_key(&seed) {
            continue;
        }
        if depths.len() >= node_budget {
            truncated = true;
            break;
        }
        depths.insert(seed, 0);
        queue.push_back((seed, 0));
    }

    while let Some((nid, dist)) = queue.pop_front() {
        if dist >= max_depth {
            continue;
        }
        let neighbor_ids = graph
            .edges_from(nid)
            .iter()
            .filter_map(|&eid| graph.get_edge(eid).ok().map(|e| e.target))
            .chain(
                graph
                    .edges_to(nid)
                    .iter()
                    .filter_map(|&eid| graph.get_edge(eid).ok().map(|e| e.source)),
            );
        for neighbor in neighbor_ids {
            if depths.contains_key(&neighbor) {
                continue;
            }
            if depths.len() >= node_budget {
                truncated = true;
                break;
            }
            depths.insert(neighbor, dist + 1);
            queue.push_back((neighbor, dist + 1));
        }
    }

    (depths, truncated)
}

/// An owned, bounded k-hop subgraph snapshot returned by [`Memory::subgraph`].
///
/// All fields are clones detached from the live graph — mutating the
/// `Memory` afterward does not affect a previously returned `Subgraph`.
#[derive(Debug, Clone)]
pub struct Subgraph {
    /// Every node visited by the bounded BFS (seeds plus in-budget neighbors).
    pub nodes: Vec<Node>,
    /// The induced edge set: every edge whose both endpoints are in `nodes`.
    pub edges: Vec<Edge>,
    /// Each visited node's hop distance from the nearest seed (seeds = 0).
    pub depths: Vec<(NodeId, usize)>,
    /// `true` if `node_budget` was reached before the BFS frontier was
    /// exhausted (i.e. the graph has more reachable nodes than were returned).
    pub truncated: bool,
}

// ── Stats — read-only health snapshot ────────────────────────────────────────

impl<S: StorageAdapter + Clone> Memory<S> {
    /// Read-only snapshot of graph size, structure, and decay/health.
    ///
    /// Combines [`Engine::health`] (structural grade) and [`Engine::graph_health`]
    /// (nine-metric observability) into one [`MemoryStats`]. This is a pure read;
    /// it does **not** flush pending session buffers, so buffered-but-unflushed
    /// turns are not counted (call [`flush_all`](Memory::flush_all) first if exact
    /// live counts matter).
    ///
    /// `Ok` is always returned — both underlying reports are infallible — but the
    /// `Result` is kept for API forward-compatibility.
    pub fn stats(&self) -> Result<MemoryStats, Error> {
        self.stats_at(Timestamp::now())
    }

    /// Deterministic variant of [`stats`](Memory::stats): the `stale_ratio`
    /// 30-day window is measured against the supplied `now` instead of the wall
    /// clock, so the snapshot is reproducible.
    pub fn stats_at(&self, now: Timestamp) -> Result<MemoryStats, Error> {
        let health = self.engine.health();
        let graph = self.engine.graph_health_at(now);
        Ok(MemoryStats {
            node_count: graph.node_count,
            edge_count: graph.edge_count,
            orphan_count: health.orphan_count,
            orphan_ratio: graph.orphan_ratio,
            contradiction_count: health.contradiction_count,
            contradiction_ratio: graph.contradiction_ratio,
            supersede_count: health.supersede_count,
            retracted_count: health.retracted_count,
            missing_embedding_count: health.missing_embedding_count,
            avg_salience: health.avg_salience,
            average_degree: graph.average_degree,
            stale_ratio: graph.stale_ratio,
            salience_entropy: graph.salience_entropy,
            scope_distribution: graph.scope_distribution,
            peer_count: health.peer_count,
            grade: health.grade,
        })
    }
}

// ── Search / recall / used / tick ─────────────────────────────────────────────

/// Optional tuning knobs for [`Memory::search_result_at_with`].
///
/// All fields default to the bench-validated recipe values. Override only when
/// you have a measured reason to deviate.
#[derive(Debug, Clone, Default)]
pub struct SearchTuning {
    /// Override the number of seed nodes to expand with graph recall.
    ///
    /// `None` (default) uses the recipe default (`limit.max(1)`).
    pub seed_limit: Option<usize>,
    /// Entity tags to inject as retrieval seeds (e.g. speaker cues).
    ///
    /// Empty (default) = entity-tag retrieval OFF (bench default, speaker cues OFF).
    pub entity_tags: Vec<String>,
}

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
    /// Query-embedding cosine vs this node, or `0.0` when either embedding is absent.
    pub cosine: f64,
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

impl Recall {
    /// Render the assembled [`ContextPackage`] into a readable, agent-consumable
    /// context block.
    ///
    /// The block is grouped into `## IDENTITY` / `## KNOWLEDGE` / `## MEMORIES` /
    /// `## TENSIONS` sections. Empty sections are skipped. Each fragment shows its
    /// knowledge type, name, relevance, body (full content if present, else
    /// summary), and a provenance line (peer, source kind, session, scope,
    /// confidence, scope relation). Tensions render as `#A ⟂ #B` lines with the
    /// optional description and the query-local stress.
    ///
    /// This is a pure read over [`Recall::package`] — it never mutates and never
    /// fails (writing into a `String` is infallible).
    pub fn as_context(&self) -> String {
        let pkg = &self.package;
        let mut out = String::new();

        render_section(&mut out, "IDENTITY", &pkg.identity);
        render_section(&mut out, "KNOWLEDGE", &pkg.knowledge);
        render_section(&mut out, "MEMORIES", &pkg.memories);

        if !pkg.tensions.is_empty() {
            out.push_str("## TENSIONS\n");
            for tension in &pkg.tensions {
                render_tension(&mut out, tension);
            }
            out.push('\n');
        }

        out
    }
}

/// Human-readable label for a node type in rendered context output.
///
/// `KnowledgeType` has no `Display`; the fixed variants render via `{:?}`
/// (`Identity`/`Semantic`/`Episodic`), but `Custom("gotcha")` would render as the
/// noisy `Custom("gotcha")`. Render `Custom` as its bare inner label instead so a
/// legacy/consumer type reads as `[gotcha]` rather than `[Custom("gotcha")]`.
fn node_type_label(kt: &KnowledgeType) -> String {
    match kt {
        KnowledgeType::Custom(label) => label.clone(),
        other => format!("{other:?}"),
    }
}

/// Render one titled fragment section (skipped entirely if `frags` is empty).
fn render_section(out: &mut String, title: &str, frags: &[Fragment]) {
    if frags.is_empty() {
        return;
    }
    let _ = writeln!(out, "## {title}");
    for f in frags {
        // Header: type label (KnowledgeType has no Display), name, relevance.
        let _ = writeln!(
            out,
            "- [{}] {} (relevance {:.2})",
            node_type_label(&f.node_type),
            f.name,
            f.relevance
        );
        // Body: prefer full content (L2), fall back to summary (L1); name is
        // already shown in the header.
        if let Some(content) = &f.content {
            let _ = writeln!(out, "    {content}");
        } else if let Some(summary) = &f.summary {
            let _ = writeln!(out, "    {summary}");
        }
        // Provenance line. ScopePath (origin.scope) HAS Display; SourceKind needs
        // {:?}. Scopes are flat opaque paths (hierarchy removed), so the origin
        // scope string is the whole story — there is no query-relative relation.
        let _ = writeln!(
            out,
            "    └ origin: peer #{}, {:?}, session \"{}\", scope {} (conf {:.2})",
            f.origin.peer_id.0,
            f.origin.source_kind,
            f.origin.session_id,
            f.origin.scope,
            f.origin.confidence,
        );
    }
    out.push('\n');
}

/// Render one tension line: `#A ⟂ #B [— description] (stress N.NN)`.
fn render_tension(out: &mut String, tension: &Tension) {
    let _ = write!(out, "- #{} ⟂ #{}", tension.node_a.0, tension.node_b.0);
    if let Some(desc) = &tension.description {
        let _ = write!(out, " — {desc}");
    }
    let _ = writeln!(out, " (stress {:.2})", tension.stress);
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
                    cosine: candidate.embedding_cosine,
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

    /// Power-user variant: like [`search_at`](Memory::search_at) but returns the
    /// raw [`SearchResult`] (including [`SearchTrace`](crate::query::SearchTrace)
    /// with pre-packaging readout candidates) and accepts optional tuning knobs.
    ///
    /// Prefer [`search_at`](Memory::search_at) for ordinary use-cases. This method exists for
    /// consumers (benchmarks, tooling) that need the full readout trace or need
    /// to override seed-limit / entity-tag cues without constructing a
    /// [`SearchInput`] manually.
    ///
    /// Flush semantics are the same as [`search_at`](Memory::search_at): all pending session buffers
    /// are flushed before the query is executed.
    pub fn search_result_at_with(
        &mut self,
        query: &str,
        limit: usize,
        now: Timestamp,
        tuning: &SearchTuning,
    ) -> Result<SearchResult, Error> {
        self.flush_all()?;

        let embedding = embed_one(&*self.provider, query)?;
        let seed_limit = tuning.seed_limit.unwrap_or_else(|| limit.max(1));
        let input = SearchInput {
            text: query.to_string(),
            query_embedding: Some(embedding),
            limit,
            seed_limit: Some(seed_limit),
            now,
            entity_tags: tuning.entity_tags.clone(),
            ..SearchInput::default()
        };
        self.engine.search(input)
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
    pub fn used(&mut self, recall: Recall) -> Result<CommitReport, Error> {
        let (_, report) = self
            .engine
            .commit(recall.package, Some(ConfidenceLevel::Medium))?;
        Ok(report)
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
    scope: ScopePath,
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
            scope,
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, model-free embedding provider.
    ///
    /// Produces a fixed-dimension unit-ish vector seeded by a per-text hash so
    /// that distinct texts get distinct (low-similarity) embeddings — enough to
    /// avoid crystallize's dedup rejection — while being fully reproducible. No
    /// network / model download.
    struct HashEmbedProvider {
        dim: usize,
    }

    impl HashEmbedProvider {
        fn new() -> Self {
            Self { dim: 8 }
        }
    }

    impl EmbeddingProvider for HashEmbedProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            Ok(texts
                .iter()
                .map(|text| {
                    // FNV-1a-ish seed from the text bytes.
                    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
                    for b in text.bytes() {
                        seed ^= b as u64;
                        seed = seed.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    // Spread the seed across the dimensions deterministically.
                    (0..self.dim)
                        .map(|i| {
                            let mut x = seed.wrapping_add(i as u64).wrapping_mul(2_654_435_761);
                            x ^= x >> 13;
                            // Map to [-1, 1].
                            ((x % 2000) as f32) / 1000.0 - 1.0
                        })
                        .collect()
                })
                .collect())
        }

        fn dimensions(&self) -> usize {
            self.dim
        }

        fn model_name(&self) -> &str {
            "hash-stub"
        }
    }

    fn mem() -> Memory<SqliteStorage> {
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedProvider::new());
        Memory::in_memory_with_provider(provider).expect("in-memory Memory")
    }

    fn t(ms: u64) -> Timestamp {
        Timestamp(ms)
    }

    // ── Relation mapping ──────────────────────────────────────────────────────

    #[test]
    fn relation_maps_to_edge_type() {
        assert_eq!(EdgeType::from(Relation::Causes), EdgeType::Causal);
        assert_eq!(EdgeType::from(Relation::Contradicts), EdgeType::Contradicts);
        assert_eq!(EdgeType::from(Relation::Supports), EdgeType::Supports);
        assert_eq!(EdgeType::from(Relation::Refutes), EdgeType::Refutes);
        assert_eq!(EdgeType::from(Relation::Reason), EdgeType::Reason);
        assert_eq!(
            EdgeType::from(Relation::RejectedAlternative),
            EdgeType::RejectedAlternative
        );
        assert_eq!(EdgeType::from(Relation::BelongsTo), EdgeType::BelongsTo);
        // `Related` deliberately maps to the generic Semantic edge.
        assert_eq!(EdgeType::from(Relation::Related), EdgeType::Semantic);
        assert_eq!(
            EdgeType::from(Relation::Custom("foo".to_string())),
            EdgeType::Custom("foo".to_string())
        );
    }

    #[test]
    fn relation_supersedes_maps_to_edge_type() {
        assert_eq!(EdgeType::from(Relation::Supersedes), EdgeType::Supersedes);
    }

    #[test]
    fn search_hits_carry_embedding_cosine() {
        let mut m = mem();
        m.add_note("the recall gate uses cosine now", t(1)).unwrap();

        let recall = m.search("recall gate", 5).unwrap();
        let top = recall.hits.first().expect("at least one hit");

        assert!(
            top.cosine > 0.0 && top.cosine <= 1.0 + f64::EPSILON,
            "cosine must be populated from the readout surface, got {}",
            top.cosine
        );
    }

    // ── relate ────────────────────────────────────────────────────────────────

    #[test]
    fn relate_creates_typed_edge() {
        let mut m = mem();
        let a = m
            .add("s", "alice", "the deploy failed", t(1))
            .unwrap()
            .episodic;
        let b = m
            .add("s", "alice", "the disk was full", t(2))
            .unwrap()
            .episodic;
        m.flush_all().unwrap();

        let edge = m.relate(b, a, Relation::Causes).unwrap();

        // The edge shows up as an outgoing neighbor of `b` with the Causal type.
        let neighbors = m.neighbors(b).unwrap();
        let causal = neighbors
            .iter()
            .find(|n| n.edge == edge)
            .expect("relate edge present in neighbors");
        assert_eq!(causal.node, a);
        assert_eq!(causal.edge_type, EdgeType::Causal);
        assert_eq!(causal.direction, Direction::Outgoing);
    }

    #[test]
    fn relate_custom_relation_roundtrips() {
        let mut m = mem();
        let a = m.add("s", "a", "x", t(1)).unwrap().episodic;
        let b = m.add("s", "a", "y", t(2)).unwrap().episodic;
        m.flush_all().unwrap();
        let edge = m
            .relate(a, b, Relation::Custom("blocks".to_string()))
            .unwrap();
        let n = m.neighbors(a).unwrap();
        let found = n.iter().find(|n| n.edge == edge).unwrap();
        assert_eq!(found.edge_type, EdgeType::Custom("blocks".to_string()));
    }

    #[test]
    fn relate_missing_endpoint_errors() {
        let mut m = mem();
        let a = m.add("s", "a", "x", t(1)).unwrap().episodic;
        m.flush_all().unwrap();
        // NodeId(9999) does not exist.
        let result = m.relate(a, NodeId(9999), Relation::Related);
        assert!(result.is_err(), "linking to a missing node must error");
    }

    // ── neighbors ─────────────────────────────────────────────────────────────

    #[test]
    fn neighbors_reports_direction() {
        let mut m = mem();
        let a = m.add("s", "a", "alpha", t(1)).unwrap().episodic;
        let b = m.add("s", "a", "beta", t(2)).unwrap().episodic;
        m.flush_all().unwrap();
        let edge = m.relate(a, b, Relation::Supports).unwrap();

        // Outgoing from a.
        let out = m.neighbors(a).unwrap();
        let out_hit = out.iter().find(|n| n.edge == edge).unwrap();
        assert_eq!(out_hit.node, b);
        assert_eq!(out_hit.direction, Direction::Outgoing);

        // Incoming to b.
        let inc = m.neighbors(b).unwrap();
        let inc_hit = inc.iter().find(|n| n.edge == edge).unwrap();
        assert_eq!(inc_hit.node, a);
        assert_eq!(inc_hit.direction, Direction::Incoming);
    }

    #[test]
    fn neighbors_includes_recipe_edges() {
        let mut m = mem();
        // Two turns produce an episodic→episodic Temporal edge and a
        // semantic→episodic ExtractedFrom edge from the recipe.
        let a = m.add("s", "a", "first", t(1)).unwrap().episodic;
        let _ = m.add("s", "a", "second", t(2)).unwrap();
        m.flush_all().unwrap();

        let n = m.neighbors(a).unwrap();
        // `a` has at least one Temporal (outgoing to the next episodic) edge and
        // is the target of an ExtractedFrom edge (incoming).
        assert!(
            n.iter().any(|x| x.edge_type == EdgeType::Temporal),
            "expected a Temporal recipe edge among neighbors: {n:?}"
        );
        assert!(
            n.iter().any(|x| x.edge_type == EdgeType::ExtractedFrom),
            "expected an ExtractedFrom recipe edge among neighbors: {n:?}"
        );
    }

    // ── subgraph ──────────────────────────────────────────────────────────────

    #[test]
    fn subgraph_returns_seed_depth0_neighbors_depth1_with_induced_edges() {
        let mut m = mem();
        // Each `add` call is the first (and only) turn of its own session, so
        // it produces exactly one Episodic node with no recipe side-edges
        // (no Temporal, no buffered Semantic/ExtractedFrom).
        let a = m.add("sA", "u", "node a", t(1)).unwrap().episodic;
        let b = m.add("sB", "u", "node b", t(2)).unwrap().episodic;
        let c = m.add("sC", "u", "node c", t(3)).unwrap().episodic;
        let ab = m.relate(a, b, Relation::Related).unwrap();
        let _bc = m.relate(b, c, Relation::Related).unwrap();

        let sg = m.subgraph(&[a], 1, 100).unwrap();

        let node_ids: std::collections::HashSet<NodeId> = sg.nodes.iter().map(|n| n.id).collect();
        assert_eq!(node_ids, std::collections::HashSet::from([a, b]));
        assert!(!node_ids.contains(&c), "C is depth 2, must be excluded");

        let depth_map: HashMap<NodeId, usize> = sg.depths.iter().cloned().collect();
        assert_eq!(depth_map.get(&a), Some(&0));
        assert_eq!(depth_map.get(&b), Some(&1));
        assert_eq!(depth_map.get(&c), None);

        let edge_ids: std::collections::HashSet<EdgeId> = sg.edges.iter().map(|e| e.id).collect();
        assert_eq!(edge_ids, std::collections::HashSet::from([ab]));
        assert!(!sg.truncated);
    }

    #[test]
    fn subgraph_respects_node_budget_sets_truncated() {
        let mut m = mem();
        // A 5-node chain: n0-n1-n2-n3-n4.
        let ids: Vec<NodeId> = (0..5)
            .map(|i| {
                m.add(&format!("s{i}"), "u", &format!("node {i}"), t(i as u64 + 1))
                    .unwrap()
                    .episodic
            })
            .collect();
        for w in ids.windows(2) {
            m.relate(w[0], w[1], Relation::Related).unwrap();
        }

        let sg = m.subgraph(&[ids[0]], 10, 2).unwrap();

        assert!(sg.nodes.len() <= 2, "budget must cap visited nodes");
        assert!(sg.truncated, "hitting the budget must set truncated");
    }

    #[test]
    fn subgraph_truncated_false_when_reachable_set_fully_collected() {
        let mut m = mem();
        // A 3-node chain (n0-n1-n2) reachable from n0 within depth 2 is
        // exactly 3 nodes; node_budget = 3 lets the BFS collect all of them
        // and exhaust its frontier before the budget is ever hit. Two extra
        // disconnected nodes push node_count above node_budget so the old
        // (buggy) global-node-count check would false-positive here.
        let chain: Vec<NodeId> = (0..3)
            .map(|i| {
                m.add(&format!("s{i}"), "u", &format!("node {i}"), t(i as u64 + 1))
                    .unwrap()
                    .episodic
            })
            .collect();
        for w in chain.windows(2) {
            m.relate(w[0], w[1], Relation::Related).unwrap();
        }
        let _extra1 = m.add("sX", "u", "disconnected x", t(100)).unwrap().episodic;
        let _extra2 = m.add("sY", "u", "disconnected y", t(101)).unwrap().episodic;

        let sg = m.subgraph(&[chain[0]], 2, 3).unwrap();

        assert_eq!(
            sg.nodes.len(),
            3,
            "the whole reachable chain must fit: {sg:?}"
        );
        assert!(
            !sg.truncated,
            "frontier fully exhausted; unrelated disconnected nodes must not \
             mark this truncated: {sg:?}"
        );
    }

    #[test]
    fn subgraph_truncated_true_when_frontier_cut_by_budget() {
        let mut m = mem();
        // A star: hub connected to 5 leaves. Depth-1 reachable set from the
        // hub is 6 nodes (hub + 5 leaves); a budget of 3 forces the BFS to
        // cut the frontier while leaves remain unvisited.
        let hub = m.add("sHub", "u", "hub node", t(1)).unwrap().episodic;
        let leaves: Vec<NodeId> = (0..5)
            .map(|i| {
                m.add(
                    &format!("sLeaf{i}"),
                    "u",
                    &format!("leaf {i}"),
                    t(i as u64 + 2),
                )
                .unwrap()
                .episodic
            })
            .collect();
        for &leaf in &leaves {
            m.relate(hub, leaf, Relation::Related).unwrap();
        }

        let sg = m.subgraph(&[hub], 1, 3).unwrap();

        assert!(sg.nodes.len() <= 3, "budget must cap visited nodes: {sg:?}");
        assert!(
            sg.truncated,
            "the frontier had unvisited in-depth leaves cut by the budget: {sg:?}"
        );
    }

    #[test]
    fn subgraph_missing_seed_is_err() {
        let m = mem();
        let result = m.subgraph(&[NodeId(9999)], 1, 100);
        assert!(result.is_err(), "a nonexistent seed must error");
    }

    // ── stats ─────────────────────────────────────────────────────────────────

    #[test]
    fn stats_counts_nodes_and_edges() {
        let mut m = mem();
        // Empty graph first.
        let empty = m.stats_at(t(0)).unwrap();
        assert_eq!(empty.node_count, 0);
        assert_eq!(empty.edge_count, 0);

        m.add("s", "a", "one", t(1)).unwrap();
        m.add("s", "a", "two", t(2)).unwrap();
        m.flush_all().unwrap();

        let s = m.stats_at(t(100)).unwrap();
        // 2 episodic + 2 semantic nodes from the recipe.
        assert!(
            s.node_count >= 4,
            "expected >= 4 nodes, got {}",
            s.node_count
        );
        assert!(
            s.edge_count >= 1,
            "expected recipe edges, got {}",
            s.edge_count
        );
        assert!((0.0..=1.0).contains(&s.orphan_ratio));
        assert!((0.0..=1.0).contains(&s.stale_ratio));
        // grade is a valid letter; just confirm it is set (no panic / valid copy).
        let _ = s.grade;
    }

    #[test]
    fn stats_counts_contradiction_edges() {
        let mut m = mem();
        let a = m.add("s", "a", "claim x is true", t(1)).unwrap().episodic;
        let b = m.add("s", "a", "claim x is false", t(2)).unwrap().episodic;
        m.flush_all().unwrap();
        m.relate(a, b, Relation::Contradicts).unwrap();

        let s = m.stats_at(t(10)).unwrap();
        assert!(
            s.contradiction_count >= 1,
            "expected a contradiction, got {}",
            s.contradiction_count
        );
    }

    // ── as_context ────────────────────────────────────────────────────────────

    #[test]
    fn as_context_renders_sections() {
        let mut m = mem();
        m.add("s", "alice", "we deploy on fridays", t(1)).unwrap();
        m.add("s", "bob", "but fridays are risky", t(2)).unwrap();
        m.flush_all().unwrap();

        let recall = m.search_at("deploy fridays", 5, t(100)).unwrap();
        let block = recall.as_context();

        // The block should be a readable string. With recipe content it will
        // contain at least one section header and the relevance annotation.
        assert!(
            block.contains("## KNOWLEDGE")
                || block.contains("## MEMORIES")
                || block.contains("## IDENTITY"),
            "expected at least one section header, got:\n{block}"
        );
        if recall.package.total_fragments() > 0 {
            assert!(
                block.contains("relevance"),
                "rendered fragments must show relevance, got:\n{block}"
            );
            assert!(
                block.contains("origin: peer #"),
                "rendered fragments must show provenance, got:\n{block}"
            );
        }
    }

    #[test]
    fn as_context_empty_package_is_empty_string() {
        let recall = Recall {
            hits: Vec::new(),
            package: ContextPackage::empty(),
        };
        assert_eq!(recall.as_context(), "");
    }

    #[test]
    fn set_metadata_persists_through_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedProvider::new());
        let id = {
            let mut m = Memory::with_provider(&path, provider.clone()).unwrap();
            let id = m
                .add("s", "user", "we chose sqlite", t(1))
                .unwrap()
                .episodic;
            m.flush_all().unwrap();
            m.set_metadata(id, "anamnesis:extracted", "false").unwrap();
            id
        };
        // Reopen: metadata must survive (only set_node writes it, not flush).
        let m2 = Memory::with_provider(&path, provider).unwrap();
        let node = m2.engine().graph().get_node(id).unwrap();
        assert_eq!(
            node.metadata.get("anamnesis:extracted").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn set_metadata_pairs_persists_both_keys_through_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta-pairs.db");
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedProvider::new());
        let id = {
            let mut m = Memory::with_provider(&path, provider.clone()).unwrap();
            let id = m.add("s", "user", "one write", t(1)).unwrap().episodic;
            m.flush_all().unwrap();
            // Both keys land in ONE set_node write — no partial-failure window.
            m.set_metadata_pairs(
                id,
                &[
                    ("anamnesis:turn_key", "abc123"),
                    ("anamnesis:extracted", "false"),
                ],
            )
            .unwrap();
            id
        };
        let m2 = Memory::with_provider(&path, provider).unwrap();
        let node = m2.engine().graph().get_node(id).unwrap();
        assert_eq!(
            node.metadata.get("anamnesis:turn_key").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(
            node.metadata.get("anamnesis:extracted").map(String::as_str),
            Some("false")
        );
    }
}
