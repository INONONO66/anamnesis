//! Edge type representing relationships between nodes

use std::collections::HashMap;

/// Types of edges in the graph
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EdgeType {
    /// Semantic relationship
    Semantic,
    /// Causal relationship
    Causal,
    /// Temporal relationship
    Temporal,
    /// Custom edge type
    Custom(String),
}

/// Represents a relationship between two nodes
#[derive(Clone, Debug)]
pub struct Edge {
    /// Source node ID
    pub source: u64,
    /// Target node ID
    pub target: u64,
    /// Type of edge
    pub edge_type: EdgeType,
    /// Weight/strength of the relationship
    pub weight: f64,
    /// Creation timestamp
    pub created_at: u64,
    /// Metadata key-value pairs
    pub metadata: HashMap<String, String>,
}

impl Edge {
    /// Create a new edge
    pub fn new(source: u64, target: u64, edge_type: EdgeType, weight: f64) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            source,
            target,
            edge_type,
            weight: weight.clamp(0.0, 1.0),
            created_at: now,
            metadata: HashMap::new(),
        }
    }

    /// Update the weight
    pub fn set_weight(&mut self, weight: f64) {
        self.weight = weight.clamp(0.0, 1.0);
    }

    /// Add metadata
    pub fn set_metadata(&mut self, key: String, value: String) {
        self.metadata.insert(key, value);
    }

    /// Get metadata
    pub fn get_metadata(&self, key: &str) -> Option<&String> {
        self.metadata.get(key)
    }
}
