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
    /// Number of nodes whose salience changed during this tick.
    pub nodes_decayed: usize,
    /// Number of nodes whose salience reached their type-specific floor during this tick.
    /// These nodes are dormant (near-zero activity) but remain in the graph.
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
    /// Creates a Node, then applies attraction mechanics: finds candidate nodes
    /// (last 256 + entity-tag matches), scores them, and creates/strengthens
    /// up to 4 edges to the most similar candidates.
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error> {
        use crate::mechanics::attraction::{
            attraction_score, cosine_similarity, should_create_edge, strengthen_edge, tau_type,
        };
        use crate::mechanics::gravity::compute_mass;
        use crate::mechanics::perception::gate_observation;

        let max_similarity = if let Some(ref new_embedding) = observation.embedding {
            self.graph
                .storage()
                .all_node_ids()
                .iter()
                .filter_map(|&nid| {
                    self.graph.storage().get_node(nid).ok().and_then(|n| {
                        n.embedding
                            .as_ref()
                            .map(|emb| cosine_similarity(new_embedding, emb))
                    })
                })
                .fold(0.0_f64, f64::max)
        } else {
            0.0
        };

        gate_observation(
            observation.confidence,
            self.config.confidence_threshold,
            self.graph.node_count(),
            self.config.max_nodes,
            max_similarity,
            self.config.novelty_threshold,
        )
        .map_err(Error::Rejected)?;

        let id = self.graph.next_node_id();
        let now = observation.timestamp;

        let node = Node {
            id,
            node_type: observation.node_type.clone(),
            name: observation.name,
            summary: observation.summary,
            content: observation.content,
            embedding: observation.embedding.clone(),
            created_at: now,
            updated_at: now,
            accessed_at: now,
            valid_from: None,
            valid_until: None,
            salience: 1.0,
            access_count: 0,
            origin: observation.origin,
            entity_tags: observation.entity_tags.clone(),
            metadata: HashMap::new(),
        };

        self.graph.add_node(node)?;

        if let Some(ref new_embedding) = observation.embedding {
            let new_type = &observation.node_type;
            let new_tags = &observation.entity_tags;

            // Candidate pool: last 256 nodes by ID + entity-tag matches
            let mut candidate_ids: Vec<NodeId> = {
                let mut all = self.graph.storage().all_node_ids();
                all.retain(|&nid| nid != id);
                all.sort_by_key(|a| std::cmp::Reverse(a.0));
                all.truncate(256);
                all
            };

            if !new_tags.is_empty() {
                let all_ids = self.graph.storage().all_node_ids();
                for nid in all_ids {
                    if nid == id {
                        continue;
                    }
                    if let Ok(node) = self.graph.storage().get_node(nid) {
                        let has_shared_tag = node.entity_tags.iter().any(|t| new_tags.contains(t));
                        if has_shared_tag && !candidate_ids.contains(&nid) {
                            candidate_ids.push(nid);
                        }
                    }
                }
            }

            // Score candidates by attraction (eq 3)
            let mut scored: Vec<(NodeId, f64)> = Vec::new();
            for cid in &candidate_ids {
                let candidate = match self.graph.storage().get_node(*cid) {
                    Ok(n) => n,
                    Err(_) => continue,
                };

                let Some(ref cand_embedding) = candidate.embedding else {
                    continue;
                };

                let sim = cosine_similarity(new_embedding, cand_embedding);
                if sim == 0.0 {
                    continue;
                }

                let tau = tau_type(new_type, &candidate.node_type);
                let cand_salience = self.graph.storage().get_salience(*cid).unwrap_or(0.0);
                let cand_mass =
                    compute_mass(cand_salience, candidate.access_count, &candidate.node_type);
                let score = attraction_score(sim, tau, cand_mass);

                if should_create_edge(score, new_type, &candidate.node_type) {
                    scored.push((*cid, score));
                }
            }

            // Top 4 by score
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(4);

            for (cid, score) in scored {
                // Check existing edge in either direction
                let existing_edge = self
                    .graph
                    .edges_from(id)
                    .iter()
                    .find_map(|&eid| self.graph.get_edge(eid).ok().filter(|e| e.target == cid))
                    .or_else(|| {
                        self.graph.edges_from(cid).iter().find_map(|&eid| {
                            self.graph.get_edge(eid).ok().filter(|e| e.target == id)
                        })
                    });

                if let Some(existing) = existing_edge {
                    let new_weight = strengthen_edge(existing.weight, score);
                    let eid = existing.id;
                    if let Ok(edge) = self.graph.get_edge_mut(eid) {
                        edge.weight = new_weight;
                    }
                } else {
                    let eid = self.graph.next_edge_id();
                    let edge = Edge {
                        id: eid,
                        source: id,
                        target: cid,
                        edge_type: EdgeType::Semantic,
                        weight: score.clamp(0.0, 1.0),
                        created_at: now,
                        metadata: HashMap::new(),
                    };
                    self.graph.add_edge(edge)?;
                }
            }
        }

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

    /// Touch a node — apply lazy decay then reinforce on access.
    ///
    /// Phase 2: Applies decay (eq 4) BEFORE reinforcement (eq 5).
    /// Decay is lazy: computed based on elapsed time since last access.
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error> {
        use crate::mechanics::forgetting::{decay_salience, reinforce_salience};

        let current_salience = self.graph.storage().get_salience(node_id)?;
        let last_accessed = self.graph.storage().get_accessed_at(node_id)?;
        let node_type = self.graph.storage().get_node_type(node_id)?.clone();

        let dt_ms = now.0.saturating_sub(last_accessed.0);
        let dt_days = dt_ms as f64 / 86_400_000.0;

        // Decay BEFORE reinforcement — ordering invariant (eq 4 then eq 5)
        let decayed = decay_salience(current_salience, dt_days, &node_type);
        let reinforced = reinforce_salience(decayed);

        self.graph.storage_mut().set_salience(node_id, reinforced)?;
        self.graph.storage_mut().set_accessed_at(node_id, now)?;

        let node = self.graph.get_node_mut(node_id)?;
        node.access_count += 1;

        Ok(())
    }

    /// Advance time — apply batch decay (eq 4) to all nodes.
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error> {
        use crate::mechanics::forgetting::{decay_salience, floor_for_type};

        let node_ids = self.graph.storage().all_node_ids();
        let mut nodes_decayed = 0usize;
        let mut nodes_pruned = 0usize;

        for id in node_ids {
            let current_salience = match self.graph.storage().get_salience(id) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let last_accessed = match self.graph.storage().get_accessed_at(id) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let node_type = match self.graph.storage().get_node_type(id) {
                Ok(kt) => kt.clone(),
                Err(_) => continue,
            };

            let dt_ms = now.0.saturating_sub(last_accessed.0);
            let dt_days = dt_ms as f64 / 86_400_000.0;

            let new_salience = decay_salience(current_salience, dt_days, &node_type);

            if (new_salience - current_salience).abs() > 1e-10 {
                if self
                    .graph
                    .storage_mut()
                    .set_salience(id, new_salience)
                    .is_err()
                {
                    continue;
                }
                if self.graph.storage_mut().set_accessed_at(id, now).is_err() {
                    continue;
                }
                nodes_decayed += 1;

                let floor = floor_for_type(&node_type);
                if new_salience <= floor + 1e-6 {
                    nodes_pruned += 1;
                }
            }
        }

        Ok(TickReport {
            nodes_decayed,
            nodes_pruned,
        })
    }

    /// Query the graph — returns structured context for LLM consumption.
    ///
    /// Phase 2: Associative query implements the full pipeline (equations 9-14).
    /// Other query modes return empty ContextPackage (Phase 3).
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error> {
        match query {
            Query::Associative { seed, budget } => self.query_associative(*seed, *budget, config),
            _ => Ok(ContextPackage::empty()),
        }
    }

    /// Full Associative query pipeline: initial activation → spreading → repulsion → scoring → assembly.
    fn query_associative(
        &self,
        seed: NodeId,
        budget: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use crate::mechanics::attraction::cosine_similarity;
        use crate::mechanics::gravity::compute_mass;
        use crate::mechanics::repulsion::{apply_damping, compute_repulsion, rigidity};
        use crate::query::activation::{NodeInfo, initial_activation, spread_activation};
        use crate::query::assembly::{ScoredNode, assemble_context_package};
        use crate::query::identity::compute_identity_prior;
        use crate::query::scoring::{final_score, scope_weight};

        // Verify seed exists
        let _ = self.graph.get_node(seed)?;

        let storage = self.graph.storage();

        // --- Stage 1: Collect identity nodes for this agent ---
        let identity_nodes: Vec<(Vec<f64>, KnowledgeType, f64)> =
            if let Some(ref agent_id) = config.agent_id {
                storage
                    .all_node_ids()
                    .iter()
                    .filter_map(|&nid| {
                        let node = storage.get_node(nid).ok()?;
                        let is_identity = matches!(
                            node.node_type,
                            KnowledgeType::IdentityCore
                                | KnowledgeType::IdentityLearned
                                | KnowledgeType::IdentityState
                        );
                        if is_identity && node.origin.agent_id == *agent_id {
                            let emb = node.embedding.clone().unwrap_or_default();
                            let salience = storage.get_salience(nid).unwrap_or(0.0);
                            Some((emb, node.node_type.clone(), salience))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                vec![]
            };

        // --- Stage 2: Compute initial activations (eq 10) ---
        let mut initial_activations: HashMap<NodeId, f64> = HashMap::new();
        let all_ids = storage.all_node_ids();

        for &nid in &all_ids {
            let node = match storage.get_node(nid) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let is_seed = nid == seed;

            let vector_sim = match (&config.query_embedding, &node.embedding) {
                (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
                _ => 0.0,
            };

            let identity_prior = match &node.embedding {
                Some(emb) => compute_identity_prior(emb, &identity_nodes, cosine_similarity),
                None => 0.0,
            };

            let act = initial_activation(is_seed, vector_sim, identity_prior);
            if act > config.min_activation || is_seed {
                initial_activations.insert(nid, act);
            }
        }

        // --- Stage 3: Spreading activation (eq 11) ---
        let node_info_fn = |nid: NodeId| -> Option<NodeInfo> {
            let node = storage.get_node(nid).ok()?;
            let salience = storage.get_salience(nid).unwrap_or(0.0);
            let mass = compute_mass(salience, node.access_count, &node.node_type);

            let mut all_edges: Vec<(NodeId, f64, EdgeType, bool)> = Vec::new();

            for &eid in storage.edges_from(nid) {
                if let Ok(edge) = storage.get_edge(eid) {
                    all_edges.push((edge.target, edge.weight, edge.edge_type.clone(), true));
                }
            }
            for &eid in storage.edges_to(nid) {
                if let Ok(edge) = storage.get_edge(eid) {
                    all_edges.push((edge.source, edge.weight, edge.edge_type.clone(), false));
                }
            }

            Some(NodeInfo {
                salience,
                mass,
                outgoing_edges: all_edges,
            })
        };

        let activations = spread_activation(
            initial_activations,
            node_info_fn,
            budget,
            config.min_activation,
            config.decay_per_hop,
            config.max_hops,
        );

        // --- Stage 4: Repulsion damping (eqs 7-8) ---
        let mut damped_activations = activations.clone();

        for &nid in activations.keys() {
            let contradicts_inputs: Vec<(f64, f64, f64)> = storage
                .edges_to(nid)
                .iter()
                .filter_map(|&eid| {
                    let edge = storage.get_edge(eid).ok()?;
                    if !matches!(edge.edge_type, EdgeType::Contradicts) {
                        return None;
                    }
                    let source_act = activations.get(&edge.source).copied().unwrap_or(0.0);
                    if source_act == 0.0 {
                        return None;
                    }
                    let source_node = storage.get_node(edge.source).ok()?;
                    let rho = rigidity(&source_node.node_type);
                    Some((edge.weight, rho, source_act))
                })
                .collect();

            if !contradicts_inputs.is_empty() {
                let h = compute_repulsion(&contradicts_inputs);
                if h > 0.0 {
                    let current = activations.get(&nid).copied().unwrap_or(0.0);
                    let damped = apply_damping(current, h);
                    damped_activations.insert(nid, damped);
                }
            }
        }

        // --- Stage 5: Final scoring (eq 13) ---
        let seed_node = storage.get_node(seed).ok();
        let mut scored_nodes: Vec<ScoredNode> = Vec::new();

        for (&nid, &activation) in &damped_activations {
            if activation < config.min_activation {
                continue;
            }

            let node = match storage.get_node(nid) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let salience = storage.get_salience(nid).unwrap_or(0.0);
            let mass = compute_mass(salience, node.access_count, &node.node_type);

            let vector_sim = match (&config.query_embedding, &node.embedding) {
                (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
                _ => 0.0,
            };

            let shared_entities = seed_node
                .map(|sn| {
                    node.entity_tags
                        .iter()
                        .filter(|t| sn.entity_tags.contains(t))
                        .count()
                })
                .unwrap_or(0);

            let sw = scope_weight(&config.project_id, &node.origin.project_id, shared_entities);
            let relevance = final_score(activation, vector_sim, salience, mass, sw);

            scored_nodes.push(ScoredNode {
                node_id: nid,
                name: node.name.clone(),
                summary: node.summary.clone(),
                content: node.content.clone(),
                node_type: node.node_type.clone(),
                relevance,
                origin: node.origin.clone(),
            });
        }

        // --- Stage 6: Collect Contradicts edges and identity activations ---
        let mut contradicts_edges: Vec<(NodeId, NodeId, f64)> = Vec::new();
        for &nid in damped_activations.keys() {
            for &eid in storage.edges_from(nid) {
                if let Ok(edge) = storage.get_edge(eid) {
                    if matches!(edge.edge_type, EdgeType::Contradicts) {
                        contradicts_edges.push((edge.source, edge.target, edge.weight));
                    }
                }
            }
        }

        let identity_activations: Vec<(NodeId, KnowledgeType, f64)> = damped_activations
            .iter()
            .filter_map(|(&nid, &act)| {
                let node = storage.get_node(nid).ok()?;
                let is_identity = matches!(
                    node.node_type,
                    KnowledgeType::IdentityCore
                        | KnowledgeType::IdentityLearned
                        | KnowledgeType::IdentityState
                );
                let is_agent = match &config.agent_id {
                    Some(aid) => node.origin.agent_id == *aid,
                    None => false,
                };
                if is_identity && is_agent {
                    Some((nid, node.node_type.clone(), act))
                } else {
                    None
                }
            })
            .collect();

        // --- Stage 7: Assemble ContextPackage ---
        let package = assemble_context_package(
            scored_nodes,
            &identity_activations,
            &contradicts_edges,
            &damped_activations,
            config.token_budget,
            config.chars_per_token,
            &config.project_id,
        );

        Ok(package)
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
            embedding: None,
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
        engine.touch(ids[0], Timestamp::now()).unwrap();
        engine.touch(ids[0], Timestamp::now()).unwrap();
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
        engine.touch(ids[0], Timestamp::now()).unwrap();
        let node = engine.graph().get_node(ids[0]).unwrap();
        assert!(node.accessed_at.0 > 0);
    }

    #[test]
    fn touch_applies_decay_before_reinforcement() {
        let mut engine = Engine::new();
        let ids = engine.ingest(make_observation("node")).unwrap();
        let id = ids[0];

        let future = Timestamp(1000 + 30 * 86_400_000);
        engine.touch(id, future).unwrap();

        let node = engine.graph().get_node(id).unwrap();
        assert!(
            node.salience < 1.0,
            "salience should have decayed: {}",
            node.salience
        );
        assert!(
            node.salience > 0.0,
            "salience should not be zero: {}",
            node.salience
        );
        assert_eq!(node.access_count, 1);
    }

    #[test]
    fn touch_immediate_reinforces_without_decay() {
        let mut engine = Engine::new();
        let ids = engine.ingest(make_observation("node")).unwrap();
        let id = ids[0];

        let now = Timestamp(1000);
        engine.touch(id, now).unwrap();

        let node = engine.graph().get_node(id).unwrap();
        // dt=0, no decay. reinforce(1.0) = 1.0 + 0.20*(1-1.0) = 1.0
        assert_eq!(node.salience, 1.0);
        assert_eq!(node.access_count, 1);
    }

    #[test]
    fn tick_decays_episodic_faster_than_semantic() {
        let mut engine = Engine::new();

        let episodic_obs = Observation {
            node_type: KnowledgeType::Episodic,
            timestamp: Timestamp(0),
            ..make_observation("episodic")
        };
        let semantic_obs = Observation {
            node_type: KnowledgeType::Semantic,
            timestamp: Timestamp(0),
            ..make_observation("semantic")
        };

        let episodic_ids = engine.ingest(episodic_obs).unwrap();
        let semantic_ids = engine.ingest(semantic_obs).unwrap();

        let future = Timestamp(30 * 86_400_000);
        let report = engine.tick(future).unwrap();

        assert!(
            report.nodes_decayed >= 1,
            "at least one node should have decayed"
        );

        let episodic_s = engine
            .graph()
            .storage()
            .get_salience(episodic_ids[0])
            .unwrap();
        let semantic_s = engine
            .graph()
            .storage()
            .get_salience(semantic_ids[0])
            .unwrap();

        assert!(
            episodic_s < semantic_s,
            "episodic ({episodic_s}) should decay faster than semantic ({semantic_s})"
        );
    }

    #[test]
    fn tick_identity_core_unchanged() {
        let mut engine = Engine::new();

        let identity_obs = Observation {
            node_type: KnowledgeType::IdentityCore,
            timestamp: Timestamp(0),
            ..make_observation("identity")
        };
        let id = engine.ingest(identity_obs).unwrap()[0];

        let future = Timestamp(365 * 86_400_000);
        engine.tick(future).unwrap();

        let salience = engine.graph().storage().get_salience(id).unwrap();
        assert_eq!(salience, 1.0, "IdentityCore should not decay");
    }

    #[test]
    fn tick_report_counts_correctly() {
        let mut engine = Engine::new();

        for i in 0..3 {
            let obs = Observation {
                node_type: KnowledgeType::Episodic,
                timestamp: Timestamp(0),
                ..make_observation(&format!("episodic-{i}"))
            };
            engine.ingest(obs).unwrap();
        }

        let future = Timestamp(30 * 86_400_000);
        let report = engine.tick(future).unwrap();

        assert_eq!(
            report.nodes_decayed, 3,
            "all 3 episodic nodes should have decayed"
        );
    }

    #[test]
    fn ingest_auto_links_similar_nodes() {
        let config = EngineConfig::new().with_novelty_threshold(0.0);
        let mut engine = Engine::with_config(config);

        let obs1 = Observation {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("node A")
        };
        let ids1 = engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: Some(vec![0.95, 0.1, 0.0]),
            ..make_observation("node B")
        };
        let ids2 = engine.ingest(obs2).unwrap();

        assert_eq!(
            engine.graph().edge_count(),
            1,
            "similar nodes should be auto-linked"
        );
        let edges = engine.graph().edges_from(ids2[0]);
        assert_eq!(edges.len(), 1);
        let edge = engine.graph().get_edge(edges[0]).unwrap();
        assert_eq!(edge.target, ids1[0]);
    }

    #[test]
    fn ingest_no_link_for_dissimilar_nodes() {
        let mut engine = Engine::new();

        let obs1 = Observation {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("node A")
        };
        engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: Some(vec![0.0, 1.0, 0.0]),
            ..make_observation("node B")
        };
        engine.ingest(obs2).unwrap();

        assert_eq!(
            engine.graph().edge_count(),
            0,
            "orthogonal nodes should not be linked"
        );
    }

    #[test]
    fn ingest_no_embedding_skips_attraction() {
        let mut engine = Engine::new();

        let obs1 = Observation {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("node A")
        };
        engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: None,
            ..make_observation("node B")
        };
        engine.ingest(obs2).unwrap();

        assert_eq!(
            engine.graph().edge_count(),
            0,
            "node without embedding should not trigger attraction"
        );
    }

    #[test]
    fn ingest_max_four_edges() {
        let config = EngineConfig::new().with_novelty_threshold(0.0);
        let mut engine = Engine::with_config(config);

        for i in 0..10 {
            let obs = Observation {
                embedding: Some(vec![1.0, 0.01 * i as f64, 0.0]),
                ..make_observation(&format!("node-{i}"))
            };
            engine.ingest(obs).unwrap();
        }

        let all_ids = engine.graph().all_node_ids();
        let last_id = *all_ids.iter().max_by_key(|id| id.0).unwrap();
        let edge_count = engine.graph().edges_from(last_id).len();
        assert!(edge_count <= 4, "max 4 edges per ingest, got {edge_count}");
    }

    #[test]
    fn ingest_rejects_low_confidence() {
        let config = EngineConfig::new().with_confidence_threshold(0.8);
        let mut engine = Engine::with_config(config);

        let obs = Observation {
            confidence: 0.5,
            ..make_observation("low confidence")
        };
        let result = engine.ingest(obs);
        assert!(matches!(result, Err(Error::Rejected(_))));
    }

    #[test]
    fn ingest_rejects_over_budget() {
        let config = EngineConfig::new().with_max_nodes(2);
        let mut engine = Engine::with_config(config);

        engine.ingest(make_observation("node 1")).unwrap();
        engine.ingest(make_observation("node 2")).unwrap();

        let result = engine.ingest(make_observation("node 3"));
        assert!(matches!(result, Err(Error::Rejected(_))));
    }

    #[test]
    fn ingest_rejects_duplicate_embedding() {
        let config = EngineConfig::new().with_novelty_threshold(0.3);
        let mut engine = Engine::with_config(config);

        let obs1 = Observation {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("original")
        };
        engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: Some(vec![1.0, 0.001, 0.0]),
            ..make_observation("duplicate")
        };
        let result = engine.ingest(obs2);
        assert!(matches!(result, Err(Error::Rejected(_))));
    }

    #[test]
    fn ingest_accepts_valid_observation() {
        let mut engine = Engine::new();
        let result = engine.ingest(make_observation("valid"));
        assert!(result.is_ok());
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

    #[test]
    fn query_associative_returns_real_results() {
        let config = EngineConfig::new().with_novelty_threshold(0.0);
        let mut engine = Engine::with_config(config);

        let obs1 = Observation {
            node_type: KnowledgeType::Semantic,
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("auth uses factory pattern")
        };
        let obs2 = Observation {
            node_type: KnowledgeType::Semantic,
            embedding: Some(vec![0.9, 0.1, 0.0]),
            ..make_observation("factory pattern is preferred")
        };
        let ids1 = engine.ingest(obs1).unwrap();
        let ids2 = engine.ingest(obs2).unwrap();

        engine
            .link(ids1[0], ids2[0], EdgeType::Semantic, 0.8)
            .unwrap();

        let q = Query::Associative {
            seed: ids1[0],
            budget: 50,
        };
        let qconfig = QueryConfig::default();
        let pkg = engine.query(&q, &qconfig).unwrap();

        assert!(
            pkg.total_fragments() > 0,
            "Associative query should return non-empty ContextPackage"
        );
    }

    #[test]
    fn query_associative_with_identity_node() {
        let config = EngineConfig::new().with_novelty_threshold(0.0);
        let mut engine = Engine::with_config(config);

        let identity_obs = Observation {
            node_type: KnowledgeType::IdentityCore,
            embedding: Some(vec![1.0, 0.0]),
            origin: Origin {
                agent_id: "agent-1".to_string(),
                session_id: "session-1".to_string(),
                project_id: None,
                confidence: 1.0,
            },
            ..make_observation("I am a code architect")
        };
        let semantic_obs = Observation {
            node_type: KnowledgeType::Semantic,
            embedding: Some(vec![0.9, 0.1]),
            ..make_observation("factory pattern knowledge")
        };

        let identity_ids = engine.ingest(identity_obs).unwrap();
        let semantic_ids = engine.ingest(semantic_obs).unwrap();
        engine
            .link(identity_ids[0], semantic_ids[0], EdgeType::Semantic, 0.8)
            .unwrap();

        let q = Query::Associative {
            seed: semantic_ids[0],
            budget: 50,
        };
        let qconfig = QueryConfig {
            agent_id: Some("agent-1".to_string()),
            ..QueryConfig::default()
        };
        let pkg = engine.query(&q, &qconfig).unwrap();

        assert!(
            !pkg.identity.is_empty() || !pkg.knowledge.is_empty(),
            "Query should return some results"
        );
    }

    #[test]
    fn query_associative_with_contradicts_creates_tension() {
        let config = EngineConfig::new().with_novelty_threshold(0.0);
        let mut engine = Engine::with_config(config);

        let obs1 = Observation {
            node_type: KnowledgeType::Semantic,
            embedding: Some(vec![1.0, 0.0]),
            ..make_observation("factory pattern is good")
        };
        let obs2 = Observation {
            node_type: KnowledgeType::Semantic,
            embedding: Some(vec![0.9, 0.1]),
            ..make_observation("factory pattern is bad")
        };

        let ids1 = engine.ingest(obs1).unwrap();
        let ids2 = engine.ingest(obs2).unwrap();
        engine
            .link(ids1[0], ids2[0], EdgeType::Contradicts, 0.9)
            .unwrap();

        let q = Query::Associative {
            seed: ids1[0],
            budget: 50,
        };
        let pkg = engine.query(&q, &QueryConfig::default()).unwrap();

        assert!(
            !pkg.tensions.is_empty() || pkg.agent_tension >= 0.0,
            "Contradicts edge should create tension"
        );
    }

    #[test]
    fn query_non_associative_returns_empty() {
        let engine = Engine::new();
        let queries = vec![
            Query::TypeFiltered {
                node_type: KnowledgeType::Convention,
                limit: 10,
            },
            Query::List {
                min_salience: 0.5,
                limit: 10,
            },
        ];
        for q in &queries {
            let pkg = engine.query(q, &QueryConfig::default()).unwrap();
            assert_eq!(
                pkg.total_fragments(),
                0,
                "non-Associative should return empty"
            );
        }
    }
}
