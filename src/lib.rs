//! Anamnesis — cognitive graph engine for LLM agents.
//!
//! Knowledge with attraction, gravity, perception, and forgetting.
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
pub mod peer;
pub mod query;
pub mod snapshot;
pub mod storage;

// Core re-exports
pub use api::{
    CrystallizeRequest, CrystallizeResult, DebugOutcome, EnergyModel, Engine, EngineConfig,
    EvidenceResult, IngestResult, MergeLog, MergePair, Observation, ObservedRef, PerspectiveKey,
    ReflectReport, SessionSummary, SpreadingModel, TickReport,
};
pub use embedding::EmbeddingProvider;
#[cfg(feature = "embed")]
pub use embedding::fastembed::FastEmbedProvider;
pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, PeerId, Timestamp};
pub use mechanics::health::GraphHealth;
pub use mechanics::social::FeedbackSignal;
pub use peer::{PeerProfile, PeerRegistry, SourceKind, TrustLevel};
pub use query::{
    ContextPackage, Fragment, PackagingMode, Query, QueryConfig, SearchInput, SearchResult,
    SearchTrace, Tension, TokenBudget,
};
pub use snapshot::{SnapshotBackend, SnapshotEntry, SnapshotId, SnapshotStore};
pub use storage::{SqliteStorage, StorageAdapter};
