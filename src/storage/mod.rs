//! Storage abstraction for the Anamnesis graph engine.
//!
//! The `StorageAdapter` trait defines the interface for all storage backends.
//! `SqliteStorage` is the default implementation, using an in-memory SQLite
//! database with write-behind dirty tracking for hot fields and FTS5 text search.

pub mod sqlite;

pub use sqlite::SqliteStorage;

use crate::error::Error;
use crate::graph::{Edge, EdgeId, KnowledgeType, Node, NodeId, PeerId, ScopePath, Timestamp};

/// Storage backend interface for the Anamnesis graph engine.
///
/// Implementations must provide O(1) amortized node/edge access.
/// The `SqliteStorage` implementation uses an in-memory SQLite database
/// with cached graph objects and SoA hot fields for fast spreading activation.
///
/// # Decay Checkpoint Invariant
///
/// `decay_checkpoint` is an internal SoA hot field separate from `accessed_at`.
/// It records the last time decay was applied to a node and is the source of
/// truth for elapsed-time computation in lazy/batch decay.
///
/// After `set_node()` and `Engine::touch()`, `decay_checkpoint == accessed_at`.
/// Only `Engine::tick()` may diverge them: tick advances `decay_checkpoint`
/// while leaving `accessed_at` untouched (preserving last-access semantics).
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
    /// `set_accessed_at()` instead for those fields.
    ///
    /// # Index Invariant
    /// Mutations to `entity_tags`, `node_type`, `origin.agent_id`, or
    /// `origin.scope` will NOT update secondary indexes. To change these
    /// fields, call `set_node()` with the modified node so indexes are rebuilt.
    ///
    /// Safe to mutate: `name`, `summary`, `content`, `embedding`, `access_count`,
    /// `access_history`, `metadata`, `valid_from`, `valid_until`.
    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error>;
    /// Delete a node. Frees the ID for reuse. Caller must remove edges first.
    fn delete_node(&mut self, id: NodeId) -> Result<(), Error>;

    // Edge CRUD
    /// Store an edge. The edge's id must have been allocated via next_edge_id().
    fn set_edge(&mut self, edge: Edge) -> Result<(), Error>;
    /// Retrieve an edge by ID.
    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error>;
    /// Retrieve a mutable reference to an edge.
    ///
    /// # Adjacency Invariant
    /// Mutations to `source` or `target` through this reference will NOT update
    /// the adjacency index. Only mutate `weight`, `metadata`, or `edge_type`.
    /// To change source/target, delete the edge and create a new one.
    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error>;
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
    /// Get the decay checkpoint timestamp for a node. O(1) direct array access.
    ///
    /// The decay checkpoint records the last time decay was applied; it is the
    /// baseline for elapsed-time computation in lazy/batch decay. See trait-level
    /// "Decay Checkpoint Invariant" docs for ordering rules vs `accessed_at`.
    fn get_decay_checkpoint(&self, id: NodeId) -> Result<Timestamp, Error>;
    /// Set the decay checkpoint timestamp for a node.
    ///
    /// `Engine::touch()` and `set_node()` keep this equal to `accessed_at`;
    /// `Engine::tick()` advances it independently.
    fn set_decay_checkpoint(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
    /// Persist any buffered hot-field writes.
    ///
    /// Storage backends that write hot fields immediately can use this default no-op.
    /// Write-behind backends should override it and preserve dirty state on failure.
    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
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

    /// Return all node IDs that have the given entity tag.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_entity_tag(&self, tag: &str) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| {
                self.get_node(id)
                    .ok()
                    .is_some_and(|n| n.entity_tags.iter().any(|t| t == tag))
            })
            .collect()
    }

    /// Return all node IDs of the given knowledge type.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_type(&self, kt: &KnowledgeType) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| self.get_node_type(id).ok().is_some_and(|t| t == kt))
            .collect()
    }

    /// Return all node IDs created by the given peer.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_peer(&self, peer_id: PeerId) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| {
                self.get_node(id)
                    .ok()
                    .is_some_and(|n| n.origin.peer_id == peer_id)
            })
            .collect()
    }

    /// Return all node IDs whose origin scope equals the given scope path.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_scope(&self, scope: &ScopePath) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| {
                self.get_node(id)
                    .ok()
                    .is_some_and(|n| n.origin.scope == *scope)
            })
            .collect()
    }

    /// Return all live node IDs sorted by ID descending (most recently allocated first).
    ///
    /// Default implementation sorts the result of all_node_ids(): O(N log N). Override for O(1).
    fn node_ids_descending(&self) -> Vec<NodeId> {
        let mut ids = self.all_node_ids();
        ids.sort_by_key(|a| std::cmp::Reverse(a.0));
        ids
    }

    // ── Peer persistence (default no-ops; SqliteStorage overrides) ────────────

    /// Store a peer profile. Write-through: called on every peer mutation.
    ///
    /// Default is a no-op for backends that don't persist peers.
    fn store_peer(&mut self, _profile: &crate::peer::PeerProfile) -> Result<(), Error> {
        Ok(())
    }

    /// Store an alias for a peer.
    fn store_peer_alias(
        &mut self,
        _peer_id: PeerId,
        _alias: &str,
        _alias_type: &str,
    ) -> Result<(), Error> {
        Ok(())
    }

    /// Load all peers from storage into a `PeerRegistry`.
    ///
    /// Default returns an empty registry for backends that don't persist peers.
    fn load_peers(&self) -> Result<crate::peer::PeerRegistry, Error> {
        Ok(crate::peer::PeerRegistry::new())
    }

    /// Delete a peer and all its aliases from storage.
    fn delete_peer(&mut self, _peer_id: PeerId) -> Result<(), Error> {
        Ok(())
    }

    /// Search nodes by text query (case-insensitive substring match on name and content).
    ///
    /// Returns up to `limit` node IDs with their match scores.
    /// Default implementation scans all nodes: O(N). Override for full-text search index.
    ///
    /// # Arguments
    /// * `query` - Search string (case-insensitive)
    /// * `limit` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of (NodeId, score) tuples. Score is 1.0 for default impl (simple match).
    fn text_search(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)> {
        let query_lower = query.to_lowercase();
        self.all_node_ids()
            .into_iter()
            .filter_map(|id| {
                self.get_node(id).ok().and_then(|node| {
                    let name_match = node.name.to_lowercase().contains(&query_lower);
                    let content_match = node.content.to_lowercase().contains(&query_lower);
                    if name_match || content_match {
                        Some((id, 1.0))
                    } else {
                        None
                    }
                })
            })
            .take(limit)
            .collect()
    }
}
