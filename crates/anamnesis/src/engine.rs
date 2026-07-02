//! Kernel API — the raw substrate, gathered in one namespace.
//!
//! `anamnesis::engine` re-exports the full kernel surface. The original module
//! paths (`crate::api`, `crate::query`, `crate::graph`, …) remain valid but are
//! hidden from documentation; they are scheduled for removal in a future major
//! release. The validated consumer layer built on this API is [`crate::memory`].
//!
//! # Public API contract
//!
//! ```text
//! Public API = Memory / Engine / Error at root
//!              anamnesis::memory  (Framework — Memory, Hit, Recall, SearchTuning, AddReceipt)
//!              anamnesis::engine  (Kernel   — Engine, EngineConfig, graph types, storage, embeddings)
//! ```

// ── Core engine types ─────────────────────────────────────────────────────────

pub use crate::api::{
    CommitReport, ConversationInput, ConversationResult, CrystallizeRequest, CrystallizeResult,
    DocumentInput, Engine, EngineConfig, ExtractedFact, GraphEvent, HealthGrade, HealthReport,
    IngestResult, Observation, PeerProfileInput, TickReport,
};

// ── Graph types ───────────────────────────────────────────────────────────────

pub use crate::graph::{
    AccessTrace, Edge, EdgeId, EdgeType, KnowledgeType, MemoryTier, Node, NodeId, Origin, PeerId,
    ScopePath, ScopeRelation, Timestamp,
};

// ── Query types ───────────────────────────────────────────────────────────────

pub use crate::query::{
    AccessedSite, ActivatedTension, ActivationResponse, CandidateSource, CandidateTrace,
    CoReadoutPair, CommitTrace, ContextPackage, ConvergenceConfig, Fragment, FusedCandidate,
    GraphRecallTrace, PackagingMode, PathCurrentMap, PathUsedEdge, Query, QueryConfig,
    ReadoutCandidate, SearchCandidate, SearchInput, SearchResult, SearchTrace, SearchTraceLevel,
    Tension, TokenBudget, additive_rwr, additive_rwr_with_alpha, scope_weight,
};

// ── Peer / trust types ────────────────────────────────────────────────────────

pub use crate::peer::{PeerProfile, PeerRegistry, SourceKind, TrustLevel};

// ── Observability / mechanics ─────────────────────────────────────────────────

pub use crate::mechanics::energy::{
    EnergyTerms, SiteBond, SiteEnergy, dirichlet_energy, energy as readout_energy,
};
pub use crate::mechanics::health::GraphHealth;
pub use crate::mechanics::observability::{
    InvariantCheck, InvariantReport, InvariantResult, OperationalWarning,
};
pub use crate::mechanics::social::{ConfidenceLevel, FeedbackSignal};

// ── Snapshot types ────────────────────────────────────────────────────────────

pub use crate::snapshot::{SnapshotBackend, SnapshotEntry, SnapshotId, SnapshotStore};

// ── Storage ───────────────────────────────────────────────────────────────────

pub use crate::storage::{SqliteStorage, StorageAdapter};

// ── Embedding ─────────────────────────────────────────────────────────────────

pub use crate::embedding::EmbeddingProvider;
#[cfg(feature = "embed")]
pub use crate::embedding::fastembed::FastEmbedProvider;
