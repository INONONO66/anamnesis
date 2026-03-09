//! Query engine: spreading activation and subgraph extraction

pub mod spread;

pub use spread::SpreadingActivation;

use crate::graph::NodeId;

/// Query configuration
#[derive(Clone, Debug)]
pub struct QueryConfig {
    /// Maximum number of nodes to return
    pub budget: usize,
    /// Activation decay per hop
    pub decay_per_hop: f64,
    /// Minimum activation threshold
    pub min_activation: f64,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            budget: 100,
            decay_per_hop: 0.8,
            min_activation: 0.01,
        }
    }
}

/// Result of a query
#[derive(Clone, Debug)]
pub struct QueryResult {
    /// Node IDs in the result subgraph
    pub nodes: Vec<NodeId>,
    /// Activation scores for each node
    pub activations: std::collections::HashMap<NodeId, f64>,
}
