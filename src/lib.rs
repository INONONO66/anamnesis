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
pub mod query;
pub mod snapshot;
pub mod storage;

// Core re-exports
pub use api::{
    CrystallizeRequest, CrystallizeResult, DecayModel, EnergyModel, Engine, EngineConfig,
    IngestResult, MergeLog, MergePair, Observation, ReflectReport, SessionSummary, SpreadingModel,
    TickReport,
};
pub use embedding::EmbeddingProvider;
pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};
pub use query::{
    ContextPackage, Fragment, PackagingMode, Query, QueryConfig, SearchInput, SearchPlan,
    SearchResult, SearchTrace, Tension, TokenBudget, decompose_query,
};
pub use snapshot::{InMemorySnapshot, SnapshotBackend, SnapshotEntry, SnapshotId, SnapshotStore};
pub use storage::{InMemoryStorage, StorageAdapter};
