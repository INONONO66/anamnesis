//! Anamnesis — cognitive graph engine for LLM agents.
//!
//! Knowledge with spreading activation, conductance, perception, and forgetting.
//!
//! # Quick Start
//!
//! ```rust
//! use anamnesis::{Engine, EngineConfig};
//! use anamnesis::{KnowledgeType, EdgeType};
//! use anamnesis::api::Observation;
//! use anamnesis::graph::node::Origin;
//! use anamnesis::graph::Timestamp;
//!
//! let mut engine = Engine::new();
//! ```

pub mod api;
pub mod embedding;
pub mod error;
pub mod graph;
pub mod mechanics;
pub mod memory;
pub mod peer;
pub mod query;
pub mod snapshot;
pub mod storage;

// Framework API — the validated consumer layer.
pub use memory::AddReceipt;
pub use memory::Hit;
pub use memory::Memory;
pub use memory::Recall;
pub use memory::SearchTuning;

// Core re-exports
pub use api::{
    CrystallizeRequest, CrystallizeResult, DebugOutcome, Engine, EngineConfig, EvidenceResult,
    IngestResult, Observation, ObservedRef, PerspectiveKey, ReflectReport, SessionSummary,
    TickReport,
};
pub use embedding::EmbeddingProvider;
#[cfg(feature = "embed")]
pub use embedding::fastembed::FastEmbedProvider;
pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, PeerId, Timestamp};
pub use mechanics::energy::{
    EnergyTerms, SiteBond, SiteEnergy, dirichlet_energy, energy as readout_energy,
};
pub use mechanics::health::GraphHealth;
pub use mechanics::observability::{
    InvariantCheck, InvariantReport, InvariantResult, OperationalWarning,
};
pub use mechanics::social::{ConfidenceLevel, FeedbackSignal};
pub use peer::{PeerProfile, PeerRegistry, SourceKind, TrustLevel};
pub use query::{
    AccessedSite, ActivatedTension, CoReadoutPair, CommitTrace, ContextPackage, Fragment,
    PackagingMode, PathUsedEdge, Query, QueryConfig, SearchInput, SearchResult, SearchTrace,
    Tension, TokenBudget,
};
pub use snapshot::{SnapshotBackend, SnapshotEntry, SnapshotId, SnapshotStore};
pub use storage::{SqliteStorage, StorageAdapter};
