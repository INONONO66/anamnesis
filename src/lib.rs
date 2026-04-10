// Phase 1 skeleton — modules declared as they are built.

pub mod error;
pub mod graph;
pub mod storage;

pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};
pub use storage::{InMemoryStorage, StorageAdapter};
