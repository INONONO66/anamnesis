// Phase 1 skeleton — modules declared as they are built.

pub mod error;
pub mod graph;

pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};
