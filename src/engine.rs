//! Kernel API — the raw substrate, gathered in one namespace.
//!
//! `anamnesis::engine` re-exports the kernel surface; the original module
//! paths (`crate::api`, `crate::query`, `crate::graph`, …) remain valid.
//! The validated consumer layer built on this API is [`crate::memory`].
pub use crate::api::{CommitReport, Engine, EngineConfig, IngestResult, Observation, TickReport};
pub use crate::embedding::EmbeddingProvider;
pub use crate::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
pub use crate::mechanics::social::ConfidenceLevel;
pub use crate::query::{
    ContextPackage, Fragment, PackagingMode, Query, QueryConfig, ReadoutCandidate, SearchInput,
    SearchResult, SearchTrace,
};
pub use crate::storage::{SqliteStorage, StorageAdapter};
