//! Storage abstraction and implementations

pub mod memory;

pub use memory::InMemoryStorage;

use crate::graph::{Edge, Node, NodeId};

/// Result type for storage operations
pub type StorageResult<T> = Result<T, StorageError>;

/// Storage errors
#[derive(Clone, Debug)]
pub enum StorageError {
    /// Node not found
    NodeNotFound(NodeId),
    /// Edge not found
    EdgeNotFound(u64),
    /// Generic error
    Other(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::NodeNotFound(id) => write!(f, "Node not found: {}", id),
            StorageError::EdgeNotFound(id) => write!(f, "Edge not found: {}", id),
            StorageError::Other(msg) => write!(f, "Storage error: {}", msg),
        }
    }
}

impl std::error::Error for StorageError {}

/// Trait for pluggable storage backends
pub trait StorageAdapter: Send + Sync {
    /// Get a node by ID
    fn get_node(&self, id: NodeId) -> StorageResult<Node>;

    /// Set/update a node
    fn set_node(&mut self, id: NodeId, node: Node) -> StorageResult<()>;

    /// Delete a node
    fn delete_node(&mut self, id: NodeId) -> StorageResult<()>;

    /// Get an edge by ID
    fn get_edge(&self, id: u64) -> StorageResult<Edge>;

    /// Set/update an edge
    fn set_edge(&mut self, id: u64, edge: Edge) -> StorageResult<()>;

    /// Delete an edge
    fn delete_edge(&mut self, id: u64) -> StorageResult<()>;

    /// List all node IDs
    fn list_nodes(&self) -> StorageResult<Vec<NodeId>>;

    /// List all edge IDs
    fn list_edges(&self) -> StorageResult<Vec<u64>>;

    /// Get all nodes
    fn get_all_nodes(&self) -> StorageResult<Vec<(NodeId, Node)>>;

    /// Get all edges
    fn get_all_edges(&self) -> StorageResult<Vec<(u64, Edge)>>;
}
