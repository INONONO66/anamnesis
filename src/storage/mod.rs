//! Storage abstraction for the Anamnesis graph engine.
//!
//! The `StorageAdapter` trait defines the interface for all storage backends.
//! `InMemoryStorage` is the default implementation using arena-based storage
//! with Struct of Arrays (SoA) hot fields for cache-friendly physics operations.

pub mod memory;

pub use memory::InMemoryStorage;

use crate::error::Error;
use crate::graph::{Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};

/// Storage backend interface for the Anamnesis graph engine.
///
/// Implementations must provide O(1) amortized node/edge access.
/// The `InMemoryStorage` implementation uses arena-based Vec storage
/// with SoA hot fields for sub-millisecond spreading activation.
pub trait StorageAdapter: Send + Sync {
    // ID allocation
    /// Allocate the next available NodeId (reuses freed IDs when available).
    fn next_node_id(&mut self) -> NodeId;
    /// Allocate the next available EdgeId (reuses freed IDs when available).
    fn next_edge_id(&mut self) -> EdgeId;

    // Node CRUD
    /// Store a node. The node's id must have been allocated via next_node_id().
    fn set_node(&mut self, node: Node) -> Result<(), Error>;
    /// Retrieve a node by ID.
    fn get_node(&self, id: NodeId) -> Result<&Node, Error>;
    /// Retrieve a mutable reference to a node.
    ///
    /// # SoA Invariant
    /// Mutations to `salience`, `accessed_at`, or `node_type` through this reference
    /// will NOT be reflected in the SoA hot-field arrays. Use `set_salience()`,
    /// `set_accessed_at()` instead for those fields. Only use `get_node_mut()` for
    /// non-hot fields like `name`, `content`, `access_count`, `metadata`, etc.
    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error>;
    /// Delete a node. Frees the ID for reuse. Caller must remove edges first.
    fn delete_node(&mut self, id: NodeId) -> Result<(), Error>;

    // Edge CRUD
    /// Store an edge. The edge's id must have been allocated via next_edge_id().
    fn set_edge(&mut self, edge: Edge) -> Result<(), Error>;
    /// Retrieve an edge by ID.
    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error>;
    /// Delete an edge. Frees the ID for reuse. Updates adjacency index.
    fn delete_edge(&mut self, id: EdgeId) -> Result<(), Error>;

    // Adjacency (O(degree) — backed by adjacency index)
    /// Return all outgoing edge IDs from a node.
    fn edges_from(&self, id: NodeId) -> &[EdgeId];
    /// Return all incoming edge IDs to a node.
    fn edges_to(&self, id: NodeId) -> &[EdgeId];

    // Hot field access (SoA — cache-friendly for physics iteration)
    /// Get salience for a node. O(1) direct array access.
    fn get_salience(&self, id: NodeId) -> Result<f64, Error>;
    /// Set salience for a node. Keeps SoA in sync with Node.salience.
    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error>;
    /// Get accessed_at for a node. O(1) direct array access.
    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error>;
    /// Set accessed_at for a node. Keeps SoA in sync with Node.accessed_at.
    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
    /// Get node type for a node. O(1) direct array access.
    fn get_node_type(&self, id: NodeId) -> Result<&KnowledgeType, Error>;

    // Counts and iteration
    /// Number of live nodes (excludes deleted slots).
    fn node_count(&self) -> usize;
    /// Number of live edges (excludes deleted slots).
    fn edge_count(&self) -> usize;
    /// All live node IDs.
    fn all_node_ids(&self) -> Vec<NodeId>;
    /// All live edge IDs.
    fn all_edge_ids(&self) -> Vec<EdgeId>;
}
