//! In-memory storage implementation

use std::collections::HashMap;

use crate::graph::{Edge, Node, NodeId};

use super::{StorageAdapter, StorageError, StorageResult};

/// In-memory storage backend
#[derive(Clone, Debug)]
pub struct InMemoryStorage {
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<u64, Edge>,
}

impl InMemoryStorage {
    /// Create a new in-memory storage
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageAdapter for InMemoryStorage {
    fn get_node(&self, id: NodeId) -> StorageResult<Node> {
        self.nodes
            .get(&id)
            .cloned()
            .ok_or(StorageError::NodeNotFound(id))
    }

    fn set_node(&mut self, id: NodeId, node: Node) -> StorageResult<()> {
        self.nodes.insert(id, node);
        Ok(())
    }

    fn delete_node(&mut self, id: NodeId) -> StorageResult<()> {
        self.nodes.remove(&id).ok_or(StorageError::NodeNotFound(id))?;
        Ok(())
    }

    fn get_edge(&self, id: u64) -> StorageResult<Edge> {
        self.edges
            .get(&id)
            .cloned()
            .ok_or(StorageError::EdgeNotFound(id))
    }

    fn set_edge(&mut self, id: u64, edge: Edge) -> StorageResult<()> {
        self.edges.insert(id, edge);
        Ok(())
    }

    fn delete_edge(&mut self, id: u64) -> StorageResult<()> {
        self.edges.remove(&id).ok_or(StorageError::EdgeNotFound(id))?;
        Ok(())
    }

    fn list_nodes(&self) -> StorageResult<Vec<NodeId>> {
        Ok(self.nodes.keys().copied().collect())
    }

    fn list_edges(&self) -> StorageResult<Vec<u64>> {
        Ok(self.edges.keys().copied().collect())
    }

    fn get_all_nodes(&self) -> StorageResult<Vec<(NodeId, Node)>> {
        Ok(self
            .nodes
            .iter()
            .map(|(id, node)| (*id, node.clone()))
            .collect())
    }

    fn get_all_edges(&self) -> StorageResult<Vec<(u64, Edge)>> {
        Ok(self
            .edges
            .iter()
            .map(|(id, edge)| (*id, edge.clone()))
            .collect())
    }
}
