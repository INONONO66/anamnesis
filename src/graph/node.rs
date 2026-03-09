//! Node type representing a concept or entity in the graph

use std::collections::HashMap;

/// Represents a concept or entity in the knowledge graph
#[derive(Clone, Debug)]
pub struct Node {
    /// Content or representation of the node
    pub content: String,
    /// Salience score (importance/recency)
    pub salience: f64,
    /// Creation timestamp
    pub created_at: u64,
    /// Last access timestamp
    pub accessed_at: u64,
    /// Node type (e.g., "concept", "entity", "event")
    pub node_type: String,
    /// Metadata key-value pairs
    pub metadata: HashMap<String, String>,
}

impl Node {
    /// Create a new node
    pub fn new(content: String, node_type: String) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            content,
            salience: 1.0,
            created_at: now,
            accessed_at: now,
            node_type,
            metadata: HashMap::new(),
        }
    }

    /// Update the salience score
    pub fn set_salience(&mut self, salience: f64) {
        self.salience = salience.max(0.0).min(1.0);
    }

    /// Touch the node (reinforce on access)
    pub fn touch(&mut self) {
        self.accessed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.salience = (self.salience + 0.1).min(1.0);
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
