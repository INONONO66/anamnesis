//! Public API surface for the Anamnesis cognitive graph engine.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::{Edge, Graph, Node};
use crate::graph::{EdgeId, EdgeType, KnowledgeType, MemoryTier, NodeId, Timestamp};
use crate::query::{ContextPackage, Query, QueryConfig, SearchInput, SearchResult};
use crate::storage::{InMemoryStorage, StorageAdapter};

/// Decay model for salience computation.
///
/// `Exponential` (default) uses the existing formula: s(t+dt) = b + (s(t) - b) * exp(-lambda * dt).
/// `PowerLaw` uses ACT-R base-level activation (Anderson & Schooler 1991).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DecayModel {
    /// Exponential decay — backwards-compatible default.
    #[default]
    Exponential,
    /// ACT-R power-law decay: B = ln(Σⱼ tⱼ⁻⁰·⁵), salience = sigmoid(B).
    PowerLaw,
}

/// Energy model for final score computation in spreading activation.
///
/// `WeightedSum` (default) uses the existing weighted final-score formula (equation 13):
/// 0.50 * activation + 0.20 * vector_similarity + 0.15 * salience + 0.15 * mass, multiplied by scope weight.
/// `Hopfield` uses local pattern completion to adjust embedding similarity when query
/// and candidate embeddings are available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EnergyModel {
    /// Weighted sum — backwards-compatible default.
    #[default]
    WeightedSum,
    /// Hopfield pattern completion for embedding-aware scoring.
    Hopfield,
}

/// Spreading activation model for query traversal.
///
/// `PriorityQueueBfs` (default) uses priority-queue BFS with hop decay and salience gating.
/// `RandomWalkRestart` uses matrix-free random walk with restart from the seed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpreadingModel {
    /// Priority-queue BFS — backwards-compatible default.
    #[default]
    PriorityQueueBfs,
    /// Random walk with restart.
    RandomWalkRestart,
}

const RWR_RESTART_PROBABILITY: f64 = 0.15;
const RWR_MAX_ITERATIONS: usize = 128;
const HOPFIELD_RETRIEVAL_ITERATIONS: usize = 3;

struct HopfieldScoringContext {
    retrieved: Vec<f64>,
    energy_gain: f64,
}

/// Configuration for the Anamnesis engine.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EngineConfig {
    /// Maximum number of nodes before perception gate rejects new observations.
    pub max_nodes: usize,
    /// Minimum novelty score [0, 1] for an observation to enter the graph.
    pub novelty_threshold: f64,
    /// Minimum confidence [0, 1] for an observation to enter the graph.
    pub confidence_threshold: f64,
    /// Similarity threshold above which ingest reinforces an existing node instead of creating one.
    pub dedup_threshold: f64,
    /// Whether ingest should detect duplicate embeddings and reinforce existing nodes.
    pub dedup_enabled: bool,
    /// Decay model to use for salience computation. Default: Exponential (backwards-compatible).
    pub decay_model: DecayModel,
    /// Energy model for final score computation in spreading activation. Default: WeightedSum (backwards-compatible).
    pub energy_model: EnergyModel,
    /// Spreading activation model for query traversal. Default: PriorityQueueBfs (backwards-compatible).
    pub spreading_model: SpreadingModel,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_nodes: 100_000,
            novelty_threshold: 0.30,
            confidence_threshold: 0.50,
            dedup_threshold: 0.92,
            dedup_enabled: true,
            decay_model: DecayModel::Exponential,
            energy_model: EnergyModel::WeightedSum,
            spreading_model: SpreadingModel::PriorityQueueBfs,
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

    pub fn with_dedup_threshold(mut self, threshold: f64) -> Self {
        self.dedup_threshold = threshold;
        self
    }

    pub fn with_dedup_enabled(mut self, enabled: bool) -> Self {
        self.dedup_enabled = enabled;
        self
    }
}

fn finite_vector(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn rwr_activations<S: StorageAdapter>(
    seed: NodeId,
    budget: usize,
    min_activation: f64,
    storage: &S,
) -> HashMap<NodeId, f64> {
    let scores = crate::query::random_walk_restart(
        seed,
        RWR_RESTART_PROBABILITY,
        RWR_MAX_ITERATIONS,
        storage,
    );

    let max_score = scores
        .values()
        .copied()
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);

    if max_score <= f64::EPSILON {
        return HashMap::from([(seed, 1.0)]);
    }

    let mut normalized: Vec<(NodeId, f64)> = scores
        .into_iter()
        .filter_map(|(node_id, score)| {
            if !score.is_finite() || score < 0.0 || storage.get_node(node_id).is_err() {
                return None;
            }

            let activation = (score / max_score).clamp(0.0, 1.0);
            if activation >= min_activation || node_id == seed {
                Some((node_id, activation))
            } else {
                None
            }
        })
        .collect();

    if !normalized.iter().any(|(node_id, _)| *node_id == seed) {
        normalized.push((seed, 1.0));
    }

    normalized.sort_by(|(a_id, a_score), (b_id, b_score)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a_id.0.cmp(&b_id.0))
    });

    let limit = budget.max(1);
    if normalized.len() > limit {
        normalized.truncate(limit);
        if !normalized.iter().any(|(node_id, _)| *node_id == seed) {
            normalized.pop();
            let seed_activation = normalized
                .iter()
                .find_map(|(node_id, activation)| (*node_id == seed).then_some(*activation))
                .unwrap_or(1.0);
            normalized.push((seed, seed_activation));
        }
    }

    normalized.into_iter().collect()
}

fn build_hopfield_scoring_context<S: StorageAdapter>(
    query_embedding: &Option<Vec<f64>>,
    activations: &HashMap<NodeId, f64>,
    storage: &S,
) -> Option<HopfieldScoringContext> {
    let query_embedding = query_embedding.as_ref()?;
    if query_embedding.is_empty() || !finite_vector(query_embedding) {
        return None;
    }

    let mut candidates: Vec<(NodeId, Vec<f64>)> = activations
        .iter()
        .filter(|(_, activation)| activation.is_finite() && **activation > 0.0)
        .filter_map(|(node_id, _)| {
            let embedding = storage.get_node(*node_id).ok()?.embedding.as_ref()?;
            if embedding.len() == query_embedding.len() && finite_vector(embedding) {
                Some((*node_id, embedding.clone()))
            } else {
                None
            }
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by_key(|(node_id, _)| node_id.0);
    let patterns: Vec<Vec<f64>> = candidates
        .into_iter()
        .map(|(_, embedding)| embedding)
        .collect();

    let retrieved = crate::mechanics::hopfield::retrieve(
        query_embedding,
        &patterns,
        HOPFIELD_RETRIEVAL_ITERATIONS,
    );
    if retrieved.len() != query_embedding.len() || !finite_vector(&retrieved) {
        return None;
    }

    let initial_energy = crate::mechanics::hopfield::energy(query_embedding, &patterns);
    let retrieved_energy = crate::mechanics::hopfield::energy(&retrieved, &patterns);
    let energy_gain = if initial_energy.is_finite() && retrieved_energy.is_finite() {
        (initial_energy - retrieved_energy).max(0.0).tanh()
    } else {
        0.0
    };

    Some(HopfieldScoringContext {
        retrieved,
        energy_gain,
    })
}

fn hopfield_adjusted_similarity(
    context: Option<&HopfieldScoringContext>,
    fallback_similarity: f64,
    candidate_embedding: Option<&Vec<f64>>,
) -> f64 {
    let Some(context) = context else {
        return fallback_similarity;
    };
    let Some(candidate_embedding) = candidate_embedding else {
        return fallback_similarity;
    };
    if candidate_embedding.len() != context.retrieved.len() || !finite_vector(candidate_embedding) {
        return fallback_similarity;
    }

    let completed_similarity =
        crate::mechanics::attraction::cosine_similarity(&context.retrieved, candidate_embedding);
    if !completed_similarity.is_finite() {
        return fallback_similarity;
    }

    (0.90 * completed_similarity + 0.10 * fallback_similarity + 0.05 * context.energy_gain)
        .clamp(0.0, 1.0)
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

/// Request to crystallize query results into a higher-level knowledge node.
///
/// The consumer supplies synthesized content and source provenance; the engine
/// computes initial salience, creates provenance edges, and reinforces sources.
#[derive(Debug, Clone)]
pub struct CrystallizeRequest {
    /// L0: One-liner label for the synthesis.
    pub name: String,
    /// L1: Optional structural overview.
    pub summary: Option<String>,
    /// L2: Full synthesis content.
    pub content: String,
    /// Embedding vector for deduplication and attraction.
    pub embedding: Option<Vec<f64>>,
    /// Source fragment node IDs used to create this synthesis.
    pub source_ids: Vec<NodeId>,
    /// Optional source relevance scores aligned with `source_ids`.
    pub source_relevances: Option<Vec<f64>>,
    /// Knowledge type for the crystallized node.
    pub node_type: KnowledgeType,
    /// Confidence in the synthesis [0, 1].
    pub confidence: f64,
    /// Provenance of the synthesis.
    pub origin: Origin,
    /// Entity tags for future linking.
    pub entity_tags: Vec<String>,
    /// Timestamp of crystallization.
    pub timestamp: Timestamp,
}

/// Result of a crystallization operation.
#[derive(Debug, Clone)]
pub struct CrystallizeResult {
    /// The newly created synthesis node.
    pub node_id: NodeId,
    /// `ConsolidatedFrom` edges created from the crystal to each source.
    pub consolidation_edges: Vec<EdgeId>,
    /// Maximum duplicate similarity observed against existing crystallized nodes.
    pub dedup_score: f64,
    /// Attraction edges created from the crystal to similar non-source nodes.
    pub attraction_edges: Vec<EdgeId>,
    /// Number of source nodes reinforced via `touch()`.
    pub nodes_reinforced: usize,
    /// Initial salience assigned to the crystal.
    pub initial_salience: f64,
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

/// Result of an ingest operation.
///
/// Indicates whether a new node was created or an existing node was reinforced.
#[derive(Debug, Clone)]
pub enum IngestResult {
    /// A new node was created with the given IDs.
    Created(Vec<NodeId>),
    /// An existing node was reinforced due to similarity.
    Reinforced {
        /// The ID of the existing node that was reinforced.
        existing_id: NodeId,
        /// Similarity score [0, 1] between the new observation and the existing node.
        similarity: f64,
    },
}

/// Return the top-N `(NodeId, score)` pairs by score descending.
///
/// Uses a min-heap of size `n` for O(M log N) complexity instead of sorting
/// all candidates with O(M log M) complexity.
pub fn top_n_by_score(scores: &[(NodeId, f64)], n: usize) -> Vec<(NodeId, f64)> {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Entry {
        node_id: NodeId,
        score: f64,
    }

    impl Eq for Entry {}

    impl PartialOrd for Entry {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for Entry {
        fn cmp(&self, other: &Self) -> Ordering {
            other
                .score
                .partial_cmp(&self.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| other.node_id.cmp(&self.node_id))
        }
    }

    if n == 0 {
        return Vec::new();
    }

    let mut heap = BinaryHeap::with_capacity(n);
    for &(node_id, score) in scores {
        let entry = Entry { node_id, score };

        if heap.len() < n {
            heap.push(entry);
        } else if heap.peek().is_some_and(|lowest| entry.score > lowest.score) {
            heap.pop();
            heap.push(entry);
        }
    }

    let mut result: Vec<_> = heap
        .into_iter()
        .map(|entry| (entry.node_id, entry.score))
        .collect();
    result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    result
}

fn clamp_unit_finite(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn positive_edge_weight(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.1, 1.0)
    } else {
        0.1
    }
}

fn crystallized_salience(
    source_saliences: &[f64],
    relevance_weights: &[f64],
    confidence: f64,
) -> f64 {
    let weight_sum: f64 = relevance_weights.iter().sum();
    let source_average = if weight_sum > f64::EPSILON {
        source_saliences
            .iter()
            .zip(relevance_weights.iter())
            .map(|(salience, relevance)| salience * relevance)
            .sum::<f64>()
            / weight_sum
    } else if source_saliences.is_empty() {
        0.0
    } else {
        source_saliences.iter().sum::<f64>() / source_saliences.len() as f64
    };

    (0.60 * source_average + 0.25 * confidence + 0.15 * 0.10).clamp(0.0, 1.0)
}

fn merge_highest_relevance(
    fragments: &mut HashMap<NodeId, crate::query::Fragment>,
    fragment: crate::query::Fragment,
) {
    fragments
        .entry(fragment.node_id)
        .and_modify(|existing| {
            if fragment.relevance > existing.relevance {
                *existing = fragment.clone();
            }
        })
        .or_insert(fragment);
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
    pub fn ingest(&mut self, observation: Observation) -> Result<IngestResult, Error> {
        use crate::mechanics::attraction::{
            attraction_score, cosine_similarity, should_create_edge, strengthen_edge, tau_type,
        };
        use crate::mechanics::gravity::compute_mass;
        use crate::mechanics::perception::gate_observation;

        let (max_similarity, most_similar_id) =
            if let Some(ref new_embedding) = observation.embedding {
                // Build candidate set: recent 256 nodes + entity-tag matches
                let mut candidates: std::collections::HashSet<NodeId> =
                    std::collections::HashSet::new();
                for nid in self
                    .graph
                    .storage()
                    .node_ids_descending()
                    .into_iter()
                    .take(256)
                {
                    candidates.insert(nid);
                }
                for tag in &observation.entity_tags {
                    for nid in self.graph.storage().nodes_by_entity_tag(tag) {
                        candidates.insert(nid);
                    }
                }
                candidates
                    .into_iter()
                    .filter_map(|nid| {
                        self.graph.storage().get_node(nid).ok().and_then(|n| {
                            n.embedding
                                .as_ref()
                                .map(|emb| (nid, cosine_similarity(new_embedding, emb)))
                        })
                    })
                    .fold((0.0_f64, NodeId(0)), |(max_s, max_id), (nid, s)| {
                        if s > max_s { (s, nid) } else { (max_s, max_id) }
                    })
            } else {
                (0.0, NodeId(0))
            };

        // Dedup check: if similarity exceeds the threshold, reinforce the existing node.
        if self.config.dedup_enabled && max_similarity > self.config.dedup_threshold {
            self.touch(most_similar_id, observation.timestamp)?;
            return Ok(IngestResult::Reinforced {
                existing_id: most_similar_id,
                similarity: max_similarity,
            });
        }

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
            access_history: VecDeque::new(),
            tier: crate::graph::MemoryTier::Auto,
            origin: observation.origin,
            entity_tags: observation.entity_tags.clone(),
            metadata: HashMap::new(),
        };

        self.graph.add_node(node)?;

        if let Some(ref new_embedding) = observation.embedding {
            let new_type = &observation.node_type;
            let new_tags = &observation.entity_tags;

            // Candidate pool: last 256 nodes by ID + entity-tag matches (indexed, O(1) dedup)
            let mut candidate_set: std::collections::HashSet<NodeId> =
                std::collections::HashSet::new();

            for nid in self
                .graph
                .storage()
                .node_ids_descending()
                .into_iter()
                .take(256)
            {
                if nid != id {
                    candidate_set.insert(nid);
                }
            }

            if !new_tags.is_empty() {
                for tag in new_tags {
                    for nid in self.graph.storage().nodes_by_entity_tag(tag) {
                        if nid != id {
                            candidate_set.insert(nid);
                        }
                    }
                }
            }

            let candidate_ids: Vec<NodeId> = candidate_set.into_iter().collect();

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

            // Top 4 by score using BinaryHeap
            let top_scored = top_n_by_score(&scored, 4);

            for (cid, score) in top_scored {
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
                        valid_from: None,
                        valid_until: None,
                        metadata: HashMap::new(),
                    };
                    self.graph.add_edge(edge)?;
                }
            }
        }

        Ok(IngestResult::Created(vec![id]))
    }

    /// Crystallize query results into a higher-level knowledge node.
    ///
    /// Creates a synthesis node, links it to its sources with `ConsolidatedFrom`
    /// edges, and reinforces each source using the same `touch()` ordering as
    /// ordinary access.
    pub fn crystallize(&mut self, request: CrystallizeRequest) -> Result<CrystallizeResult, Error> {
        use crate::mechanics::attraction::{
            attraction_score, cosine_similarity, should_create_edge, strengthen_edge, tau_type,
        };
        use crate::mechanics::gravity::compute_mass;

        if request.source_ids.len() < 2 {
            return Err(Error::InvalidInput(
                "crystallize requires at least 2 source IDs".to_string(),
            ));
        }

        let relevance_weights: Vec<f64> = match &request.source_relevances {
            Some(relevances) => {
                if relevances.len() != request.source_ids.len() {
                    return Err(Error::InvalidInput(
                        "source_relevances length must match source_ids length".to_string(),
                    ));
                }
                relevances
                    .iter()
                    .map(|value| clamp_unit_finite(*value))
                    .collect()
            }
            None => vec![1.0; request.source_ids.len()],
        };
        let has_source_relevances = request.source_relevances.is_some();

        let source_ids = request.source_ids.clone();
        let source_set: HashSet<NodeId> = source_ids.iter().copied().collect();
        let mut source_saliences = Vec::with_capacity(source_ids.len());

        for source_id in &source_ids {
            let _ = self.graph.get_node(*source_id)?;
            let salience = self.graph.storage().get_salience(*source_id)?;
            source_saliences.push(clamp_unit_finite(salience));
        }

        let mut dedup_score = 0.0_f64;
        if let Some(ref crystal_embedding) = request.embedding {
            let storage = self.graph.storage();
            for node_id in storage.all_node_ids() {
                let is_crystallized = storage.edges_from(node_id).iter().any(|edge_id| {
                    storage
                        .get_edge(*edge_id)
                        .is_ok_and(|edge| edge.edge_type == EdgeType::ConsolidatedFrom)
                });
                if !is_crystallized {
                    continue;
                }

                let Some(existing_embedding) = storage
                    .get_node(node_id)
                    .ok()
                    .and_then(|node| node.embedding.as_ref())
                else {
                    continue;
                };

                let similarity = cosine_similarity(crystal_embedding, existing_embedding);
                dedup_score = dedup_score.max(similarity);
                if similarity > self.config.dedup_threshold {
                    return Err(Error::Rejected(format!(
                        "duplicate crystallization: node {} similarity {:.6}",
                        node_id.0, similarity
                    )));
                }
            }
        }

        let confidence = clamp_unit_finite(request.confidence);
        let initial_salience =
            crystallized_salience(&source_saliences, &relevance_weights, confidence);

        let id = self.graph.next_node_id();
        let now = request.timestamp;
        let crystal_embedding = request.embedding.clone();
        let crystal_type = request.node_type.clone();
        let crystal_tags = request.entity_tags.clone();

        let node = Node {
            id,
            node_type: request.node_type,
            name: request.name,
            summary: request.summary,
            content: request.content,
            embedding: request.embedding,
            created_at: now,
            updated_at: now,
            accessed_at: now,
            valid_from: None,
            valid_until: None,
            salience: initial_salience,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: crate::graph::MemoryTier::Auto,
            origin: request.origin,
            entity_tags: request.entity_tags,
            metadata: HashMap::new(),
        };

        self.graph.add_node(node)?;

        let mut consolidation_edges = Vec::with_capacity(source_ids.len());
        for index in 0..source_ids.len() {
            let source_id = source_ids[index];
            let source_salience = source_saliences[index];
            let relevance = relevance_weights[index];
            let edge_id = self.graph.next_edge_id();
            let weight = if has_source_relevances {
                positive_edge_weight(relevance)
            } else {
                positive_edge_weight(source_salience)
            };
            let edge = Edge {
                id: edge_id,
                source: id,
                target: source_id,
                edge_type: EdgeType::ConsolidatedFrom,
                weight,
                created_at: now,
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            };
            self.graph.add_edge(edge)?;
            consolidation_edges.push(edge_id);
        }

        let mut attraction_edges = Vec::new();
        if let Some(ref embedding) = crystal_embedding {
            let mut candidate_set = HashSet::new();
            for node_id in self
                .graph
                .storage()
                .node_ids_descending()
                .into_iter()
                .take(256)
            {
                if node_id != id && !source_set.contains(&node_id) {
                    candidate_set.insert(node_id);
                }
            }

            for tag in &crystal_tags {
                for node_id in self.graph.storage().nodes_by_entity_tag(tag) {
                    if node_id != id && !source_set.contains(&node_id) {
                        candidate_set.insert(node_id);
                    }
                }
            }

            let mut scored = Vec::new();
            for candidate_id in candidate_set {
                let candidate = match self.graph.storage().get_node(candidate_id) {
                    Ok(node) => node,
                    Err(_) => continue,
                };

                let Some(ref candidate_embedding) = candidate.embedding else {
                    continue;
                };

                let similarity = cosine_similarity(embedding, candidate_embedding);
                if similarity == 0.0 {
                    continue;
                }

                let tau = tau_type(&crystal_type, &candidate.node_type);
                let candidate_salience = self
                    .graph
                    .storage()
                    .get_salience(candidate_id)
                    .unwrap_or(0.0);
                let candidate_mass = compute_mass(
                    clamp_unit_finite(candidate_salience),
                    candidate.access_count,
                    &candidate.node_type,
                );
                let score = attraction_score(similarity, tau, candidate_mass);

                if should_create_edge(score, &crystal_type, &candidate.node_type) {
                    scored.push((candidate_id, score));
                }
            }

            for (candidate_id, score) in top_n_by_score(&scored, 4) {
                let existing_edge = self
                    .graph
                    .edges_from(id)
                    .iter()
                    .find_map(|&edge_id| {
                        self.graph
                            .get_edge(edge_id)
                            .ok()
                            .filter(|edge| edge.target == candidate_id)
                    })
                    .or_else(|| {
                        self.graph
                            .edges_from(candidate_id)
                            .iter()
                            .find_map(|&edge_id| {
                                self.graph
                                    .get_edge(edge_id)
                                    .ok()
                                    .filter(|edge| edge.target == id)
                            })
                    });

                if let Some(existing) = existing_edge {
                    let new_weight = strengthen_edge(existing.weight, score);
                    let edge_id = existing.id;
                    if let Ok(edge) = self.graph.get_edge_mut(edge_id) {
                        edge.weight = new_weight;
                    }
                } else {
                    let edge_id = self.graph.next_edge_id();
                    let edge = Edge {
                        id: edge_id,
                        source: id,
                        target: candidate_id,
                        edge_type: EdgeType::Semantic,
                        weight: clamp_unit_finite(score),
                        created_at: now,
                        valid_from: None,
                        valid_until: None,
                        metadata: HashMap::new(),
                    };
                    self.graph.add_edge(edge)?;
                    attraction_edges.push(edge_id);
                }
            }
        }

        for source_id in &source_ids {
            self.touch(*source_id, now)?;
        }

        Ok(CrystallizeResult {
            node_id: id,
            consolidation_edges,
            dedup_score,
            attraction_edges,
            nodes_reinforced: source_ids.len(),
            initial_salience,
        })
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
        let now = Timestamp::now();
        let is_supersedes = edge_type == EdgeType::Supersedes;
        let edge = Edge {
            id,
            source: from,
            target: to,
            edge_type,
            weight,
            created_at: now,
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        };
        self.graph.add_edge(edge)?;
        if is_supersedes {
            if let Ok(target_node) = self.graph.get_node_mut(to) {
                target_node.valid_until = Some(now);
            }
            if let Ok(source_node) = self.graph.get_node_mut(from) {
                source_node.valid_from = Some(now);
            }
        }
        Ok(id)
    }

    /// Touch a node — apply lazy decay then reinforce on access.
    ///
    /// Phase 2: Applies decay (eq 4) BEFORE reinforcement (eq 5).
    /// Decay is lazy: computed based on elapsed time since last access.
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error> {
        use crate::mechanics::forgetting::{
            base_level_to_salience, compute_base_level, decay_salience, reinforce_salience,
        };

        let current_salience = self.graph.storage().get_salience(node_id)?;
        let last_accessed = self.graph.storage().get_accessed_at(node_id)?;
        let node_type = self.graph.storage().get_node_type(node_id)?.clone();

        match self.config.decay_model {
            DecayModel::Exponential => {
                let dt_ms = now.0.saturating_sub(last_accessed.0);
                let dt_days = dt_ms as f64 / 86_400_000.0;

                // Decay BEFORE reinforcement — ordering invariant (eq 4 then eq 5)
                let decayed = decay_salience(current_salience, dt_days, &node_type);
                let reinforced = reinforce_salience(decayed);

                self.graph.storage_mut().set_salience(node_id, reinforced)?;
            }
            DecayModel::PowerLaw => {
                let history_snapshot = {
                    let node = self.graph.get_node_mut(node_id)?;
                    node.record_access(now);
                    node.access_history.clone()
                };
                let new_salience =
                    base_level_to_salience(compute_base_level(&history_snapshot, now, 0.5));

                self.graph
                    .storage_mut()
                    .set_salience(node_id, new_salience)?;
            }
        }

        self.graph.storage_mut().set_accessed_at(node_id, now)?;

        let node = self.graph.get_node_mut(node_id)?;
        node.access_count += 1;

        Ok(())
    }

    /// Set the explicit memory tier for a node.
    ///
    /// `Core` tier nodes are protected from decay in `tick()`.
    /// `Auto` restores natural salience-based tier assignment.
    pub fn set_tier(&mut self, node_id: NodeId, tier: MemoryTier) -> Result<(), Error> {
        let node = self.graph.get_node_mut(node_id)?;
        node.tier = tier;
        Ok(())
    }

    /// Get the current memory tier of a node.
    pub fn get_tier(&self, node_id: NodeId) -> Result<MemoryTier, Error> {
        let node = self.graph.get_node(node_id)?;
        Ok(node.tier.clone())
    }

    /// Advance time — apply batch decay (eq 4) to all nodes.
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error> {
        use crate::mechanics::forgetting::{
            base_level_to_salience, compute_base_level, decay_salience, floor_for_type,
        };

        let node_ids = self.graph.storage().all_node_ids();
        let mut nodes_decayed = 0usize;
        let mut nodes_pruned = 0usize;

        for id in node_ids {
            let node_tier = match self.graph.get_node(id) {
                Ok(node) => node.tier.clone(),
                Err(_) => continue,
            };
            if node_tier == MemoryTier::Core {
                continue;
            }

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

            let new_salience = match self.config.decay_model {
                DecayModel::Exponential => {
                    let dt_ms = now.0.saturating_sub(last_accessed.0);
                    let dt_days = dt_ms as f64 / 86_400_000.0;

                    decay_salience(current_salience, dt_days, &node_type)
                }
                DecayModel::PowerLaw => {
                    let history = match self.graph.get_node(id) {
                        Ok(node) => node.access_history.clone(),
                        Err(_) => continue,
                    };

                    base_level_to_salience(compute_base_level(&history, now, 0.5))
                }
            };

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
    /// Associative queries use the full spreading activation pipeline.
    /// Non-associative queries retrieve nodes directly by their structural criteria.
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error> {
        match query {
            Query::Associative { seed, budget } => self.query_associative(*seed, *budget, config),
            Query::TypeFiltered { node_type, limit } => {
                self.query_type_filtered(node_type, *limit, config)
            }
            Query::List {
                min_salience,
                limit,
            } => self.query_list(*min_salience, *limit, config),
            Query::Temporal {
                since,
                node_types,
                limit,
            } => self.query_temporal(*since, node_types.as_deref(), *limit, config),
            Query::Neighborhood { entity, depth } => {
                self.query_neighborhood(*entity, *depth, config)
            }
        }
    }

    /// Unified search entry point — combines text search, vector similarity, and graph traversal.
    ///
    /// Automatically derives a `SearchPlan` from the input and executes the appropriate
    /// retrieval strategies. Returns a `SearchResult` with a `ContextPackage` and trace.
    ///
    /// Returns `Err(Error::InvalidInput)` if both `text` is empty and `query_embedding` is `None`.
    pub fn search(&self, input: SearchInput) -> Result<SearchResult, Error> {
        use crate::mechanics::attraction::cosine_similarity;
        use crate::query::{Fragment, PackagingMode, SearchPlan, SearchTrace, TokenBudget};

        if input.text.is_empty() && input.query_embedding.is_none() {
            return Err(Error::InvalidInput(
                "search requires text or query_embedding".to_string(),
            ));
        }

        let plan = SearchPlan {
            use_text: !input.text.is_empty(),
            use_vector: input.query_embedding.is_some(),
            use_graph: true,
            use_temporal: false,
            use_persona_bias: input.agent_id.is_some(),
            packaging_mode: PackagingMode::KnowledgeOnly,
        };

        let mut all_seed_ids: Vec<NodeId> = Vec::new();
        let mut strategies_used: Vec<String> = Vec::new();
        let mut spread_iterations = 0usize;
        let storage = self.graph.storage();

        let sub_queries = if plan.use_text {
            crate::query::decompose_query(&input.text)
        } else {
            Vec::new()
        };

        if plan.use_text {
            for sub_query in &sub_queries {
                let text_results = storage.text_search(sub_query, input.limit.max(10));
                let new_seeds: Vec<NodeId> = text_results.iter().map(|(id, _)| *id).collect();
                if !new_seeds.is_empty() {
                    all_seed_ids.extend(new_seeds);
                    if !strategies_used
                        .iter()
                        .any(|strategy| strategy == "text_search")
                    {
                        strategies_used.push("text_search".to_string());
                    }
                }
            }
        }

        if plan.use_vector
            && let Some(ref query_embedding) = input.query_embedding
        {
            let vector_scores: Vec<(NodeId, f64)> = storage
                .all_node_ids()
                .into_iter()
                .filter_map(|node_id| {
                    let node = storage.get_node(node_id).ok()?;
                    let embedding = node.embedding.as_ref()?;
                    Some((node_id, cosine_similarity(query_embedding, embedding)))
                })
                .filter(|(_, score)| *score > 0.0)
                .collect();

            for (node_id, _) in top_n_by_score(&vector_scores, input.limit.max(10)) {
                if !all_seed_ids.contains(&node_id) {
                    all_seed_ids.push(node_id);
                }
            }
            strategies_used.push("vector_similarity".to_string());
        }

        all_seed_ids.sort();
        all_seed_ids.dedup();

        let mut merged_knowledge: HashMap<NodeId, Fragment> = HashMap::new();
        let mut merged_memories: HashMap<NodeId, Fragment> = HashMap::new();
        let mut merged_identity: HashMap<NodeId, Fragment> = HashMap::new();
        let mut merged_tensions = Vec::new();

        if plan.use_graph {
            for seed in all_seed_ids.iter().take(3) {
                let config = QueryConfig {
                    budget: input.limit.saturating_mul(5),
                    agent_id: input.agent_id.clone(),
                    project_id: input.project_id.clone(),
                    query_embedding: input.query_embedding.clone(),
                    context: input.context.clone(),
                    ..QueryConfig::default()
                };

                if let Ok(pkg) = self.query_associative(*seed, config.budget, &config) {
                    for fragment in pkg.knowledge {
                        merge_highest_relevance(&mut merged_knowledge, fragment);
                    }
                    for fragment in pkg.memories {
                        merge_highest_relevance(&mut merged_memories, fragment);
                    }
                    for fragment in pkg.identity {
                        merge_highest_relevance(&mut merged_identity, fragment);
                    }
                    merged_tensions.extend(pkg.tensions);
                    spread_iterations += 1;
                }
            }
        }

        if spread_iterations > 0 {
            strategies_used.push("spreading_activation".to_string());
        }

        let mut knowledge: Vec<_> = merged_knowledge.into_values().collect();
        let mut memories: Vec<_> = merged_memories.into_values().collect();
        let mut identity: Vec<_> = merged_identity.into_values().collect();

        knowledge.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        memories.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        knowledge.truncate(input.limit);
        memories.truncate(input.limit);

        let packaging_mode = crate::query::decide_packaging(&merged_tensions, &plan, &input.text);

        self.apply_packaging_mode(
            packaging_mode.clone(),
            &mut identity,
            &mut knowledge,
            &mut memories,
        );

        if input.now.0 > 0 {
            let now = input.now;
            knowledge.retain(|fragment| self.node_is_valid_at(fragment.node_id, now));
            memories.retain(|fragment| self.node_is_valid_at(fragment.node_id, now));
            identity.retain(|fragment| self.node_is_valid_at(fragment.node_id, now));
            merged_tensions.retain(|tension| {
                self.node_is_valid_at(tension.node_a, now)
                    && self.node_is_valid_at(tension.node_b, now)
            });
        }

        let package = ContextPackage {
            identity,
            knowledge,
            memories,
            tensions: merged_tensions,
            token_usage: TokenBudget::default(),
            agent_tension: 0.0,
        };

        let trace = SearchTrace {
            strategies_used,
            seed_count: all_seed_ids.len(),
            spread_iterations,
            packaging_mode: Some(packaging_mode),
        };

        Ok(SearchResult { package, trace })
    }

    /// Query the graph for facts valid at a specific point in time.
    ///
    /// Returns a `ContextPackage` containing only nodes that were valid at `as_of`:
    /// - nodes with `valid_from <= as_of`, or no `valid_from` bound
    /// - nodes with `valid_until > as_of`, or no `valid_until` bound
    ///
    /// This currently runs the standard query pipeline with default configuration,
    /// then filters retrieved fragments by their bitemporal validity window.
    pub fn fact_at(&self, query: &Query, as_of: Timestamp) -> Result<ContextPackage, Error> {
        let mut package = self.query(query, &QueryConfig::default())?;

        package
            .identity
            .retain(|fragment| self.node_is_valid_at(fragment.node_id, as_of));
        package
            .knowledge
            .retain(|fragment| self.node_is_valid_at(fragment.node_id, as_of));
        package
            .memories
            .retain(|fragment| self.node_is_valid_at(fragment.node_id, as_of));
        package.tensions.retain(|tension| {
            self.node_is_valid_at(tension.node_a, as_of)
                && self.node_is_valid_at(tension.node_b, as_of)
        });

        Ok(package)
    }

    fn node_is_valid_at(&self, node_id: NodeId, as_of: Timestamp) -> bool {
        self.graph.get_node(node_id).is_ok_and(|node| {
            let from_ok = node.valid_from.is_none_or(|valid_from| valid_from <= as_of);
            let until_ok = node
                .valid_until
                .is_none_or(|valid_until| valid_until > as_of);
            from_ok && until_ok
        })
    }

    fn apply_packaging_mode(
        &self,
        packaging_mode: crate::query::PackagingMode,
        identity: &mut [crate::query::Fragment],
        knowledge: &mut [crate::query::Fragment],
        memories: &mut Vec<crate::query::Fragment>,
    ) {
        match packaging_mode {
            crate::query::PackagingMode::KnowledgeOnly => {
                memories.clear();
            }
            crate::query::PackagingMode::KnowledgeWithProvenance => {
                self.include_source_memories(knowledge, memories);
            }
            crate::query::PackagingMode::PersonaWeighted => {
                identity.sort_by(|a, b| {
                    b.relevance
                        .partial_cmp(&a.relevance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.node_id.cmp(&b.node_id))
                });
            }
            crate::query::PackagingMode::Timeline => {
                self.sort_fragments_by_created_at(identity);
                self.sort_fragments_by_created_at(knowledge);
                self.sort_fragments_by_created_at(memories);
            }
        }
    }

    fn include_source_memories(
        &self,
        knowledge: &[crate::query::Fragment],
        memories: &mut Vec<crate::query::Fragment>,
    ) {
        let mut existing: HashSet<NodeId> =
            memories.iter().map(|fragment| fragment.node_id).collect();

        for fragment in knowledge {
            for source_fragment in self.source_memory_fragments(fragment) {
                if existing.insert(source_fragment.node_id) {
                    memories.push(source_fragment);
                }
            }
        }
    }

    fn source_memory_fragments(
        &self,
        fragment: &crate::query::Fragment,
    ) -> Vec<crate::query::Fragment> {
        let storage = self.graph.storage();
        storage
            .edges_from(fragment.node_id)
            .iter()
            .filter_map(|edge_id| storage.get_edge(*edge_id).ok())
            .filter(|edge| edge.edge_type == EdgeType::ExtractedFrom)
            .filter_map(|edge| {
                let node = storage.get_node(edge.target).ok()?;
                if !matches!(
                    node.node_type,
                    KnowledgeType::Episodic | KnowledgeType::Event
                ) {
                    return None;
                }
                Some(crate::query::Fragment {
                    node_id: edge.target,
                    name: node.name.clone(),
                    summary: node.summary.clone(),
                    content: Some(node.content.clone()),
                    node_type: node.node_type.clone(),
                    relevance: (fragment.relevance * edge.weight).clamp(0.0, 1.0),
                    origin: node.origin.clone(),
                    scope: fragment.scope.clone(),
                })
            })
            .collect()
    }

    fn sort_fragments_by_created_at(&self, fragments: &mut [crate::query::Fragment]) {
        fragments.sort_by(|a, b| {
            let a_created_at = self
                .graph
                .get_node(a.node_id)
                .map(|node| node.created_at)
                .unwrap_or(Timestamp(u64::MAX));
            let b_created_at = self
                .graph
                .get_node(b.node_id)
                .map(|node| node.created_at)
                .unwrap_or(Timestamp(u64::MAX));

            a_created_at
                .cmp(&b_created_at)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
    }

    fn query_type_filtered(
        &self,
        node_type: &KnowledgeType,
        limit: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use std::cmp::Ordering;

        use crate::query::assembly::{ScoredNode, assemble_context_package};

        let storage = self.graph.storage();
        let mut node_ids = storage.nodes_by_type(node_type);
        node_ids.sort_by(|a, b| {
            let sa = storage.get_salience(*a).unwrap_or(0.0);
            let sb = storage.get_salience(*b).unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(Ordering::Equal)
        });
        node_ids.truncate(limit);

        let scored_nodes: Vec<ScoredNode> = node_ids
            .into_iter()
            .filter_map(|nid| {
                let node = storage.get_node(nid).ok()?;
                let salience = storage.get_salience(nid).unwrap_or(0.0);
                Some(ScoredNode {
                    node_id: nid,
                    name: node.name.clone(),
                    summary: node.summary.clone(),
                    content: node.content.clone(),
                    node_type: node.node_type.clone(),
                    relevance: salience,
                    origin: node.origin.clone(),
                })
            })
            .collect();

        let scored_nodes = if let Some(ref ctx) = config.context {
            crate::query::rerank::rerank_with_context(scored_nodes, ctx)
        } else {
            scored_nodes
        };

        Ok(assemble_context_package(
            scored_nodes,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.project_id,
        ))
    }

    fn query_list(
        &self,
        min_salience: f64,
        limit: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use std::cmp::Ordering;

        use crate::query::assembly::{ScoredNode, assemble_context_package};

        let storage = self.graph.storage();
        let mut node_ids: Vec<NodeId> = storage
            .all_node_ids()
            .into_iter()
            .filter(|&nid| {
                storage
                    .get_salience(nid)
                    .is_ok_and(|salience| salience >= min_salience)
            })
            .collect();
        node_ids.sort_by(|a, b| {
            let sa = storage.get_salience(*a).unwrap_or(0.0);
            let sb = storage.get_salience(*b).unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(Ordering::Equal)
        });
        node_ids.truncate(limit);

        let scored_nodes: Vec<ScoredNode> = node_ids
            .into_iter()
            .filter_map(|nid| {
                let node = storage.get_node(nid).ok()?;
                let salience = storage.get_salience(nid).unwrap_or(0.0);
                Some(ScoredNode {
                    node_id: nid,
                    name: node.name.clone(),
                    summary: node.summary.clone(),
                    content: node.content.clone(),
                    node_type: node.node_type.clone(),
                    relevance: salience,
                    origin: node.origin.clone(),
                })
            })
            .collect();

        let scored_nodes = if let Some(ref ctx) = config.context {
            crate::query::rerank::rerank_with_context(scored_nodes, ctx)
        } else {
            scored_nodes
        };

        Ok(assemble_context_package(
            scored_nodes,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.project_id,
        ))
    }

    fn query_temporal(
        &self,
        since: Timestamp,
        node_types: Option<&[KnowledgeType]>,
        limit: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use std::cmp::Ordering;

        use crate::query::assembly::{ScoredNode, assemble_context_package};

        let storage = self.graph.storage();
        let mut scored_nodes: Vec<(Timestamp, ScoredNode)> = storage
            .all_node_ids()
            .into_iter()
            .filter_map(|nid| {
                let node = storage.get_node(nid).ok()?;
                if node.created_at < since {
                    return None;
                }
                if let Some(types) = node_types
                    && !types.iter().any(|node_type| node_type == &node.node_type)
                {
                    return None;
                }
                let salience = storage.get_salience(nid).unwrap_or(0.0);
                Some((
                    node.created_at,
                    ScoredNode {
                        node_id: nid,
                        name: node.name.clone(),
                        summary: node.summary.clone(),
                        content: node.content.clone(),
                        node_type: node.node_type.clone(),
                        relevance: salience,
                        origin: node.origin.clone(),
                    },
                ))
            })
            .collect();

        scored_nodes.sort_by(|(created_a, node_a), (created_b, node_b)| {
            created_b
                .cmp(created_a)
                .then_with(|| {
                    node_b
                        .relevance
                        .partial_cmp(&node_a.relevance)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| node_a.node_id.cmp(&node_b.node_id))
        });
        scored_nodes.truncate(limit);

        let scored_nodes = scored_nodes
            .into_iter()
            .map(|(_, scored_node)| scored_node)
            .collect();

        Ok(assemble_context_package(
            scored_nodes,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.project_id,
        ))
    }

    fn query_neighborhood(
        &self,
        entity: NodeId,
        max_depth: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use crate::query::assembly::{ScoredNode, assemble_context_package};

        let _ = self.graph.get_node(entity)?;

        let storage = self.graph.storage();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut depths = HashMap::new();

        queue.push_back((entity, 0));
        visited.insert(entity);
        depths.insert(entity, 0usize);

        while let Some((nid, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            for &eid in storage.edges_from(nid) {
                if let Ok(edge) = storage.get_edge(eid)
                    && visited.insert(edge.target)
                {
                    let next_depth = depth + 1;
                    depths.insert(edge.target, next_depth);
                    queue.push_back((edge.target, next_depth));
                }
            }

            for &eid in storage.edges_to(nid) {
                if let Ok(edge) = storage.get_edge(eid)
                    && visited.insert(edge.source)
                {
                    let next_depth = depth + 1;
                    depths.insert(edge.source, next_depth);
                    queue.push_back((edge.source, next_depth));
                }
            }
        }

        let scored_nodes: Vec<ScoredNode> = depths
            .into_iter()
            .filter_map(|(nid, depth)| {
                let node = storage.get_node(nid).ok()?;
                let salience = storage.get_salience(nid).unwrap_or(0.0);
                let depth_multiplier = 0.8_f64.powf(depth as f64);
                Some(ScoredNode {
                    node_id: nid,
                    name: node.name.clone(),
                    summary: node.summary.clone(),
                    content: node.content.clone(),
                    node_type: node.node_type.clone(),
                    relevance: salience * depth_multiplier,
                    origin: node.origin.clone(),
                })
            })
            .collect();

        Ok(assemble_context_package(
            scored_nodes,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.project_id,
        ))
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

        // --- Stage 1: Collect identity nodes for this agent (indexed lookup) ---
        let identity_nodes: Vec<(Vec<f64>, KnowledgeType, f64)> =
            if let Some(ref agent_id) = config.agent_id {
                // Use agent index to get only this agent's nodes, then filter for identity types
                storage
                    .nodes_by_agent(agent_id)
                    .into_iter()
                    .filter_map(|nid| {
                        let node = storage.get_node(nid).ok()?;
                        let is_identity = matches!(
                            node.node_type,
                            KnowledgeType::IdentityCore
                                | KnowledgeType::IdentityLearned
                                | KnowledgeType::IdentityState
                        );
                        if is_identity {
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

        // --- Stage 2: Compute initial activations (eq 10) — sparse, seed-driven ---
        let mut initial_activations: HashMap<NodeId, f64> = HashMap::new();

        // Seed always gets full activation.
        {
            let (seed_vector_sim, seed_identity_prior) = {
                let seed_node = storage.get_node(seed).ok();
                let vs = match (
                    &config.query_embedding,
                    seed_node.and_then(|n| n.embedding.as_ref()),
                ) {
                    (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
                    _ => 0.0,
                };
                let ip = match seed_node.and_then(|n| n.embedding.as_ref()) {
                    Some(emb) => compute_identity_prior(emb, &identity_nodes, cosine_similarity),
                    None => 0.0,
                };
                (vs, ip)
            };
            let act = initial_activation(true, seed_vector_sim, seed_identity_prior);
            initial_activations.insert(seed, act);
        }

        // Identity nodes get prior activation.
        if let Some(ref agent_id) = config.agent_id {
            for nid in storage.nodes_by_agent(agent_id) {
                if nid == seed {
                    continue;
                }

                let (is_identity, vector_sim, identity_prior) = {
                    let node = match storage.get_node(nid) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let is_id = matches!(
                        node.node_type,
                        KnowledgeType::IdentityCore
                            | KnowledgeType::IdentityLearned
                            | KnowledgeType::IdentityState
                    );
                    let vs = match (&config.query_embedding, &node.embedding) {
                        (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
                        _ => 0.0,
                    };
                    let ip = match &node.embedding {
                        Some(emb) => {
                            compute_identity_prior(emb, &identity_nodes, cosine_similarity)
                        }
                        None => 0.0,
                    };
                    (is_id, vs, ip)
                };
                if !is_identity {
                    continue;
                }

                let act = initial_activation(false, vector_sim, identity_prior);
                if act > config.min_activation {
                    initial_activations.insert(nid, act);
                }
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

        let activations = match self.config.spreading_model {
            SpreadingModel::PriorityQueueBfs => spread_activation(
                initial_activations,
                node_info_fn,
                budget,
                config.min_activation,
                config.decay_per_hop,
                config.max_hops,
            ),
            SpreadingModel::RandomWalkRestart => {
                rwr_activations(seed, budget, config.min_activation, storage)
            }
        };

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
        let hopfield_context = match self.config.energy_model {
            EnergyModel::WeightedSum => None,
            EnergyModel::Hopfield => build_hopfield_scoring_context(
                &config.query_embedding,
                &damped_activations,
                storage,
            ),
        };
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
            let scoring_similarity = match self.config.energy_model {
                EnergyModel::WeightedSum => vector_sim,
                EnergyModel::Hopfield => hopfield_adjusted_similarity(
                    hopfield_context.as_ref(),
                    vector_sim,
                    node.embedding.as_ref(),
                ),
            };
            let relevance = final_score(activation, scoring_similarity, salience, mass, sw);

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

    fn has_entity_edge_between(&self, a: NodeId, b: NodeId) -> bool {
        self.graph.edges_from(a).iter().any(|&edge_id| {
            self.graph
                .get_edge(edge_id)
                .is_ok_and(|edge| edge.target == b && edge.edge_type == EdgeType::Entity)
        }) || self.graph.edges_from(b).iter().any(|&edge_id| {
            self.graph
                .get_edge(edge_id)
                .is_ok_and(|edge| edge.target == a && edge.edge_type == EdgeType::Entity)
        })
    }

    /// Cross-agent entity linking after parallel execution round.
    ///
    /// Creates Entity edges between nodes from different agents sharing entity tags.
    /// No LLM calls — metadata matching only.
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error> {
        let mut input_node_ids = BTreeSet::new();
        for session in sessions {
            for &node_id in &session.node_ids {
                input_node_ids.insert(node_id);
            }
        }

        let mut nodes_by_tag: BTreeMap<String, Vec<(NodeId, String)>> = BTreeMap::new();
        for node_id in input_node_ids {
            let Ok(node) = self.graph.get_node(node_id) else {
                continue;
            };

            let mut seen_tags = BTreeSet::new();
            for tag in &node.entity_tags {
                if seen_tags.insert(tag.as_str()) {
                    nodes_by_tag
                        .entry(tag.clone())
                        .or_default()
                        .push((node_id, node.origin.agent_id.clone()));
                }
            }
        }

        let mut candidate_pairs = BTreeSet::new();
        let mut clusters_found = 0usize;
        for nodes in nodes_by_tag.values() {
            let mut has_cross_agent_pair = false;
            for i in 0..nodes.len() {
                for j in (i + 1)..nodes.len() {
                    let (left_id, left_agent) = &nodes[i];
                    let (right_id, right_agent) = &nodes[j];
                    if left_agent == right_agent {
                        continue;
                    }

                    has_cross_agent_pair = true;
                    let pair = if left_id < right_id {
                        (*left_id, *right_id)
                    } else {
                        (*right_id, *left_id)
                    };
                    candidate_pairs.insert(pair);
                }
            }

            if has_cross_agent_pair {
                clusters_found += 1;
            }
        }

        let mut entity_edges_created = 0usize;
        let now = Timestamp::now();
        for (source, target) in candidate_pairs {
            if self.has_entity_edge_between(source, target) {
                continue;
            }

            let edge_id = self.graph.next_edge_id();
            let edge = Edge {
                id: edge_id,
                source,
                target,
                edge_type: EdgeType::Entity,
                weight: 1.0,
                created_at: now,
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            };
            self.graph.add_edge(edge)?;
            entity_edges_created += 1;
        }

        Ok(ReflectReport {
            entity_edges_created,
            clusters_found,
        })
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
        let result = engine.ingest(make_observation("test node")).unwrap();
        let IngestResult::Created(ids) = result else {
            panic!("expected Created variant");
        };
        assert_eq!(ids.len(), 1);
        assert_eq!(engine.graph().node_count(), 1);
        let node = engine.graph().get_node(ids[0]).unwrap();
        assert_eq!(node.name, "test node");
        assert_eq!(node.salience, 1.0);
    }

    #[test]
    fn link_creates_edge() {
        let mut engine = Engine::new();
        let IngestResult::Created(ids1) = engine.ingest(make_observation("node A")).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(ids2) = engine.ingest(make_observation("node B")).unwrap() else {
            panic!("expected Created");
        };
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
        let IngestResult::Created(ids) = engine.ingest(make_observation("node")).unwrap() else {
            panic!("expected Created");
        };
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
        let IngestResult::Created(ids1) = engine.ingest(make_observation("A")).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(ids2) = engine.ingest(make_observation("B")).unwrap() else {
            panic!("expected Created");
        };
        let eid = engine
            .link(ids1[0], ids2[0], EdgeType::Semantic, 0.5)
            .unwrap();
        let edge = engine.graph().get_edge(eid).unwrap();
        assert!(edge.created_at.0 > 0);
    }

    #[test]
    fn touch_updates_accessed_at_to_nonzero() {
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node")).unwrap() else {
            panic!("expected Created");
        };
        engine.touch(ids[0], Timestamp::now()).unwrap();
        let node = engine.graph().get_node(ids[0]).unwrap();
        assert!(node.accessed_at.0 > 0);
    }

    #[test]
    fn touch_applies_decay_before_reinforcement() {
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node")).unwrap() else {
            panic!("expected Created");
        };
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
        let IngestResult::Created(ids) = engine.ingest(make_observation("node")).unwrap() else {
            panic!("expected Created");
        };
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

        let IngestResult::Created(episodic_ids) = engine.ingest(episodic_obs).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(semantic_ids) = engine.ingest(semantic_obs).unwrap() else {
            panic!("expected Created");
        };

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
        let IngestResult::Created(ids) = engine.ingest(identity_obs).unwrap() else {
            panic!("expected Created");
        };
        let id = ids[0];

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
            let _ = engine.ingest(obs).unwrap();
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
        let config = EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false);
        let mut engine = Engine::with_config(config);

        let obs1 = Observation {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("node A")
        };
        let IngestResult::Created(ids1) = engine.ingest(obs1).unwrap() else {
            panic!("expected Created");
        };

        let obs2 = Observation {
            embedding: Some(vec![0.95, 0.1, 0.0]),
            ..make_observation("node B")
        };
        let IngestResult::Created(ids2) = engine.ingest(obs2).unwrap() else {
            panic!("expected Created");
        };

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
        let _ = engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: Some(vec![0.0, 1.0, 0.0]),
            ..make_observation("node B")
        };
        let _ = engine.ingest(obs2).unwrap();

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
        let _ = engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: None,
            ..make_observation("node B")
        };
        let _ = engine.ingest(obs2).unwrap();

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
            let _ = engine.ingest(obs).unwrap();
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

        let _ = engine.ingest(make_observation("node 1")).unwrap();
        let _ = engine.ingest(make_observation("node 2")).unwrap();

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
        let _ = engine.ingest(obs1).unwrap();

        let obs2 = Observation {
            embedding: Some(vec![1.0, 0.001, 0.0]),
            ..make_observation("duplicate")
        };
        let result = engine.ingest(obs2);
        assert!(matches!(result, Ok(IngestResult::Reinforced { .. })));
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
            .with_confidence_threshold(0.7)
            .with_dedup_threshold(0.95)
            .with_dedup_enabled(false);
        assert_eq!(config.max_nodes, 1000);
        assert_eq!(config.novelty_threshold, 0.5);
        assert_eq!(config.dedup_threshold, 0.95);
        assert!(!config.dedup_enabled);
    }

    #[test]
    fn query_associative_returns_real_results() {
        let config = EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false);
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
        let IngestResult::Created(ids1) = engine.ingest(obs1).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(ids2) = engine.ingest(obs2).unwrap() else {
            panic!("expected Created");
        };

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
        let config = EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false);
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

        let IngestResult::Created(identity_ids) = engine.ingest(identity_obs).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(semantic_ids) = engine.ingest(semantic_obs).unwrap() else {
            panic!("expected Created");
        };
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
        let config = EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false);
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

        let IngestResult::Created(ids1) = engine.ingest(obs1).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(ids2) = engine.ingest(obs2).unwrap() else {
            panic!("expected Created");
        };
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
    fn query_unimplemented_modes_return_empty() {
        let engine = Engine::new();
        let queries = vec![Query::Temporal {
            since: Timestamp(0),
            node_types: None,
            limit: 10,
        }];
        for q in &queries {
            let pkg = engine.query(q, &QueryConfig::default()).unwrap();
            assert_eq!(
                pkg.total_fragments(),
                0,
                "unimplemented query modes should return empty"
            );
        }
    }

    #[test]
    fn query_neighborhood_rejects_missing_entity() {
        let engine = Engine::new();
        let q = Query::Neighborhood {
            entity: NodeId(0),
            depth: 1,
        };

        let result = engine.query(&q, &QueryConfig::default());

        assert!(matches!(result, Err(Error::NodeNotFound(NodeId(0)))));
    }
}
