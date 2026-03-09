//! Public API surface

use crate::graph::edge::EdgeType;
use crate::graph::{Edge, Graph, Node, NodeId};
use crate::storage::{InMemoryStorage, StorageAdapter};

/// Engine configuration
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Maximum number of nodes
    pub max_nodes: usize,
    /// Novelty threshold for ingestion
    pub novelty_threshold: f64,
    /// Confidence threshold for ingestion
    pub confidence_threshold: f64,
    /// Decay rate for forgetting
    pub decay_rate: f64,
    /// Use exponential decay (vs polynomial)
    pub use_exponential_decay: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_nodes: 10000,
            novelty_threshold: 0.3,
            confidence_threshold: 0.5,
            decay_rate: 0.1,
            use_exponential_decay: true,
        }
    }
}

/// Observation to ingest into the graph
#[derive(Clone, Debug)]
pub struct Observation {
    /// Content of the observation
    pub content: String,
    /// Embedding representation
    pub embedding: Vec<f64>,
    /// Confidence score
    pub confidence: f64,
    /// Node type
    pub node_type: String,
}

/// The main engine
pub struct Engine {
    graph: Graph,
    #[allow(dead_code)]
    config: EngineConfig,
    storage: Box<dyn StorageAdapter>,
}

impl Engine {
    /// Create a new engine with default configuration
    pub fn new() -> Self {
        Self::with_config(EngineConfig::default())
    }

    /// Create a new engine with custom configuration
    pub fn with_config(config: EngineConfig) -> Self {
        Self {
            graph: Graph::new(),
            config,
            storage: Box::new(InMemoryStorage::new()),
        }
    }

    /// Create a new engine with custom storage
    pub fn with_storage(config: EngineConfig, storage: Box<dyn StorageAdapter>) -> Self {
        Self {
            graph: Graph::new(),
            config,
            storage,
        }
    }

    /// Ingest an observation into the graph
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, String> {
        let node = Node::new(observation.content, observation.node_type);
        let node_id = self.graph.add_node(node);

        self.storage
            .set_node(node_id, self.graph.get_node(node_id).unwrap().clone())
            .map_err(|e| e.to_string())?;

        Ok(vec![node_id])
    }

    /// Create a link between two nodes
    pub fn link(
        &mut self,
        from: NodeId,
        to: NodeId,
        edge_type: EdgeType,
        weight: f64,
    ) -> Result<u64, String> {
        let edge = Edge::new(from, to, edge_type, weight);
        let edge_id = self.graph.add_edge(edge.clone());

        self.storage
            .set_edge(edge_id, edge)
            .map_err(|e| e.to_string())?;

        Ok(edge_id)
    }

    /// Advance time — apply decay to all nodes
    pub fn tick(&mut self, _now: u64) -> Result<(), String> {
        // Placeholder for decay logic
        Ok(())
    }

    /// Query the graph
    pub fn query(&self, seed: NodeId, _budget: usize) -> Result<Vec<NodeId>, String> {
        // Placeholder for query logic
        Ok(vec![seed])
    }

    /// Touch a node to reinforce it
    pub fn touch(&mut self, node_id: NodeId) -> Result<(), String> {
        if let Some(node) = self.graph.get_node_mut(node_id) {
            node.touch();
            self.storage
                .set_node(node_id, node.clone())
                .map_err(|e| e.to_string())?;
            Ok(())
        } else {
            Err(format!("Node {} not found", node_id))
        }
    }

    /// Get merge candidates
    pub fn merge_candidates(&self, _threshold: f64) -> Result<Vec<(NodeId, NodeId)>, String> {
        // Placeholder for merge candidate logic
        Ok(Vec::new())
    }

    /// Execute auto-merge
    pub fn auto_merge(&mut self, _threshold: f64) -> Result<usize, String> {
        // Placeholder for auto-merge logic
        Ok(0)
    }

    /// Get the graph
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Get mutable graph
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
