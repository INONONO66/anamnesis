//! Memory — the Framework API for Anamnesis.
//!
//! # Overview
//!
//! This module is the canonical consumer layer of the Anamnesis crate. It owns
//! conversation ingestion, source-aware recall, local reranking, evidence
//! selection, rendering, and commit-safe packaging, exposed as the official
//! front door: `anamnesis::Memory`.
//!
//! # Recipe origin
//!
//! The encoding strategy preserves each speaker-prefixed `Episodic` turn,
//! adds a bounded neighboring-turn `Semantic` view, links provenance and
//! temporal sequence, and records normalized session/speaker tags. Recall uses
//! the same [`Memory::search_reranked`] contract for every consumer surface.
//!
//! # Materialized source fragments
//!
//! [`Memory::add_source_fragment`] admits immutable textual evidence that a
//! consumer has already materialized, such as a local OCR transcript, vision
//! observation, or document observation. It creates one raw `Episodic` source
//! with explicit provenance and never calls an attachment resolver or model.
//! Conversation buffering and `Semantic` window synthesis do not apply to this
//! path.
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
    DEFAULT_RERANK_SEARCH_LIMIT, DEFAULT_SIMPLE_DELIVERY_LIMIT, DeepRecallOptions,
    EventBoundaryEvidence, EvidenceDocument, EvidenceSelection, GroundedAnswerDraft,
    GroundedAnswerItem, GroundedComparedCandidate, GroundedDraftDisposition,
    GroundedDraftRecoveryAction, GroundedDraftRecoveryState, GroundedDraftStatus,
    GroundedDraftValidation, GroundedDraftValidationContext, GroundedDraftValidationError,
    GroundedDraftValidationFailure, GroundedDraftValidationResult, GroundedEvidenceFinding,
    GroundedFindingDisposition, GroundedOccurrenceActuality, GroundedOperatorInput,
    GroundedOperatorInputRole, GroundedReadoutAction, GroundedReasoningOperator,
    GroundedReasoningOperatorKind, ReaderAnswerForm, ReaderFinalDisposition, RecallCoverage,
    RecallDerivation, RecallIntent, RecallPlan, RecallReaderContract, RecallReaderStage,
    RecallSourceAttribution, ReflectionRecommendation, RequestedAnswerCardinality,
    RequestedAnswerModality, RequestedAnswerRole, RequestedAnswerSpec,
    RequestedTemporalGranularity, RerankedRecallOptions, TemporalBoundaryDirection,
    TemporalConstraint, TemporalConstraintKind,
};
pub use view::{ListFilter, MemoryView};

use crate::Engine;
use crate::api::{
    CommitReport, EngineConfig, HealthGrade, IngestResult, Observation, TickReport,
    apply_packaging_mode, apply_validity_filter,
};
pub use crate::embedding::RerankScore;
use crate::embedding::{EmbeddingProvider, RerankingProvider};
use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::types::SourceKind;
use crate::graph::types::{EdgeId, PeerId};
use crate::graph::{Edge, EdgeType, KnowledgeType, Node, NodeId, ScopePath, Timestamp};
use crate::mechanics::social::ConfidenceLevel;
use crate::query::assembly::{
    ScoredNode, apply_result_limit, assemble_context_package, estimate_tokens,
};
use crate::query::{
    AccessedSite, ActivatedTension, CoReadoutPair, CommitTrace, ContextPackage, Fragment,
    QueryConfig, SearchDiagnostics, SearchInput, SearchResult, Tension,
};
use crate::storage::{AtomicFact, AtomicFactRelation, SqliteStorage, StorageAdapter};
pub use crate::storage::{AtomicFactId, AtomicFactRelationId, AtomicFactRelationKind};

pub(super) const REVIEWED_DERIVATION_METADATA_PREFIX: &str = "anamnesis:reviewed-derivation:v1:";
pub(super) const REVIEWED_DERIVATION_SCHEMA_KEY: &str = "anamnesis:reviewed-derivation:v1:schema";
pub(super) const REVIEWED_DERIVATION_SUBJECT_KEY: &str = "anamnesis:reviewed-derivation:v1:subject";
pub(super) const REVIEWED_DERIVATION_PREDICATE_KEY: &str =
    "anamnesis:reviewed-derivation:v1:predicate";
pub(super) const REVIEWED_DERIVATION_OBJECT_KEY: &str = "anamnesis:reviewed-derivation:v1:object";
pub(super) const REVIEWED_DERIVATION_VALUE_KIND_KEY: &str =
    "anamnesis:reviewed-derivation:v1:value-kind";
pub(super) const REVIEWED_DERIVATION_POLARITY_KEY: &str =
    "anamnesis:reviewed-derivation:v1:polarity";
pub(super) const REVIEWED_DERIVATION_MODALITY_KEY: &str =
    "anamnesis:reviewed-derivation:v1:modality";
pub(super) const REVIEWED_DERIVATION_REVIEWED_BY_KEY: &str =
    "anamnesis:reviewed-derivation:v1:reviewed-by";
pub(super) const REVIEWED_DERIVATION_REVIEW_PROFILE_KEY: &str =
    "anamnesis:reviewed-derivation:v1:review-profile";
pub(super) const REVIEWED_DERIVATION_REVIEWED_AT_KEY: &str =
    "anamnesis:reviewed-derivation:v1:reviewed-at-ms";
pub(super) const REVIEWED_DERIVATION_IDEMPOTENCY_KEY: &str =
    "anamnesis:reviewed-derivation:v1:idempotency-key";

const MAX_REVIEWED_DERIVATION_PROJECTION_CHARS: usize = 2_000;
const MAX_REVIEWED_DERIVATION_COMPONENT_CHARS: usize = 512;
const MAX_REVIEWED_DERIVATION_VALUE_KIND_CHARS: usize = 128;
const MAX_REVIEW_PROVENANCE_FIELD_CHARS: usize = 512;

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

/// One immutable textual source fragment materialized by a consumer.
///
/// This is the narrow admission type for evidence that did not originate as a
/// conversational speaker turn: for example, a local OCR transcript, a local
/// vision observation, or a document observation. The consumer resolves any
/// bytes, paths, URLs, and models before calling [`Memory::add_source_fragment`];
/// the engine stores text and provenance and performs no attachment I/O or
/// model invocation on this path.
///
/// Admission creates exactly one [`KnowledgeType::Episodic`] node. It does not
/// prefix a speaker, synthesize a `Semantic` window, or participate in the
/// pending conversation buffers maintained by [`Memory::add`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SourceFragmentInput {
    /// Full immutable textual evidence. Leading and trailing whitespace is
    /// preserved so downstream evidence spans remain byte-addressable.
    pub content: String,
    /// Explicit producer, source kind, session, scope, and confidence.
    pub origin: Origin,
    /// Time at which the source was observed or materialized.
    pub observed_at: Timestamp,
    /// Optional caller-supplied embedding in this `Memory`'s vector space.
    /// `None` stores an unembedded fragment that remains available to text search.
    pub embedding: Option<Vec<f64>>,
    /// Selective entity tags used for indexing and graph attraction.
    pub entity_tags: Vec<String>,
    /// Optional half-open fact-validity start.
    pub valid_from: Option<Timestamp>,
    /// Optional half-open fact-validity end.
    pub valid_until: Option<Timestamp>,
    /// Consumer provenance such as attachment hash, processor digest, profile,
    /// source-turn identifier, or stable external identifier.
    pub metadata: Vec<(String, String)>,
}

impl SourceFragmentInput {
    /// Create an unembedded source fragment with no tags, validity, or metadata.
    pub fn new(content: impl Into<String>, origin: Origin, observed_at: Timestamp) -> Self {
        Self {
            content: content.into(),
            origin,
            observed_at,
            embedding: None,
            entity_tags: Vec::new(),
            valid_from: None,
            valid_until: None,
            metadata: Vec::new(),
        }
    }

    /// Attach a caller-supplied embedding.
    pub fn with_embedding(mut self, embedding: Vec<f64>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /// Attach selective entity tags.
    pub fn with_entity_tags(mut self, entity_tags: Vec<String>) -> Self {
        self.entity_tags = entity_tags;
        self
    }

    /// Attach a half-open validity interval.
    pub fn with_validity(
        mut self,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Self {
        self.valid_from = valid_from;
        self.valid_until = valid_until;
        self
    }

    /// Attach consumer-owned provenance metadata.
    pub fn with_metadata(mut self, metadata: Vec<(String, String)>) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Input for one isolated atomic fact.
///
/// Atomic facts are compact, source-grounded extraction records used only to
/// route a query back to authoritative raw [`KnowledgeType::Episodic`] sources.
/// This input does not assert engine-verified review provenance; consumers that
/// have an explicit review decision use [`ReviewedDerivationInput`]. Atomic
/// facts are persisted in a separate sidecar table/index and never become graph
/// nodes, affect normal FTS statistics, consume graph budgets, or participate
/// in attraction and forgetting.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AtomicFactInput {
    /// Standalone atomic claim.
    pub content: String,
    /// Optional richer text used only to compute the stored embedding.
    ///
    /// The surface itself is not persisted. This lets consumers ground an
    /// embedding in live raw evidence without duplicating that evidence into
    /// the isolated fact sidecar.
    pub embedding_surface: Option<String>,
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
            embedding_surface: None,
            source_node_ids,
            entity_tags: Vec::new(),
            valid_from: None,
            valid_until: None,
            metadata: Vec::new(),
        }
    }

    /// Use a richer, non-persisted surface for the fact embedding.
    pub fn with_embedding_surface(mut self, embedding_surface: impl Into<String>) -> Self {
        self.embedding_surface = Some(embedding_surface.into());
        self
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

/// Polarity of a reviewed routing proposition.
///
/// Polarity is retained as routing metadata. It does not turn the isolated
/// sidecar record into independently renderable evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DerivationPolarity {
    /// The proposition is affirmed by the review decision.
    Affirmed,
    /// The proposition is negated by the review decision.
    Negated,
}

impl DerivationPolarity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Affirmed => "affirmed",
            Self::Negated => "negated",
        }
    }
}

/// Modality of a reviewed routing proposition.
///
/// Consumers declare modality at formation so later retrieval can distinguish
/// an observation from a plan, possibility, or reviewed inference without
/// asking a model at query time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DerivationModality {
    /// Directly observed state or event.
    Observed,
    /// A source-attributed report.
    Reported,
    /// A reviewed conclusion derived from cited premises.
    Inferred,
    /// An uncertain claim.
    Uncertain,
    /// A claim that applies only under an explicit condition.
    Conditional,
    /// A stated plan that has not been observed as completed.
    Planned,
    /// A hypothetical possibility rather than an observed fact.
    Hypothetical,
}

impl DerivationModality {
    fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Reported => "reported",
            Self::Inferred => "inferred",
            Self::Uncertain => "uncertain",
            Self::Conditional => "conditional",
            Self::Planned => "planned",
            Self::Hypothetical => "hypothetical",
        }
    }
}

/// Typed proposition used by a reviewed derivation routing record.
///
/// The proposition is a retrieval representation, not source evidence. Its
/// subject, predicate, and object remain separately addressable so a later
/// query planner does not need to recover their roles from one generated
/// sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RoutingProposition {
    /// Canonical subject label.
    pub subject: String,
    /// Canonical or consumer-namespaced predicate.
    pub predicate: String,
    /// Canonical object or scalar value.
    pub object: String,
    /// Optional consumer-defined value kind such as `person`, `place`, or a
    /// namespaced domain kind.
    pub value_kind: Option<String>,
    /// Affirmed or negated meaning.
    pub polarity: DerivationPolarity,
    /// Observation, report, plan, possibility, or inference status.
    pub modality: DerivationModality,
}

impl RoutingProposition {
    /// Create an affirmed, reviewed inference.
    pub fn new(
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            value_kind: None,
            polarity: DerivationPolarity::Affirmed,
            modality: DerivationModality::Inferred,
        }
    }

    /// Attach an optional consumer-defined value kind.
    pub fn with_value_kind(mut self, value_kind: impl Into<String>) -> Self {
        self.value_kind = Some(value_kind.into());
        self
    }

    /// Set the proposition polarity.
    pub fn with_polarity(mut self, polarity: DerivationPolarity) -> Self {
        self.polarity = polarity;
        self
    }

    /// Set the proposition modality.
    pub fn with_modality(mut self, modality: DerivationModality) -> Self {
        self.modality = modality;
        self
    }
}

/// Required review provenance for one reviewed derivation.
///
/// The stable idempotency key binds retries to the complete admitted record.
/// Reusing it for different content, sources, validity, or review metadata is
/// rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReviewProvenance {
    /// Stable reviewer or review-process identity.
    pub reviewed_by: String,
    /// Versioned review policy or model profile.
    pub review_profile: String,
    /// Time at which the review decision was made.
    pub reviewed_at: Timestamp,
    /// Stable retry key for this complete admission decision.
    pub idempotency_key: String,
}

impl ReviewProvenance {
    /// Create explicit review provenance.
    pub fn new(
        reviewed_by: impl Into<String>,
        review_profile: impl Into<String>,
        reviewed_at: Timestamp,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            reviewed_by: reviewed_by.into(),
            review_profile: review_profile.into(),
            reviewed_at,
            idempotency_key: idempotency_key.into(),
        }
    }
}

/// Input for one reviewed derivation in the isolated routing sidecar.
///
/// A reviewed derivation may express a consumer-accepted bridge that is not a
/// verbatim source phrase. It remains routing-only: recall can use its typed
/// projection to find the cited live raw sources, but neither the proposition
/// nor `search_projection` is emitted as reader evidence.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReviewedDerivationInput {
    /// Typed routing proposition accepted by the review decision.
    pub proposition: RoutingProposition,
    /// Bounded query-facing projection embedded and stored in the isolated
    /// sidecar.
    pub search_projection: String,
    /// Authoritative raw Episodic sources containing the reviewed premises.
    pub source_node_ids: Vec<NodeId>,
    /// Selective entity names used as a query-time boost.
    pub entity_tags: Vec<String>,
    /// Optional half-open derivation-validity start.
    pub valid_from: Option<Timestamp>,
    /// Optional half-open derivation-validity end.
    pub valid_until: Option<Timestamp>,
    /// Required review identity, policy, time, and retry key.
    pub review: ReviewProvenance,
    /// Non-authoritative consumer audit metadata. Keys must be unique and
    /// consumer-owned; the `anamnesis:` namespace is reserved by the engine.
    pub metadata: Vec<(String, String)>,
}

impl ReviewedDerivationInput {
    /// Create a reviewed derivation citing raw source nodes.
    pub fn new(
        proposition: RoutingProposition,
        search_projection: impl Into<String>,
        source_node_ids: Vec<NodeId>,
        review: ReviewProvenance,
    ) -> Self {
        Self {
            proposition,
            search_projection: search_projection.into(),
            source_node_ids,
            entity_tags: Vec::new(),
            valid_from: None,
            valid_until: None,
            review,
            metadata: Vec::new(),
        }
    }

    /// Attach selective entity tags.
    pub fn with_entity_tags(mut self, entity_tags: Vec<String>) -> Self {
        self.entity_tags = entity_tags;
        self
    }

    /// Attach a half-open validity interval.
    pub fn with_validity(
        mut self,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Self {
        self.valid_from = valid_from;
        self.valid_until = valid_until;
        self
    }

    /// Attach non-authoritative consumer audit metadata.
    ///
    /// Admission rejects duplicate, whitespace-padded, or engine-owned keys.
    pub fn with_metadata(mut self, metadata: Vec<(String, String)>) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Input for one reviewed relation between isolated atomic facts.
///
/// The relation is a routing aid, not independently renderable evidence. Its
/// endpoints must already be admitted atomic facts, and recall may use it only
/// to reach the live raw Episodic sources cited by those facts.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AtomicFactRelationInput {
    /// Directed source endpoint.
    pub from_fact_id: AtomicFactId,
    /// Directed target endpoint.
    pub to_fact_id: AtomicFactId,
    /// Reviewed relation kind.
    pub kind: AtomicFactRelationKind,
    /// Stable reviewer or review-process identity.
    pub reviewed_by: String,
    /// Versioned review policy or profile.
    pub review_profile: String,
    /// Time at which the relation decision was reviewed.
    pub reviewed_at: Timestamp,
    /// Stable key used to make promotion retry-safe.
    pub idempotency_key: String,
    /// Optional half-open relation-validity interval.
    pub valid_from: Option<Timestamp>,
    /// Optional half-open relation-validity interval.
    pub valid_until: Option<Timestamp>,
    /// Non-authoritative consumer audit metadata.
    pub metadata: Vec<(String, String)>,
}

impl AtomicFactRelationInput {
    /// Create a reviewed, directed relation between two admitted facts.
    pub fn new(
        from_fact_id: AtomicFactId,
        to_fact_id: AtomicFactId,
        kind: AtomicFactRelationKind,
        reviewed_by: impl Into<String>,
        review_profile: impl Into<String>,
        reviewed_at: Timestamp,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            from_fact_id,
            to_fact_id,
            kind,
            reviewed_by: reviewed_by.into(),
            review_profile: review_profile.into(),
            reviewed_at,
            idempotency_key: idempotency_key.into(),
            valid_from: None,
            valid_until: None,
            metadata: Vec::new(),
        }
    }

    /// Attach a half-open validity interval.
    pub fn with_validity(
        mut self,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Self {
        self.valid_from = valid_from;
        self.valid_until = valid_until;
        self
    }

    /// Attach non-authoritative consumer audit metadata.
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
    /// Deterministic plan used by every stage of this recall.
    ///
    /// Pass this exact value to [`Memory::readout_for_plan`] or
    /// [`Memory::render_context_for_plan_with`] so structured intent is not
    /// inferred again after retrieval and packaging.
    pub plan: RecallPlan,
    /// Reranker ordering retained after completion-time document and authority
    /// revalidation, before source-aware final selection.
    pub ranking: Vec<RerankedCandidate>,
    /// Exact validated document surface supplied to the reranking provider.
    ///
    /// This sidecar is retained for local observability and deterministic
    /// score replay. It precedes provider-side subset selection and therefore
    /// can be wider than [`Self::ranking`]. Reader-facing evidence remains
    /// [`Self::recall`].
    pub rerank_documents: Vec<EvidenceDocument>,
    /// Commit-safe final hits and context package.
    pub recall: Recall,
    /// Original cognitive score and embedding cosine for final selected nodes.
    ///
    /// Product gates calibrated on cognitive activation/cosine use this
    /// sidecar; reranker scores remain authoritative for final ordering. A raw
    /// source outside the captured readout inherits the cognitive signals of
    /// the live document representative that caused it to be delivered.
    pub cognitive_scores: Vec<CognitiveRecallScore>,
    validation_context: GroundedDraftValidationContext,
    validation_binding: Option<RerankedRecallValidationBinding>,
}

impl RerankedRecall {
    /// Return the event-boundary source roles retained by prepared-rerank
    /// completion, when the complete role group survived visible delivery.
    pub fn event_boundary_evidence(&self) -> Option<&EventBoundaryEvidence> {
        self.validation_binding
            .as_ref()
            .filter(|binding| {
                binding.matches(self)
                    && self
                        .validation_context
                        .is_bound_to(&binding.plan, &binding.readout.source_node_ids)
            })
            .and_then(|_| self.validation_context.event_boundary_evidence())
    }
}

#[derive(Debug, Clone)]
struct RerankedRecallValidationBinding {
    plan: RecallPlan,
    ranking: Vec<RerankedCandidate>,
    rerank_documents: Vec<EvidenceDocument>,
    recall: Recall,
    cognitive_scores: Vec<CognitiveRecallScore>,
    readout: RecallReadoutValidationBinding,
}

impl RerankedRecallValidationBinding {
    fn matches(&self, reranked: &RerankedRecall) -> bool {
        self.plan == reranked.plan
            && self.ranking == reranked.ranking
            && self.rerank_documents == reranked.rerank_documents
            && recall_surface_is_identical(&self.recall, &reranked.recall)
            && self.cognitive_scores == reranked.cognitive_scores
    }
}

fn recall_surface_is_identical(expected: &Recall, actual: &Recall) -> bool {
    expected.hits.len() == actual.hits.len()
        && expected.hits.iter().zip(&actual.hits).all(|(left, right)| {
            left.node_id == right.node_id
                && left.text == right.text
                && left.score.to_bits() == right.score.to_bits()
                && left.cosine.to_bits() == right.cosine.to_bits()
                && left.at == right.at
                && left.speaker == right.speaker
                && left.session == right.session
        })
        && expected.package == actual.package
}

/// Opaque receipt for one externally scored production rerank.
///
/// A receipt owns the exact plan, source-search snapshot, evidence documents,
/// delivered and scoring-only raw-source allocations, scope, timestamp, and
/// recall options captured by
/// [`Memory::prepare_rerank_for_plan_at`]. Its fields are deliberately private:
/// consumers may inspect the immutable reranker texts, but only
/// [`Memory::complete_prepared_rerank`] can turn document-index scores back
/// into a commit-safe [`RerankedRecall`].
///
/// Preparing first ensures that no raw text is exposed to an external reranker
/// until every contributing source's current authority, validity, and scope have
/// been checked. Completing consumes the receipt, revalidates the same source
/// allocations, and exactly recompiles every scored document after the reranker
/// returns. The receipt is process-local, single-use, and can be completed only
/// by the `Memory` instance that created it.
pub struct PreparedRerank {
    authority: Arc<PreparedRerankAuthority>,
    plan: RecallPlan,
    result: SearchResult,
    evidence: Vec<EvidenceDocument>,
    bindings: HashMap<NodeId, BoundEvidenceDocument>,
    scope: ScopePath,
    as_of: Timestamp,
    options: RerankedRecallOptions,
}

/// Process-local capability that binds a prepared receipt to one `Memory`.
///
/// The allocation identity, rather than its zero-sized value, is authoritative.
/// It remains private so a consumer cannot manufacture or transfer a receipt
/// between independent memory namespaces.
struct PreparedRerankAuthority;

impl std::fmt::Debug for PreparedRerank {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedRerank")
            .field("document_count", &self.evidence.len())
            .finish_non_exhaustive()
    }
}

impl PreparedRerank {
    /// Query text bound to the prepared plan.
    pub fn query(&self) -> &str {
        &self.plan.query
    }

    /// Number of validated documents awaiting scores.
    pub fn document_count(&self) -> usize {
        self.evidence.len()
    }

    /// Exact validated documents whose positions external scores address.
    ///
    /// The display text and delivery-source ids remain distinct from each
    /// document's [`EvidenceDocument::rerank_text`] scoring surface.
    pub fn documents(&self) -> &[EvidenceDocument] {
        &self.evidence
    }

    /// Iterate over the exact texts whose indices [`RerankScore`] must address.
    pub fn rerank_texts(&self) -> impl ExactSizeIterator<Item = &str> {
        self.evidence.iter().map(|document| document.rerank_text())
    }
}

/// Authority snapshot for one node that contributed to a reranker document.
///
/// This stays internal: callers continue to exchange ordinary node ids and
/// scores, while the canonical reranked path keeps enough information to
/// reject a deleted, replaced, or edited evidence source after the reranker
/// returns.
#[derive(Debug, Clone)]
struct BoundEvidenceNode {
    node_id: NodeId,
    incarnation: String,
    node_type: KnowledgeType,
    scope: ScopePath,
}

/// One reranker document bound to the exact graph allocations that supplied
/// its representative, canonical evidence, and scoring-only dialogue context.
#[derive(Debug, Clone)]
struct BoundEvidenceDocument {
    representative: BoundEvidenceNode,
    sources: Vec<BoundEvidenceNode>,
    scoring_sources: Vec<BoundEvidenceNode>,
    representative_text_is_bound: bool,
}

struct BoundRerankRepackage {
    recall: Recall,
    hydrated_readout: Vec<crate::query::ReadoutCandidate>,
    eligible_ranking: Vec<RerankedCandidate>,
    event_boundary_group: Option<readout::EventBoundarySourceGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RerankedValidityPolicy {
    Current,
    HistoricalTrend,
}

impl RerankedValidityPolicy {
    fn for_plan(plan: &RecallPlan) -> Self {
        if readout::plan_allows_historical_expired(plan) {
            Self::HistoricalTrend
        } else {
            Self::Current
        }
    }
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

/// The framework API — canonical ingest and recall with incremental window
/// finalization.
///
/// `Memory<S>` wraps an [`Engine<S>`] and manages per-session buffering so
/// that each `add` call produces the same graph topology as uninterrupted
/// batch ingestion. The default storage type is [`SqliteStorage`] (in-memory SQLite).
///
/// See the [module docs](self) for design and buffering semantics.
pub struct Memory<S: StorageAdapter + Clone = SqliteStorage> {
    engine: Engine<S>,
    provider: Arc<dyn EmbeddingProvider>,
    sessions: HashMap<String, SessionBuffer>,
    prepared_rerank_authority: Arc<PreparedRerankAuthority>,
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
    /// Compatibility field; always `0` because the peer registry is not part
    /// of the current engine.
    pub peer_count: usize,
    /// Overall structural health grade (A/B/C/D).
    pub grade: HealthGrade,
}

// ── Canonical Engine config used by Memory ───────────────────────────────────

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
            prepared_rerank_authority: Arc::new(PreparedRerankAuthority),
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
            prepared_rerank_authority: Arc::new(PreparedRerankAuthority),
        })
    }
}

// ── Core API (generic over S) ─────────────────────────────────────────────────

impl<S: StorageAdapter + Clone> Memory<S> {
    /// Add a conversational turn using the canonical incremental recipe.
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

    /// Admit one already-materialized textual evidence fragment.
    ///
    /// The fragment is stored as exactly one raw [`KnowledgeType::Episodic`]
    /// node with the supplied [`Origin`], observation time, validity, tags,
    /// metadata, and optional embedding. Unlike [`add`](Memory::add), this
    /// method does not prefix a speaker, create a neighboring-turn `Semantic`
    /// view, add a `Temporal` edge, or inspect or mutate any pending session
    /// buffer. Unlike [`add_note`](Memory::add_note), it does not manufacture a
    /// second node.
    ///
    /// Attachment bytes, URLs, OCR, and vision models are deliberately outside
    /// this API. A plugin or direct crate consumer runs those operations under
    /// its own local policy and supplies their immutable textual result and
    /// provenance here. The resulting node is an ordinary raw source: it can be
    /// cited by [`add_atomic_fact`](Memory::add_atomic_fact), is protected by
    /// source-incarnation checks, and obeys normal scope, validity, retraction,
    /// forgetting, and readout rules.
    ///
    /// Metadata is included in the initial base-node storage write. Engine-owned
    /// source-incarnation keys are rejected rather than silently accepted or
    /// overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] for blank content or session identity,
    /// non-finite/out-of-range confidence, an empty/non-finite/wrong-dimension
    /// embedding, an invalid validity interval, malformed or duplicate metadata
    /// keys, or engine-owned incarnation metadata.
    pub fn add_source_fragment(&mut self, input: SourceFragmentInput) -> Result<NodeId, Error> {
        let SourceFragmentInput {
            content,
            origin,
            observed_at,
            embedding,
            entity_tags,
            valid_from,
            valid_until,
            metadata,
        } = input;

        if content.trim().is_empty() {
            return Err(Error::InvalidInput(
                "source fragment content must not be empty".to_owned(),
            ));
        }
        if origin.session_id.trim().is_empty() {
            return Err(Error::InvalidInput(
                "source fragment origin session_id must not be empty".to_owned(),
            ));
        }
        if origin.session_id.trim() != origin.session_id {
            return Err(Error::InvalidInput(
                "source fragment origin session_id must not have surrounding whitespace".to_owned(),
            ));
        }
        if !origin.confidence.is_finite() || !(0.0..=1.0).contains(&origin.confidence) {
            return Err(Error::InvalidInput(
                "source fragment origin confidence must be finite and within [0, 1]".to_owned(),
            ));
        }
        if let (Some(start), Some(end)) = (valid_from, valid_until)
            && end <= start
        {
            return Err(Error::InvalidInput(
                "source fragment valid_until must be greater than valid_from".to_owned(),
            ));
        }
        if let Some(values) = embedding.as_deref() {
            if values.is_empty() || !values.iter().all(|value| value.is_finite()) {
                return Err(Error::InvalidInput(
                    "source fragment embedding must be non-empty and finite".to_owned(),
                ));
            }
            let expected_dimensions = self.provider.dimensions();
            if values.len() != expected_dimensions {
                return Err(Error::InvalidInput(format!(
                    "source fragment embedding has {} dimensions, expected {expected_dimensions}",
                    values.len()
                )));
            }
        }

        let mut metadata_map = HashMap::with_capacity(metadata.len());
        for (key, value) in metadata {
            if key.trim().is_empty() || key.trim() != key {
                return Err(Error::InvalidInput(
                    "source fragment metadata keys must be non-empty without surrounding whitespace"
                        .to_owned(),
                ));
            }
            if key == crate::storage::NODE_INCARNATION_METADATA_KEY
                || crate::storage::is_atomic_source_incarnation_key(&key)
            {
                return Err(Error::InvalidInput(format!(
                    "source fragment metadata key {key:?} is engine-owned"
                )));
            }
            if metadata_map.insert(key.clone(), value).is_some() {
                return Err(Error::InvalidInput(format!(
                    "source fragment metadata key {key:?} is duplicated"
                )));
            }
        }

        let mut entity_tags: Vec<String> = entity_tags
            .into_iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect();
        entity_tags.sort();
        entity_tags.dedup();

        let observation = Observation {
            name: make_name(&content),
            summary: None,
            content,
            embedding,
            confidence: origin.confidence,
            node_type: KnowledgeType::Episodic,
            entity_tags,
            origin,
            timestamp: observed_at,
            valid_from,
            valid_until,
        };
        match self
            .engine
            .ingest_with_metadata(observation, metadata_map)?
        {
            IngestResult::Created(ids) => match ids.as_slice() {
                [id] => Ok(*id),
                _ => Err(Error::InvalidInput(
                    "source fragment admission must create exactly one node".to_owned(),
                )),
            },
            IngestResult::Reinforced { .. } => Err(Error::InvalidInput(
                "source fragment admission must allocate an immutable source node".to_owned(),
            )),
        }
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

    /// Add one source-grounded atomic extraction record to the routing sidecar.
    ///
    /// The fact is embedded once at ingest and remains outside the cognitive
    /// graph. Every source must be a live Episodic node from the same session
    /// and scope. Query-time routing may select the fact, but only its cited raw
    /// sources can enter the normal readout/reranker lane. This method records
    /// no engine-verified reviewer identity or decision; use
    /// [`add_reviewed_derivation`](Self::add_reviewed_derivation) when that
    /// provenance exists.
    pub fn add_atomic_fact(&mut self, input: AtomicFactInput) -> Result<AtomicFactId, Error> {
        self.add_atomic_fact_internal(input, Vec::new(), None, None)
    }

    /// Add one explicitly reviewed derivation to the isolated routing sidecar.
    ///
    /// The proposition and search projection are routing representations only.
    /// They never become graph nodes or independently renderable evidence;
    /// recall may return only the cited raw Episodic sources. Every source must
    /// exist in one session and scope, remain current and unretracted, and have
    /// been valid when the supplied review occurred.
    ///
    /// The idempotency key in [`ReviewProvenance`] binds the complete normalized
    /// input. Repeating an identical admission returns the existing id. Reusing
    /// the key for a different proposition, projection, source allocation,
    /// validity interval, review, tag, or metadata value is rejected.
    /// Idempotency is serialized by the exclusive `&mut Memory` receiver; it
    /// does not add coordination between independently opened storage instances.
    /// File-backed embedders must follow the multi-writer contract documented by
    /// [`SqliteStorage::open`].
    pub fn add_reviewed_derivation(
        &mut self,
        input: ReviewedDerivationInput,
    ) -> Result<AtomicFactId, Error> {
        let ReviewedDerivationInput {
            proposition,
            search_projection,
            mut source_node_ids,
            entity_tags,
            valid_from,
            valid_until,
            review,
            metadata,
        } = input;
        let search_projection = normalize_reviewed_derivation_field(
            "search projection",
            &search_projection,
            MAX_REVIEWED_DERIVATION_PROJECTION_CHARS,
            true,
        )?;
        let subject = normalize_reviewed_derivation_field(
            "subject",
            &proposition.subject,
            MAX_REVIEWED_DERIVATION_COMPONENT_CHARS,
            false,
        )?;
        let predicate = normalize_reviewed_derivation_field(
            "predicate",
            &proposition.predicate,
            MAX_REVIEWED_DERIVATION_COMPONENT_CHARS,
            false,
        )?;
        let object = normalize_reviewed_derivation_field(
            "object",
            &proposition.object,
            MAX_REVIEWED_DERIVATION_COMPONENT_CHARS,
            false,
        )?;
        let value_kind = proposition
            .value_kind
            .as_deref()
            .map(|value_kind| {
                normalize_reviewed_derivation_field(
                    "value kind",
                    value_kind,
                    MAX_REVIEWED_DERIVATION_VALUE_KIND_CHARS,
                    false,
                )
            })
            .transpose()?;
        let reviewed_by = normalize_reviewed_derivation_field(
            "reviewer",
            &review.reviewed_by,
            MAX_REVIEW_PROVENANCE_FIELD_CHARS,
            false,
        )?;
        let review_profile = normalize_reviewed_derivation_field(
            "review profile",
            &review.review_profile,
            MAX_REVIEW_PROVENANCE_FIELD_CHARS,
            false,
        )?;
        let idempotency_key = normalize_reviewed_derivation_field(
            "idempotency key",
            &review.idempotency_key,
            MAX_REVIEW_PROVENANCE_FIELD_CHARS,
            false,
        )?;
        let metadata = validate_reviewed_derivation_metadata(metadata)?;

        source_node_ids.sort_unstable();
        source_node_ids.dedup();
        let mut engine_metadata = vec![
            (
                REVIEWED_DERIVATION_SCHEMA_KEY.to_owned(),
                "reviewed-routing-v1".to_owned(),
            ),
            (REVIEWED_DERIVATION_SUBJECT_KEY.to_owned(), subject),
            (REVIEWED_DERIVATION_PREDICATE_KEY.to_owned(), predicate),
            (REVIEWED_DERIVATION_OBJECT_KEY.to_owned(), object),
            (
                REVIEWED_DERIVATION_POLARITY_KEY.to_owned(),
                proposition.polarity.as_str().to_owned(),
            ),
            (
                REVIEWED_DERIVATION_MODALITY_KEY.to_owned(),
                proposition.modality.as_str().to_owned(),
            ),
            (REVIEWED_DERIVATION_REVIEWED_BY_KEY.to_owned(), reviewed_by),
            (
                REVIEWED_DERIVATION_REVIEW_PROFILE_KEY.to_owned(),
                review_profile,
            ),
            (
                REVIEWED_DERIVATION_REVIEWED_AT_KEY.to_owned(),
                review.reviewed_at.0.to_string(),
            ),
            (
                REVIEWED_DERIVATION_IDEMPOTENCY_KEY.to_owned(),
                idempotency_key.clone(),
            ),
        ];
        if let Some(value_kind) = value_kind {
            engine_metadata.push((REVIEWED_DERIVATION_VALUE_KIND_KEY.to_owned(), value_kind));
        }
        let atomic_input = AtomicFactInput {
            content: search_projection,
            embedding_surface: None,
            source_node_ids,
            entity_tags,
            valid_from,
            valid_until,
            metadata,
        };
        self.add_atomic_fact_internal(
            atomic_input,
            engine_metadata,
            Some(idempotency_key),
            Some(review.reviewed_at),
        )
    }

    fn add_atomic_fact_internal(
        &mut self,
        input: AtomicFactInput,
        engine_metadata: Vec<(String, String)>,
        idempotency_key: Option<String>,
        reviewed_at: Option<Timestamp>,
    ) -> Result<AtomicFactId, Error> {
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
        let mut source_incarnations = Vec::with_capacity(input.source_node_ids.len());
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
            if reviewed_at.is_some_and(|reviewed_at| {
                source.created_at > reviewed_at
                    || !crate::graph::valid_at(source.valid_from, source.valid_until, reviewed_at)
                    || source
                        .metadata
                        .get("retracted")
                        .is_some_and(|value| value == "true")
            }) {
                return Err(Error::InvalidInput(format!(
                    "reviewed derivation source {} was not live at review time",
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
            source_incarnations.push((
                crate::storage::atomic_source_incarnation_key(source_node_id),
                self.engine
                    .graph()
                    .storage()
                    .atomic_source_incarnation(source)?,
            ));
            source_node_ids.push(source_node_id);
        }
        // A reviewed derivation cannot be available to a historical readout
        // before the decision that admitted it. Atomic facts have no separate
        // admission-time column, and every current read lane already treats
        // `observed_at` as the lower availability bound, so retain the later of
        // source observation and review time here.
        if let Some(reviewed_at) = reviewed_at {
            observed_at = observed_at.max(reviewed_at);
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
        if let Some((key, _)) = input
            .metadata
            .iter()
            .find(|(key, _)| is_reviewed_derivation_metadata_key(key))
        {
            return Err(Error::InvalidInput(format!(
                "atomic fact metadata key {key:?} is engine-owned"
            )));
        }
        let mut metadata: HashMap<_, _> = input.metadata.into_iter().collect();
        for (key, value) in engine_metadata {
            if !is_reviewed_derivation_metadata_key(&key) {
                return Err(Error::InvalidInput(format!(
                    "reviewed derivation engine metadata key {key:?} is outside its reserved namespace"
                )));
            }
            if metadata.insert(key.clone(), value).is_some() {
                return Err(Error::InvalidInput(format!(
                    "reviewed derivation engine metadata key {key:?} is duplicated"
                )));
            }
        }
        // Incarnation keys are engine-owned provenance. Stamp them after
        // consumer metadata so a caller cannot grant a replacement node the
        // authority of the source that was reviewed.
        metadata.extend(source_incarnations);
        if let Some(idempotency_key) = idempotency_key.as_deref() {
            let existing_ids = self.engine.graph().storage().atomic_fact_ids_by_metadata(
                REVIEWED_DERIVATION_IDEMPOTENCY_KEY,
                idempotency_key,
            )?;
            if existing_ids.len() > 1 {
                return Err(Error::StorageError(format!(
                    "reviewed derivation idempotency key {idempotency_key:?} matches multiple atomic facts"
                )));
            }
            if let Some(existing_id) = existing_ids.first().copied() {
                let existing = self.engine.graph().storage().get_atomic_fact(existing_id)?;
                if atomic_fact_matches_admission(
                    existing,
                    content,
                    &source_node_ids,
                    &entity_tags,
                    &source_session_id,
                    &scope,
                    observed_at,
                    input.valid_from,
                    input.valid_until,
                    &metadata,
                ) {
                    return Ok(existing.id);
                }
                return Err(Error::InvalidInput(format!(
                    "reviewed derivation idempotency key {idempotency_key:?} conflicts with an existing atomic fact"
                )));
            }
        }
        let embedding_surface = input
            .embedding_surface
            .as_deref()
            .map(str::trim)
            .filter(|surface| !surface.is_empty())
            .unwrap_or(content);
        let embedding = embed_one_passage(&*self.provider, embedding_surface)?;
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

    /// Add one reviewed typed relation to the isolated atomic-fact sidecar.
    ///
    /// Both endpoint facts must still resolve to live raw Episodic evidence.
    /// Two distinct concrete scopes cannot be joined; a universal endpoint
    /// adopts the other endpoint's concrete scope. The stored validity is the
    /// intersection of the supplied interval and both endpoint intervals.
    /// Repeating an identical idempotency key returns the existing relation;
    /// reusing it for different content is rejected.
    pub fn add_atomic_fact_relation(
        &mut self,
        input: AtomicFactRelationInput,
    ) -> Result<AtomicFactRelationId, Error> {
        if input.from_fact_id == input.to_fact_id {
            return Err(Error::InvalidInput(
                "atomic fact relation endpoints must be distinct".to_owned(),
            ));
        }
        let reviewed_by = input.reviewed_by.trim();
        if reviewed_by.is_empty() {
            return Err(Error::InvalidInput(
                "atomic fact relation reviewer must not be empty".to_owned(),
            ));
        }
        let review_profile = input.review_profile.trim();
        if review_profile.is_empty() {
            return Err(Error::InvalidInput(
                "atomic fact relation review profile must not be empty".to_owned(),
            ));
        }
        let idempotency_key = input.idempotency_key.trim();
        if idempotency_key.is_empty() {
            return Err(Error::InvalidInput(
                "atomic fact relation idempotency key must not be empty".to_owned(),
            ));
        }
        if let (Some(valid_from), Some(valid_until)) = (input.valid_from, input.valid_until)
            && valid_until <= valid_from
        {
            return Err(Error::InvalidInput(
                "atomic fact relation valid_until must be greater than valid_from".to_owned(),
            ));
        }

        let storage = self.engine.graph().storage();
        let from_fact = storage.get_atomic_fact(input.from_fact_id)?.clone();
        let to_fact = storage.get_atomic_fact(input.to_fact_id)?.clone();
        ensure_atomic_fact_has_live_sources(storage, &from_fact, input.reviewed_at)?;
        ensure_atomic_fact_has_live_sources(storage, &to_fact, input.reviewed_at)?;
        let scope = intersect_atomic_relation_scope(&from_fact.scope, &to_fact.scope)?;
        let valid_from = [input.valid_from, from_fact.valid_from, to_fact.valid_from]
            .into_iter()
            .flatten()
            .max();
        let valid_until = [
            input.valid_until,
            from_fact.valid_until,
            to_fact.valid_until,
        ]
        .into_iter()
        .flatten()
        .min();
        if let (Some(valid_from), Some(valid_until)) = (valid_from, valid_until)
            && valid_until <= valid_from
        {
            return Err(Error::InvalidInput(
                "atomic fact relation has no valid endpoint-time intersection".to_owned(),
            ));
        }
        let metadata: HashMap<_, _> = input.metadata.into_iter().collect();
        let candidate = AtomicFactRelation {
            id: AtomicFactRelationId(0),
            from_fact_id: input.from_fact_id,
            to_fact_id: input.to_fact_id,
            kind: input.kind,
            reviewed_by: reviewed_by.to_owned(),
            review_profile: review_profile.to_owned(),
            reviewed_at: input.reviewed_at,
            idempotency_key: idempotency_key.to_owned(),
            scope,
            valid_from,
            valid_until,
            metadata,
        };

        if let Some(existing) =
            storage.atomic_fact_relation_by_idempotency_key(&candidate.idempotency_key)?
        {
            let mut expected = candidate.clone();
            expected.id = existing.id;
            if existing == &expected {
                return Ok(existing.id);
            }
            return Err(Error::InvalidInput(format!(
                "atomic fact relation idempotency key {:?} conflicts with an existing relation",
                candidate.idempotency_key
            )));
        }

        let storage = self.engine.graph_mut().storage_mut();
        let id = storage.next_atomic_fact_relation_id()?;
        let mut relation = candidate;
        relation.id = id;
        storage.set_atomic_fact_relation(relation)?;
        Ok(id)
    }

    /// Delete one reviewed relation from the isolated atomic-fact sidecar.
    pub fn delete_atomic_fact_relation(&mut self, id: AtomicFactRelationId) -> Result<(), Error> {
        self.engine
            .graph_mut()
            .storage_mut()
            .delete_atomic_fact_relation(id)
    }

    /// Number of live reviewed atomic-fact relations.
    pub fn atomic_fact_relation_count(&self) -> usize {
        self.engine
            .graph()
            .storage()
            .all_atomic_fact_relation_ids()
            .len()
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
/// All fields default to the canonical framework policy. Override only when a
/// consumer needs an explicitly measured alternative.
#[derive(Debug, Clone, Default)]
pub struct SearchTuning {
    /// Override the number of seed nodes to expand with graph recall.
    ///
    /// `None` (default) uses the recipe default (`limit.max(1)`).
    pub seed_limit: Option<usize>,
    /// Entity tags to inject as retrieval seeds (e.g. speaker cues).
    ///
    /// Empty (default) keeps broad entity-tag seeding off.
    pub entity_tags: Vec<String>,
}

/// A single ranked memory hit from a [`Recall`].
///
/// Returned by [`Memory::search`] and [`Memory::search_at`] from the engine's
/// pre-packaging readout surface.
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

/// One exact raw source line selected for query-focused reading.
///
/// The line is drawn only from the validated evidence already delivered in a
/// [`Recall`]. A delivered `Semantic` line is exposed here only after it has
/// resolved unambiguously to a live `Episodic` source with matching
/// provenance. Selection never searches for additional evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FocusedEvidence {
    /// Canonical raw `Episodic` source for this line.
    pub source_node_id: NodeId,
    /// Immutable observation time of the raw source.
    pub observed_at: Timestamp,
    /// Source session retained for grouping and attribution.
    pub session_id: String,
    /// Exact, trimmed source line selected for reading.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecallReadoutValidationBinding {
    plan: RecallPlan,
    reader_contract: RecallReaderContract,
    reader_guidance: Option<String>,
    source_node_ids: Vec<NodeId>,
    source_attributions: Vec<RecallSourceAttribution>,
    focused_evidence: Vec<FocusedEvidence>,
}

impl RecallReadoutValidationBinding {
    fn capture(readout: &RecallReadout) -> Self {
        Self {
            plan: readout.plan.clone(),
            reader_contract: readout.reader_contract.clone(),
            reader_guidance: readout.reader_guidance.clone(),
            source_node_ids: readout.source_node_ids.clone(),
            source_attributions: readout.source_attributions.clone(),
            focused_evidence: readout.focused_evidence.clone(),
        }
    }

    fn matches(&self, readout: &RecallReadout) -> bool {
        self.plan == readout.plan
            && self.reader_contract == readout.reader_contract
            && self.reader_guidance == readout.reader_guidance
            && self.source_node_ids == readout.source_node_ids
            && self.source_attributions == readout.source_attributions
            && self.focused_evidence == readout.focused_evidence
    }
}

/// Model-free reader contract compiled from an existing [`Recall`].
///
/// This is the typed counterpart of the guidance and query-focused evidence
/// appended by [`Memory::render_context_for`]. It lets direct crate, protocol,
/// and UI consumers use the same source selection without parsing Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecallReadout {
    /// Deterministic intent used to compile this readout.
    ///
    /// Query-based entry points infer it, while plan-aware entry points retain
    /// the caller-supplied plan unchanged.
    pub plan: RecallPlan,
    /// Provider-neutral staged reading contract compiled from [`Self::plan`].
    pub reader_contract: RecallReaderContract,
    /// Reader guidance emitted when the recall contains evidence.
    pub reader_guidance: Option<String>,
    /// Canonical source nodes visibly exposed by the rendered recall.
    ///
    /// A source-bound line inside a `Semantic` window contributes its exact
    /// raw `Episodic` source instead of the enclosing window node. Consumers
    /// can use this set to validate citations in a structured reader draft.
    pub source_node_ids: Vec<NodeId>,
    /// Trusted ownership and ordering for every visibly source-bound line.
    ///
    /// Unlike [`Self::focused_evidence`], this list covers the full delivered
    /// context and preserves its original block and line order.
    pub source_attributions: Vec<RecallSourceAttribution>,
    /// Bounded exact source lines in reader order.
    pub focused_evidence: Vec<FocusedEvidence>,
    validation_context: GroundedDraftValidationContext,
    context_aware_validation: bool,
    validation_binding: Option<RecallReadoutValidationBinding>,
}

impl RecallReadout {
    fn validation_binding_is_current(&self) -> bool {
        self.validation_binding
            .as_ref()
            .is_some_and(|binding| binding.matches(self))
    }

    /// Return the event-boundary roles only while every public readout field
    /// still matches the exact surface bound at issuance.
    pub fn event_boundary_evidence(&self) -> Option<&EventBoundaryEvidence> {
        self.validation_binding_is_current()
            .then(|| self.validation_context.event_boundary_evidence())
            .flatten()
    }

    /// Return closed authority guidance suitable for composition with
    /// [`RecallReaderContract::system_context_guidance`].
    ///
    /// The result contains only receipt direction, canonical node ids, and a
    /// fixed citation-role contract. It never contains query or evidence text.
    pub fn system_authority_guidance(&self) -> Option<String> {
        self.event_boundary_evidence()
            .map(readout::event_boundary_authority_guidance)
    }

    /// Validate a newly adjudicated draft against this readout's exact visible
    /// source set.
    pub fn validate_adjudicated_draft(
        &self,
        draft: &GroundedAnswerDraft,
    ) -> GroundedDraftValidationResult {
        if self.context_aware_validation && self.validation_binding_is_current() {
            self.reader_contract
                .validate_adjudicated_draft_with_context(
                    draft,
                    &self.source_node_ids,
                    &self.validation_context,
                )
        } else if self.context_aware_validation {
            self.reader_contract
                .validate_adjudicated_draft_with_context(
                    draft,
                    &self.source_node_ids,
                    &GroundedDraftValidationContext::membership_only(),
                )
        } else {
            self.reader_contract
                .validate_adjudicated_draft(draft, &self.source_node_ids)
        }
    }

    /// Deterministically materialize a newly adjudicated draft through this
    /// readout's exact visible source set.
    pub fn materialize_adjudicated_draft(
        &self,
        draft: &GroundedAnswerDraft,
    ) -> Result<Option<String>, GroundedDraftValidationError> {
        if self.context_aware_validation && self.validation_binding_is_current() {
            self.reader_contract
                .materialize_adjudicated_draft_with_context(
                    draft,
                    &self.source_node_ids,
                    &self.validation_context,
                )
        } else if self.context_aware_validation {
            self.reader_contract
                .materialize_adjudicated_draft_with_context(
                    draft,
                    &self.source_node_ids,
                    &GroundedDraftValidationContext::membership_only(),
                )
        } else {
            self.reader_contract
                .materialize_adjudicated_draft(draft, &self.source_node_ids)
        }
    }

    /// Reconcile a typed draft through this readout's exact membership and
    /// ownership metadata.
    pub fn reconcile_grounded_draft(
        &self,
        draft: &GroundedAnswerDraft,
        final_answer: &str,
    ) -> Option<String> {
        self.reader_contract
            .reconcile_grounded_draft_with_attributions(
                draft,
                final_answer,
                &self.source_node_ids,
                &self.source_attributions,
            )
    }
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
/// This node-addressed form is retained for diagnostic, unbound repackaging
/// through [`Memory::repackage_reranked`]. The safe external production workflow
/// scores prepared document indices as [`RerankScore`] values and completes the
/// resulting [`PreparedRerank`] receipt. The cognitive readout remains available
/// in [`SearchResult::trace`] for provenance and observability.
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
        render_context_package(&self.package, None, None, None)
    }
}

#[derive(Debug, Clone, Copy)]
struct FragmentTime {
    observed_at: Timestamp,
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
}

type FragmentLineSources = HashMap<NodeId, HashMap<String, NodeId>>;

const CONTEXT_RENDER_CHARS_PER_TOKEN: usize = 4;

#[derive(Debug)]
struct QueryFocusedCandidate {
    evidence: FocusedEvidence,
    peer_id: PeerId,
    scope: ScopePath,
    lexical_overlap: usize,
    dialogue_reply_overlap: usize,
    link_terms: HashSet<String>,
    value_terms: HashSet<String>,
    information_terms: HashSet<String>,
    semantic_windows: HashSet<NodeId>,
    temporal_alignment: u8,
    temporal_distance_ms: u64,
    relevance: f64,
    fragment_order: usize,
    line_order: usize,
}

#[derive(Debug, Clone)]
struct QueryFocusedPreferenceSignal {
    strength: u8,
    relation_kinds: u8,
    candidate_identity: String,
}

#[derive(Debug, Clone, Copy)]
struct QueryFocusedFulfillmentSignal {
    target_overlap: usize,
    relation_kinds: u8,
}

#[derive(Debug, Clone, Copy)]
struct QueryFocusedModalBridge {
    fulfillment_index: usize,
    preference_index: usize,
    preference_strength: u8,
    target_overlap: usize,
    relation_overlap: u32,
    preference_observed_at: u64,
    fulfillment_observed_at: u64,
}

const QUERY_FOCUS_RELATION_AFFILIATION: u8 = 1 << 0;
const QUERY_FOCUS_RELATION_EMPLOYMENT: u8 = 1 << 1;
const QUERY_FOCUS_RELATION_VISIT: u8 = 1 << 2;
const QUERY_FOCUS_RELATION_PURCHASE: u8 = 1 << 3;
const QUERY_FOCUS_RELATION_CHOICE: u8 = 1 << 4;

fn query_focus_owner(value: &str) -> Option<String> {
    let (owner, body) = value.split_once(':')?;
    let owner = owner.trim();
    if owner.is_empty()
        || owner.len() > 48
        || body.trim().is_empty()
        || owner
            .chars()
            .any(|character| !(character.is_alphanumeric() || " '-_".contains(character)))
    {
        return None;
    }
    let normalized = owner
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    (1..=4)
        .contains(&normalized.len())
        .then(|| normalized.join(" "))
}

fn query_focus_normalized_text(value: &str) -> String {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn query_focus_named_token_is_common(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "after"
            | "always"
            | "and"
            | "as"
            | "at"
            | "because"
            | "before"
            | "but"
            | "currently"
            | "despite"
            | "during"
            | "earlier"
            | "eventually"
            | "finally"
            | "first"
            | "for"
            | "fortunately"
            | "from"
            | "favorite"
            | "favourite"
            | "great"
            | "however"
            | "i"
            | "id"
            | "ill"
            | "im"
            | "it"
            | "its"
            | "ive"
            | "last"
            | "later"
            | "luckily"
            | "my"
            | "new"
            | "next"
            | "now"
            | "of"
            | "on"
            | "or"
            | "our"
            | "really"
            | "recently"
            | "previously"
            | "since"
            | "so"
            | "thank"
            | "thanks"
            | "that"
            | "the"
            | "then"
            | "these"
            | "this"
            | "those"
            | "today"
            | "tomorrow"
            | "to"
            | "travel"
            | "trip"
            | "visit"
            | "we"
            | "when"
            | "while"
            | "with"
            | "without"
            | "work"
            | "working"
            | "yeah"
            | "yes"
            | "yesterday"
            | "ultimately"
            | "unfortunately"
            | "until"
            | "upon"
            | "monday"
            | "tuesday"
            | "wednesday"
            | "thursday"
            | "friday"
            | "saturday"
            | "sunday"
            | "january"
            | "february"
            | "march"
            | "april"
            | "may"
            | "june"
            | "july"
            | "august"
            | "september"
            | "october"
            | "november"
            | "december"
    )
}

fn query_focus_named_identities(value: &str) -> Vec<String> {
    let mut identities = Vec::new();
    let mut current = Vec::new();
    let flush = |current: &mut Vec<String>, identities: &mut Vec<String>| {
        if !current.is_empty() {
            identities.push(current.join(" "));
            current.clear();
        }
    };

    for raw_token in value.split_whitespace() {
        let token = raw_token.trim_matches(|character: char| {
            !character.is_alphanumeric() && character != '-' && character != '\''
        });
        let normalized = query_focus_normalized_text(token);
        let alphabetic_count = token
            .chars()
            .filter(|character| character.is_alphabetic())
            .count();
        let is_acronym = alphabetic_count > 1
            && token
                .chars()
                .filter(|character| character.is_alphabetic())
                .all(char::is_uppercase);
        let starts_uppercase = token.chars().next().is_some_and(char::is_uppercase);
        if !normalized.is_empty()
            && (starts_uppercase || is_acronym)
            && !query_focus_named_token_is_common(&normalized)
        {
            current.push(normalized);
            if raw_token
                .chars()
                .last()
                .is_some_and(|character| matches!(character, ',' | ';' | ':' | '/' | '|'))
            {
                flush(&mut current, &mut identities);
            }
        } else {
            flush(&mut current, &mut identities);
        }
    }
    flush(&mut current, &mut identities);
    identities.sort();
    identities.dedup();
    identities
}

fn query_focus_contains_normalized_phrase(normalized_value: &str, phrase: &str) -> bool {
    normalized_value == phrase
        || normalized_value
            .strip_prefix(phrase)
            .is_some_and(|suffix| suffix.starts_with(' '))
        || normalized_value
            .strip_suffix(phrase)
            .is_some_and(|prefix| prefix.ends_with(' '))
        || normalized_value.contains(&format!(" {phrase} "))
}

fn query_focus_preference_is_negated_or_corrected(value: &str) -> bool {
    let normalized = query_focus_normalized_text(value);
    let words: HashSet<_> = normalized.split_whitespace().collect();
    words.iter().any(|word| {
        matches!(
            *word,
            "against"
                | "avoid"
                | "avoided"
                | "avoiding"
                | "cancel"
                | "canceled"
                | "canceling"
                | "cancelled"
                | "cancelling"
                | "correction"
                | "instead"
                | "mistake"
                | "mistaken"
                | "never"
                | "not"
                | "reconsidered"
                | "retract"
                | "retracted"
        )
    }) || [
        "but not",
        "but now",
        "changed my mind",
        "don t",
        "instead of",
        "no longer",
        "rather than",
        "scratch that",
    ]
    .iter()
    .any(|phrase| query_focus_contains_normalized_phrase(&normalized, phrase))
}

fn query_focus_relation_kinds(value: &str) -> u8 {
    let normalized = query_focus_normalized_text(value);
    let words: HashSet<_> = normalized.split_whitespace().collect();
    let has_any = |needles: &[&str]| needles.iter().any(|needle| words.contains(needle));
    let mut kinds = 0;
    if has_any(&[
        "collaborate",
        "collaborated",
        "collaboration",
        "deal",
        "endorse",
        "endorsed",
        "endorsement",
        "partner",
        "partnered",
        "partnership",
        "sponsor",
        "sponsored",
        "sponsorship",
    ]) || normalized.contains("work with")
        || normalized.contains("working with")
        || normalized.contains("signed with")
    {
        kinds |= QUERY_FOCUS_RELATION_AFFILIATION;
    }
    if has_any(&[
        "employ", "employed", "hire", "hired", "job", "joined", "position", "role",
    ]) || normalized.contains("work for")
        || normalized.contains("working for")
    {
        kinds |= QUERY_FOCUS_RELATION_EMPLOYMENT;
    }
    if has_any(&[
        "trip",
        "travel",
        "traveled",
        "travelled",
        "visit",
        "visited",
    ]) {
        kinds |= QUERY_FOCUS_RELATION_VISIT;
    }
    if has_any(&["bought", "buy", "order", "ordered", "purchase", "purchased"]) {
        kinds |= QUERY_FOCUS_RELATION_PURCHASE;
    }
    if has_any(&[
        "choose",
        "chose",
        "choice",
        "pick",
        "picked",
        "prefer",
        "preferred",
        "select",
        "selected",
    ]) {
        kinds |= QUERY_FOCUS_RELATION_CHOICE;
    }
    kinds
}

fn query_focus_preference_signal(value: &str) -> Option<QueryFocusedPreferenceSignal> {
    let (_, body) = value.split_once(':')?;
    // ASCII folding preserves byte offsets into `body`; the fixed markers are
    // ASCII while candidate identity retains the source's original casing.
    let lowercase = body.to_ascii_lowercase();
    let markers = [
        ("i've always liked ", 3),
        ("i have always liked ", 3),
        ("i've always loved ", 3),
        ("i have always loved ", 3),
        ("my first choice is ", 3),
        ("my favorite is ", 3),
        ("my favourite is ", 3),
        ("i really like ", 3),
        ("i really love ", 3),
        ("i prefer ", 3),
        ("i'd love to ", 2),
        ("i would love to ", 2),
        ("i'd like to ", 2),
        ("i would like to ", 2),
        ("i want to ", 2),
        ("i hope to ", 2),
        ("i plan to ", 2),
        ("i intend to ", 2),
    ];
    let mut best = None;
    for (marker, strength) in markers {
        let Some(marker_start) = lowercase.find(marker) else {
            continue;
        };
        let sentence_start = lowercase[..marker_start]
            .rfind(['.', '!', '?'])
            .map_or(0, |offset| offset.saturating_add(1));
        let marker_end = marker_start.saturating_add(marker.len());
        let sentence_end = lowercase[marker_end..]
            .find(['.', '!', '?'])
            .map_or(body.len(), |offset| marker_end.saturating_add(offset));
        let sentence = body.get(sentence_start..sentence_end)?.trim();
        if query_focus_preference_is_negated_or_corrected(sentence) {
            continue;
        }
        let suffix = body
            .get(marker_end..sentence_end)?
            .chars()
            .take(240)
            .collect::<String>();
        let candidate_identities = query_focus_named_identities(&suffix);
        let [candidate_identity] = candidate_identities.as_slice() else {
            continue;
        };
        let relation_kinds = query_focus_relation_kinds(sentence);
        if relation_kinds == 0 {
            continue;
        }
        let signal = QueryFocusedPreferenceSignal {
            strength,
            relation_kinds,
            candidate_identity: candidate_identity.clone(),
        };
        if best
            .as_ref()
            .is_none_or(|current: &QueryFocusedPreferenceSignal| signal.strength > current.strength)
        {
            best = Some(signal);
        }
    }
    best
}

fn query_focus_fulfillment_signal(
    value: &str,
    target_terms: &HashSet<String>,
) -> Option<QueryFocusedFulfillmentSignal> {
    let (_, body) = value.split_once(':')?;
    let required_overlap = target_terms.len().clamp(1, 2);
    body.split(['.', '!', '?', ';'])
        .filter_map(|clause| {
            let normalized = query_focus_normalized_text(clause);
            let words: HashSet<_> = normalized.split_whitespace().collect();
            let clause_facets = readout::facet_terms(clause);
            let target_overlap = target_terms.intersection(&clause_facets).count();
            if target_overlap < required_overlap
                || !query_focus_named_identities(clause).is_empty()
                || [
                    "could",
                    "hope",
                    "hoping",
                    "may",
                    "might",
                    "plan",
                    "planned",
                    "planning",
                    "possible",
                    "possibly",
                    "potential",
                    "would",
                ]
                .iter()
                .any(|word| words.contains(word))
                || normalized.contains("in talks")
                || normalized.contains("did not")
                || normalized.contains("didn't")
                || normalized.contains("never ")
            {
                return None;
            }
            let completed = [
                "accepted",
                "began",
                "booked",
                "bought",
                "chose",
                "completed",
                "finished",
                "got",
                "hired",
                "joined",
                "landed",
                "partnered",
                "purchased",
                "secured",
                "selected",
                "signed",
                "started",
            ]
            .iter()
            .any(|word| words.contains(word));
            let relation_kinds = query_focus_relation_kinds(clause);
            (completed && relation_kinds != 0).then_some(QueryFocusedFulfillmentSignal {
                target_overlap,
                relation_kinds,
            })
        })
        .max_by_key(|signal| (signal.target_overlap, signal.relation_kinds.count_ones()))
}

fn query_focus_modal_bridge(
    candidates: &[QueryFocusedCandidate],
    plan: &RecallPlan,
) -> Option<QueryFocusedModalBridge> {
    if plan.answer_shape != AnswerShape::Inference
        || plan.derivation != RecallDerivation::GroundedInference
    {
        return None;
    }
    let contract = plan.reader_contract();
    let requested = contract.requested_answer_spec();
    if requested.role() != RequestedAnswerRole::Value
        || requested.cardinality() != RequestedAnswerCardinality::One
        || !matches!(
            requested.modality(),
            RequestedAnswerModality::Likely | RequestedAnswerModality::Possible
        )
    {
        return None;
    }
    let target_terms = readout::facet_terms(requested.value_target()?);
    let query_relation_kinds = query_focus_relation_kinds(&plan.query);
    let normalized_query = query_focus_normalized_text(&plan.query);
    if target_terms.is_empty() || query_relation_kinds == 0 {
        return None;
    }

    const MAX_MODAL_SIGNALS: usize = 32;
    let preferences = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let owner = query_focus_owner(&candidate.evidence.text)?;
            if !query_focus_contains_normalized_phrase(&normalized_query, &owner) {
                return None;
            }
            let signal = query_focus_preference_signal(&candidate.evidence.text)?;
            ((signal.relation_kinds & query_relation_kinds) != 0).then_some((index, owner, signal))
        })
        .take(MAX_MODAL_SIGNALS.saturating_add(1))
        .collect::<Vec<_>>();
    let fulfillments = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let owner = query_focus_owner(&candidate.evidence.text)?;
            if !query_focus_contains_normalized_phrase(&normalized_query, &owner) {
                return None;
            }
            let signal = query_focus_fulfillment_signal(&candidate.evidence.text, &target_terms)?;
            ((signal.relation_kinds & query_relation_kinds) != 0).then_some((index, owner, signal))
        })
        .take(MAX_MODAL_SIGNALS.saturating_add(1))
        .collect::<Vec<_>>();
    if preferences.len() > MAX_MODAL_SIGNALS || fulfillments.len() > MAX_MODAL_SIGNALS {
        return None;
    }

    let mut best = None;
    for (fulfillment_index, fulfillment_owner, fulfillment) in fulfillments {
        let fulfillment_candidate = &candidates[fulfillment_index];
        let qualifying = preferences
            .iter()
            .filter(|(preference_index, preference_owner, preference)| {
                let preference_candidate = &candidates[*preference_index];
                preference_owner == &fulfillment_owner
                    && preference_candidate.peer_id == fulfillment_candidate.peer_id
                    && preference_candidate.scope == fulfillment_candidate.scope
                    && preference_candidate.evidence.source_node_id
                        != fulfillment_candidate.evidence.source_node_id
                    && preference_candidate.evidence.observed_at
                        < fulfillment_candidate.evidence.observed_at
                    && (preference.relation_kinds
                        & fulfillment.relation_kinds
                        & query_relation_kinds)
                        != 0
            })
            .collect::<Vec<_>>();
        let candidate_identities: HashSet<_> = qualifying
            .iter()
            .map(|(_, _, preference)| preference.candidate_identity.as_str())
            .collect();
        if candidate_identities.len() != 1 {
            continue;
        }
        let Some((preference_index, _, preference)) =
            qualifying
                .into_iter()
                .max_by(|(left_index, _, left), (right_index, _, right)| {
                    candidates[*left_index]
                        .evidence
                        .observed_at
                        .cmp(&candidates[*right_index].evidence.observed_at)
                        .then_with(|| left.strength.cmp(&right.strength))
                        .then_with(|| right_index.cmp(left_index))
                })
        else {
            continue;
        };
        let preference_candidate = &candidates[*preference_index];
        let shared_relations =
            preference.relation_kinds & fulfillment.relation_kinds & query_relation_kinds;
        let bridge = QueryFocusedModalBridge {
            fulfillment_index,
            preference_index: *preference_index,
            preference_strength: preference.strength,
            target_overlap: fulfillment.target_overlap,
            relation_overlap: shared_relations.count_ones(),
            preference_observed_at: preference_candidate.evidence.observed_at.0,
            fulfillment_observed_at: fulfillment_candidate.evidence.observed_at.0,
        };
        let rank_key = |candidate: QueryFocusedModalBridge| {
            (
                candidate.target_overlap,
                candidate.relation_overlap,
                candidate.preference_observed_at,
                candidate.fulfillment_observed_at,
                candidate.preference_strength,
                std::cmp::Reverse((candidate.fulfillment_index, candidate.preference_index)),
            )
        };
        let better = best
            .is_none_or(|current: QueryFocusedModalBridge| rank_key(bridge) > rank_key(current));
        if better {
            best = Some(bridge);
        }
    }
    best
}

fn query_focus_surface_terms(value: &str) -> HashSet<String> {
    let mut terms = readout::facet_terms(value);
    if let Some((speaker, _)) = value.split_once(':')
        && speaker.len() <= 48
    {
        terms.extend(
            speaker
                .split(|character: char| !character.is_alphanumeric())
                .filter(|term| term.len() > 1)
                .map(str::to_lowercase),
        );
    }
    terms.extend(
        value
            .split(|character: char| !character.is_alphanumeric() && character != '\'')
            .filter(|term| term.len() > 1 && term.chars().next().is_some_and(char::is_uppercase))
            .map(str::to_lowercase),
    );
    terms
}

fn query_focus_value_terms(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .filter(|term| {
            term.chars().any(|character| character.is_ascii_digit())
                || matches!(
                    term.as_str(),
                    "january"
                        | "february"
                        | "march"
                        | "april"
                        | "may"
                        | "june"
                        | "july"
                        | "august"
                        | "september"
                        | "october"
                        | "november"
                        | "december"
                        | "monday"
                        | "tuesday"
                        | "wednesday"
                        | "thursday"
                        | "friday"
                        | "saturday"
                        | "sunday"
                        | "today"
                        | "yesterday"
                        | "tomorrow"
                        | "week"
                        | "weekend"
                        | "month"
                        | "year"
                )
        })
        .collect()
}

fn query_focus_connection(left: &QueryFocusedCandidate, right: &QueryFocusedCandidate) -> usize {
    let shared_terms = left.link_terms.intersection(&right.link_terms).count();
    let shared_values = left.value_terms.intersection(&right.value_terms).count();
    let same_window = !left.semantic_windows.is_disjoint(&right.semantic_windows);
    let same_session = left.evidence.session_id == right.evidence.session_id;
    if !same_window && shared_terms == 0 && !(same_session && shared_values > 0) {
        return 0;
    }
    usize::from(same_window) * 16
        + shared_terms.saturating_mul(3)
        + shared_values.saturating_mul(3)
        + usize::from(same_session) * 10
}

fn query_focus_complement_limit(plan: &RecallPlan, line_limit: usize) -> usize {
    match plan.answer_shape {
        AnswerShape::Relationship
        | AnswerShape::Inference
        | AnswerShape::Temporal
        | AnswerShape::Collection
        | AnswerShape::Frequency
        | AnswerShape::Count => line_limit.saturating_sub(1),
        AnswerShape::Fact if readout::plan_uses_temporal_readout(plan) => {
            line_limit.saturating_sub(1)
        }
        AnswerShape::Fact => line_limit.saturating_sub(1),
    }
}

fn query_focus_session_limit(plan: &RecallPlan, line_limit: usize) -> usize {
    match plan.answer_shape {
        AnswerShape::Relationship
        | AnswerShape::Inference
        | AnswerShape::Temporal
        | AnswerShape::Collection
        | AnswerShape::Frequency
        | AnswerShape::Count => line_limit.div_ceil(2).max(2),
        AnswerShape::Fact if readout::plan_uses_temporal_readout(plan) => {
            line_limit.div_ceil(2).max(2)
        }
        AnswerShape::Fact => line_limit,
    }
}

fn query_focus_line_limit(plan: &RecallPlan) -> usize {
    match plan.answer_shape {
        AnswerShape::Frequency | AnswerShape::Count | AnswerShape::Collection => 8,
        AnswerShape::Relationship | AnswerShape::Inference => 8,
        AnswerShape::Temporal => 5,
        AnswerShape::Fact if readout::plan_uses_temporal_readout(plan) => 5,
        AnswerShape::Fact => 4,
    }
}

fn select_query_focused_evidence(
    package: &ContextPackage,
    times: &HashMap<NodeId, FragmentTime>,
    line_sources: &FragmentLineSources,
    plan: &RecallPlan,
    token_allowance: usize,
) -> Vec<FocusedEvidence> {
    const HEADER: &str = "## QUERY-FOCUSED RAW EVIDENCE\n";
    const FOOTER: &str = "\n";

    let framing_tokens = estimate_tokens(HEADER, CONTEXT_RENDER_CHARS_PER_TOKEN)
        .saturating_add(estimate_tokens(FOOTER, CONTEXT_RENDER_CHARS_PER_TOKEN));
    if token_allowance <= framing_tokens {
        return Vec::new();
    }

    // Stay strictly inside the already validated delivery surface. In
    // particular, do not search the graph for a more convenient excerpt. A raw
    // Episodic fragment is directly delivered, while a Semantic line is
    // eligible only when the normal context renderer already bound that exact,
    // unambiguous line to an authoritative raw source.
    let fragments: Vec<_> = package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .collect();
    let delivered_raw: HashMap<_, _> = fragments
        .iter()
        .enumerate()
        .filter_map(|(order, fragment)| {
            (fragment.node_type == KnowledgeType::Episodic)
                .then_some((fragment.node_id, (*fragment, order)))
        })
        .collect();
    let query_facets = readout::facet_terms(&plan.query);
    let query_time_ranges = if readout::plan_uses_temporal_readout(plan) {
        crate::query::temporal::parse_time_cues(&plan.query, 0)
    } else {
        Vec::new()
    };
    let range_distance = |left_start: u64, left_end: u64, right_start: u64, right_end: u64| {
        if left_end < right_start {
            right_start.saturating_sub(left_end)
        } else if right_end < left_start {
            left_start.saturating_sub(right_end)
        } else {
            0
        }
    };
    let temporal_fit = |text: &str, observed_at: Timestamp| {
        if query_time_ranges.is_empty() {
            return (0, u64::MAX);
        }
        let mut alignment = 0u8;
        let mut distance = u64::MAX;
        for query_range in &query_time_ranges {
            let observed_distance = range_distance(
                observed_at.0,
                observed_at.0,
                query_range.start,
                query_range.end,
            );
            if observed_distance == 0 {
                alignment = alignment.max(1);
            }
            distance = distance.min(observed_distance);
        }
        for resolution in crate::query::temporal::resolve_relative_time_cues(text, observed_at.0) {
            for query_range in &query_time_ranges {
                let resolved_distance = range_distance(
                    resolution.range.start,
                    resolution.range.end,
                    query_range.start,
                    query_range.end,
                );
                if resolved_distance == 0 {
                    alignment = alignment.max(2);
                }
                distance = distance.min(resolved_distance);
            }
        }
        (alignment, distance)
    };
    let mut candidates: HashMap<(NodeId, String), QueryFocusedCandidate> = HashMap::new();
    let mut admit = |source_id: NodeId,
                     text: &str,
                     session_id: &str,
                     origin: &Origin,
                     semantic_window: Option<NodeId>,
                     relevance: f64,
                     fragment_order: usize,
                     line_order: usize,
                     dialogue_reply_overlap: usize| {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Some(observed_at) = times.get(&source_id).map(|time| time.observed_at) else {
            return;
        };
        let lexical_overlap = query_facets
            .intersection(&readout::facet_terms(text))
            .count();
        let link_terms = query_focus_surface_terms(text);
        let value_terms = query_focus_value_terms(text);
        let mut information_terms = link_terms.clone();
        information_terms.extend(value_terms.iter().cloned());
        information_terms.retain(|term| !query_facets.contains(term));
        let semantic_windows = semantic_window.into_iter().collect();
        let (temporal_alignment, temporal_distance_ms) = temporal_fit(text, observed_at);
        let key = (source_id, text.to_owned());
        let candidate = QueryFocusedCandidate {
            evidence: FocusedEvidence {
                source_node_id: source_id,
                observed_at,
                session_id: session_id.to_owned(),
                text: text.to_owned(),
            },
            peer_id: origin.peer_id,
            scope: origin.scope.clone(),
            lexical_overlap,
            dialogue_reply_overlap,
            link_terms,
            value_terms,
            information_terms,
            semantic_windows,
            temporal_alignment,
            temporal_distance_ms,
            relevance,
            fragment_order,
            line_order,
        };
        match candidates.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry
                    .get_mut()
                    .semantic_windows
                    .extend(candidate.semantic_windows.iter().copied());
                let existing = entry.get();
                if candidate.dialogue_reply_overlap > existing.dialogue_reply_overlap
                    || (candidate.dialogue_reply_overlap == existing.dialogue_reply_overlap
                        && candidate.relevance.total_cmp(&existing.relevance).is_gt())
                    || (candidate.dialogue_reply_overlap == existing.dialogue_reply_overlap
                        && candidate.relevance.total_cmp(&existing.relevance).is_eq()
                        && (candidate.fragment_order, candidate.line_order)
                            < (existing.fragment_order, existing.line_order))
                {
                    let semantic_windows = entry.get().semantic_windows.clone();
                    let mut candidate = candidate;
                    candidate.semantic_windows = semantic_windows;
                    entry.insert(candidate);
                }
            }
        }
    };

    for (fragment_order, fragment) in fragments.iter().enumerate() {
        let Some(content) = fragment.content.as_deref() else {
            continue;
        };
        if fragment.node_type == KnowledgeType::Episodic {
            for (line_order, line) in content.lines().enumerate() {
                admit(
                    fragment.node_id,
                    line,
                    &fragment.origin.session_id,
                    &fragment.origin,
                    None,
                    fragment.relevance,
                    fragment_order,
                    line_order,
                    0,
                );
            }
            continue;
        }
        if fragment.node_type != KnowledgeType::Semantic {
            continue;
        }
        let Some(bound_lines) = line_sources.get(&fragment.node_id) else {
            continue;
        };
        let lines: Vec<_> = content.lines().collect();
        for (line_order, line) in lines.iter().copied().enumerate() {
            let Some(source_id) = bound_lines.get(line.trim()).copied() else {
                continue;
            };
            let dialogue_reply_overlap = line_order
                .checked_sub(1)
                .and_then(|previous| lines.get(previous).copied())
                .filter(|previous| previous.trim_end().ends_with('?'))
                .map(|previous| {
                    let mut pair_terms = readout::facet_terms(previous);
                    pair_terms.extend(readout::facet_terms(line));
                    query_facets.intersection(&pair_terms).count()
                })
                .filter(|overlap| *overlap >= query_facets.len().min(2))
                .unwrap_or_default();
            let (relevance, source_order, source_origin) =
                if let Some((source, source_order)) = delivered_raw.get(&source_id) {
                    if source.origin.peer_id != fragment.origin.peer_id
                        || source.origin.session_id != fragment.origin.session_id
                        || source.origin.scope != fragment.origin.scope
                    {
                        continue;
                    }
                    (
                        fragment.relevance.max(source.relevance),
                        (*source_order).min(fragment_order),
                        &source.origin,
                    )
                } else {
                    (fragment.relevance, fragment_order, &fragment.origin)
                };
            admit(
                source_id,
                line,
                &source_origin.session_id,
                source_origin,
                Some(fragment.node_id),
                relevance,
                source_order,
                line_order,
                dialogue_reply_overlap,
            );
        }
    }

    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(|left, right| {
        right
            .temporal_alignment
            .cmp(&left.temporal_alignment)
            .then_with(|| left.temporal_distance_ms.cmp(&right.temporal_distance_ms))
            .then_with(|| {
                right
                    .dialogue_reply_overlap
                    .cmp(&left.dialogue_reply_overlap)
            })
            .then_with(|| right.lexical_overlap.cmp(&left.lexical_overlap))
            .then_with(|| right.relevance.total_cmp(&left.relevance))
            .then_with(|| left.fragment_order.cmp(&right.fragment_order))
            .then_with(|| left.line_order.cmp(&right.line_order))
            .then_with(|| {
                left.evidence
                    .source_node_id
                    .cmp(&right.evidence.source_node_id)
            })
            .then_with(|| left.evidence.text.cmp(&right.evidence.text))
    });

    if candidates.is_empty() {
        return Vec::new();
    }
    let line_limit = query_focus_line_limit(plan);
    let mut ordered_indices = Vec::new();
    let mut selected_indices = HashSet::new();
    if let Some(bridge) = query_focus_modal_bridge(&candidates, plan) {
        for index in [bridge.fulfillment_index, bridge.preference_index] {
            if selected_indices.insert(index) {
                ordered_indices.push(index);
            }
        }
    }
    if selected_indices.insert(0) {
        ordered_indices.push(0);
    }
    let query_anchor_index = ordered_indices[0];
    let mut covered_information = HashSet::new();
    for index in &ordered_indices {
        covered_information.extend(candidates[*index].information_terms.iter().cloned());
    }
    let complement_limit = query_focus_complement_limit(plan, line_limit);
    let session_limit = query_focus_session_limit(plan, line_limit);
    let prioritizes_answer_values = matches!(
        plan.answer_shape,
        AnswerShape::Temporal
            | AnswerShape::Frequency
            | AnswerShape::Count
            | AnswerShape::Collection
    );
    while ordered_indices.len() < line_limit
        && ordered_indices.len().saturating_sub(1) < complement_limit
    {
        let best = candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected_indices.contains(index))
            .filter_map(|(index, candidate)| {
                let connection = ordered_indices
                    .iter()
                    .map(|selected| query_focus_connection(&candidates[*selected], candidate))
                    .max()
                    .unwrap_or_default();
                if connection == 0 {
                    return None;
                }
                let novel_information = candidate
                    .information_terms
                    .difference(&covered_information)
                    .count();
                if novel_information == 0 {
                    return None;
                }
                let novel_values = candidate
                    .value_terms
                    .difference(&covered_information)
                    .count();
                let anchor_connection =
                    query_focus_connection(&candidates[query_anchor_index], candidate);
                let session_count = ordered_indices
                    .iter()
                    .filter(|selected| {
                        candidates[**selected].evidence.session_id == candidate.evidence.session_id
                    })
                    .count();
                let window_count = candidate
                    .semantic_windows
                    .iter()
                    .map(|window| {
                        ordered_indices
                            .iter()
                            .filter(|selected| {
                                candidates[**selected].semantic_windows.contains(window)
                            })
                            .count()
                    })
                    .max()
                    .unwrap_or_default();
                Some((
                    index,
                    window_count.saturating_sub(1),
                    session_count.saturating_sub(session_limit.saturating_sub(1)),
                    anchor_connection,
                    connection,
                    novel_values,
                    novel_information,
                ))
            })
            .max_by(|left, right| {
                let answer_value_order = if prioritizes_answer_values {
                    left.5.cmp(&right.5)
                } else {
                    std::cmp::Ordering::Equal
                };
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| right.2.cmp(&left.2))
                    .then(answer_value_order)
                    .then_with(|| left.6.min(4).cmp(&right.6.min(4)))
                    .then_with(|| left.3.cmp(&right.3))
                    .then_with(|| left.4.cmp(&right.4))
                    .then_with(|| left.5.cmp(&right.5))
                    .then_with(|| left.6.cmp(&right.6))
                    .then_with(|| {
                        candidates[left.0]
                            .relevance
                            .total_cmp(&candidates[right.0].relevance)
                    })
                    .then_with(|| {
                        candidates[left.0]
                            .lexical_overlap
                            .cmp(&candidates[right.0].lexical_overlap)
                    })
                    .then_with(|| right.0.cmp(&left.0))
            });
        let Some((index, _, _, _, _, _, _)) = best else {
            break;
        };
        selected_indices.insert(index);
        covered_information.extend(candidates[index].information_terms.iter().cloned());
        ordered_indices.push(index);
    }
    ordered_indices.extend((0..candidates.len()).filter(|index| selected_indices.insert(*index)));

    let mut used_tokens = framing_tokens;
    let mut selected = Vec::new();
    for index in ordered_indices {
        if selected.len() >= line_limit {
            break;
        }
        let candidate = &candidates[index];
        let line = render_focused_evidence_line(&candidate.evidence);
        let line_tokens = estimate_tokens(&line, CONTEXT_RENDER_CHARS_PER_TOKEN);
        if used_tokens.saturating_add(line_tokens) > token_allowance {
            continue;
        }
        used_tokens = used_tokens.saturating_add(line_tokens);
        selected.push(candidate.evidence.clone());
    }
    selected
}

fn render_focused_evidence_line(evidence: &FocusedEvidence) -> String {
    format!(
        "- [source=node:{} observed {}] {}\n",
        evidence.source_node_id.0,
        format_timestamp_utc(evidence.observed_at),
        evidence.text
    )
}

fn render_query_focused_evidence(evidence: &[FocusedEvidence]) -> Option<String> {
    const HEADER: &str = "## QUERY-FOCUSED RAW EVIDENCE\n";
    const FOOTER: &str = "\n";

    if evidence.is_empty() {
        return None;
    }

    let mut out = String::from(HEADER);
    for item in evidence {
        out.push_str(&render_focused_evidence_line(item));
    }
    out.push_str(FOOTER);
    Some(out)
}

#[cfg(test)]
fn query_focused_raw_evidence(
    package: &ContextPackage,
    times: &HashMap<NodeId, FragmentTime>,
    line_sources: &FragmentLineSources,
    plan: &RecallPlan,
    token_allowance: usize,
) -> Option<String> {
    let evidence =
        select_query_focused_evidence(package, times, line_sources, plan, token_allowance);
    render_query_focused_evidence(&evidence)
}

fn render_context_package(
    pkg: &ContextPackage,
    times: Option<&HashMap<NodeId, FragmentTime>>,
    relative_times: Option<&HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>>>,
    line_sources: Option<&FragmentLineSources>,
) -> String {
    let mut out = String::new();

    render_section(
        &mut out,
        "IDENTITY",
        &pkg.identity,
        times,
        relative_times,
        line_sources,
    );
    render_section(
        &mut out,
        "KNOWLEDGE",
        &pkg.knowledge,
        times,
        relative_times,
        line_sources,
    );
    render_section(
        &mut out,
        "MEMORIES",
        &pkg.memories,
        times,
        relative_times,
        line_sources,
    );

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
    line_sources: Option<&FragmentLineSources>,
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
                "- [E{evidence_index}] [{} source=node:{}]",
                node_type_label(&fragment.node_type),
                fragment.node_id.0
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
                render_source_bound_line(&mut out, fragment.node_id, line, line_sources, "    ");
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
    line_sources: Option<&FragmentLineSources>,
) {
    if frags.is_empty() {
        return;
    }
    let _ = writeln!(out, "## {title}");
    for f in frags {
        // Header: type label (KnowledgeType has no Display), name, relevance.
        if times.is_some() {
            let _ = writeln!(
                out,
                "- [{} source=node:{}] {} (relevance {:.2})",
                node_type_label(&f.node_type),
                f.node_id.0,
                f.name,
                f.relevance
            );
        } else {
            let _ = writeln!(
                out,
                "- [{}] {} (relevance {:.2})",
                node_type_label(&f.node_type),
                f.name,
                f.relevance
            );
        }
        // Body: prefer full content (L2), fall back to summary (L1); name is
        // already shown in the header.
        if let Some(content) = &f.content {
            if line_sources.is_some_and(|sources| sources.contains_key(&f.node_id)) {
                for line in content.lines() {
                    render_source_bound_line(out, f.node_id, line, line_sources, "    ");
                }
            } else {
                let _ = writeln!(out, "    {content}");
            }
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

fn render_source_bound_line(
    out: &mut String,
    fragment_id: NodeId,
    line: &str,
    line_sources: Option<&FragmentLineSources>,
    indent: &str,
) {
    let source = line_sources
        .and_then(|sources| sources.get(&fragment_id))
        .and_then(|sources| sources.get(line.trim()));
    if let Some(source) = source {
        let _ = writeln!(
            out,
            "{indent}[turn-source=node:{}] {}",
            source.0,
            line.trim()
        );
    } else {
        let _ = writeln!(out, "{indent}{line}");
    }
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

    fn recall_fragment_line_sources(&self, recall: &Recall) -> Result<FragmentLineSources, Error> {
        let storage = self.engine.graph().storage();
        let mut fragment_sources = HashMap::new();
        let mut episodic_ids_by_session: HashMap<(PeerId, String, ScopePath), Vec<NodeId>> =
            HashMap::new();
        for fragment in recall
            .package
            .identity
            .iter()
            .chain(recall.package.knowledge.iter())
            .chain(recall.package.memories.iter())
        {
            if fragment.node_type != KnowledgeType::Semantic {
                continue;
            }
            let Some(content) = fragment.content.as_deref() else {
                continue;
            };
            let content_lines: HashSet<_> = content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            let source_ids = readout::canonical_sources(storage, fragment.node_id)?;
            let mut candidates: HashMap<String, Option<NodeId>> = HashMap::new();
            let mut source_ids = source_ids;
            let session_tag = format!(
                "session-{}",
                normalize_tag(fragment.origin.session_id.as_str())
            );
            let session_key = (
                fragment.origin.peer_id,
                fragment.origin.session_id.clone(),
                fragment.origin.scope.clone(),
            );
            let session_source_ids =
                if let Some(source_ids) = episodic_ids_by_session.get(&session_key) {
                    source_ids.clone()
                } else {
                    let mut eligible_ids = Vec::new();
                    for source_id in storage.nodes_by_entity_tag(&session_tag) {
                        let source = storage.get_node(source_id)?;
                        if source.node_type == KnowledgeType::Episodic
                            && source.origin.peer_id == fragment.origin.peer_id
                            && source.origin.session_id == fragment.origin.session_id
                            && source.origin.scope == fragment.origin.scope
                        {
                            eligible_ids.push(source_id);
                        }
                    }
                    episodic_ids_by_session.insert(session_key, eligible_ids.clone());
                    eligible_ids
                };
            source_ids.extend(session_source_ids);
            source_ids.sort_unstable();
            source_ids.dedup();
            for source_id in source_ids {
                let source = storage.get_node(source_id)?;
                if source.node_type != KnowledgeType::Episodic
                    || source.origin.peer_id != fragment.origin.peer_id
                    || source.origin.session_id != fragment.origin.session_id
                    || source.origin.scope != fragment.origin.scope
                    || source
                        .metadata
                        .get("retracted")
                        .is_some_and(|value| value == "true")
                {
                    continue;
                }
                for line in source
                    .content
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && content_lines.contains(line))
                {
                    match candidates.entry(line.to_owned()) {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(Some(source_id));
                        }
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            if entry.get().is_some_and(|existing| existing != source_id) {
                                entry.insert(None);
                            }
                        }
                    }
                }
            }
            let unique_sources: HashMap<_, _> = candidates
                .into_iter()
                .filter_map(|(line, source)| source.map(|source| (line, source)))
                .collect();
            if !unique_sources.is_empty() {
                fragment_sources.insert(fragment.node_id, unique_sources);
            }
        }
        Ok(fragment_sources)
    }

    fn extend_line_source_times(
        &self,
        line_sources: &FragmentLineSources,
        times: &mut HashMap<NodeId, FragmentTime>,
    ) -> Result<(), Error> {
        let storage = self.engine.graph().storage();
        let source_ids: HashSet<_> = line_sources
            .values()
            .flat_map(|sources| sources.values().copied())
            .collect();
        for source_id in source_ids {
            if times.contains_key(&source_id) {
                continue;
            }
            let source = storage.get_node(source_id)?;
            if source.node_type != KnowledgeType::Episodic
                || source
                    .metadata
                    .get("retracted")
                    .is_some_and(|value| value == "true")
            {
                continue;
            }
            times.insert(
                source_id,
                FragmentTime {
                    observed_at: source.created_at,
                    valid_from: source.valid_from,
                    valid_until: source.valid_until,
                },
            );
        }
        Ok(())
    }

    fn recall_relative_time_resolutions(
        &self,
        recall: &Recall,
        times: &HashMap<NodeId, FragmentTime>,
        query: Option<&str>,
    ) -> HashMap<NodeId, Vec<crate::query::temporal::RelativeTimeResolution>> {
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
        for fragment in fragments.into_iter().take(DEFAULT_RERANK_FINAL_LIMIT) {
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

    fn compile_recall_readout(
        &self,
        plan: &RecallPlan,
        recall: &Recall,
        times: &HashMap<NodeId, FragmentTime>,
        line_sources: &FragmentLineSources,
    ) -> Result<RecallReadout, Error> {
        let has_evidence = !recall.package.identity.is_empty()
            || !recall.package.knowledge.is_empty()
            || !recall.package.memories.is_empty();
        let reader_contract = plan.reader_contract();
        let reader_guidance = has_evidence.then(|| reader_contract.context_guidance());
        let guidance_tokens = reader_guidance.as_deref().map_or(0, |guidance| {
            let block = format!("## RECALL GUIDANCE\n- {guidance}\n\n");
            estimate_tokens(&block, CONTEXT_RENDER_CHARS_PER_TOKEN)
        });
        let focused_evidence = if has_evidence {
            select_query_focused_evidence(
                &recall.package,
                times,
                line_sources,
                plan,
                recall
                    .package
                    .token_usage
                    .remaining()
                    .saturating_sub(guidance_tokens),
            )
        } else {
            Vec::new()
        };
        let mut source_node_ids = Vec::new();
        for fragment in recall
            .package
            .identity
            .iter()
            .chain(recall.package.knowledge.iter())
            .chain(recall.package.memories.iter())
        {
            if fragment.node_type == KnowledgeType::Semantic
                && let Some(bound_lines) = line_sources.get(&fragment.node_id)
                && !bound_lines.is_empty()
            {
                source_node_ids.extend(bound_lines.values().copied());
            } else {
                source_node_ids.push(fragment.node_id);
            }
        }
        source_node_ids.sort_unstable();
        source_node_ids.dedup();
        let storage = self.engine.graph().storage();
        let mut source_speakers = HashMap::new();
        for source_node_id in &source_node_ids {
            let source = storage.get_node(*source_node_id)?;
            if let (Some(speaker), _) = parse_entity_tags(&source.entity_tags) {
                source_speakers.insert(*source_node_id, speaker);
            }
        }
        let mut source_attributions = Vec::new();
        for fragment in recall
            .package
            .identity
            .iter()
            .chain(recall.package.knowledge.iter())
            .chain(recall.package.memories.iter())
        {
            if fragment.node_type == KnowledgeType::Semantic
                && let Some(bound_lines) = line_sources.get(&fragment.node_id)
                && !bound_lines.is_empty()
            {
                if let Some(content) = fragment.content.as_deref() {
                    for (line_order, line) in content.lines().enumerate() {
                        let line = line.trim();
                        let Some(source_node_id) = bound_lines.get(line).copied() else {
                            continue;
                        };
                        source_attributions.push(RecallSourceAttribution::new(
                            source_node_id,
                            source_speakers.get(&source_node_id).cloned(),
                            line,
                            fragment.origin.session_id.clone(),
                            fragment.node_id,
                            line_order,
                        ));
                    }
                }
            } else {
                let body = fragment
                    .content
                    .as_deref()
                    .or(fragment.summary.as_deref())
                    .unwrap_or(fragment.name.as_str());
                for (line_order, line) in body.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    source_attributions.push(RecallSourceAttribution::new(
                        fragment.node_id,
                        source_speakers.get(&fragment.node_id).cloned(),
                        line,
                        fragment.origin.session_id.clone(),
                        fragment.node_id,
                        line_order,
                    ));
                }
            }
        }

        Ok(RecallReadout {
            plan: plan.clone(),
            reader_contract,
            reader_guidance,
            source_node_ids,
            source_attributions,
            focused_evidence,
            validation_context: GroundedDraftValidationContext::membership_only(),
            context_aware_validation: false,
            validation_binding: None,
        })
    }

    /// Compile deterministic reader intent and exact focused evidence for a
    /// previously retrieved [`Recall`].
    ///
    /// The returned [`RecallReadout`] is the structured counterpart of the
    /// guidance and focused-evidence tail emitted by
    /// [`render_context_for`](Memory::render_context_for). It is read-only,
    /// performs no model or network call, and does not retrieve evidence beyond
    /// the validated package. Source nodes needed to validate delivered
    /// `Semantic` lines are read by id or indexed session provenance.
    pub fn readout_for(&self, query: &str, recall: &Recall) -> Result<RecallReadout, Error> {
        let plan = RecallPlan::infer(query);
        self.readout_for_plan(&plan, recall)
    }

    /// Compile a structured reader contract using a precomputed recall plan.
    ///
    /// This is useful when a protocol already has a typed [`AnswerShape`] or
    /// [`RecallCoverage`]. The plan affects deterministic presentation intent
    /// and focused selection but cannot add evidence to the supplied [`Recall`].
    pub fn readout_for_plan(
        &self,
        plan: &RecallPlan,
        recall: &Recall,
    ) -> Result<RecallReadout, Error> {
        let mut times = self.recall_fragment_times(recall)?;
        let line_sources = self.recall_fragment_line_sources(recall)?;
        self.extend_line_source_times(&line_sources, &mut times)?;
        self.compile_recall_readout(plan, recall, &times, &line_sources)
    }

    /// Compile a structured reader contract from one completed prepared-rerank
    /// receipt.
    ///
    /// Unlike [`Self::readout_for_plan`], this entry point retains commit-safe
    /// source-role authority issued during completion. Event-boundary
    /// collection drafts therefore validate anchor premises and target items
    /// without reconstructing roles from ordinary recall membership.
    pub fn readout_for_reranked(&self, reranked: &RerankedRecall) -> Result<RecallReadout, Error> {
        let mut readout = self.readout_for_plan(&reranked.plan, &reranked.recall)?;
        readout.context_aware_validation = true;
        let binding_is_current = reranked.validation_binding.as_ref().is_some_and(|binding| {
            binding.matches(reranked)
                && binding.readout.matches(&readout)
                && reranked
                    .validation_context
                    .is_bound_to(&readout.plan, &readout.source_node_ids)
        });
        if binding_is_current
            && let Some(evidence) = reranked.validation_context.event_boundary_evidence()
        {
            readout.validation_context = reranked.validation_context.clone();
            let authority_guidance = readout::event_boundary_authority_guidance(evidence);
            readout.reader_guidance = Some(match readout.reader_guidance.take() {
                Some(guidance) if !guidance.trim().is_empty() => {
                    format!("{guidance} {authority_guidance}")
                }
                Some(_) | None => authority_guidance,
            });
            readout.validation_binding = Some(RecallReadoutValidationBinding::capture(&readout));
        }
        Ok(readout)
    }

    /// Render one completed prepared-rerank receipt with its commit-safe
    /// source-role contract.
    pub fn render_context_for_reranked(&self, reranked: &RerankedRecall) -> Result<String, Error> {
        self.render_context_for_reranked_with(reranked, ContextRenderOptions::default())
    }

    /// Render one completed prepared-rerank receipt through a selected layout.
    ///
    /// When a validated event-boundary role group survived delivery, a bounded
    /// provider-neutral section identifies only its direction and canonical
    /// raw source ids. Internal fact ids and search-trace encodings are never
    /// exposed. Without such a receipt, output is byte-for-byte identical to
    /// [`Self::render_context_for_plan_with`] for the same plan, recall, and
    /// options.
    pub fn render_context_for_reranked_with(
        &self,
        reranked: &RerankedRecall,
        options: ContextRenderOptions,
    ) -> Result<String, Error> {
        let readout = self.readout_for_reranked(reranked)?;
        let mut rendered =
            self.render_context_for_plan_with(&reranked.plan, &reranked.recall, options)?;
        let Some(authority_guidance) = readout.system_authority_guidance() else {
            return Ok(rendered);
        };
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        rendered.push_str(&format!(
            "## EVENT BOUNDARY SOURCE ROLES\n- {authority_guidance}\n\n",
        ));
        Ok(rendered)
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
    /// observation time. Query-aware rendering can also append a bounded
    /// excerpt of exact delivered raw lines and deterministic reading guidance
    /// after the complete evidence. Rendering without a query remains
    /// byte-for-byte compatible with [`render_context`](Memory::render_context).
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
    /// [`AnswerShape`] and [`RecallCoverage`] while keeping evidence matching,
    /// temporal compilation, and rendering inside `Memory`.
    pub fn render_context_for_plan_with(
        &self,
        plan: &RecallPlan,
        recall: &Recall,
        mut options: ContextRenderOptions,
    ) -> Result<String, Error> {
        if readout::plan_uses_temporal_readout(plan) {
            options.resolve_relative_times = true;
        }
        self.render_context_internal(recall, options, Some(plan))
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
        query: Option<&RecallPlan>,
    ) -> Result<String, Error> {
        let mut times = self.recall_fragment_times(recall)?;
        let line_sources = self.recall_fragment_line_sources(recall)?;
        self.extend_line_source_times(&line_sources, &mut times)?;
        let relative_times = options.resolve_relative_times.then(|| {
            self.recall_relative_time_resolutions(
                recall,
                &times,
                query.map(|plan| plan.query.as_str()),
            )
        });
        let mut context = match options.style {
            ContextRenderStyle::Detailed => render_context_package(
                &recall.package,
                Some(&times),
                relative_times.as_ref(),
                Some(&line_sources),
            ),
            ContextRenderStyle::Evidence => render_evidence_context(
                &recall.package,
                &times,
                relative_times.as_ref(),
                Some(&line_sources),
            ),
        };
        let readout = query
            .map(|plan| self.compile_recall_readout(plan, recall, &times, &line_sources))
            .transpose()?;
        if let Some(guidance) = readout
            .as_ref()
            .and_then(|readout| readout.reader_guidance.as_deref())
            .map(|guidance| format!("## RECALL GUIDANCE\n- {guidance}\n\n"))
        {
            if !context.ends_with('\n') {
                context.push('\n');
            }
            context.push_str(&guidance);
        }
        if let Some(focused) = readout
            .as_ref()
            .and_then(|readout| render_query_focused_evidence(&readout.focused_evidence))
        {
            if !context.ends_with('\n') {
                context.push('\n');
            }
            context.push_str(&focused);
        }
        Ok(context)
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
    /// runs the canonical `SearchInput` through the engine, and maps the
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
        let now = Timestamp::now();
        let plan = RecallPlan::infer(query);
        self.search_reranked_for_plan_at(&plan, reranker, options, now)
    }

    /// Run the canonical production recall pipeline with a precomputed plan.
    ///
    /// The plan's query is authoritative for cognitive search, local reranking,
    /// source-aware selection, and the plan returned in [`RerankedRecall`]. This
    /// is the structured-intent entry point for consumers that use
    /// [`RecallPlan::infer_with_answer_shape`] or
    /// [`RecallPlan::with_coverage`].
    pub fn search_reranked_for_plan(
        &mut self,
        plan: &RecallPlan,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
    ) -> Result<RerankedRecall, Error> {
        self.search_reranked_for_plan_at(plan, reranker, options, Timestamp::now())
    }

    /// Run canonical production recall at an explicit timestamp.
    pub fn search_reranked_at(
        &mut self,
        query: &str,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
        now: Timestamp,
    ) -> Result<RerankedRecall, Error> {
        let plan = RecallPlan::infer(query);
        self.search_reranked_for_plan_at(&plan, reranker, options, now)
    }

    /// Run canonical production recall from a precomputed plan at `now`.
    ///
    /// All retrieval, document compilation, adaptive delivery, and deep
    /// selection stages consume this same plan. No model or transport behavior
    /// is introduced by accepting the plan.
    pub fn search_reranked_for_plan_at(
        &mut self,
        plan: &RecallPlan,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
        now: Timestamp,
    ) -> Result<RerankedRecall, Error> {
        let prepared = self.prepare_rerank_for_plan_at(plan, options, now)?;
        self.rerank_prepared_with_provider(prepared, reranker)
    }

    /// Apply the canonical production rerank and deep-selection stages to an
    /// existing live [`SearchResult`].
    ///
    /// Diagnostic consumers may use this overload to inspect one existing
    /// source search without re-running it. Ordinary callers use
    /// [`search_reranked`](Memory::search_reranked). `query` must be the
    /// original query used to produce `result`; an explicit `options.scope`
    /// must likewise match the scope used for that source search.
    pub fn rerank_search_result_at(
        &self,
        query: &str,
        result: &SearchResult,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
        as_of: Timestamp,
    ) -> Result<RerankedRecall, Error> {
        let plan = RecallPlan::infer(query);
        self.rerank_search_result_for_plan_at(&plan, result, reranker, options, as_of)
    }

    fn rerank_search_result_for_plan_at(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        reranker: &dyn RerankingProvider,
        options: RerankedRecallOptions,
        as_of: Timestamp,
    ) -> Result<RerankedRecall, Error> {
        let query_scope = options.scope.clone().unwrap_or_else(ScopePath::universal);
        let prepared = self.prepare_rerank_from_search_result_for_plan_at(
            plan,
            result,
            query_scope,
            options,
            as_of,
        )?;
        self.rerank_prepared_with_provider(prepared, reranker)
    }

    fn rerank_prepared_with_provider(
        &self,
        prepared: PreparedRerank,
        reranker: &dyn RerankingProvider,
    ) -> Result<RerankedRecall, Error> {
        if prepared.document_count() == 0 {
            return self.complete_prepared_rerank(prepared, &[]);
        }

        let documents: Vec<_> = prepared.rerank_texts().map(str::to_owned).collect();
        let scores = reranker.rerank(prepared.query(), &documents)?;
        if scores.is_empty() {
            return Err(Error::InvalidInput(format!(
                "reranker {:?} returned no scores for {} documents",
                reranker.model_name(),
                documents.len()
            )));
        }
        self.complete_prepared_rerank(prepared, &scores)
    }

    /// Prepare canonical production recall for external scoring at wall-clock time.
    pub fn prepare_rerank(
        &mut self,
        query: &str,
        options: RerankedRecallOptions,
    ) -> Result<PreparedRerank, Error> {
        let plan = RecallPlan::infer(query);
        self.prepare_rerank_for_plan_at(&plan, options, Timestamp::now())
    }

    /// Prepare canonical production recall for external scoring at `now`.
    pub fn prepare_rerank_at(
        &mut self,
        query: &str,
        options: RerankedRecallOptions,
        now: Timestamp,
    ) -> Result<PreparedRerank, Error> {
        let plan = RecallPlan::infer(query);
        self.prepare_rerank_for_plan_at(&plan, options, now)
    }

    /// Prepare a precomputed plan for external scoring at wall-clock time.
    pub fn prepare_rerank_for_plan(
        &mut self,
        plan: &RecallPlan,
        options: RerankedRecallOptions,
    ) -> Result<PreparedRerank, Error> {
        self.prepare_rerank_for_plan_at(plan, options, Timestamp::now())
    }

    /// Run canonical source search and prepare its bounded evidence for an
    /// external reranker at `now`.
    ///
    /// This method owns the complete production search. The plan query, scope,
    /// timestamp, search width, diagnostic candidate surface, and downstream
    /// options are captured in one opaque receipt, so an external transport
    /// cannot accidentally combine a plan with a different [`SearchResult`].
    ///
    /// Before any text is exposed through [`PreparedRerank::rerank_texts`],
    /// every document representative, delivered raw source, and scoring-only
    /// raw dialogue contributor is bound to its storage incarnation and checked
    /// for type, scope, retraction, creation time, and validity. A Trend plan
    /// admits expired historical evidence, but still rejects future, retracted,
    /// malformed, or stale allocations.
    /// `ScopePath` is a retrieval boundary, not caller authentication; the
    /// consumer remains responsible for choosing an authorized scope.
    pub fn prepare_rerank_for_plan_at(
        &mut self,
        plan: &RecallPlan,
        options: RerankedRecallOptions,
        as_of: Timestamp,
    ) -> Result<PreparedRerank, Error> {
        validate_prepared_rerank_options(&options)?;
        validate_prepared_rerank_timestamp(as_of)?;
        let diagnostics =
            SearchDiagnostics::with_readout_trace_limit(options.candidate_limit.max(200));
        let scope = options.scope.clone().unwrap_or_else(ScopePath::universal);
        let result = self.search_result_scoped_at_with_diagnostics_for_plan(
            plan,
            options.search_limit,
            as_of,
            &SearchTuning::default(),
            &diagnostics,
            scope.clone(),
        )?;
        self.prepare_rerank_from_search_result_for_plan_at(plan, &result, scope, options, as_of)
    }

    /// Bind a diagnostic source-search result to an internal receipt.
    ///
    /// Only engine-owned callers reach this helper. Public external scoring
    /// goes through [`Memory::prepare_rerank_for_plan_at`], which creates the
    /// source search and receipt together.
    fn prepare_rerank_from_search_result_for_plan_at(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        scope: ScopePath,
        options: RerankedRecallOptions,
        as_of: Timestamp,
    ) -> Result<PreparedRerank, Error> {
        validate_prepared_rerank_options(&options)?;
        validate_prepared_rerank_timestamp(as_of)?;
        if options
            .scope
            .as_ref()
            .is_some_and(|configured| configured != &scope)
        {
            return Err(Error::InvalidInput(
                "prepared rerank scope must match the scope in reranked recall options".to_owned(),
            ));
        }

        let evidence = self.rerank_documents_for_plan_at_internal(
            plan,
            result,
            options.candidate_limit,
            &scope,
            as_of,
        )?;
        let validity_policy = RerankedValidityPolicy::for_plan(plan);
        let mut eligible_evidence = Vec::with_capacity(evidence.len());
        let mut bindings = HashMap::with_capacity(evidence.len());
        for document in evidence {
            let Some(binding) = bind_evidence_document(self.engine.graph().storage(), &document)?
            else {
                continue;
            };
            if !bound_evidence_document_is_eligible_with_policy(
                self.engine.graph().storage(),
                &binding,
                &scope,
                as_of,
                validity_policy,
            )? {
                continue;
            }
            if bindings
                .insert(binding.representative.node_id, binding)
                .is_some()
            {
                return Err(Error::InvalidInput(format!(
                    "rerank documents contain duplicate representative node {:?}",
                    document.node_id
                )));
            }
            eligible_evidence.push(document);
        }

        Ok(PreparedRerank {
            authority: self.prepared_rerank_authority.clone(),
            plan: plan.clone(),
            result: result.clone(),
            evidence: eligible_evidence,
            bindings,
            scope,
            as_of,
            options,
        })
    }

    /// Complete an externally scored rerank through production revalidation.
    ///
    /// `scores` address the zero-based document order returned by
    /// [`PreparedRerank::rerank_texts`]. Indices must be unique and in bounds,
    /// and scores must be finite. A provider may score a non-empty subset, but
    /// it may not return an empty set for a non-empty receipt.
    ///
    /// Completion consumes the receipt, revalidates every bound representative,
    /// delivered source, and scoring-only raw contributor, and exactly
    /// recompiles scoring context before applying adaptive delivery and deep
    /// selection. It then rebuilds the package and commit trace from retained
    /// evidence only. Sources deleted, retracted, replaced, reused, regrouped,
    /// or attached to changed scoring cues after preparation are therefore not
    /// delivered.
    pub fn complete_prepared_rerank(
        &self,
        prepared: PreparedRerank,
        scores: &[RerankScore],
    ) -> Result<RerankedRecall, Error> {
        let PreparedRerank {
            authority,
            plan,
            result,
            evidence,
            bindings,
            scope,
            as_of,
            options,
        } = prepared;
        if !Arc::ptr_eq(&self.prepared_rerank_authority, &authority) {
            return Err(Error::InvalidInput(
                "prepared rerank receipt belongs to a different Memory instance".to_owned(),
            ));
        }

        let ranking = compile_prepared_rerank_scores(&evidence, scores)?;
        if evidence.is_empty() {
            return Ok(RerankedRecall {
                plan,
                ranking,
                rerank_documents: evidence,
                recall: empty_reranked_recall(&result),
                cognitive_scores: Vec::new(),
                validation_context: GroundedDraftValidationContext::membership_only(),
                validation_binding: None,
            });
        }

        // Recompile the complete scoring surface from current storage. Node
        // incarnation checks below protect raw allocations; exact document
        // equality additionally protects scoring-only dialogue context,
        // reviewed atomic cues, and source-topology-derived grouping. A score
        // never transfers to a document whose bytes or source allocation
        // changed while an external provider was running.
        let exact_document_ids = self.exact_current_prepared_document_ids(
            &plan,
            &result,
            &evidence,
            options.candidate_limit,
            &scope,
            as_of,
        )?;
        let ranking: Vec<_> = ranking
            .into_iter()
            .filter(|candidate| exact_document_ids.contains(&candidate.node_id))
            .collect();

        let deep = if options.adaptive_delivery {
            DeepRecallOptions {
                limit: readout::adaptive_delivery_limit(&plan, options.deep.limit),
                selection: options.deep.selection,
            }
        } else {
            options.deep
        };
        let BoundRerankRepackage {
            recall,
            hydrated_readout,
            eligible_ranking,
            event_boundary_group,
        } = self.repackage_bound_reranked_deep_for_plan_at(
            &plan, &result, &ranking, &bindings, deep, &scope, as_of,
        )?;
        let final_ids: HashSet<_> = recall.hits.iter().map(|hit| hit.node_id).collect();
        let cognitive_scores: Vec<CognitiveRecallScore> = hydrated_readout
            .iter()
            .filter(|candidate| final_ids.contains(&candidate.node_id))
            .map(|candidate| CognitiveRecallScore {
                node_id: candidate.node_id,
                score: candidate.score,
                cosine: candidate.embedding_cosine,
            })
            .collect();
        let (validation_context, validation_readout_binding) = if event_boundary_group.is_some() {
            let readout = self.readout_for_plan(&plan, &recall)?;
            let context = readout::event_boundary_validation_context(
                &plan,
                event_boundary_group.as_ref(),
                &readout.source_node_ids,
            );
            let binding = context
                .event_boundary_evidence()
                .map(|_| RecallReadoutValidationBinding::capture(&readout));
            (context, binding)
        } else {
            (GroundedDraftValidationContext::membership_only(), None)
        };
        let validation_binding =
            validation_readout_binding.map(|readout| RerankedRecallValidationBinding {
                plan: plan.clone(),
                ranking: eligible_ranking.clone(),
                rerank_documents: evidence.clone(),
                recall: recall.clone(),
                cognitive_scores: cognitive_scores.clone(),
                readout,
            });
        Ok(RerankedRecall {
            plan,
            ranking: eligible_ranking,
            rerank_documents: evidence,
            recall,
            cognitive_scores,
            validation_context,
            validation_binding,
        })
    }

    fn exact_current_prepared_document_ids(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        prepared_evidence: &[EvidenceDocument],
        candidate_limit: usize,
        scope: &ScopePath,
        as_of: Timestamp,
    ) -> Result<HashSet<NodeId>, Error> {
        let current_evidence = match self.rerank_documents_for_plan_at_internal(
            plan,
            result,
            candidate_limit,
            scope,
            as_of,
        ) {
            Ok(evidence) => evidence,
            Err(Error::NodeNotFound(_) | Error::EdgeNotFound(_)) => return Ok(HashSet::new()),
            Err(error) => return Err(error),
        };
        let mut current_by_representative = HashMap::with_capacity(current_evidence.len());
        for document in current_evidence {
            let node_id = document.node_id;
            if current_by_representative
                .insert(node_id, document)
                .is_some()
            {
                return Err(Error::InvalidInput(format!(
                    "rerank documents contain duplicate representative node {node_id:?}"
                )));
            }
        }

        Ok(prepared_evidence
            .iter()
            .filter(|document| {
                current_by_representative
                    .get(&document.node_id)
                    .is_some_and(|current| current == *document)
            })
            .map(|document| document.node_id)
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    fn repackage_bound_reranked_deep_for_plan_at(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        bindings: &HashMap<NodeId, BoundEvidenceDocument>,
        options: DeepRecallOptions,
        query_scope: &ScopePath,
        as_of: Timestamp,
    ) -> Result<BoundRerankRepackage, Error> {
        let storage = self.engine.graph().storage();
        let validity_policy = RerankedValidityPolicy::for_plan(plan);
        let mut seen_source_sets = HashSet::new();
        let mut eligible_ranking = Vec::with_capacity(ranking.len());
        let mut eligible_sources = HashMap::new();

        // Revalidate after the reranker returns. Exact source-set duplicates
        // (normally an Episodic node and an overlapping Semantic view of that
        // same turn) keep only their highest-ranked document so the final
        // fragment budget is spent on distinct authoritative evidence.
        for candidate in ranking {
            let Some(binding) = bindings.get(&candidate.node_id) else {
                continue;
            };
            if !bound_evidence_document_is_eligible_with_policy(
                storage,
                binding,
                query_scope,
                as_of,
                validity_policy,
            )? {
                continue;
            }
            let source_ids: Vec<_> = binding
                .sources
                .iter()
                .map(|source| source.node_id)
                .collect();
            if source_ids.is_empty() || !seen_source_sets.insert(source_ids.clone()) {
                continue;
            }
            eligible_sources.insert(candidate.node_id, source_ids);
            eligible_ranking.push(*candidate);
        }
        if eligible_ranking.is_empty() {
            return Ok(BoundRerankRepackage {
                recall: empty_reranked_recall(result),
                hydrated_readout: Vec::new(),
                eligible_ranking: Vec::new(),
                event_boundary_group: None,
            });
        }

        let routed_atomic_sources =
            readout::parse_atomic_source_markers(&result.trace.strategies_used);
        let atomic_relation_paths = readout::validated_atomic_relation_paths(
            storage,
            &result.trace.strategies_used,
            as_of,
            query_scope,
        )?;
        let premise_bundle = readout::validated_premise_bundle_trace(
            storage,
            plan,
            &result.trace.strategies_used,
            as_of,
            query_scope,
        )?;
        let event_boundary_group = readout::validated_event_boundary_source_group(
            storage,
            plan,
            &result.trace.strategies_used,
            &result.trace.readout,
            as_of,
            query_scope,
        )?;
        let selected_documents =
            readout::compile_ranking_with_atomic_chains_and_required_source_groups_at(
                storage,
                plan,
                &eligible_ranking,
                options.selection,
                options.limit,
                &routed_atomic_sources,
                &atomic_relation_paths,
                premise_bundle.as_ref(),
                event_boundary_group.as_ref(),
                as_of,
                query_scope,
            )?;

        let original_readout: HashMap<_, _> = result
            .trace
            .readout
            .iter()
            .map(|candidate| (candidate.node_id, candidate))
            .collect();
        let mut hydrated_ranking = Vec::new();
        let mut hydrated_readout = Vec::new();
        let mut delivered_nodes = HashSet::new();
        for document in selected_documents {
            let Some(binding) = bindings.get(&document.node_id) else {
                continue;
            };
            let representative = original_readout.get(&document.node_id).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "reranked node {:?} disappeared from the source readout",
                    document.node_id
                ))
            })?;
            // A Semantic representative can be emitted only when its complete
            // content was reconstructed from the bound raw scoring sources.
            // Otherwise hydrate the validated authoritative sources just like
            // an Episodic/raw document; this prevents an unbound line retained
            // in a derived window from entering the final hit or package.
            let delivered_ids = if binding.representative.node_type == KnowledgeType::Episodic
                || !binding.representative_text_is_bound
            {
                eligible_sources
                    .get(&document.node_id)
                    .cloned()
                    .unwrap_or_default()
            } else {
                vec![document.node_id]
            };
            for delivered_id in delivered_ids {
                if !delivered_nodes.insert(delivered_id) {
                    continue;
                }
                hydrated_ranking.push(RerankedCandidate {
                    node_id: delivered_id,
                    score: document.score,
                });
                let mut readout = original_readout
                    .get(&delivered_id)
                    .copied()
                    .unwrap_or(representative)
                    .clone();
                readout.node_id = delivered_id;
                hydrated_readout.push(readout);
            }
        }
        if hydrated_ranking.is_empty() {
            return Ok(BoundRerankRepackage {
                recall: empty_reranked_recall(result),
                hydrated_readout: Vec::new(),
                eligible_ranking,
                event_boundary_group,
            });
        }

        let hydrated_ranking =
            readout::compile_atomic_chain_source_ranking_with_required_source_groups(
                storage,
                plan,
                &hydrated_ranking,
                options.limit,
                &atomic_relation_paths,
                premise_bundle.as_ref(),
                event_boundary_group.as_ref(),
            )?;
        let hydrated_ranking = readout::compile_strict_factual_action_source_ranking_at(
            storage,
            plan,
            &hydrated_ranking,
            options.limit,
            &routed_atomic_sources,
            as_of,
            query_scope,
        )?;

        let mut hydrated_result = result.clone();
        hydrated_result.trace.readout = hydrated_readout.clone();
        let recall = self.repackage_reranked_at_with_policy(
            &hydrated_result,
            &hydrated_ranking,
            options.limit,
            as_of,
            validity_policy,
        )?;
        Ok(BoundRerankRepackage {
            recall,
            hydrated_readout,
            eligible_ranking,
            event_boundary_group,
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

        // Build the canonical SearchInput: text + query embedding + limit +
        // seed_limit = Some(limit.max(1)); broad entity cues off; explicit now.
        let input = SearchInput {
            text: query.to_string(),
            query_embedding: Some(embedding),
            limit,
            seed_limit: Some(limit.max(1)),
            now,
            scope: scope.unwrap_or_else(ScopePath::universal),
            entity_tags: Vec::new(), // broad entity seeding is opt-in
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
    /// consumers and diagnostic tooling that need the full readout trace or need
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
        let plan = RecallPlan::infer(query);
        self.search_result_scoped_at_with_diagnostics_for_plan(
            &plan,
            limit,
            now,
            tuning,
            diagnostics,
            scope,
        )
    }

    fn search_result_scoped_at_with_diagnostics_for_plan(
        &mut self,
        plan: &RecallPlan,
        limit: usize,
        now: Timestamp,
        tuning: &SearchTuning,
        diagnostics: &SearchDiagnostics,
        scope: ScopePath,
    ) -> Result<SearchResult, Error> {
        self.flush_all()?;

        let query = plan.query.as_str();
        let (query_variants, engine_variant_indices, atomic_variant_indices, premise_slots) =
            if readout::uses_dense_query_expansion(plan) {
                let relation_first = matches!(
                    plan.answer_shape,
                    readout::AnswerShape::Relationship | readout::AnswerShape::Inference
                );
                let dense_plan =
                    crate::api::planned_complex_dense_query_plan(query, relation_first);
                (
                    dense_plan.variants,
                    dense_plan.engine_variant_indices,
                    dense_plan.atomic_variant_indices,
                    dense_plan.premise_slots,
                )
            } else {
                (vec![query.trim().to_owned()], vec![0], vec![0], Vec::new())
            };
        let query_embeddings = if query_variants.len() > 1 {
            let borrowed: Vec<_> = query_variants.iter().map(String::as_str).collect();
            let embedded = self.provider.embed_queries(&borrowed)?;
            if embedded.len() != query_variants.len() {
                return Err(Error::InvalidInput(format!(
                    "embedding provider returned {} query vectors for {} dense variants",
                    embedded.len(),
                    query_variants.len()
                )));
            }
            embedded
                .into_iter()
                .map(|values| crate::embedding::widen(&values))
                .collect()
        } else {
            vec![embed_one_query(&*self.provider, query)?]
        };
        let embedding = query_embeddings.first().cloned().ok_or_else(|| {
            Error::InvalidInput("embedding provider returned no primary query vector".to_owned())
        })?;
        let mut premise_query_slots = Vec::with_capacity(premise_slots.len());
        for premise_slot in &premise_slots {
            let surface = query_variants
                .get(premise_slot.variant_index)
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "premise dense variant index {} is out of bounds",
                        premise_slot.variant_index
                    ))
                })?;
            let premise_embedding = query_embeddings
                .get(premise_slot.variant_index)
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "premise dense embedding index {} is out of bounds",
                        premise_slot.variant_index
                    ))
                })?;
            premise_query_slots.push(readout::PremiseBundleQuerySlot {
                slot_id: premise_slot.id,
                surface,
                embedding: premise_embedding,
            });
        }
        let mut auxiliary_query_embeddings = Vec::new();
        for &index in engine_variant_indices.iter().filter(|&&index| index != 0) {
            let selected = query_embeddings.get(index).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "engine dense variant index {index} is out of bounds"
                ))
            })?;
            auxiliary_query_embeddings.push(selected.clone());
        }
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
        let mut result = self.engine.search_with_auxiliary_query_embeddings(
            input,
            diagnostics,
            &auxiliary_query_embeddings,
        )?;
        let mut atomic_query_embeddings = Vec::new();
        let mut auxiliary_atomic_query_embeddings = Vec::new();
        for &index in &atomic_variant_indices {
            let selected = query_embeddings.get(index).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "atomic dense variant index {index} is out of bounds"
                ))
            })?;
            atomic_query_embeddings.push(selected.as_slice());
            if index != 0 {
                auxiliary_atomic_query_embeddings.push(selected.as_slice());
            }
        }
        let primary_atomic_embedding = [embedding.as_slice()];
        let mut routed = readout::route_atomic_fact_sources(
            self.engine.graph().storage(),
            plan,
            &primary_atomic_embedding,
            now,
            &scope,
        )?;
        let event_boundary_source_group = readout::canonical_event_boundary_source_group(
            self.engine.graph().storage(),
            plan,
            now,
            &scope,
        )?;
        let routed_premise_bundle = readout::route_premise_bundle(
            self.engine.graph().storage(),
            plan,
            &premise_query_slots,
            now,
            &scope,
        )?;
        let action_uses_predicate_affinity = readout::is_factual_action_request(plan)
            && !auxiliary_atomic_query_embeddings.is_empty();
        if action_uses_predicate_affinity {
            for source in &mut routed {
                source.kind_priority = 0;
                source.fact_priorities.clear();
            }
        }
        let auxiliary_route_embeddings: &[&[f64]] = if action_uses_predicate_affinity {
            &auxiliary_atomic_query_embeddings
        } else {
            &atomic_query_embeddings
        };
        let has_auxiliary_atomic_route = if action_uses_predicate_affinity {
            !auxiliary_route_embeddings.is_empty()
        } else {
            atomic_query_embeddings.len() > 1
        };
        if has_auxiliary_atomic_route {
            let auxiliary_routed = readout::route_atomic_fact_sources(
                self.engine.graph().storage(),
                plan,
                auxiliary_route_embeddings,
                now,
                &scope,
            )?;
            let mut routed_position_by_source: HashMap<_, _> = routed
                .iter()
                .enumerate()
                .map(|(position, source)| (source.candidate.node_id, position))
                .collect();
            for mut auxiliary_source in auxiliary_routed {
                if let Some(position) = routed_position_by_source
                    .get(&auxiliary_source.candidate.node_id)
                    .copied()
                {
                    let baseline_source = &mut routed[position];
                    baseline_source.kind_priority = baseline_source
                        .kind_priority
                        .max(auxiliary_source.kind_priority);
                    for (fact_id, priority) in auxiliary_source.fact_priorities {
                        baseline_source.record_fact_priority(fact_id, priority);
                    }
                    for fact_id in auxiliary_source.fact_ids {
                        if !baseline_source.fact_ids.contains(&fact_id) {
                            baseline_source.fact_ids.push(fact_id);
                        }
                    }
                    continue;
                }
                auxiliary_source.origin = readout::AtomicRouteOrigin::AuxiliaryQuery;
                routed_position_by_source.insert(auxiliary_source.candidate.node_id, routed.len());
                routed.push(auxiliary_source);
            }
        }
        let chain_expansion = readout::expand_atomic_fact_relation_sources(
            self.engine.graph().storage(),
            plan,
            &routed,
            &atomic_query_embeddings,
            now,
            &scope,
        )?;
        let chain_route_count = chain_expansion.sources.len();
        let chain_paths = chain_expansion.paths;
        let chain_diagnostics = chain_expansion.diagnostics;
        routed.extend(chain_expansion.sources);
        let covered_atomic_source_ids: HashSet<_> = if matches!(
            plan.answer_shape,
            AnswerShape::Count | AnswerShape::Frequency
        ) {
            routed
                .iter()
                .map(|source| source.candidate.node_id)
                .collect()
        } else {
            HashSet::new()
        };
        let strict_action_source_ids = readout::strict_factual_action_source_ids(
            self.engine.graph().storage(),
            plan,
            now,
            &scope,
        )?;
        let raw_routed = readout::route_subject_raw_sources(
            self.engine.graph().storage(),
            plan,
            &atomic_query_embeddings,
            now,
            &scope,
            &covered_atomic_source_ids,
            &strict_action_source_ids,
        )?;
        let reply_routed = readout::route_same_session_reply_sources(
            self.engine.graph().storage(),
            plan,
            &result.trace.readout,
            &atomic_query_embeddings,
            now,
            &scope,
            &strict_action_source_ids,
        )?;
        if routed.is_empty()
            && raw_routed.is_empty()
            && reply_routed.is_empty()
            && routed_premise_bundle.is_none()
        {
            return Ok(result);
        }
        let (premise_bundle_trace, premise_bundle_sources) = routed_premise_bundle.map_or_else(
            || (None, Vec::new()),
            |bundle| (Some(bundle.trace), bundle.sources),
        );
        let premise_bundle_source_ids: HashSet<_> = premise_bundle_trace
            .iter()
            .flat_map(|trace| trace.members.iter())
            .map(|member| member.source_node_id)
            .collect();

        // Preserve the authoritative cognitive head exactly. Atomic facts only earn
        // source slots in the deeper lane; direct and time-constrained complex
        // shapes never reach this branch. Existing raw candidates keep their
        // native score signals when promoted, while a source absent from the trace
        // receives the fact-lane synthetic diagnostic score.
        let head_limit = result
            .trace
            .readout
            .len()
            .min(readout::DEFAULT_RERANK_PROTECTED_HEAD_LIMIT);
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
        // Exact-subject raw premises retain four tail slots. The first thirty
        // native rows remain immutable and the complete candidate surface
        // remains fixed at fifty.
        let raw_promotion_limit = readout::subject_raw_promotion_limit(plan, raw_routed.len());
        let chain_atomic_promotion_limit = routed
            .iter()
            .filter(|source| matches!(source.origin, readout::AtomicRouteOrigin::Chain { .. }))
            .map(|source| source.candidate.node_id)
            .filter(|node_id| !head_ids.contains(node_id) && !head_sources.contains(node_id))
            .collect::<HashSet<_>>()
            .len()
            .min(chain_route_count)
            .min(8);
        let direct_atomic_promotion_limit = match plan.answer_shape {
            AnswerShape::Collection => 12,
            AnswerShape::Count | AnswerShape::Frequency => 12,
            AnswerShape::Relationship => 20usize.saturating_sub(raw_promotion_limit),
            AnswerShape::Inference => 12,
            _ => 4,
        }
        .saturating_sub(chain_atomic_promotion_limit);
        // Relation-bearing query surfaces are a recovery lane, not a
        // replacement for the complete-query route. Give novel sources
        // a small independent quota so they cannot consume baseline atomic
        // slots. The later production selector still decides whether any of
        // these recovered raw sources reach the final package.
        let auxiliary_atomic_promotion_limit = match plan.answer_shape {
            AnswerShape::Collection => 4,
            AnswerShape::Inference => 8usize.saturating_sub(raw_promotion_limit),
            _ => 0,
        };
        let mut routed_markers = Vec::new();
        let mut promoted = Vec::new();
        let mut promoted_sources = head_sources.clone();
        let mut direct_promoted = 0usize;
        let mut auxiliary_promoted = 0usize;
        let mut chain_promoted = 0usize;
        let mut deferred = Vec::new();
        let mut deferred_ids = HashSet::new();

        // A complete premise bundle may occupy at most two existing auxiliary
        // recovery slots. If more than two of its raw sources are absent from
        // the fixed candidate surface, promote none and leave the all-or-none
        // preselector to reject the unrepresentable bundle.
        let mut candidate_sources = HashSet::new();
        for candidate in result
            .trace
            .readout
            .iter()
            .take(readout::DEFAULT_RERANK_CANDIDATE_LIMIT)
        {
            candidate_sources.extend(readout::canonical_sources(
                self.engine.graph().storage(),
                candidate.node_id,
            )?);
        }
        let missing_bundle_source_count = premise_bundle_sources
            .iter()
            .filter(|candidate| !candidate_sources.contains(&candidate.node_id))
            .count();
        let bundle_promotion_limit = auxiliary_atomic_promotion_limit.min(2);
        let promote_complete_missing_bundle = missing_bundle_source_count > 0
            && missing_bundle_source_count <= bundle_promotion_limit;
        for routed_candidate in premise_bundle_sources {
            let source_id = routed_candidate.node_id;
            if head_sources.contains(&source_id) || candidate_sources.contains(&source_id) {
                continue;
            }
            if promote_complete_missing_bundle {
                let candidate = result
                    .trace
                    .readout
                    .iter()
                    .position(|existing| existing.node_id == source_id)
                    .map(|position| result.trace.readout.remove(position))
                    .unwrap_or(routed_candidate);
                promoted_sources.insert(source_id);
                candidate_sources.insert(source_id);
                promoted.push(candidate);
                auxiliary_promoted += 1;
            } else if !result
                .trace
                .readout
                .iter()
                .any(|existing| existing.node_id == source_id)
                && deferred_ids.insert(source_id)
            {
                deferred.push(routed_candidate);
            }
        }

        let mut reply_markers = Vec::new();
        for routed_candidate in reply_routed {
            if premise_bundle_source_ids.contains(&routed_candidate.node_id)
                || head_ids.contains(&routed_candidate.node_id)
                || promoted_sources.contains(&routed_candidate.node_id)
            {
                continue;
            }
            let candidate = result
                .trace
                .readout
                .iter()
                .position(|existing| existing.node_id == routed_candidate.node_id)
                .map(|position| result.trace.readout.remove(position))
                .unwrap_or(routed_candidate);
            promoted_sources.insert(candidate.node_id);
            reply_markers.push(candidate.node_id);
            promoted.push(candidate);
        }
        for routed_source in routed {
            let fact_markers = routed_source.fact_markers();
            let route_origin = routed_source.origin;
            let routed_candidate = routed_source.candidate;
            for marker in fact_markers {
                if !routed_markers.contains(&marker) {
                    routed_markers.push(marker);
                }
            }
            if premise_bundle_source_ids.contains(&routed_candidate.node_id)
                || head_ids.contains(&routed_candidate.node_id)
                || head_sources.contains(&routed_candidate.node_id)
                || promoted_sources.contains(&routed_candidate.node_id)
            {
                continue;
            }
            let has_promotion_capacity = match route_origin {
                readout::AtomicRouteOrigin::Direct => {
                    direct_promoted < direct_atomic_promotion_limit
                }
                readout::AtomicRouteOrigin::AuxiliaryQuery => {
                    auxiliary_promoted < auxiliary_atomic_promotion_limit
                }
                readout::AtomicRouteOrigin::Chain { .. } => {
                    chain_promoted < chain_atomic_promotion_limit
                }
            };
            if has_promotion_capacity {
                let candidate = result
                    .trace
                    .readout
                    .iter()
                    .position(|existing| existing.node_id == routed_candidate.node_id)
                    .map(|position| result.trace.readout.remove(position))
                    .unwrap_or(routed_candidate);
                promoted_sources.insert(candidate.node_id);
                promoted.push(candidate);
                match route_origin {
                    readout::AtomicRouteOrigin::Direct => direct_promoted += 1,
                    readout::AtomicRouteOrigin::AuxiliaryQuery => auxiliary_promoted += 1,
                    readout::AtomicRouteOrigin::Chain { .. } => chain_promoted += 1,
                }
            } else if !result
                .trace
                .readout
                .iter()
                .any(|existing| existing.node_id == routed_candidate.node_id)
                && deferred_ids.insert(routed_candidate.node_id)
            {
                deferred.push(routed_candidate);
            }
        }
        let mut raw_promoted = 0usize;
        let mut raw_markers = Vec::new();
        for routed_candidate in raw_routed {
            if head_ids.contains(&routed_candidate.node_id)
                || promoted_sources.contains(&routed_candidate.node_id)
            {
                continue;
            }
            if raw_promoted < raw_promotion_limit {
                let candidate = result
                    .trace
                    .readout
                    .iter()
                    .position(|existing| existing.node_id == routed_candidate.node_id)
                    .map(|position| result.trace.readout.remove(position))
                    .unwrap_or(routed_candidate);
                promoted_sources.insert(candidate.node_id);
                raw_markers.push(candidate.node_id);
                promoted.push(candidate);
                raw_promoted += 1;
            } else if !result
                .trace
                .readout
                .iter()
                .any(|existing| existing.node_id == routed_candidate.node_id)
                && deferred_ids.insert(routed_candidate.node_id)
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
        let mut represented_sources = HashSet::new();
        for candidate in &result.trace.readout {
            represented_sources.extend(readout::canonical_sources(
                self.engine.graph().storage(),
                candidate.node_id,
            )?);
        }
        routed_markers.retain(|marker| represented_sources.contains(&marker.source_node_id));
        raw_markers.retain(|node_id| represented_sources.contains(node_id));
        reply_markers.retain(|node_id| represented_sources.contains(node_id));
        if let Some(trace) = premise_bundle_trace.as_ref().filter(|trace| {
            trace
                .members
                .iter()
                .all(|member| represented_sources.contains(&member.source_node_id))
        }) {
            result
                .trace
                .strategies_used
                .push("grounded_premise_bundle_routing".to_owned());
            if let Some(encoded) = readout::encode_premise_bundle_trace(trace) {
                result.trace.strategies_used.push(encoded);
            }
        }
        if let Some(group) = event_boundary_source_group.as_ref()
            && let Some(encoded) = readout::encode_represented_event_boundary_source_group(
                self.engine.graph().storage(),
                group,
                &result.trace.readout,
                readout::DEFAULT_RERANK_CANDIDATE_LIMIT,
            )?
        {
            result
                .trace
                .strategies_used
                .push("event_boundary_source_group_routing".to_owned());
            result.trace.strategies_used.push(encoded);
        }
        if !routed_markers.is_empty() {
            result
                .trace
                .strategies_used
                .push("atomic_fact_routing".to_owned());
        }
        if let Some(strategy) = readout::encode_atomic_source_markers(&routed_markers) {
            result.trace.strategies_used.push(strategy);
        }
        if !raw_markers.is_empty() {
            result
                .trace
                .strategies_used
                .push("subject_raw_routing".to_owned());
            let routed_ids = raw_markers
                .iter()
                .map(|node_id| node_id.0.to_string())
                .collect::<Vec<_>>()
                .join(",");
            result
                .trace
                .strategies_used
                .push(format!("subject_raw_sources:{routed_ids}"));
        }
        if !reply_markers.is_empty() {
            result
                .trace
                .strategies_used
                .push("same_session_reply_routing".to_owned());
        }
        if chain_diagnostics.visited_relations > 0 {
            result.trace.strategies_used.push(format!(
                "atomic_relation_chain:visited={};facts={};sources={};contradictions_excluded={};truncated={}",
                chain_diagnostics.visited_relations,
                chain_diagnostics.expanded_facts,
                chain_diagnostics.routed_sources,
                chain_diagnostics.contradictions_excluded,
                chain_diagnostics.truncated,
            ));
        }
        if let Some(encoded_paths) = readout::encode_atomic_relation_paths(&chain_paths) {
            result.trace.strategies_used.push(encoded_paths);
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
    /// This is a diagnostic, unbound score surface: node scores alone cannot
    /// prove which raw-source incarnations an external provider inspected.
    /// Production external rerankers should use
    /// [`prepare_rerank_for_plan_at`](Memory::prepare_rerank_for_plan_at) and
    /// [`complete_prepared_rerank`](Memory::complete_prepared_rerank).
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

    /// Compile reranker documents after inferring a deterministic [`RecallPlan`].
    ///
    /// Count, frequency, and non-temporal relationship queries use canonical
    /// raw-evidence documents to protect distinct facts from overlapping graph
    /// windows. Ordinary collection and inference documents can retain a
    /// highest-ranked Semantic scoring window while exposing canonical raw
    /// evidence in [`EvidenceDocument::text`]. Explicitly exhaustive
    /// collections instead retain distinct canonical raw documents within the
    /// bounded candidate surface. Direct and temporal queries preserve the
    /// ordinary node-document surface.
    ///
    /// A consumer with structured intent should call
    /// [`rerank_documents_for_plan`](Memory::rerank_documents_for_plan), score
    /// each [`EvidenceDocument::rerank_text`], and pass that same plan and the
    /// resulting node scores to
    /// [`repackage_reranked_deep_for_plan`](Memory::repackage_reranked_deep_for_plan).
    /// This pair is retained as a diagnostic, unbound surface. Production
    /// external rerankers should use
    /// [`prepare_rerank_for_plan_at`](Memory::prepare_rerank_for_plan_at), which
    /// binds source incarnations before exposing these texts.
    pub fn rerank_documents(
        &self,
        query: &str,
        result: &SearchResult,
        candidate_limit: usize,
    ) -> Result<Vec<EvidenceDocument>, Error> {
        let plan = RecallPlan::infer(query);
        self.rerank_documents_for_plan(&plan, result, candidate_limit)
    }

    /// Compile reranker documents using an existing deterministic recall plan.
    ///
    /// The supplied plan is authoritative for answer shape, temporal state,
    /// and bounded [`RecallCoverage`]. No intent is inferred again. Reviewed
    /// atomic routing cues may be added only to the scoring surface; canonical
    /// raw source nodes and [`EvidenceDocument::text`] remain authoritative.
    ///
    /// This method is diagnostic and unbound across an external provider call.
    /// Use [`prepare_rerank_for_plan_at`](Memory::prepare_rerank_for_plan_at)
    /// for production external scoring.
    pub fn rerank_documents_for_plan(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        candidate_limit: usize,
    ) -> Result<Vec<EvidenceDocument>, Error> {
        if candidate_limit == 0 {
            return Err(Error::InvalidInput(
                "rerank document candidate limit must be greater than zero".to_owned(),
            ));
        }
        self.rerank_documents_for_plan_at_internal(
            plan,
            result,
            candidate_limit,
            &ScopePath::universal(),
            Timestamp::now(),
        )
    }

    fn rerank_documents_for_plan_at_internal(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        candidate_limit: usize,
        scope: &ScopePath,
        as_of: Timestamp,
    ) -> Result<Vec<EvidenceDocument>, Error> {
        let routed_atomic_markers =
            readout::parse_atomic_source_markers(&result.trace.strategies_used);
        let premise_bundle = readout::validated_premise_bundle_trace(
            self.engine.graph().storage(),
            plan,
            &result.trace.strategies_used,
            as_of,
            scope,
        )?;
        let event_boundary_group = readout::validated_event_boundary_source_group(
            self.engine.graph().storage(),
            plan,
            &result.trace.strategies_used,
            &result.trace.readout,
            as_of,
            scope,
        )?;
        readout::compile_rerank_documents_with_required_source_groups_at(
            self.engine.graph().storage(),
            plan,
            &result.trace.readout,
            candidate_limit,
            &routed_atomic_markers,
            premise_bundle.as_ref(),
            event_boundary_group.as_ref(),
            as_of,
            scope,
        )
    }

    /// Compile consumer scores through the model-free deep readout at wall-clock time.
    ///
    /// The consumer supplies only scores. Query intent detection, canonical raw
    /// source grouping, evidence selection, validity, packaging, and commit
    /// trace reconstruction remain owned by [`Memory`].
    ///
    /// This scores-only overload is diagnostic and unbound. Production external
    /// scoring should complete a [`PreparedRerank`] receipt instead.
    pub fn repackage_reranked_deep(
        &self,
        query: &str,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        options: DeepRecallOptions,
    ) -> Result<Recall, Error> {
        self.repackage_reranked_deep_at(query, result, ranking, options, Timestamp::now())
    }

    /// Compile consumer scores through a precomputed plan at wall-clock time.
    ///
    /// Pass the same plan used by
    /// [`rerank_documents_for_plan`](Memory::rerank_documents_for_plan) so
    /// caller-supplied answer shape, temporal state, and bounded coverage are
    /// retained through source-aware selection and packaging.
    /// This scores-only overload is diagnostic and unbound; prefer
    /// [`Memory::complete_prepared_rerank`] for external production reranking.
    pub fn repackage_reranked_deep_for_plan(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        options: DeepRecallOptions,
    ) -> Result<Recall, Error> {
        self.repackage_reranked_deep_for_plan_at(plan, result, ranking, options, Timestamp::now())
    }

    /// Compile consumer scores through deterministic deep readout at `as_of`.
    ///
    /// `query` must be the original query used to produce `result`. This method
    /// infers the same deterministic plan used by the query-based document
    /// compiler. Structured consumers should instead use
    /// [`repackage_reranked_deep_for_plan_at`](Memory::repackage_reranked_deep_for_plan_at)
    /// with the same plan they supplied to document compilation. The compiled
    /// ranking is validated by
    /// [`repackage_reranked_at`](Memory::repackage_reranked_at), so no source,
    /// validity, tension, budget, or commit invariant is bypassed.
    /// This scores-only overload is diagnostic and unbound across provider work.
    pub fn repackage_reranked_deep_at(
        &self,
        query: &str,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        options: DeepRecallOptions,
        as_of: Timestamp,
    ) -> Result<Recall, Error> {
        let plan = RecallPlan::infer(query);
        self.repackage_reranked_deep_for_plan_at(&plan, result, ranking, options, as_of)
    }

    /// Compile consumer scores through a precomputed plan at `as_of`.
    ///
    /// [`EvidenceSelection::Auto`] resolves its source-aware policy from the
    /// complete plan. The plan is consumed as supplied; answer shape, temporal
    /// state, and bounded coverage are not inferred from query text again. The
    /// compiled ranking is then validated by
    /// [`repackage_reranked_at`](Memory::repackage_reranked_at), so no source,
    /// validity, tension, budget, or commit invariant is bypassed.
    /// This scores-only overload is diagnostic and unbound across provider work.
    pub fn repackage_reranked_deep_for_plan_at(
        &self,
        plan: &RecallPlan,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        options: DeepRecallOptions,
        as_of: Timestamp,
    ) -> Result<Recall, Error> {
        if options.limit == 0 || ranking.is_empty() {
            return self.repackage_reranked_at_with_policy(
                result,
                ranking,
                options.limit,
                as_of,
                RerankedValidityPolicy::for_plan(plan),
            );
        }
        // The diagnostic scores-only surface does not require callers to sort
        // their node-addressed scores. Establish the same documented score and
        // cognitive-readout tie order used by final packaging before any
        // required-source group chooses its bounded target members.
        let ranking = normalized_unbound_reranker_order(result, ranking)?;
        let routed_atomic_sources =
            readout::parse_atomic_source_markers(&result.trace.strategies_used);
        let atomic_relation_paths = readout::validated_atomic_relation_paths(
            self.engine.graph().storage(),
            &result.trace.strategies_used,
            as_of,
            &ScopePath::universal(),
        )?;
        let premise_bundle = readout::validated_premise_bundle_trace(
            self.engine.graph().storage(),
            plan,
            &result.trace.strategies_used,
            as_of,
            &ScopePath::universal(),
        )?;
        let event_boundary_group = readout::validated_event_boundary_source_group(
            self.engine.graph().storage(),
            plan,
            &result.trace.strategies_used,
            &result.trace.readout,
            as_of,
            &ScopePath::universal(),
        )?;
        let compiled = readout::compile_ranking_with_atomic_chains_and_required_source_groups_at(
            self.engine.graph().storage(),
            plan,
            &ranking,
            options.selection,
            options.limit,
            &routed_atomic_sources,
            &atomic_relation_paths,
            premise_bundle.as_ref(),
            event_boundary_group.as_ref(),
            as_of,
            &ScopePath::universal(),
        )?;
        self.repackage_reranked_at_with_policy(
            result,
            &compiled,
            options.limit,
            as_of,
            RerankedValidityPolicy::for_plan(plan),
        )
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
    /// This scores-only overload is diagnostic and unbound across provider work;
    /// external production rerankers should complete a [`PreparedRerank`].
    pub fn repackage_reranked_at(
        &self,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        limit: usize,
        as_of: Timestamp,
    ) -> Result<Recall, Error> {
        self.repackage_reranked_at_with_policy(
            result,
            ranking,
            limit,
            as_of,
            RerankedValidityPolicy::Current,
        )
    }

    fn repackage_reranked_at_with_policy(
        &self,
        result: &SearchResult,
        ranking: &[RerankedCandidate],
        limit: usize,
        as_of: Timestamp,
        validity_policy: RerankedValidityPolicy,
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
        let ranked = normalized_unbound_reranker_order(result, ranking)?
            .into_iter()
            .map(|candidate| {
                let position = readout_positions
                    .get(&candidate.node_id)
                    .copied()
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "reranked node {:?} is absent from the search readout",
                            candidate.node_id
                        ))
                    })?;
                Ok((candidate, position))
            })
            .collect::<Result<Vec<_>, Error>>()?;

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
                match validity_policy {
                    RerankedValidityPolicy::Current => {
                        apply_validity_filter(&self.engine, &mut package, as_of);
                    }
                    RerankedValidityPolicy::HistoricalTrend => {
                        apply_historical_visibility_filter(&self.engine, &mut package, as_of);
                    }
                }
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

fn normalized_unbound_reranker_order(
    result: &SearchResult,
    ranking: &[RerankedCandidate],
) -> Result<Vec<RerankedCandidate>, Error> {
    let readout_positions: HashMap<_, _> = result
        .trace
        .readout
        .iter()
        .enumerate()
        .map(|(position, candidate)| (candidate.node_id, position))
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
        let position = readout_positions
            .get(&candidate.node_id)
            .copied()
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "reranked node {:?} is absent from the search readout",
                    candidate.node_id
                ))
            })?;
        ranked.push((*candidate, position));
    }
    ranked.sort_by(|(left, left_position), (right, right_position)| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left_position.cmp(right_position))
    });
    Ok(ranked.into_iter().map(|(candidate, _)| candidate).collect())
}

fn validate_prepared_rerank_options(options: &RerankedRecallOptions) -> Result<(), Error> {
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
    Ok(())
}

fn validate_prepared_rerank_timestamp(as_of: Timestamp) -> Result<(), Error> {
    if as_of.0 == 0 {
        return Err(Error::InvalidInput(
            "prepared rerank timestamp must be non-zero so authority and validity can be checked"
                .to_owned(),
        ));
    }
    Ok(())
}

fn compile_prepared_rerank_scores(
    evidence: &[EvidenceDocument],
    scores: &[RerankScore],
) -> Result<Vec<RerankedCandidate>, Error> {
    if scores.is_empty() && !evidence.is_empty() {
        return Err(Error::InvalidInput(format!(
            "reranker returned no scores for {} documents",
            evidence.len()
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
        ranking.push((
            score.index,
            RerankedCandidate {
                node_id: document.node_id,
                score: score.score,
            },
        ));
    }
    ranking.sort_by(|(left_index, left), (right_index, right)| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left_index.cmp(right_index))
    });
    Ok(ranking
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect())
}

fn bind_evidence_node<S: StorageAdapter>(
    storage: &S,
    node_id: NodeId,
) -> Result<Option<BoundEvidenceNode>, Error> {
    let node = match storage.get_node(node_id) {
        Ok(node) => node,
        Err(Error::NodeNotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(BoundEvidenceNode {
        node_id,
        incarnation: storage.atomic_source_incarnation(node)?,
        node_type: node.node_type.clone(),
        scope: node.origin.scope.clone(),
    }))
}

fn bind_evidence_document<S: StorageAdapter>(
    storage: &S,
    document: &EvidenceDocument,
) -> Result<Option<BoundEvidenceDocument>, Error> {
    let Some(representative) = bind_evidence_node(storage, document.node_id)? else {
        return Ok(None);
    };
    let mut sources = Vec::with_capacity(document.source_node_ids.len());
    for source_id in &document.source_node_ids {
        let Some(source) = bind_evidence_node(storage, *source_id)? else {
            return Ok(None);
        };
        sources.push(source);
    }
    if sources.is_empty() {
        return Ok(None);
    }
    let delivery_source_ids: HashSet<_> = sources.iter().map(|source| source.node_id).collect();
    let mut seen_scoring_sources = HashSet::new();
    let mut scoring_sources = Vec::with_capacity(document.scoring_source_node_ids().len());
    for source_id in document.scoring_source_node_ids() {
        if delivery_source_ids.contains(source_id) || !seen_scoring_sources.insert(*source_id) {
            continue;
        }
        let Some(source) = bind_evidence_node(storage, *source_id)? else {
            return Ok(None);
        };
        scoring_sources.push(source);
    }
    Ok(Some(BoundEvidenceDocument {
        representative,
        sources,
        scoring_sources,
        representative_text_is_bound: document.representative_text_is_bound(),
    }))
}

fn node_matches_reranked_validity_policy(
    node: &Node,
    as_of: Timestamp,
    policy: RerankedValidityPolicy,
) -> bool {
    if as_of.0 == 0 {
        return true;
    }
    if node.created_at > as_of
        || matches!(
            (node.valid_from, node.valid_until),
            (Some(from), Some(until)) if from >= until
        )
        || node.valid_from.is_some_and(|from| from > as_of)
    {
        return false;
    }
    match policy {
        RerankedValidityPolicy::Current => {
            crate::graph::valid_at(node.valid_from, node.valid_until, as_of)
        }
        RerankedValidityPolicy::HistoricalTrend => true,
    }
}

fn package_node_matches_reranked_validity_policy<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    node_id: NodeId,
    as_of: Timestamp,
    policy: RerankedValidityPolicy,
) -> bool {
    engine.graph().get_node(node_id).is_ok_and(|node| {
        !node
            .metadata
            .get("retracted")
            .is_some_and(|value| value == "true")
            && node_matches_reranked_validity_policy(node, as_of, policy)
    })
}

fn apply_historical_visibility_filter<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    package: &mut ContextPackage,
    as_of: Timestamp,
) {
    let policy = RerankedValidityPolicy::HistoricalTrend;
    package.knowledge.retain(|fragment| {
        package_node_matches_reranked_validity_policy(engine, fragment.node_id, as_of, policy)
    });
    package.memories.retain(|fragment| {
        package_node_matches_reranked_validity_policy(engine, fragment.node_id, as_of, policy)
    });
    package.identity.retain(|fragment| {
        package_node_matches_reranked_validity_policy(engine, fragment.node_id, as_of, policy)
    });
    package.tensions.retain(|tension| {
        package_node_matches_reranked_validity_policy(engine, tension.node_a, as_of, policy)
            && package_node_matches_reranked_validity_policy(engine, tension.node_b, as_of, policy)
    });
}

fn bound_evidence_node_is_eligible_with_policy<S: StorageAdapter>(
    storage: &S,
    binding: &BoundEvidenceNode,
    query_scope: &ScopePath,
    as_of: Timestamp,
    require_episodic: bool,
    validity_policy: RerankedValidityPolicy,
) -> Result<bool, Error> {
    let node = match storage.get_node(binding.node_id) {
        Ok(node) => node,
        Err(Error::NodeNotFound(_)) => return Ok(false),
        Err(error) => return Err(error),
    };
    if (require_episodic && node.node_type != KnowledgeType::Episodic)
        || node.node_type != binding.node_type
        || node.origin.scope != binding.scope
        || storage.atomic_source_incarnation(node)? != binding.incarnation
        || node
            .metadata
            .get("retracted")
            .is_some_and(|value| value == "true")
        || (!query_scope.is_universal()
            && !node.origin.scope.is_universal()
            && node.origin.scope != *query_scope)
        || !node_matches_reranked_validity_policy(node, as_of, validity_policy)
    {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
fn bound_evidence_document_is_eligible<S: StorageAdapter>(
    storage: &S,
    binding: &BoundEvidenceDocument,
    query_scope: &ScopePath,
    as_of: Timestamp,
) -> Result<bool, Error> {
    bound_evidence_document_is_eligible_with_policy(
        storage,
        binding,
        query_scope,
        as_of,
        RerankedValidityPolicy::Current,
    )
}

fn bound_evidence_document_is_eligible_with_policy<S: StorageAdapter>(
    storage: &S,
    binding: &BoundEvidenceDocument,
    query_scope: &ScopePath,
    as_of: Timestamp,
    validity_policy: RerankedValidityPolicy,
) -> Result<bool, Error> {
    if !bound_evidence_node_is_eligible_with_policy(
        storage,
        &binding.representative,
        query_scope,
        as_of,
        false,
        validity_policy,
    )? {
        return Ok(false);
    }
    for source in &binding.sources {
        let compatible_source_scope = binding.representative.scope == source.scope
            || binding.representative.scope.is_universal()
            || source.scope.is_universal();
        if !compatible_source_scope
            || !bound_evidence_node_is_eligible_with_policy(
                storage,
                source,
                query_scope,
                as_of,
                true,
                validity_policy,
            )?
        {
            return Ok(false);
        }
    }
    for source in &binding.scoring_sources {
        let compatible_source_scope = binding.representative.scope == source.scope
            || binding.representative.scope.is_universal()
            || source.scope.is_universal();
        if !compatible_source_scope
            || !bound_evidence_node_is_eligible_with_policy(
                storage,
                source,
                query_scope,
                as_of,
                true,
                validity_policy,
            )?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn empty_reranked_recall(result: &SearchResult) -> Recall {
    let mut package = ContextPackage::empty();
    package.token_usage.total = result.package.token_usage.total;
    Recall {
        hits: Vec::new(),
        package,
    }
}

/// Extract `(speaker, session)` from a node's entity tags.
///
/// Looks for `speaker-<norm>` and `session-<norm>` tags (the convention used
/// by the canonical conversation recipe). Returns `None` for each if the corresponding tag is
/// absent.
fn intersect_atomic_relation_scope(from: &ScopePath, to: &ScopePath) -> Result<ScopePath, Error> {
    if from == to {
        return Ok(from.clone());
    }
    if from.is_universal() {
        return Ok(to.clone());
    }
    if to.is_universal() {
        return Ok(from.clone());
    }
    Err(Error::InvalidInput(format!(
        "atomic fact relation cannot join concrete scopes {from:?} and {to:?}"
    )))
}

fn is_reviewed_derivation_metadata_key(key: &str) -> bool {
    key.starts_with(REVIEWED_DERIVATION_METADATA_PREFIX)
}

fn validate_reviewed_derivation_metadata(
    metadata: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, Error> {
    let mut seen = HashSet::with_capacity(metadata.len());
    for (key, _) in &metadata {
        if key.trim().is_empty() || key.trim() != key {
            return Err(Error::InvalidInput(
                "reviewed derivation metadata keys must be non-empty without surrounding whitespace"
                    .to_owned(),
            ));
        }
        if key.starts_with("anamnesis:") {
            return Err(Error::InvalidInput(format!(
                "reviewed derivation metadata key {key:?} is engine-owned"
            )));
        }
        if !seen.insert(key.as_str()) {
            return Err(Error::InvalidInput(format!(
                "reviewed derivation metadata key {key:?} is duplicated"
            )));
        }
    }
    Ok(metadata)
}

fn normalize_reviewed_derivation_field(
    label: &str,
    value: &str,
    max_chars: usize,
    allow_line_breaks: bool,
) -> Result<String, Error> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(Error::InvalidInput(format!(
            "reviewed derivation {label} must not be empty"
        )));
    }
    if normalized.chars().count() > max_chars {
        return Err(Error::InvalidInput(format!(
            "reviewed derivation {label} exceeds {max_chars} characters"
        )));
    }
    let has_forbidden_control = normalized.chars().any(|character| {
        character.is_control() && !(allow_line_breaks && matches!(character, '\n' | '\r' | '\t'))
    });
    if has_forbidden_control {
        return Err(Error::InvalidInput(format!(
            "reviewed derivation {label} contains a control character"
        )));
    }
    Ok(normalized.to_owned())
}

#[allow(clippy::too_many_arguments)]
fn atomic_fact_matches_admission(
    fact: &AtomicFact,
    content: &str,
    source_node_ids: &[NodeId],
    entity_tags: &[String],
    source_session_id: &str,
    scope: &ScopePath,
    observed_at: Timestamp,
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
    metadata: &HashMap<String, String>,
) -> bool {
    fact.content == content
        && fact.source_node_ids == source_node_ids
        && fact.entity_tags == entity_tags
        && fact.source_session_id == source_session_id
        && &fact.scope == scope
        && fact.observed_at == observed_at
        && fact.valid_from == valid_from
        && fact.valid_until == valid_until
        && &fact.metadata == metadata
}

fn ensure_atomic_fact_has_live_sources<S: StorageAdapter>(
    storage: &S,
    fact: &AtomicFact,
    reviewed_at: Timestamp,
) -> Result<(), Error> {
    if fact
        .metadata
        .get("retracted")
        .is_some_and(|value| value == "true")
        || fact.observed_at > reviewed_at
        || !crate::graph::valid_at(fact.valid_from, fact.valid_until, reviewed_at)
    {
        return Err(Error::InvalidInput(format!(
            "atomic fact {} was not eligible at relation review time",
            fact.id.0
        )));
    }
    for source_id in &fact.source_node_ids {
        let source = match storage.get_node(*source_id) {
            Ok(source) => source,
            Err(Error::NodeNotFound(_)) => continue,
            Err(error) => return Err(error),
        };
        if source.node_type == KnowledgeType::Episodic
            && source.origin.session_id == fact.source_session_id
            && source.origin.scope == fact.scope
            && storage.atomic_fact_source_is_current(fact, source)?
            && source.created_at <= reviewed_at
            && crate::graph::valid_at(source.valid_from, source.valid_until, reviewed_at)
            && !source
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
        {
            return Ok(());
        }
    }
    Err(Error::InvalidInput(format!(
        "atomic fact {} has no live raw source at relation review time",
        fact.id.0
    )))
}

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

/// Stable entity tags derived from the source session and speaker.
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

/// Derive a bounded node name from the first 50 content characters.
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

    struct OrderReranker;

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

    impl RerankingProvider for OrderReranker {
        fn rerank(&self, _query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error> {
            Ok(documents
                .iter()
                .enumerate()
                .map(|(index, _)| RerankScore {
                    index,
                    score: (documents.len() - index) as f64,
                })
                .collect())
        }

        fn model_name(&self) -> &str {
            "order-test"
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

    #[test]
    fn complex_search_batches_bounded_dense_query_variants() {
        let (mut m, calls) = recording_mem();
        m.add(
            "s",
            "user",
            "Nimbus and the worker discussed a shared retry policy",
            t(1),
        )
        .unwrap();
        m.flush_all().unwrap();
        calls.lock().unwrap().clear();

        let result = m
            .search_result_at_with_diagnostics(
                "What retry advice could Nimbus share with the worker?",
                20,
                t(100),
                &SearchTuning::default(),
                &SearchDiagnostics::with_readout_trace_limit(50),
            )
            .unwrap();

        let query_calls: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| *kind == "query")
            .map(|(_, text)| text.clone())
            .collect();
        assert_eq!(
            query_calls,
            [
                "What retry advice could Nimbus share with the worker?",
                "retry advice Nimbus share with the worker",
                "retry advice share with worker",
                "Nimbus"
            ]
        );
        assert!(
            result
                .trace
                .strategies_used
                .iter()
                .any(|strategy| strategy == "dense_query_union:4")
        );
    }

    #[test]
    fn direct_and_temporal_search_keep_one_dense_query() {
        let (mut m, calls) = recording_mem();
        m.add("s", "user", "Nimbus deployed Atlas in staging", t(1))
            .unwrap();
        m.flush_all().unwrap();

        for query in [
            "Where does Nimbus deploy Atlas?",
            "When did Nimbus deploy Atlas?",
        ] {
            calls.lock().unwrap().clear();
            let result = m
                .search_result_at_with_diagnostics(
                    query,
                    20,
                    t(100),
                    &SearchTuning::default(),
                    &SearchDiagnostics::with_readout_trace_limit(50),
                )
                .unwrap();
            let query_calls: Vec<_> = calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(kind, _)| *kind == "query")
                .map(|(_, text)| text.clone())
                .collect();
            assert_eq!(query_calls, [query]);
            assert!(
                !result
                    .trace
                    .strategies_used
                    .iter()
                    .any(|strategy| strategy.starts_with("dense_query_union:"))
            );
        }
    }

    fn t(ms: u64) -> Timestamp {
        Timestamp(ms)
    }

    fn single_readout_result(node_id: NodeId) -> SearchResult {
        readout_result(&[node_id])
    }

    fn readout_result(node_ids: &[NodeId]) -> SearchResult {
        SearchResult {
            package: ContextPackage::empty(),
            trace: crate::query::SearchTrace {
                packaging_mode: Some(crate::query::PackagingMode::Balanced),
                readout: node_ids
                    .iter()
                    .enumerate()
                    .map(|(index, node_id)| crate::query::ReadoutCandidate {
                        node_id: *node_id,
                        score: 1.0 - index as f64 * 0.01,
                        activation: 0.8,
                        phi: 0.4,
                        embedding_cosine: 0.7,
                        salience: 0.6,
                        impedance: 0.1,
                        scope_weight: 1.0,
                        trust_weight: 1.0,
                        stress: 0.0,
                    })
                    .collect(),
                ..crate::query::SearchTrace::default()
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_memory_event_boundary_fact(
        memory: &mut Memory<SqliteStorage>,
        source_node_id: NodeId,
        subject: &str,
        predicate: &str,
        object: &str,
        evidence_object: &str,
        valid_from: Timestamp,
    ) -> AtomicFactId {
        let storage = memory.engine().graph().storage();
        let source = storage
            .get_node(source_node_id)
            .expect("event-boundary source");
        let source_content = source.content.clone();
        let source_session_id = source.origin.session_id.clone();
        let scope = source.origin.scope.clone();
        let observed_at = source.created_at;
        let incarnation = storage
            .atomic_source_incarnation(source)
            .expect("event-boundary source incarnation");
        let storage = memory.engine_mut().graph_mut().storage_mut();
        let fact_id = storage
            .next_atomic_fact_id()
            .expect("event-boundary fact id");
        let metadata = HashMap::from([
            (
                REVIEWED_DERIVATION_SCHEMA_KEY.to_owned(),
                "reviewed-routing-v1".to_owned(),
            ),
            (
                REVIEWED_DERIVATION_SUBJECT_KEY.to_owned(),
                subject.to_owned(),
            ),
            (
                REVIEWED_DERIVATION_PREDICATE_KEY.to_owned(),
                predicate.to_owned(),
            ),
            (REVIEWED_DERIVATION_OBJECT_KEY.to_owned(), object.to_owned()),
            (
                REVIEWED_DERIVATION_POLARITY_KEY.to_owned(),
                "affirmed".to_owned(),
            ),
            (
                REVIEWED_DERIVATION_MODALITY_KEY.to_owned(),
                "observed".to_owned(),
            ),
            (
                REVIEWED_DERIVATION_REVIEWED_BY_KEY.to_owned(),
                "memory-integration-reviewer".to_owned(),
            ),
            (
                REVIEWED_DERIVATION_REVIEW_PROFILE_KEY.to_owned(),
                "memory-integration:v1".to_owned(),
            ),
            (
                REVIEWED_DERIVATION_REVIEWED_AT_KEY.to_owned(),
                observed_at.0.to_string(),
            ),
            (
                REVIEWED_DERIVATION_IDEMPOTENCY_KEY.to_owned(),
                format!("memory-event-boundary:{}", fact_id.0),
            ),
            (
                "anamnesis:evidence-object".to_owned(),
                evidence_object.to_owned(),
            ),
            (
                "anamnesis:evidence-source-node-id".to_owned(),
                source_node_id.0.to_string(),
            ),
            ("anamnesis:evidence-span-start".to_owned(), "0".to_owned()),
            (
                "anamnesis:evidence-span-end".to_owned(),
                source_content.len().to_string(),
            ),
            (
                crate::storage::atomic_source_incarnation_key(source_node_id),
                incarnation,
            ),
        ]);
        storage
            .set_atomic_fact(AtomicFact {
                id: fact_id,
                content: format!("{subject} {predicate} {object}"),
                embedding: vec![0.0; 8],
                source_node_ids: vec![source_node_id],
                entity_tags: vec![subject.to_owned()],
                source_session_id,
                scope,
                observed_at,
                valid_from: Some(valid_from),
                valid_until: None,
                metadata,
            })
            .expect("event-boundary atomic fact");
        fact_id
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
        assert!(defaults.adaptive_delivery);
        assert!(
            !RerankedRecallOptions::new(20)
                .with_adaptive_delivery(false)
                .adaptive_delivery
        );

        let mut m = mem();
        m.add_note("The archive key is amber", t(1)).unwrap();
        m.add_note("The deployment key is cobalt", t(2)).unwrap();

        let query = "Which deployment key should I use?";
        let output = m
            .search_reranked_at(
                query,
                &KeywordReranker,
                RerankedRecallOptions::new(4).with_candidate_limit(20),
                t(100),
            )
            .unwrap();

        assert_eq!(output.plan, RecallPlan::infer(query));
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
    fn planned_production_recall_uses_one_structured_plan_end_to_end() {
        let (mut m, calls) = recording_mem();
        m.add_note("Alice writes essays and poetry", t(1)).unwrap();
        calls.lock().unwrap().clear();

        let query = "What writing does Alice do?";
        assert_eq!(RecallPlan::infer(query).answer_shape, AnswerShape::Fact);
        let plan = RecallPlan::infer_with_answer_shape(query, AnswerShape::Collection);
        let output = m
            .search_reranked_for_plan_at(
                &plan,
                &OrderReranker,
                RerankedRecallOptions::new(8).with_candidate_limit(20),
                t(100),
            )
            .unwrap();

        let query_calls: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| *kind == "query")
            .map(|(_, text)| text.clone())
            .collect();
        assert_eq!(query_calls.first().map(String::as_str), Some(query));
        assert!(
            query_calls.len() > 1,
            "the collection plan must control source-search expansion: {query_calls:?}"
        );
        assert_eq!(output.plan, plan);
        assert!(!output.recall.hits.is_empty());

        let readout = m.readout_for_plan(&output.plan, &output.recall).unwrap();
        assert_eq!(readout.plan, plan);
        assert_eq!(
            readout.reader_contract.plan.answer_shape,
            AnswerShape::Collection
        );
    }

    #[test]
    fn trend_history_survives_the_fake_reranker_and_plan_aware_packaging() {
        let mut m = mem();
        let source = m
            .add_source_fragment(
                SourceFragmentInput::new(
                    "Alice completed the cobalt project before it was archived",
                    Origin {
                        peer_id: PeerId(0),
                        source_kind: SourceKind::HumanInput,
                        session_id: "historical-project".to_owned(),
                        scope: ScopePath::universal(),
                        confidence: 0.95,
                    },
                    t(1),
                )
                .with_validity(Some(t(1)), Some(t(50))),
            )
            .expect("historical raw source");
        m.add_atomic_fact(
            AtomicFactInput::new("Alice completed the cobalt project", vec![source])
                .with_entity_tags(vec!["Alice".to_owned(), "cobalt project".to_owned()])
                .with_validity(Some(t(1)), Some(t(50))),
        )
        .expect("historical atomic fact");

        let query = "How did Alice's cobalt project status change over time?";
        let trend_plan = RecallPlan::infer_with_answer_shape(query, AnswerShape::Fact)
            .with_temporal_constraint(TemporalConstraint::trend());
        let trend = m
            .search_reranked_for_plan_at(
                &trend_plan,
                &OrderReranker,
                RerankedRecallOptions::new(4).with_candidate_limit(20),
                t(100),
            )
            .expect("historical Trend recall");
        assert!(
            trend
                .ranking
                .iter()
                .any(|candidate| candidate.node_id == source),
            "the expired raw source must reach the local reranker"
        );
        assert!(
            trend.recall.hits.iter().any(|hit| hit.node_id == source),
            "the expired raw source must survive post-rerank authority checks and packaging"
        );
        assert!(
            trend
                .recall
                .package
                .memories
                .iter()
                .any(|fragment| fragment.node_id == source)
        );

        let current_plan = RecallPlan::infer_with_answer_shape(query, AnswerShape::Collection)
            .with_temporal_constraint(TemporalConstraint::none());
        let current = m
            .search_reranked_for_plan_at(
                &current_plan,
                &OrderReranker,
                RerankedRecallOptions::new(4).with_candidate_limit(20),
                t(100),
            )
            .expect("current recall");
        assert!(
            current.recall.hits.iter().all(|hit| hit.node_id != source),
            "the public current-validity route must not inherit Trend history semantics"
        );
    }

    #[test]
    fn production_rerank_keeps_semantic_document_and_binds_raw_source() {
        let mut m = mem();
        let receipt = m.add_note("Alice repaired the blue bicycle", t(1)).unwrap();
        let semantic = receipt.finalized_semantic.unwrap();
        let result = single_readout_result(semantic);

        let output = m
            .rerank_search_result_at(
                "What did Alice repair?",
                &result,
                &OrderReranker,
                RerankedRecallOptions::new(4).with_adaptive_delivery(false),
                t(100),
            )
            .unwrap();

        let packaged_ids: HashSet<_> = output
            .recall
            .package
            .identity
            .iter()
            .chain(output.recall.package.knowledge.iter())
            .chain(output.recall.package.memories.iter())
            .map(|fragment| fragment.node_id)
            .collect();
        let committed_ids: HashSet<_> = output
            .recall
            .package
            .commit_trace
            .accessed
            .iter()
            .map(|site| site.node_id)
            .collect();

        assert_eq!(packaged_ids, HashSet::from([semantic]));
        assert_eq!(committed_ids, packaged_ids);
        assert_eq!(output.recall.hits.len(), 1);
        assert_eq!(output.recall.hits[0].node_id, semantic);
        assert!(output.recall.hits[0].text.contains("blue bicycle"));
        assert_eq!(output.recall.package.total_fragments(), 1);
        assert!(output.recall.package.token_usage.used <= output.recall.package.token_usage.total);
        let rendered = m
            .render_context_for_with(
                "What did Alice repair?",
                &output.recall,
                ContextRenderOptions::with_style(ContextRenderStyle::Evidence),
            )
            .unwrap();
        assert!(rendered.contains("Alice repaired the blue bicycle"));
        assert!(rendered.contains(&format!("turn-source=node:{}", receipt.episodic.0)));
        assert!(rendered.contains(&format!("source=node:{} observed", receipt.episodic.0)));

        let report = m.used(output.recall).unwrap();
        assert_eq!(report.sites_accessed, 1);
    }

    #[test]
    fn production_rerank_keeps_multi_line_semantic_window_with_fixed_delivery_width() {
        let mut m = mem();
        let first = m
            .add(
                "repair-session",
                "alice",
                "I repaired the blue bicycle",
                t(1),
            )
            .unwrap();
        let second = m
            .add(
                "repair-session",
                "bob",
                "I repaired the green bicycle",
                t(2),
            )
            .unwrap();
        let semantic = second.finalized_semantic.unwrap();
        let result = single_readout_result(semantic);

        let output = m
            .rerank_search_result_at(
                "What did Alice and Bob repair?",
                &result,
                &OrderReranker,
                RerankedRecallOptions::new(4).with_adaptive_delivery(false),
                t(100),
            )
            .unwrap();
        let packaged_ids: HashSet<_> = output
            .recall
            .package
            .knowledge
            .iter()
            .map(|fragment| fragment.node_id)
            .collect();
        let committed_ids: HashSet<_> = output
            .recall
            .package
            .commit_trace
            .accessed
            .iter()
            .map(|site| site.node_id)
            .collect();
        assert_eq!(output.recall.hits.len(), 1);
        assert_eq!(output.recall.hits[0].node_id, semantic);
        assert_eq!(packaged_ids, HashSet::from([semantic]));
        assert_eq!(committed_ids, packaged_ids);
        assert_eq!(output.recall.package.total_fragments(), 1);
        assert!(output.recall.package.token_usage.used <= output.recall.package.token_usage.total);
        let rendered = m
            .render_context_for_with(
                "What did Alice and Bob repair?",
                &output.recall,
                ContextRenderOptions::with_style(ContextRenderStyle::Evidence),
            )
            .unwrap();
        for source_id in [first.episodic, second.episodic] {
            assert!(
                rendered.contains(&format!("turn-source=node:{}", source_id.0)),
                "missing source-bound semantic line for {source_id:?}:\n{rendered}"
            );
            assert!(
                rendered.contains(&format!("source=node:{} observed", source_id.0)),
                "missing focused raw line for {source_id:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn prepared_rerank_filters_ineligible_sources_before_exposing_text() {
        let mut memory = mem();
        let accepted_scope = ScopePath::new("workspace/accepted").unwrap();
        let other_scope = ScopePath::new("workspace/other").unwrap();
        let origin = |session: &str, scope: ScopePath| Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::HumanInput,
            session_id: session.to_owned(),
            scope,
            confidence: 0.95,
        };
        let (eligible, retracted, expired, future, malformed, wrong_scope) = {
            let mut add_source = |label: &str, at: Timestamp, scope: ScopePath| {
                memory
                    .add_source_fragment(SourceFragmentInput::new(label, origin(label, scope), at))
                    .unwrap()
            };
            (
                add_source("eligible record", t(10), accepted_scope.clone()),
                add_source("retracted record", t(11), accepted_scope.clone()),
                add_source("expired record", t(12), accepted_scope.clone()),
                add_source("future record", t(200), accepted_scope.clone()),
                add_source("malformed record", t(13), accepted_scope.clone()),
                add_source("other scope record", t(14), other_scope),
            )
        };

        memory.set_metadata(retracted, "retracted", "true").unwrap();
        memory
            .set_validity_window(expired, Some(t(1)), Some(t(50)))
            .unwrap();
        let mut malformed_node = memory.engine().graph().get_node(malformed).unwrap().clone();
        malformed_node.valid_from = Some(t(80));
        malformed_node.valid_until = Some(t(70));
        memory
            .engine_mut()
            .graph_mut()
            .storage_mut()
            .set_node(malformed_node)
            .unwrap();

        let result =
            readout_result(&[eligible, retracted, expired, future, malformed, wrong_scope]);
        let plan = RecallPlan::infer_with_answer_shape(
            "What is the currently eligible record?",
            AnswerShape::Fact,
        );
        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                accepted_scope.clone(),
                RerankedRecallOptions::new(6)
                    .with_candidate_limit(6)
                    .with_scope(accepted_scope),
                t(100),
            )
            .unwrap();
        let texts: Vec<_> = prepared.rerank_texts().collect();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("eligible record"));
        let prepared_documents = prepared.documents().to_vec();

        let completed = memory
            .complete_prepared_rerank(prepared, &[RerankScore::new(0, 1.0)])
            .unwrap();
        assert_eq!(completed.rerank_documents, prepared_documents);
        assert_eq!(completed.recall.hits.len(), 1);
        assert_eq!(completed.recall.hits[0].node_id, eligible);
    }

    #[test]
    fn prepared_rerank_rejects_node_id_reuse_between_prepare_and_complete() {
        let mut memory = mem();
        let source = memory
            .add_source_fragment(SourceFragmentInput::new(
                "original ledger entry",
                Origin {
                    peer_id: PeerId(0),
                    source_kind: SourceKind::HumanInput,
                    session_id: "ledger-original".to_owned(),
                    scope: ScopePath::universal(),
                    confidence: 0.95,
                },
                t(10),
            ))
            .unwrap();
        let result = single_readout_result(source);
        let plan =
            RecallPlan::infer_with_answer_shape("What is the ledger entry?", AnswerShape::Fact);
        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(1)
                    .with_candidate_limit(1)
                    .with_adaptive_delivery(false),
                t(100),
            )
            .unwrap();
        assert_eq!(prepared.document_count(), 1);
        assert!(
            prepared
                .rerank_texts()
                .next()
                .is_some_and(|text| text.contains("original ledger entry"))
        );

        let mut replacement = memory.engine().graph().get_node(source).unwrap().clone();
        memory.engine_mut().graph_mut().remove_node(source).unwrap();
        let replacement_id = memory.engine_mut().graph_mut().next_node_id();
        assert_eq!(replacement_id, source);
        replacement.id = replacement_id;
        replacement.name = "replacement ledger entry".to_owned();
        replacement.content = "replacement ledger entry".to_owned();
        replacement.origin.session_id = "ledger-replacement".to_owned();
        memory
            .engine_mut()
            .graph_mut()
            .add_node(replacement)
            .unwrap();

        let completed = memory
            .complete_prepared_rerank(prepared, &[RerankScore::new(0, 1.0)])
            .unwrap();
        assert!(completed.ranking.is_empty());
        assert!(completed.recall.hits.is_empty());
        assert_eq!(completed.recall.package.total_fragments(), 0);
    }

    #[test]
    fn prepared_rerank_validates_external_document_scores() {
        let mut memory = mem();
        let source = memory
            .add_source_fragment(SourceFragmentInput::new(
                "bounded scoring record",
                Origin {
                    peer_id: PeerId(0),
                    source_kind: SourceKind::HumanInput,
                    session_id: "bounded-scoring".to_owned(),
                    scope: ScopePath::universal(),
                    confidence: 0.95,
                },
                t(10),
            ))
            .unwrap();
        let plan = RecallPlan::infer("What is the bounded scoring record?");
        let prepare = || {
            memory
                .prepare_rerank_from_search_result_for_plan_at(
                    &plan,
                    &single_readout_result(source),
                    ScopePath::universal(),
                    RerankedRecallOptions::new(1).with_candidate_limit(1),
                    t(100),
                )
                .unwrap()
        };

        assert!(matches!(
            memory.complete_prepared_rerank(prepare(), &[]),
            Err(Error::InvalidInput(message)) if message.contains("no scores")
        ));
        assert!(matches!(
            memory.complete_prepared_rerank(
                prepare(),
                &[RerankScore::new(0, 2.0), RerankScore::new(0, 1.0)],
            ),
            Err(Error::InvalidInput(message)) if message.contains("duplicate")
        ));
        assert!(matches!(
            memory.complete_prepared_rerank(
                prepare(),
                &[RerankScore::new(1, 1.0)],
            ),
            Err(Error::InvalidInput(message)) if message.contains("out-of-bounds")
        ));
        assert!(matches!(
            memory.complete_prepared_rerank(prepare(), &[RerankScore::new(0, f64::NAN)]),
            Err(Error::NonFinite(_))
        ));
    }

    #[test]
    fn prepared_rerank_fails_closed_when_source_package_has_no_documents() {
        let mut memory = mem();
        memory
            .add_source_fragment(SourceFragmentInput::new(
                "package-only confidential record",
                Origin {
                    peer_id: PeerId(0),
                    source_kind: SourceKind::HumanInput,
                    session_id: "package-only".to_owned(),
                    scope: ScopePath::universal(),
                    confidence: 0.95,
                },
                t(10),
            ))
            .unwrap();
        let plan = RecallPlan::infer("What is the package-only confidential record?");
        let mut result = memory
            .search_result_at_with(&plan.query, 4, t(100), &SearchTuning::default())
            .unwrap();
        assert!(result.package.total_fragments() > 0);
        result.trace.readout.clear();

        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(4),
                t(100),
            )
            .unwrap();
        assert_eq!(prepared.document_count(), 0);

        let completed = memory.complete_prepared_rerank(prepared, &[]).unwrap();
        assert!(completed.ranking.is_empty());
        assert!(completed.recall.hits.is_empty());
        assert_eq!(completed.recall.package.total_fragments(), 0);
        assert!(completed.recall.package.commit_trace.accessed.is_empty());
    }

    #[test]
    fn prepared_rerank_is_bound_to_its_originating_memory() {
        let mut first = mem();
        let mut second = mem();
        let origin = Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::HumanInput,
            session_id: "receipt-origin".to_owned(),
            scope: ScopePath::universal(),
            confidence: 0.95,
        };
        for memory in [&mut first, &mut second] {
            memory
                .add_source_fragment(SourceFragmentInput::new(
                    "origin-bound prepared evidence",
                    origin.clone(),
                    t(10),
                ))
                .unwrap();
        }
        let plan = RecallPlan::infer("What is the origin-bound prepared evidence?");
        let prepared = first
            .prepare_rerank_for_plan_at(
                &plan,
                RerankedRecallOptions::new(1)
                    .with_candidate_limit(1)
                    .with_search_limit(1),
                t(100),
            )
            .unwrap();
        assert_eq!(prepared.document_count(), 1);
        let redacted = format!("{prepared:?}");
        assert!(!redacted.contains("origin-bound prepared evidence"));

        let error = second
            .complete_prepared_rerank(prepared, &[RerankScore::new(0, 1.0)])
            .unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidInput(message) if message.contains("different Memory instance")
        ));
    }

    #[test]
    fn prepared_rerank_equal_scores_use_document_index_as_tie_breaker() {
        let mut memory = mem();
        for (session, content) in [
            ("tie-first", "first independent prepared record"),
            ("tie-second", "second independent prepared record"),
        ] {
            memory
                .add_source_fragment(SourceFragmentInput::new(
                    content,
                    Origin {
                        peer_id: PeerId(0),
                        source_kind: SourceKind::HumanInput,
                        session_id: session.to_owned(),
                        scope: ScopePath::universal(),
                        confidence: 0.95,
                    },
                    t(10),
                ))
                .unwrap();
        }
        let plan = RecallPlan::infer("Which independent prepared records exist?");
        let prepared = memory
            .prepare_rerank_for_plan_at(
                &plan,
                RerankedRecallOptions::new(2)
                    .with_candidate_limit(2)
                    .with_search_limit(2)
                    .with_adaptive_delivery(false),
                t(100),
            )
            .unwrap();
        assert_eq!(prepared.document_count(), 2);
        let expected = [prepared.evidence[0].node_id, prepared.evidence[1].node_id];

        let completed = memory
            .complete_prepared_rerank(
                prepared,
                &[RerankScore::new(1, 1.0), RerankScore::new(0, 1.0)],
            )
            .unwrap();
        assert_eq!(
            completed
                .ranking
                .iter()
                .map(|candidate| candidate.node_id)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn event_boundary_group_survives_bound_hydration_and_unbound_score_normalization() {
        let mut memory = mem();
        let semantic_target_receipt = memory
            .add(
                "event-semantic-target",
                "Morgan",
                "Morgan struggled to find accessible housing.",
                t(10),
            )
            .expect("semantic target source");
        let semantic_target_source = semantic_target_receipt.episodic;
        let semantic_target = memory
            .add(
                "event-semantic-target",
                "assistant",
                "The housing search was recorded as a difficulty.",
                t(11),
            )
            .expect("semantic target window")
            .finalized_semantic
            .expect("semantic target representative");
        let raw_source = |memory: &mut Memory<SqliteStorage>, content: &str, session: &str, at| {
            memory
                .add_source_fragment(SourceFragmentInput::new(
                    content,
                    Origin {
                        peer_id: PeerId(0),
                        source_kind: SourceKind::HumanInput,
                        session_id: session.to_owned(),
                        scope: ScopePath::universal(),
                        confidence: 0.95,
                    },
                    at,
                ))
                .expect("event-boundary raw source")
        };
        let second_target_source = raw_source(
            &mut memory,
            "Morgan found choosing a veterinarian difficult.",
            "event-target-two",
            t(20),
        );
        let third_target_source = raw_source(
            &mut memory,
            "Morgan had difficulty arranging accessible transport.",
            "event-target-three",
            t(25),
        );
        let anchor_source = raw_source(&mut memory, "Morgan adopted Pip.", "event-anchor", t(30));
        let after_target_source = raw_source(
            &mut memory,
            "After adopting Pip, Morgan had difficulty with training.",
            "event-target-after",
            t(35),
        );
        seed_memory_event_boundary_fact(
            &mut memory,
            semantic_target_source,
            "Morgan",
            "struggled",
            "to find accessible housing",
            "accessible housing",
            t(10),
        );
        seed_memory_event_boundary_fact(
            &mut memory,
            second_target_source,
            "Morgan",
            "found difficult",
            "choosing a veterinarian",
            "veterinarian",
            t(20),
        );
        seed_memory_event_boundary_fact(
            &mut memory,
            third_target_source,
            "Morgan",
            "had difficulty",
            "arranging accessible transport",
            "accessible transport",
            t(25),
        );
        seed_memory_event_boundary_fact(
            &mut memory,
            anchor_source,
            "Morgan",
            "adopted",
            "Pip",
            "Pip",
            t(30),
        );
        seed_memory_event_boundary_fact(
            &mut memory,
            after_target_source,
            "Morgan",
            "had difficulty",
            "with training",
            "training",
            t(35),
        );

        let mut ordinary = Vec::new();
        for index in 0..19 {
            ordinary.push(raw_source(
                &mut memory,
                &format!("Independent event-boundary control record {index}."),
                &format!("event-control-{index}"),
                t(40 + index),
            ));
        }
        memory.flush_all().expect("flush integration fixture");
        assert_eq!(
            memory
                .engine()
                .graph()
                .get_node(semantic_target)
                .expect("semantic representative")
                .node_type,
            KnowledgeType::Semantic
        );
        assert!(
            readout::canonical_sources(memory.engine().graph().storage(), semantic_target)
                .expect("semantic representative sources")
                .contains(&semantic_target_source)
        );

        let plan = RecallPlan::infer("What problems did Morgan face before adopting Pip?");
        let as_of = t(100);
        let searched = memory
            .search_result_at_with_diagnostics(
                &plan.query,
                20,
                as_of,
                &SearchTuning::default(),
                &SearchDiagnostics::with_readout_trace_limit(DEFAULT_RERANK_CANDIDATE_LIMIT),
            )
            .expect("production event-boundary search trace");
        assert!(
            searched
                .trace
                .strategies_used
                .iter()
                .any(|strategy| { strategy.starts_with("event_boundary_source_group:v1:") })
        );
        assert!(
            !memory
                .rerank_documents_for_plan_at_internal(
                    &plan,
                    &searched,
                    DEFAULT_RERANK_CANDIDATE_LIMIT,
                    &ScopePath::universal(),
                    as_of,
                )
                .expect("production trace document compilation")
                .is_empty()
        );

        let mut node_ids = vec![anchor_source];
        node_ids.extend(ordinary);
        node_ids.extend([
            semantic_target,
            second_target_source,
            third_target_source,
            after_target_source,
        ]);
        let mut result = readout_result(&node_ids);
        let group = readout::canonical_event_boundary_source_group(
            memory.engine().graph().storage(),
            &plan,
            as_of,
            &ScopePath::universal(),
        )
        .expect("canonical event group")
        .expect("complete event group");
        let encoded = readout::encode_represented_event_boundary_source_group(
            memory.engine().graph().storage(),
            &group,
            &result.trace.readout,
            DEFAULT_RERANK_CANDIDATE_LIMIT,
        )
        .expect("represented event group")
        .expect("event group trace");
        result
            .trace
            .strategies_used
            .push("event_boundary_source_group_routing".to_owned());
        result.trace.strategies_used.push(encoded);

        let candidate_score = |node_id: NodeId, position: usize| {
            if node_id == anchor_source {
                100.0
            } else if node_id == semantic_target {
                3.0
            } else if node_id == second_target_source {
                2.0
            } else if node_id == third_target_source {
                1.0
            } else {
                50.0 - position as f64 * 0.01
            }
        };
        let selected_sources = |recall: &Recall| {
            let mut sources = HashSet::new();
            for hit in &recall.hits {
                sources.extend(
                    readout::canonical_sources(memory.engine().graph().storage(), hit.node_id)
                        .expect("selected canonical sources"),
                );
            }
            sources
        };

        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(20)
                    .with_candidate_limit(node_ids.len())
                    .with_adaptive_delivery(false),
                as_of,
            )
            .expect("bound event-group documents");
        assert!(prepared.documents().iter().any(|document| {
            document.node_id == semantic_target
                && document.source_node_ids.contains(&semantic_target_source)
        }));
        let scores = prepared
            .documents()
            .iter()
            .enumerate()
            .map(|(index, document)| {
                RerankScore::new(index, candidate_score(document.node_id, index))
            })
            .collect::<Vec<_>>();
        let bound = memory
            .complete_prepared_rerank(prepared, &scores)
            .expect("bound event-group completion");
        let bound_sources = selected_sources(&bound.recall);
        assert!(bound_sources.contains(&semantic_target_source));
        assert!(bound_sources.contains(&second_target_source));
        assert!(!bound_sources.contains(&third_target_source));
        let receipt = bound
            .event_boundary_evidence()
            .expect("bound completion must retain event roles");
        assert_eq!(receipt.direction(), TemporalBoundaryDirection::Before);
        assert!(receipt.anchor_source_node_ids().contains(&anchor_source));
        assert!(
            receipt
                .target_source_node_ids()
                .contains(&semantic_target_source)
        );
        assert!(
            receipt
                .target_source_node_ids()
                .contains(&second_target_source)
        );
        assert!(
            !receipt
                .target_source_node_ids()
                .contains(&third_target_source)
        );

        let bound_draft = GroundedAnswerDraft::new(
            "accessible housing, veterinarian",
            vec![
                GroundedAnswerItem::new("accessible housing", vec![semantic_target_source])
                    .with_finding_ids(vec!["housing".to_owned()]),
                GroundedAnswerItem::new("veterinarian", vec![second_target_source])
                    .with_finding_ids(vec!["veterinarian".to_owned()]),
            ],
            vec![anchor_source, semantic_target_source, second_target_source],
            false,
        )
        .with_findings(vec![
            GroundedEvidenceFinding::new(
                "anchor",
                "Morgan adopted Pip.",
                vec![anchor_source],
                GroundedFindingDisposition::Premise,
            ),
            GroundedEvidenceFinding::new(
                "housing",
                "Morgan struggled to find accessible housing.",
                vec![semantic_target_source],
                GroundedFindingDisposition::Item,
            )
            .with_answer_value("accessible housing"),
            GroundedEvidenceFinding::new(
                "veterinarian",
                "Morgan found choosing a veterinarian difficult.",
                vec![second_target_source],
                GroundedFindingDisposition::Item,
            )
            .with_answer_value("veterinarian"),
        ])
        .with_reasoning_operator(
            GroundedReasoningOperator::new(GroundedReasoningOperatorKind::CollectionLedger)
                .with_inputs(vec![
                    GroundedOperatorInput::new(
                        GroundedOperatorInputRole::Premise,
                        vec!["anchor".to_owned()],
                    ),
                    GroundedOperatorInput::new(
                        GroundedOperatorInputRole::Item,
                        vec!["housing".to_owned(), "veterinarian".to_owned()],
                    ),
                ])
                .with_output("accessible housing, veterinarian"),
        );
        let bound_readout = memory
            .readout_for_reranked(&bound)
            .expect("bound event-role readout");
        let system_authority = bound_readout
            .system_authority_guidance()
            .expect("closed event authority guidance");
        assert!(system_authority.contains("direction=before"));
        assert!(system_authority.contains(&format!("node:{}", anchor_source.0)));
        assert!(!system_authority.contains(&format!("#{}", anchor_source.0)));
        assert!(!system_authority.contains(&bound.plan.query));
        assert!(!system_authority.contains("Morgan adopted Pip"));
        assert!(!system_authority.contains("accessible housing"));
        assert!(
            bound_readout
                .reader_guidance
                .as_deref()
                .is_some_and(|guidance| guidance.contains(&system_authority))
        );
        assert!(
            bound_readout
                .validate_adjudicated_draft(&bound_draft)
                .is_ok()
        );
        assert_eq!(
            bound_readout.materialize_adjudicated_draft(&bound_draft),
            Ok(Some("accessible housing, veterinarian".to_owned()))
        );
        assert!(
            memory
                .readout_for_plan(&bound.plan, &bound.recall)
                .expect("membership-only compatibility readout")
                .validate_adjudicated_draft(&bound_draft)
                .is_ok()
        );
        let rendered_bound = memory
            .render_context_for_reranked(&bound)
            .expect("render bound event roles");
        assert!(rendered_bound.contains("## EVENT BOUNDARY SOURCE ROLES"));
        assert!(rendered_bound.contains("direction=before"));
        assert!(rendered_bound.contains(&format!("node:{}", anchor_source.0)));
        assert!(!rendered_bound.contains("event_boundary_source_group:v1:"));

        let mut changed_query = bound.clone();
        changed_query.plan.query =
            "Which unrelated records existed before another adoption?".to_owned();
        assert!(changed_query.event_boundary_evidence().is_none());
        let changed_query_readout = memory
            .readout_for_reranked(&changed_query)
            .expect("readout for mutated query surface");
        assert!(changed_query_readout.event_boundary_evidence().is_none());
        assert!(changed_query_readout.system_authority_guidance().is_none());
        let error = changed_query_readout
            .validate_adjudicated_draft(&bound_draft)
            .expect_err("same-direction query mutation must lose receipt authority");
        assert!(
            error
                .failures
                .contains(&GroundedDraftValidationFailure::MissingEventBoundaryEvidence)
        );
        let changed_query_render = memory
            .render_context_for_reranked(&changed_query)
            .expect("render mutated query surface");
        assert!(!changed_query_render.contains("## EVENT BOUNDARY SOURCE ROLES"));
        assert!(!changed_query_render.contains("Event-boundary authority:"));

        let mut changed_ranking = bound.clone();
        changed_ranking.ranking.reverse();
        assert!(changed_ranking.event_boundary_evidence().is_none());

        let mut filtered_package = bound.clone();
        let target_dialogue_block = bound_readout
            .source_attributions
            .iter()
            .find(|attribution| attribution.source_node_id == semantic_target_source)
            .map(|attribution| attribution.dialogue_block_node_id)
            .expect("target source must have one visible dialogue block");
        let original_fragment_count = filtered_package.recall.package.total_fragments();
        for fragments in [
            &mut filtered_package.recall.package.identity,
            &mut filtered_package.recall.package.knowledge,
            &mut filtered_package.recall.package.memories,
        ] {
            fragments.retain(|fragment| fragment.node_id != target_dialogue_block);
        }
        assert!(filtered_package.recall.package.total_fragments() < original_fragment_count);
        assert!(filtered_package.event_boundary_evidence().is_none());
        let filtered_render = memory
            .render_context_for_reranked(&filtered_package)
            .expect("render filtered event surface");
        assert!(!filtered_render.contains("## EVENT BOUNDARY SOURCE ROLES"));
        assert!(!filtered_render.contains("Event-boundary authority:"));

        let role_ids = receipt
            .anchor_source_node_ids()
            .iter()
            .chain(receipt.target_source_node_ids())
            .copied()
            .collect::<HashSet<_>>();
        let role_block_ids = bound_readout
            .source_attributions
            .iter()
            .filter(|attribution| role_ids.contains(&attribution.source_node_id))
            .map(|attribution| attribution.dialogue_block_node_id)
            .collect::<HashSet<_>>();
        let mut role_only_reinjection = bound.clone();
        for fragments in [
            &mut role_only_reinjection.recall.package.identity,
            &mut role_only_reinjection.recall.package.knowledge,
            &mut role_only_reinjection.recall.package.memories,
        ] {
            fragments.retain(|fragment| role_block_ids.contains(&fragment.node_id));
        }
        assert!(
            role_only_reinjection
                .recall
                .package
                .identity
                .iter()
                .chain(&role_only_reinjection.recall.package.knowledge)
                .chain(&role_only_reinjection.recall.package.memories)
                .any(|fragment| role_block_ids.contains(&fragment.node_id))
        );
        assert!(role_only_reinjection.event_boundary_evidence().is_none());
        assert!(
            memory
                .readout_for_reranked(&role_only_reinjection)
                .expect("readout for role-id reinjection")
                .system_authority_guidance()
                .is_none()
        );

        let mut mutated_readout = memory
            .readout_for_reranked(&bound)
            .expect("mutable public readout surface");
        mutated_readout.source_node_ids.reverse();
        assert!(mutated_readout.event_boundary_evidence().is_none());
        assert!(mutated_readout.system_authority_guidance().is_none());
        let error = mutated_readout
            .validate_adjudicated_draft(&bound_draft)
            .expect_err("mutated readout surface must lose receipt authority");
        assert!(
            error
                .failures
                .contains(&GroundedDraftValidationFailure::MissingEventBoundaryEvidence)
        );

        let mut unsorted = result
            .trace
            .readout
            .iter()
            .enumerate()
            .map(|(position, candidate)| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate_score(candidate.node_id, position),
            })
            .collect::<Vec<_>>();
        unsorted.reverse();
        let unbound = memory
            .repackage_reranked_deep_for_plan_at(
                &plan,
                &result,
                &unsorted,
                DeepRecallOptions::new(20),
                as_of,
            )
            .expect("unbound normalized event-group completion");
        let unbound_sources = selected_sources(&unbound);
        assert!(unbound_sources.contains(&semantic_target_source));
        assert!(unbound_sources.contains(&second_target_source));
        assert!(!unbound_sources.contains(&third_target_source));

        let after_plan = RecallPlan::infer_with_answer_shape(
            "What problems did Morgan face after adopting Pip?",
            AnswerShape::Collection,
        );
        let mut after_result = readout_result(&node_ids);
        let after_group = readout::canonical_event_boundary_source_group(
            memory.engine().graph().storage(),
            &after_plan,
            as_of,
            &ScopePath::universal(),
        )
        .expect("canonical after-event group")
        .expect("complete after-event group");
        let after_encoded = readout::encode_represented_event_boundary_source_group(
            memory.engine().graph().storage(),
            &after_group,
            &after_result.trace.readout,
            DEFAULT_RERANK_CANDIDATE_LIMIT,
        )
        .expect("represented after-event group")
        .expect("after-event group trace");
        after_result
            .trace
            .strategies_used
            .push("event_boundary_source_group_routing".to_owned());
        after_result.trace.strategies_used.push(after_encoded);
        let after_prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &after_plan,
                &after_result,
                ScopePath::universal(),
                RerankedRecallOptions::new(20)
                    .with_candidate_limit(node_ids.len())
                    .with_adaptive_delivery(false),
                as_of,
            )
            .expect("bound after-event documents");
        let after_scores = after_prepared
            .documents()
            .iter()
            .enumerate()
            .map(|(index, document)| {
                RerankScore::new(index, candidate_score(document.node_id, index))
            })
            .collect::<Vec<_>>();
        let after_bound = memory
            .complete_prepared_rerank(after_prepared, &after_scores)
            .expect("bound after-event completion");
        let after_receipt = after_bound
            .event_boundary_evidence()
            .expect("after-event receipt");
        assert_eq!(after_receipt.direction(), TemporalBoundaryDirection::After);
        assert!(
            after_receipt
                .target_source_node_ids()
                .contains(&after_target_source)
        );
        let after_draft = GroundedAnswerDraft::new(
            "training",
            vec![
                GroundedAnswerItem::new("training", vec![after_target_source])
                    .with_finding_ids(vec!["training".to_owned()]),
            ],
            vec![anchor_source, after_target_source],
            false,
        )
        .with_findings(vec![
            GroundedEvidenceFinding::new(
                "anchor",
                "Morgan adopted Pip.",
                vec![anchor_source],
                GroundedFindingDisposition::Premise,
            ),
            GroundedEvidenceFinding::new(
                "training",
                "Morgan had difficulty with training.",
                vec![after_target_source],
                GroundedFindingDisposition::Item,
            )
            .with_answer_value("training"),
        ])
        .with_reasoning_operator(
            GroundedReasoningOperator::new(GroundedReasoningOperatorKind::CollectionLedger)
                .with_inputs(vec![
                    GroundedOperatorInput::new(
                        GroundedOperatorInputRole::Premise,
                        vec!["anchor".to_owned()],
                    ),
                    GroundedOperatorInput::new(
                        GroundedOperatorInputRole::Item,
                        vec!["training".to_owned()],
                    ),
                ])
                .with_output("training"),
        );
        assert!(
            memory
                .readout_for_reranked(&after_bound)
                .expect("after-event readout")
                .validate_adjudicated_draft(&after_draft)
                .is_ok()
        );
        assert!(
            memory
                .render_context_for_reranked(&after_bound)
                .expect("render after-event roles")
                .contains("direction=after")
        );

        let invalidated_prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(20)
                    .with_candidate_limit(node_ids.len())
                    .with_adaptive_delivery(false),
                as_of,
            )
            .expect("prepare event group before source invalidation");
        let invalidated_scores = invalidated_prepared
            .documents()
            .iter()
            .enumerate()
            .map(|(index, document)| {
                RerankScore::new(index, candidate_score(document.node_id, index))
            })
            .collect::<Vec<_>>();
        memory
            .set_metadata(anchor_source, "retracted", "true")
            .expect("invalidate prepared boundary anchor");
        let invalidated = memory
            .complete_prepared_rerank(invalidated_prepared, &invalidated_scores)
            .expect("complete after event anchor invalidation");
        assert!(invalidated.event_boundary_evidence().is_none());
        let invalidated_readout = memory
            .readout_for_reranked(&invalidated)
            .expect("readout after anchor invalidation");
        let error = invalidated_readout
            .validate_adjudicated_draft(&bound_draft)
            .expect_err("invalidated preparation cannot retain event authority");
        assert!(
            error
                .failures
                .contains(&GroundedDraftValidationFailure::MissingEventBoundaryEvidence)
        );
        let options = ContextRenderOptions::with_style(ContextRenderStyle::Evidence);
        assert_eq!(
            memory
                .render_context_for_reranked_with(&invalidated, options)
                .expect("render receipt-less rerank"),
            memory
                .render_context_for_plan_with(&invalidated.plan, &invalidated.recall, options,)
                .expect("render membership-only control")
        );
    }

    #[test]
    fn prepared_temporal_fact_witness_survives_candidate_fifty_to_final_twenty() {
        const MAY_8_2023: u64 = 1_683_504_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut memory = mem();
        let mut node_ids = Vec::new();
        for index in 0..24 {
            node_ids.push(
                memory
                    .add_source_fragment(SourceFragmentInput::new(
                        format!("Independent field log {index}."),
                        Origin {
                            peer_id: PeerId(0),
                            source_kind: SourceKind::HumanInput,
                            session_id: format!("field-log-{index}"),
                            scope: ScopePath::universal(),
                            confidence: 0.95,
                        },
                        t(MAY_8_2023 + index),
                    ))
                    .expect("ordinary field log"),
            );
        }
        let witness_source = memory
            .add_source_fragment(SourceFragmentInput::new(
                "Orion pursued a coastal survey.".to_owned(),
                Origin {
                    peer_id: PeerId(0),
                    source_kind: SourceKind::HumanInput,
                    session_id: "orion-field-session".to_owned(),
                    scope: ScopePath::universal(),
                    confidence: 0.95,
                },
                t(MAY_8_2023 + 3_600_000),
            ))
            .expect("temporal witness source");
        let fact_id = seed_memory_event_boundary_fact(
            &mut memory,
            witness_source,
            "Orion",
            "pursued",
            "a coastal survey",
            "coastal survey",
            t(MAY_8_2023),
        );
        node_ids.push(witness_source);
        memory.flush_all().expect("flush temporal witness fixture");

        let plan = RecallPlan::infer_with_answer_shape(
            "What activity was Orion pursuing on 2023-05-08?",
            AnswerShape::Fact,
        );
        let mut result = readout_result(&node_ids);
        let marker = readout::AtomicSourceMarker {
            source_node_id: witness_source,
            kind_priority: 0,
            fact_id: Some(fact_id),
        };
        result
            .trace
            .strategies_used
            .push("atomic_fact_routing".to_owned());
        result.trace.strategies_used.push(
            readout::encode_atomic_source_markers(&[marker])
                .expect("temporal witness marker trace"),
        );

        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(20)
                    .with_candidate_limit(DEFAULT_RERANK_CANDIDATE_LIMIT)
                    .with_adaptive_delivery(false),
                t(MAY_8_2023 + DAY_MS),
            )
            .expect("bound temporal witness documents");
        assert_eq!(prepared.document_count(), 25);
        let scores = prepared
            .documents()
            .iter()
            .enumerate()
            .map(|(index, document)| {
                let score = if document.node_id == witness_source {
                    -100.0
                } else {
                    100.0 - index as f64
                };
                RerankScore::new(index, score)
            })
            .collect::<Vec<_>>();
        let completed = memory
            .complete_prepared_rerank(prepared, &scores)
            .expect("production temporal witness completion");
        let delivered_sources: HashSet<_> = completed
            .recall
            .hits
            .iter()
            .flat_map(|hit| {
                readout::canonical_sources(memory.engine().graph().storage(), hit.node_id)
                    .expect("delivered temporal sources")
            })
            .collect();

        assert_eq!(completed.recall.hits.len(), 20);
        assert!(
            delivered_sources.contains(&witness_source),
            "the revalidated rank-25 witness must survive final20 production selection"
        );
    }

    #[test]
    fn prepared_action_marker_source_survives_multisource_hydration_at_final_cap() {
        let mut memory = mem();
        let setup = memory
            .add(
                "artifact-dialogue",
                "technician",
                "I reviewed the portable device requirements.",
                t(1),
            )
            .expect("action setup")
            .episodic;
        let question_receipt = memory
            .add(
                "artifact-dialogue",
                "reviewer",
                "Which portable device did the technician complete?",
                t(2),
            )
            .expect("action question");
        let question = question_receipt.episodic;
        let semantic = question_receipt
            .finalized_semantic
            .expect("question-ended semantic window");
        memory
            .link_extracted_source(semantic, question)
            .expect("explicit question provenance for multi-source hydration");
        let answer = memory
            .add(
                "artifact-dialogue",
                "technician",
                "The technician built a portable device.",
                t(3),
            )
            .expect("action answer")
            .episodic;
        let fact_id = seed_memory_event_boundary_fact(
            &mut memory,
            answer,
            "the technician",
            "built",
            "a portable device",
            "portable device",
            t(3),
        );

        let mut fillers = Vec::new();
        for index in 0..20 {
            fillers.push(
                memory
                    .add_source_fragment(SourceFragmentInput::new(
                        format!("Independent calibration log {index}."),
                        Origin {
                            peer_id: PeerId(0),
                            source_kind: SourceKind::HumanInput,
                            session_id: format!("calibration-log-{index}"),
                            scope: ScopePath::universal(),
                            confidence: 0.95,
                        },
                        t(10 + index),
                    ))
                    .expect("calibration filler"),
            );
        }
        memory.flush_all().expect("flush hydrated action fixture");

        let mut readout_ids = fillers.clone();
        readout_ids.extend([semantic, answer]);
        let mut result = readout_result(&readout_ids);
        let marker = readout::AtomicSourceMarker {
            source_node_id: answer,
            kind_priority: 4,
            fact_id: Some(fact_id),
        };
        result
            .trace
            .strategies_used
            .push("atomic_fact_routing".to_owned());
        result.trace.strategies_used.push(
            readout::encode_atomic_source_markers(&[marker]).expect("hydrated action marker trace"),
        );
        let plan = RecallPlan::infer("What kinds of devices has the technician tried to build?");
        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(DEFAULT_RERANK_FINAL_LIMIT)
                    .with_candidate_limit(DEFAULT_RERANK_CANDIDATE_LIMIT)
                    .with_adaptive_delivery(false),
                t(100),
            )
            .expect("prepared hydrated action documents");
        let action_document = prepared
            .documents()
            .iter()
            .find(|document| document.source_node_ids.contains(&answer))
            .expect("multi-source action document");
        assert!(action_document.source_node_ids.contains(&setup));
        assert!(action_document.source_node_ids.contains(&question));
        assert!(action_document.source_node_ids.contains(&answer));

        let scores = prepared
            .documents()
            .iter()
            .enumerate()
            .map(|(index, document)| {
                let score = if document.source_node_ids.contains(&answer) {
                    -100.0
                } else {
                    100.0 - index as f64
                };
                RerankScore::new(index, score)
            })
            .collect::<Vec<_>>();
        let completed = memory
            .complete_prepared_rerank(prepared, &scores)
            .expect("hydrated action completion");
        let readout = memory
            .readout_for_reranked(&completed)
            .expect("hydrated action readout");
        let rendered = memory
            .render_context_for_reranked(&completed)
            .expect("render hydrated action evidence");

        assert_eq!(completed.recall.hits.len(), DEFAULT_RERANK_FINAL_LIMIT);
        assert!(readout.source_node_ids.contains(&answer));
        assert!(rendered.contains("built a portable device"));
    }

    #[test]
    fn prepared_rerank_rejects_changed_scoring_only_dialogue_context() {
        let mut memory = mem();
        let question = memory
            .add(
                "migration-session",
                "operator",
                "What motivated the migration change?",
                t(10),
            )
            .unwrap()
            .episodic;
        let answer = memory
            .add(
                "migration-session",
                "maintainer",
                "The migration reduced repeated failures.",
                t(11),
            )
            .unwrap()
            .episodic;
        memory.flush_all().unwrap();

        let result = readout_result(&[question, answer]);
        let plan = RecallPlan::infer_with_answer_shape(
            "What motivated the migration change?",
            AnswerShape::Relationship,
        );
        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &result,
                ScopePath::universal(),
                RerankedRecallOptions::new(2)
                    .with_candidate_limit(2)
                    .with_search_limit(2)
                    .with_adaptive_delivery(false),
                t(100),
            )
            .unwrap();
        let answer_index = prepared
            .rerank_texts()
            .position(|text| {
                text.contains("Immediate same-session question:")
                    && text.contains("migration reduced repeated failures")
            })
            .expect("answer scoring surface must include the immediate question");

        let temporal_edge = memory
            .engine()
            .graph()
            .edges_from(question)
            .iter()
            .copied()
            .find(|edge_id| {
                memory
                    .engine()
                    .graph()
                    .get_edge(*edge_id)
                    .is_ok_and(|edge| edge.edge_type == EdgeType::Temporal && edge.target == answer)
            })
            .expect("question-to-answer temporal edge");
        memory
            .engine_mut()
            .graph_mut()
            .remove_edge(temporal_edge)
            .unwrap();

        let completed = memory
            .complete_prepared_rerank(prepared, &[RerankScore::new(answer_index, 10.0)])
            .unwrap();
        assert!(completed.ranking.is_empty());
        assert!(completed.recall.hits.is_empty());
        assert_eq!(completed.recall.package.total_fragments(), 0);
    }

    #[test]
    fn prepared_action_cue_rejects_stale_modality_and_value_kind_before_score_commit() {
        for (metadata_key, replacement) in [
            (REVIEWED_DERIVATION_MODALITY_KEY, "planned"),
            (REVIEWED_DERIVATION_OBJECT_KEY, "a public website"),
        ] {
            let mut memory = mem();
            let source = memory
                .add_source_fragment(SourceFragmentInput::new(
                    "The technician built a portable device.".to_owned(),
                    Origin {
                        peer_id: PeerId(0),
                        source_kind: SourceKind::HumanInput,
                        session_id: format!("action-cue-{metadata_key}"),
                        scope: ScopePath::universal(),
                        confidence: 0.95,
                    },
                    t(3),
                ))
                .expect("action cue source");
            let fact_id = seed_memory_event_boundary_fact(
                &mut memory,
                source,
                "the technician",
                "built",
                "a portable device",
                "portable device",
                t(3),
            );
            memory.flush_all().expect("flush action cue fixture");

            let mut result = single_readout_result(source);
            let marker = readout::AtomicSourceMarker {
                source_node_id: source,
                kind_priority: 4,
                fact_id: Some(fact_id),
            };
            result
                .trace
                .strategies_used
                .push("atomic_fact_routing".to_owned());
            result.trace.strategies_used.push(
                readout::encode_atomic_source_markers(&[marker]).expect("action cue marker trace"),
            );
            let plan =
                RecallPlan::infer("What kinds of devices has the technician tried to build?");
            let prepared = memory
                .prepare_rerank_from_search_result_for_plan_at(
                    &plan,
                    &result,
                    ScopePath::universal(),
                    RerankedRecallOptions::new(1)
                        .with_candidate_limit(1)
                        .with_search_limit(1)
                        .with_adaptive_delivery(false),
                    t(100),
                )
                .expect("prepared action cue");
            assert_eq!(prepared.document_count(), 1);
            assert!(
                prepared
                    .rerank_texts()
                    .any(|text| text.contains("Fact: the technician built a portable device"))
            );

            let mut stale = memory
                .engine()
                .graph()
                .storage()
                .get_atomic_fact(fact_id)
                .expect("prepared action fact")
                .clone();
            stale
                .metadata
                .insert(metadata_key.to_owned(), replacement.to_owned());
            memory
                .engine_mut()
                .graph_mut()
                .storage_mut()
                .set_atomic_fact(stale)
                .expect("stale action fact updates");

            let completed = memory
                .complete_prepared_rerank(prepared, &[RerankScore::new(0, 10.0)])
                .expect("stale action cue completion");
            assert!(completed.ranking.is_empty(), "metadata key: {metadata_key}");
            assert!(
                completed.recall.hits.is_empty(),
                "metadata key: {metadata_key}"
            );
            assert_eq!(
                completed.recall.package.total_fragments(),
                0,
                "metadata key: {metadata_key}"
            );
        }
    }

    #[test]
    fn prepared_rerank_never_exposes_cross_scope_native_semantic_context() {
        let mut memory = mem();
        let accepted_scope = ScopePath::new("workspace/accepted").unwrap();
        let private_scope = ScopePath::new("workspace/private").unwrap();
        memory
            .add_in_scope(
                "scope-transition",
                "maintainer",
                "accepted release evidence",
                t(10),
                accepted_scope.clone(),
            )
            .unwrap();
        let private_source = memory
            .add_in_scope(
                "scope-transition",
                "reviewer",
                "private unreleased migration detail",
                t(11),
                private_scope,
            )
            .unwrap();
        let semantic = private_source.finalized_semantic.unwrap();
        assert!(
            memory
                .engine()
                .graph()
                .get_node(semantic)
                .unwrap()
                .content
                .contains("private unreleased migration detail")
        );

        for answer_shape in [AnswerShape::Collection, AnswerShape::Fact] {
            let plan = RecallPlan::infer_with_answer_shape(
                "Which release evidence is accepted?",
                answer_shape,
            );
            let prepared = memory
                .prepare_rerank_from_search_result_for_plan_at(
                    &plan,
                    &single_readout_result(semantic),
                    accepted_scope.clone(),
                    RerankedRecallOptions::new(4)
                        .with_candidate_limit(4)
                        .with_scope(accepted_scope.clone()),
                    t(100),
                )
                .unwrap();
            assert_eq!(prepared.document_count(), 0);
            assert!(
                prepared
                    .rerank_texts()
                    .all(|text| !text.contains("private unreleased migration detail"))
            );

            let completed = memory.complete_prepared_rerank(prepared, &[]).unwrap();
            assert!(completed.ranking.is_empty());
            assert!(completed.recall.hits.is_empty());
            assert_eq!(completed.recall.package.total_fragments(), 0);
        }
    }

    #[test]
    fn prepared_rerank_rejects_retracted_native_semantic_neighbor() {
        let mut memory = mem();
        memory
            .add(
                "retraction-window",
                "maintainer",
                "the release report is ready",
                t(10),
            )
            .unwrap();
        let next = memory
            .add(
                "retraction-window",
                "reviewer",
                "the reviewer approved the report",
                t(11),
            )
            .unwrap();
        let semantic = next.finalized_semantic.unwrap();
        let plan = RecallPlan::infer_with_answer_shape(
            "Which report did the reviewer approve?",
            AnswerShape::Collection,
        );
        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &single_readout_result(semantic),
                ScopePath::universal(),
                RerankedRecallOptions::new(4).with_candidate_limit(4),
                t(100),
            )
            .unwrap();
        assert_eq!(prepared.document_count(), 1);
        assert!(
            prepared.evidence[0]
                .scoring_source_node_ids()
                .contains(&next.episodic)
        );
        assert!(
            prepared
                .rerank_texts()
                .any(|text| text.contains("the reviewer approved the report"))
        );

        memory
            .set_metadata(next.episodic, "retracted", "true")
            .unwrap();
        let completed = memory
            .complete_prepared_rerank(prepared, &[RerankScore::new(0, 1.0)])
            .unwrap();
        assert!(completed.ranking.is_empty());
        assert!(completed.recall.hits.is_empty());
        assert_eq!(completed.recall.package.total_fragments(), 0);
    }

    #[test]
    fn prepared_rerank_rejects_changed_native_semantic_neighbor() {
        let mut memory = mem();
        memory
            .add(
                "changed-window",
                "maintainer",
                "the compatibility report is ready",
                t(10),
            )
            .unwrap();
        let next = memory
            .add(
                "changed-window",
                "reviewer",
                "the reviewer accepted the compatibility report",
                t(11),
            )
            .unwrap();
        let semantic = next.finalized_semantic.unwrap();
        let plan = RecallPlan::infer_with_answer_shape(
            "Which compatibility report was accepted?",
            AnswerShape::Collection,
        );
        let prepared = memory
            .prepare_rerank_from_search_result_for_plan_at(
                &plan,
                &single_readout_result(semantic),
                ScopePath::universal(),
                RerankedRecallOptions::new(4).with_candidate_limit(4),
                t(100),
            )
            .unwrap();
        assert_eq!(prepared.document_count(), 1);
        assert!(
            prepared.evidence[0]
                .scoring_source_node_ids()
                .contains(&next.episodic)
        );

        let mut changed = memory
            .engine()
            .graph()
            .get_node(next.episodic)
            .unwrap()
            .clone();
        changed.content = "reviewer: the decision is still pending".to_owned();
        memory
            .engine_mut()
            .graph_mut()
            .storage_mut()
            .set_node(changed)
            .unwrap();

        let completed = memory
            .complete_prepared_rerank(prepared, &[RerankScore::new(0, 1.0)])
            .unwrap();
        assert!(completed.ranking.is_empty());
        assert!(completed.recall.hits.is_empty());
        assert_eq!(completed.recall.package.total_fragments(), 0);
    }

    #[test]
    fn prepared_rerank_requires_complete_nonzero_options() {
        let mut memory = mem();
        let plan = RecallPlan::infer("What is the prepared record?");
        assert!(matches!(
            memory.prepare_rerank_for_plan_at(
                &plan,
                RerankedRecallOptions::new(1).with_search_limit(0),
                t(100),
            ),
            Err(Error::InvalidInput(message)) if message.contains("search limit")
        ));
        assert!(matches!(
            memory.prepare_rerank_for_plan_at(
                &plan,
                RerankedRecallOptions::new(1),
                Timestamp(0),
            ),
            Err(Error::InvalidInput(message)) if message.contains("timestamp")
        ));
    }

    #[test]
    fn bound_evidence_fails_closed_for_stale_retracted_expired_and_scoped_sources() {
        let mut m = mem();
        let stale = m.add_note("stale source", t(1)).unwrap().episodic;
        let stale_document = m
            .rerank_documents("stale source", &single_readout_result(stale), 1)
            .unwrap()
            .pop()
            .unwrap();
        let stale_binding = bind_evidence_document(m.engine().graph().storage(), &stale_document)
            .unwrap()
            .unwrap();
        m.set_metadata(stale, "evidence-revision", "2").unwrap();
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &stale_binding,
                &ScopePath::universal(),
                t(100),
            )
            .unwrap()
        );

        let retracted = m.add_note("retracted source", t(2)).unwrap().episodic;
        let retracted_document = m
            .rerank_documents("retracted source", &single_readout_result(retracted), 1)
            .unwrap()
            .pop()
            .unwrap();
        let retracted_binding =
            bind_evidence_document(m.engine().graph().storage(), &retracted_document)
                .unwrap()
                .unwrap();
        m.set_metadata(retracted, "retracted", "true").unwrap();
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &retracted_binding,
                &ScopePath::universal(),
                t(100),
            )
            .unwrap()
        );

        let expired = m.add_note("expired source", t(3)).unwrap().episodic;
        m.set_validity_window(expired, Some(t(1)), Some(t(50)))
            .unwrap();
        let expired_document = m
            .rerank_documents("expired source", &single_readout_result(expired), 1)
            .unwrap()
            .pop()
            .unwrap();
        let expired_binding =
            bind_evidence_document(m.engine().graph().storage(), &expired_document)
                .unwrap()
                .unwrap();
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &expired_binding,
                &ScopePath::universal(),
                t(100),
            )
            .unwrap()
        );

        let project_a = ScopePath::new("project-a").unwrap();
        let project_b = ScopePath::new("project-b").unwrap();
        let scoped = m
            .add_in_scope(
                "scope-session",
                "alice",
                "scoped source",
                t(4),
                project_a.clone(),
            )
            .unwrap()
            .episodic;
        let scoped_document = m
            .rerank_documents("scoped source", &single_readout_result(scoped), 1)
            .unwrap()
            .pop()
            .unwrap();
        let scoped_binding = bind_evidence_document(m.engine().graph().storage(), &scoped_document)
            .unwrap()
            .unwrap();
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &scoped_binding,
                &project_b,
                t(100),
            )
            .unwrap()
        );

        m.add_in_scope(
            "cross-scope-representative",
            "alice",
            "derived premise",
            t(6),
            project_a.clone(),
        )
        .unwrap();
        let concrete_representative = m
            .add_in_scope(
                "cross-scope-representative",
                "alice",
                "follow-up",
                t(7),
                project_a,
            )
            .unwrap()
            .finalized_semantic
            .unwrap();
        let cross_scope_source = m
            .add_in_scope(
                "cross-scope-source",
                "bob",
                "other project evidence",
                t(8),
                project_b,
            )
            .unwrap()
            .episodic;
        let cross_scope_document = BoundEvidenceDocument {
            representative: bind_evidence_node(
                m.engine().graph().storage(),
                concrete_representative,
            )
            .unwrap()
            .unwrap(),
            sources: vec![
                bind_evidence_node(m.engine().graph().storage(), cross_scope_source)
                    .unwrap()
                    .unwrap(),
            ],
            scoring_sources: Vec::new(),
            representative_text_is_bound: false,
        };
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &cross_scope_document,
                &ScopePath::universal(),
                t(100),
            )
            .unwrap()
        );

        let future = m.add_note("future source", t(200)).unwrap().episodic;
        let future_document = m
            .rerank_documents("future source", &single_readout_result(future), 1)
            .unwrap()
            .pop()
            .unwrap();
        let future_binding = bind_evidence_document(m.engine().graph().storage(), &future_document)
            .unwrap()
            .unwrap();
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &future_binding,
                &ScopePath::universal(),
                t(100),
            )
            .unwrap()
        );

        let semantic = m
            .add_note("semantic is not raw evidence", t(5))
            .unwrap()
            .finalized_semantic
            .unwrap();
        let semantic_binding = bind_evidence_node(m.engine().graph().storage(), semantic)
            .unwrap()
            .unwrap();
        let non_raw_document = BoundEvidenceDocument {
            representative: semantic_binding.clone(),
            sources: vec![semantic_binding],
            scoring_sources: Vec::new(),
            representative_text_is_bound: false,
        };
        assert!(
            !bound_evidence_document_is_eligible(
                m.engine().graph().storage(),
                &non_raw_document,
                &ScopePath::universal(),
                t(100),
            )
            .unwrap()
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
    fn production_reranked_recall_applies_and_can_disable_adaptive_delivery() {
        let mut m = mem();
        for index in 0..30 {
            m.add_note(
                &format!("Archive record {index} is stored in location {index}"),
                t(index + 1),
            )
            .unwrap();
        }

        let adaptive = m
            .search_reranked_at(
                "Where are the archive records stored?",
                &OrderReranker,
                RerankedRecallOptions::new(20),
                t(100),
            )
            .unwrap();
        let fixed = m
            .search_reranked_at(
                "Where are the archive records stored?",
                &OrderReranker,
                RerankedRecallOptions::new(20).with_adaptive_delivery(false),
                t(100),
            )
            .unwrap();

        assert_eq!(adaptive.recall.hits.len(), DEFAULT_SIMPLE_DELIVERY_LIMIT);
        assert!(fixed.recall.hits.len() > adaptive.recall.hits.len());
        assert!(fixed.recall.hits.len() <= 20);
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
    fn inference_rerank_documents_use_a_semantic_representative_with_raw_sources() {
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
            .find(|document| document.source_node_ids.contains(&first.episodic))
            .unwrap();
        let node = m.get(representative.node_id).unwrap();

        assert_eq!(node.node_type, KnowledgeType::Semantic);
        assert_eq!(representative.rerank_text(), node.content);
        for source_node_id in &representative.source_node_ids {
            let source = m.get(*source_node_id).unwrap();
            assert_eq!(source.node_type, KnowledgeType::Episodic);
            assert!(representative.text.contains(&source.content));
        }
        assert!(representative.text.contains("repaired the blue bicycle"));
    }

    #[test]
    fn collection_rerank_documents_use_a_semantic_representative_with_raw_sources() {
        let mut m = mem();
        let first = m
            .add(
                "writing-session",
                "alice",
                "I write poems to relax after difficult days",
                t(1),
            )
            .unwrap();
        m.add(
            "writing-session",
            "bob",
            "That writing habit also helps you process difficult experiences",
            t(2),
        )
        .unwrap();
        m.flush_all().unwrap();

        let query = "What kind of writing does Alice do to relax?";
        assert_eq!(
            RecallPlan::infer(query).answer_shape,
            AnswerShape::Collection
        );
        let result = m
            .search_result_at_with(
                query,
                20,
                t(100),
                &SearchTuning {
                    seed_limit: Some(20),
                    entity_tags: Vec::new(),
                },
            )
            .unwrap();
        let documents = m.rerank_documents(query, &result, 20).unwrap();
        let representative = documents
            .iter()
            .find(|document| document.source_node_ids.contains(&first.episodic))
            .unwrap();
        let node = m.get(representative.node_id).unwrap();

        assert_eq!(node.node_type, KnowledgeType::Semantic);
        assert_eq!(representative.rerank_text(), node.content);
        for source_node_id in &representative.source_node_ids {
            let source = m.get(*source_node_id).unwrap();
            assert_eq!(source.node_type, KnowledgeType::Episodic);
            assert!(representative.text.contains(&source.content));
        }
        assert!(representative.text.contains("write poems to relax"));
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
    fn atomic_fact_embedding_surface_is_used_but_never_persisted() {
        let (mut memory, calls) = recording_mem();
        let source = memory
            .add(
                "session",
                "Alice",
                "Alice completed the cobalt project after a private discussion",
                t(1),
            )
            .expect("raw source")
            .episodic;
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();

        let fact_id = memory
            .add_atomic_fact(
                AtomicFactInput::new("Alice completed the cobalt project", vec![source])
                    .with_embedding_surface(
                        "Alice completed the cobalt project\nEvidence: private discussion",
                    ),
            )
            .expect("grounded atomic fact");

        assert_eq!(
            calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            [(
                "passage",
                "Alice completed the cobalt project\nEvidence: private discussion".to_owned()
            )]
        );
        let fact = memory
            .engine()
            .graph()
            .storage()
            .get_atomic_fact(fact_id)
            .expect("stored atomic fact");
        assert_eq!(fact.content, "Alice completed the cobalt project");
        assert!(
            !fact.content.contains("private discussion"),
            "the richer evidence surface is embedding-only"
        );
    }

    fn reviewed_derivation_input(source: NodeId, idempotency_key: &str) -> ReviewedDerivationInput {
        ReviewedDerivationInput::new(
            RoutingProposition::new("Alice", "media:read-author", "Ursula K. Le Guin")
                .with_value_kind("person")
                .with_polarity(DerivationPolarity::Affirmed)
                .with_modality(DerivationModality::Inferred),
            "Alice has read books by author Ursula K. Le Guin",
            vec![source],
            ReviewProvenance::new("reviewer:test", "public-bridge:v1", t(2), idempotency_key),
        )
        .with_entity_tags(vec!["Alice".to_owned(), "Ursula K. Le Guin".to_owned()])
        .with_metadata(vec![("consumer:run".to_owned(), "run-1".to_owned())])
    }

    #[test]
    fn reviewed_derivation_admission_stamps_typed_review_provenance() {
        let mut memory = mem();
        let source = memory
            .add(
                "session",
                "Alice",
                "The Left Hand of Darkness is one of my favorite books",
                t(1),
            )
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");

        let fact_id = memory
            .add_reviewed_derivation(reviewed_derivation_input(source, "bridge:alice-le-guin"))
            .expect("reviewed derivation");
        let fact = memory
            .engine()
            .graph()
            .storage()
            .get_atomic_fact(fact_id)
            .expect("stored reviewed derivation");

        assert_eq!(
            fact.content,
            "Alice has read books by author Ursula K. Le Guin"
        );
        assert_eq!(fact.source_node_ids, vec![source]);
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_SCHEMA_KEY),
            Some(&"reviewed-routing-v1".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_SUBJECT_KEY),
            Some(&"Alice".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_PREDICATE_KEY),
            Some(&"media:read-author".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_OBJECT_KEY),
            Some(&"Ursula K. Le Guin".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_VALUE_KIND_KEY),
            Some(&"person".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_POLARITY_KEY),
            Some(&"affirmed".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_MODALITY_KEY),
            Some(&"inferred".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_REVIEWED_BY_KEY),
            Some(&"reviewer:test".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_REVIEW_PROFILE_KEY),
            Some(&"public-bridge:v1".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_REVIEWED_AT_KEY),
            Some(&"2".to_owned())
        );
        assert_eq!(
            fact.metadata.get(REVIEWED_DERIVATION_IDEMPOTENCY_KEY),
            Some(&"bridge:alice-le-guin".to_owned())
        );
        assert_eq!(fact.metadata.get("consumer:run"), Some(&"run-1".to_owned()));
        assert_eq!(memory.atomic_fact_count(), 1);
    }

    #[test]
    fn reviewed_derivation_is_idempotent_without_reembedding_and_conflicts_fail() {
        let (mut memory, calls) = recording_mem();
        let source = memory
            .add(
                "session",
                "Alice",
                "The Left Hand of Darkness is one of my favorite books",
                t(1),
            )
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();

        let input = reviewed_derivation_input(source, "bridge:idempotent");
        let first = memory
            .add_reviewed_derivation(input.clone())
            .expect("first admission");
        let repeated = memory
            .add_reviewed_derivation(input)
            .expect("idempotent retry");
        assert_eq!(first, repeated);
        assert_eq!(memory.atomic_fact_count(), 1);
        assert_eq!(
            calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            [(
                "passage",
                "Alice has read books by author Ursula K. Le Guin".to_owned()
            )],
            "an idempotent retry must not invoke the embedding provider again"
        );

        let mut conflicting = reviewed_derivation_input(source, "bridge:idempotent");
        conflicting.proposition.object = "Octavia E. Butler".to_owned();
        conflicting.search_projection = "Alice has read books by Octavia E. Butler".to_owned();
        assert!(
            memory.add_reviewed_derivation(conflicting).is_err(),
            "one retry key cannot authorize different derivation content"
        );
        assert_eq!(memory.atomic_fact_count(), 1);
    }

    #[test]
    fn atomic_fact_input_cannot_spoof_reviewed_derivation_authority() {
        let mut memory = mem();
        let source = memory
            .add("session", "Alice", "raw premise", t(1))
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");

        let spoof = AtomicFactInput::new("fabricated bridge", vec![source]).with_metadata(vec![(
            REVIEWED_DERIVATION_REVIEWED_BY_KEY.to_owned(),
            "self-authorized".to_owned(),
        )]);
        assert!(memory.add_atomic_fact(spoof).is_err());
        assert_eq!(memory.atomic_fact_count(), 0);
    }

    #[test]
    fn reviewed_derivation_rejects_ambiguous_or_engine_owned_metadata() {
        let mut memory = mem();
        let source = memory
            .add("session", "Alice", "raw premise", t(1))
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");

        let invalid_metadata = [
            vec![
                ("consumer:key".to_owned(), "first".to_owned()),
                ("consumer:key".to_owned(), "second".to_owned()),
            ],
            vec![(" consumer:key".to_owned(), "value".to_owned())],
            vec![(
                REVIEWED_DERIVATION_REVIEWED_BY_KEY.to_owned(),
                "self-authorized".to_owned(),
            )],
            vec![(
                "anamnesis:evidence-object".to_owned(),
                "fabricated rerank cue".to_owned(),
            )],
            vec![(
                crate::storage::NODE_INCARNATION_METADATA_KEY.to_owned(),
                "forged".to_owned(),
            )],
            vec![(
                crate::storage::atomic_source_incarnation_key(source),
                "forged".to_owned(),
            )],
        ];
        for (index, metadata) in invalid_metadata.into_iter().enumerate() {
            let input = reviewed_derivation_input(source, &format!("bridge:metadata:{index}"))
                .with_metadata(metadata);
            assert!(memory.add_reviewed_derivation(input).is_err());
        }
        assert_eq!(memory.atomic_fact_count(), 0);
    }

    #[test]
    fn reviewed_derivation_projection_never_enters_reader_evidence() {
        const ROUTING_ONLY_SENTINEL: &str = "zxqvbridge";

        let mut memory = mem();
        let source = memory
            .add(
                "session",
                "Alice",
                "The Left Hand of Darkness is one of my favorite books",
                t(1),
            )
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");
        let mut input = reviewed_derivation_input(source, "bridge:routing-only");
        input.search_projection =
            format!("{ROUTING_ONLY_SENTINEL} connects Alice to Ursula K. Le Guin");
        memory
            .add_reviewed_derivation(input)
            .expect("reviewed derivation");

        let recall = memory
            .search_at(
                "Why does zxqvbridge connect Alice to Ursula K. Le Guin?",
                8,
                t(100),
            )
            .expect("route through reviewed derivation");
        assert!(recall.hits.iter().any(|hit| hit.node_id == source));
        assert!(
            recall
                .hits
                .iter()
                .all(|hit| !hit.text.contains(ROUTING_ONLY_SENTINEL))
        );
        assert!(!recall.as_context().contains(ROUTING_ONLY_SENTINEL));
    }

    #[test]
    fn reviewed_derivation_is_not_routable_before_its_review_time() {
        const ROUTING_ONLY_SENTINEL: &str = "zxqvreviewclock";

        let mut memory = mem();
        let source = memory
            .add("session", "Alice", "an unrelated raw premise", t(1))
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");
        let mut input = reviewed_derivation_input(source, "bridge:review-clock");
        input.search_projection = ROUTING_ONLY_SENTINEL.to_owned();
        input.review.reviewed_at = t(10);
        let fact_id = memory
            .add_reviewed_derivation(input)
            .expect("reviewed derivation");
        let fact = memory
            .engine()
            .graph()
            .storage()
            .get_atomic_fact(fact_id)
            .expect("stored reviewed derivation");
        assert_eq!(fact.observed_at, t(10));

        let before_review = memory
            .search_result_at_with_diagnostics(
                "Why zxqvreviewclock?",
                8,
                t(9),
                &SearchTuning::default(),
                &SearchDiagnostics::with_readout_trace_limit(20),
            )
            .expect("historical read before review");
        assert!(
            !before_review
                .trace
                .strategies_used
                .iter()
                .any(|strategy| strategy == "atomic_fact_routing")
        );

        let at_review = memory
            .search_result_at_with_diagnostics(
                "Why zxqvreviewclock?",
                8,
                t(10),
                &SearchTuning::default(),
                &SearchDiagnostics::with_readout_trace_limit(20),
            )
            .expect("read at review time");
        assert!(
            at_review
                .trace
                .strategies_used
                .iter()
                .any(|strategy| strategy == "atomic_fact_routing")
        );
    }

    #[test]
    fn reviewed_derivation_requires_complete_review_provenance() {
        let mut memory = mem();
        let source = memory
            .add("session", "Alice", "raw premise", t(1))
            .expect("raw premise")
            .episodic;
        memory.flush_all().expect("flush raw premise");

        for (reviewed_by, review_profile, idempotency_key) in [
            ("", "profile", "key-1"),
            ("reviewer", "", "key-2"),
            ("reviewer", "profile", ""),
        ] {
            let mut input = reviewed_derivation_input(source, idempotency_key);
            input.review.reviewed_by = reviewed_by.to_owned();
            input.review.review_profile = review_profile.to_owned();
            input.review.idempotency_key = idempotency_key.to_owned();
            assert!(memory.add_reviewed_derivation(input).is_err());
        }
        assert_eq!(memory.atomic_fact_count(), 0);
    }

    #[test]
    fn reviewed_derivation_rejects_sources_not_live_at_review_time() {
        let mut future_memory = mem();
        let future_source = future_memory
            .add("session", "Alice", "future premise", t(10))
            .expect("future premise")
            .episodic;
        future_memory.flush_all().expect("flush future premise");
        let mut future_input = reviewed_derivation_input(future_source, "bridge:future");
        future_input.review.reviewed_at = t(9);
        assert!(future_memory.add_reviewed_derivation(future_input).is_err());
        assert_eq!(future_memory.atomic_fact_count(), 0);

        let mut retracted_memory = mem();
        let retracted_source = retracted_memory
            .add("session", "Alice", "retracted premise", t(1))
            .expect("retracted premise")
            .episodic;
        retracted_memory
            .flush_all()
            .expect("flush retracted premise");
        retracted_memory
            .set_metadata(retracted_source, "retracted", "true")
            .expect("retract source");
        assert!(
            retracted_memory
                .add_reviewed_derivation(reviewed_derivation_input(
                    retracted_source,
                    "bridge:retracted",
                ))
                .is_err()
        );
        assert_eq!(retracted_memory.atomic_fact_count(), 0);
    }

    #[test]
    fn reviewed_atomic_fact_relation_is_typed_idempotent_and_cascades() {
        let mut memory = mem();
        let source_a = memory
            .add("session-a", "user", "the rollout was delayed", t(1))
            .expect("source a")
            .episodic;
        let source_b = memory
            .add(
                "session-b",
                "user",
                "a supplier changed the delivery terms",
                t(2),
            )
            .expect("source b")
            .episodic;
        memory.flush_all().expect("flush sources");
        let fact_a = memory
            .add_atomic_fact(AtomicFactInput::new(
                "the rollout was delayed",
                vec![source_a],
            ))
            .expect("fact a");
        let fact_b = memory
            .add_atomic_fact(AtomicFactInput::new(
                "a supplier changed the delivery terms",
                vec![source_b],
            ))
            .expect("fact b");
        let input = AtomicFactRelationInput::new(
            fact_b,
            fact_a,
            AtomicFactRelationKind::Reason,
            "reviewer",
            "policy-v1",
            t(3),
            "relation-key",
        );
        let first = memory
            .add_atomic_fact_relation(input.clone())
            .expect("reviewed relation");
        let repeated = memory
            .add_atomic_fact_relation(input)
            .expect("idempotent retry");
        assert_eq!(first, repeated);
        assert_eq!(memory.atomic_fact_relation_count(), 1);
        assert!(
            memory
                .add_atomic_fact_relation(AtomicFactRelationInput::new(
                    fact_b,
                    fact_a,
                    AtomicFactRelationKind::Supports,
                    "reviewer",
                    "policy-v1",
                    t(3),
                    "relation-key",
                ))
                .is_err(),
            "an idempotency key cannot authorize different relation content"
        );

        memory
            .delete_atomic_fact(fact_a)
            .expect("delete endpoint fact");
        assert_eq!(memory.atomic_fact_relation_count(), 0);
    }

    #[test]
    fn reviewed_atomic_relation_path_survives_the_production_search_trace() {
        let mut memory = mem();
        let outcome_source = memory
            .add("session-a", "user", "the rollout was delayed", t(1))
            .expect("outcome source")
            .episodic;
        let reason_source = memory
            .add(
                "session-b",
                "user",
                "a supplier changed the delivery terms",
                t(2),
            )
            .expect("reason source")
            .episodic;
        memory.flush_all().expect("flush relation sources");
        let outcome_fact = memory
            .add_atomic_fact(AtomicFactInput::new(
                "the rollout was delayed",
                vec![outcome_source],
            ))
            .expect("outcome fact");
        let reason_fact = memory
            .add_atomic_fact(AtomicFactInput::new(
                "a supplier changed the delivery terms",
                vec![reason_source],
            ))
            .expect("reason fact");
        memory
            .add_atomic_fact_relation(AtomicFactRelationInput::new(
                reason_fact,
                outcome_fact,
                AtomicFactRelationKind::Reason,
                "reviewer",
                "policy-v1",
                t(3),
                "production-trace-link",
            ))
            .expect("reviewed relation");

        let result = memory
            .search_result_at_with_diagnostics(
                "Why was the rollout delayed, given that the supplier changed the delivery terms?",
                8,
                t(100),
                &SearchTuning::default(),
                &SearchDiagnostics::with_readout_trace_limit(50),
            )
            .expect("relation-aware source search");
        let paths = readout::validated_atomic_relation_paths(
            memory.engine().graph().storage(),
            &result.trace.strategies_used,
            t(100),
            &ScopePath::universal(),
        )
        .expect("trace relation path validation");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].fact_ids.len(), 2);
        assert!(paths[0].fact_ids.contains(&outcome_fact));
        assert!(paths[0].fact_ids.contains(&reason_fact));
        assert_eq!(paths[0].hops[0].from_fact_id, reason_fact);
        assert_eq!(paths[0].hops[0].to_fact_id, outcome_fact);
    }

    #[test]
    fn reviewed_relation_rejects_reused_raw_source_incarnation() {
        let mut memory = mem();
        let source_a = memory
            .add("shared-session", "user", "the rollout was delayed", t(1))
            .expect("source a")
            .episodic;
        let source_b = memory
            .add(
                "other-session",
                "user",
                "a supplier changed the delivery terms",
                t(2),
            )
            .expect("source b")
            .episodic;
        memory.flush_all().expect("flush sources");
        let fact_a = memory
            .add_atomic_fact(AtomicFactInput::new(
                "the rollout was delayed",
                vec![source_a],
            ))
            .expect("fact a");
        let fact_b = memory
            .add_atomic_fact(AtomicFactInput::new(
                "a supplier changed the delivery terms",
                vec![source_b],
            ))
            .expect("fact b");

        let mut replacement = memory
            .engine()
            .graph()
            .get_node(source_a)
            .expect("original raw source")
            .clone();
        memory
            .engine_mut()
            .graph_mut()
            .remove_node(source_a)
            .expect("original raw source deletes");
        let replacement_id = memory.engine_mut().graph_mut().next_node_id();
        assert_eq!(replacement_id, source_a);
        replacement.id = replacement_id;
        memory
            .engine_mut()
            .graph_mut()
            .add_node(replacement)
            .expect("replacement raw source stores");

        assert!(
            memory
                .add_atomic_fact_relation(AtomicFactRelationInput::new(
                    fact_a,
                    fact_b,
                    AtomicFactRelationKind::Supports,
                    "reviewer",
                    "policy-v1",
                    t(3),
                    "reused-source-relation",
                ))
                .is_err(),
            "a reused numeric node ID must not inherit reviewed source authority"
        );
        assert_eq!(memory.atomic_fact_relation_count(), 0);
    }

    #[test]
    fn atomic_fact_lane_serves_count_and_frequency_queries() {
        let mut m = mem();
        let source = m
            .add(
                "session",
                "alice",
                "Alice completed the cobalt project on 5 June 2023 and attends a health checkup every year",
                t(1),
            )
            .unwrap()
            .episodic;
        m.flush_all().unwrap();
        m.add_atomic_fact(
            AtomicFactInput::new(
                "Alice completed the cobalt project on 5 June 2023",
                vec![source],
            )
            .with_entity_tags(vec!["Alice".to_owned(), "cobalt project".to_owned()]),
        )
        .unwrap();
        m.add_atomic_fact(
            AtomicFactInput::new("Alice attends a health checkup every year", vec![source])
                .with_entity_tags(vec!["Alice".to_owned(), "health checkup".to_owned()]),
        )
        .unwrap();

        for query in [
            "How many cobalt projects did Alice complete?",
            "How often does Alice attend a health checkup?",
        ] {
            let result = m
                .search_result_at_with_diagnostics(
                    query,
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
                    .any(|strategy| strategy == "atomic_fact_routing"),
                "{query:?} should enter the isolated atomic lane"
            );
            assert!(
                result.trace.strategies_used.iter().any(|strategy| {
                    strategy
                        .strip_prefix("atomic_fact_sources:")
                        .is_some_and(|sources| {
                            sources
                                .split(',')
                                .all(|source| source.split('@').count() == 3)
                        })
                }),
                "{query:?} should retain typed claim provenance"
            );
        }
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
            "When was the alpha archive created?",
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
                "sidecar must preserve direct or temporal readout for {query:?}"
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
        assert!(
            block.contains("source=node:"),
            "memory-owned rendering must expose fragment-local source ids, got:\n{block}"
        );
        assert!(
            !recall.as_context().contains("source=node:"),
            "the legacy package-only wire must remain byte-compatible"
        );
    }

    #[test]
    fn memory_owned_rendering_binds_each_semantic_window_line_to_its_raw_turn() {
        let mut m = mem();
        let first = m
            .add(
                "deploy",
                "Alpha",
                "I deployed to staging, canary, and production",
                t(1),
            )
            .unwrap()
            .episodic;
        let second = m
            .add("deploy", "Beta", "I deployed to sandbox", t(2))
            .unwrap()
            .episodic;
        let third_receipt = m
            .add(
                "deploy",
                "Alpha",
                "I want to deploy again\nAlpha shared a deployment chart",
                t(3),
            )
            .unwrap();
        let third = third_receipt.episodic;
        let middle_window = third_receipt
            .finalized_semantic
            .expect("the third turn finalizes the middle semantic window");
        m.flush_all().unwrap();

        let recall = m.search_at("deployment environments", 10, t(100)).unwrap();
        let detailed = m.render_context(&recall).unwrap();
        let evidence = m
            .render_context_with(
                &recall,
                ContextRenderOptions::with_style(ContextRenderStyle::Evidence),
            )
            .unwrap();
        let middle_marker = format!("- [Semantic source=node:{}]", middle_window.0);
        let middle_block = detailed
            .split_once(&middle_marker)
            .expect("the middle semantic window is rendered")
            .1
            .split("\n- [")
            .next()
            .expect("the middle semantic block has a body");

        assert!(
            middle_block.contains(&format!(
                "[turn-source=node:{}] Alpha: I deployed to staging, canary, and production",
                first.0
            )),
            "Alpha's detailed line must retain its raw source in the same semantic block:\n\
             {middle_block}"
        );
        assert!(
            middle_block.contains(&format!(
                "[turn-source=node:{}] Beta: I deployed to sandbox",
                second.0
            )),
            "Beta's detailed line must not inherit the enclosing semantic source:\n{middle_block}"
        );
        assert!(
            middle_block.contains(&format!(
                "[turn-source=node:{}] Alpha: I want to deploy again",
                third.0
            )),
            "the following detailed line must retain its own source:\n{middle_block}"
        );
        assert!(
            middle_block.contains(&format!(
                "[turn-source=node:{}] Alpha shared a deployment chart",
                third.0
            )),
            "every line of a multiline raw turn must retain its source:\n{middle_block}"
        );
        for (source, line) in [
            (
                first,
                "Alpha: I deployed to staging, canary, and production",
            ),
            (second, "Beta: I deployed to sandbox"),
            (third, "Alpha: I want to deploy again"),
        ] {
            assert!(
                evidence.contains(&format!("[Episodic source=node:{}]", source.0))
                    && evidence.contains(line),
                "coalesced evidence must retain one raw-source block per turn:\n{evidence}"
            );
        }
        assert!(!recall.as_context().contains("turn-source=node:"));
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
        assert!(block.contains("source=node:"));
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
        let readout = m.readout_for("When did Alice do yoga?", &recall).unwrap();
        assert!(!readout.source_node_ids.is_empty());
        assert!(
            readout.source_attributions.iter().any(|source| {
                source.speaker.as_deref() == Some("alice")
                    && source.text.contains("yoga yesterday")
                    && readout.source_node_ids.contains(&source.source_node_id)
            }),
            "source ownership must come from canonical speaker provenance"
        );

        let temporal = m
            .render_context_for("When did Alice do yoga?", &recall)
            .unwrap();
        assert!(temporal.contains("resolved relative time: \"yesterday\" = 5 June 2023"));

        let direct = m
            .render_context_for("What exercise did Alice do?", &recall)
            .unwrap();
        let query_blind = m.render_context(&recall).unwrap();
        assert!(!query_blind.contains("## QUERY-FOCUSED RAW EVIDENCE"));
        assert!(!query_blind.contains("## RECALL GUIDANCE"));
        assert!(direct.contains("## QUERY-FOCUSED RAW EVIDENCE"));
        assert!(direct.contains("requested attribute and granularity"));
        assert!(!direct.contains("resolved relative time"));

        let guided = m
            .render_context_for("Which service likely accepted the request?", &recall)
            .unwrap();
        assert!(guided.contains("## RECALL GUIDANCE"));
        assert!(guided.contains("one concise conclusion"));

        let date_scoped_query = "Which yoga exercise did Alice do in June 2023?";
        let date_scoped_plan = RecallPlan::infer(date_scoped_query);
        assert_eq!(date_scoped_plan.answer_shape, AnswerShape::Fact);
        assert_eq!(date_scoped_plan.recall_intent, RecallIntent::Temporal);
        let date_scoped = m.render_context_for(date_scoped_query, &recall).unwrap();
        assert!(
            date_scoped.contains("resolved relative time: \"yesterday\" = 5 June 2023"),
            "date-scoped factual queries must retain temporal intent:\n{date_scoped}"
        );

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
    fn query_focus_uses_only_delivered_raw_or_source_bound_lines() {
        let raw_id = NodeId(101);
        let semantic_id = NodeId(102);
        let origin = Origin {
            peer_id: PeerId(7),
            source_kind: SourceKind::AgentObservation,
            session_id: "session-a".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let other_origin = Origin {
            scope: ScopePath::new("workspace/other").expect("test scope"),
            ..origin.clone()
        };
        let raw_line = "Alice: The archive key is cobalt.";
        let derived_line = "The archive probably uses a blue key.";
        let mut package = ContextPackage::empty();
        package.memories.push(Fragment {
            node_id: raw_id,
            name: "raw turn".to_owned(),
            summary: None,
            content: Some(raw_line.to_owned()),
            node_type: KnowledgeType::Episodic,
            relevance: 0.7,
            origin: origin.clone(),
        });
        package.memories.push(Fragment {
            node_id: NodeId(999),
            name: "other scoped turn".to_owned(),
            summary: Some(derived_line.to_owned()),
            content: None,
            node_type: KnowledgeType::Episodic,
            relevance: 0.8,
            origin: other_origin,
        });
        package.knowledge.push(Fragment {
            node_id: semantic_id,
            name: "window".to_owned(),
            summary: None,
            content: Some(format!("{raw_line}\n{derived_line}")),
            node_type: KnowledgeType::Semantic,
            relevance: 0.9,
            origin,
        });
        package.token_usage.total = 4_000;
        let times = HashMap::from([
            (
                raw_id,
                FragmentTime {
                    observed_at: Timestamp(1_683_504_000_000),
                    valid_from: None,
                    valid_until: None,
                },
            ),
            (
                semantic_id,
                FragmentTime {
                    observed_at: Timestamp(1_683_504_000_000),
                    valid_from: None,
                    valid_until: None,
                },
            ),
            (
                NodeId(999),
                FragmentTime {
                    observed_at: Timestamp(1_683_504_000_000),
                    valid_from: None,
                    valid_until: None,
                },
            ),
        ]);
        let line_sources = HashMap::from([(
            semantic_id,
            HashMap::from([
                (raw_line.to_owned(), raw_id),
                (derived_line.to_owned(), NodeId(999)),
            ]),
        )]);

        let focused = query_focused_raw_evidence(
            &package,
            &times,
            &line_sources,
            &RecallPlan::infer("What is Alice's archive key?"),
            1_000,
        )
        .expect("one delivered raw line is focusable");

        assert_eq!(focused.matches(raw_line).count(), 1);
        assert!(!focused.contains(derived_line));
        assert!(!focused.contains("node:999"));
        assert!(focused.contains("source=node:101 observed 2023-05-08T00:00:00Z"));
    }

    #[test]
    fn structured_readout_exposes_only_live_source_bound_semantic_lines() {
        let mut m = mem();
        let receipt = m
            .add("session-a", "Alice", "The archive key is cobalt.", t(10))
            .unwrap();
        let semantic_id = m
            .flush_session("session-a")
            .unwrap()
            .expect("semantic window");
        let semantic = m.engine().graph().get_node(semantic_id).unwrap();
        let mut package = ContextPackage::empty();
        package.knowledge.push(Fragment {
            node_id: semantic_id,
            name: "delivered semantic window".to_owned(),
            summary: None,
            content: Some(
                "Alice: The archive key is cobalt.\nThe archive probably uses a blue key."
                    .to_owned(),
            ),
            node_type: KnowledgeType::Semantic,
            relevance: 0.9,
            origin: semantic.origin.clone(),
        });
        package.token_usage.total = 1_000;
        let recall = Recall {
            hits: Vec::new(),
            package,
        };

        let readout = m
            .readout_for("What is Alice's archive key?", &recall)
            .unwrap();

        assert_eq!(readout.plan.answer_shape, AnswerShape::Fact);
        assert_eq!(readout.plan.recall_intent, RecallIntent::Direct);
        assert!(readout.reader_guidance.is_some());
        assert_eq!(readout.focused_evidence.len(), 1);
        let evidence = &readout.focused_evidence[0];
        assert_eq!(evidence.source_node_id, receipt.episodic);
        assert_eq!(evidence.observed_at, t(10));
        assert_eq!(evidence.session_id, "session-a");
        assert_eq!(evidence.text, "Alice: The archive key is cobalt.");
        assert!(
            readout
                .focused_evidence
                .iter()
                .all(|evidence| !evidence.text.contains("probably"))
        );
    }

    #[test]
    fn query_focus_promotes_complementary_bridge_after_query_anchor() {
        let semantic_id = NodeId(200);
        let origin = Origin {
            peer_id: PeerId(9),
            source_kind: SourceKind::AgentObservation,
            session_id: "bridge-session".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let anchor = "Alice: We discussed the bookshelf picture.";
        let distractors = [
            "Alice: The bookshelf picture was mentioned again.",
            "Alice: We kept talking about the bookshelf picture.",
            "Alice: The picture remained near the bookshelf.",
            "Alice: The bookshelf still held the picture.",
            "Alice: We remembered the same bookshelf picture.",
            "Alice: The picture and bookshelf came up later.",
        ];
        let bridge = "Alice: It was created by Atelier Nimbus in 2021.";
        let mut lines = vec![anchor];
        lines.extend(distractors);
        lines.push(bridge);

        let mut package = ContextPackage::empty();
        package.knowledge.push(Fragment {
            node_id: semantic_id,
            name: "source-bound semantic window".to_owned(),
            summary: None,
            content: Some(lines.join("\n")),
            node_type: KnowledgeType::Semantic,
            relevance: 0.9,
            origin,
        });
        package.token_usage.total = 4_000;

        let mut times = HashMap::from([(
            semantic_id,
            FragmentTime {
                observed_at: Timestamp(1_683_504_000_000),
                valid_from: None,
                valid_until: None,
            },
        )]);
        let mut bound_lines = HashMap::new();
        for (index, line) in lines.iter().enumerate() {
            let source_id = NodeId(201 + index as u64);
            bound_lines.insert((*line).to_owned(), source_id);
            times.insert(
                source_id,
                FragmentTime {
                    observed_at: Timestamp(1_683_504_000_000 + index as u64),
                    valid_from: None,
                    valid_until: None,
                },
            );
        }
        let line_sources = HashMap::from([(semantic_id, bound_lines)]);
        let focused = query_focused_raw_evidence(
            &package,
            &times,
            &line_sources,
            &RecallPlan::infer("How are Alice and the bookshelf picture related?"),
            4_000,
        )
        .expect("source-bound bridge evidence");

        assert!(focused.contains(anchor));
        assert!(focused.contains(bridge));
        assert!(
            focused.find(anchor) < focused.find(bridge),
            "the query anchor must lead its complementary bridge:\n{focused}"
        );
        assert_eq!(
            focused
                .lines()
                .filter(|line| line.starts_with("- [source=node:"))
                .count(),
            query_focus_line_limit(&RecallPlan::infer(
                "How are Alice and the bookshelf picture related?"
            ))
        );
    }

    #[test]
    fn modal_named_target_focus_preserves_strong_preference_and_later_fulfillment() {
        let origin = Origin {
            peer_id: PeerId(9),
            source_kind: SourceKind::AgentObservation,
            session_id: "endorsement-history".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let fulfillment = "Rowan: Last week I landed a partnership deal with a renowned outdoor equipment company.";
        let strong_preference =
            "Rowan: I've always liked Alpine Forge; working with them would be really exciting.";
        let generic_list =
            "Rowan: I am currently considering brands like Summit Works and Alpine Forge.";
        let rows = [
            (NodeId(401), fulfillment, Timestamp(200), 0.95),
            (NodeId(402), generic_list, Timestamp(80), 0.94),
            (
                NodeId(403),
                "Rowan: The outdoor equipment partnership discussion covered product testing.",
                Timestamp(160),
                0.93,
            ),
            (
                NodeId(404),
                "Rowan: The outdoor equipment company requested a forest photo session.",
                Timestamp(170),
                0.92,
            ),
            (
                NodeId(405),
                "Rowan: The partnership paperwork included a launch calendar.",
                Timestamp(175),
                0.91,
            ),
            (
                NodeId(406),
                "Rowan: The outdoor equipment samples included a weatherproof pack.",
                Timestamp(180),
                0.90,
            ),
            (
                NodeId(407),
                "Rowan: The company arranged a product photography schedule.",
                Timestamp(185),
                0.89,
            ),
            (
                NodeId(408),
                "Rowan: The partnership launch involved a trail demonstration.",
                Timestamp(190),
                0.88,
            ),
            (
                NodeId(409),
                "Rowan: The outdoor company provided several equipment samples.",
                Timestamp(195),
                0.87,
            ),
            (NodeId(410), strong_preference, Timestamp(100), 0.05),
        ];
        let mut package = ContextPackage::empty();
        let mut times = HashMap::new();
        for (node_id, text, observed_at, relevance) in rows {
            package.memories.push(Fragment {
                node_id,
                name: format!("raw source {}", node_id.0),
                summary: None,
                content: Some(text.to_owned()),
                node_type: KnowledgeType::Episodic,
                relevance,
                origin: origin.clone(),
            });
            times.insert(
                node_id,
                FragmentTime {
                    observed_at,
                    valid_from: None,
                    valid_until: None,
                },
            );
        }
        package.token_usage.total = 4_000;
        let plan = RecallPlan::infer(
            "Which outdoor equipment company likely signed Rowan for a partnership?",
        );
        assert_eq!(plan.answer_shape, AnswerShape::Inference);
        assert_eq!(
            plan.reader_contract().requested_answer_spec().modality(),
            RequestedAnswerModality::Likely
        );

        let focused = query_focused_raw_evidence(&package, &times, &HashMap::new(), &plan, 4_000)
            .expect("modal identity evidence focus");
        let evidence_lines = focused
            .lines()
            .filter(|line| line.starts_with("- [source=node:"))
            .collect::<Vec<_>>();

        assert_eq!(evidence_lines.len(), query_focus_line_limit(&plan));
        assert!(evidence_lines[0].contains(fulfillment));
        assert!(evidence_lines[1].contains(strong_preference));
        assert!(focused.contains(generic_list));
        assert!(query_focus_preference_signal(generic_list).is_none());
    }

    fn modal_bridge_test_candidate(
        node_id: u64,
        text: &str,
        observed_at: u64,
        peer_id: u64,
        scope: &str,
    ) -> QueryFocusedCandidate {
        QueryFocusedCandidate {
            evidence: FocusedEvidence {
                source_node_id: NodeId(node_id),
                observed_at: Timestamp(observed_at),
                session_id: format!("session-{node_id}"),
                text: text.to_owned(),
            },
            peer_id: PeerId(peer_id),
            scope: ScopePath::new(scope).expect("test scope"),
            lexical_overlap: 0,
            dialogue_reply_overlap: 0,
            link_terms: HashSet::new(),
            value_terms: HashSet::new(),
            information_terms: HashSet::new(),
            semantic_windows: HashSet::new(),
            temporal_alignment: 0,
            temporal_distance_ms: u64::MAX,
            relevance: 0.0,
            fragment_order: node_id as usize,
            line_order: 0,
        }
    }

    fn modal_bridge_test_plan() -> RecallPlan {
        RecallPlan::infer("Which outdoor equipment company likely signed Rowan for a partnership?")
    }

    const MODAL_BRIDGE_TEST_PREFERENCE: &str =
        "Rowan: I've always liked Alpine Forge; working with them would be really exciting.";
    const MODAL_BRIDGE_TEST_FULFILLMENT: &str =
        "Rowan: Recently, I landed a partnership deal with a renowned outdoor equipment company.";

    #[test]
    fn modal_bridge_requires_the_exact_query_owner() {
        let candidates = [
            modal_bridge_test_candidate(
                1,
                "Morgan: I've always liked Alpine Forge; working with them would be exciting.",
                100,
                1,
                "workspace/a",
            ),
            modal_bridge_test_candidate(
                2,
                "Morgan: I landed a partnership deal with an outdoor equipment company.",
                200,
                1,
                "workspace/a",
            ),
        ];

        assert!(query_focus_modal_bridge(&candidates, &modal_bridge_test_plan()).is_none());
    }

    #[test]
    fn modal_bridge_requires_matching_peer_and_scope_provenance() {
        let preference =
            modal_bridge_test_candidate(1, MODAL_BRIDGE_TEST_PREFERENCE, 100, 1, "workspace/a");
        let different_peer =
            modal_bridge_test_candidate(2, MODAL_BRIDGE_TEST_FULFILLMENT, 200, 2, "workspace/a");
        assert!(
            query_focus_modal_bridge(&[preference, different_peer], &modal_bridge_test_plan())
                .is_none()
        );

        let preference =
            modal_bridge_test_candidate(3, MODAL_BRIDGE_TEST_PREFERENCE, 100, 1, "workspace/a");
        let different_scope =
            modal_bridge_test_candidate(4, MODAL_BRIDGE_TEST_FULFILLMENT, 200, 1, "workspace/b");
        assert!(
            query_focus_modal_bridge(&[preference, different_scope], &modal_bridge_test_plan())
                .is_none()
        );
    }

    #[test]
    fn modal_bridge_rejects_negated_cancelled_and_corrected_preferences() {
        for (index, preference) in [
            "Rowan: I prefer not to work with Alpine Forge.",
            "Rowan: I want to work with Alpine Forge, but that plan is cancelled.",
            "Rowan: I've always liked Alpine Forge, but now I changed my mind about working with them.",
        ]
        .into_iter()
        .enumerate()
        {
            let candidates = [
                modal_bridge_test_candidate(
                    index as u64 + 1,
                    preference,
                    100,
                    1,
                    "workspace/a",
                ),
                modal_bridge_test_candidate(
                    index as u64 + 10,
                    MODAL_BRIDGE_TEST_FULFILLMENT,
                    200,
                    1,
                    "workspace/a",
                ),
            ];
            assert!(
                query_focus_modal_bridge(&candidates, &modal_bridge_test_plan()).is_none(),
                "unsafe preference was admitted: {preference}"
            );
        }
    }

    #[test]
    fn modal_bridge_disables_ambiguous_competing_candidate_identities() {
        let candidates = [
            modal_bridge_test_candidate(1, MODAL_BRIDGE_TEST_PREFERENCE, 100, 1, "workspace/a"),
            modal_bridge_test_candidate(
                2,
                "Rowan: I really like Summit Works; working with them would be exciting.",
                150,
                1,
                "workspace/a",
            ),
            modal_bridge_test_candidate(3, MODAL_BRIDGE_TEST_FULFILLMENT, 200, 1, "workspace/a"),
        ];

        assert!(query_focus_modal_bridge(&candidates, &modal_bridge_test_plan()).is_none());
    }

    #[test]
    fn modal_bridge_prefers_the_most_recent_repetition_of_one_candidate() {
        let candidates = [
            modal_bridge_test_candidate(1, MODAL_BRIDGE_TEST_PREFERENCE, 100, 1, "workspace/a"),
            modal_bridge_test_candidate(
                2,
                "Rowan: I'd like to work with Alpine Forge on a partnership.",
                150,
                1,
                "workspace/a",
            ),
            modal_bridge_test_candidate(3, MODAL_BRIDGE_TEST_FULFILLMENT, 200, 1, "workspace/a"),
        ];

        let bridge = query_focus_modal_bridge(&candidates, &modal_bridge_test_plan())
            .expect("one repeated identity remains unambiguous");
        assert_eq!(bridge.preference_index, 1);
    }

    #[test]
    fn modal_bridge_requires_the_query_relation_on_both_passages() {
        let plan = RecallPlan::infer("Which software company likely hired Rowan?");
        let candidates = [
            modal_bridge_test_candidate(1, MODAL_BRIDGE_TEST_PREFERENCE, 100, 1, "workspace/a"),
            modal_bridge_test_candidate(
                2,
                "Rowan: I got a job at a software company.",
                200,
                1,
                "workspace/a",
            ),
        ];

        assert!(query_focus_modal_bridge(&candidates, &plan).is_none());
    }

    #[test]
    fn modal_bridge_ignores_sentence_initial_common_words_as_entities() {
        assert!(
            query_focus_named_identities(
                "Recently, I landed a partnership deal with a renowned outdoor equipment company."
            )
            .is_empty()
        );
        let candidates = [
            modal_bridge_test_candidate(1, MODAL_BRIDGE_TEST_PREFERENCE, 100, 1, "workspace/a"),
            modal_bridge_test_candidate(2, MODAL_BRIDGE_TEST_FULFILLMENT, 200, 1, "workspace/a"),
        ];

        assert!(query_focus_modal_bridge(&candidates, &modal_bridge_test_plan()).is_some());
    }

    #[test]
    fn modal_named_target_focus_does_not_bridge_another_owners_preference() {
        let rowan_origin = Origin {
            peer_id: PeerId(9),
            source_kind: SourceKind::AgentObservation,
            session_id: "endorsement-history".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let fulfillment = "Rowan: Last week I landed a partnership deal with a renowned outdoor equipment company.";
        let unrelated_preference =
            "Morgan: I've always liked Cedar Studio; working with them would be really exciting.";
        let mut package = ContextPackage::empty();
        let mut times = HashMap::new();
        for index in 0..9u64 {
            let node_id = NodeId(451 + index);
            let text = if index == 0 {
                fulfillment.to_owned()
            } else {
                format!(
                    "Rowan: Outdoor equipment partnership detail {index} covered a distinct launch task."
                )
            };
            package.memories.push(Fragment {
                node_id,
                name: format!("rowan source {index}"),
                summary: None,
                content: Some(text),
                node_type: KnowledgeType::Episodic,
                relevance: 1.0 - index as f64 / 100.0,
                origin: rowan_origin.clone(),
            });
            times.insert(
                node_id,
                FragmentTime {
                    observed_at: Timestamp(200 + index),
                    valid_from: None,
                    valid_until: None,
                },
            );
        }
        let other_id = NodeId(499);
        package.memories.push(Fragment {
            node_id: other_id,
            name: "other owner preference".to_owned(),
            summary: None,
            content: Some(unrelated_preference.to_owned()),
            node_type: KnowledgeType::Episodic,
            relevance: 0.01,
            origin: Origin {
                session_id: "other-history".to_owned(),
                ..rowan_origin
            },
        });
        times.insert(
            other_id,
            FragmentTime {
                observed_at: Timestamp(100),
                valid_from: None,
                valid_until: None,
            },
        );
        package.token_usage.total = 4_000;
        let plan = RecallPlan::infer(
            "Which outdoor equipment company likely signed Rowan for a partnership?",
        );

        let focused = query_focused_raw_evidence(&package, &times, &HashMap::new(), &plan, 4_000)
            .expect("same-owner modal evidence focus");

        assert!(focused.contains(fulfillment));
        assert!(!focused.contains(unrelated_preference));
    }

    #[test]
    fn query_focus_leads_with_a_query_matching_immediate_reply() {
        let semantic_id = NodeId(260);
        let origin = Origin {
            peer_id: PeerId(9),
            source_kind: SourceKind::AgentObservation,
            session_id: "design-session".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let distractor = "Operator: The cobalt pattern uses several colors.";
        let question = "Reviewer: Why did you choose the cobalt pattern?";
        let answer = "Operator: I chose it to catch attention and make users smile.";
        let lines = [distractor, question, answer];
        let mut package = ContextPackage::empty();
        package.knowledge.push(Fragment {
            node_id: semantic_id,
            name: "source-bound dialogue".to_owned(),
            summary: None,
            content: Some(lines.join("\n")),
            node_type: KnowledgeType::Semantic,
            relevance: 0.9,
            origin,
        });
        package.token_usage.total = 2_000;

        let mut times = HashMap::from([(
            semantic_id,
            FragmentTime {
                observed_at: Timestamp(1_683_504_000_000),
                valid_from: None,
                valid_until: None,
            },
        )]);
        let mut bound_lines = HashMap::new();
        for (index, line) in lines.iter().enumerate() {
            let source_id = NodeId(261 + index as u64);
            bound_lines.insert((*line).to_owned(), source_id);
            times.insert(
                source_id,
                FragmentTime {
                    observed_at: Timestamp(1_683_504_000_000 + index as u64),
                    valid_from: None,
                    valid_until: None,
                },
            );
        }
        let focused = query_focused_raw_evidence(
            &package,
            &times,
            &HashMap::from([(semantic_id, bound_lines)]),
            &RecallPlan::infer("Why did the operator choose the cobalt pattern?"),
            2_000,
        )
        .expect("query-matching reply evidence");
        let first_evidence = focused
            .lines()
            .find(|line| line.starts_with("- [source=node:"))
            .expect("focused evidence line");

        assert!(
            first_evidence.contains(answer),
            "an immediate response inherits its query-matching question only for focus ordering:\n{focused}"
        );
    }

    #[test]
    fn query_focus_prefers_relative_event_that_covers_explicit_query_date() {
        let origin = Origin {
            peer_id: PeerId(9),
            source_kind: SourceKind::AgentObservation,
            session_id: "dated-session".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let mut package = ContextPackage::empty();
        let rows = [
            (
                NodeId(301),
                "Alice: Her favorite activity is relaxing outdoors.",
                Timestamp(1_655_424_000_000),
                0.9,
            ),
            (
                NodeId(302),
                "Alice: Yesterday I went bowling after work.",
                Timestamp(1_647_475_200_000),
                0.2,
            ),
            (
                NodeId(303),
                "Alice: The weather was clear that day.",
                Timestamp(1_647_388_800_000),
                0.8,
            ),
        ];
        let mut times = HashMap::new();
        for (node_id, text, observed_at, relevance) in rows {
            package.memories.push(Fragment {
                node_id,
                name: "dated raw turn".to_owned(),
                summary: None,
                content: Some(text.to_owned()),
                node_type: KnowledgeType::Episodic,
                relevance,
                origin: origin.clone(),
            });
            times.insert(
                node_id,
                FragmentTime {
                    observed_at,
                    valid_from: None,
                    valid_until: None,
                },
            );
        }
        package.token_usage.total = 4_000;

        let focused = query_focused_raw_evidence(
            &package,
            &times,
            &HashMap::new(),
            &RecallPlan::infer("Which activity was Alice pursuing on 16 March 2022?"),
            4_000,
        )
        .expect("dated evidence focus");
        let first_evidence = focused
            .lines()
            .find(|line| line.starts_with("- [source=node:"))
            .expect("focused evidence line");
        assert!(
            first_evidence.contains("Yesterday I went bowling"),
            "a source-relative event that covers the requested date must lead:\n{focused}"
        );
    }

    #[test]
    fn query_focus_is_bounded_by_answer_shape_and_remaining_tokens() {
        let origin = Origin {
            peer_id: PeerId(1),
            source_kind: SourceKind::AgentObservation,
            session_id: "collection".to_owned(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        };
        let mut package = ContextPackage::empty();
        let mut times = HashMap::new();
        for index in 0..10u64 {
            let node_id = NodeId(index + 1);
            let content = if index == 0 {
                "Alice: unrelated weather note".to_owned()
            } else {
                format!("Alice: archive project item {index}")
            };
            package.memories.push(Fragment {
                node_id,
                name: format!("raw turn {index}"),
                summary: None,
                content: Some(content),
                node_type: KnowledgeType::Episodic,
                relevance: 1.0 - index as f64 / 100.0,
                origin: origin.clone(),
            });
            times.insert(
                node_id,
                FragmentTime {
                    observed_at: Timestamp(index),
                    valid_from: None,
                    valid_until: None,
                },
            );
        }
        package.token_usage.total = 4_000;
        let plan = RecallPlan::infer("List every archive project item Alice mentioned.");
        let focused = query_focused_raw_evidence(&package, &times, &HashMap::new(), &plan, 4_000)
            .expect("collection focus");
        assert_eq!(
            focused
                .lines()
                .filter(|line| line.starts_with("- [source=node:"))
                .count(),
            8
        );
        assert!(!focused.contains("unrelated weather note"));
        assert!(query_focused_raw_evidence(&package, &times, &HashMap::new(), &plan, 1,).is_none());
        assert_eq!(
            query_focus_line_limit(&RecallPlan::infer("Where does Alice live?")),
            4
        );
        assert_eq!(
            query_focus_line_limit(&RecallPlan::infer("When did Alice move?")),
            5
        );
        assert_eq!(
            query_focus_line_limit(&RecallPlan::infer("What might Alice do next?")),
            8
        );
    }

    #[test]
    fn query_focus_is_identical_across_detailed_and_evidence_layouts() {
        let mut m = mem();
        m.add("session-a", "alice", "the archive key is cobalt", t(1))
            .unwrap();
        m.flush_all().unwrap();
        let recall = m.search_at("What is the archive key?", 10, t(100)).unwrap();
        let detailed = m
            .render_context_for_with(
                "What is the archive key?",
                &recall,
                ContextRenderOptions::with_style(ContextRenderStyle::Detailed),
            )
            .unwrap();
        let evidence = m
            .render_context_for_with(
                "What is the archive key?",
                &recall,
                ContextRenderOptions::with_style(ContextRenderStyle::Evidence),
            )
            .unwrap();
        let structured = m.readout_for("What is the archive key?", &recall).unwrap();
        let focus_section = |rendered: &str| {
            rendered
                .split_once("## QUERY-FOCUSED RAW EVIDENCE\n")
                .map(|(_, focus)| focus.to_owned())
        };

        assert_eq!(focus_section(&detailed), focus_section(&evidence));
        assert!(focus_section(&detailed).is_some());
        let structured_focus = render_query_focused_evidence(&structured.focused_evidence)
            .expect("typed focused evidence");
        assert_eq!(
            focus_section(&detailed),
            structured_focus
                .split_once("## QUERY-FOCUSED RAW EVIDENCE\n")
                .map(|(_, focus)| focus.to_owned())
        );
        for rendered in [&detailed, &evidence] {
            assert!(
                rendered.find("## RECALL GUIDANCE")
                    < rendered.find("## QUERY-FOCUSED RAW EVIDENCE"),
                "guidance must precede the exact evidence tail:\n{rendered}"
            );
        }
    }

    #[test]
    fn empty_query_aware_rendering_remains_byte_stable() {
        let m = mem();
        let recall = Recall {
            hits: Vec::new(),
            package: ContextPackage::empty(),
        };

        assert_eq!(
            m.render_context_for("Where does Alice live?", &recall)
                .unwrap(),
            m.render_context(&recall).unwrap()
        );
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
