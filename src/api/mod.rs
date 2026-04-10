//! Public API surface for the Anamnesis cognitive graph engine.

use std::collections::HashMap;

use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::{Edge, Graph, Node};
use crate::graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};
use crate::query::{ContextPackage, Query, QueryConfig};
use crate::storage::{InMemoryStorage, StorageAdapter};

/// Configuration for the Anamnesis engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Maximum number of nodes before perception gate rejects new observations.
    pub max_nodes: usize,
    /// Minimum novelty score [0, 1] for an observation to enter the graph.
    pub novelty_threshold: f64,
    /// Minimum confidence [0, 1] for an observation to enter the graph.
    pub confidence_threshold: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_nodes: 100_000,
            novelty_threshold: 0.30,
            confidence_threshold: 0.50,
        }
    }
}

impl EngineConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_nodes(mut self, max_nodes: usize) -> Self {
        self.max_nodes = max_nodes;
        self
    }

    pub fn with_novelty_threshold(mut self, threshold: f64) -> Self {
        self.novelty_threshold = threshold;
        self
    }

    pub fn with_confidence_threshold(mut self, threshold: f64) -> Self {
        self.confidence_threshold = threshold;
        self
    }
}

/// An observation to be ingested into the graph.
///
/// The consumer is responsible for providing embeddings and extracting
/// entity tags. The engine stores the observation as a Node.
#[derive(Debug, Clone)]
pub struct Observation {
    /// L0: One-liner label for the observation.
    pub name: String,
    /// L1: Optional summary (consumer-generated).
    pub summary: Option<String>,
    /// L2: Full content of the observation.
    pub content: String,
    /// Embedding vector (consumer-provided). Used for similarity operations.
    pub embedding: Option<Vec<f64>>,
    /// Creation-time confidence [0, 1].
    pub confidence: f64,
    /// Knowledge type of this observation.
    pub node_type: KnowledgeType,
    /// Entity tags for automatic cross-node linking.
    pub entity_tags: Vec<String>,
    /// Provenance of this observation.
    pub origin: Origin,
    /// When this observation occurred. Defaults to Timestamp(0) if not provided.
    pub timestamp: Timestamp,
}

/// Report returned by Engine::tick().
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    /// Number of nodes whose salience was updated.
    pub nodes_decayed: usize,
    /// Number of nodes pruned (salience reached 0).
    pub nodes_pruned: usize,
}

/// A pair of nodes that are candidates for merging.
#[derive(Debug, Clone)]
pub struct MergePair {
    pub node_a: NodeId,
    pub node_b: NodeId,
    /// Similarity score [0, 1].
    pub similarity: f64,
}

/// Log of merges performed by Engine::auto_merge().
#[derive(Debug, Clone, Default)]
pub struct MergeLog {
    /// Number of merges performed.
    pub merges_performed: usize,
    /// Node IDs that were merged (the surviving node ID).
    pub merged_into: Vec<NodeId>,
}

/// Summary of a completed agent session for reflect_batch().
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub agent_id: String,
    pub session_id: String,
    pub node_ids: Vec<NodeId>,
}

/// Report returned by Engine::reflect_batch().
#[derive(Debug, Clone, Default)]
pub struct ReflectReport {
    /// Number of Entity edges created between cross-agent nodes.
    pub entity_edges_created: usize,
    /// Number of entity clusters found.
    pub clusters_found: usize,
}

/// The Anamnesis cognitive graph engine.
///
/// `Engine<S>` is generic over the storage backend. The default is
/// `InMemoryStorage` (arena-based, sub-millisecond access).
///
/// Phase 1: All methods have correct signatures. `ingest`, `link`, and `touch`
/// perform real operations. Other methods return placeholder results.
pub struct Engine<S: StorageAdapter = InMemoryStorage> {
    graph: Graph<S>,
    #[allow(dead_code)] // Used in Phase 2 for perception gating
    config: EngineConfig,
}

impl Engine<InMemoryStorage> {
    /// Create a new engine with default configuration and in-memory storage.
    pub fn new() -> Self {
        Engine {
            graph: Graph::new(),
            config: EngineConfig::default(),
        }
    }

    /// Create a new engine with custom configuration.
    pub fn with_config(config: EngineConfig) -> Self {
        Engine {
            graph: Graph::new(),
            config,
        }
    }
}

impl Default for Engine<InMemoryStorage> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: StorageAdapter> Engine<S> {
    /// Create an engine with a custom storage backend.
    pub fn with_storage(config: EngineConfig, storage: S) -> Self {
        Engine {
            graph: Graph::with_storage(storage),
            config,
        }
    }

    /// Ingest a new observation into the graph.
    ///
    /// Phase 1: Creates a Node directly without perception gating.
    /// Phase 2 will add: novelty scoring, confidence filtering, budget check,
    /// and attraction-based auto-linking.
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error> {
        let id = self.graph.next_node_id();
        let now = observation.timestamp;

        let node = Node {
            id,
            node_type: observation.node_type,
            name: observation.name,
            summary: observation.summary,
            content: observation.content,
            embedding: observation.embedding,
            created_at: now,
            updated_at: now,
            accessed_at: now,
            valid_from: None,
            valid_until: None,
            salience: 1.0,
            access_count: 0,
            origin: observation.origin,
            entity_tags: observation.entity_tags,
            metadata: HashMap::new(),
        };

        self.graph.add_node(node)?;
        Ok(vec![id])
    }

    /// Create or strengthen a link between two nodes.
    pub fn link(
        &mut self,
        from: NodeId,
        to: NodeId,
        edge_type: EdgeType,
        weight: f64,
    ) -> Result<EdgeId, Error> {
        let id = self.graph.next_edge_id();
        let edge = Edge {
            id,
            source: from,
            target: to,
            edge_type,
            weight,
            created_at: Timestamp::now(),
            metadata: HashMap::new(),
        };
        self.graph.add_edge(edge)?;
        Ok(id)
    }

    /// Touch a node — reinforce on access.
    ///
    /// Phase 1: Updates accessed_at and increments access_count.
    /// Phase 2 will add: lazy decay application + salience reinforcement (eq. 5).
    pub fn touch(&mut self, node_id: NodeId) -> Result<(), Error> {
        let now = Timestamp::now();
        self.graph.storage_mut().set_accessed_at(node_id, now)?;
        let node = self.graph.get_node_mut(node_id)?;
        node.access_count += 1;
        Ok(())
    }

    /// Advance time — apply decay to all nodes.
    ///
    /// Phase 1: No-op, returns empty report.
    /// Phase 2 will implement: equation (4) decay for all nodes.
    pub fn tick(&mut self, _now: Timestamp) -> Result<TickReport, Error> {
        Ok(TickReport::default())
    }

    /// Query the graph — returns structured context for LLM consumption.
    ///
    /// Phase 1: Returns empty ContextPackage.
    /// Phase 2 will implement: full pipeline (equations 10-14).
    pub fn query(&self, _query: &Query, _config: &QueryConfig) -> Result<ContextPackage, Error> {
        Ok(ContextPackage::empty())
    }

    /// Find merge candidates above similarity threshold.
    ///
    /// Phase 1: Returns empty list.
    /// Phase 2 will implement: attraction-based merge candidate detection.
    pub fn merge_candidates(&self, _threshold: f64) -> Result<Vec<MergePair>, Error> {
        Ok(vec![])
    }

    /// Execute auto-merge with undo log.
    ///
    /// Phase 1: Returns empty log.
    /// Phase 2 will implement: merge logic with undo support.
    pub fn auto_merge(&mut self, _threshold: f64) -> Result<MergeLog, Error> {
        Ok(MergeLog::default())
    }

    /// Cross-agent entity linking after parallel execution round.
    ///
    /// Creates Entity edges between nodes from different agents sharing entity tags.
    /// No LLM calls — metadata matching only.
    ///
    /// Phase 1: Returns empty report.
    /// Phase 2 will implement: entity tag matching + Entity edge creation.
    pub fn reflect_batch(&mut self, _sessions: &[SessionSummary]) -> Result<ReflectReport, Error> {
        Ok(ReflectReport::default())
    }

    /// Read-only access to the underlying graph.
    pub fn graph(&self) -> &Graph<S> {
        &self.graph
    }

    /// Mutable access to the underlying graph.
    pub fn graph_mut(&mut self) -> &mut Graph<S> {
        &mut self.graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeType, KnowledgeType};
    use crate::query::{Query, QueryConfig};

    fn make_observation(name: &str) -> Observation {
        Observation {
            name: name.to_string(),
            summary: None,
            content: format!("Content for {}", name),
            embedding: Some(vec![0.1, 0.2, 0.3]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec!["test".to_string()],
            origin: Origin {
                agent_id: "agent-1".to_string(),
                session_id: "session-1".to_string(),
                project_id: None,
                confidence: 0.9,
            },
            timestamp: Timestamp(1000),
        }
    }

    #[test]
    fn engine_new() {
        let engine = Engine::new();
        assert_eq!(engine.graph().node_count(), 0);
    }

    #[test]
    fn engine_default() {
        let engine = Engine::default();
        assert_eq!(engine.graph().node_count(), 0);
    }

    #[test]
    fn ingest_creates_node() {
        let mut engine = Engine::new();
        let ids = engine.ingest(make_observation("test node")).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(engine.graph().node_count(), 1);
        let node = engine.graph().get_node(ids[0]).unwrap();
        assert_eq!(node.name, "test node");
        assert_eq!(node.salience, 1.0);
    }

    #[test]
    fn link_creates_edge() {
        let mut engine = Engine::new();
        let ids1 = engine.ingest(make_observation("node A")).unwrap();
        let ids2 = engine.ingest(make_observation("node B")).unwrap();
        let eid = engine
            .link(ids1[0], ids2[0], EdgeType::Semantic, 0.8)
            .unwrap();
        assert_eq!(engine.graph().edge_count(), 1);
        let edge = engine.graph().get_edge(eid).unwrap();
        assert_eq!(edge.weight, 0.8);
    }

    #[test]
    fn touch_increments_access_count() {
        let mut engine = Engine::new();
        let ids = engine.ingest(make_observation("node")).unwrap();
        engine.touch(ids[0]).unwrap();
        engine.touch(ids[0]).unwrap();
        let node = engine.graph().get_node(ids[0]).unwrap();
        assert_eq!(node.access_count, 2);
    }

    #[test]
    fn tick_returns_report() {
        let mut engine = Engine::new();
        let report = engine.tick(Timestamp(1000)).unwrap();
        assert_eq!(report.nodes_decayed, 0);
    }

    #[test]
    fn query_returns_empty_package() {
        let engine = Engine::new();
        let q = Query::List {
            min_salience: 0.5,
            limit: 10,
        };
        let pkg = engine.query(&q, &QueryConfig::default()).unwrap();
        assert_eq!(pkg.total_fragments(), 0);
        assert_eq!(pkg.agent_tension, 0.0);
    }

    #[test]
    fn merge_candidates_returns_empty() {
        let engine = Engine::new();
        let candidates = engine.merge_candidates(0.9).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn reflect_batch_returns_report() {
        let mut engine = Engine::new();
        let report = engine.reflect_batch(&[]).unwrap();
        assert_eq!(report.entity_edges_created, 0);
    }

    #[test]
    fn link_has_nonzero_timestamp() {
        let mut engine = Engine::new();
        let ids1 = engine.ingest(make_observation("A")).unwrap();
        let ids2 = engine.ingest(make_observation("B")).unwrap();
        let eid = engine
            .link(ids1[0], ids2[0], EdgeType::Semantic, 0.5)
            .unwrap();
        let edge = engine.graph().get_edge(eid).unwrap();
        assert!(edge.created_at.0 > 0);
    }

    #[test]
    fn touch_updates_accessed_at_to_nonzero() {
        let mut engine = Engine::new();
        let ids = engine.ingest(make_observation("node")).unwrap();
        engine.touch(ids[0]).unwrap();
        let node = engine.graph().get_node(ids[0]).unwrap();
        assert!(node.accessed_at.0 > 0);
    }

    #[test]
    fn engine_config_builder() {
        let config = EngineConfig::new()
            .with_max_nodes(1000)
            .with_novelty_threshold(0.5)
            .with_confidence_threshold(0.7);
        assert_eq!(config.max_nodes, 1000);
        assert_eq!(config.novelty_threshold, 0.5);
    }
}
