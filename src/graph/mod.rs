//! Graph layer — core data structures for the Anamnesis cognitive graph.

pub mod edge;
pub mod node;
pub mod scope;
pub mod temporal;
pub mod types;

pub use edge::Edge;
pub use node::{Node, Origin};
pub use scope::{ScopePath, ScopeRelation};
pub use temporal::valid_at;
pub use types::{EdgeId, EdgeType, KnowledgeType, MemoryTier, NodeId, PeerId, Timestamp};

use crate::error::Error;
use crate::storage::{SqliteStorage, StorageAdapter};

/// The cognitive graph — a directed graph of knowledge fragments.
///
/// `Graph<S>` is generic over the storage backend. The default backend
/// is `SqliteStorage` (in-memory SQLite with write-behind hot fields).
///
/// All data lives in the storage backend. `Graph` provides typed
/// query methods on top of the raw storage interface.
pub struct Graph<S: StorageAdapter = SqliteStorage> {
    storage: S,
}

impl Graph<SqliteStorage> {
    /// Create a new graph with the default SQLite storage backend (in-memory).
    pub fn new() -> Self {
        Graph {
            storage: SqliteStorage::new().expect("failed to initialize in-memory SQLite storage"),
        }
    }
}

impl Default for Graph<SqliteStorage> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: StorageAdapter> Graph<S> {
    /// Create a graph with a custom storage backend.
    pub fn with_storage(storage: S) -> Self {
        Graph { storage }
    }

    /// Allocate a new NodeId from the storage backend.
    pub fn next_node_id(&mut self) -> NodeId {
        self.storage.next_node_id()
    }

    /// Allocate a new EdgeId from the storage backend.
    pub fn next_edge_id(&mut self) -> EdgeId {
        self.storage.next_edge_id()
    }

    /// Add a node to the graph. The node's id must be allocated via next_node_id().
    pub fn add_node(&mut self, node: Node) -> Result<NodeId, Error> {
        let id = node.id;
        self.storage.set_node(node)?;
        Ok(id)
    }

    /// Retrieve a node by ID.
    pub fn get_node(&self, id: NodeId) -> Result<&Node, Error> {
        self.storage.get_node(id)
    }

    /// Retrieve a mutable reference to a node.
    pub fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error> {
        self.storage.get_node_mut(id)
    }

    /// Remove a node from the graph. Also removes all edges connected to it.
    pub fn remove_node(&mut self, id: NodeId) -> Result<(), Error> {
        let out_edges: Vec<EdgeId> = self.storage.edges_from(id).to_vec();
        let in_edges: Vec<EdgeId> = self.storage.edges_to(id).to_vec();

        let mut seen = std::collections::HashSet::new();
        for eid in out_edges.iter().chain(in_edges.iter()) {
            if seen.insert(*eid) {
                self.storage.delete_edge(*eid)?;
            }
        }
        self.storage.delete_node(id)
    }

    /// Add an edge to the graph. The edge's id must be allocated via next_edge_id().
    pub fn add_edge(&mut self, edge: Edge) -> Result<EdgeId, Error> {
        let id = edge.id;
        self.storage.set_edge(edge)?;
        Ok(id)
    }

    /// Retrieve an edge by ID.
    pub fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error> {
        self.storage.get_edge(id)
    }

    /// Retrieve a mutable reference to an edge.
    pub fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error> {
        self.storage.get_edge_mut(id)
    }

    /// Remove an edge from the graph.
    pub fn remove_edge(&mut self, id: EdgeId) -> Result<(), Error> {
        self.storage.delete_edge(id)
    }

    /// Return all outgoing edge IDs from a node. O(degree).
    pub fn edges_from(&self, id: NodeId) -> &[EdgeId] {
        self.storage.edges_from(id)
    }

    /// Return all incoming edge IDs to a node. O(degree).
    pub fn edges_to(&self, id: NodeId) -> &[EdgeId] {
        self.storage.edges_to(id)
    }

    /// Number of live nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.storage.node_count()
    }

    /// Number of live edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.storage.edge_count()
    }

    /// All live node IDs.
    pub fn all_node_ids(&self) -> Vec<NodeId> {
        self.storage.all_node_ids()
    }

    /// All live edge IDs.
    pub fn all_edge_ids(&self) -> Vec<EdgeId> {
        self.storage.all_edge_ids()
    }

    /// Direct access to the storage backend (for mechanics that need raw access).
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Mutable access to the storage backend.
    pub fn storage_mut(&mut self) -> &mut S {
        &mut self.storage
    }

    /// Replace the storage backend wholesale.
    pub fn replace_storage(&mut self, storage: S) {
        self.storage = storage;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Origin;
    use std::collections::{HashMap, VecDeque};

    fn make_node(id: u64, name: &str) -> Node {
        Node {
            id: NodeId(id),
            node_type: KnowledgeType::Semantic,
            name: name.to_string(),
            summary: None,
            content: "test content".to_string(),
            embedding: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            accessed_at: Timestamp(0),
            valid_from: None,
            valid_until: None,
            salience: 0.8,
            retained_action: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::peer::SourceKind::AgentObservation,
                session_id: "session-1".to_string(),
                scope: crate::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: vec![],
            metadata: HashMap::new(),
        }
    }

    fn make_edge(id: u64, source: u64, target: u64) -> Edge {
        Edge {
            id: EdgeId(id),
            source: NodeId(source),
            target: NodeId(target),
            edge_type: EdgeType::Semantic,
            weight: 0.8,
            conductance: 0.0,
            edge_source: crate::graph::edge::EdgeSource::Auto,
            created_at: Timestamp(0),
            accessed_at: Timestamp(0),
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn new_graph_is_empty() {
        let g = Graph::new();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn add_and_get_node() {
        let mut g = Graph::new();
        let id = g.next_node_id();
        let node = make_node(id.0, "test node");
        g.add_node(node).unwrap();
        assert_eq!(g.node_count(), 1);
        let retrieved = g.get_node(id).unwrap();
        assert_eq!(retrieved.name, "test node");
    }

    #[test]
    fn add_and_get_edge() {
        let mut g = Graph::new();
        let id1 = g.next_node_id();
        let id2 = g.next_node_id();
        g.add_node(make_node(id1.0, "node A")).unwrap();
        g.add_node(make_node(id2.0, "node B")).unwrap();

        let eid = g.next_edge_id();
        g.add_edge(make_edge(eid.0, id1.0, id2.0)).unwrap();
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.edges_from(id1).len(), 1);
        assert_eq!(g.edges_to(id2).len(), 1);
    }

    #[test]
    fn remove_node_also_removes_edges() {
        let mut g = Graph::new();
        let id1 = g.next_node_id();
        let id2 = g.next_node_id();
        g.add_node(make_node(id1.0, "A")).unwrap();
        g.add_node(make_node(id2.0, "B")).unwrap();
        let eid = g.next_edge_id();
        g.add_edge(make_edge(eid.0, id1.0, id2.0)).unwrap();

        g.remove_node(id1).unwrap();
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn remove_edge() {
        let mut g = Graph::new();
        let id1 = g.next_node_id();
        let id2 = g.next_node_id();
        g.add_node(make_node(id1.0, "A")).unwrap();
        g.add_node(make_node(id2.0, "B")).unwrap();
        let eid = g.next_edge_id();
        g.add_edge(make_edge(eid.0, id1.0, id2.0)).unwrap();

        g.remove_edge(eid).unwrap();
        assert_eq!(g.edge_count(), 0);
        assert_eq!(g.edges_from(id1).len(), 0);
    }

    #[test]
    fn remove_node_with_self_loop() {
        let mut g = Graph::new();
        let id = g.next_node_id();
        g.add_node(make_node(id.0, "self-ref")).unwrap();
        let eid = g.next_edge_id();
        g.add_edge(make_edge(eid.0, id.0, id.0)).unwrap();
        g.remove_node(id).unwrap();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn get_node_not_found() {
        let g = Graph::new();
        let result = g.get_node(NodeId(999));
        assert!(matches!(result, Err(Error::NodeNotFound(_))));
    }

    #[test]
    fn default_graph() {
        let g = Graph::default();
        assert_eq!(g.node_count(), 0);
    }
}
