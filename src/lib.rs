// Phase 1 skeleton — modules declared as they are built.

pub mod api;
pub mod error;
pub mod graph;
pub mod query;
pub mod storage;

pub use api::Engine;
pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};
pub use query::{ContextPackage, Fragment, Query, QueryConfig, Tension, TokenBudget};
pub use storage::{InMemoryStorage, StorageAdapter};
