//! Core graph types: Node, Edge, Graph

pub mod edge;
pub mod node;

pub use edge::Edge;
pub use node::Node;

use std::collections::HashMap;

/// Unique identifier for a node
pub type NodeId = u64;

/// Unique identifier for an edge
pub type EdgeId = u64;

/// The main graph structure
pub struct Graph {
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<EdgeId, Edge>,
    next_node_id: NodeId,
    next_edge_id: EdgeId,
}

impl Graph {
    /// Create a new empty graph
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
            next_node_id: 1,
            next_edge_id: 1,
        }
    }

    /// Get a node by ID
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Get a mutable node by ID
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// Add a node to the graph
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = self.next_node_id;
        self.next_node_id += 1;
        self.nodes.insert(id, node);
        id
    }

    /// Get an edge by ID
    pub fn get_edge(&self, id: EdgeId) -> Option<&Edge> {
        self.edges.get(&id)
    }

    /// Add an edge to the graph
    pub fn add_edge(&mut self, edge: Edge) -> EdgeId {
        let id = self.next_edge_id;
        self.next_edge_id += 1;
        self.edges.insert(id, edge);
        id
    }

    /// Get all nodes
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().map(|(id, node)| (*id, node))
    }

    /// Get all edges
    pub fn edges(&self) -> impl Iterator<Item = (EdgeId, &Edge)> {
        self.edges.iter().map(|(id, edge)| (*id, edge))
    }

    /// Get edges from a source node
    pub fn edges_from(&self, source: NodeId) -> Vec<(EdgeId, &Edge)> {
        self.edges
            .iter()
            .filter(|(_, edge)| edge.source == source)
            .map(|(id, edge)| (*id, edge))
            .collect()
    }

    /// Get edges to a target node
    pub fn edges_to(&self, target: NodeId) -> Vec<(EdgeId, &Edge)> {
        self.edges
            .iter()
            .filter(|(_, edge)| edge.target == target)
            .map(|(id, edge)| (*id, edge))
            .collect()
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}
