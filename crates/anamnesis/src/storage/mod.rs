//! Storage abstraction for the Anamnesis graph engine.
//!
//! The `StorageAdapter` trait defines the interface for all storage backends.
//! `SqliteStorage` is the default implementation, using an in-memory SQLite
//! database with write-behind dirty tracking for hot fields and FTS5 text search.

pub mod sqlite;

pub use sqlite::SqliteStorage;

use crate::error::Error;
use crate::graph::{
    AccessTrace, Edge, EdgeId, KnowledgeType, Node, NodeId, PeerId, ScopePath, Timestamp,
};
use std::collections::VecDeque;

/// Storage backend interface for the Anamnesis graph engine.
///
/// Implementations must provide O(1) amortized node/edge access.
/// The `SqliteStorage` implementation uses an in-memory SQLite database
/// with cached graph objects and SoA hot fields for fast spreading activation.
///
/// # Node strength substrate
///
/// Persistent node strength is `A_i = B_i + P_i` (ADR-0008). The base level `B_i`
/// is the multi-trace ACT-R activation `ln(Σ_j (now − at_j)^(−d_j))` over the
/// node's bounded 32-trace `access_history`, where each trace carries its own
/// activation-dependent decay `d_j` (Pavlik & Anderson 2005); it is computed on
/// demand and is NOT a stored field, so the trait exposes no `B_i` setter. The
/// persistent substrate is the access-history window (a committed access appends a
/// now-stamped [`AccessTrace`] via
/// [`append_access_trace`](StorageAdapter::append_access_trace)) plus the
/// decay-EXEMPT evidence prior `P_i`
/// ([`get_evidence_prior`](StorageAdapter::get_evidence_prior) /
/// [`set_evidence_prior`](StorageAdapter::set_evidence_prior)). `retained_action`
/// and `salience` are CACHED projections of the composite, refreshed only by
/// commit/touch/tick; read-only access returns the cache unchanged.
///
/// # Decay checkpoint (obsolete)
///
/// `decay_checkpoint` is retained only for snapshot/back-compat (the `v2 -> v3`
/// migration introduced it). Under recompute-from-history it is no longer
/// load-bearing for memory strength: `B_i` ages every trace to `now` directly, so
/// no "as-of" baseline is needed. Engine maintenance no longer reads or advances it.
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
    /// OBSOLETE under the base-level model: retained for snapshot/back-compat only
    /// (see the trait-level "Decay checkpoint (obsolete)" docs). It is no longer a
    /// load-bearing input to memory strength — `B_i` ages traces to `now` directly.
    fn get_decay_checkpoint(&self, id: NodeId) -> Result<Timestamp, Error>;
    /// Set the decay checkpoint timestamp for a node.
    ///
    /// OBSOLETE: kept for snapshot/back-compat parity. Engine maintenance no longer
    /// advances it.
    fn set_decay_checkpoint(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;

    // ── Base-level substrate: access-trace history (B_i) ──────────────────────
    //
    // B_i = ln(Σ_j (now − at_j)^(−d_j)) is computed on demand from these traces
    // ([`crate::mechanics::forgetting::compute_base_level`]); it is not a stored
    // scalar. Each [`AccessTrace`] carries its own activation-dependent decay `d_j`
    // (Pavlik & Anderson 2005). A committed access appends a now-stamped trace whose
    // `d_j` was computed from the existing history
    // ([`crate::mechanics::forgetting::compute_trace_decay`]), evicting the oldest
    // beyond the bounded 32-trace window, raising B_i.

    /// Get the node's bounded access-trace history (the substrate of `B_i`).
    fn get_access_history(&self, id: NodeId) -> Result<&VecDeque<AccessTrace>, Error>;

    /// Append an access trace, maintaining the bounded 32-trace window, and durably
    /// persist it. Called only from commit/touch (a committed access). The trace's
    /// `decay` must already be computed from the pre-append history
    /// ([`crate::mechanics::forgetting::compute_trace_decay`]).
    fn append_access_trace(&mut self, id: NodeId, trace: AccessTrace) -> Result<(), Error>;

    // ── Persistent reservoirs (decay-exempt evidence prior P_i, conductance) ──
    //
    // `P_i` (`evidence_prior`) is the persistent, decay-exempt log-odds offset
    // holding encoding surprise, feedback / social reinforcement, and peer trust
    // (ADR-0008). `conductance` `C_ij` is the edge associative reservoir; `weight`
    // is its bounded projection. The setters recompute the projection inside the
    // setter (the ADR "commit recomputes projections" step).

    /// Get the evidence prior `P_i` (decay-exempt log-odds offset) for a node.
    fn get_evidence_prior(&self, id: NodeId) -> Result<f64, Error>;

    /// Set the evidence prior `P_i` for a node. Called only from
    /// ingest/feedback/commit; the engine refreshes the `salience`/`retained_action`
    /// cache from `B_i(now) + P_i` afterwards.
    fn set_evidence_prior(&mut self, id: NodeId, prior: f64) -> Result<(), Error>;

    /// Get the cached composite retained action `A_i = B_i + P_i` for a node.
    ///
    /// This returns the CACHED snapshot last written by commit/touch/tick (it is not
    /// recomputed on read), so read-only query/search return a stable value.
    fn get_retained_action(&self, id: NodeId) -> Result<f64, Error>;

    /// Refresh the cached composite retained action `A_i` for a node and recompute
    /// the `salience` projection (`salience = project_salience(value)`). Called only
    /// from commit/touch/tick with the freshly recomputed `B_i(now) + P_i`.
    fn set_retained_action(&mut self, id: NodeId, value: f64) -> Result<(), Error>;

    /// Get the conductance `C_ij` (log-likelihood-ratio reservoir) for an edge.
    fn get_conductance(&self, id: EdgeId) -> Result<f64, Error>;

    /// Set the conductance `C_ij` for an edge and recompute the `weight`
    /// projection (`weight = project_weight(value)`). Called only from
    /// commit/tick.
    fn set_conductance(&mut self, id: EdgeId, value: f64) -> Result<(), Error>;

    /// Get the last-accessed timestamp for an edge. O(1) direct array access.
    fn get_edge_accessed_at(&self, id: EdgeId) -> Result<Timestamp, Error>;

    /// Set the last-accessed timestamp for an edge.
    ///
    /// A committed use is, by definition, not idle: implementations also reset
    /// the edge's `leaked_at` checkpoint to the same instant, clearing any
    /// outstanding idle-leak debt (the two fields stay distinct; this keeps
    /// them synchronized on every "use" event, per interactions.md).
    fn set_edge_accessed_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error>;

    /// Get the per-edge leak checkpoint — the last `now` idle-edge leakage was
    /// actually charged from (see [`Edge::leaked_at`]). O(1) direct array access.
    fn get_edge_leaked_at(&self, id: EdgeId) -> Result<Timestamp, Error>;

    /// Set the leak checkpoint for an edge. Called by `Engine::tick` after a
    /// successful leak, so a fixed idle window is charged once regardless of
    /// how many times `tick` runs at the same `now`.
    fn set_edge_leaked_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error>;

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

    /// Return up to `limit` live node IDs sorted by ID descending.
    ///
    /// Default delegates to `node_ids_descending()` + truncate.
    /// Override for O(limit) instead of O(N log N) when only a small
    /// prefix of the descending list is needed (e.g. ingest trigger pool).
    fn node_ids_descending_limit(&self, limit: usize) -> Vec<NodeId> {
        let mut ids = self.node_ids_descending();
        ids.truncate(limit);
        ids
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
