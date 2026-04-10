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
pub mod error;
pub mod graph;
pub mod mechanics;
pub mod query;
pub mod storage;

// Core re-exports
pub use api::{
    Engine, EngineConfig, MergeLog, MergePair, Observation, ReflectReport, SessionSummary,
    TickReport,
};
pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};
pub use query::{ContextPackage, Fragment, Query, QueryConfig, Tension, TokenBudget};
pub use storage::{InMemoryStorage, StorageAdapter};
