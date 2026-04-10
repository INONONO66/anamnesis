//! Query types for the Anamnesis cognitive graph engine.

use crate::graph::Origin;
use crate::graph::{KnowledgeType, NodeId, Timestamp};

/// Query modes for different retrieval patterns.
///
/// Each mode corresponds to a different agent retrieval need.
/// All modes return a `ContextPackage` via the full query pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// Associative retrieval: spreading activation from a seed node.
    /// "What's related to X?"
    Associative { seed: NodeId, budget: usize },

    /// Type-filtered retrieval: all nodes of a given type, ordered by salience.
    /// "Show me all conventions" or "show me all gotchas"
    TypeFiltered {
        node_type: KnowledgeType,
        limit: usize,
    },

    /// Neighborhood retrieval: entity node + k-hop subgraph.
    /// "Everything about the auth module"
    Neighborhood { entity: NodeId, depth: usize },

    /// Temporal retrieval: nodes created/updated since a timestamp.
    /// "What changed recently?"
    Temporal {
        since: Timestamp,
        node_types: Option<Vec<KnowledgeType>>,
        limit: usize,
    },

    /// List retrieval: all nodes above a salience threshold.
    /// "What do I know?" (session start)
    List { min_salience: f64, limit: usize },
}

/// Configuration for a query execution.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryConfig {
    /// Maximum number of nodes to visit during spreading activation.
    pub budget: usize,
    /// Activation decay per hop [0, 1]. Lower = faster decay.
    pub decay_per_hop: f64,
    /// Minimum activation to continue spreading. Below this, traversal stops.
    pub min_activation: f64,
    /// Maximum hops from seed before stopping.
    pub max_hops: usize,
    /// Agent ID for identity prior computation. None = no identity bias.
    pub agent_id: Option<String>,
    /// Token budget for ContextPackage assembly.
    pub token_budget: usize,
}

impl Default for QueryConfig {
    fn default() -> Self {
        QueryConfig {
            budget: 500,
            decay_per_hop: 0.65,
            min_activation: 0.02,
            max_hops: 6,
            agent_id: None,
            token_budget: 4000,
        }
    }
}

/// A single knowledge fragment in a query result.
///
/// Carries multi-resolution content (L0/L1/L2) based on token budget.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    /// The node this fragment represents.
    pub node_id: NodeId,
    /// L0: One-liner label. Always present.
    pub name: String,
    /// L1: Summary. Present when budget allows.
    pub summary: Option<String>,
    /// L2: Full content. Present only for top-ranked nodes.
    pub content: Option<String>,
    /// Knowledge type of the source node.
    pub node_type: KnowledgeType,
    /// Final relevance score R_i from the query pipeline [0, 1].
    pub relevance: f64,
    /// Provenance of the source node.
    pub origin: Origin,
}

/// An active contradiction between two nodes.
///
/// Surfaced when a Contradicts edge connects two activated nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Tension {
    /// First node in the contradiction.
    pub node_a: NodeId,
    /// Second node in the contradiction.
    pub node_b: NodeId,
    /// Weight of the Contradicts edge [0, 1].
    pub edge_weight: f64,
    /// Optional human-readable description of the contradiction.
    pub description: Option<String>,
}

/// Token usage breakdown for a ContextPackage.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TokenBudget {
    /// Total token budget for this query.
    pub total: usize,
    /// Tokens used so far.
    pub used: usize,
    /// Tokens used by identity fragments.
    pub identity_used: usize,
    /// Tokens used by knowledge fragments.
    pub knowledge_used: usize,
    /// Tokens used by memory fragments.
    pub memories_used: usize,
}

impl TokenBudget {
    pub fn new(total: usize) -> Self {
        TokenBudget {
            total,
            ..Default::default()
        }
    }

    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.used)
    }
}

/// Structured query output ready for LLM injection.
///
/// Partitions results into identity/knowledge/memories/tensions
/// so consumers can map directly to prompt structure:
/// - identity → system prompt
/// - knowledge → context block
/// - memories → evidence block
/// - tensions → warning block
#[derive(Debug, Clone, PartialEq)]
pub struct ContextPackage {
    /// Agent persona traits (IdentityCore, IdentityLearned, IdentityState nodes).
    pub identity: Vec<Fragment>,
    /// Query-relevant knowledge (Semantic, Decision, Convention, etc. nodes).
    pub knowledge: Vec<Fragment>,
    /// Supporting episodic evidence (Episodic, Event nodes).
    pub memories: Vec<Fragment>,
    /// Active contradictions between retrieved nodes.
    pub tensions: Vec<Tension>,
    /// Token usage breakdown.
    pub token_usage: TokenBudget,
    /// Overall tension score T_agent [0, 1]. High = identity conflicts with retrieved knowledge.
    pub agent_tension: f64,
}

impl ContextPackage {
    /// Create an empty ContextPackage (placeholder for unimplemented query).
    pub fn empty() -> Self {
        ContextPackage {
            identity: vec![],
            knowledge: vec![],
            memories: vec![],
            tensions: vec![],
            token_usage: TokenBudget::default(),
            agent_tension: 0.0,
        }
    }

    /// Total number of fragments across all categories.
    pub fn total_fragments(&self) -> usize {
        self.identity.len() + self.knowledge.len() + self.memories.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Origin;
    use crate::graph::{KnowledgeType, NodeId, Timestamp};

    fn make_origin() -> Origin {
        Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn all_query_variants_constructable() {
        let queries = vec![
            Query::Associative {
                seed: NodeId(1),
                budget: 100,
            },
            Query::TypeFiltered {
                node_type: KnowledgeType::Convention,
                limit: 10,
            },
            Query::Neighborhood {
                entity: NodeId(2),
                depth: 3,
            },
            Query::Temporal {
                since: Timestamp(1000),
                node_types: Some(vec![KnowledgeType::Decision]),
                limit: 20,
            },
            Query::List {
                min_salience: 0.5,
                limit: 50,
            },
        ];
        assert_eq!(queries.len(), 5);
    }

    #[test]
    fn query_config_default() {
        let config = QueryConfig::default();
        assert_eq!(config.budget, 500);
        assert_eq!(config.decay_per_hop, 0.65);
        assert_eq!(config.min_activation, 0.02);
        assert!(config.agent_id.is_none());
    }

    #[test]
    fn context_package_empty() {
        let pkg = ContextPackage::empty();
        assert_eq!(pkg.total_fragments(), 0);
        assert_eq!(pkg.agent_tension, 0.0);
        assert!(pkg.identity.is_empty());
        assert!(pkg.tensions.is_empty());
    }

    #[test]
    fn token_budget_remaining() {
        let mut budget = TokenBudget::new(4000);
        budget.used = 1500;
        assert_eq!(budget.remaining(), 2500);
    }

    #[test]
    fn token_budget_saturating_sub() {
        let mut budget = TokenBudget::new(100);
        budget.used = 200; // over budget
        assert_eq!(budget.remaining(), 0); // saturating_sub prevents underflow
    }

    #[test]
    fn fragment_construction() {
        let frag = Fragment {
            node_id: NodeId(1),
            name: "auth uses factory pattern".to_string(),
            summary: Some("Confirmed in sessions 5, 12, 23".to_string()),
            content: None,
            node_type: KnowledgeType::Convention,
            relevance: 0.85,
            origin: make_origin(),
        };
        assert_eq!(frag.relevance, 0.85);
        assert!(frag.content.is_none());
    }

    #[test]
    fn tension_construction() {
        let tension = Tension {
            node_a: NodeId(1),
            node_b: NodeId(2),
            edge_weight: 0.9,
            description: Some("factory pattern vs DI refactor".to_string()),
        };
        assert_eq!(tension.edge_weight, 0.9);
    }
}
