//! Memory — the Framework API for Anamnesis.
//!
//! # Overview
//!
//! This module is the **validated consumer layer** of the Anamnesis crate. It
//! owns the production ingest and recall pipeline exercised by
//! `benches/eval_common/real_bench/graph.rs` and exposes it as the official
//! front door: `anamnesis::Memory`.
//!
//! # Recipe origin
//!
//! The encoding strategy (speaker-prefixed Episodic turn + ±1-window Semantic
//! view, `ExtractedFrom` and `Temporal` edges, session/speaker entity tags,
//! ingest-everything engine config) is the recipe validated by the LoCoMo and
//! LongMemEval harness. The harness calls the same
//! [`Memory::search_reranked`] pipeline as production consumers; absolute
//! scores still depend on model and evaluation configuration.
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
mod readout;
mod view;
pub use readout::{
    AnswerShape, DEFAULT_RERANK_CANDIDATE_LIMIT, DEFAULT_RERANK_FINAL_LIMIT,
    DEFAULT_RERANK_SEARCH_LIMIT, DeepRecallOptions, EvidenceDocument, EvidenceSelection,
    RecallIntent, RecallPlan, RerankedRecallOptions,
};
pub use view::{ListFilter, MemoryView};

use crate::Engine;
use crate::api::{
    CommitReport, EngineConfig, HealthGrade, IngestResult, Observation, TickReport,
    apply_packaging_mode, apply_validity_filter,
};
use crate::embedding::{EmbeddingProvider, RerankingProvider};
use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::types::SourceKind;
use crate::graph::types::{EdgeId, PeerId};
use crate::graph::{Edge, EdgeType, KnowledgeType, Node, NodeId, ScopePath, Timestamp};
use crate::mechanics::social::ConfidenceLevel;
use crate::query::assembly::{ScoredNode, apply_result_limit, assemble_context_package};
use crate::query::{
    AccessedSite, ActivatedTension, CoReadoutPair, CommitTrace, ContextPackage, Fragment,
    QueryConfig, SearchDiagnostics, SearchInput, SearchResult, Tension,
};
pub use crate::storage::AtomicFactId;
use crate::storage::{AtomicFact, SqliteStorage, StorageAdapter};

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
    /// Origin scope to stamp on this turn's Semantic node when finalized.
    scope: ScopePath,
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

/// Input for one isolated atomic fact.
///
/// Atomic facts are compact, reviewed extraction records used only to route a
/// query back to authoritative raw [`KnowledgeType::Episodic`] sources. They
/// are persisted in a separate sidecar table/index and never become graph
/// nodes, affect normal FTS statistics, consume graph budgets, or participate
/// in attraction and forgetting.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AtomicFactInput {
    /// Standalone atomic claim.
    pub content: String,
    /// One or more authoritative raw Episodic source nodes.
    pub source_node_ids: Vec<NodeId>,
    /// Selective entity names used as a small query-time boost.
    pub entity_tags: Vec<String>,
    /// Optional fact-validity start.
    pub valid_from: Option<Timestamp>,
    /// Optional fact-validity end.
    pub valid_until: Option<Timestamp>,
    /// Consumer metadata such as extractor version or stable external id.
    pub metadata: Vec<(String, String)>,
}

impl AtomicFactInput {
    /// Create an atomic fact citing raw source nodes.
    pub fn new(content: impl Into<String>, source_node_ids: Vec<NodeId>) -> Self {
        Self {
            content: content.into(),
            source_node_ids,
            entity_tags: Vec::new(),
            valid_from: None,
            valid_until: None,
            metadata: Vec::new(),
        }
    }

    /// Attach selective entity tags.
    pub fn with_entity_tags(mut self, entity_tags: Vec<String>) -> Self {
        self.entity_tags = entity_tags;
        self
    }

    /// Attach a half-open fact-validity window.
    pub fn with_validity(
        mut self,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Self {
        self.valid_from = valid_from;
        self.valid_until = valid_until;
        self
    }

    /// Attach consumer metadata.
    pub fn with_metadata(mut self, metadata: Vec<(String, String)>) -> Self {
        self.metadata = metadata;
        self
    }
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

/// Result of the canonical production reranked-recall pipeline.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RerankedRecall {
    /// Validated reranker ordering before source-aware final selection.
    pub ranking: Vec<RerankedCandidate>,
    /// Commit-safe final hits and context package.
    pub recall: Recall,
    /// Original cognitive score and embedding cosine for final selected nodes.
    ///
    /// Product gates calibrated on cognitive activation/cosine use this
    /// sidecar; reranker scores remain authoritative for final ordering.
    pub cognitive_scores: Vec<CognitiveRecallScore>,
}

/// Cognitive retrieval signals retained alongside a reranked final hit.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct CognitiveRecallScore {
    /// Final selected node.
    pub node_id: NodeId,
    /// Original cognitive readout score before local reranking.
    pub score: f64,
    /// Original query-embedding cosine before local reranking.
    pub cosine: f64,
}

/// The framework API — validated ingest and recall with incremental window
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
/// [`Memory::link_extracted_source`] for reviewed derived knowledge that must
/// retain source provenance. Other engine-owned relations remain outside this
/// vocabulary.
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
        self.add_in_scope(session, speaker, text, at, ScopePath::universal())
    }

    /// Like [`add`](Memory::add), but stamps the current turn and its eventual
    /// Semantic window with `scope`.
    pub fn add_in_scope(
        &mut self,
        session: &str,
        speaker: &str,
        text: &str,
        at: Timestamp,
        scope: ScopePath,
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
        let epi_embedding = embed_one_passage(&*self.provider, &speaker_text)?;

        // (b) If pending: build window and embed it. Both are fallible.
        let pending_window_result: Option<(String, Vec<f64>)> =
            if let Some(ref pending) = pending_snapshot {
                let window = build_window(
                    pending.prev_speaker_text.as_deref(),
                    &pending.speaker_text,
                    Some(&speaker_text),
                );
                let sem_embedding = embed_one_passage(&*self.provider, &window)?;
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
            scope.clone(),
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
                pending.scope.clone(),
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
            scope,
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
        self.add_single_shot_note(text, at, &session_id, opts)
    }

    /// Add one reviewed, consumer-derived `Semantic` node.
    ///
    /// This is the provenance-preserving materialization path for knowledge
    /// distilled from raw conversation turns that already exist as `Episodic`
    /// nodes. It does not manufacture a second episodic copy of the derived
    /// text. Raw sources remain authoritative and must be connected explicitly
    /// with [`link_extracted_source`](Self::link_extracted_source).
    ///
    /// `source_session_id` is retained on the node origin. `opts.scope`,
    /// `opts.tags`, and `opts.metadata` are applied to the one created
    /// `Semantic` node.
    pub fn add_derived_knowledge_with(
        &mut self,
        text: &str,
        at: Timestamp,
        source_session_id: &str,
        opts: NoteOptions,
    ) -> Result<NodeId, Error> {
        let source_session_id = source_session_id.trim();
        if source_session_id.is_empty() {
            return Err(Error::InvalidInput(
                "derived knowledge requires a non-empty source_session_id".to_owned(),
            ));
        }
        let scope = opts.scope.unwrap_or_else(ScopePath::universal);
        let mut entity_tags = entity_tags_for(source_session_id, "derived");
        entity_tags.extend(opts.tags.iter().cloned());
        entity_tags.push("anamnesis:derived".to_owned());
        entity_tags.sort();
        entity_tags.dedup();
        let embedding = embed_one_passage(&*self.provider, text)?;
        let semantic = ingest_node(
            &mut self.engine,
            text,
            text.to_owned(),
            embedding,
            KnowledgeType::Semantic,
            at,
            entity_tags,
            None,
            source_session_id,
            scope,
        )?;
        if !opts.metadata.is_empty() {
            let pairs: Vec<(&str, &str)> = opts
                .metadata
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            self.set_metadata_pairs(semantic, &pairs)?;
        }
        Ok(semantic)
    }

    /// Add one reviewed atomic fact to the isolated routing sidecar.
    ///
    /// The fact is embedded once at ingest and remains outside the cognitive
    /// graph. Every source must be a live Episodic node from the same session
    /// and scope. Query-time routing may select the fact, but only its cited raw
    /// sources can enter the normal readout/reranker lane.
    pub fn add_atomic_fact(&mut self, input: AtomicFactInput) -> Result<AtomicFactId, Error> {
        let content = input.content.trim();
        if content.is_empty() {
            return Err(Error::InvalidInput(
                "atomic fact content must not be empty".to_string(),
            ));
        }
        if input.source_node_ids.is_empty() {
            return Err(Error::InvalidInput(
                "atomic fact requires at least one raw source".to_string(),
            ));
        }
        if let (Some(valid_from), Some(valid_until)) = (input.valid_from, input.valid_until)
            && valid_until <= valid_from
        {
            return Err(Error::InvalidInput(
                "atomic fact valid_until must be greater than valid_from".to_string(),
            ));
        }

        let mut source_node_ids = Vec::with_capacity(input.source_node_ids.len());
        let mut source_session_id: Option<String> = None;
        let mut scope: Option<ScopePath> = None;
        let mut observed_at = Timestamp(0);
        for source_node_id in input.source_node_ids {
            if source_node_ids.contains(&source_node_id) {
                continue;
            }
            let source = self.engine.graph().get_node(source_node_id)?;
            if source.node_type != KnowledgeType::Episodic {
                return Err(Error::InvalidInput(format!(
                    "atomic fact source {} must be Episodic",
                    source_node_id.0
                )));
            }
            if source_session_id
                .as_ref()
                .is_some_and(|session| session != &source.origin.session_id)
            {
                return Err(Error::InvalidInput(
                    "atomic fact sources must belong to one session".to_string(),
                ));
            }
            if scope
                .as_ref()
                .is_some_and(|existing| existing != &source.origin.scope)
            {
                return Err(Error::InvalidInput(
                    "atomic fact sources must share one scope".to_string(),
                ));
            }
            source_session_id.get_or_insert_with(|| source.origin.session_id.clone());
            scope.get_or_insert_with(|| source.origin.scope.clone());
            observed_at = observed_at.max(source.created_at);
            source_node_ids.push(source_node_id);
        }

        let source_session_id = source_session_id.ok_or_else(|| {
            Error::InvalidInput("atomic fact requires a live raw source".to_string())
        })?;
        let scope = scope.unwrap_or_else(ScopePath::universal);
        let mut entity_tags: Vec<String> = input
            .entity_tags
            .into_iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .filter(|tag| {
                let normalized = tag.to_ascii_lowercase();
                !normalized.starts_with("speaker-")
                    && !normalized.starts_with("session-")
                    && normalized != "anamnesis:derived"
            })
            .collect();
        entity_tags.sort();
        entity_tags.dedup();
        let metadata = input.metadata.into_iter().collect();
        let embedding = embed_one_passage(&*self.provider, content)?;
        if embedding.is_empty() || !embedding.iter().all(|value| value.is_finite()) {
            return Err(Error::InvalidInput(
                "atomic fact embedding must be non-empty and finite".to_string(),
            ));
        }

        let storage = self.engine.graph_mut().storage_mut();
        let id = storage.next_atomic_fact_id()?;
        storage.set_atomic_fact(AtomicFact {
            id,
            content: content.to_owned(),
            embedding,
            source_node_ids,
            entity_tags,
            source_session_id,
            scope,
            observed_at,
            valid_from: input.valid_from,
            valid_until: input.valid_until,
            metadata,
        })?;
        Ok(id)
    }

    /// Delete one atomic fact from the isolated routing sidecar.
    pub fn delete_atomic_fact(&mut self, id: AtomicFactId) -> Result<(), Error> {
        self.engine.graph_mut().storage_mut().delete_atomic_fact(id)
    }

    /// Number of live records in the isolated atomic-fact sidecar.
    pub fn atomic_fact_count(&self) -> usize {
        self.engine.graph().storage().all_atomic_fact_ids().len()
    }

    fn add_single_shot_note(
        &mut self,
        text: &str,
        at: Timestamp,
        session_id: &str,
        opts: NoteOptions,
    ) -> Result<AddReceipt, Error> {
        let speaker = "note";
        let speaker_text = text.to_string();
        let scope = opts.scope.unwrap_or_else(ScopePath::universal);

        let mut entity_tags = entity_tags_for(session_id, speaker);
        entity_tags.extend(opts.tags.iter().cloned());

        let epi_embedding = embed_one_passage(&*self.provider, &speaker_text)?;
        let epi_id = ingest_node(
            &mut self.engine,
            &speaker_text,
            speaker_text.clone(),
            epi_embedding,
            KnowledgeType::Episodic,
            at,
            entity_tags.clone(),
            None,
            session_id,
            scope.clone(),
        )?;

        // Window = just itself (no prev, no next).
        let window = speaker_text.clone();
        let sem_embedding = embed_one_passage(&*self.provider, &window)?;
        let sem_id = ingest_node(
            &mut self.engine,
            &window,
            window.clone(),
            sem_embedding,
            KnowledgeType::Semantic,
            at,
            entity_tags,
            None,
            session_id,
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
        let sem_embedding = embed_one_passage(&*self.provider, &window)?;
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
            pending.scope,
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

    /// Set or clear a node's half-open validity window.
    ///
    /// Observation time remains immutable. A bounded window must satisfy
    /// `valid_from < valid_until`; either endpoint may be omitted. The base row
    /// is written eagerly through the storage adapter.
    pub fn set_validity_window(
        &mut self,
        id: NodeId,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Result<(), Error> {
        if valid_from
            .zip(valid_until)
            .is_some_and(|(start, end)| start >= end)
        {
            return Err(Error::InvalidInput(
                "validity window requires valid_from < valid_until".to_owned(),
            ));
        }
        let mut node = self.engine.graph().get_node(id)?.clone();
        node.valid_from = valid_from;
        node.valid_until = valid_until;
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
    /// never inhibitory. Engine-internal edge types (`Temporal`, etc.) and the
    /// time-mutating `Supersedes` are intentionally *not* reachable here.
    /// Reviewed consumer-derived knowledge can retain raw provenance through
    /// [`link_extracted_source`](Memory::link_extracted_source).
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

    /// Link reviewed derived knowledge to one authoritative raw source turn.
    ///
    /// This is the narrow consumer front door for [`EdgeType::ExtractedFrom`].
    /// It preserves the distinction between agent-authored [`Relation`] edges
    /// and provenance edges owned by an extraction/materialization workflow.
    /// `derived` must be a `Semantic` node and `source` must be an `Episodic`
    /// node. Repeating the same link is idempotent and returns the existing
    /// edge id.
    pub fn link_extracted_source(
        &mut self,
        derived: NodeId,
        source: NodeId,
    ) -> Result<EdgeId, Error> {
        let derived_node = self.engine.graph().get_node(derived)?;
        let source_node = self.engine.graph().get_node(source)?;
        if derived_node.node_type != KnowledgeType::Semantic {
            return Err(Error::InvalidInput(
                "derived extraction node must be Semantic".to_owned(),
            ));
        }
        if source_node.node_type != KnowledgeType::Episodic {
            return Err(Error::InvalidInput(
                "extraction source node must be Episodic".to_owned(),
            ));
        }
        if let Some(existing) = self.neighbors(derived)?.into_iter().find(|neighbor| {
            neighbor.direction == Direction::Outgoing
                && neighbor.node == source
                && neighbor.edge_type == EdgeType::ExtractedFrom
        }) {
            return Ok(existing.edge);
        }
        self.engine.link(derived, source, EdgeType::ExtractedFrom)
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

/// Consumer-selectable context rendering style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContextRenderStyle {
    /// Full sections, scores, temporal metadata, and provenance.
    #[default]
    Detailed,
    /// Compact evidence blocks grouped by source session and ordered by
    /// observation time within each group.
    Evidence,
}

/// Options for [`Memory::render_context_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextRenderOptions {
    /// Context layout used by the consumer.
    pub style: ContextRenderStyle,
    /// Resolve explicit relative expressions in evidence against each
    /// fragment's immutable observation time.
    pub resolve_relative_times: bool,
}

impl ContextRenderOptions {
    /// Construct options for one rendering style.
    pub fn with_style(style: ContextRenderStyle) -> Self {
        Self {
            style,
            ..Self::default()
        }
    }

    /// Enable or disable deterministic relative-time annotations.
    pub fn with_relative_time_resolution(mut self, enabled: bool) -> Self {
        self.resolve_relative_times = enabled;
        self
    }
}

/// One consumer-supplied score on the readout candidates of a [`SearchResult`].
///
/// Anamnesis deliberately does not own or call a reranking model. A consumer
/// can score the candidates locally (for example with a cross-encoder), then
/// pass the ordered scores to [`Memory::repackage_reranked`]. The cognitive
/// readout remains available in [`SearchResult::trace`] for provenance and
/// observability.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerankedCandidate {
    /// Candidate node from [`SearchResult::trace`].
    pub node_id: NodeId,
    /// Consumer-supplied finite ranking score; higher ranks first.
    pub score: f64,
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
    /// fails (writing into a `String` is infallible). It intentionally preserves
    /// the original package-only wire. Consumers that have the originating
    /// [`Memory`] should prefer [`Memory::render_context`], which also renders
    /// observation and validity times from the source nodes.
    pub fn as_context(&self) -> String {
        render_context_package(&self.package, None, None)
    }
}

#[derive(Debug, Clone, Copy)]
struct FragmentTime {
    observed_at: Timestamp,
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
}

fn render_context_package(
    pkg: &ContextPackage,
    times: Option<&HashMap<NodeId, FragmentTime>>,
    relative_times: Option<&HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>>>,
) -> String {
    let mut out = String::new();

    render_section(&mut out, "IDENTITY", &pkg.identity, times, relative_times);
    render_section(&mut out, "KNOWLEDGE", &pkg.knowledge, times, relative_times);
    render_section(&mut out, "MEMORIES", &pkg.memories, times, relative_times);

    if !pkg.tensions.is_empty() {
        out.push_str("## TENSIONS\n");
        for tension in &pkg.tensions {
            render_tension(&mut out, tension);
        }
        out.push('\n');
    }

    out
}

fn render_evidence_context(
    pkg: &ContextPackage,
    times: &HashMap<NodeId, FragmentTime>,
    relative_times: Option<&HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>>>,
) -> String {
    let mut groups: Vec<(String, Vec<&Fragment>)> = Vec::new();
    for fragment in pkg
        .identity
        .iter()
        .chain(pkg.knowledge.iter())
        .chain(pkg.memories.iter())
    {
        let session = fragment.origin.session_id.clone();
        if let Some((_, fragments)) = groups.iter_mut().find(|(key, _)| key == &session) {
            if !fragments
                .iter()
                .any(|existing| existing.node_id == fragment.node_id)
            {
                fragments.push(fragment);
            }
        } else {
            groups.push((session, vec![fragment]));
        }
    }
    for (_, fragments) in &mut groups {
        fragments.sort_by(|left, right| {
            times
                .get(&left.node_id)
                .map(|time| time.observed_at)
                .cmp(&times.get(&right.node_id).map(|time| time.observed_at))
                .then_with(|| right.relevance.total_cmp(&left.relevance))
                .then_with(|| left.node_id.cmp(&right.node_id))
        });
    }
    groups.sort_by(
        |(left_session, left_fragments), (right_session, right_fragments)| {
            let earliest = |fragments: &[&Fragment]| {
                fragments
                    .iter()
                    .filter_map(|fragment| times.get(&fragment.node_id))
                    .map(|time| time.observed_at)
                    .min()
            };
            earliest(left_fragments)
                .cmp(&earliest(right_fragments))
                .then_with(|| left_session.cmp(right_session))
        },
    );

    let mut out = String::new();
    out.push_str("## EVIDENCE\n");
    let mut evidence_index = 1usize;
    for (session, fragments) in groups {
        let raw_turn_lines: HashSet<String> = fragments
            .iter()
            .filter(|fragment| fragment.node_type == KnowledgeType::Episodic)
            .filter_map(|fragment| fragment.content.as_deref())
            .flat_map(|content| content.lines())
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        let _ = writeln!(out, "### session \"{session}\"");
        for fragment in fragments {
            let body = evidence_fragment_body(fragment, &raw_turn_lines);
            if body.is_empty() {
                continue;
            }
            let _ = write!(
                out,
                "- [E{evidence_index}] [{}]",
                node_type_label(&fragment.node_type)
            );
            if let Some(time) = times.get(&fragment.node_id) {
                let _ = write!(out, " observed {}", format_timestamp_utc(time.observed_at));
                if time.valid_from.is_some() || time.valid_until.is_some() {
                    let valid_from = time
                        .valid_from
                        .map_or_else(|| "-∞".to_string(), format_timestamp_utc);
                    let valid_until = time
                        .valid_until
                        .map_or_else(|| "+∞".to_string(), format_timestamp_utc);
                    let _ = write!(out, "; valid [{valid_from}, {valid_until})");
                }
            }
            out.push('\n');
            for line in body.lines() {
                let _ = writeln!(out, "    {line}");
            }
            render_relative_times(
                &mut out,
                fragment.node_id,
                relative_times,
                times.get(&fragment.node_id).map(|time| time.observed_at),
                "    ",
            );
            evidence_index = evidence_index.saturating_add(1);
        }
    }
    if !pkg.tensions.is_empty() {
        out.push_str("## TENSIONS\n");
        for tension in &pkg.tensions {
            render_tension(&mut out, tension);
        }
    }
    out.push('\n');
    out
}

fn evidence_fragment_body(fragment: &Fragment, raw_turn_lines: &HashSet<String>) -> String {
    let Some(content) = fragment.content.as_deref() else {
        return fragment
            .summary
            .as_deref()
            .unwrap_or(&fragment.name)
            .trim()
            .to_string();
    };
    if fragment.node_type != KnowledgeType::Semantic {
        return content.trim().to_string();
    }

    // Semantic windows deliberately overlap adjacent raw turns. Keep the
    // window-only context, but let an exact Episodic fragment be the sole
    // rendered copy when that source turn is already in the package. This is a
    // presentation-only coalescing pass: the package, provenance graph, commit
    // trace, and every raw fragment remain untouched.
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !raw_turn_lines.contains(*line))
        .collect::<Vec<_>>()
        .join("\n")
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
fn render_section(
    out: &mut String,
    title: &str,
    frags: &[Fragment],
    times: Option<&HashMap<NodeId, FragmentTime>>,
    relative_times: Option<&HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>>>,
) {
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
        if let Some(time) = times.and_then(|values| values.get(&f.node_id)) {
            let _ = write!(
                out,
                "    └ time: observed {}",
                format_timestamp_utc(time.observed_at)
            );
            if time.valid_from.is_some() || time.valid_until.is_some() {
                let valid_from = time
                    .valid_from
                    .map_or_else(|| "-∞".to_string(), format_timestamp_utc);
                let valid_until = time
                    .valid_until
                    .map_or_else(|| "+∞".to_string(), format_timestamp_utc);
                let _ = write!(out, "; valid [{valid_from}, {valid_until})");
            }
            out.push('\n');
        }
        render_relative_times(
            out,
            f.node_id,
            relative_times,
            times
                .and_then(|values| values.get(&f.node_id))
                .map(|time| time.observed_at),
            "    ",
        );
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

fn render_relative_times(
    out: &mut String,
    node_id: NodeId,
    relative_times: Option<&HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>>>,
    observed_at: Option<Timestamp>,
    indentation: &str,
) {
    let Some(resolutions) = relative_times.and_then(|values| values.get(&node_id)) else {
        return;
    };
    for resolution in resolutions {
        let _ = writeln!(
            out,
            "{indentation}└ resolved relative time: \"{}\" = {}",
            resolution.phrase,
            format_relative_time_resolution(resolution, observed_at)
        );
    }
}

fn utc_date_parts(timestamp: Timestamp) -> (i64, i64, i64) {
    let days = (timestamp.0 / 1_000 / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn format_natural_date(timestamp: Timestamp) -> String {
    const MONTH_NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let (year, month, day) = utc_date_parts(timestamp);
    let month_name = usize::try_from(month - 1)
        .ok()
        .and_then(|index| MONTH_NAMES.get(index))
        .copied()
        .unwrap_or("unknown month");
    format!("{day} {month_name} {year}")
}

fn format_relative_time_resolution(
    resolution: &crate::query::temporal::RelativeTimeResolution,
    observed_at: Option<Timestamp>,
) -> String {
    use crate::query::temporal::RelativeTimePrecision;

    let absolute = match resolution.precision {
        RelativeTimePrecision::Day => format_natural_date(Timestamp(resolution.range.start)),
        RelativeTimePrecision::Month => {
            let natural = format_natural_date(Timestamp(resolution.range.start));
            natural
                .split_once(' ')
                .map_or(natural.clone(), |(_, month_year)| month_year.to_owned())
        }
        RelativeTimePrecision::Range => format!(
            "{} through {}",
            format_natural_date(Timestamp(resolution.range.start)),
            format_natural_date(Timestamp(resolution.range.end))
        ),
    };
    let relation = observed_at.and_then(|observed_at| {
        let phrase = resolution.phrase.to_lowercase();
        let anchor = format_natural_date(observed_at);
        match phrase.as_str() {
            "yesterday" => Some(format!("the day before {anchor}")),
            "last night" => Some(format!("the night before {anchor}")),
            "tomorrow" => Some(format!("the day after {anchor}")),
            "last week" => Some(format!("the week before {anchor}")),
            "this week" => Some(format!("the week of {anchor}")),
            "next week" => Some(format!("the week after {anchor}")),
            "last weekend" => Some(format!("the weekend before {anchor}")),
            "next weekend" => Some(format!("the weekend after {anchor}")),
            "last month" => Some(format!("the month before {anchor}")),
            "this month" => Some(format!("the month of {anchor}")),
            "next month" => Some(format!("the month after {anchor}")),
            value if value.ends_with(" ago") => Some(format!(
                "{} before {anchor}",
                value.trim_end_matches(" ago")
            )),
            value if value.starts_with("last ") => Some(format!(
                "the {} before {anchor}",
                weekday_display(value.trim_start_matches("last "))
            )),
            value if value.starts_with("next ") => Some(format!(
                "the {} after {anchor}",
                weekday_display(value.trim_start_matches("next "))
            )),
            _ => None,
        }
    });
    relation.map_or(absolute.clone(), |relation| {
        format!("{absolute}; relation: {relation}")
    })
}

fn weekday_display(value: &str) -> &str {
    match value {
        "mon" => "Monday",
        "tue" | "tues" => "Tuesday",
        "wed" => "Wednesday",
        "thu" | "thur" | "thurs" => "Thursday",
        "fri" => "Friday",
        "sat" => "Saturday",
        "sun" => "Sunday",
        other => other,
    }
}

fn format_timestamp_utc(timestamp: Timestamp) -> String {
    let total_seconds = timestamp.0 / 1_000;
    let seconds_of_day = total_seconds % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = utc_date_parts(timestamp);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
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
    fn recall_fragment_times(
        &self,
        recall: &Recall,
    ) -> Result<HashMap<NodeId, FragmentTime>, Error> {
        let mut times = HashMap::new();
        for fragment in recall
            .package
            .identity
            .iter()
            .chain(recall.package.knowledge.iter())
            .chain(recall.package.memories.iter())
        {
            if times.contains_key(&fragment.node_id) {
                continue;
            }
            let node = self.engine.graph().get_node(fragment.node_id)?;
            times.insert(
                fragment.node_id,
                FragmentTime {
                    observed_at: node.created_at,
                    valid_from: node.valid_from,
                    valid_until: node.valid_until,
                },
            );
        }
        Ok(times)
    }

    fn recall_relative_time_resolutions(
        &self,
        recall: &Recall,
        times: &HashMap<NodeId, FragmentTime>,
        query: Option<&str>,
    ) -> HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>> {
        const TEMPORAL_EVIDENCE_LIMIT: usize = 4;

        let mut resolutions = HashMap::new();
        let mut fragments: Vec<_> = recall
            .package
            .identity
            .iter()
            .chain(recall.package.knowledge.iter())
            .chain(recall.package.memories.iter())
            .collect();
        fragments.sort_by(|left, right| {
            right
                .relevance
                .total_cmp(&left.relevance)
                .then_with(|| left.node_id.cmp(&right.node_id))
        });
        for fragment in fragments.into_iter().take(TEMPORAL_EVIDENCE_LIMIT) {
            let Some(time) = times.get(&fragment.node_id) else {
                continue;
            };
            let body = fragment
                .content
                .as_deref()
                .or(fragment.summary.as_deref())
                .unwrap_or(fragment.name.as_str());
            if query.is_some_and(|query| !readout::temporal_evidence_matches(query, body)) {
                continue;
            }
            let mut fragment_resolutions = Vec::new();
            for source_line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
                for resolution in crate::query::temporal::resolve_relative_time_cues(
                    source_line,
                    time.observed_at.0,
                ) {
                    if !fragment_resolutions.contains(&resolution) {
                        fragment_resolutions.push(resolution);
                    }
                }
            }
            if !fragment_resolutions.is_empty() {
                resolutions.insert(fragment.node_id, fragment_resolutions);
            }
        }
        resolutions
    }

    /// Render a recall with source-node temporal metadata.
    ///
    /// This is the product context wire for consumers that need to resolve
    /// relative expressions such as "last week" or "next month". It preserves
    /// [`Recall::as_context`]'s sections and provenance, while adding each
    /// fragment's immutable observation time and optional half-open validity
    /// window. Source fragments added during packaging are looked up directly,
    /// so they receive timestamps even when they were not standalone ranked
    /// [`Hit`] values.
    ///
    /// The method is read-only. It returns an error if a packaged source node
    /// can no longer be read instead of silently emitting incomplete temporal
    /// context.
    pub fn render_context(&self, recall: &Recall) -> Result<String, Error> {
        self.render_context_with(recall, ContextRenderOptions::default())
    }

    /// Render context using deterministic query intent.
    ///
    /// Temporal questions receive reference-blind annotations that resolve
    /// explicit relative expressions against each fragment's immutable
    /// observation time. Other query intents are byte-for-byte equivalent to
    /// [`render_context`](Memory::render_context). The annotations preserve the
    /// original evidence and never inspect an expected answer or call a model.
    pub fn render_context_for(&self, query: &str, recall: &Recall) -> Result<String, Error> {
        self.render_context_for_with(query, recall, ContextRenderOptions::default())
    }

    /// Render one consumer-selected layout with deterministic query intent.
    pub fn render_context_for_with(
        &self,
        query: &str,
        recall: &Recall,
        options: ContextRenderOptions,
    ) -> Result<String, Error> {
        let plan = RecallPlan::infer(query);
        self.render_context_for_plan_with(&plan, recall, options)
    }

    /// Render context using a precomputed deterministic recall plan.
    ///
    /// This additive route lets structured consumers provide an explicit
    /// [`AnswerShape`] while keeping evidence matching, temporal compilation,
    /// and rendering inside `Memory`.
    pub fn render_context_for_plan_with(
        &self,
        plan: &RecallPlan,
        recall: &Recall,
        mut options: ContextRenderOptions,
    ) -> Result<String, Error> {
        if matches!(
            plan.answer_shape,
            AnswerShape::Temporal | AnswerShape::Frequency
        ) {
            options.resolve_relative_times = true;
        }
        self.render_context_internal(recall, options, Some(&plan.query))
    }

    /// Render a recall through a consumer-selected product context style.
    ///
    /// Both styles read the exact same validated [`ContextPackage`]. `Evidence`
    /// changes only presentation: it groups fragments by source session,
    /// orders sessions by their earliest evidence and fragments by observation
    /// time, and removes score/origin prose that answer readers do not need. It
    /// does not remove nodes from the package, alter commit traces, or mutate
    /// graph state.
    pub fn render_context_with(
        &self,
        recall: &Recall,
        options: ContextRenderOptions,
    ) -> Result<String, Error> {
        self.render_context_internal(recall, options, None)
    }

    fn render_context_internal(
        &self,
        recall: &Recall,
        options: ContextRenderOptions,
        query: Option<&str>,
    ) -> Result<String, Error> {
        let times = self.recall_fragment_times(recall)?;
        let relative_times = options
            .resolve_relative_times
            .then(|| self.recall_relative_time_resolutions(recall, &times, query));
        match options.style {
            ContextRenderStyle::Detailed => Ok(render_context_package(
                &recall.package,
                Some(&times),
                relative_times.as_ref(),
            )),
            ContextRenderStyle::Evidence => Ok(render_evidence_context(
                &recall.package,
                &times,
                relative_times.as_ref(),
            )),
        }
    }

    /// Search memory at wall-clock `now`.
    ///
    /// Equivalent to `search_at(query, limit, Timestamp::now())`. For deterministic
    /// or time-travel queries use [`search_at`](Memory::search_at) instead.
    pub fn search(&mut self, query: &str, limit: usize) -> Result<Recall, Error> {
        self.search_at(query, limit, Timestamp::now())
    }

    /// Search memory with an optional query scope for scope-aware ranking.
    pub fn search_scoped(
        &mut self,
        query: &str,
        limit: usize,
        scope: Option<ScopePath>,
    ) -> Result<Recall, Error> {
        self.search_scoped_at(query, limit, scope, Timestamp::now())
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
        self.search_scoped_at(query, limit, None, now)
    }

    /// Run the model-free, source-aware readout at wall-clock time.
    ///
    /// This is the higher-quality counterpart to [`search`](Memory::search).
    /// It uses the same engine search and package validation, then lets the
    /// [`Memory`] facade compile the ranked graph nodes into distinct evidence
    /// units according to [`DeepRecallOptions`]. No generative model is called.
    pub fn search_deep(
        &mut self,
        query: &str,
        options: DeepRecallOptions,
    ) -> Result<Recall, Error> {
        self.search_deep_at(query, options, Timestamp::now())
    }

    /// Run deterministic source-aware readout at an explicit timestamp.
    ///
    /// The search remains read-only. Pending turns are flushed by
    /// [`search_result_at_with`](Memory::search_result_at_with), and the
    /// resulting cognitive ranking is compiled through the same commit-safe
    /// reranked package path exposed to local cross-encoder consumers.
    pub fn search_deep_at(
        &mut self,
        query: &str,
        options: DeepRecallOptions,
        now: Timestamp,
    ) -> Result<Recall, Error> {
        let result =
            self.search_result_at_with(query, options.limit, now, &SearchTuning::default())?;
        let ranking: Vec<_> = result
            .trace
            .readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect();
        if ranking.is_empty() {
            return Ok(Recall {
                hits: Vec::new(),
                package: result.package,
            });
        }
        self.repackage_reranked_deep_at(query, &result, &ranking, options, now)
    }

    /// Run the canonical production recall pipeline at wall-clock time.
    ///
    /// This performs cognitive search, compiles canonical source-aware evidence
    /// documents, invokes the supplied local reranker, and passes its scores
    /// through deep selection and commit-safe package reconstruction.
    pub fn search_reranked(
        &mut self,
        query: &str,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
    ) -> Result<RerankedRecall, Error> {
        self.search_reranked_at(query, reranker, options, Timestamp::now())
    }

    /// Run canonical production recall at an explicit timestamp.
    pub fn search_reranked_at(
        &mut self,
        query: &str,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
        now: Timestamp,
    ) -> Result<RerankedRecall, Error> {
        if options.candidate_limit == 0 {
            return Err(Error::InvalidInput(
                "reranked recall candidate limit must be greater than zero".to_owned(),
            ));
        }
        if options.deep.limit == 0 {
            return Err(Error::InvalidInput(
                "reranked recall result limit must be greater than zero".to_owned(),
            ));
        }
        if options.search_limit == 0 {
            return Err(Error::InvalidInput(
                "reranked recall search limit must be greater than zero".to_owned(),
            ));
        }
        let diagnostics =
            SearchDiagnostics::with_readout_trace_limit(options.candidate_limit.max(200));
        let scope = options.scope.clone().unwrap_or_else(ScopePath::universal);
        let result = self.search_result_scoped_at_with_diagnostics(
            query,
            options.search_limit,
            now,
            &SearchTuning::default(),
            &diagnostics,
            scope,
        )?;
        self.rerank_search_result_at(query, &result, reranker, options, now)
    }

    /// Apply the canonical production rerank and deep-selection stages to an
    /// existing live [`SearchResult`].
    ///
    /// Benchmarks use this overload so retrieval diagnostics can observe the
    /// same source search without re-running it. Product callers normally use
    /// [`search_reranked`](Memory::search_reranked). `query` must be the
    /// original query used to produce `result`.
    pub fn rerank_search_result_at(
        &self,
        query: &str,
        result: &SearchResult,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
        as_of: Timestamp,
    ) -> Result<RerankedRecall, Error> {
        if options.candidate_limit == 0 {
            return Err(Error::InvalidInput(
                "reranked recall candidate limit must be greater than zero".to_owned(),
            ));
        }
        if options.deep.limit == 0 {
            return Err(Error::InvalidInput(
                "reranked recall result limit must be greater than zero".to_owned(),
            ));
        }

        let evidence = self.rerank_documents(query, result, options.candidate_limit)?;
        if evidence.is_empty() {
            return Ok(RerankedRecall {
                ranking: Vec::new(),
                recall: Recall {
                    hits: Vec::new(),
                    package: result.package.clone(),
                },
                cognitive_scores: Vec::new(),
            });
        }

        let documents: Vec<_> = evidence
            .iter()
            .map(|document| document.text.clone())
            .collect();
        let scores = reranker.rerank(query, &documents)?;
        if scores.is_empty() {
            return Err(Error::InvalidInput(format!(
                "reranker {:?} returned no scores for {} documents",
                reranker.model_name(),
                documents.len()
            )));
        }

        let mut seen = HashSet::new();
        let mut ranking = Vec::with_capacity(scores.len());
        for score in scores {
            if !score.score.is_finite() {
                return Err(Error::NonFinite(format!(
                    "reranker score at document index {}",
                    score.index
                )));
            }
            if !seen.insert(score.index) {
                return Err(Error::InvalidInput(format!(
                    "reranker returned duplicate document index {}",
                    score.index
                )));
            }
            let document = evidence.get(score.index).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "reranker returned out-of-bounds document index {} for {} documents",
                    score.index,
                    evidence.len()
                ))
            })?;
            ranking.push(RerankedCandidate {
                node_id: document.node_id,
                score: score.score,
            });
        }
        ranking.sort_by(|left, right| right.score.total_cmp(&left.score));
        let recall =
            self.repackage_reranked_deep_at(query, result, &ranking, options.deep, as_of)?;
        let final_ids: HashSet<_> = recall.hits.iter().map(|hit| hit.node_id).collect();
        let cognitive_scores = result
            .trace
            .readout
            .iter()
            .filter(|candidate| final_ids.contains(&candidate.node_id))
            .map(|candidate| CognitiveRecallScore {
                node_id: candidate.node_id,
                score: candidate.score,
                cosine: candidate.embedding_cosine,
            })
            .collect();
        Ok(RerankedRecall {
            ranking,
            recall,
            cognitive_scores,
        })
    }

    fn search_scoped_at(
        &mut self,
        query: &str,
        limit: usize,
        scope: Option<ScopePath>,
        now: Timestamp,
    ) -> Result<Recall, Error> {
        // Flush pending buffers so the last buffered turn is searchable.
        self.flush_all()?;

        // Embed the query via the provider.
        let embedding = embed_one_query(&*self.provider, query)?;

        // Build bench-default SearchInput: text + query_embedding + limit +
        // seed_limit = Some(limit.max(1)); speaker cues OFF; now = explicit.
        let input = SearchInput {
            text: query.to_string(),
            query_embedding: Some(embedding),
            limit,
            seed_limit: Some(limit.max(1)),
            now,
            scope: scope.unwrap_or_else(ScopePath::universal),
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
        self.search_result_at_with_diagnostics(
            query,
            limit,
            now,
            tuning,
            &SearchDiagnostics::default(),
        )
    }

    /// Diagnostic variant of [`search_result_at_with`](Self::search_result_at_with).
    ///
    /// `diagnostics` changes only the number of pre-packaging readout rows
    /// retained in the returned trace. It cannot change activation, ranking,
    /// package contents, or commit semantics.
    pub fn search_result_at_with_diagnostics(
        &mut self,
        query: &str,
        limit: usize,
        now: Timestamp,
        tuning: &SearchTuning,
        diagnostics: &SearchDiagnostics,
    ) -> Result<SearchResult, Error> {
        self.search_result_scoped_at_with_diagnostics(
            query,
            limit,
            now,
            tuning,
            diagnostics,
            ScopePath::universal(),
        )
    }

    fn search_result_scoped_at_with_diagnostics(
        &mut self,
        query: &str,
        limit: usize,
        now: Timestamp,
        tuning: &SearchTuning,
        diagnostics: &SearchDiagnostics,
        scope: ScopePath,
    ) -> Result<SearchResult, Error> {
        self.flush_all()?;

        let embedding = embed_one_query(&*self.provider, query)?;
        let seed_limit = tuning.seed_limit.unwrap_or_else(|| limit.max(1));
        let input = SearchInput {
            text: query.to_string(),
            query_embedding: Some(embedding.clone()),
            limit,
            seed_limit: Some(seed_limit),
            now,
            scope: scope.clone(),
            entity_tags: tuning.entity_tags.clone(),
            ..SearchInput::default()
        };
        let mut result = self.engine.search_with_diagnostics(input, diagnostics)?;
        let plan = RecallPlan::infer(query);
        let routed = readout::route_atomic_fact_sources(
            self.engine.graph().storage(),
            &plan,
            &embedding,
            now,
            &scope,
        )?;
        if routed.is_empty() {
            return Ok(result);
        }

        // Preserve the proven cognitive head exactly. Atomic facts only earn
        // source slots in the deeper lane; direct/temporal shapes never reach
        // this branch. Existing raw candidates keep their native score signals
        // when promoted, while a source absent from the trace receives the
        // fact-lane synthetic diagnostic score.
        let head_limit = result.trace.readout.len().min(30);
        let head_ids: HashSet<_> = result
            .trace
            .readout
            .iter()
            .take(head_limit)
            .map(|candidate| candidate.node_id)
            .collect();
        let mut head_sources = HashSet::new();
        for candidate in result.trace.readout.iter().take(head_limit) {
            head_sources.extend(readout::canonical_sources(
                self.engine.graph().storage(),
                candidate.node_id,
            )?);
        }
        // The sidecar may expose up to the complete 20-row tail for later
        // coverage/session-bridge selection. Collections reserve twelve raw
        // slots because each distinct fact may be a required list item; other
        // shapes admit only four so an entity-rich fact cluster cannot replace
        // the native tail wholesale.
        let direct_atomic_promotion_limit = if plan.answer_shape == AnswerShape::Collection {
            12
        } else {
            4
        };
        let mut routed_ids = Vec::new();
        let mut promoted = Vec::new();
        let mut deferred = Vec::new();
        for routed_source in routed {
            let routed_candidate = routed_source.candidate;
            if head_ids.contains(&routed_candidate.node_id)
                || head_sources.contains(&routed_candidate.node_id)
                || routed_ids
                    .iter()
                    .any(|(node_id, _)| *node_id == routed_candidate.node_id)
            {
                continue;
            }
            routed_ids.push((routed_candidate.node_id, routed_source.kind_priority));
            if promoted.len() < direct_atomic_promotion_limit {
                let candidate = result
                    .trace
                    .readout
                    .iter()
                    .position(|existing| existing.node_id == routed_candidate.node_id)
                    .map(|position| result.trace.readout.remove(position))
                    .unwrap_or(routed_candidate);
                promoted.push(candidate);
            } else if !result
                .trace
                .readout
                .iter()
                .any(|existing| existing.node_id == routed_candidate.node_id)
            {
                deferred.push(routed_candidate);
            }
        }
        for (offset, candidate) in promoted.into_iter().enumerate() {
            result.trace.readout.insert(
                (head_limit + offset).min(result.trace.readout.len()),
                candidate,
            );
        }
        let native_capacity = diagnostics
            .readout_trace_limit
            .saturating_sub(deferred.len());
        result.trace.readout.truncate(native_capacity);
        result.trace.readout.extend(deferred);
        result
            .trace
            .readout
            .truncate(diagnostics.readout_trace_limit);
        routed_ids.retain(|(node_id, _)| {
            result
                .trace
                .readout
                .iter()
                .any(|candidate| candidate.node_id == *node_id)
        });
        result
            .trace
            .strategies_used
            .push("atomic_fact_routing".to_owned());
        if !routed_ids.is_empty() {
            let routed_ids = routed_ids
                .iter()
                .map(|(node_id, kind_priority)| format!("{}@{kind_priority}", node_id.0))
                .collect::<Vec<_>>()
                .join(",");
            result
                .trace
                .strategies_used
                .push(format!("atomic_fact_sources:{routed_ids}"));
        }
        Ok(result)
    }

    /// Validate consumer-supplied reranker scores and rebuild a commit-safe recall.
    ///
    /// The ranking must contain unique node ids drawn from `result.trace.readout`
    /// and finite scores. Scores are sorted descending with the cognitive readout
    /// order as the deterministic tie-breaker. Package assembly, tension endpoint
    /// preservation, and token accounting use the same rules as native search.
    ///
    /// This method is intentionally model-agnostic and read-only. It does not call
    /// an LLM or reranker. The returned package's commit trace is restricted to the
    /// fragments actually exposed after reranking, so [`Memory::used`] never
    /// reinforces discarded baseline results.
    ///
    /// Node validity is evaluated at [`Timestamp::now`]. Consumers replaying a
    /// historical or deterministic query should call
    /// [`repackage_reranked_at`](Memory::repackage_reranked_at) with the same
    /// timestamp used for the source search.
    ///
    /// Path-current reinforcement can only be preserved for edges present in the
    /// source result's commit trace; the public search trace does not expose raw
    /// path currents. Access and co-readout reinforcement are rebuilt from the
    /// selected nodes and their captured activations.
    pub fn repackage_reranked(
        &self,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        limit: usize,
    ) -> Result<Recall, Error> {
        self.repackage_reranked_at(result, ranking, limit, Timestamp::now())
    }

    /// Compile the live cognitive readout into canonical raw-evidence documents.
    ///
    /// External rerankers should prefer this source-aware surface when
    /// overlapping Semantic windows would otherwise repeat the same raw turns.
    /// Each returned document keeps a representative node from
    /// `result.trace.readout`, so its score can be passed back through
    /// [`repackage_reranked`](Memory::repackage_reranked) without consumer-side
    /// source grouping. Raw Episodic fragments remain authoritative and are
    /// emitted at most once in cognitive-rank order.
    ///
    /// The method is model-free and read-only. `candidate_limit` limits the
    /// cognitive readout rows inspected, not the number of documents returned.
    pub fn evidence_documents(
        &self,
        result: &SearchResult,
        candidate_limit: usize,
    ) -> Result<Vec<EvidenceDocument>, Error> {
        if candidate_limit == 0 {
            return Err(Error::InvalidInput(
                "evidence document candidate limit must be greater than zero".to_owned(),
            ));
        }
        readout::compile_evidence_documents(
            self.engine.graph().storage(),
            &result.trace.readout,
            candidate_limit,
        )
    }

    /// Compile reranker documents using the deterministic [`RecallPlan`].
    ///
    /// Enumeration and relational queries use canonical raw-evidence
    /// documents to protect distinct facts from overlapping graph windows.
    /// Inference documents retain their highest-ranked Semantic representative
    /// while exposing only canonical raw evidence as text, so later rendering
    /// can recover the evidence window. Direct and temporal queries preserve
    /// the ordinary node-document surface. This is the recommended
    /// minimal-consumer entry point: a consumer only scores the returned text
    /// and passes the node scores back to
    /// [`repackage_reranked_deep`](Memory::repackage_reranked_deep).
    pub fn rerank_documents(
        &self,
        query: &str,
        result: &SearchResult,
        candidate_limit: usize,
    ) -> Result<Vec<EvidenceDocument>, Error> {
        if candidate_limit == 0 {
            return Err(Error::InvalidInput(
                "rerank document candidate limit must be greater than zero".to_owned(),
            ));
        }
        let plan = RecallPlan::infer(query);
        let mut routed_atomic_sources = Vec::new();
        for strategy in &result.trace.strategies_used {
            let Some(encoded_sources) = strategy.strip_prefix("atomic_fact_sources:") else {
                continue;
            };
            for encoded_source in encoded_sources.split(',') {
                let (encoded_id, encoded_priority) = encoded_source
                    .split_once('@')
                    .unwrap_or((encoded_source, "0"));
                let Ok(source_id) = encoded_id.parse::<u64>() else {
                    continue;
                };
                let Ok(kind_priority) = encoded_priority.parse::<usize>() else {
                    continue;
                };
                let source = (NodeId(source_id), kind_priority);
                if !routed_atomic_sources
                    .iter()
                    .any(|(node_id, _)| *node_id == source.0)
                {
                    routed_atomic_sources.push(source);
                }
            }
        }
        readout::compile_rerank_documents(
            self.engine.graph().storage(),
            &plan,
            &result.trace.readout,
            candidate_limit,
            &routed_atomic_sources,
        )
    }

    /// Compile consumer scores through the model-free deep readout at wall-clock time.
    ///
    /// The consumer supplies only scores. Query intent detection, canonical raw
    /// source grouping, evidence selection, validity, packaging, and commit
    /// trace reconstruction remain owned by [`Memory`].
    pub fn repackage_reranked_deep(
        &self,
        query: &str,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        options: DeepRecallOptions,
    ) -> Result<Recall, Error> {
        self.repackage_reranked_deep_at(query, result, ranking, options, Timestamp::now())
    }

    /// Compile consumer scores through deterministic deep readout at `as_of`.
    ///
    /// [`EvidenceSelection::Auto`] preserves reranker order for direct queries
    /// so downstream filters can retain alternate knowledge representations.
    /// It applies canonical raw-source coverage for inference and date queries,
    /// and bounded source-session coverage for explicit
    /// collection/relationship/frequency queries. `query` must be the original
    /// query used to produce `result` because the same deterministic plan also
    /// controls evidence document compilation and query-aware rendering. The
    /// compiled ranking is then validated by
    /// [`repackage_reranked_at`](Memory::repackage_reranked_at), so no source,
    /// validity, tension, budget, or commit invariant is bypassed.
    pub fn repackage_reranked_deep_at(
        &self,
        query: &str,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        options: DeepRecallOptions,
        as_of: Timestamp,
    ) -> Result<Recall, Error> {
        let plan = RecallPlan::infer(query);
        let compiled = readout::compile_ranking(
            self.engine.graph().storage(),
            &plan,
            ranking,
            options.selection,
            options.limit,
        )?;
        self.repackage_reranked_at(result, &compiled, options.limit, as_of)
    }

    /// Rebuild a commit-safe recall from consumer scores at an explicit time.
    ///
    /// This is the deterministic/historical counterpart to
    /// [`repackage_reranked`](Memory::repackage_reranked). It reapplies the source
    /// search's packaging mode, contradiction discovery, and half-open validity
    /// windows at `as_of` before the final result limit. Passing `Timestamp(0)`
    /// preserves the native search sentinel semantics and skips node validity
    /// filtering while evaluating contradiction validity at the current time.
    ///
    /// The ranking validation and read-only guarantees are identical to
    /// [`repackage_reranked`](Memory::repackage_reranked).
    pub fn repackage_reranked_at(
        &self,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        limit: usize,
        as_of: Timestamp,
    ) -> Result<Recall, Error> {
        if limit == 0 {
            return Err(Error::InvalidInput(
                "reranked result limit must be greater than zero".to_string(),
            ));
        }
        if ranking.is_empty() {
            return Err(Error::InvalidInput(
                "reranked candidate list must not be empty".to_string(),
            ));
        }

        let readout_positions: HashMap<NodeId, usize> = result
            .trace
            .readout
            .iter()
            .enumerate()
            .map(|(index, candidate)| (candidate.node_id, index))
            .collect();
        let readout_by_id: HashMap<NodeId, _> = result
            .trace
            .readout
            .iter()
            .map(|candidate| (candidate.node_id, candidate))
            .collect();
        let mut seen = HashSet::new();
        let mut ranked = Vec::with_capacity(ranking.len());
        for candidate in ranking {
            if !candidate.score.is_finite() {
                return Err(Error::NonFinite(format!(
                    "reranker score for node {:?}",
                    candidate.node_id
                )));
            }
            if !seen.insert(candidate.node_id) {
                return Err(Error::InvalidInput(format!(
                    "duplicate reranked node {:?}",
                    candidate.node_id
                )));
            }
            let Some(position) = readout_positions.get(&candidate.node_id).copied() else {
                return Err(Error::InvalidInput(format!(
                    "reranked node {:?} is absent from the search readout",
                    candidate.node_id
                )));
            };
            ranked.push((*candidate, position));
        }
        ranked.sort_by(|(left, left_position), (right, right_position)| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left_position.cmp(right_position))
        });

        let mut scored_nodes = Vec::with_capacity(ranked.len());
        for (candidate, _) in &ranked {
            let node = self.engine.graph().get_node(candidate.node_id)?;
            scored_nodes.push(ScoredNode {
                node_id: candidate.node_id,
                name: node.name.clone(),
                summary: node.summary.clone(),
                content: node.content.clone(),
                node_type: node.node_type.clone(),
                relevance: candidate.score,
                origin: node.origin.clone(),
            });
        }

        let activations: HashMap<NodeId, f64> = ranked
            .iter()
            .filter_map(|(candidate, _)| {
                readout_by_id
                    .get(&candidate.node_id)
                    .map(|readout| (candidate.node_id, readout.activation))
            })
            .collect();
        let contradiction_time = if as_of.0 > 0 { as_of } else { Timestamp::now() };
        let (contradiction_pairs, _) = crate::query::assembly::collect_contradiction_pairs(
            self.engine.graph().storage(),
            &activations,
            0.0,
            contradiction_time,
        );
        let identity_activations: Vec<_> = scored_nodes
            .iter()
            .filter(|node| matches!(node.node_type, KnowledgeType::Identity))
            .filter_map(|node| {
                readout_by_id
                    .get(&node.node_id)
                    .map(|candidate| (node.node_id, node.node_type.clone(), candidate.activation))
            })
            .collect();
        let config = QueryConfig::default();
        let packaging_mode = result
            .trace
            .packaging_mode
            .clone()
            .unwrap_or(crate::query::PackagingMode::Balanced);
        let assemble = |nodes| {
            let mut package = assemble_context_package(
                nodes,
                &identity_activations,
                &contradiction_pairs,
                config.token_budget,
                config.chars_per_token,
            );
            apply_packaging_mode(&self.engine, packaging_mode.clone(), &mut package);
            if as_of.0 > 0 {
                apply_validity_filter(&self.engine, &mut package, as_of);
            }
            package
        };

        // The first pass decides the exact surviving candidate set using the
        // established packaging-mode, validity, result-limit, and tension
        // preservation semantics. Reassemble only that final set so candidates
        // discarded by the result limit do not permanently consume the token
        // budget and strand surviving Episodic fragments at synthetic L0/L1
        // labels.
        let mut package = assemble(scored_nodes.clone());
        apply_result_limit(&mut package, limit, config.chars_per_token);
        let initially_selected: HashSet<NodeId> = package
            .identity
            .iter()
            .chain(package.knowledge.iter())
            .chain(package.memories.iter())
            .map(|fragment| fragment.node_id)
            .collect();
        let selected_scored_nodes = scored_nodes
            .into_iter()
            .filter(|node| initially_selected.contains(&node.node_id))
            .collect();
        package = assemble(selected_scored_nodes);
        apply_result_limit(&mut package, limit, config.chars_per_token);

        let selected_ids: HashSet<NodeId> = package
            .identity
            .iter()
            .chain(package.knowledge.iter())
            .chain(package.memories.iter())
            .map(|fragment| fragment.node_id)
            .collect();
        package.commit_trace = self.reranked_commit_trace(result, &selected_ids, &readout_by_id);

        let hits = ranked
            .iter()
            .filter(|(candidate, _)| selected_ids.contains(&candidate.node_id))
            .take(limit)
            .map(|(candidate, _)| {
                let node = self.engine.graph().get_node(candidate.node_id)?;
                let readout = readout_by_id.get(&candidate.node_id).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "reranked node {:?} disappeared from readout",
                        candidate.node_id
                    ))
                })?;
                let (speaker, session) = parse_entity_tags(&node.entity_tags);
                Ok(Hit {
                    node_id: candidate.node_id,
                    text: node.content.clone(),
                    score: candidate.score,
                    cosine: readout.embedding_cosine,
                    at: node.created_at,
                    speaker,
                    session,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(Recall { hits, package })
    }

    fn reranked_commit_trace(
        &self,
        result: &SearchResult,
        selected_ids: &HashSet<NodeId>,
        readout_by_id: &HashMap<NodeId, &crate::query::ReadoutCandidate>,
    ) -> CommitTrace {
        let mut selected: Vec<_> = selected_ids.iter().copied().collect();
        selected.sort_by_key(|node_id| {
            result
                .trace
                .readout
                .iter()
                .position(|candidate| candidate.node_id == *node_id)
                .unwrap_or(usize::MAX)
        });

        let accessed = selected
            .iter()
            .filter_map(|node_id| {
                readout_by_id.get(node_id).map(|candidate| AccessedSite {
                    node_id: *node_id,
                    readout_work: candidate.activation.clamp(0.0, 1.0),
                })
            })
            .collect();
        let mut co_readout = Vec::new();
        for (index, node_a) in selected.iter().enumerate() {
            for node_b in &selected[index + 1..] {
                let connected = self
                    .engine
                    .graph()
                    .storage()
                    .edges_from(*node_a)
                    .iter()
                    .any(|edge_id| {
                        self.engine
                            .graph()
                            .storage()
                            .get_edge(*edge_id)
                            .is_ok_and(|edge| {
                                edge.target == *node_b
                                    && !matches!(edge.edge_type, EdgeType::Contradicts)
                            })
                    })
                    || self
                        .engine
                        .graph()
                        .storage()
                        .edges_from(*node_b)
                        .iter()
                        .any(|edge_id| {
                            self.engine
                                .graph()
                                .storage()
                                .get_edge(*edge_id)
                                .is_ok_and(|edge| {
                                    edge.target == *node_a
                                        && !matches!(edge.edge_type, EdgeType::Contradicts)
                                })
                        });
                if connected
                    && let (Some(a), Some(b)) =
                        (readout_by_id.get(node_a), readout_by_id.get(node_b))
                {
                    co_readout.push(CoReadoutPair {
                        node_a: *node_a,
                        node_b: *node_b,
                        activation_a: a.activation,
                        activation_b: b.activation,
                    });
                }
            }
        }

        CommitTrace {
            accessed,
            co_readout,
            path_used: result
                .package
                .commit_trace
                .path_used
                .iter()
                .filter(|path| {
                    selected_ids.contains(&path.source) && selected_ids.contains(&path.target)
                })
                .cloned()
                .collect(),
            tensions_activated: package_tensions_to_trace(&result.package, selected_ids),
        }
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

fn package_tensions_to_trace(
    package: &ContextPackage,
    selected_ids: &HashSet<NodeId>,
) -> Vec<ActivatedTension> {
    package
        .tensions
        .iter()
        .filter(|tension| {
            selected_ids.contains(&tension.node_a) && selected_ids.contains(&tension.node_b)
        })
        .map(|tension| ActivatedTension {
            node_a: tension.node_a,
            node_b: tension.node_b,
            stress: tension.stress,
        })
        .collect()
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

fn embed_one_query(provider: &dyn EmbeddingProvider, text: &str) -> Result<Vec<f64>, Error> {
    Ok(crate::embedding::widen(&provider.embed_query(text)?))
}

fn embed_one_passage(provider: &dyn EmbeddingProvider, text: &str) -> Result<Vec<f64>, Error> {
    Ok(crate::embedding::widen(&provider.embed_passage(text)?))
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
    use crate::embedding::RerankScore;

    /// Deterministic, model-free embedding provider.
    ///
    /// Produces a fixed-dimension unit-ish vector seeded by a per-text hash so
    /// that distinct texts get distinct (low-similarity) embeddings — enough to
    /// avoid crystallize's dedup rejection — while being fully reproducible. No
    /// network / model download.
    struct HashEmbedProvider {
        dim: usize,
    }

    struct KeywordReranker;

    impl RerankingProvider for KeywordReranker {
        fn rerank(&self, _query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error> {
            Ok(documents
                .iter()
                .enumerate()
                .map(|(index, document)| RerankScore {
                    index,
                    score: if document.contains("cobalt") {
                        10.0
                    } else {
                        0.0
                    },
                })
                .collect())
        }

        fn model_name(&self) -> &str {
            "keyword-test"
        }
    }

    struct InvalidIndexReranker;

    impl RerankingProvider for InvalidIndexReranker {
        fn rerank(&self, _query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error> {
            Ok(vec![RerankScore {
                index: documents.len(),
                score: 1.0,
            }])
        }

        fn model_name(&self) -> &str {
            "invalid-index-test"
        }
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

    type EmbedCalls = Arc<std::sync::Mutex<Vec<(&'static str, String)>>>;

    struct RecordingProvider {
        calls: EmbedCalls,
    }

    impl EmbeddingProvider for RecordingProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            Ok(texts.iter().map(|_| vec![0.25; 8]).collect())
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>, Error> {
            self.calls.lock().unwrap().push(("query", text.to_string()));
            self.embed_single(text)
        }

        fn embed_passage(&self, text: &str) -> Result<Vec<f32>, Error> {
            self.calls
                .lock()
                .unwrap()
                .push(("passage", text.to_string()));
            self.embed_single(text)
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn model_name(&self) -> &str {
            "recording"
        }
    }

    fn recording_mem() -> (Memory<SqliteStorage>, EmbedCalls) {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(RecordingProvider {
            calls: calls.clone(),
        });
        (
            Memory::in_memory_with_provider(provider).expect("in-memory Memory"),
            calls,
        )
    }

    #[test]
    fn memory_uses_passage_for_add_and_query_for_search() {
        let (mut m, calls) = recording_mem();
        m.add("s", "user", "본문", t(1)).unwrap();
        let _ = m.search("질의", 3).unwrap();

        let calls = calls.lock().unwrap();
        let kinds: Vec<&str> = calls.iter().map(|(kind, _)| *kind).collect();
        assert!(kinds.contains(&"passage"), "{kinds:?}");
        assert!(kinds.contains(&"query"), "{kinds:?}");
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

    #[test]
    fn production_reranked_recall_owns_documents_scoring_and_deep_packaging() {
        let defaults = RerankedRecallOptions::new(4);
        assert_eq!(defaults.candidate_limit, DEFAULT_RERANK_CANDIDATE_LIMIT);
        assert_eq!(defaults.search_limit, DEFAULT_RERANK_SEARCH_LIMIT);
        assert_eq!(defaults.deep.limit, 4);

        let mut m = mem();
        m.add_note("The archive key is amber", t(1)).unwrap();
        m.add_note("The deployment key is cobalt", t(2)).unwrap();

        let output = m
            .search_reranked_at(
                "Which deployment key should I use?",
                &KeywordReranker,
                RerankedRecallOptions::new(4).with_candidate_limit(20),
                t(100),
            )
            .unwrap();

        assert!(!output.ranking.is_empty());
        assert_eq!(output.ranking[0].score, 10.0);
        assert!(
            output
                .recall
                .hits
                .first()
                .is_some_and(|hit| hit.text.contains("cobalt"))
        );
        assert!(
            output
                .recall
                .package
                .commit_trace
                .accessed
                .iter()
                .any(|site| site.node_id == output.ranking[0].node_id)
        );
    }

    #[test]
    fn production_reranked_recall_rejects_invalid_provider_indices() {
        let mut m = mem();
        m.add_note("A bounded reranker candidate", t(1)).unwrap();
        let error = m
            .search_reranked_at(
                "bounded candidate",
                &InvalidIndexReranker,
                RerankedRecallOptions::new(2).with_candidate_limit(10),
                t(100),
            )
            .unwrap_err();
        assert!(matches!(error, Error::InvalidInput(message) if message.contains("out-of-bounds")));
    }

    #[test]
    fn repackage_reranked_uses_consumer_order_and_commit_matches_selected_fragments() {
        let mut m = mem();
        for index in 0..8 {
            m.add_note(&format!("memory fact number {index}"), t(index + 1))
                .unwrap();
        }
        let result = m
            .search_result_at_with(
                "memory fact",
                5,
                t(100),
                &SearchTuning {
                    seed_limit: Some(5),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        assert!(result.trace.readout.len() >= 4);

        let source: Vec<_> = result.trace.readout.iter().take(4).collect();
        let ranking = vec![
            RerankedCandidate {
                node_id: source[3].node_id,
                score: 4.0,
            },
            RerankedCandidate {
                node_id: source[2].node_id,
                score: 3.0,
            },
            RerankedCandidate {
                node_id: source[1].node_id,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: source[0].node_id,
                score: 1.0,
            },
        ];

        let recall = m.repackage_reranked(&result, &ranking, 2).unwrap();
        assert_eq!(
            recall
                .hits
                .iter()
                .map(|hit| hit.node_id)
                .collect::<Vec<_>>(),
            vec![source[3].node_id, source[2].node_id]
        );

        let packaged_ids: HashSet<_> = recall
            .package
            .identity
            .iter()
            .chain(recall.package.knowledge.iter())
            .chain(recall.package.memories.iter())
            .map(|fragment| fragment.node_id)
            .collect();
        let accessed_ids: HashSet<_> = recall
            .package
            .commit_trace
            .accessed
            .iter()
            .map(|site| site.node_id)
            .collect();
        assert_eq!(packaged_ids, accessed_ids);
        assert_eq!(
            packaged_ids,
            HashSet::from([source[3].node_id, source[2].node_id])
        );

        let report = m.used(recall).unwrap();
        assert_eq!(report.sites_accessed, 2);
    }

    #[test]
    fn deep_readout_groups_semantic_and_episodic_representations_by_raw_source() {
        let mut m = mem();
        let first = m.add_note("Alice repaired the blue bicycle", t(1)).unwrap();
        let second = m.add_note("Bob repaired the green bicycle", t(2)).unwrap();
        let first_semantic = first.finalized_semantic.unwrap();
        let second_semantic = second.finalized_semantic.unwrap();

        let result = m
            .search_result_at_with(
                "What are the bicycles that Alice and Bob repaired?",
                10,
                t(100),
                &SearchTuning::default(),
            )
            .unwrap();
        let live: HashSet<_> = result
            .trace
            .readout
            .iter()
            .map(|candidate| candidate.node_id)
            .collect();
        for expected in [
            first_semantic,
            first.episodic,
            second_semantic,
            second.episodic,
        ] {
            assert!(
                live.contains(&expected),
                "missing readout node {expected:?}"
            );
        }
        assert_eq!(
            readout::canonical_sources(m.engine().graph().storage(), first_semantic).unwrap(),
            vec![first.episodic]
        );
        assert_eq!(
            readout::canonical_sources(m.engine().graph().storage(), first.episodic).unwrap(),
            vec![first.episodic]
        );

        let ranking = [
            RerankedCandidate {
                node_id: first_semantic,
                score: 4.0,
            },
            RerankedCandidate {
                node_id: first.episodic,
                score: 3.0,
            },
            RerankedCandidate {
                node_id: second_semantic,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: second.episodic,
                score: 1.0,
            },
        ];
        let recall = m
            .repackage_reranked_deep_at(
                "What are the bicycles that Alice and Bob repaired?",
                &result,
                &ranking,
                DeepRecallOptions::new(2).with_selection(EvidenceSelection::DistinctSources),
                t(100),
            )
            .unwrap();

        assert_eq!(
            recall
                .hits
                .iter()
                .map(|hit| hit.node_id)
                .collect::<Vec<_>>(),
            vec![first_semantic, second_semantic]
        );
        assert_eq!(recall.package.total_fragments(), 2);
    }

    #[test]
    fn evidence_documents_emit_each_raw_source_once() {
        let mut m = mem();
        let first = m.add_note("Alice repaired the blue bicycle", t(1)).unwrap();
        let second = m.add_note("Bob repaired the green bicycle", t(2)).unwrap();
        let result = m
            .search_result_at_with(
                "Which bicycles were repaired?",
                10,
                t(100),
                &SearchTuning::default(),
            )
            .unwrap();

        let documents = m.evidence_documents(&result, 10).unwrap();
        let sources: Vec<_> = documents
            .iter()
            .flat_map(|document| document.source_node_ids.iter().copied())
            .collect();
        let unique_sources: HashSet<_> = sources.iter().copied().collect();

        assert_eq!(sources.len(), unique_sources.len());
        assert!(unique_sources.contains(&first.episodic));
        assert!(unique_sources.contains(&second.episodic));
        assert!(
            documents
                .iter()
                .any(|document| document.text.contains("blue bicycle"))
        );
        assert!(
            documents
                .iter()
                .any(|document| document.text.contains("green bicycle"))
        );
    }

    #[test]
    fn inference_rerank_documents_keep_semantic_representatives() {
        let mut m = mem();
        let first = m
            .add(
                "repair-session",
                "alice",
                "I repaired the blue bicycle",
                t(1),
            )
            .unwrap();
        m.add(
            "repair-session",
            "bob",
            "That repair made the bicycle safe",
            t(2),
        )
        .unwrap();
        m.flush_all().unwrap();

        let result = m
            .search_result_at_with(
                "Would Alice repair the bicycle again?",
                20,
                t(100),
                &SearchTuning {
                    seed_limit: Some(20),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        let documents = m
            .rerank_documents("Would Alice repair the bicycle again?", &result, 20)
            .unwrap();
        let representative = documents
            .iter()
            .find(|document| {
                document.source_node_ids.contains(&first.episodic)
                    && m.get(document.node_id)
                        .is_ok_and(|node| node.node_type == KnowledgeType::Semantic)
            })
            .unwrap();
        let node = m.get(representative.node_id).unwrap();

        assert_eq!(node.node_type, KnowledgeType::Semantic);
        assert!(representative.text.contains("repaired the blue bicycle"));
    }

    #[test]
    fn inference_rerank_documents_join_a_question_to_its_raw_answer() {
        let mut m = mem();
        m.add(
            "collection-session",
            "bob",
            "What is the picture on your bookshelf?",
            t(1),
        )
        .unwrap();
        let answer = m
            .add(
                "collection-session",
                "alice",
                "The picture is from Atelier Nimbus",
                t(2),
            )
            .unwrap()
            .episodic;
        m.flush_all().unwrap();

        let result = m
            .search_result_at_with(
                "Would Alice enjoy a shop related to the picture on her bookshelf?",
                20,
                t(100),
                &SearchTuning {
                    seed_limit: Some(20),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        let document = m
            .rerank_documents(
                "Would Alice enjoy a shop related to the picture on her bookshelf?",
                &result,
                20,
            )
            .unwrap()
            .into_iter()
            .find(|document| document.node_id == answer)
            .unwrap();

        assert!(document.text.contains("What is the picture"));
        assert!(document.text.contains("The picture is from Atelier Nimbus"));
        assert!(document.source_node_ids.contains(&answer));
    }

    #[test]
    fn automatic_deep_readout_deduplicates_temporal_sources_in_relevance_order() {
        let mut m = mem();
        let first = m.add_note("Alice moved in January", t(1)).unwrap();
        let second = m.add_note("Alice moved in February", t(2)).unwrap();
        let first_semantic = first.finalized_semantic.unwrap();
        let second_semantic = second.finalized_semantic.unwrap();
        let result = m
            .search_result_at_with("When did Alice move?", 4, t(100), &SearchTuning::default())
            .unwrap();
        let ranking = [
            RerankedCandidate {
                node_id: first_semantic,
                score: 4.0,
            },
            RerankedCandidate {
                node_id: first.episodic,
                score: 3.0,
            },
            RerankedCandidate {
                node_id: second_semantic,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: second.episodic,
                score: 1.0,
            },
        ];
        let ordinary = m
            .repackage_reranked_at(&result, &ranking, 4, t(100))
            .unwrap();
        let deep = m
            .repackage_reranked_deep_at(
                "When did Alice move?",
                &result,
                &ranking,
                DeepRecallOptions::new(4),
                t(100),
            )
            .unwrap();

        assert_eq!(ordinary.hits.len(), 4);
        assert_eq!(
            deep.hits.iter().map(|hit| hit.node_id).collect::<Vec<_>>(),
            vec![first_semantic, second_semantic]
        );
    }

    #[test]
    fn automatic_deep_readout_covers_sessions_before_backfilling() {
        let mut m = mem();
        let mut session_a = Vec::new();
        for index in 0..5 {
            session_a.push(
                m.add(
                    "session-a",
                    "alice",
                    &format!("shared detail alpha {index}"),
                    t(index + 1),
                )
                .unwrap()
                .episodic,
            );
        }
        let session_b = m
            .add("session-b", "alice", "shared detail beta", t(6))
            .unwrap()
            .episodic;
        m.flush_all().unwrap();

        let result = m
            .search_result_at_with(
                "Which shared details did Alice mention?",
                20,
                t(100),
                &SearchTuning {
                    seed_limit: Some(20),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        let live: HashSet<_> = result
            .trace
            .readout
            .iter()
            .map(|candidate| candidate.node_id)
            .collect();
        assert!(session_a.iter().all(|node_id| live.contains(node_id)));
        assert!(live.contains(&session_b));

        let mut ranking: Vec<_> = session_a
            .iter()
            .enumerate()
            .map(|(index, node_id)| RerankedCandidate {
                node_id: *node_id,
                score: 6.0 - index as f64,
            })
            .collect();
        ranking.push(RerankedCandidate {
            node_id: session_b,
            score: 1.0,
        });
        let recall = m
            .repackage_reranked_deep_at(
                "Which shared details did Alice mention?",
                &result,
                &ranking,
                DeepRecallOptions::new(5),
                t(100),
            )
            .unwrap();

        assert_eq!(
            recall
                .hits
                .iter()
                .map(|hit| hit.node_id)
                .collect::<Vec<_>>(),
            vec![
                session_a[0],
                session_a[1],
                session_a[2],
                session_a[3],
                session_b
            ]
        );
    }

    #[test]
    fn repackage_reranked_reclaims_discarded_candidate_budget() {
        let mut m = mem();
        for index in 0..48 {
            let payload = format!(
                "turn {index} carries a distinct durable detail {}",
                "with enough context to consume package budget ".repeat(8)
            );
            m.add("budget-session", "alice", &payload, t(index + 1))
                .unwrap();
        }
        m.flush_all().unwrap();

        let result = m
            .search_result_at_with(
                "durable detail context",
                20,
                t(100),
                &SearchTuning {
                    seed_limit: Some(20),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        let mut episodic_rank = 0usize;
        let mut other_rank = 0usize;
        let mut ranking = Vec::new();
        for candidate in &result.trace.readout {
            let node = m.engine().graph().get_node(candidate.node_id).unwrap();
            let score = if matches!(node.node_type, KnowledgeType::Episodic) {
                episodic_rank += 1;
                10_000.0 - episodic_rank as f64
            } else {
                other_rank += 1;
                1_000.0 - other_rank as f64
            };
            ranking.push(RerankedCandidate {
                node_id: candidate.node_id,
                score,
            });
        }

        let recall = m
            .repackage_reranked_at(&result, &ranking, 2, t(100))
            .unwrap();
        assert_eq!(recall.package.total_fragments(), 2);
        assert_eq!(recall.package.memories.len(), 2);
        assert!(
            recall
                .package
                .memories
                .iter()
                .all(|fragment| fragment.content.is_some()),
            "the final selected Episodic set must be reassembled at full resolution"
        );
    }

    #[test]
    fn repackage_reranked_at_reapplies_validity_windows() {
        let mut m = mem();
        let expired = m.add_note("expired memory fact", t(10)).unwrap();
        let current = m.add_note("current memory fact", t(20)).unwrap();
        for node_id in [expired.episodic, expired.finalized_semantic.unwrap()] {
            let mut node = m.engine().graph().get_node(node_id).unwrap().clone();
            node.valid_until = Some(t(50));
            m.engine_mut()
                .graph_mut()
                .storage_mut()
                .set_node(node)
                .unwrap();
        }

        let result = m
            .search_result_at_with(
                "memory fact",
                10,
                t(100),
                &SearchTuning {
                    seed_limit: Some(10),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        let readout_ids: HashSet<_> = result
            .trace
            .readout
            .iter()
            .map(|candidate| candidate.node_id)
            .collect();
        assert!(readout_ids.contains(&expired.episodic));
        assert!(readout_ids.contains(&current.episodic));

        let ranking = vec![
            RerankedCandidate {
                node_id: expired.episodic,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: current.episodic,
                score: 1.0,
            },
        ];
        let historical = m
            .repackage_reranked_at(&result, &ranking, 2, t(25))
            .unwrap();
        assert!(
            historical
                .package
                .memories
                .iter()
                .any(|fragment| fragment.node_id == expired.episodic)
        );

        let current_recall = m
            .repackage_reranked_at(&result, &ranking, 2, t(100))
            .unwrap();
        assert!(
            current_recall
                .package
                .memories
                .iter()
                .all(|fragment| fragment.node_id != expired.episodic)
        );
        assert!(
            current_recall
                .package
                .memories
                .iter()
                .any(|fragment| fragment.node_id == current.episodic)
        );
        assert!(
            current_recall
                .hits
                .iter()
                .all(|hit| hit.node_id != expired.episodic)
        );
    }

    #[test]
    fn repackage_reranked_at_reapplies_timeline_packaging() {
        let mut m = mem();
        let older = m.add_note("older history fact", t(10)).unwrap();
        let newer = m.add_note("newer history fact", t(20)).unwrap();
        let result = m
            .search_result_at_with(
                "history fact",
                10,
                t(100),
                &SearchTuning {
                    seed_limit: Some(10),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(
            result.trace.packaging_mode,
            Some(crate::query::PackagingMode::Timeline)
        );
        let ranking = vec![
            RerankedCandidate {
                node_id: newer.episodic,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: older.episodic,
                score: 1.0,
            },
        ];

        let recall = m
            .repackage_reranked_at(&result, &ranking, 10, t(100))
            .unwrap();
        let memory_ids: Vec<_> = recall
            .package
            .memories
            .iter()
            .map(|fragment| fragment.node_id)
            .collect();
        assert_eq!(memory_ids, [older.episodic, newer.episodic]);
    }

    #[test]
    fn repackage_reranked_at_reapplies_knowledge_provenance() {
        let mut m = mem();
        let note = m
            .add_note("source fragment for a derived fact", t(10))
            .unwrap();
        let semantic = note.finalized_semantic.unwrap();
        let mut result = m
            .search_result_at_with(
                "derived fact",
                10,
                t(100),
                &SearchTuning {
                    seed_limit: Some(10),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            result
                .trace
                .readout
                .iter()
                .any(|candidate| candidate.node_id == semantic)
        );
        result.trace.packaging_mode = Some(crate::query::PackagingMode::KnowledgeWithProvenance);

        let recall = m
            .repackage_reranked_at(
                &result,
                &[RerankedCandidate {
                    node_id: semantic,
                    score: 1.0,
                }],
                10,
                t(100),
            )
            .unwrap();

        assert!(
            recall
                .package
                .knowledge
                .iter()
                .any(|fragment| fragment.node_id == semantic)
        );
        assert!(recall.package.memories.iter().any(|fragment| {
            fragment.node_id == note.episodic
                && fragment.content.as_deref() == Some("source fragment for a derived fact")
        }));
    }

    #[test]
    fn repackage_reranked_at_rediscovers_tensions_from_readout() {
        let mut m = mem();
        let left = m.add_note("the release is approved", t(10)).unwrap();
        let right = m.add_note("the release is rejected", t(20)).unwrap();
        let left_semantic = left.finalized_semantic.unwrap();
        let right_semantic = right.finalized_semantic.unwrap();
        m.relate(left_semantic, right_semantic, Relation::Contradicts)
            .unwrap();
        let mut result = m
            .search_result_at_with(
                "release approved rejected",
                10,
                t(100),
                &SearchTuning {
                    seed_limit: Some(10),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        result.package.tensions.clear();
        result.trace.packaging_mode = Some(crate::query::PackagingMode::Balanced);
        let ranking = vec![
            RerankedCandidate {
                node_id: left_semantic,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: right_semantic,
                score: 1.0,
            },
        ];

        let recall = m
            .repackage_reranked_at(&result, &ranking, 10, t(100))
            .unwrap();
        assert_eq!(recall.package.tensions.len(), 1);
        assert_eq!(recall.package.tensions[0].node_a, left_semantic);
        assert_eq!(recall.package.tensions[0].node_b, right_semantic);
    }

    #[test]
    fn repackage_reranked_rejects_unknown_duplicate_and_non_finite_scores() {
        let mut m = mem();
        m.add_note("alpha memory", t(1)).unwrap();
        m.add_note("beta memory", t(2)).unwrap();
        let result = m
            .search_result_at_with("memory", 2, t(100), &SearchTuning::default())
            .unwrap();
        let known = result.trace.readout[0].node_id;

        let duplicate = [
            RerankedCandidate {
                node_id: known,
                score: 2.0,
            },
            RerankedCandidate {
                node_id: known,
                score: 1.0,
            },
        ];
        assert!(matches!(
            m.repackage_reranked(&result, &duplicate, 2),
            Err(Error::InvalidInput(_))
        ));

        let unknown = [RerankedCandidate {
            node_id: NodeId(u64::MAX),
            score: 1.0,
        }];
        assert!(matches!(
            m.repackage_reranked(&result, &unknown, 1),
            Err(Error::InvalidInput(_))
        ));

        let non_finite = [RerankedCandidate {
            node_id: known,
            score: f64::NAN,
        }];
        assert!(matches!(
            m.repackage_reranked(&result, &non_finite, 1),
            Err(Error::NonFinite(_))
        ));
    }

    #[test]
    fn add_in_scope_stamps_origin_scope_on_episodic_and_semantic() {
        let mut m = mem();
        let scope = ScopePath::new("project/anamnesis").unwrap();
        m.add_in_scope("s1", "user", "first turn", t(1), scope.clone())
            .unwrap();
        m.add_in_scope("s1", "user", "second turn", t(2), scope.clone())
            .unwrap();
        m.flush_all().unwrap();

        for id in m.engine().graph().storage().all_node_ids() {
            let node = m.engine().graph().get_node(id).unwrap();
            assert_eq!(
                node.origin.scope.as_str(),
                "project/anamnesis",
                "node {id:?}"
            );
        }
    }

    #[test]
    fn search_scoped_ranks_same_scope_above_cross_scope() {
        let mut m = mem();
        let a = ScopePath::new("project/aaa").unwrap();
        let b = ScopePath::new("project/bbb").unwrap();
        m.add_in_scope(
            "sa",
            "user",
            "shared topic phrase aaa local detail",
            t(1),
            a.clone(),
        )
        .unwrap();
        m.add_in_scope(
            "sb",
            "user",
            "shared topic phrase bbb foreign detail",
            t(2),
            b,
        )
        .unwrap();
        m.flush_all().unwrap();

        let recall = m.search_scoped("shared topic phrase", 2, Some(a)).unwrap();
        let top = m.engine().graph().get_node(recall.hits[0].node_id).unwrap();
        assert_eq!(
            top.origin.scope.as_str(),
            "project/aaa",
            "same-scope must outrank cross-scope"
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

    #[test]
    fn reviewed_extraction_source_link_is_typed_and_idempotent() {
        let mut m = mem();
        let source = m
            .add("session", "alice", "Alice moved to Seoul", t(1))
            .unwrap()
            .episodic;
        m.flush_all().unwrap();
        let derived = m
            .add_derived_knowledge_with(
                "Alice lives in Seoul",
                t(2),
                "session",
                NoteOptions::default(),
            )
            .unwrap();
        assert_eq!(
            m.engine()
                .graph()
                .get_node(derived)
                .unwrap()
                .origin
                .session_id,
            "session"
        );

        let first = m.link_extracted_source(derived, source).unwrap();
        let repeated = m.link_extracted_source(derived, source).unwrap();
        assert_eq!(first, repeated);
        assert!(m.neighbors(derived).unwrap().iter().any(|neighbor| {
            neighbor.edge == first
                && neighbor.node == source
                && neighbor.direction == Direction::Outgoing
                && neighbor.edge_type == EdgeType::ExtractedFrom
        }));
        assert!(
            m.link_extracted_source(source, source).is_err(),
            "raw episodic nodes cannot masquerade as derived knowledge"
        );
    }

    #[test]
    fn atomic_fact_lane_routes_only_raw_sources() {
        let mut m = mem();
        let source = m
            .add(
                "session",
                "alice",
                "The cobalt project was completed after the launch review",
                t(1),
            )
            .unwrap()
            .episodic;
        m.flush_all().unwrap();
        m.add_atomic_fact(
            AtomicFactInput::new("Alice completed the cobalt project", vec![source])
                .with_entity_tags(vec!["Alice".to_owned(), "cobalt project".to_owned()]),
        )
        .unwrap();

        let result = m
            .search_result_at_with_diagnostics(
                "What projects did Alice complete?",
                4,
                t(100),
                &SearchTuning::default(),
                &SearchDiagnostics::with_readout_trace_limit(20),
            )
            .unwrap();

        assert!(
            result
                .trace
                .strategies_used
                .iter()
                .any(|strategy| strategy == "atomic_fact_routing")
        );
        assert!(
            result
                .trace
                .readout
                .iter()
                .any(|candidate| candidate.node_id == source)
        );
        assert_eq!(m.atomic_fact_count(), 1);
    }

    #[test]
    fn derived_sidecar_leaves_direct_raw_readout_byte_identical() {
        let mut baseline = mem();
        let mut with_sidecar = mem();
        let mut sidecar_sources = Vec::new();
        for index in 0..40 {
            let text = format!("alpha archive raw fact number {index}");
            baseline
                .add("session", "alice", &text, t(index + 1))
                .unwrap();
            let receipt = with_sidecar
                .add("session", "alice", &text, t(index + 1))
                .unwrap();
            sidecar_sources.push(receipt.episodic);
        }
        baseline.flush_all().unwrap();
        with_sidecar.flush_all().unwrap();
        for index in 0..20 {
            with_sidecar
                .add_atomic_fact(
                    AtomicFactInput::new(
                        format!("Alice reviewed archive item {index}"),
                        vec![sidecar_sources[index as usize]],
                    )
                    .with_entity_tags(vec!["Alice".to_owned(), "archive".to_owned()]),
                )
                .unwrap();
        }

        let diagnostics = SearchDiagnostics::with_readout_trace_limit(100);
        for query in [
            "Where is the alpha archive?",
            "Why did Alice review the alpha archive?",
            "What device could Alice gift Bob for the alpha archive?",
        ] {
            let baseline_result = baseline
                .search_result_at_with_diagnostics(
                    query,
                    20,
                    t(1_000),
                    &SearchTuning::default(),
                    &diagnostics,
                )
                .unwrap();
            let sidecar_result = with_sidecar
                .search_result_at_with_diagnostics(
                    query,
                    20,
                    t(1_000),
                    &SearchTuning::default(),
                    &diagnostics,
                )
                .unwrap();

            assert_eq!(
                sidecar_result.trace.readout, baseline_result.trace.readout,
                "sidecar must preserve conservative readout for {query:?}"
            );
        }
    }

    #[test]
    fn reviewed_derived_knowledge_creates_one_semantic_node_without_an_episodic_copy() {
        let mut m = mem();
        let before = m.engine().graph().storage().all_node_ids().len();
        let derived = m
            .add_derived_knowledge_with(
                "Alice lives in Seoul",
                t(2),
                "session",
                NoteOptions {
                    metadata: vec![("candidate".to_owned(), "one".to_owned())],
                    ..NoteOptions::default()
                },
            )
            .unwrap();
        let after = m.engine().graph().storage().all_node_ids().len();
        assert_eq!(after - before, 1);
        let node = m.engine().graph().get_node(derived).unwrap();
        assert_eq!(node.node_type, KnowledgeType::Semantic);
        assert_eq!(node.origin.session_id, "session");
        assert_eq!(
            node.metadata.get("candidate").map(String::as_str),
            Some("one")
        );
        assert!(
            m.engine()
                .graph()
                .storage()
                .all_node_ids()
                .into_iter()
                .all(|node_id| m
                    .engine()
                    .graph()
                    .get_node(node_id)
                    .is_ok_and(|candidate| candidate.node_type != KnowledgeType::Episodic))
        );
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
    fn render_context_adds_source_timestamp() {
        let mut m = mem();
        let observed_at = Timestamp(1_683_504_000_000);
        m.add("s", "alice", "we deploy on fridays", observed_at)
            .unwrap();
        m.flush_all().unwrap();

        let recall = m
            .search_at("deploy fridays", 5, Timestamp(observed_at.0 + 86_400_000))
            .unwrap();
        let block = m.render_context(&recall).unwrap();

        assert!(
            block.contains("time: observed 2023-05-08T00:00:00Z"),
            "rendered fragments must expose their observation time, got:\n{block}"
        );
    }

    #[test]
    fn evidence_context_groups_sessions_and_keeps_temporal_source_text() {
        let mut m = mem();
        let first = Timestamp(1_683_504_000_000);
        m.add("session-a", "alice", "first deployment fact", first)
            .unwrap();
        m.add(
            "session-a",
            "alice",
            "second deployment fact",
            Timestamp(first.0 + 60_000),
        )
        .unwrap();
        m.add(
            "session-b",
            "bob",
            "deployment fact from the following day",
            Timestamp(first.0 + 86_400_000),
        )
        .unwrap();
        m.flush_all().unwrap();
        let recall = m
            .search_at("deployment fact", 10, Timestamp(first.0 + 2 * 86_400_000))
            .unwrap();
        let block = m
            .render_context_with(
                &recall,
                ContextRenderOptions::with_style(ContextRenderStyle::Evidence),
            )
            .unwrap();

        assert!(block.starts_with("## EVIDENCE\n"));
        assert!(block.contains("### session \"session-a\""));
        assert!(block.contains("### session \"session-b\""));
        assert!(
            block.find("session \"session-a\"") < block.find("session \"session-b\""),
            "evidence sessions must follow their earliest observation time:\n{block}"
        );
        assert!(block.contains("[E1]"));
        assert!(block.contains("observed 2023-05-08T00:00:00Z"));
        assert!(block.contains("first deployment fact"));
        assert_eq!(
            block.matches("alice: first deployment fact").count(),
            1,
            "an exact raw turn must not be repeated through its semantic window:\n{block}"
        );
        assert!(!block.contains("relevance "));
        assert!(!block.contains("origin: peer #"));
    }

    #[test]
    fn timestamp_renderer_uses_epoch_milliseconds() {
        assert_eq!(format_timestamp_utc(Timestamp(0)), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_timestamp_utc(Timestamp(1_683_554_160_000)),
            "2023-05-08T13:56:00Z"
        );
    }

    #[test]
    fn query_aware_context_resolves_relative_evidence_time_only_for_temporal_intent() {
        let mut m = mem();
        let observed_at = Timestamp(1_686_009_600_000); // 2023-06-06 00:00 UTC
        m.add(
            "session-a",
            "alice",
            "I did yoga yesterday morning.",
            observed_at,
        )
        .unwrap();
        m.flush_all().unwrap();
        let recall = m
            .search_at(
                "When did Alice do yoga?",
                10,
                Timestamp(observed_at.0 + 86_400_000),
            )
            .unwrap();

        let temporal = m
            .render_context_for("When did Alice do yoga?", &recall)
            .unwrap();
        assert!(temporal.contains("resolved relative time: \"yesterday\" = 5 June 2023"));

        let direct = m
            .render_context_for("What exercise did Alice do?", &recall)
            .unwrap();
        assert_eq!(direct, m.render_context(&recall).unwrap());
        assert!(!direct.contains("resolved relative time"));

        let wrapped = m
            .render_context_for("Could you tell me when Alice did yoga?", &recall)
            .unwrap();
        assert!(wrapped.contains("resolved relative time: \"yesterday\" = 5 June 2023"));

        let hinted_plan = RecallPlan::infer_with_answer_shape(
            "Tell me about Alice's yoga.",
            AnswerShape::Temporal,
        );
        let hinted = m
            .render_context_for_plan_with(&hinted_plan, &recall, ContextRenderOptions::default())
            .unwrap();
        assert!(hinted.contains("resolved relative time: \"yesterday\" = 5 June 2023"));
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
