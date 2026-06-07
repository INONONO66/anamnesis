//! Public API surface for the Anamnesis cognitive graph engine.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::{AccessTrace, Edge, Graph, Node};
use crate::graph::{EdgeId, EdgeType, KnowledgeType, MemoryTier, NodeId, ScopePath, Timestamp};
use crate::query::{ContextPackage, Query, QueryConfig, SearchInput, SearchResult};
use crate::snapshot::{SnapshotId, SnapshotStore};
use crate::storage::{SqliteStorage, StorageAdapter};

mod search;

const ARCHIVE_SALIENCE_THRESHOLD: f64 = 0.10;
const SALIENCE_CHANGE_EPSILON: f64 = 1e-10;

/// Default readout work tag for a bare `Engine::touch()` access.
///
/// Under the `A_i = B_i + P_i` model a committed access appends a trace (raising
/// the base level `B_i`); there is no scalar access-gain that `readout_work` scales
/// (ADR-0008). The value is retained as the canonical per-touch readout-work tag
/// the commit pipeline carries; the access effect itself is the appended trace.
const ACCESS_READOUT_WORK: f64 = 1.0;

/// Configuration for the Anamnesis engine.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EngineConfig {
    /// Maximum number of nodes before perception gate rejects new observations.
    pub max_nodes: usize,
    /// Separation boundary `theta_sep` [0, 1]: novelty `> theta_sep` allocates a new
    /// site, novelty `<= theta_sep` routes to and reinforces the nearest one
    /// (perception.md). This is not a free knob — its default derives from the encoder
    /// distinct-pair distribution as `theta_sep = 1 - q95`
    /// ([`crate::mechanics::priors::theta_sep`]); the setter exists only to override
    /// the operative boundary in tests and diagnostics.
    pub novelty_threshold: f64,
    /// Minimum confidence [0, 1] for an observation to enter the graph.
    pub confidence_threshold: f64,
    /// Similarity threshold above which ingest reinforces an existing node instead of creating one.
    pub dedup_threshold: f64,
    /// Whether ingest should detect duplicate embeddings and reinforce existing nodes.
    pub dedup_enabled: bool,
    /// Maximum number of mutation events retained for draining. Default: 10,000.
    pub max_events: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_nodes: 100_000,
            // theta_sep is NOT a free knob (perception.md, ADR-0009): it is derived
            // deterministically from the embedding encoder's distinct-pair similarity
            // distribution as `theta_sep = 1 - q95`. The default tracks the encoder
            // constant rather than a hard-coded literal, so it recomputes exactly
            // whenever the encoder changes. `with_novelty_threshold` exists only to
            // override the operative boundary in tests/diagnostics.
            novelty_threshold: crate::mechanics::priors::theta_sep(
                crate::mechanics::priors::ENCODER_DISTINCT_PAIR_Q95,
            ),
            confidence_threshold: 0.50,
            dedup_threshold: 0.92,
            dedup_enabled: true,
            max_events: 10_000,
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

    /// Override the encoder-derived separation boundary `theta_sep`.
    ///
    /// The default already tracks the encoder (`1 - q95`, see
    /// [`crate::mechanics::priors::theta_sep`]); this setter exists only to override
    /// the operative boundary in tests and diagnostics, not as a production knob.
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

    pub fn with_max_events(mut self, max_events: usize) -> Self {
        self.max_events = max_events;
        self
    }
}

fn finite_vector(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

/// Compare two storages field-for-field and return a description of the first
/// per-node or per-edge mismatch, or `None` if they are byte-for-byte equivalent.
///
/// This backs the `snapshot_restore_consistency` invariant: a clone (the
/// "restore") must reproduce identical state, so this checks the full `Node` and
/// `Edge` records (content, summary, embedding, origin, metadata, access_history,
/// evidence_prior, tier, validity, and the bounded projections via their
/// `PartialEq`) plus the SoA reservoir/hot fields read through their accessors
/// (the cached composite `retained_action` `A_i`, `conductance` `C_ij`,
/// `accessed_at`, the dormant `decay_checkpoint`) — fields a struct copy can lag
/// when marked dirty, and the exact class of field the clone path could silently
/// drop. The persistent node substrate (the access-trace history and `P_i`) lives
/// on the `Node` record and is covered by the record `PartialEq`. Float
/// reservoir/hot fields are compared by bit pattern so a dropped column reading
/// back as a default (or a `NaN`) is always caught.
fn snapshot_restore_mismatch<S: StorageAdapter>(original: &S, restored: &S) -> Option<String> {
    let bits_differ = |a: f64, b: f64| a.to_bits() != b.to_bits();

    let orig_node_ids = original.all_node_ids();
    let restored_node_ids = restored.all_node_ids();
    if orig_node_ids != restored_node_ids {
        return Some(format!(
            "node id set differs: original {orig_node_ids:?} vs restored {restored_node_ids:?}"
        ));
    }

    for id in orig_node_ids {
        match (original.get_node(id), restored.get_node(id)) {
            (Ok(a), Ok(b)) if a != b => {
                return Some(format!("node {id:?} record differs after restore"));
            }
            (Ok(_), Ok(_)) => {}
            _ => return Some(format!("node {id:?} not retrievable from both stores")),
        }
        if bits_differ(
            original.get_salience(id).unwrap_or(f64::NAN),
            restored.get_salience(id).unwrap_or(f64::NAN),
        ) {
            return Some(format!("node {id:?} salience differs after restore"));
        }
        if bits_differ(
            original.get_retained_action(id).unwrap_or(f64::NAN),
            restored.get_retained_action(id).unwrap_or(f64::NAN),
        ) {
            return Some(format!("node {id:?} retained_action differs after restore"));
        }
        match (original.get_accessed_at(id), restored.get_accessed_at(id)) {
            (Ok(a), Ok(b)) if a != b => {
                return Some(format!("node {id:?} accessed_at differs after restore"));
            }
            (Ok(_), Ok(_)) => {}
            _ => {
                return Some(format!(
                    "node {id:?} accessed_at not retrievable from both stores"
                ));
            }
        }
        match (
            original.get_decay_checkpoint(id),
            restored.get_decay_checkpoint(id),
        ) {
            (Ok(a), Ok(b)) if a != b => {
                return Some(format!(
                    "node {id:?} decay_checkpoint differs after restore"
                ));
            }
            (Ok(_), Ok(_)) => {}
            _ => {
                return Some(format!(
                    "node {id:?} decay_checkpoint not retrievable from both stores"
                ));
            }
        }
    }

    let orig_edge_ids = original.all_edge_ids();
    let restored_edge_ids = restored.all_edge_ids();
    if orig_edge_ids != restored_edge_ids {
        return Some(format!(
            "edge id set differs: original {orig_edge_ids:?} vs restored {restored_edge_ids:?}"
        ));
    }

    for id in orig_edge_ids {
        match (original.get_edge(id), restored.get_edge(id)) {
            (Ok(a), Ok(b)) if a != b => {
                return Some(format!("edge {id:?} record differs after restore"));
            }
            (Ok(_), Ok(_)) => {}
            _ => return Some(format!("edge {id:?} not retrievable from both stores")),
        }
        if bits_differ(
            original.get_conductance(id).unwrap_or(f64::NAN),
            restored.get_conductance(id).unwrap_or(f64::NAN),
        ) {
            return Some(format!("edge {id:?} conductance differs after restore"));
        }
    }

    None
}

/// Builds the read-only [`CommitTrace`] for a packaged retrieval (ADR-0004).
///
/// Captures, from the settled [`ActivationResponse`](crate::query::ActivationResponse)
/// and the assembled [`ContextPackage`], the work a later
/// [`Engine::commit`](Engine::commit) may integrate:
///
/// - `accessed`: every packaged fragment, with `readout_work = clamp(a_i, 0, 1)`
///   (the settled query-local activation as the readout-work proxy);
/// - `co_readout`: every distinct ordered pair of packaged fragments connected by a
///   live, propagating (non-`Contradicts`) edge, with their activations;
/// - `path_used`: every edge that carried positive path current `I_ij`, snapshotting
///   its `source`/`target`/`edge_type` so commit can detect a stale trace;
/// - `tensions_activated`: every surfaced [`Tension`], with its presented stress.
///
/// Pure read of storage: no reservoir, projection, or timestamp is mutated.
fn build_commit_trace<S: StorageAdapter>(
    storage: &S,
    response: &crate::query::ActivationResponse,
    package: &ContextPackage,
) -> crate::query::CommitTrace {
    use crate::query::{AccessedSite, ActivatedTension, CoReadoutPair, CommitTrace, PathUsedEdge};

    let activation_of = |id: NodeId| response.activation.get(&id).copied().unwrap_or(0.0);

    // Packaged fragment ids, deterministically ordered (identity, knowledge, memories).
    let mut packaged: Vec<NodeId> = Vec::new();
    for frag in package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
    {
        if !packaged.contains(&frag.node_id) {
            packaged.push(frag.node_id);
        }
    }

    // `Accessed`: each packaged site, readout work = bounded settled activation.
    let accessed: Vec<AccessedSite> = packaged
        .iter()
        .map(|&node_id| AccessedSite {
            node_id,
            readout_work: activation_of(node_id).clamp(0.0, 1.0),
        })
        .collect();

    // `CoReadout`: distinct ordered pairs of packaged sites joined by a live,
    // propagating edge. Iterate the stable packaged order; for each pair check both
    // directions; skip excluded `Contradicts` edges (those are tensions, not flux).
    let excluded: HashSet<EdgeId> = response.excluded_edges.iter().copied().collect();
    let mut co_readout: Vec<CoReadoutPair> = Vec::new();
    for (idx, &node_a) in packaged.iter().enumerate() {
        for &node_b in &packaged[idx + 1..] {
            let connected = storage.edges_from(node_a).iter().any(|&eid| {
                !excluded.contains(&eid) && storage.get_edge(eid).is_ok_and(|e| e.target == node_b)
            }) || storage.edges_from(node_b).iter().any(|&eid| {
                !excluded.contains(&eid) && storage.get_edge(eid).is_ok_and(|e| e.target == node_a)
            });
            if connected {
                co_readout.push(CoReadoutPair {
                    node_a,
                    node_b,
                    activation_a: activation_of(node_a),
                    activation_b: activation_of(node_b),
                });
            }
        }
    }

    // `PathUsed`: every edge carrying positive path current, with a topology snapshot.
    let mut path_edges: Vec<EdgeId> = response.path_current.keys().copied().collect();
    path_edges.sort_by_key(|e| e.0);
    let mut path_used: Vec<PathUsedEdge> = Vec::new();
    for edge_id in path_edges {
        let flux = response.path_current.get(&edge_id).copied().unwrap_or(0.0);
        if !flux.is_finite() || flux <= 0.0 {
            continue;
        }
        let Ok(edge) = storage.get_edge(edge_id) else {
            continue;
        };
        path_used.push(PathUsedEdge {
            edge_id,
            source: edge.source,
            target: edge.target,
            edge_type: edge.edge_type.clone(),
            flux,
        });
    }

    // `TensionActivated`: each surfaced contradiction with its presented stress.
    let tensions_activated: Vec<ActivatedTension> = package
        .tensions
        .iter()
        .map(|t| ActivatedTension {
            node_a: t.node_a,
            node_b: t.node_b,
            stress: t.stress,
        })
        .collect();

    CommitTrace {
        accessed,
        co_readout,
        path_used,
        tensions_activated,
    }
}

/// Builds the query-local readout energy decomposition `E(S | Q)` over the packaged
/// active subsystem (energy.md / ADR-0007).
///
/// The active subsystem `S` is the set of packaged fragments. From the settled
/// [`ActivationResponse`](crate::query::ActivationResponse) this assembles the four
/// structural energy terms:
///
/// - **field_alignment**: per-site `a_i * phi_i`, where `phi_i` is the same embedding
///   alignment the readout score used (cosine of query/site embeddings, else `0`), so
///   the explanation is consistent with the ranking;
/// - **conductive_support**: per-bond `project_weight(C_ij) * min(a_i, a_j)` over the
///   live propagating edges between packaged sites (`Contradicts` excluded);
/// - **impedance_regularization**: per-site `a_i * Z_i` from the response impedance;
/// - **frustration_penalty**: the summed surfaced tension stresses.
///
/// This is **interpretive and never stored** — it explains and ranks the bundle
/// around the RWR stationary vector `a*`, which is the true fixed point. Pure read of
/// storage; nothing is mutated.
fn build_readout_energy<S: StorageAdapter>(
    storage: &S,
    response: &crate::query::ActivationResponse,
    package: &ContextPackage,
    query_embedding: Option<&Vec<f64>>,
) -> crate::mechanics::energy::EnergyTerms {
    use crate::mechanics::attraction::cosine_similarity;
    use crate::mechanics::energy::{SiteBond, SiteEnergy, energy};

    let activation_of = |id: NodeId| response.activation.get(&id).copied().unwrap_or(0.0);

    // The active subsystem S = packaged fragment ids, deterministically ordered.
    let mut packaged: Vec<NodeId> = Vec::new();
    for frag in package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
    {
        if !packaged.contains(&frag.node_id) {
            packaged.push(frag.node_id);
        }
    }

    // Per-site energy contributions: a_i, phi_i (embedding alignment, matching the
    // readout score), Z_i (effective impedance from the settled response).
    let mut sites: Vec<SiteEnergy> = Vec::with_capacity(packaged.len());
    for &id in &packaged {
        let phi = match (
            query_embedding,
            storage.get_node(id).ok().and_then(|n| n.embedding.as_ref()),
        ) {
            (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
            _ => 0.0,
        };
        sites.push(SiteEnergy {
            activation: activation_of(id),
            phi,
            impedance: response.impedance.get(&id).copied().unwrap_or_default(),
        });
    }

    // Conductive bonds between distinct packaged sites joined by a live, propagating
    // (non-`Contradicts`) edge. Each undirected bond is counted once.
    let excluded: HashSet<EdgeId> = response.excluded_edges.iter().copied().collect();
    let mut bonds: Vec<SiteBond> = Vec::new();
    for (idx, &node_a) in packaged.iter().enumerate() {
        for &node_b in &packaged[idx + 1..] {
            let conductance = storage
                .edges_from(node_a)
                .iter()
                .chain(storage.edges_to(node_a).iter())
                .filter(|&&eid| !excluded.contains(&eid))
                .filter_map(|&eid| storage.get_edge(eid).ok())
                .find(|e| {
                    (e.source == node_a && e.target == node_b)
                        || (e.source == node_b && e.target == node_a)
                })
                .map(|e| storage.get_conductance(e.id).unwrap_or(e.conductance));
            if let Some(conductance) = conductance {
                bonds.push(SiteBond {
                    conductance,
                    activation_i: activation_of(node_a),
                    activation_j: activation_of(node_b),
                });
            }
        }
    }

    // Frustration: the surfaced tension stresses over the active subsystem.
    let stresses: Vec<f64> = package.tensions.iter().map(|t| t.stress).collect();

    energy(&sites, &bonds, &stresses)
}

fn ingest_trigger_candidates<S: StorageAdapter>(
    storage: &S,
    entity_tags: &[String],
    exclude: Option<NodeId>,
) -> HashSet<NodeId> {
    let mut candidates = HashSet::new();
    for nid in storage.node_ids_descending_limit(256) {
        if Some(nid) != exclude {
            candidates.insert(nid);
        }
    }
    for tag in entity_tags {
        for nid in storage.nodes_by_entity_tag(tag) {
            if Some(nid) != exclude {
                candidates.insert(nid);
            }
        }
    }
    candidates
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
    /// When the fact represented by this observation became valid. None = always valid.
    ///
    /// Passed through to `Node.valid_from` during ingest. Used by `fact_at()` for
    /// bitemporal filtering.
    pub valid_from: Option<Timestamp>,
    /// When the fact represented by this observation became invalid. None = still valid.
    ///
    /// Passed through to `Node.valid_until` during ingest. Used by `fact_at()` for
    /// bitemporal filtering.
    pub valid_until: Option<Timestamp>,
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
    /// Number of source nodes mutated by crystallization.
    ///
    /// Always `0`: crystallization is additive synthesis and **never mutates its
    /// sources** (overview.md `crystallize` contract, interactions.md
    /// `Crystallized`). Retained for API compatibility.
    pub nodes_reinforced: usize,
    /// Initial salience assigned to the crystal.
    pub initial_salience: f64,
    /// Source consistency score: `clamp(support_density - contradiction_rate, 0.0, 1.0)`.
    pub consistency_score: f64,
    /// Fraction of edges between sources that are `Contradicts`.
    pub contradiction_rate: f64,
    /// Fraction of edges between sources that are supportive
    /// (`Supports`, `ReinforcedBy`, `ConsolidatedFrom`, `ExtractedFrom`, `Entity`, `Reason`).
    pub support_density: f64,
    /// True if any source is already a `ConsolidatedFrom` target of another crystal node.
    pub circular_evidence_warning: bool,
    /// True if all sources share the same `(agent_id, session_id)` pair.
    pub single_source_warning: bool,
}

/// Classification for evidence logged against a debugging hypothesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceResult {
    /// Evidence supports the hypothesis.
    Supports,
    /// Evidence contradicts the hypothesis.
    Contradicts,
    /// Evidence is relevant but does not clearly support or refute.
    Neutral,
    /// Evidence could not be interpreted conclusively.
    Inconclusive,
}

/// Final state recorded for a debugging session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugOutcome {
    /// The investigation resolved with the given conclusion.
    Resolved(String),
    /// The investigation ended unresolved with the given reason.
    Unresolved(String),
    /// The investigation was abandoned without a conclusion.
    Abandoned,
}

/// Report returned by [`Engine::tick`] — the dissipation volume and projection
/// deltas of one maintenance pass (observability.md).
///
/// `tick` decays the retained-action reservoir `A_i` (power-law, ADR-0008) and
/// re-projects `salience = project_salience(A_i)`; this report summarizes how
/// much dissipation occurred and how far the salience projections moved.
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    /// Dissipation volume: number of sites whose salience projection changed.
    pub nodes_decayed: usize,
    /// Number of sites whose salience projection crossed below the archive
    /// threshold this tick. These sites are dormant (hidden from broad retrieval)
    /// but are never deleted (ADR-0006 / ADR-0008).
    pub nodes_pruned: usize,
    /// Total projection delta: summed magnitude of the salience drops across all
    /// decayed sites (`Σ |s_before - s_after|`). Zero when nothing decayed.
    pub total_salience_delta: f64,
    /// Number of edges whose conductance projection dropped this tick from
    /// idle-edge leakage (`C_ij' = C_ij - eta_leak * idle_edge_leakage_ij`,
    /// conductance.md / interactions.md `TimeElapsed`). Unused weak coupling is
    /// drained over time (density control); recently-used and protected edges are
    /// untouched. Edges are never deleted (ADR-0006 / ADR-0008).
    pub edges_leaked: usize,
    /// Total conductance-projection delta: summed magnitude of the edge-weight drops
    /// across all leaked edges (`Σ |w_before - w_after|`). Zero when nothing leaked.
    pub total_conductance_delta: f64,
}

/// Report returned by [`Engine::commit`] — the integrated work the commit recorded.
///
/// Every count names a committed interaction integrated into the reservoirs
/// (ADR-0004 / interactions.md); the commit is the only reservoir-mutation path
/// besides `tick`. Read-only retrieval mutates nothing, so an uncommitted package
/// produces no deltas.
#[derive(Debug, Clone, Default)]
pub struct CommitReport {
    /// Number of sites that received an `Accessed` (decay-then-reinforce) update.
    pub sites_accessed: usize,
    /// Number of sites that received a `FeedbackReceived` Rescorla-Wagner update.
    pub feedback_applied: usize,
    /// Number of edges that received a `PathUsed` Hebbian-Oja conductance update.
    pub paths_strengthened: usize,
    /// Number of `CoReadout` site pairs whose connecting edges were strengthened.
    pub pairs_strengthened: usize,
    /// Number of `TensionActivated` contradictions recorded.
    pub tensions_recorded: usize,
}

/// Summary of a completed agent session for reflect_batch().
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub peer_id: crate::graph::types::PeerId,
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

/// Event emitted by graph mutation methods.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphEvent {
    NodeCreated {
        node_id: NodeId,
        node_type: KnowledgeType,
    },
    SalienceChanged {
        node_id: NodeId,
        old: f64,
        new: f64,
    },
    TierTransition {
        node_id: NodeId,
        from_tier: MemoryTier,
        to_tier: MemoryTier,
    },
    EdgeCreated {
        edge_id: EdgeId,
        source: NodeId,
        target: NodeId,
        edge_type: EdgeType,
    },
    NodeArchived {
        node_id: NodeId,
    },
    NodeRevived {
        node_id: NodeId,
        new_salience: f64,
    },
    /// A peer's evidence trust reservoir moved through `update_peer_trust`
    /// (social.md "Peer Trust": "Trust updates must leave traces"). The coarse
    /// `trust_level` and origin are unchanged — only the evidence estimate moved.
    PeerTrustChanged {
        peer_id: crate::graph::types::PeerId,
        /// Trust reservoir (log trust-odds) before the evidence update.
        old: f64,
        /// Trust reservoir (log trust-odds) after the evidence update.
        new: f64,
    },
}

/// Report of supporting and contradicting evidence for a node.
///
/// Returned by `Engine::support_report()`. Traverses only direct edges (1-hop)
/// from the target node to assess evidence quality and independence.
#[derive(Debug, Clone, Default)]
pub struct SupportReport {
    /// Number of nodes connected via supporting edges (ConsolidatedFrom, ReinforcedBy, Supports).
    pub supporting_sources: usize,
    /// Number of nodes connected via contradicting edges (Contradicts, Refutes).
    pub contradicting_sources: usize,
    /// Number of distinct (peer_id, session_id) pairs among all source nodes.
    /// Same-peer repetitions in different sessions count as independent.
    pub independent_origins: usize,
    /// Sum of salience values of all supporting source nodes.
    pub total_support_salience: f64,
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

/// Reference to what is being observed in a perspective query.
///
/// Determines how candidate nodes (owned by the observer) are filtered
/// before spreading activation.
#[derive(Debug, Clone, PartialEq)]
pub enum ObservedRef {
    /// Observe knowledge related to another peer's contributions.
    /// Filters to observer nodes that are connected (via edges) to nodes produced by this peer.
    Agent(crate::graph::types::PeerId),
    /// Observe knowledge about an entity tag.
    /// Filters to observer nodes that carry this entity tag.
    EntityTag(String),
    /// Observe knowledge about a specific node.
    /// Filters to observer nodes that are connected (via edges) to this node.
    Node(NodeId),
}

/// Perspective query key — retrieves what one agent knows about a subject.
///
/// Combines observer identity, observed target, and scope to produce a
/// perspective-filtered view of the graph. Non-retroactive: the observer
/// cannot recall events created before their first contribution.
#[derive(Debug, Clone)]
pub struct PerspectiveKey {
    /// The peer whose perspective we are querying from.
    pub observer_peer_id: crate::graph::types::PeerId,
    /// What the observer is looking at.
    pub observed: ObservedRef,
    /// Scope filter — only nodes matching this scope (or universal) are included.
    pub scope: ScopePath,
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

/// Synthesis evidence prior for a crystallized site — the additive,
/// relevance-weighted average of the source composite strengths `A_i = B_i + P_i`.
///
/// Crystallization is **additive synthesis only** (overview.md, interactions.md
/// `Crystallized`): the new synthesis site inherits the weighted-mean need-odds of
/// the fragments it consolidates and **never mutates the sources**. The synthesized
/// strength is evidence-derived, so the result seeds the synthesis node's
/// decay-exempt evidence prior `P_i` (a creation trace seeds its base level `B_i`).
/// The work is expressed entirely in log-need-odds space (ADR-0008/ADR-0003), so
/// the synthesis is a posterior-odds combination, not a `[0, 1]` salience blend.
/// Confidence enters as a bounded additive log-odds offset
/// (`(2*confidence - 1) * REWARD_LOG_ODDS_SCALE`): a fully confident synthesis is
/// lifted by `+REWARD_LOG_ODDS_SCALE`, a zero-confidence one is pulled down by the
/// symmetric amount, and a neutral `0.5` confidence leaves the source average
/// unchanged. The result is clamped to the finite log-odds range.
fn crystallized_action(source_actions: &[f64], relevance_weights: &[f64], confidence: f64) -> f64 {
    let weight_sum: f64 = relevance_weights.iter().sum();
    let source_average = if weight_sum > f64::EPSILON {
        source_actions
            .iter()
            .zip(relevance_weights.iter())
            .map(|(action, relevance)| action * relevance)
            .sum::<f64>()
            / weight_sum
    } else if source_actions.is_empty() {
        0.0
    } else {
        source_actions.iter().sum::<f64>() / source_actions.len() as f64
    };

    let confidence_offset =
        (2.0 * confidence - 1.0) * crate::mechanics::priors::REWARD_LOG_ODDS_SCALE;
    let clamp = crate::mechanics::priors::LOG_ODDS_CLAMP;
    (source_average + confidence_offset).clamp(-clamp, clamp)
}

/// Seed the strength state for a freshly created node under `A_i = B_i + P_i`.
///
/// Returns `(salience, retained_action, access_history)` for a node whose evidence
/// prior is `prior` and whose creation trace is stamped at `now` (ADR-0008). The
/// lone creation trace floors to 1ms at `now`, so `B_i ≈ ln(1) = 0` and the cached
/// `salience = logistic(B_i + prior)`; the trace keeps `B_i` finite for a node that
/// is never accessed (`compute_base_level` returns `NEG_INFINITY` on empty history).
fn seed_node_strength(
    node_type: &KnowledgeType,
    prior: f64,
    now: Timestamp,
) -> (f64, f64, VecDeque<AccessTrace>) {
    // The creation trace is the first trace, so its activation-dependent decay is the
    // floor `d_j = m_type·α` (Pavlik & Anderson 2005; equals `compute_trace_decay`
    // on an empty history).
    let m_type = crate::mechanics::priors::decay_multiplier_for_type(node_type);
    let creation_decay = m_type * crate::mechanics::priors::DECAY_INTERCEPT;
    let mut access_history = VecDeque::new();
    access_history.push_back(AccessTrace {
        at: now,
        decay: creation_decay,
    });
    let base_level = crate::mechanics::forgetting::compute_base_level(&access_history, now);
    let retained_action = base_level + prior;
    let salience = crate::mechanics::priors::project_salience(retained_action);
    (salience, retained_action, access_history)
}

/// Entity-overlap NPMI feature `entity_npmi` — Jaccard of the two tag sets.
///
/// Returns `0.0` when either node has no tags. The overlap is a coupling cue: the
/// more entities two sites share, the more likely they are co-needed.
fn entity_overlap_npmi(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set_a: HashSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: HashSet<&str> = b.iter().map(String::as_str).collect();
    let intersection = set_a.intersection(&set_b).count();
    if intersection == 0 {
        return 0.0;
    }
    let union = set_a.union(&set_b).count();
    intersection as f64 / union as f64
}

fn salience_tier(salience: f64) -> MemoryTier {
    if salience < ARCHIVE_SALIENCE_THRESHOLD {
        MemoryTier::Archival
    } else if salience > 0.80 {
        MemoryTier::Core
    } else {
        MemoryTier::Recall
    }
}

/// Grade assigned to the graph's overall health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthGrade {
    /// Excellent — orphan < 5%, contradiction < 3%, supersede < 10%.
    A,
    /// Good — orphan < 15%, contradiction < 8%, supersede < 20%.
    B,
    /// Fair — orphan < 30%, contradiction < 15%, supersede < 35%.
    C,
    /// Poor — any metric exceeds C thresholds.
    D,
}

/// Diagnostic report for the cognitive graph.
///
/// Returned by `Engine::health()`. Contains structural metrics and an overall grade.
#[derive(Debug, Clone)]
pub struct HealthReport {
    /// Total number of live nodes.
    pub total_nodes: usize,
    /// Number of nodes with no edges (orphans).
    pub orphan_count: usize,
    /// Number of Contradicts edges.
    pub contradiction_count: usize,
    /// Number of Supersedes edges.
    pub supersede_count: usize,
    /// Fraction of nodes that are orphans [0, 1].
    pub orphan_rate: f64,
    /// Fraction of edges that are Contradicts [0, 1].
    pub contradiction_rate: f64,
    /// Fraction of edges that are Supersedes [0, 1].
    pub supersede_rate: f64,
    /// Number of retracted nodes.
    pub retracted_count: usize,
    /// Number of nodes without an embedding vector.
    pub missing_embedding_count: usize,
    /// Number of registered peers.
    pub peer_count: usize,
    /// Average salience across all nodes.
    pub avg_salience: f64,
    /// Overall health grade.
    pub grade: HealthGrade,
}

/// Input for `Engine::learn()` — project knowledge injection.
#[derive(Debug, Clone)]
pub struct LearnInput {
    /// L0: One-liner label.
    pub name: String,
    /// L1: Optional summary.
    pub summary: Option<String>,
    /// L2: Full content.
    pub content: String,
    /// Optional embedding vector.
    pub embedding: Option<Vec<f64>>,
    /// Confidence [0, 1]. Default: 0.9.
    pub confidence: Option<f64>,
    /// Knowledge type. Default: `Semantic`.
    pub node_type: Option<KnowledgeType>,
    /// Entity tags.
    pub entity_tags: Vec<String>,
    /// Provenance.
    pub origin: Origin,
    /// Timestamp. Default: now.
    pub timestamp: Option<Timestamp>,
}

/// Input for `Engine::remember_peer()` — peer profile recording.
#[derive(Debug, Clone)]
pub struct PeerProfileInput {
    /// Primary display name of the peer (used for auto-registration).
    pub peer_name: String,
    /// L0: One-liner label for this profile entry.
    pub name: String,
    /// L1: Optional summary.
    pub summary: Option<String>,
    /// L2: Full content.
    pub content: String,
    /// Optional embedding vector.
    pub embedding: Option<Vec<f64>>,
    /// Confidence [0, 1]. Default: 0.9.
    pub confidence: Option<f64>,
    /// Entity tags (peer name is added automatically).
    pub entity_tags: Vec<String>,
    /// Source kind. Default: `HumanInput`.
    pub source_kind: Option<crate::peer::SourceKind>,
    /// Session ID. Default: "profile".
    pub session_id: Option<String>,
    /// Timestamp. Default: now.
    pub timestamp: Option<Timestamp>,
}

/// Input for `Engine::log_activity()` — peer activity recording.
#[derive(Debug, Clone)]
pub struct ActivityInput {
    /// Primary display name of the peer.
    pub peer_name: String,
    /// L0: One-liner label.
    pub name: String,
    /// L1: Optional summary.
    pub summary: Option<String>,
    /// L2: Full content.
    pub content: String,
    /// Optional embedding vector.
    pub embedding: Option<Vec<f64>>,
    /// Confidence [0, 1]. Default: 0.8.
    pub confidence: Option<f64>,
    /// Knowledge type. Default: `Episodic`.
    pub node_type: Option<KnowledgeType>,
    /// Entity tags.
    pub entity_tags: Vec<String>,
    /// Source kind. Default: `HumanInput`.
    pub source_kind: Option<crate::peer::SourceKind>,
    /// Session ID. Default: "activity".
    pub session_id: Option<String>,
    /// Timestamp. Default: now.
    pub timestamp: Option<Timestamp>,
    /// When the activity started (bitemporal).
    pub valid_from: Option<Timestamp>,
    /// When the activity ended (bitemporal).
    pub valid_until: Option<Timestamp>,
}

/// Input for `Engine::schedule()` — event scheduling.
#[derive(Debug, Clone)]
pub struct ScheduleInput {
    /// Primary display name of the peer organizing the event.
    pub peer_name: String,
    /// L0: One-liner label.
    pub name: String,
    /// L1: Optional summary.
    pub summary: Option<String>,
    /// L2: Full content.
    pub content: String,
    /// Optional embedding vector.
    pub embedding: Option<Vec<f64>>,
    /// Confidence [0, 1]. Default: 0.9.
    pub confidence: Option<f64>,
    /// Participants — converted to entity tags automatically.
    pub participants: Vec<String>,
    /// Additional entity tags.
    pub entity_tags: Vec<String>,
    /// Session ID. Default: "schedule".
    pub session_id: Option<String>,
    /// Timestamp. Default: now.
    pub timestamp: Option<Timestamp>,
    /// When the event starts (required).
    pub valid_from: Timestamp,
    /// When the event ends (optional).
    pub valid_until: Option<Timestamp>,
}

/// Input for `Engine::ingest_document()` — document chunk ingestion.
#[derive(Debug, Clone)]
pub struct DocumentInput {
    /// Document name (used as prefix for chunk labels).
    pub name: String,
    /// Text chunks to ingest as sequential `Episodic` nodes.
    pub chunks: Vec<String>,
    /// Confidence [0, 1]. Default: 0.8.
    pub confidence: Option<f64>,
    /// Entity tags applied to all chunks.
    pub entity_tags: Vec<String>,
    /// Provenance applied to all chunks.
    pub origin: Origin,
    /// Timestamp applied to all chunks. Default: now.
    pub timestamp: Option<Timestamp>,
}

/// A single extracted fact for `Engine::ingest_conversation()`.
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    /// L0: One-liner label.
    pub name: String,
    /// L1: Optional summary.
    pub summary: Option<String>,
    /// L2: Full content.
    pub content: String,
    /// Optional embedding vector.
    pub embedding: Option<Vec<f64>>,
    /// Confidence [0, 1]. Default: inherits from ConversationInput.
    pub confidence: Option<f64>,
    /// Entity tags.
    pub entity_tags: Vec<String>,
}

/// Input for `Engine::ingest_conversation()` — conversation ingestion.
#[derive(Debug, Clone)]
pub struct ConversationInput {
    /// L0: One-liner label for the raw episode.
    pub name: String,
    /// L1: Optional summary.
    pub summary: Option<String>,
    /// L2: Raw conversation text.
    pub raw_text: String,
    /// Extracted facts to link to the episode.
    pub extracted_facts: Vec<ExtractedFact>,
    /// Confidence [0, 1]. Default: 0.8.
    pub confidence: Option<f64>,
    /// Entity tags for the episode.
    pub entity_tags: Vec<String>,
    /// Provenance.
    pub origin: Origin,
    /// Timestamp. Default: now.
    pub timestamp: Option<Timestamp>,
    /// If set, extracted facts are also stored in this peer's profile scope.
    pub about_peer: Option<String>,
}

/// Result of `Engine::ingest_conversation()`.
#[derive(Debug, Clone)]
pub struct ConversationResult {
    /// Node ID of the raw episode.
    pub episode_id: NodeId,
    /// Node IDs of the extracted facts.
    pub extracted_ids: Vec<NodeId>,
}

/// The Anamnesis cognitive graph engine.
///
/// `Engine<S>` is generic over the storage backend. The default is
/// `SqliteStorage` (in-memory SQLite with write-behind hot fields).
pub struct Engine<S: StorageAdapter + Clone = SqliteStorage> {
    graph: Graph<S>,
    config: EngineConfig,
    snapshots: SnapshotStore<S>,
    events: Vec<GraphEvent>,
    /// Peer registry — maps PeerId to PeerProfile (name, trust, aliases, platforms).
    peers: crate::peer::PeerRegistry,
}

impl Engine<SqliteStorage> {
    /// Create a new engine with default configuration and in-memory SQLite storage.
    pub fn new() -> Self {
        let graph = Graph::new();
        let peers = graph.storage().load_peers().unwrap_or_default();
        Engine {
            graph,
            config: EngineConfig::default(),
            snapshots: SnapshotStore::new(),
            events: Vec::new(),
            peers,
        }
    }

    /// Create a new engine with custom configuration.
    pub fn with_config(config: EngineConfig) -> Self {
        let graph = Graph::new();
        let peers = graph.storage().load_peers().unwrap_or_default();
        Engine {
            graph,
            config,
            snapshots: SnapshotStore::new(),
            events: Vec::new(),
            peers,
        }
    }
}

impl Default for Engine<SqliteStorage> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: StorageAdapter + Clone> Engine<S> {
    /// Create an engine with a custom storage backend.
    pub fn with_storage(config: EngineConfig, storage: S) -> Self {
        let graph = Graph::with_storage(storage);
        let peers = graph.storage().load_peers().unwrap_or_default();
        Engine {
            graph,
            config,
            snapshots: SnapshotStore::new(),
            events: Vec::new(),
            peers,
        }
    }

    fn register_peer_internal(
        &mut self,
        name: &str,
        trust_level: crate::peer::TrustLevel,
    ) -> Result<crate::graph::types::PeerId, Error> {
        let id = self.peers.register_peer(name, trust_level)?;
        let profile = self.peers.get_peer(id).cloned();
        if let Some(p) = profile {
            self.graph.storage_mut().store_peer(&p)?;
        }
        Ok(id)
    }

    // ── Peer registry API ─────────────────────────────────────────────────────

    /// Register a new peer and return its assigned `PeerId`.
    ///
    /// Returns `Err(Error::DuplicateAlias)` if the name is already taken.
    pub fn register_peer(
        &mut self,
        name: impl Into<String>,
        trust_level: crate::peer::TrustLevel,
    ) -> Result<crate::graph::types::PeerId, Error> {
        self.register_peer_internal(&name.into(), trust_level)
    }

    /// Resolve any identifier (name, alias, platform username) to a `PeerId`.
    pub fn resolve_peer(&self, identifier: &str) -> Option<crate::graph::types::PeerId> {
        self.peers.resolve_peer(identifier)
    }

    /// Get a peer profile by `PeerId`.
    pub fn get_peer(&self, id: crate::graph::types::PeerId) -> Option<&crate::peer::PeerProfile> {
        self.peers.get_peer(id)
    }

    /// Set the coarse trust level of an existing peer — an explicit policy decision.
    ///
    /// The coarse `TrustLevel` is the *prior* for the evidence trust reservoir
    /// (social.md "Peer Trust"). Re-labelling re-seeds the reservoir to the new
    /// level's prior ONLY when no evidence has yet moved it; once corroboration or
    /// feedback has accumulated, the learned estimate is preserved (never silently
    /// erased). For evidence-driven movement use
    /// [`update_peer_trust_evidence`](Engine::update_peer_trust_evidence).
    pub fn update_peer_trust(
        &mut self,
        id: crate::graph::types::PeerId,
        trust_level: crate::peer::TrustLevel,
    ) -> Result<(), Error> {
        self.peers.update_trust(id, trust_level)?;
        if let Some(profile) = self.peers.get_peer(id) {
            self.graph.storage_mut().store_peer(profile)?;
        }
        Ok(())
    }

    /// Move a peer's evidence trust reservoir toward a signed evidence target —
    /// the single traceable trust-evidence update path (social.md "Peer Trust").
    ///
    /// `signed_strength ∈ [-1, 1]`: positive is corroboration / useful feedback
    /// (raises trust), negative is contradiction / not-useful feedback (lowers it).
    /// The reservoir moves a fraction
    /// [`TRUST_LEARNING_RATE`](crate::mechanics::priors::TRUST_LEARNING_RATE) — the
    /// slow peer-reliability rate — toward
    /// [`trust_evidence_target`](crate::mechanics::priors::trust_evidence_target),
    /// the Rescorla-Wagner form shared with site feedback. The move is **traceable**
    /// (a [`GraphEvent::PeerTrustChanged`] is emitted and the moved reservoir is
    /// persisted) and **non-destructive**: the coarse `trust_level`, name, aliases,
    /// and every site origin are untouched — only the evidence estimate moves
    /// (social.md: "Peer trust never erases origin"). Returns the new reservoir.
    pub fn update_peer_trust_evidence(
        &mut self,
        id: crate::graph::types::PeerId,
        signed_strength: f64,
    ) -> Result<f64, Error> {
        use crate::mechanics::priors::{TRUST_LEARNING_RATE, trust_evidence_target};

        let target = trust_evidence_target(signed_strength);
        let (old, new) = self.peers.nudge_trust(id, target, TRUST_LEARNING_RATE)?;
        if let Some(profile) = self.peers.get_peer(id) {
            self.graph.storage_mut().store_peer(profile)?;
        }
        if (new - old).abs() > f64::EPSILON {
            self.emit_event(GraphEvent::PeerTrustChanged {
                peer_id: id,
                old,
                new,
            });
        }
        Ok(new)
    }

    /// Add an alias to an existing peer.
    pub fn add_peer_alias(
        &mut self,
        id: crate::graph::types::PeerId,
        alias: impl Into<String>,
    ) -> Result<(), Error> {
        let alias = alias.into();
        self.peers.add_alias(id, &alias)?;
        self.graph
            .storage_mut()
            .store_peer_alias(id, &alias, "alias")?;
        Ok(())
    }

    /// Add a platform username mapping to an existing peer.
    pub fn add_peer_platform(
        &mut self,
        id: crate::graph::types::PeerId,
        platform: impl Into<String>,
        username: impl Into<String>,
    ) -> Result<(), Error> {
        let platform = platform.into();
        let username = username.into();
        self.peers.add_platform(id, &platform, &username)?;
        let alias_type = format!("platform:{platform}");
        self.graph
            .storage_mut()
            .store_peer_alias(id, &username, &alias_type)?;
        Ok(())
    }

    /// List all registered peers.
    pub fn list_peers(&self) -> Vec<&crate::peer::PeerProfile> {
        self.peers.list_peers()
    }

    /// Returns the number of registered peers.
    pub fn peer_count(&self) -> usize {
        self.peers.peer_count()
    }

    // ── Convenience API ───────────────────────────────────────────────────────

    /// Ingest project knowledge — a semantic fact, convention, or decision.
    ///
    /// Convenience wrapper around `ingest()` for structured knowledge injection.
    /// The consumer specifies the scope; the engine sets `node_type` to `Semantic`
    /// by default (override via `node_type` field).
    pub fn learn(&mut self, input: LearnInput) -> Result<IngestResult, Error> {
        self.ingest(Observation {
            name: input.name,
            summary: input.summary,
            content: input.content,
            embedding: input.embedding,
            confidence: input.confidence.unwrap_or(0.9),
            node_type: input.node_type.unwrap_or(KnowledgeType::Semantic),
            entity_tags: input.entity_tags,
            origin: input.origin,
            timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
            valid_from: None,
            valid_until: None,
        })
    }

    /// Record a peer profile — who this person is, what they know, their preferences.
    ///
    /// Stores the profile under `scope: peer/{peer_id}/profile` with
    /// `node_type: IdentityLearned`. If `peer_name` is not yet registered,
    /// the peer is auto-registered with `TrustLevel::Member`.
    pub fn remember_peer(
        &mut self,
        input: PeerProfileInput,
    ) -> Result<(crate::graph::types::PeerId, IngestResult), Error> {
        let peer_id = match self.peers.resolve_peer(&input.peer_name) {
            Some(id) => id,
            None => {
                self.register_peer_internal(&input.peer_name, crate::peer::TrustLevel::Member)?
            }
        };

        let scope_str = format!("peer/{}/profile", peer_id.0);
        let scope = ScopePath::new(&scope_str).unwrap_or_else(|_| ScopePath::universal());

        let mut entity_tags = input.entity_tags;
        if !entity_tags.contains(&input.peer_name) {
            entity_tags.push(input.peer_name.clone());
        }
        // Add all peer aliases as entity tags
        for alias in self.peers.all_identifiers(peer_id) {
            if !entity_tags.contains(&alias) {
                entity_tags.push(alias);
            }
        }

        let result = self.ingest(Observation {
            name: input.name,
            summary: input.summary,
            content: input.content,
            embedding: input.embedding,
            confidence: input.confidence.unwrap_or(0.9),
            node_type: KnowledgeType::IdentityLearned,
            entity_tags,
            origin: Origin {
                peer_id,
                source_kind: input
                    .source_kind
                    .unwrap_or(crate::peer::SourceKind::HumanInput),
                session_id: input.session_id.unwrap_or_else(|| "profile".to_string()),
                scope,
                confidence: input.confidence.unwrap_or(0.9),
            },
            timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
            valid_from: None,
            valid_until: None,
        })?;

        Ok((peer_id, result))
    }

    /// Record a peer activity — what they did, said, or experienced.
    ///
    /// Stores the activity under `scope: peer/{peer_id}/activity` with
    /// `node_type: Episodic`. Supports `valid_from`/`valid_until` for
    /// time-bounded activities.
    pub fn log_activity(
        &mut self,
        input: ActivityInput,
    ) -> Result<(crate::graph::types::PeerId, IngestResult), Error> {
        // Auto-register peer if not found
        let peer_id = match self.peers.resolve_peer(&input.peer_name) {
            Some(id) => id,
            None => {
                self.register_peer_internal(&input.peer_name, crate::peer::TrustLevel::Member)?
            }
        };

        let scope_str = format!("peer/{}/activity", peer_id.0);
        let scope = ScopePath::new(&scope_str).unwrap_or_else(|_| ScopePath::universal());

        let result = self.ingest(Observation {
            name: input.name,
            summary: input.summary,
            content: input.content,
            embedding: input.embedding,
            confidence: input.confidence.unwrap_or(0.8),
            node_type: input.node_type.unwrap_or(KnowledgeType::Episodic),
            entity_tags: input.entity_tags,
            origin: Origin {
                peer_id,
                source_kind: input
                    .source_kind
                    .unwrap_or(crate::peer::SourceKind::HumanInput),
                session_id: input.session_id.unwrap_or_else(|| "activity".to_string()),
                scope,
                confidence: input.confidence.unwrap_or(0.8),
            },
            timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
            valid_from: input.valid_from,
            valid_until: input.valid_until,
        })?;

        Ok((peer_id, result))
    }

    /// Schedule an event — a time-bounded activity with participants.
    ///
    /// Stores the event under `scope: peer/{peer_id}/activity` with
    /// `node_type: Event`. Participants are converted to entity tags.
    /// `valid_from` is required.
    pub fn schedule(
        &mut self,
        input: ScheduleInput,
    ) -> Result<(crate::graph::types::PeerId, IngestResult), Error> {
        // Auto-register peer if not found
        let peer_id = match self.peers.resolve_peer(&input.peer_name) {
            Some(id) => id,
            None => {
                self.register_peer_internal(&input.peer_name, crate::peer::TrustLevel::Member)?
            }
        };

        let scope_str = format!("peer/{}/activity", peer_id.0);
        let scope = ScopePath::new(&scope_str).unwrap_or_else(|_| ScopePath::universal());

        // Convert participants to entity tags
        let mut entity_tags = input.entity_tags;
        for participant in &input.participants {
            if !entity_tags.contains(participant) {
                entity_tags.push(participant.clone());
            }
        }

        let result = self.ingest(Observation {
            name: input.name,
            summary: input.summary,
            content: input.content,
            embedding: input.embedding,
            confidence: input.confidence.unwrap_or(0.9),
            node_type: KnowledgeType::Event,
            entity_tags,
            origin: Origin {
                peer_id,
                source_kind: crate::peer::SourceKind::HumanInput,
                session_id: input.session_id.unwrap_or_else(|| "schedule".to_string()),
                scope,
                confidence: input.confidence.unwrap_or(0.9),
            },
            timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
            valid_from: Some(input.valid_from),
            valid_until: input.valid_until,
        })?;

        Ok((peer_id, result))
    }

    /// Ingest a document as a sequence of chunks.
    ///
    /// Each chunk becomes an `Episodic` node. Consecutive chunks are linked
    /// with `Temporal` edges to preserve document order.
    pub fn ingest_document(&mut self, input: DocumentInput) -> Result<Vec<NodeId>, Error> {
        let mut node_ids = Vec::new();
        let chunk_count = input.chunks.len();

        for (i, chunk) in input.chunks.into_iter().enumerate() {
            let result = self.ingest(Observation {
                name: format!("{} [chunk {}/{}]", input.name, i + 1, chunk_count),
                summary: None,
                content: chunk,
                embedding: None,
                confidence: input.confidence.unwrap_or(0.8),
                node_type: KnowledgeType::Episodic,
                entity_tags: input.entity_tags.clone(),
                origin: input.origin.clone(),
                timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
                valid_from: None,
                valid_until: None,
            })?;

            let node_id = match result {
                IngestResult::Created(ids) => ids[0],
                IngestResult::Reinforced { existing_id, .. } => existing_id,
            };
            node_ids.push(node_id);
        }

        // Link consecutive chunks with Temporal edges. Conductance is the cold-start
        // coupling log-LR prior (calibrated over {sim, entity, scope, type}); the
        // weight is its bounded projection, never authored (conductance.md / ADR-0002).
        for window in node_ids.windows(2) {
            let (from, to) = (window[0], window[1]);
            let from_node = self.graph.get_node(from)?.clone();
            let to_node = self.graph.get_node(to)?.clone();
            let conductance =
                self.cold_start_conductance(&from_node, &to_node, &EdgeType::Temporal);
            if !conductance.is_finite() {
                continue;
            }
            let eid = self.graph.next_edge_id();
            let now = Timestamp::now();
            let edge = Edge::seeded(
                eid,
                from,
                to,
                EdgeType::Temporal,
                conductance,
                crate::graph::edge::EdgeSource::Auto,
                now,
                now,
                HashMap::new(),
            );
            self.graph.add_edge(edge)?;
        }

        Ok(node_ids)
    }

    /// Ingest a conversation — raw episode + extracted knowledge.
    ///
    /// The raw text becomes an `Episodic` node. Each extracted fact becomes
    /// a `Semantic` node linked to the episode via `ExtractedFrom`. If
    /// `about_peer` is set, extracted facts are also stored in the peer's
    /// profile scope.
    pub fn ingest_conversation(
        &mut self,
        input: ConversationInput,
    ) -> Result<ConversationResult, Error> {
        // Ingest the raw episode
        let episode_result = self.ingest(Observation {
            name: input.name.clone(),
            summary: input.summary.clone(),
            content: input.raw_text.clone(),
            embedding: None,
            confidence: input.confidence.unwrap_or(0.8),
            node_type: KnowledgeType::Episodic,
            entity_tags: input.entity_tags.clone(),
            origin: input.origin.clone(),
            timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
            valid_from: None,
            valid_until: None,
        })?;

        let episode_id = match episode_result {
            IngestResult::Created(ids) => ids[0],
            IngestResult::Reinforced { existing_id, .. } => existing_id,
        };

        let mut extracted_ids = Vec::new();

        // Ingest extracted facts
        for fact in input.extracted_facts {
            let fact_result = self.ingest(Observation {
                name: fact.name.clone(),
                summary: fact.summary.clone(),
                content: fact.content.clone(),
                embedding: fact.embedding.clone(),
                confidence: fact.confidence.unwrap_or(input.confidence.unwrap_or(0.8)),
                node_type: KnowledgeType::Semantic,
                entity_tags: fact.entity_tags.clone(),
                origin: input.origin.clone(),
                timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
                valid_from: None,
                valid_until: None,
            })?;

            let fact_id = match fact_result {
                IngestResult::Created(ids) => ids[0],
                IngestResult::Reinforced { existing_id, .. } => existing_id,
            };

            // Link fact to episode via ExtractedFrom. Conductance is the cold-start
            // coupling log-LR prior; weight is its bounded projection (ADR-0002).
            let fact_node = self.graph.get_node(fact_id)?.clone();
            let episode_node = self.graph.get_node(episode_id)?.clone();
            let conductance =
                self.cold_start_conductance(&fact_node, &episode_node, &EdgeType::ExtractedFrom);
            if conductance.is_finite() {
                let eid = self.graph.next_edge_id();
                let now = Timestamp::now();
                let edge = Edge::seeded(
                    eid,
                    fact_id,
                    episode_id,
                    EdgeType::ExtractedFrom,
                    conductance,
                    crate::graph::edge::EdgeSource::Auto,
                    now,
                    now,
                    HashMap::new(),
                );
                self.graph.add_edge(edge)?;
            }

            // If about_peer is set, also store in peer profile
            if let Some(ref peer_name) = input.about_peer {
                let peer_id = match self.peers.resolve_peer(peer_name) {
                    Some(id) => id,
                    None => {
                        self.register_peer_internal(peer_name, crate::peer::TrustLevel::Member)?
                    }
                };
                let scope_str = format!("peer/{}/profile", peer_id.0);
                let scope = ScopePath::new(&scope_str).unwrap_or_else(|_| ScopePath::universal());

                let mut profile_tags = fact.entity_tags.clone();
                if !profile_tags.contains(peer_name) {
                    profile_tags.push(peer_name.clone());
                }

                self.ingest(Observation {
                    name: fact.name,
                    summary: fact.summary,
                    content: fact.content,
                    embedding: fact.embedding,
                    confidence: fact.confidence.unwrap_or(0.8),
                    node_type: KnowledgeType::IdentityLearned,
                    entity_tags: profile_tags,
                    origin: Origin {
                        peer_id,
                        source_kind: crate::peer::SourceKind::HumanInput,
                        session_id: input.origin.session_id.clone(),
                        scope,
                        confidence: fact.confidence.unwrap_or(0.8),
                    },
                    timestamp: input.timestamp.unwrap_or_else(Timestamp::now),
                    valid_from: None,
                    valid_until: None,
                })?;
            }

            extracted_ids.push(fact_id);
        }

        Ok(ConversationResult {
            episode_id,
            extracted_ids,
        })
    }

    /// Create an engine with a default peer pre-registered.
    ///
    /// Convenience constructor for quick-start scenarios.
    pub fn with_default_peer(
        name: impl Into<String>,
        trust_level: crate::peer::TrustLevel,
    ) -> Result<Self, Error>
    where
        S: Default,
    {
        let mut engine = Self::with_storage(EngineConfig::default(), S::default());
        engine.register_peer(name, trust_level)?;
        Ok(engine)
    }

    fn emit_event(&mut self, event: GraphEvent) {
        let max_events = self.config.max_events;
        if max_events == 0 {
            return;
        }
        if self.events.len() >= max_events {
            let drop_count = self.events.len() + 1 - max_events;
            self.events.drain(0..drop_count);
        }
        self.events.push(event);
    }

    /// Return whether the engine has buffered graph mutation events.
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Drain buffered graph mutation events in chronological order.
    pub fn drain_events(&mut self) -> Vec<GraphEvent> {
        std::mem::take(&mut self.events)
    }

    /// Store a clone of the current storage state under a label.
    pub fn snapshot(&mut self, label: &str) -> Result<SnapshotId, Error> {
        self.graph.storage_mut().flush()?;
        Ok(self
            .snapshots
            .take(label, self.graph.storage(), Timestamp::now()))
    }

    /// Restore the graph storage from a previously captured snapshot.
    pub fn restore(&mut self, id: &SnapshotId) -> Result<(), Error> {
        let storage = self.snapshots.restore(id)?;
        self.graph.replace_storage(storage);
        Ok(())
    }

    /// List stored snapshot metadata in insertion order.
    pub fn list_snapshots(&self) -> Vec<(SnapshotId, String, Timestamp)> {
        self.snapshots.list()
    }

    /// Start a debugging session for a problem statement.
    pub fn start_debug(
        &mut self,
        problem: &str,
        origin: Origin,
        timestamp: Timestamp,
    ) -> Result<NodeId, Error> {
        if problem.trim().is_empty() {
            return Err(Error::InvalidInput(
                "debug problem must not be empty".to_string(),
            ));
        }

        let id = self.graph.next_node_id();
        let mut metadata = HashMap::new();
        metadata.insert("debug_kind".to_string(), "session".to_string());
        metadata.insert("debug_started_at".to_string(), timestamp.0.to_string());
        metadata.insert("debug_problem".to_string(), problem.to_string());

        // Debug-lifecycle node: decay-exempt (m_type = 0). The flat prior enters as
        // the evidence prior P_i; a creation trace seeds B_i (ADR-0008).
        let (salience, retained_action, access_history) = seed_node_strength(
            &KnowledgeType::DebugSession,
            crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            timestamp,
        );
        let node = Node {
            id,
            node_type: KnowledgeType::DebugSession,
            name: problem.to_string(),
            summary: None,
            content: problem.to_string(),
            embedding: None,
            created_at: timestamp,
            updated_at: timestamp,
            accessed_at: timestamp,
            valid_from: None,
            valid_until: None,
            salience,
            retained_action,
            evidence_prior: crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            access_count: 0,
            access_history,
            tier: MemoryTier::Auto,
            origin,
            entity_tags: Vec::new(),
            metadata,
        };

        self.graph.add_node(node)?;
        self.emit_event(GraphEvent::NodeCreated {
            node_id: id,
            node_type: KnowledgeType::DebugSession,
        });

        Ok(id)
    }

    /// Log a hypothesis inside an existing debugging session.
    pub fn log_hypothesis(
        &mut self,
        session: NodeId,
        text: &str,
        origin: Origin,
        timestamp: Timestamp,
    ) -> Result<NodeId, Error> {
        if text.trim().is_empty() {
            return Err(Error::InvalidInput(
                "hypothesis text must not be empty".to_string(),
            ));
        }
        let session_node = self.graph.get_node(session)?;
        if session_node.node_type != KnowledgeType::DebugSession {
            return Err(Error::InvalidInput(
                "log_hypothesis requires a DebugSession node".to_string(),
            ));
        }

        let id = self.graph.next_node_id();
        let mut metadata = HashMap::new();
        metadata.insert("debug_kind".to_string(), "hypothesis".to_string());
        metadata.insert("debug_session_id".to_string(), session.0.to_string());
        metadata.insert("hypothesis_status".to_string(), "open".to_string());
        metadata.insert("hypothesis_logged_at".to_string(), timestamp.0.to_string());

        // Debug-lifecycle node: decay-exempt (m_type = 0). The flat prior enters as
        // the evidence prior P_i; a creation trace seeds B_i (ADR-0008).
        let (salience, retained_action, access_history) = seed_node_strength(
            &KnowledgeType::Hypothesis,
            crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            timestamp,
        );
        let node = Node {
            id,
            node_type: KnowledgeType::Hypothesis,
            name: text.to_string(),
            summary: None,
            content: text.to_string(),
            embedding: None,
            created_at: timestamp,
            updated_at: timestamp,
            accessed_at: timestamp,
            valid_from: None,
            valid_until: None,
            salience,
            retained_action,
            evidence_prior: crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            access_count: 0,
            access_history,
            tier: MemoryTier::Auto,
            origin,
            entity_tags: Vec::new(),
            metadata,
        };
        self.graph.add_node(node)?;
        self.emit_event(GraphEvent::NodeCreated {
            node_id: id,
            node_type: KnowledgeType::Hypothesis,
        });

        let mut edge_metadata = HashMap::new();
        edge_metadata.insert(
            "debug_relation".to_string(),
            "hypothesis_session".to_string(),
        );
        // Conductance is the cold-start coupling log-LR prior; weight is its bounded
        // projection, never authored (conductance.md / ADR-0002).
        let hypothesis_node = self.graph.get_node(id)?.clone();
        let session_node = self.graph.get_node(session)?.clone();
        let conductance =
            self.cold_start_conductance(&hypothesis_node, &session_node, &EdgeType::BelongsTo);
        if conductance.is_finite() {
            let edge_id = self.graph.next_edge_id();
            let edge = Edge::seeded(
                edge_id,
                id,
                session,
                EdgeType::BelongsTo,
                conductance,
                crate::graph::edge::EdgeSource::Auto,
                timestamp,
                timestamp,
                edge_metadata,
            );
            self.graph.add_edge(edge)?;
            self.emit_event(GraphEvent::EdgeCreated {
                edge_id,
                source: id,
                target: session,
                edge_type: EdgeType::BelongsTo,
            });
        }

        Ok(id)
    }

    /// Log evidence against an existing hypothesis.
    pub fn log_evidence(
        &mut self,
        hypothesis: NodeId,
        text: &str,
        result: EvidenceResult,
        origin: Origin,
        timestamp: Timestamp,
    ) -> Result<NodeId, Error> {
        if text.trim().is_empty() {
            return Err(Error::InvalidInput(
                "evidence text must not be empty".to_string(),
            ));
        }
        let hypothesis_node = self.graph.get_node(hypothesis)?;
        if hypothesis_node.node_type != KnowledgeType::Hypothesis {
            return Err(Error::InvalidInput(
                "log_evidence requires a Hypothesis node".to_string(),
            ));
        }

        let id = self.graph.next_node_id();
        let result_label = match result {
            EvidenceResult::Supports => "supports",
            EvidenceResult::Contradicts => "contradicts",
            EvidenceResult::Neutral => "neutral",
            EvidenceResult::Inconclusive => "inconclusive",
        };
        let mut metadata = HashMap::new();
        metadata.insert("debug_kind".to_string(), "evidence".to_string());
        metadata.insert("debug_hypothesis_id".to_string(), hypothesis.0.to_string());
        metadata.insert("evidence_result".to_string(), result_label.to_string());
        metadata.insert("evidence_logged_at".to_string(), timestamp.0.to_string());
        if matches!(
            result,
            EvidenceResult::Neutral | EvidenceResult::Inconclusive
        ) {
            metadata.insert(
                "automatic_hypothesis_action".to_string(),
                "none".to_string(),
            );
        }

        // Debug-lifecycle node: decay-exempt (m_type = 0). The flat prior enters as
        // the evidence prior P_i; a creation trace seeds B_i (ADR-0008).
        let (salience, retained_action, access_history) = seed_node_strength(
            &KnowledgeType::Evidence,
            crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            timestamp,
        );
        let node = Node {
            id,
            node_type: KnowledgeType::Evidence,
            name: text.to_string(),
            summary: None,
            content: text.to_string(),
            embedding: None,
            created_at: timestamp,
            updated_at: timestamp,
            accessed_at: timestamp,
            valid_from: None,
            valid_until: None,
            salience,
            retained_action,
            evidence_prior: crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            access_count: 0,
            access_history,
            tier: MemoryTier::Auto,
            origin,
            entity_tags: Vec::new(),
            metadata,
        };
        self.graph.add_node(node)?;
        self.emit_event(GraphEvent::NodeCreated {
            node_id: id,
            node_type: KnowledgeType::Evidence,
        });

        let edge_type = match result {
            EvidenceResult::Supports => Some(EdgeType::Supports),
            EvidenceResult::Contradicts => Some(EdgeType::Refutes),
            EvidenceResult::Neutral | EvidenceResult::Inconclusive => None,
        };

        if let Some(edge_type) = edge_type {
            let event_edge_type = edge_type.clone();
            let mut edge_metadata = HashMap::new();
            edge_metadata.insert(
                "debug_relation".to_string(),
                "evidence_hypothesis".to_string(),
            );
            edge_metadata.insert("evidence_result".to_string(), result_label.to_string());
            // Conductance is the cold-start coupling log-LR prior; weight is its
            // bounded projection, never authored (conductance.md / ADR-0002).
            let evidence_node = self.graph.get_node(id)?.clone();
            let hypothesis_node = self.graph.get_node(hypothesis)?.clone();
            let conductance =
                self.cold_start_conductance(&evidence_node, &hypothesis_node, &edge_type);
            if conductance.is_finite() {
                let edge_id = self.graph.next_edge_id();
                let edge = Edge::seeded(
                    edge_id,
                    id,
                    hypothesis,
                    edge_type,
                    conductance,
                    crate::graph::edge::EdgeSource::Auto,
                    timestamp,
                    timestamp,
                    edge_metadata,
                );
                self.graph.add_edge(edge)?;
                self.emit_event(GraphEvent::EdgeCreated {
                    edge_id,
                    source: id,
                    target: hypothesis,
                    edge_type: event_edge_type,
                });
            }
        }

        Ok(id)
    }

    /// Mark a hypothesis as rejected with a reason.
    pub fn reject_hypothesis(
        &mut self,
        hypothesis: NodeId,
        reason: &str,
        timestamp: Timestamp,
    ) -> Result<(), Error> {
        let node = self.graph.get_node_mut(hypothesis)?;
        if node.node_type != KnowledgeType::Hypothesis {
            return Err(Error::InvalidInput(
                "reject_hypothesis requires a Hypothesis node".to_string(),
            ));
        }

        node.metadata
            .insert("hypothesis_status".to_string(), "rejected".to_string());
        node.metadata
            .insert("rejection_reason".to_string(), reason.to_string());
        node.metadata
            .insert("rejected_at".to_string(), timestamp.0.to_string());
        node.updated_at = timestamp;

        Ok(())
    }

    /// Mark a hypothesis as confirmed with a conclusion.
    pub fn confirm_hypothesis(
        &mut self,
        hypothesis: NodeId,
        conclusion: &str,
        timestamp: Timestamp,
    ) -> Result<(), Error> {
        let node = self.graph.get_node_mut(hypothesis)?;
        if node.node_type != KnowledgeType::Hypothesis {
            return Err(Error::InvalidInput(
                "confirm_hypothesis requires a Hypothesis node".to_string(),
            ));
        }

        node.metadata
            .insert("hypothesis_status".to_string(), "confirmed".to_string());
        node.metadata.insert(
            "confirmation_conclusion".to_string(),
            conclusion.to_string(),
        );
        node.metadata
            .insert("confirmed_at".to_string(), timestamp.0.to_string());
        node.updated_at = timestamp;

        Ok(())
    }

    /// End a debugging session and record its final outcome.
    pub fn end_debug(
        &mut self,
        session: NodeId,
        outcome: DebugOutcome,
        timestamp: Timestamp,
    ) -> Result<(), Error> {
        let node = self.graph.get_node_mut(session)?;
        if node.node_type != KnowledgeType::DebugSession {
            return Err(Error::InvalidInput(
                "end_debug requires a DebugSession node".to_string(),
            ));
        }

        match outcome {
            DebugOutcome::Resolved(conclusion) => {
                node.metadata
                    .insert("debug_outcome".to_string(), "resolved".to_string());
                node.metadata
                    .insert("debug_resolution".to_string(), conclusion);
            }
            DebugOutcome::Unresolved(reason) => {
                node.metadata
                    .insert("debug_outcome".to_string(), "unresolved".to_string());
                node.metadata
                    .insert("debug_unresolved_reason".to_string(), reason);
            }
            DebugOutcome::Abandoned => {
                node.metadata
                    .insert("debug_outcome".to_string(), "abandoned".to_string());
            }
        }
        node.metadata
            .insert("debug_ended_at".to_string(), timestamp.0.to_string());
        node.updated_at = timestamp;

        Ok(())
    }

    /// Ingest a new observation into the graph.
    ///
    /// Creates a Node, then applies attraction mechanics: finds candidate nodes
    /// (last 256 + entity-tag matches), scores them, and creates/strengthens
    /// up to 4 edges to the most similar candidates.
    pub fn ingest(&mut self, observation: Observation) -> Result<IngestResult, Error> {
        use crate::mechanics::attraction::{
            attraction_score, cosine_similarity, should_create_edge, tau_type,
        };
        use crate::mechanics::perception::{self, PerceptionDecision};

        // Nearest existing site: max similarity and the candidate embedding for the
        // Bayesian-surprise (Mahalanobis/isotropic) prediction error.
        let (max_similarity, most_similar_id, predicted_embedding) =
            if let Some(ref new_embedding) = observation.embedding {
                let candidates =
                    ingest_trigger_candidates(self.graph.storage(), &observation.entity_tags, None);

                let (sim, id) = candidates
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
                    });
                let pred = (sim > 0.0)
                    .then(|| {
                        self.graph
                            .storage()
                            .get_node(id)
                            .ok()
                            .and_then(|n| n.embedding.clone())
                    })
                    .flatten();
                (sim, id, pred)
            } else {
                (0.0, NodeId(0), None)
            };

        // ── Stage 1 of perception: reject untrusted / unaffordable-and-not-novel ──
        // ── plus Stage 2 routing. theta_sep is the encoder-derived separation     ──
        // ── boundary `1 - q95` (perception.md, ADR-0009); the engine defaults     ──
        // ── `novelty_threshold` to that derivation (priors::theta_sep), so the    ──
        // ── operative value tracks the encoder rather than a hard-coded literal.  ──
        let theta_sep = self.config.novelty_threshold;

        // Surprise-gated initial evidence prior P_i = k * eps for an Allocate
        // decision (ADR-0009). eps is the precision-weighted (isotropic-fallback)
        // embedding prediction error against the nearest site; absent any prediction
        // (no embeddings yet) the site is maximally surprising and its prior enters
        // at the surprise ceiling (`SURPRISE_GAIN_K == INITIAL_RETAINED_ACTION`).
        let surprise_charge = match (&observation.embedding, &predicted_embedding) {
            (Some(obs), Some(pred)) => {
                let eps = perception::bayesian_surprise(obs, pred, None);
                perception::surprise_charge(eps)
            }
            _ => crate::mechanics::priors::INITIAL_RETAINED_ACTION,
        };

        let decision = perception::gate(
            observation.confidence,
            self.config.confidence_threshold,
            self.graph.node_count(),
            self.config.max_nodes,
            max_similarity,
            theta_sep,
            surprise_charge,
        );

        // Route-via-reinforce (familiar) vs allocate-with-surprise-charge (novel).
        // The resolved value is the initial evidence prior P_i for the new site.
        let initial_evidence_prior = match decision {
            PerceptionDecision::Reject(reason) => {
                return Err(Error::Rejected(format!(
                    "{}: confidence {:.2}, novelty vs theta_sep {:.2}",
                    reason.as_str(),
                    observation.confidence,
                    theta_sep
                )));
            }
            PerceptionDecision::Route { .. } => {
                // Route is the SSOT invariant "reinforce existing site; no new site"
                // (perception.md decision table): the gate has already judged the
                // input familiar (novelty <= theta_sep), so it reinforces the nearest
                // matched site and never allocates a duplicate. This is independent of
                // `dedup_threshold` — that knob does not gate whether routing
                // reinforces; theta_sep alone draws the route/allocate boundary
                // (ADR-0009). `dedup_enabled` remains the explicit master switch that
                // turns reinforce-on-similarity off entirely. A matched site must
                // actually exist (`max_similarity > 0`); with no embedding candidate
                // there is nothing to route to, so the observation allocates.
                if self.config.dedup_enabled && max_similarity > 0.0 {
                    self.touch(most_similar_id, observation.timestamp)?;
                    return Ok(IngestResult::Reinforced {
                        existing_id: most_similar_id,
                        similarity: max_similarity,
                    });
                }
                // With dedup disabled, reinforce-on-route is opted out: the observation
                // falls through to allocation with its surprise-gated charge (low, since
                // novelty is low) rather than the flat ceiling.
                surprise_charge
            }
            PerceptionDecision::Allocate {
                surprise_charge, ..
            } => surprise_charge,
        };

        let id = self.graph.next_node_id();
        let now = observation.timestamp;

        if let (Some(vf), Some(vu)) = (observation.valid_from, observation.valid_until) {
            if vu <= vf {
                return Err(Error::InvalidInput(
                    "valid_until must be greater than valid_from".to_string(),
                ));
            }
        }

        // Seed the creation trace so B_i is finite at birth (compute_base_level
        // returns NEG_INFINITY on empty history). At `now` the lone trace floors to
        // 1ms, so B_i ≈ ln(1) = 0 and salience ≈ logistic(P_i) (ADR-0008/0009). Its
        // per-trace decay is the creation floor d_j = m_type·α (Pavlik & Anderson 2005).
        let creation_decay =
            crate::mechanics::priors::decay_multiplier_for_type(&observation.node_type)
                * crate::mechanics::priors::DECAY_INTERCEPT;
        let mut access_history = VecDeque::new();
        access_history.push_back(AccessTrace {
            at: now,
            decay: creation_decay,
        });
        let base_level = crate::mechanics::forgetting::compute_base_level(&access_history, now);
        let initial_action = base_level + initial_evidence_prior;

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
            valid_from: observation.valid_from,
            valid_until: observation.valid_until,
            // salience = logistic(B_i + P_i), the cached composite projection.
            salience: crate::mechanics::priors::project_salience(initial_action),
            retained_action: initial_action,
            evidence_prior: initial_evidence_prior,
            access_count: 0,
            access_history,
            tier: crate::graph::MemoryTier::Auto,
            origin: observation.origin,
            entity_tags: observation.entity_tags.clone(),
            metadata: HashMap::new(),
        };

        self.graph.add_node(node)?;
        self.emit_event(GraphEvent::NodeCreated {
            node_id: id,
            node_type: observation.node_type.clone(),
        });

        if let Some(ref new_embedding) = observation.embedding {
            use crate::mechanics::interactions::hebbian_oja;
            use crate::mechanics::priors::{
                TARGET_COACTIVATION_N, coupling_clears_threshold, initialize_conductance,
                learning_rate,
            };
            let eta = learning_rate(TARGET_COACTIVATION_N);

            let new_type = &observation.node_type;
            let new_tags = &observation.entity_tags;

            // Trigger pool: last 256 nodes by ID + entity-tag matches (indexed, O(1) dedup).
            let candidate_ids: Vec<NodeId> =
                ingest_trigger_candidates(self.graph.storage(), new_tags, Some(id))
                    .into_iter()
                    .collect();

            // Score candidates by attraction (similarity * type affinity)
            let mut scored: Vec<(NodeId, f64)> = Vec::new();
            let mut attraction_scores: HashMap<NodeId, f64> = HashMap::new();
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
                // Attraction is similarity * type-affinity only. There is no mass or
                // gravity boost: importance is emergent (overview.md / conductance.md
                // "Importance is emergent ... without a separate gravity or mass force").
                let attraction = attraction_score(sim, tau);

                if should_create_edge(attraction, new_type, &candidate.node_type) {
                    scored.push((*cid, attraction));
                    attraction_scores.insert(*cid, attraction);
                }
            }

            // Top 4 by score using BinaryHeap
            let top_scored = top_n_by_score(&scored, 4);

            // The new site (already inserted) — needed to seed cold-start coupling.
            let new_node = self.graph.get_node(id)?.clone();
            for (cid, score) in top_scored {
                let edge_score = attraction_scores.get(&cid).copied().unwrap_or(score);
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
                    // Reinforce the authoritative conductance reservoir via the bounded
                    // Hebbian-Oja update (conductance.md / interactions.md), never the
                    // bounded `weight` projection. `set_conductance` re-projects weight.
                    let eid = existing.id;
                    let current = self.graph.storage().get_conductance(eid)?;
                    let next = hebbian_oja(current, clamp_unit_finite(edge_score), eta);
                    if next.is_finite() {
                        self.graph.storage_mut().set_conductance(eid, next)?;
                    }
                } else {
                    let Ok(candidate_node) = self.graph.get_node(cid).cloned() else {
                        continue;
                    };
                    // Cold-start density gate (conductance.md "Cold Start":
                    // `if coupling_seed >= conductance_threshold: create edge`).
                    // Sub-threshold coupling is a noisy weak path and must NOT
                    // create an edge — the declared `conductance_threshold` density
                    // knob (ADR-0010 / dissipation.md "minimum coupling").
                    let seed = self.cold_start_coupling_seed(
                        &new_node,
                        &candidate_node,
                        &EdgeType::Semantic,
                    );
                    if !coupling_clears_threshold(seed) {
                        continue;
                    }
                    // Seed conductance from the cold-start coupling; weight is the
                    // bounded projection of the seeded reservoir, never authored
                    // (conductance.md "Cold Start", ADR-0002).
                    let conductance = initialize_conductance(seed);
                    if !conductance.is_finite() {
                        continue;
                    }
                    let eid = self.graph.next_edge_id();
                    let edge = Edge::seeded(
                        eid,
                        id,
                        cid,
                        EdgeType::Semantic,
                        conductance,
                        crate::graph::edge::EdgeSource::Auto,
                        now,
                        now,
                        HashMap::new(),
                    );
                    self.graph.add_edge(edge)?;
                    self.emit_event(GraphEvent::EdgeCreated {
                        edge_id: eid,
                        source: id,
                        target: cid,
                        edge_type: EdgeType::Semantic,
                    });
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
            attraction_score, cosine_similarity, should_create_edge, tau_type,
        };
        use crate::mechanics::interactions::hebbian_oja;
        use crate::mechanics::priors::{TARGET_COACTIVATION_N, learning_rate};

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
        let source_ids = request.source_ids.clone();
        let source_set: HashSet<NodeId> = source_ids.iter().copied().collect();
        let mut source_actions = Vec::with_capacity(source_ids.len());

        for source_id in &source_ids {
            let _ = self.graph.get_node(*source_id)?;
            // Composite source strength A_i = B_i + P_i (cached) — the synthesis
            // node's evidence prior is the weighted average of these (ADR-0008).
            source_actions.push(self.graph.storage().get_retained_action(*source_id)?);
        }

        let mut edges_between = 0_usize;
        let mut contradicts_count = 0_usize;
        let mut supportive_count = 0_usize;

        {
            let storage = self.graph.storage();
            for &src in &source_ids {
                for &eid in storage.edges_from(src) {
                    if let Ok(edge) = storage.get_edge(eid) {
                        if source_set.contains(&edge.target) {
                            edges_between += 1;
                            match edge.edge_type {
                                EdgeType::Contradicts => contradicts_count += 1,
                                EdgeType::Supports
                                | EdgeType::ReinforcedBy
                                | EdgeType::ConsolidatedFrom
                                | EdgeType::ExtractedFrom
                                | EdgeType::Entity
                                | EdgeType::Reason => supportive_count += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        let denominator = edges_between.max(1) as f64;
        let contradiction_rate = contradicts_count as f64 / denominator;
        let support_density = supportive_count as f64 / denominator;
        let consistency_score = (support_density - contradiction_rate).clamp(0.0, 1.0);

        let circular_evidence_warning = {
            let storage = self.graph.storage();
            source_ids.iter().any(|&sid| {
                storage.edges_to(sid).iter().any(|&eid| {
                    storage
                        .get_edge(eid)
                        .is_ok_and(|e| e.edge_type == EdgeType::ConsolidatedFrom)
                })
            })
        };

        let single_source_warning = {
            let storage = self.graph.storage();
            storage.get_node(source_ids[0]).ok().is_some_and(|first| {
                let peer = first.origin.peer_id;
                let session = &first.origin.session_id;
                source_ids[1..].iter().all(|&sid| {
                    storage
                        .get_node(sid)
                        .is_ok_and(|n| n.origin.peer_id == peer && n.origin.session_id == *session)
                })
            })
        };

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
        // Additive synthesis: the synthesized strength is evidence-derived, so the
        // relevance-weighted average of the SOURCE composite strengths becomes the
        // synthesis node's decay-exempt evidence prior P_i (sources are never
        // mutated). A creation trace seeds its base level B_i; salience is the
        // logistic projection of B_i + P_i (ADR-0008).
        let synthesis_prior = crystallized_action(&source_actions, &relevance_weights, confidence);

        let id = self.graph.next_node_id();
        let now = request.timestamp;
        // Creation trace at the floor decay d_j = m_type·α (Pavlik & Anderson 2005).
        let creation_decay =
            crate::mechanics::priors::decay_multiplier_for_type(&request.node_type)
                * crate::mechanics::priors::DECAY_INTERCEPT;
        let mut access_history = VecDeque::new();
        access_history.push_back(AccessTrace {
            at: now,
            decay: creation_decay,
        });
        let base_level = crate::mechanics::forgetting::compute_base_level(&access_history, now);
        let initial_action = base_level + synthesis_prior;
        let initial_salience = crate::mechanics::priors::project_salience(initial_action);
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
            retained_action: initial_action,
            evidence_prior: synthesis_prior,
            access_count: 0,
            access_history,
            tier: crate::graph::MemoryTier::Auto,
            origin: request.origin,
            entity_tags: request.entity_tags,
            metadata: HashMap::new(),
        };

        self.graph.add_node(node)?;
        self.emit_event(GraphEvent::NodeCreated {
            node_id: id,
            node_type: crystal_type.clone(),
        });

        // Synthesis node (already inserted) — seeds the provenance edges' cold-start
        // coupling. `ConsolidatedFrom` conductance is the calibrated log-LR prior over
        // the {sim, entity, scope, type} features, never an authored weight (ADR-0002).
        let synthesis_node = self.graph.get_node(id)?.clone();
        let mut consolidation_edges = Vec::with_capacity(source_ids.len());
        for &source_id in &source_ids {
            let Ok(source_node) = self.graph.get_node(source_id).cloned() else {
                continue;
            };
            let conductance = self.cold_start_conductance(
                &synthesis_node,
                &source_node,
                &EdgeType::ConsolidatedFrom,
            );
            if !conductance.is_finite() {
                continue;
            }
            let edge_id = self.graph.next_edge_id();
            let edge = Edge::seeded(
                edge_id,
                id,
                source_id,
                EdgeType::ConsolidatedFrom,
                conductance,
                crate::graph::edge::EdgeSource::Inferred,
                now,
                now,
                HashMap::new(),
            );
            self.graph.add_edge(edge)?;
            self.emit_event(GraphEvent::EdgeCreated {
                edge_id,
                source: id,
                target: source_id,
                edge_type: EdgeType::ConsolidatedFrom,
            });
            consolidation_edges.push(edge_id);
        }

        let mut attraction_edges = Vec::new();
        if let Some(ref embedding) = crystal_embedding {
            let mut candidate_set = HashSet::new();
            for node_id in self.graph.storage().node_ids_descending_limit(256) {
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
                // No mass / gravity boost (overview.md): attraction is the
                // similarity * type-affinity candidate-selection score only.
                let score = attraction_score(similarity, tau);

                if should_create_edge(score, &crystal_type, &candidate.node_type) {
                    scored.push((candidate_id, score));
                }
            }

            let eta = learning_rate(TARGET_COACTIVATION_N);
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
                    // Reinforce the conductance reservoir via bounded Hebbian-Oja, not
                    // the bounded weight projection (conductance.md / ADR-0002).
                    let edge_id = existing.id;
                    let current = self.graph.storage().get_conductance(edge_id)?;
                    let next = hebbian_oja(current, clamp_unit_finite(score), eta);
                    if next.is_finite() {
                        self.graph.storage_mut().set_conductance(edge_id, next)?;
                    }
                } else {
                    let Ok(candidate_node) = self.graph.get_node(candidate_id).cloned() else {
                        continue;
                    };
                    // Cold-start density gate (conductance.md "Cold Start":
                    // `if coupling_seed >= conductance_threshold: create edge`).
                    let seed = self.cold_start_coupling_seed(
                        &synthesis_node,
                        &candidate_node,
                        &EdgeType::Semantic,
                    );
                    if !crate::mechanics::priors::coupling_clears_threshold(seed) {
                        continue;
                    }
                    // Cold-start coupling seed; weight derived from the reservoir.
                    let conductance = crate::mechanics::priors::initialize_conductance(seed);
                    if !conductance.is_finite() {
                        continue;
                    }
                    let edge_id = self.graph.next_edge_id();
                    let edge = Edge::seeded(
                        edge_id,
                        id,
                        candidate_id,
                        EdgeType::Semantic,
                        conductance,
                        crate::graph::edge::EdgeSource::Auto,
                        now,
                        now,
                        HashMap::new(),
                    );
                    self.graph.add_edge(edge)?;
                    self.emit_event(GraphEvent::EdgeCreated {
                        edge_id,
                        source: id,
                        target: candidate_id,
                        edge_type: EdgeType::Semantic,
                    });
                    attraction_edges.push(edge_id);
                }
            }
        }

        // Additive synthesis is non-destructive: crystallize NEVER mutates its
        // sources (overview.md `crystallize` contract, interactions.md
        // `Crystallized`). The synthesis node and its `ConsolidatedFrom` edges are
        // added; the source reservoirs, timestamps, and content are left untouched.

        Ok(CrystallizeResult {
            node_id: id,
            consolidation_edges,
            dedup_score,
            attraction_edges,
            nodes_reinforced: 0,
            initial_salience,
            consistency_score,
            contradiction_rate,
            support_density,
            circular_evidence_warning,
            single_source_warning,
        })
    }

    /// Cold-start conductance `C_ij = initialize_conductance(coupling_seed(...))`
    /// for a new link between two nodes under a relation (conductance.md).
    ///
    /// Extracts the four normalized NPMI features `{sim, entity, scope, type}` from
    /// the endpoints and combines them with the single `beta_coupling` regression
    /// vector. Pure over the two nodes and the edge type; reads no storage.
    fn cold_start_conductance(&self, from: &Node, to: &Node, edge_type: &EdgeType) -> f64 {
        use crate::mechanics::priors::initialize_conductance;
        initialize_conductance(self.cold_start_coupling_seed(from, to, edge_type))
    }

    /// The cold-start `coupling_seed = sum_f beta_coupling[f] * npmi_f` for a
    /// candidate link (conductance.md "Cold Start").
    ///
    /// Extracts the four normalized NPMI features `{sim, entity, scope, type}` from
    /// the endpoints under the requested relation and combines them with the single
    /// `beta_coupling` regression vector. This is the value the documented density
    /// gate `coupling_seed >= conductance_threshold`
    /// ([`crate::mechanics::priors::coupling_clears_threshold`]) is tested against,
    /// and the input to [`Self::cold_start_conductance`]. Pure; reads no storage.
    fn cold_start_coupling_seed(&self, from: &Node, to: &Node, edge_type: &EdgeType) -> f64 {
        use crate::mechanics::attraction::cosine_similarity;
        use crate::mechanics::frustration::scope_overlap;
        use crate::mechanics::priors::{coupling_seed, edge_type_affinity_npmi};

        let sim_npmi = match (&from.embedding, &to.embedding) {
            (Some(a), Some(b)) => cosine_similarity(a, b),
            _ => 0.0,
        };
        let entity_npmi = entity_overlap_npmi(&from.entity_tags, &to.entity_tags);
        let scope_npmi = scope_overlap(&from.origin.scope, &to.origin.scope);
        let type_npmi = edge_type_affinity_npmi(edge_type);

        coupling_seed(sim_npmi, entity_npmi, scope_npmi, type_npmi)
    }

    /// Create a typed link, seeding its conductance from the cold-start coupling.
    ///
    /// Conductance is **never set directly** (conductance.md): the caller does not
    /// supply a weight. When a link is created before any co-activation history
    /// exists, its initial `C_ij` is the calibrated log-LR prior estimated from the
    /// four normalized NPMI features `{sim, entity, scope, type}` of the two
    /// endpoints under the requested relation:
    ///
    /// ```text
    /// coupling_seed = sum_f beta_coupling[f] * npmi_f(from, to, edge_type)
    /// C_ij          = initialize_conductance(coupling_seed)
    /// weight        = project_weight(C_ij)        // bounded projection
    /// ```
    ///
    /// (`crate::mechanics::priors::coupling_seed` over the
    /// `{sim, entity, scope, type}` features, then `initialize_conductance`.) The
    /// resulting `weight` is the bounded projection of the seeded reservoir, written
    /// only as the projection of `C_ij` (ADR-0002). Committed path flux and
    /// co-readout later replace this cold-start prior with measured strength via the
    /// bounded Hebbian-Oja update. Both endpoints must exist.
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType) -> Result<EdgeId, Error> {
        let from_node = self.graph.get_node(from)?.clone();
        let to_node = self.graph.get_node(to)?.clone();

        // Cold-start coupling seed: a calibrated log-LR prior from the four
        // normalized NPMI features of the endpoints under this relation
        // (conductance.md "Cold Start"). The conductance is derived, never set.
        let conductance = self.cold_start_conductance(&from_node, &to_node, &edge_type);
        if !conductance.is_finite() {
            return Err(Error::NonFinite("seeded conductance".to_string()));
        }
        let weight = crate::mechanics::priors::project_weight(conductance);

        let id = self.graph.next_edge_id();
        let now = Timestamp::now();
        let is_supersedes = edge_type == EdgeType::Supersedes;
        let event_edge_type = edge_type.clone();
        let edge = Edge {
            id,
            source: from,
            target: to,
            edge_type,
            weight,
            conductance,
            edge_source: crate::graph::edge::EdgeSource::Manual,
            created_at: now,
            accessed_at: now,
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
        self.emit_event(GraphEvent::EdgeCreated {
            edge_id: id,
            source: from,
            target: to,
            edge_type: event_edge_type,
        });
        Ok(id)
    }

    /// Apply a `PathUsed` interaction — bounded Hebbian-Oja conductance update on
    /// the edges actually used by a committed context generation.
    ///
    /// Operates on the authoritative `C_ij` reservoir (log likelihood ratio,
    /// ADR-0002), NOT on the bounded `weight` projection. For each `(edge, flux)`
    /// pair the conductance moves by the single-`eta` bounded Hebbian-Oja step
    /// `dC_ij = eta * flux_ij * (1 - project_weight(C_ij))`
    /// ([`crate::mechanics::interactions::hebbian_oja`], conductance.md /
    /// interactions.md). The Oja bound — realized on the projection per
    /// ADR-0002/Decision 5 ([`crate::mechanics::priors`]) — prevents raw Hebbian
    /// runaway and hub explosion. `set_conductance` then persists the moved
    /// reservoir and re-projects `weight = project_weight(C_ij')`.
    ///
    /// Read-only retrieval never reaches here: only a committed retrieval trace
    /// (path current `I_ij` from the query field) supplies `flux`. `edges` and
    /// `flux` must be the same length; each edge must exist; every `flux` value
    /// must be finite. Edges are processed in the given order; the operation is
    /// deterministic for a fixed input.
    pub fn apply_path_used(&mut self, edges: Vec<EdgeId>, flux: Vec<f64>) -> Result<(), Error> {
        use crate::mechanics::interactions::hebbian_oja;
        use crate::mechanics::priors::{TARGET_COACTIVATION_N, learning_rate};

        if edges.len() != flux.len() {
            return Err(Error::InvalidInput(
                "edges and flux must have the same length".to_string(),
            ));
        }
        if !finite_vector(&flux) {
            return Err(Error::NonFinite("path flux".to_string()));
        }

        let eta = learning_rate(TARGET_COACTIVATION_N);
        for (&edge_id, &flux_ij) in edges.iter().zip(flux.iter()) {
            // Validate the edge exists before mutating (commit must match graph state).
            self.graph.get_edge(edge_id)?;
            let current = self.graph.storage().get_conductance(edge_id)?;
            let next = hebbian_oja(current, flux_ij, eta);
            if !next.is_finite() {
                return Err(Error::NonFinite(format!(
                    "conductance for edge {edge_id:?}"
                )));
            }
            // `set_conductance` writes both C_ij and the weight projection.
            self.graph.storage_mut().set_conductance(edge_id, next)?;
        }
        Ok(())
    }

    /// Apply a `CoReadout` interaction — bounded Hebbian-Oja conductance update on
    /// edges between site pairs that were read out together in a committed result.
    ///
    /// Operates on the authoritative `C_ij` reservoir (ADR-0002), NOT on the
    /// bounded `weight` projection. For each `(site_i, site_j)` pair the
    /// co-readout flux is `co_flux_ij = min(a_i, a_j)` (conductance.md): the pair
    /// strengthens only as much as its weaker co-activation. That flux drives the
    /// same single-`eta` bounded step
    /// `dC_ij = eta * co_flux_ij * (1 - project_weight(C_ij))`
    /// ([`crate::mechanics::interactions::hebbian_oja`]) on every existing edge
    /// connecting the two sites (in either direction). `set_conductance` persists
    /// the moved reservoir and re-projects `weight`.
    ///
    /// `pairs` and `activations` must be the same length; each activation pair
    /// `(a_i, a_j)` must be finite. Pairs with no connecting edge are skipped (no
    /// edge is created — co-readout strengthens existing paths only). For a fixed
    /// graph and input the updated edges are visited in a deterministic order.
    pub fn apply_co_readout(
        &mut self,
        pairs: Vec<(NodeId, NodeId)>,
        activations: Vec<(f64, f64)>,
    ) -> Result<(), Error> {
        use crate::mechanics::interactions::hebbian_oja;
        use crate::mechanics::priors::{TARGET_COACTIVATION_N, learning_rate};

        if pairs.len() != activations.len() {
            return Err(Error::InvalidInput(
                "pairs and activations must have the same length".to_string(),
            ));
        }

        let eta = learning_rate(TARGET_COACTIVATION_N);
        for (&(site_i, site_j), &(a_i, a_j)) in pairs.iter().zip(activations.iter()) {
            if !a_i.is_finite() || !a_j.is_finite() {
                return Err(Error::NonFinite("co-readout activation".to_string()));
            }
            // Co-readout flux is the weaker of the two co-activations (conductance.md).
            let co_flux = a_i.min(a_j).max(0.0);

            // Collect every edge connecting the two sites (either direction),
            // de-duplicated and stably ordered for determinism.
            let mut edge_ids: BTreeSet<EdgeId> = BTreeSet::new();
            for &eid in self.graph.edges_from(site_i) {
                if let Ok(edge) = self.graph.get_edge(eid) {
                    if edge.target == site_j {
                        edge_ids.insert(eid);
                    }
                }
            }
            for &eid in self.graph.edges_from(site_j) {
                if let Ok(edge) = self.graph.get_edge(eid) {
                    if edge.target == site_i {
                        edge_ids.insert(eid);
                    }
                }
            }

            for edge_id in edge_ids {
                let current = self.graph.storage().get_conductance(edge_id)?;
                let next = hebbian_oja(current, co_flux, eta);
                if !next.is_finite() {
                    return Err(Error::NonFinite(format!(
                        "conductance for edge {edge_id:?}"
                    )));
                }
                // `set_conductance` writes both C_ij and the weight projection.
                self.graph.storage_mut().set_conductance(edge_id, next)?;
            }
        }
        Ok(())
    }

    /// Touch a node — an `Accessed` interaction (ADR-0008).
    ///
    /// A committed access appends a now-stamped trace to the node's `access_history`,
    /// raising the base level `B_i`; the `salience`/`retained_action` cache is then
    /// refreshed from `B_i(now) + P_i`:
    ///
    /// ```text
    /// d_new    = m_type·(c·e^{m} + α)              // activation-dependent (P&A 2005)
    /// traces_i ← append(traces_i, {now, d_new})    // bounded 32-trace window
    /// B_i      = ln( Σ_j (now − at_j)^(−d_j) )     // ages prior traces to now
    /// salience = logistic(B_i + P_i)               // refresh cache
    /// ```
    ///
    /// Decay-first ordering is intrinsic: `B_i` ages every prior trace to `now`
    /// inside the same sum that adds the new one — there is no scalar decay step and
    /// no scalar access gain. `accessed_at` is advanced to `now`. The decay-exempt
    /// `P_i` is unchanged. Non-finite values are rejected at the boundary.
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error> {
        if self.is_retracted(node_id)? {
            return Ok(());
        }
        // `Accessed` interaction: append a trace + recompute B_i. Shares the exact
        // path with `commit_accessed`; `ACCESS_READOUT_WORK` is the canonical
        // per-touch readout-work tag (the access effect is the appended trace).
        self.commit_accessed(node_id, ACCESS_READOUT_WORK, now)
    }

    /// Batch `Accessed` interaction — apply [`touch`](Engine::touch) to many sites at
    /// one timestamp (interactions.md `Accessed`).
    ///
    /// Each site is processed independently with the same trace-append semantics as
    /// the single-site `touch`: a now-stamped trace is appended to its
    /// `access_history` (raising `B_i`), then the `salience`/`retained_action` cache
    /// is refreshed from `B_i(now) + P_i`. Retracted sites are skipped. Iteration
    /// follows the caller-supplied slice order, so the operation is deterministic for
    /// a fixed `(graph, ids, now)`.
    ///
    /// Returns the ids whose reservoirs were actually moved (i.e. non-retracted),
    /// deduplicated in first-seen order, so callers can attribute the access work.
    pub fn touch_batch(
        &mut self,
        node_ids: &[NodeId],
        now: Timestamp,
    ) -> Result<Vec<NodeId>, Error> {
        let mut touched: Vec<NodeId> = Vec::new();
        let mut seen: HashSet<NodeId> = HashSet::new();
        for &node_id in node_ids {
            if !seen.insert(node_id) {
                continue; // already touched this id in this batch
            }
            if self.is_retracted(node_id)? {
                continue;
            }
            self.commit_accessed(node_id, ACCESS_READOUT_WORK, now)?;
            touched.push(node_id);
        }
        Ok(touched)
    }

    /// Set the explicit memory tier for a node.
    ///
    /// `Core` tier nodes are protected from decay in `tick()`.
    /// `Auto` restores natural salience-based tier assignment.
    pub fn set_tier(&mut self, node_id: NodeId, tier: MemoryTier) -> Result<(), Error> {
        let node = self.graph.get_node_mut(node_id)?;
        let old_tier = node.tier.clone();
        node.tier = tier;
        let new_tier = node.tier.clone();
        if old_tier != new_tier {
            self.emit_event(GraphEvent::TierTransition {
                node_id,
                from_tier: old_tier,
                to_tier: new_tier,
            });
        }
        Ok(())
    }

    /// Get the current memory tier of a node.
    pub fn get_tier(&self, node_id: NodeId) -> Result<MemoryTier, Error> {
        let node = self.graph.get_node(node_id)?;
        Ok(node.tier.clone())
    }

    /// Read-only observability getter for a node's authoritative retained-action
    /// reservoir `A_i` (log need-odds — ADR-0002/0003).
    ///
    /// Exposes the reservoir directly so callers can inspect the log-odds state that
    /// `salience` is a bounded projection of. This is a pure read: it applies no decay
    /// and mutates nothing (retrieval/observation is read-only — ADR-0004). For the
    /// public bounded projection use the node's `salience`; the reservoir only moves
    /// through [`commit`](Engine::commit) / [`tick`](Engine::tick).
    pub fn retained_action(&self, node_id: NodeId) -> Result<f64, Error> {
        self.graph.storage().get_retained_action(node_id)
    }

    /// Read-only observability getter for an edge's authoritative conductance
    /// reservoir `C_ij` (log likelihood ratio — ADR-0002/0003).
    ///
    /// Exposes the reservoir directly so callers can inspect the log-LR state that the
    /// public edge `weight` is a bounded projection of. This is a pure read and mutates
    /// nothing; the reservoir only moves through [`commit`](Engine::commit) /
    /// [`tick`](Engine::tick) (`PathUsed`/`CoReadout` Hebbian-Oja and idle leakage).
    pub fn conductance(&self, edge_id: EdgeId) -> Result<f64, Error> {
        self.graph.storage().get_conductance(edge_id)
    }

    /// Explicitly invalidate a node — mark it as retracted.
    ///
    /// Retracted nodes:
    /// - Are excluded from `search()` results and `query()` spreading activation.
    /// - Are still accessible via `get_node()` for audit purposes.
    /// - Are ignored by `touch()` (salience unchanged).
    /// - Trigger a warning when used as a `crystallize()` source.
    ///
    /// Edges are preserved (not deleted). The retraction is recorded in node metadata.
    pub fn retract(
        &mut self,
        node_id: NodeId,
        reason: &str,
        timestamp: Timestamp,
    ) -> Result<(), Error> {
        let node = self.graph.get_node_mut(node_id)?;
        node.metadata
            .insert("retracted".to_string(), "true".to_string());
        node.metadata
            .insert("retraction_reason".to_string(), reason.to_string());
        node.metadata
            .insert("retracted_at".to_string(), timestamp.0.to_string());
        node.updated_at = timestamp;
        Ok(())
    }

    /// Returns true if the node has been retracted.
    pub fn is_retracted(&self, node_id: NodeId) -> Result<bool, Error> {
        let node = self.graph.get_node(node_id)?;
        Ok(node.metadata.get("retracted").is_some_and(|v| v == "true"))
    }

    /// Apply a consumer feedback signal — a `FeedbackReceived` interaction.
    ///
    /// Rescorla-Wagner prediction error on the decay-exempt evidence prior `P_i`
    /// (ADR-0008): `dP_i = eta * (lambda - predicted)` where `lambda` is the reward
    /// target derived from the signal and `predicted` is the current `P_i`
    /// (interactions.md). Feedback moves the persistent prior, NOT the base level:
    /// already-well-predicted sites move less; negative feedback lowers `P_i` but
    /// preserves provenance and source content. The `salience`/`retained_action`
    /// cache is then refreshed from `B_i(now) + P_i_new`.
    pub fn apply_feedback(
        &mut self,
        node_id: NodeId,
        signal: crate::mechanics::social::FeedbackSignal,
    ) -> Result<(), Error> {
        use crate::mechanics::interactions::{lambda_reward, rescorla_wagner};
        use crate::mechanics::priors::{TARGET_COACTIVATION_N, learning_rate, project_salience};

        let current_salience = self.graph.storage().get_salience(node_id)?;
        let current_prior = self.graph.storage().get_evidence_prior(node_id)?;
        let now = self.graph.get_node(node_id)?.accessed_at;

        let eta = learning_rate(TARGET_COACTIVATION_N);
        let new_prior = rescorla_wagner(current_prior, lambda_reward(&signal), eta);
        if !new_prior.is_finite() {
            return Err(Error::NonFinite(format!(
                "evidence prior for node {node_id:?}"
            )));
        }
        self.graph
            .storage_mut()
            .set_evidence_prior(node_id, new_prior)?;

        // Refresh the composite cache from B_i(now) + P_i_new.
        let new_action = self.recompute_composite_action(node_id, now)?;
        if !new_action.is_finite() {
            return Err(Error::NonFinite(format!(
                "retained action for node {node_id:?}"
            )));
        }
        self.graph
            .storage_mut()
            .set_retained_action(node_id, new_action)?;
        let new_salience = project_salience(new_action);

        if (new_salience - current_salience).abs() > SALIENCE_CHANGE_EPSILON {
            self.emit_event(GraphEvent::SalienceChanged {
                node_id,
                old: current_salience,
                new: new_salience,
            });
            if current_salience < ARCHIVE_SALIENCE_THRESHOLD
                && new_salience >= ARCHIVE_SALIENCE_THRESHOLD
            {
                self.emit_event(GraphEvent::NodeRevived {
                    node_id,
                    new_salience,
                });
            }
        }
        Ok(())
    }

    /// Commit a retrieved [`ContextPackage`] — the only reservoir-mutation path
    /// besides [`tick`](Engine::tick).
    ///
    /// Retrieval is read-only (ADR-0004): `query`/`search` return a package and its
    /// [`CommitTrace`](crate::query::CommitTrace) but change no reservoir. This method
    /// integrates the work the caller actually used, in two strict stages:
    ///
    /// 1. **Validate the trace against the current graph** (interactions.md: "Commit
    ///    must validate that the trace corresponds to the graph state it updates").
    ///    Every accessed / co-readout / tension node must still exist, and every
    ///    `PathUsed` edge must still exist *with the same source, target, and type* it
    ///    had at retrieval (topology match). A stale or mismatched trace is a hard
    ///    [`Error`] and **no** reservoir is touched — commit is all-or-nothing.
    /// 2. **Integrate the committed interactions** into the persistent substrate
    ///    (ADR-0008/0003):
    ///    - `Accessed` — append a now-stamped trace to each packaged site's
    ///      `access_history` (raising `B_i`; decay-first is intrinsic to the
    ///      base-level sum), then refresh its salience cache;
    ///    - `FeedbackReceived` — Rescorla-Wagner `dP_i = eta*(lambda - P_i)` on each
    ///      packaged site's decay-exempt evidence prior, with `lambda` mapped from
    ///      `feedback` ([`ConfidenceLevel`](crate::ConfidenceLevel) → `lambda_reward`).
    ///      `None` feedback records use without a feedback signal;
    ///    - `PathUsed` — bounded Hebbian-Oja `dC_ij = eta*flux*(1 - project_weight(C))`
    ///      on each edge that carried committed path current `I_ij`;
    ///    - `CoReadout` — the same Hebbian-Oja step with flux `min(a_i, a_j)` on the
    ///      edges between co-read pairs;
    ///    - `TensionActivated` — records each presented contradiction (frustration is
    ///      surfaced, never suppressed: ADR-0006 — no activation is reduced here).
    ///
    /// On success the returned package has its `committed_ids` populated with the
    /// sites whose reservoirs moved, and a [`CommitReport`] of the integrated work is
    /// returned. Deterministic for a fixed graph + trace.
    pub fn commit(
        &mut self,
        package: ContextPackage,
        feedback: Option<crate::mechanics::social::ConfidenceLevel>,
    ) -> Result<(ContextPackage, CommitReport), Error> {
        use crate::mechanics::interactions::{hebbian_oja, lambda_reward, rescorla_wagner};
        use crate::mechanics::priors::{TARGET_COACTIVATION_N, learning_rate, project_salience};

        // Clone the trace so the package can be returned (with `committed_ids`) after
        // the integration loops; the trace is small and transient.
        let trace = package.commit_trace.clone();
        let trace = &trace;

        // ── Stage 1: validate the whole trace BEFORE any mutation (all-or-nothing) ──

        // Accessed / co-readout / tension nodes must still exist.
        for site in &trace.accessed {
            if !site.readout_work.is_finite() {
                return Err(Error::NonFinite(format!(
                    "readout work for node {:?}",
                    site.node_id
                )));
            }
            self.graph.get_node(site.node_id)?;
        }
        for pair in &trace.co_readout {
            if !pair.activation_a.is_finite() || !pair.activation_b.is_finite() {
                return Err(Error::NonFinite("co-readout activation".to_string()));
            }
            self.graph.get_node(pair.node_a)?;
            self.graph.get_node(pair.node_b)?;
        }
        for tension in &trace.tensions_activated {
            if !tension.stress.is_finite() {
                return Err(Error::NonFinite("tension stress".to_string()));
            }
            self.graph.get_node(tension.node_a)?;
            self.graph.get_node(tension.node_b)?;
        }

        // PathUsed edges must still exist AND match the recorded topology snapshot.
        // A moved/retyped/deleted edge means the trace no longer describes the graph
        // it would update — a stale trace, which is a hard error (interactions.md).
        for path in &trace.path_used {
            if !path.flux.is_finite() {
                return Err(Error::NonFinite(format!(
                    "path flux for edge {:?}",
                    path.edge_id
                )));
            }
            let edge = self.graph.get_edge(path.edge_id)?;
            if edge.source != path.source
                || edge.target != path.target
                || edge.edge_type != path.edge_type
            {
                return Err(Error::InvalidInput(format!(
                    "stale commit trace: edge {:?} topology changed since retrieval",
                    path.edge_id
                )));
            }
        }

        // ── Stage 2: integrate the committed interactions into the reservoirs ──

        let eta = learning_rate(TARGET_COACTIVATION_N);
        let mut report = CommitReport::default();
        // Sites whose reservoirs moved, in stable order, deduplicated.
        let mut committed: Vec<NodeId> = Vec::new();
        let mut committed_set: HashSet<NodeId> = HashSet::new();

        // `Accessed` — decay-then-reinforce on each packaged site (skip retracted).
        let now = Timestamp::now();
        for site in &trace.accessed {
            if self.is_retracted(site.node_id)? {
                continue;
            }
            self.commit_accessed(site.node_id, site.readout_work, now)?;
            report.sites_accessed += 1;
            if committed_set.insert(site.node_id) {
                committed.push(site.node_id);
            }
        }

        // `FeedbackReceived` — Rescorla-Wagner toward the confidence-derived target.
        if let Some(level) = feedback {
            let signal: crate::mechanics::social::FeedbackSignal = level.into();
            let lambda = lambda_reward(&signal);
            // Same feedback also nudges the reliability of the peers who originated
            // the used sites (social.md: "positive feedback reinforces peer
            // reliability; negative feedback lowers trust through prediction error").
            // Each distinct origin peer is nudged once, in stable order, so a multi-
            // site package does not over-credit one peer.
            let feedback_strength = signal.signed_strength();
            let mut peers_to_nudge: Vec<crate::graph::types::PeerId> = Vec::new();
            let mut peer_seen: HashSet<crate::graph::types::PeerId> = HashSet::new();
            for site in &trace.accessed {
                if self.is_retracted(site.node_id)? {
                    continue;
                }
                // Feedback moves the decay-exempt evidence prior P_i (ADR-0008),
                // then the composite cache is refreshed from B_i(now) + P_i_new.
                let current_prior = self.graph.storage().get_evidence_prior(site.node_id)?;
                let current_salience = self.graph.storage().get_salience(site.node_id)?;
                let new_prior = rescorla_wagner(current_prior, lambda, eta);
                if !new_prior.is_finite() {
                    return Err(Error::NonFinite(format!(
                        "evidence prior for node {:?}",
                        site.node_id
                    )));
                }
                self.graph
                    .storage_mut()
                    .set_evidence_prior(site.node_id, new_prior)?;
                let new_action = self.recompute_composite_action(site.node_id, now)?;
                if !new_action.is_finite() {
                    return Err(Error::NonFinite(format!(
                        "retained action for node {:?}",
                        site.node_id
                    )));
                }
                self.graph
                    .storage_mut()
                    .set_retained_action(site.node_id, new_action)?;
                let new_salience = project_salience(new_action);
                if (new_salience - current_salience).abs() > SALIENCE_CHANGE_EPSILON {
                    self.emit_event(GraphEvent::SalienceChanged {
                        node_id: site.node_id,
                        old: current_salience,
                        new: new_salience,
                    });
                }
                if let Ok(node) = self.graph.get_node(site.node_id) {
                    let peer_id = node.origin.peer_id;
                    if self.peers.get_peer(peer_id).is_some() && peer_seen.insert(peer_id) {
                        peers_to_nudge.push(peer_id);
                    }
                }
                report.feedback_applied += 1;
                if committed_set.insert(site.node_id) {
                    committed.push(site.node_id);
                }
            }
            // Route every peer trust move through the single traceable evidence path.
            for peer_id in peers_to_nudge {
                self.update_peer_trust_evidence(peer_id, feedback_strength)?;
            }
        }

        // `PathUsed` — bounded Hebbian-Oja conductance update on used edges.
        for path in &trace.path_used {
            let current = self.graph.storage().get_conductance(path.edge_id)?;
            let next = hebbian_oja(current, path.flux, eta);
            if !next.is_finite() {
                return Err(Error::NonFinite(format!(
                    "conductance for edge {:?}",
                    path.edge_id
                )));
            }
            self.graph
                .storage_mut()
                .set_conductance(path.edge_id, next)?;
            self.graph
                .storage_mut()
                .set_edge_accessed_at(path.edge_id, now)?;
            report.paths_strengthened += 1;
        }

        // `CoReadout` — strengthen the edges between co-read pairs by `min(a_i, a_j)`.
        for pair in &trace.co_readout {
            let co_flux = pair.activation_a.min(pair.activation_b).max(0.0);
            let mut edge_ids: BTreeSet<EdgeId> = BTreeSet::new();
            for &eid in self.graph.edges_from(pair.node_a) {
                if self
                    .graph
                    .get_edge(eid)
                    .is_ok_and(|e| e.target == pair.node_b)
                {
                    edge_ids.insert(eid);
                }
            }
            for &eid in self.graph.edges_from(pair.node_b) {
                if self
                    .graph
                    .get_edge(eid)
                    .is_ok_and(|e| e.target == pair.node_a)
                {
                    edge_ids.insert(eid);
                }
            }
            let mut strengthened = false;
            for edge_id in edge_ids {
                let current = self.graph.storage().get_conductance(edge_id)?;
                let next = hebbian_oja(current, co_flux, eta);
                if !next.is_finite() {
                    return Err(Error::NonFinite(format!(
                        "conductance for edge {edge_id:?}"
                    )));
                }
                self.graph.storage_mut().set_conductance(edge_id, next)?;
                self.graph
                    .storage_mut()
                    .set_edge_accessed_at(edge_id, now)?;
                strengthened = true;
            }
            if strengthened {
                report.pairs_strengthened += 1;
            }
        }

        // `TensionActivated` — record that the conflict was presented. Frustration is
        // surfaced, never suppressed (ADR-0006): no activation is reduced here.
        report.tensions_recorded = trace.tensions_activated.len();

        let mut package = package;
        package.committed_ids = committed;
        Ok((package, report))
    }

    /// `Accessed` integration for one site — append a trace, recompute strength.
    ///
    /// Under `A_i = B_i + P_i` a committed access appends a now-stamped trace (with
    /// its own activation-dependent decay `d_new`, Pavlik & Anderson 2005) to the
    /// node's `access_history` (dissipation.md / ADR-0008). Decay-first ordering is
    /// intrinsic: recomputing `B_i = ln(Σ_j (now − at_j)^(−d_j))` ages every
    /// prior trace to `now` inside the same sum that adds the new one — there is no
    /// scalar decay step and no scalar access gain. The `salience`/`retained_action`
    /// cache is then refreshed from the recomputed `B_i(now) + P_i` and `accessed_at`
    /// advanced to `now`. `_readout_work` no longer scales a gain (the access effect
    /// is the appended trace), but is retained on the signature for the commit
    /// pipeline's per-readout work tag.
    fn commit_accessed(
        &mut self,
        node_id: NodeId,
        _readout_work: f64,
        now: Timestamp,
    ) -> Result<(), Error> {
        let current_salience = self.graph.storage().get_salience(node_id)?;

        // The committed access IS the new trace (raises B_i); persist it durably. Its
        // activation-dependent decay d_new = m_type·(c·e^{m} + α) is computed ONCE
        // from the EXISTING history (before the append), where m is the activation of
        // the prior traces at `now` (Pavlik & Anderson 2005). The trace then stores
        // its decay immutably.
        let new_trace = {
            use crate::mechanics::forgetting::compute_trace_decay;
            use crate::mechanics::priors::{
                DECAY_INTERCEPT, DECAY_SCALE, decay_multiplier_for_type,
            };
            let m_type = decay_multiplier_for_type(self.graph.storage().get_node_type(node_id)?);
            let existing = self.graph.storage().get_access_history(node_id)?;
            let d_new = compute_trace_decay(existing, now, m_type, DECAY_SCALE, DECAY_INTERCEPT);
            AccessTrace {
                at: now,
                decay: d_new,
            }
        };
        self.graph
            .storage_mut()
            .append_access_trace(node_id, new_trace)?;

        let new_action = self.recompute_composite_action(node_id, now)?;
        if !new_action.is_finite() {
            return Err(Error::NonFinite(format!(
                "retained action for node {node_id:?}"
            )));
        }
        // Refresh the composite cache (recomputes salience = logistic(B_i + P_i)).
        self.graph
            .storage_mut()
            .set_retained_action(node_id, new_action)?;
        let new_salience = crate::mechanics::priors::project_salience(new_action);
        if (new_salience - current_salience).abs() > SALIENCE_CHANGE_EPSILON {
            self.emit_event(GraphEvent::SalienceChanged {
                node_id,
                old: current_salience,
                new: new_salience,
            });
            if current_salience < ARCHIVE_SALIENCE_THRESHOLD
                && new_salience >= ARCHIVE_SALIENCE_THRESHOLD
            {
                self.emit_event(GraphEvent::NodeRevived {
                    node_id,
                    new_salience,
                });
            }
        }
        self.graph.storage_mut().set_accessed_at(node_id, now)?;

        let node = self.graph.get_node_mut(node_id)?;
        node.access_count += 1;
        Ok(())
    }

    /// Recompute the composite retained action `A_i = B_i(now) + P_i` for a node.
    ///
    /// `B_i` is the multi-trace ACT-R base level over the node's current
    /// `access_history` aged to `now`, where each trace carries its OWN
    /// activation-dependent decay `d_j` (Pavlik & Anderson 2005), so
    /// `B_i = ln(Σ_j (now − at_j)^(−d_j))`. `P_i` is the stored, decay-exempt
    /// `evidence_prior`. The creation trace seeded at ingest keeps `B_i` finite for a
    /// node that has never been accessed.
    fn recompute_composite_action(&self, node_id: NodeId, now: Timestamp) -> Result<f64, Error> {
        use crate::mechanics::forgetting::compute_base_level;

        let history = self.graph.storage().get_access_history(node_id)?;
        let base_level = compute_base_level(history, now);
        let prior = self.graph.storage().get_evidence_prior(node_id)?;
        Ok(base_level + prior)
    }

    /// Advance time — a batch `TimeElapsed` interaction that recomputes every node's
    /// salience from its base level aged to `now`.
    ///
    /// Under `A_i = B_i + P_i` (ADR-0008) time applies NO scalar shift: each node's
    /// `salience = logistic(B_i(now) + P_i)` is recomputed, where
    /// `B_i = ln(Σ_j (now − at_j)^(−d_j))` falls purely because `now` advanced
    /// relative to the fixed access traces (each with its own stored `d_j`), and the
    /// decay-exempt `P_i` is unchanged.
    /// `accessed_at` is left untouched, preserving last user-access semantics. Idle
    /// edges still leak their conductance.
    ///
    /// Core tier and `IdentityCore` are protected (never decay under ordinary tick).
    /// Non-finite values are rejected at the boundary.
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error> {
        use crate::mechanics::priors::project_salience;

        let node_ids = self.graph.storage().all_node_ids();
        let mut nodes_decayed = 0usize;
        let mut nodes_pruned = 0usize;
        let mut total_salience_delta = 0.0_f64;
        let mut edges_leaked = 0usize;
        let mut total_conductance_delta = 0.0_f64;

        for id in node_ids {
            let node_tier = match self.graph.get_node(id) {
                Ok(node) => node.tier.clone(),
                Err(_) => continue,
            };
            // Core tier is protected from ordinary decay.
            if node_tier == MemoryTier::Core {
                continue;
            }

            let current_salience = match self.graph.storage().get_salience(id) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let node_type = match self.graph.storage().get_node_type(id) {
                Ok(kt) => kt.clone(),
                Err(_) => continue,
            };
            // IdentityCore is protected regardless of tier.
            if node_type == KnowledgeType::IdentityCore {
                continue;
            }

            // Recompute B_i(now) + P_i — B_i falls because `now` advanced relative to
            // the fixed traces; no stored reservoir is shifted.
            let new_action = match self.recompute_composite_action(id, now) {
                Ok(a) => a,
                Err(_) => continue,
            };
            if !new_action.is_finite() {
                return Err(Error::NonFinite(format!("retained action for node {id:?}")));
            }
            let new_salience = project_salience(new_action);

            if (new_salience - current_salience).abs() > SALIENCE_CHANGE_EPSILON {
                // `set_retained_action` refreshes the composite cache + salience.
                if self
                    .graph
                    .storage_mut()
                    .set_retained_action(id, new_action)
                    .is_err()
                {
                    continue;
                }
                nodes_decayed += 1;
                total_salience_delta += (current_salience - new_salience).abs();
                self.emit_event(GraphEvent::SalienceChanged {
                    node_id: id,
                    old: current_salience,
                    new: new_salience,
                });

                if current_salience >= ARCHIVE_SALIENCE_THRESHOLD
                    && new_salience < ARCHIVE_SALIENCE_THRESHOLD
                {
                    self.emit_event(GraphEvent::NodeArchived { node_id: id });
                    // "Pruned" = hidden from broad retrieval (projected below the
                    // archive threshold), never deleted (ADR-0008 / ADR-0006).
                    nodes_pruned += 1;
                }

                let old_tier = salience_tier(current_salience);
                let new_tier = salience_tier(new_salience);
                if old_tier != new_tier {
                    self.emit_event(GraphEvent::TierTransition {
                        node_id: id,
                        from_tier: old_tier,
                        to_tier: new_tier,
                    });
                }
            }
        }

        // ── Idle-edge leakage (TimeElapsed on conductance) ────────────────────
        // `C_ij' = C_ij - eta_leak * idle_edge_leakage(C_ij, idle_days)`
        // (conductance.md post-commit plasticity / interactions.md `TimeElapsed`).
        // Unused weak coupling drains over time (density control). Idle time is
        // `now - edge.accessed_at` (the committed-use timestamp). Edges incident to
        // a protected node (Core tier / `IdentityCore`) are exempt, mirroring node
        // decay; `Contradicts` edges are excluded (routed to frustration, not
        // propagation — ADR-0005/0006). Edges are never deleted (ADR-0008/0006).
        {
            use crate::mechanics::interactions::leak_idle_edge_default;
            use crate::mechanics::priors::{ETA_LEAK, project_weight};

            if ETA_LEAK > 0.0 {
                let edge_ids = self.graph.storage().all_edge_ids();
                for eid in edge_ids {
                    let (source, target, edge_type, accessed_at) =
                        match self.graph.storage().get_edge(eid) {
                            Ok(e) => (e.source, e.target, e.edge_type.clone(), e.accessed_at),
                            Err(_) => continue,
                        };
                    // Contradicts is excluded from conductance dynamics.
                    if edge_type == EdgeType::Contradicts {
                        continue;
                    }
                    // Protected if either endpoint is protected from ordinary decay.
                    if self.is_protected_node(source) || self.is_protected_node(target) {
                        continue;
                    }

                    let current = match self.graph.storage().get_conductance(eid) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let dt_ms = now.0.saturating_sub(accessed_at.0);
                    let idle_days = dt_ms as f64 / 86_400_000.0;
                    if idle_days <= 0.0 {
                        continue;
                    }

                    let next = leak_idle_edge_default(current, idle_days);
                    if !next.is_finite() {
                        return Err(Error::NonFinite(format!("conductance for edge {eid:?}")));
                    }
                    let weight_before = project_weight(current);
                    let weight_after = project_weight(next);
                    if (weight_before - weight_after).abs() > SALIENCE_CHANGE_EPSILON {
                        // `set_conductance` persists the leaked reservoir and
                        // re-projects `weight = project_weight(C_ij')`. `accessed_at`
                        // is NOT advanced — leakage is not a use (interactions.md).
                        if self.graph.storage_mut().set_conductance(eid, next).is_err() {
                            continue;
                        }
                        edges_leaked += 1;
                        total_conductance_delta += (weight_before - weight_after).abs();
                    }
                }
            }
        }

        self.graph.storage_mut().flush()?;

        Ok(TickReport {
            nodes_decayed,
            nodes_pruned,
            total_salience_delta,
            edges_leaked,
            total_conductance_delta,
        })
    }

    /// True when a node is protected from ordinary `tick` dissipation — Core tier
    /// or `IdentityCore` type (dissipation.md "Memory Tier" / priors decay policy).
    /// Used to exempt protected edges from idle-edge leakage. A missing node is
    /// treated as protected (no leak applied) so a dangling edge is never mutated.
    fn is_protected_node(&self, id: NodeId) -> bool {
        match self.graph.get_node(id) {
            Ok(node) => {
                node.tier == MemoryTier::Core || node.node_type == KnowledgeType::IdentityCore
            }
            Err(_) => true,
        }
    }

    /// Compute the health of the cognitive graph.
    ///
    /// Returns a `HealthReport` with structural metrics and an overall `HealthGrade`.
    /// This is a read-only operation — it does not mutate any state.
    /// Call this periodically to monitor graph quality; do NOT call from `tick()`.
    pub fn health(&self) -> HealthReport {
        let storage = self.graph.storage();
        let node_ids = storage.all_node_ids();
        let edge_ids = storage.all_edge_ids();
        let total_nodes = node_ids.len();
        let total_edges = edge_ids.len();

        let mut orphan_count = 0usize;
        let mut retracted_count = 0usize;
        let mut missing_embedding_count = 0usize;
        let mut salience_sum = 0.0_f64;

        for &nid in &node_ids {
            if let Ok(node) = storage.get_node(nid) {
                if storage.edges_from(nid).is_empty() && storage.edges_to(nid).is_empty() {
                    orphan_count += 1;
                }
                if node.metadata.get("retracted").is_some_and(|v| v == "true") {
                    retracted_count += 1;
                }
                if node.embedding.is_none() {
                    missing_embedding_count += 1;
                }
                salience_sum += storage.get_salience(nid).unwrap_or(0.0);
            }
        }

        let mut contradiction_count = 0usize;
        let mut supersede_count = 0usize;
        for &eid in &edge_ids {
            if let Ok(edge) = storage.get_edge(eid) {
                match edge.edge_type {
                    EdgeType::Contradicts => contradiction_count += 1,
                    EdgeType::Supersedes => supersede_count += 1,
                    _ => {}
                }
            }
        }

        let orphan_rate = if total_nodes > 0 {
            orphan_count as f64 / total_nodes as f64
        } else {
            0.0
        };
        let contradiction_rate = if total_edges > 0 {
            contradiction_count as f64 / total_edges as f64
        } else {
            0.0
        };
        let supersede_rate = if total_edges > 0 {
            supersede_count as f64 / total_edges as f64
        } else {
            0.0
        };
        let avg_salience = if total_nodes > 0 {
            salience_sum / total_nodes as f64
        } else {
            0.0
        };

        let grade = if orphan_rate < 0.05 && contradiction_rate < 0.03 && supersede_rate < 0.10 {
            HealthGrade::A
        } else if orphan_rate < 0.15 && contradiction_rate < 0.08 && supersede_rate < 0.20 {
            HealthGrade::B
        } else if orphan_rate < 0.30 && contradiction_rate < 0.15 && supersede_rate < 0.35 {
            HealthGrade::C
        } else {
            HealthGrade::D
        };

        HealthReport {
            total_nodes,
            orphan_count,
            contradiction_count,
            supersede_count,
            orphan_rate,
            contradiction_rate,
            supersede_rate,
            retracted_count,
            missing_embedding_count,
            peer_count: self.peers.peer_count(),
            avg_salience,
            grade,
        }
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
        search::search(self, input)
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
        let config = QueryConfig {
            now: Some(as_of),
            ..QueryConfig::default()
        };
        let mut package = self.query(query, &config)?;

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

    /// Query the graph from a specific agent's perspective about a subject.
    ///
    /// Returns a `ContextPackage` containing only what the observer agent knows
    /// about the observed reference, filtered by scope and temporal validity.
    /// Non-retroactive: excludes nodes created before the observer's first contribution.
    pub fn query_perspective(
        &self,
        perspective: PerspectiveKey,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use crate::query::Fragment;

        let storage = self.graph.storage();
        let now = config.now.unwrap_or_else(Timestamp::now);

        // Stage 1: Collect all observer nodes and find observer's earliest timestamp
        let observer_node_ids = storage.nodes_by_peer(perspective.observer_peer_id);
        if observer_node_ids.is_empty() {
            return Ok(ContextPackage::empty());
        }

        let observer_earliest = observer_node_ids
            .iter()
            .filter_map(|&nid| storage.get_node(nid).ok())
            .map(|node| node.created_at)
            .min()
            .unwrap_or(Timestamp(0));

        // Stage 2: Filter by observed reference
        let observer_set: HashSet<NodeId> = observer_node_ids.iter().copied().collect();
        let filtered_ids: Vec<NodeId> =
            match &perspective.observed {
                ObservedRef::EntityTag(tag) => {
                    let tagged = storage.nodes_by_entity_tag(tag);
                    tagged
                        .into_iter()
                        .filter(|nid| observer_set.contains(nid))
                        .collect()
                }
                ObservedRef::Agent(target_peer_id) => {
                    let target_nodes: HashSet<NodeId> =
                        storage.nodes_by_peer(*target_peer_id).into_iter().collect();
                    observer_node_ids
                        .iter()
                        .copied()
                        .filter(|&nid| {
                            storage.edges_from(nid).iter().any(|&eid| {
                                storage
                                    .get_edge(eid)
                                    .is_ok_and(|e| target_nodes.contains(&e.target))
                            }) || storage.edges_to(nid).iter().any(|&eid| {
                                storage
                                    .get_edge(eid)
                                    .is_ok_and(|e| target_nodes.contains(&e.source))
                            })
                        })
                        .collect()
                }
                ObservedRef::Node(target_nid) => observer_node_ids
                    .iter()
                    .copied()
                    .filter(|&nid| {
                        if nid == *target_nid {
                            return true;
                        }
                        storage.edges_from(nid).iter().any(|&eid| {
                            storage.get_edge(eid).is_ok_and(|e| e.target == *target_nid)
                        }) || storage.edges_to(nid).iter().any(|&eid| {
                            storage.get_edge(eid).is_ok_and(|e| e.source == *target_nid)
                        })
                    })
                    .collect(),
            };

        if filtered_ids.is_empty() {
            return Ok(ContextPackage::empty());
        }

        // Stage 3: Apply scope filtering
        let scope_filtered: Vec<NodeId> = if perspective.scope.is_universal() {
            filtered_ids
        } else {
            filtered_ids
                .into_iter()
                .filter(|&nid| {
                    storage.get_node(nid).is_ok_and(|node| {
                        let rel = perspective.scope.relation_to(&node.origin.scope);
                        matches!(
                            rel,
                            crate::graph::ScopeRelation::Equal
                                | crate::graph::ScopeRelation::Ancestor
                                | crate::graph::ScopeRelation::Universal
                        )
                    })
                })
                .collect()
        };

        if scope_filtered.is_empty() {
            return Ok(ContextPackage::empty());
        }

        // Stage 4: Build the query potential field from filtered candidates.
        // Retained action enters phi with unit coefficient (potential-landscape.md).
        let mut field = crate::query::QueryField::new();
        for &nid in &scope_filtered {
            let salience = storage.get_salience(nid).unwrap_or(0.0);
            let retained_action = storage.get_retained_action(nid).unwrap_or(0.0);
            field.set(
                nid,
                crate::query::FieldSignals {
                    text_score: salience,
                    retained_action,
                    ..Default::default()
                },
            );
        }

        // Stage 5: Additive directed RWR over conductance (read-only).
        let seed_distribution = field.seed_distribution();
        let response = crate::query::additive_rwr(&seed_distribution, storage, now);

        // Stage 6: Score with the readout score, applying the non-retroactive filter.
        let scoped_config = QueryConfig {
            scope: perspective.scope.clone(),
            ..config.clone()
        };
        let mut package = self.assemble_readout_package(&response, &scoped_config);
        let retain_recent = |frag: &Fragment| match self.graph.get_node(frag.node_id) {
            Ok(node) => node.created_at >= observer_earliest,
            Err(_) => false,
        };
        package.identity.retain(retain_recent);
        package.knowledge.retain(retain_recent);
        package.memories.retain(retain_recent);

        Ok(package)
    }

    fn node_is_valid_at(&self, node_id: NodeId, as_of: Timestamp) -> bool {
        self.graph
            .get_node(node_id)
            .is_ok_and(|node| crate::graph::valid_at(node.valid_from, node.valid_until, as_of))
    }

    fn query_type_filtered(
        &self,
        node_type: &KnowledgeType,
        limit: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use std::cmp::Ordering;

        use crate::query::assembly::{ModeContext, ScoredNode, assemble_context_package_for_mode};

        let storage = self.graph.storage();
        let mut node_ids: Vec<NodeId> = storage
            .nodes_by_type(node_type)
            .into_iter()
            .filter(|&nid| {
                storage
                    .get_node(nid)
                    .ok()
                    .is_none_or(|n| n.metadata.get("retracted").is_none_or(|v| v != "true"))
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

        let query = Query::TypeFiltered {
            node_type: node_type.clone(),
            limit,
        };

        Ok(assemble_context_package_for_mode(
            scored_nodes,
            &query,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.scope,
            &ModeContext::default(),
        ))
    }

    fn query_list(
        &self,
        min_salience: f64,
        limit: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use std::cmp::Ordering;

        use crate::query::assembly::{ModeContext, ScoredNode, assemble_context_package_for_mode};

        let storage = self.graph.storage();
        let mut node_ids: Vec<NodeId> = storage
            .all_node_ids()
            .into_iter()
            .filter(|&nid| {
                storage
                    .get_salience(nid)
                    .is_ok_and(|salience| salience >= min_salience)
                    && storage
                        .get_node(nid)
                        .ok()
                        .is_none_or(|n| n.metadata.get("retracted").is_none_or(|v| v != "true"))
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

        let query = Query::List {
            min_salience,
            limit,
        };

        Ok(assemble_context_package_for_mode(
            scored_nodes,
            &query,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.scope,
            &ModeContext::default(),
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

        use crate::query::assembly::{ModeContext, ScoredNode, assemble_context_package_for_mode};

        let storage = self.graph.storage();
        let mut scored_nodes: Vec<(Timestamp, ScoredNode)> = storage
            .all_node_ids()
            .into_iter()
            .filter_map(|nid| {
                let node = storage.get_node(nid).ok()?;
                if node.created_at < since {
                    return None;
                }
                if let Some(types) = node_types {
                    if !types.iter().any(|node_type| node_type == &node.node_type) {
                        return None;
                    }
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

        let query = Query::Temporal {
            since,
            node_types: node_types.map(|t| t.to_vec()),
            limit,
        };

        Ok(assemble_context_package_for_mode(
            scored_nodes,
            &query,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.scope,
            &ModeContext::default(),
        ))
    }

    fn query_neighborhood(
        &self,
        entity: NodeId,
        max_depth: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        use crate::query::assembly::{ModeContext, ScoredNode, assemble_context_package_for_mode};

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
                if let Ok(edge) = storage.get_edge(eid) {
                    if visited.insert(edge.target) {
                        let next_depth = depth + 1;
                        depths.insert(edge.target, next_depth);
                        queue.push_back((edge.target, next_depth));
                    }
                }
            }

            for &eid in storage.edges_to(nid) {
                if let Ok(edge) = storage.get_edge(eid) {
                    if visited.insert(edge.source) {
                        let next_depth = depth + 1;
                        depths.insert(edge.source, next_depth);
                        queue.push_back((edge.source, next_depth));
                    }
                }
            }
        }

        let adjacent_ids: HashSet<NodeId> = depths
            .iter()
            .filter_map(|(&nid, &depth)| if depth == 1 { Some(nid) } else { None })
            .collect();

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

        let query = Query::Neighborhood {
            entity,
            depth: max_depth,
        };

        Ok(assemble_context_package_for_mode(
            scored_nodes,
            &query,
            &[],
            &[],
            &HashMap::new(),
            config.token_budget,
            config.chars_per_token,
            &config.scope,
            &ModeContext { adjacent_ids },
        ))
    }

    /// Full Associative query pipeline: seed field → additive RWR flow → readout
    /// scoring (with frustration `-w_stress`) → assembly. Contradictions surface as
    /// tensions, never as activation damping (ADR-0006).
    fn query_associative(
        &self,
        seed: NodeId,
        budget: usize,
        config: &QueryConfig,
    ) -> Result<ContextPackage, Error> {
        // Verify seed exists.
        let _ = self.graph.get_node(seed)?;

        let storage = self.graph.storage();
        let now = config.now.unwrap_or_else(Timestamp::now);

        // --- Stage 1: Build the query potential field from the seed + identity ---
        //
        // The explicit seed is the primary restart cue; the active agent's identity
        // nodes contribute an identity bias. `A_i` (retained action) enters phi with
        // unit coefficient by design (potential-landscape.md).
        let mut field = crate::query::QueryField::new();
        field.set(
            seed,
            crate::query::FieldSignals {
                text_score: 1.0,
                retained_action: storage.get_retained_action(seed).unwrap_or(0.0),
                ..Default::default()
            },
        );

        if let Some(ref agent_id) = config.agent_id {
            let peer_id = crate::graph::types::PeerId(agent_id.parse::<u64>().unwrap_or(0));
            for nid in storage.nodes_by_peer(peer_id) {
                let Ok(node) = storage.get_node(nid) else {
                    continue;
                };
                let is_identity = matches!(
                    node.node_type,
                    KnowledgeType::IdentityCore
                        | KnowledgeType::IdentityLearned
                        | KnowledgeType::IdentityState
                );
                if !is_identity {
                    continue;
                }
                let salience = storage.get_salience(nid).unwrap_or(0.0);
                let retained_action = storage.get_retained_action(nid).unwrap_or(0.0);
                let entry = field.entry(nid);
                entry.identity_bias += salience;
                entry.retained_action = retained_action;
            }
        }

        // --- Stage 2: Additive directed RWR over conductance (read-only) ---
        let seed_distribution = field.seed_distribution();
        let response = crate::query::additive_rwr(&seed_distribution, storage, now);

        let _ = budget;
        let package = self.assemble_readout_package(&response, config);
        Ok(package)
    }

    /// Score the settled activation response with the authoritative seven-term
    /// readout score and assemble a `ContextPackage`. Read-only.
    fn assemble_readout_package(
        &self,
        response: &crate::query::ActivationResponse,
        config: &QueryConfig,
    ) -> ContextPackage {
        use crate::mechanics::attraction::cosine_similarity;
        use crate::query::assembly::ScoredNode;
        use crate::query::scoring::{
            ReadoutInputs, TieBreakKey, rank, readout_score, scope_weight,
        };

        let storage = self.graph.storage();
        let now = config.now.unwrap_or_else(Timestamp::now);
        let activations = &response.activation;

        // Surface frustration contradiction pairs (and per-node stress) before
        // scoring, so the readout `-w_stress` term can penalize contradicting
        // bundles toward separation (frustration.md / ADR-0006).
        let (contradiction_pairs, node_stress) =
            crate::query::assembly::collect_contradiction_pairs(
                storage,
                activations,
                config.min_activation,
                now,
            );

        let mut scored: Vec<(f64, TieBreakKey, ScoredNode)> = Vec::new();
        for (&nid, &activation) in activations {
            if activation < config.min_activation {
                continue;
            }
            let node = match storage.get_node(nid) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let salience = storage.get_salience(nid).unwrap_or(0.0);
            let retained_action = storage.get_retained_action(nid).unwrap_or(0.0);
            let impedance = response.impedance.get(&nid).copied().unwrap_or_default();
            let phi = match (&config.query_embedding, &node.embedding) {
                (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
                _ => 0.0,
            };
            let sw = scope_weight(&config.scope, &node.origin.scope, 0);
            // Coarse-level prior bonus + evidence-driven trust projection (social.md
            // "Retrieval Effects"); mirrors the assemble readout path.
            let trust_weight = self
                .get_peer(node.origin.peer_id)
                .map(|p| {
                    p.trust_level.scope_weight_bonus()
                        + crate::mechanics::priors::project_trust(p.trust_reservoir)
                })
                .unwrap_or(0.0);

            let stress = node_stress.get(&nid).copied().unwrap_or(0.0);
            let score = readout_score(&ReadoutInputs {
                activation,
                phi,
                salience,
                impedance,
                scope_weight: sw,
                trust_weight,
                stress,
            });

            scored.push((
                score,
                TieBreakKey {
                    node_id: nid,
                    retained_action,
                    impedance,
                    accessed_at: node.accessed_at,
                },
                ScoredNode {
                    node_id: nid,
                    name: node.name.clone(),
                    summary: node.summary.clone(),
                    content: node.content.clone(),
                    node_type: node.node_type.clone(),
                    relevance: score,
                    origin: node.origin.clone(),
                },
            ));
        }

        scored.sort_by(|(sa, ka, _), (sb, kb, _)| rank(*sa, ka, *sb, kb));
        let scored_nodes: Vec<ScoredNode> = scored.into_iter().map(|(_, _, n)| n).collect();

        let identity_activations: Vec<(NodeId, KnowledgeType, f64)> = activations
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
                    Some(aid) => node.origin.peer_id.0.to_string() == *aid,
                    None => false,
                };
                (is_identity && is_agent).then(|| (nid, node.node_type.clone(), act))
            })
            .collect();

        let mut package = crate::query::assembly::assemble_context_package_for_mode(
            scored_nodes,
            &Query::List {
                min_salience: 0.0,
                limit: usize::MAX,
            },
            &identity_activations,
            &contradiction_pairs,
            activations,
            config.token_budget,
            config.chars_per_token,
            &config.scope,
            &crate::query::assembly::ModeContext::default(),
        );

        // Capture the read-only commit trace from the settled response and the
        // packaged result (ADR-0004 / interactions.md). This carries no persistent
        // quantity — it only lets a later `commit` integrate the work the caller
        // actually used. Retrieval stays read-only.
        package.commit_trace = build_commit_trace(storage, response, &package);
        package
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

        let mut nodes_by_tag: BTreeMap<String, Vec<(NodeId, crate::graph::types::PeerId)>> =
            BTreeMap::new();
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
                        .push((node_id, node.origin.peer_id));
                }
            }
        }

        let mut candidate_pairs = BTreeSet::new();
        let mut clusters_found = 0usize;
        // Distinct peers whose claims were corroborated by ≥1 other agent on a
        // shared entity. Corroboration raises trust (social.md "Peer Trust"); each
        // such peer is nudged once per reflect_batch via the traceable evidence path.
        let mut corroborated_peers: BTreeSet<crate::graph::types::PeerId> = BTreeSet::new();
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
                    // Both peers agreed on this entity — corroboration evidence.
                    corroborated_peers.insert(*left_agent);
                    corroborated_peers.insert(*right_agent);
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

            // Conductance is the cold-start coupling log-LR prior; weight is its
            // bounded projection, never authored (conductance.md / ADR-0002).
            let (Ok(source_node), Ok(target_node)) = (
                self.graph.get_node(source).cloned(),
                self.graph.get_node(target).cloned(),
            ) else {
                continue;
            };
            let conductance =
                self.cold_start_conductance(&source_node, &target_node, &EdgeType::Entity);
            if !conductance.is_finite() {
                continue;
            }
            let edge_id = self.graph.next_edge_id();
            let edge = Edge::seeded(
                edge_id,
                source,
                target,
                EdgeType::Entity,
                conductance,
                crate::graph::edge::EdgeSource::Auto,
                now,
                now,
                HashMap::new(),
            );
            self.graph.add_edge(edge)?;
            self.emit_event(GraphEvent::EdgeCreated {
                edge_id,
                source,
                target,
                edge_type: EdgeType::Entity,
            });
            entity_edges_created += 1;
        }

        // Corroboration trust update: every peer corroborated by another agent on a
        // shared entity gets a full-strength positive trust nudge through the single
        // traceable evidence path (social.md "Peer Trust": corroboration raises
        // trust). Only registered peers move; origin and the coarse level are intact.
        for peer_id in corroborated_peers {
            if self.peers.get_peer(peer_id).is_some() {
                self.update_peer_trust_evidence(peer_id, 1.0)?;
            }
        }

        Ok(ReflectReport {
            entity_edges_created,
            clusters_found,
        })
    }

    /// Search for rejected hypotheses matching a query string.
    ///
    /// Returns `NodeId`s of `KnowledgeType::Hypothesis` nodes whose metadata contains
    /// `hypothesis_status = "rejected"` and whose name, content, or `rejection_reason`
    /// contains the query substring (case-insensitive). If `limit` is 0 an empty vector
    /// is returned immediately.
    pub fn search_rejected_hypotheses(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<NodeId>, Error> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let storage = self.graph.storage();
        let hypothesis_ids = storage.nodes_by_type(&KnowledgeType::Hypothesis);
        let query_lower = query.to_lowercase();

        let mut results = Vec::new();
        for nid in hypothesis_ids {
            let node = storage.get_node(nid)?;

            let is_rejected = node
                .metadata
                .get("hypothesis_status")
                .is_some_and(|status| status == "rejected");
            if !is_rejected {
                continue;
            }

            if query_lower.is_empty() {
                results.push(nid);
            } else {
                let name_match = node.name.to_lowercase().contains(&query_lower);
                let content_match = node.content.to_lowercase().contains(&query_lower);
                let reason_match = node
                    .metadata
                    .get("rejection_reason")
                    .is_some_and(|reason| reason.to_lowercase().contains(&query_lower));

                if name_match || content_match || reason_match {
                    results.push(nid);
                }
            }

            if results.len() >= limit {
                break;
            }
        }

        Ok(results)
    }

    /// Compute the nine-metric read-only [`GraphHealth`] for the graph
    /// (observability.md). Uses the current system time as the `stale_ratio`
    /// reference; for a deterministic reference use [`Engine::graph_health_at`].
    ///
    /// [`GraphHealth`]: crate::mechanics::health::GraphHealth
    pub fn graph_health(&self) -> crate::mechanics::health::GraphHealth {
        self.graph_health_at(Timestamp::now())
    }

    /// Compute the nine-metric [`GraphHealth`] as of the supplied `now` timestamp.
    ///
    /// `now` is the reference for the `stale_ratio` window, so passing a fixed
    /// timestamp makes the report fully deterministic (used by tests and the
    /// determinism invariant). Read-only — never mutates state.
    ///
    /// [`GraphHealth`]: crate::mechanics::health::GraphHealth
    pub fn graph_health_at(&self, now: Timestamp) -> crate::mechanics::health::GraphHealth {
        crate::mechanics::health::compute_health(self.graph.storage(), now)
    }

    /// Run the full [`InvariantCheck`] suite (observability.md) and return an
    /// [`InvariantReport`].
    ///
    /// Covers the eight structural invariants plus `snapshot_restore_consistency`
    /// (a clone must re-read identical field-for-field — every `Node`/`Edge`
    /// record plus the authoritative SoA reservoir/hot fields) and `determinism`
    /// (same graph + query twice yields an identical `ContextPackage`). The
    /// determinism probe re-runs
    /// `probe` — a representative query — against the live graph twice and against
    /// a clone, asserting byte-identical `ContextPackage`s. Pass `None` to skip
    /// the query-dependent determinism probe (the structural checks still run).
    ///
    /// Read-only: the clone is discarded; no graph state is mutated.
    ///
    /// [`InvariantCheck`]: crate::mechanics::observability::InvariantCheck
    /// [`InvariantReport`]: crate::mechanics::observability::InvariantReport
    pub fn check_invariants(
        &self,
        probe: Option<&SearchInput>,
    ) -> crate::mechanics::observability::InvariantReport {
        use crate::mechanics::observability::{
            InvariantCheck, InvariantResult, check_storage_invariants,
        };

        let mut results = check_storage_invariants(self.graph.storage());

        // snapshot_restore_consistency: a clone must reproduce identical per-node
        // and per-edge state. The clone is a deep copy of storage, so any field
        // the clone path drops (e.g. a reservoir column, content, or access
        // history) surfaces here as a field-level mismatch — not merely as a shift
        // in one of the nine aggregate health metrics. We compare the full `Node`
        // and `Edge` records (which carry content, summary, embedding, origin,
        // metadata, access_history, evidence_prior, tier, validity, and the
        // projections) together with the SoA reservoir/hot fields read through their
        // accessors (the cached composite `retained_action`, `conductance`,
        // `accessed_at`, the dormant `decay_checkpoint`), which the cached struct can
        // lag when marked dirty.
        let snapshot_result = {
            let cloned = self.graph.storage().clone();
            match snapshot_restore_mismatch(self.graph.storage(), &cloned) {
                None => InvariantResult::ok(InvariantCheck::SnapshotRestoreConsistency),
                Some(detail) => {
                    InvariantResult::failed(InvariantCheck::SnapshotRestoreConsistency, 1, detail)
                }
            }
        };
        results.push(snapshot_result);

        // determinism: same graph + query twice => identical ContextPackage.
        if let Some(input) = probe {
            let determinism_result = match (self.search(input.clone()), self.search(input.clone()))
            {
                (Ok(a), Ok(b)) => {
                    if a.package == b.package {
                        InvariantResult::ok(InvariantCheck::Determinism)
                    } else {
                        InvariantResult::failed(
                            InvariantCheck::Determinism,
                            1,
                            "two identical queries produced different ContextPackages",
                        )
                    }
                }
                _ => InvariantResult::failed(
                    InvariantCheck::Determinism,
                    1,
                    "determinism probe query errored",
                ),
            };
            results.push(determinism_result);
        }

        crate::mechanics::observability::InvariantReport { results }
    }

    /// Derive the [`OperationalWarning`]s implied by the current [`GraphHealth`]
    /// (observability.md). Read-only; uses the current system time as the
    /// `stale_ratio` reference. For a deterministic reference use
    /// [`Engine::operational_warnings_at`].
    ///
    /// [`OperationalWarning`]: crate::mechanics::observability::OperationalWarning
    /// [`GraphHealth`]: crate::mechanics::health::GraphHealth
    pub fn operational_warnings(&self) -> Vec<crate::mechanics::observability::OperationalWarning> {
        self.operational_warnings_at(Timestamp::now())
    }

    /// Derive the [`OperationalWarning`]s as of the supplied `now` timestamp.
    ///
    /// `now` is the reference for the staleness-derived warnings, so passing a
    /// fixed timestamp makes the result deterministic. Read-only.
    ///
    /// [`OperationalWarning`]: crate::mechanics::observability::OperationalWarning
    pub fn operational_warnings_at(
        &self,
        now: Timestamp,
    ) -> Vec<crate::mechanics::observability::OperationalWarning> {
        crate::mechanics::observability::derive_warnings(&self.graph_health_at(now))
    }

    /// Generate a support report for a node.
    ///
    /// Traverses only direct edges (1-hop) from the target node to count supporting
    /// and contradicting sources, measure evidence independence, and sum support salience.
    ///
    /// Supporting edges: `ConsolidatedFrom`, `ReinforcedBy`, `Supports`.
    /// Contradicting edges: `Contradicts`, `Refutes` (the debug-lifecycle
    /// counter-evidence edge created by `log_evidence`).
    ///
    /// Returns an error if the node does not exist.
    pub fn support_report(&self, node_id: NodeId) -> Result<SupportReport, Error> {
        let storage = self.graph.storage();
        let _node = storage.get_node(node_id)?; // Verify node exists

        let mut supporting_sources = 0;
        let mut contradicting_sources = 0;
        let mut origins = std::collections::HashSet::new();
        let mut total_support_salience = 0.0;
        let mut visited = std::collections::HashSet::new();

        // Traverse outgoing edges (node_id -> target)
        for &edge_id in storage.edges_from(node_id) {
            let edge = storage.get_edge(edge_id)?;
            let target_id = edge.target;

            // Skip if already visited (prevent circular evidence inflation)
            if visited.contains(&target_id) {
                continue;
            }
            visited.insert(target_id);

            match edge.edge_type {
                EdgeType::ConsolidatedFrom | EdgeType::ReinforcedBy | EdgeType::Supports => {
                    supporting_sources += 1;
                    let target_node = storage.get_node(target_id)?;
                    total_support_salience += target_node.salience;
                    origins.insert((
                        target_node.origin.peer_id,
                        target_node.origin.session_id.clone(),
                    ));
                }
                EdgeType::Contradicts | EdgeType::Refutes => {
                    contradicting_sources += 1;
                    let target_node = storage.get_node(target_id)?;
                    origins.insert((
                        target_node.origin.peer_id,
                        target_node.origin.session_id.clone(),
                    ));
                }
                _ => {}
            }
        }

        // Traverse incoming edges (source -> node_id)
        for &edge_id in storage.edges_to(node_id) {
            let edge = storage.get_edge(edge_id)?;
            let source_id = edge.source;

            // Skip if already visited (prevent circular evidence inflation)
            if visited.contains(&source_id) {
                continue;
            }
            visited.insert(source_id);

            match edge.edge_type {
                EdgeType::ConsolidatedFrom | EdgeType::ReinforcedBy | EdgeType::Supports => {
                    supporting_sources += 1;
                    let source_node = storage.get_node(source_id)?;
                    total_support_salience += source_node.salience;
                    origins.insert((
                        source_node.origin.peer_id,
                        source_node.origin.session_id.clone(),
                    ));
                }
                EdgeType::Contradicts | EdgeType::Refutes => {
                    contradicting_sources += 1;
                    let source_node = storage.get_node(source_id)?;
                    origins.insert((
                        source_node.origin.peer_id,
                        source_node.origin.session_id.clone(),
                    ));
                }
                _ => {}
            }
        }

        Ok(SupportReport {
            supporting_sources,
            contradicting_sources,
            independent_origins: origins.len(),
            total_support_salience,
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
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::peer::SourceKind::AgentObservation,
                session_id: "session-1".to_string(),
                scope: crate::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(1000),
            valid_from: None,
            valid_until: None,
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
    fn snapshot_restore_mismatch_clean_for_faithful_clone() {
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node A")).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(ids2) = engine.ingest(make_observation("node B")).unwrap() else {
            panic!("expected Created");
        };
        engine.link(ids[0], ids2[0], EdgeType::Semantic).unwrap();

        let clone = engine.graph().storage().clone();
        assert!(
            snapshot_restore_mismatch(engine.graph().storage(), &clone).is_none(),
            "a faithful clone must re-read identical field-for-field"
        );
    }

    #[test]
    fn snapshot_restore_mismatch_detects_dropped_reservoir_field() {
        // Simulate the regression the invariant guards against: a "restore" that
        // silently loses an authoritative reservoir/hot field even though every
        // aggregate health metric would be unaffected. Tampering the clone's SoA
        // salience must be caught field-for-field.
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node A")).unwrap() else {
            panic!("expected Created");
        };

        let mut clone = engine.graph().storage().clone();
        let original_salience = clone.get_salience(ids[0]).unwrap();
        clone.set_salience(ids[0], original_salience * 0.5).unwrap();

        let mismatch = snapshot_restore_mismatch(engine.graph().storage(), &clone);
        assert!(
            mismatch.is_some(),
            "a clone that lost a hot field must be reported as a mismatch"
        );
    }

    #[test]
    fn snapshot_restore_mismatch_detects_dropped_decay_checkpoint() {
        // decay_checkpoint is a SoA-only field absent from the `Node` record and
        // from every aggregate health metric — exactly the silent-drop class the
        // finding calls out.
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node A")).unwrap() else {
            panic!("expected Created");
        };

        let mut clone = engine.graph().storage().clone();
        let checkpoint = clone.get_decay_checkpoint(ids[0]).unwrap();
        clone
            .set_decay_checkpoint(ids[0], Timestamp(checkpoint.0 + 1))
            .unwrap();

        assert!(
            snapshot_restore_mismatch(engine.graph().storage(), &clone).is_some(),
            "a clone that lost decay_checkpoint must be reported as a mismatch"
        );
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
        // salience = logistic(B_i + P_i) (ADR-0008), never a flat 1.0. With no
        // embedding the observation is maximally surprising, so its evidence prior
        // P_i enters near the ceiling (k·eps fallback = INITIAL_RETAINED_ACTION) and
        // salience ≈ logistic(B_creation ≈ 0 + P_i) ≈ 1.
        assert_eq!(
            node.salience,
            crate::mechanics::priors::project_salience(node.retained_action)
        );
        assert!(node.salience > 0.999 && node.salience < 1.0);
        // Ingest seeds exactly the creation trace so B_i is finite at birth.
        assert_eq!(node.access_history.len(), 1, "creation trace seeded");
        assert_eq!(
            node.evidence_prior,
            crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            "encoding-surprise prior P_i ← k·eps (max-surprise fallback)"
        );
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
        let eid = engine.link(ids1[0], ids2[0], EdgeType::Semantic).unwrap();
        assert_eq!(engine.graph().edge_count(), 1);
        let edge = engine.graph().get_edge(eid).unwrap();
        // Conductance is seeded from the cold-start coupling; weight is its bounded
        // projection (ADR-0002), not a caller-supplied value.
        assert!(edge.conductance.is_finite());
        assert!((0.0..=1.0).contains(&edge.weight));
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
        assert_eq!(report.edges_leaked, 0);
    }

    // ── Idle-edge leakage (TimeElapsed on conductance) ────────────────────────

    /// Build an edge with a fixed conductance reservoir at a known `accessed_at`,
    /// so leakage tests do not depend on cold-start coupling magnitudes.
    fn seed_edge(
        engine: &mut Engine,
        from: NodeId,
        to: NodeId,
        edge_type: EdgeType,
        conductance: f64,
        accessed_at: Timestamp,
    ) -> EdgeId {
        let eid = engine.graph_mut().next_edge_id();
        let edge = Edge::seeded(
            eid,
            from,
            to,
            edge_type,
            conductance,
            crate::graph::edge::EdgeSource::Manual,
            Timestamp(1000),
            accessed_at,
            HashMap::new(),
        );
        engine.graph_mut().add_edge(edge).unwrap();
        eid
    }

    #[test]
    fn tick_leaks_idle_edge() {
        let mut engine = Engine::new();
        let IngestResult::Created(a) = engine.ingest(make_observation("A")).unwrap() else {
            panic!()
        };
        let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
            panic!()
        };
        // Edge last used at t=1000; conductance well above zero so it can leak.
        let eid = seed_edge(
            &mut engine,
            a[0],
            b[0],
            EdgeType::Semantic,
            2.0,
            Timestamp(1000),
        );
        let before = engine.graph().get_edge(eid).unwrap().conductance;

        // Tick 60 days later → the edge is idle and must lose conductance.
        let later = Timestamp(1000 + 60 * 86_400_000);
        let report = engine.tick(later).unwrap();

        let after = engine.graph().get_edge(eid).unwrap().conductance;
        assert!(after < before, "idle edge must leak: {after} !< {before}");
        assert!(after.is_finite());
        assert_eq!(report.edges_leaked, 1);
        assert!(report.total_conductance_delta > 0.0);
        // Weight projection re-derived from the leaked reservoir (ADR-0002).
        let edge = engine.graph().get_edge(eid).unwrap();
        assert!((edge.weight - crate::mechanics::priors::project_weight(after)).abs() < 1e-12);
    }

    #[test]
    fn tick_does_not_leak_recently_used_edge() {
        let mut engine = Engine::new();
        let IngestResult::Created(a) = engine.ingest(make_observation("A")).unwrap() else {
            panic!()
        };
        let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
            panic!()
        };
        // Edge accessed AT the tick time → zero idle days → no leak.
        let tick_time = Timestamp(1000 + 60 * 86_400_000);
        let eid = seed_edge(&mut engine, a[0], b[0], EdgeType::Semantic, 2.0, tick_time);
        let before = engine.graph().get_edge(eid).unwrap().conductance;

        let report = engine.tick(tick_time).unwrap();

        let after = engine.graph().get_edge(eid).unwrap().conductance;
        assert_eq!(after, before, "recently-used edge must not leak");
        assert_eq!(report.edges_leaked, 0);
    }

    #[test]
    fn tick_does_not_leak_contradicts_edge() {
        let mut engine = Engine::new();
        let IngestResult::Created(a) = engine.ingest(make_observation("A")).unwrap() else {
            panic!()
        };
        let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
            panic!()
        };
        let eid = seed_edge(
            &mut engine,
            a[0],
            b[0],
            EdgeType::Contradicts,
            2.0,
            Timestamp(1000),
        );
        let before = engine.graph().get_edge(eid).unwrap().conductance;

        let later = Timestamp(1000 + 365 * 86_400_000);
        let report = engine.tick(later).unwrap();

        let after = engine.graph().get_edge(eid).unwrap().conductance;
        assert_eq!(after, before, "Contradicts is excluded from leakage");
        assert_eq!(report.edges_leaked, 0);
    }

    #[test]
    fn tick_does_not_leak_edge_incident_to_protected_node() {
        let mut engine = Engine::new();
        // Identity-core endpoint is protected from ordinary dissipation.
        let mut core_obs = make_observation("identity");
        core_obs.node_type = KnowledgeType::IdentityCore;
        let IngestResult::Created(core) = engine.ingest(core_obs).unwrap() else {
            panic!()
        };
        let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
            panic!()
        };
        let eid = seed_edge(
            &mut engine,
            core[0],
            b[0],
            EdgeType::Semantic,
            2.0,
            Timestamp(1000),
        );
        let before = engine.graph().get_edge(eid).unwrap().conductance;

        let later = Timestamp(1000 + 365 * 86_400_000);
        engine.tick(later).unwrap();

        let after = engine.graph().get_edge(eid).unwrap().conductance;
        assert_eq!(after, before, "edge to protected node must not leak");
    }

    #[test]
    fn tick_edge_leakage_is_deterministic() {
        // Same graph + same tick time => identical leaked conductance.
        let build = || {
            let mut engine = Engine::new();
            let IngestResult::Created(a) = engine.ingest(make_observation("A")).unwrap() else {
                panic!()
            };
            let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
                panic!()
            };
            let eid = seed_edge(
                &mut engine,
                a[0],
                b[0],
                EdgeType::Semantic,
                1.5,
                Timestamp(1000),
            );
            let later = Timestamp(1000 + 90 * 86_400_000);
            engine.tick(later).unwrap();
            engine.graph().get_edge(eid).unwrap().conductance
        };
        assert_eq!(build(), build());
    }

    // ── Cold-start conductance_threshold density gate ─────────────────────────

    #[test]
    fn cold_start_subthreshold_coupling_creates_no_edge() {
        use crate::mechanics::priors::{CONDUCTANCE_THRESHOLD, coupling_clears_threshold};
        // Two nodes with NO embedding and DISJOINT entity tags → the cold-start
        // coupling seed is far below `conductance_threshold`, so the auto-link path
        // must create no edge (conductance.md "Cold Start" density gate).
        let mut engine = Engine::new();
        let mut o1 = make_observation("alpha");
        o1.embedding = Some(vec![1.0, 0.0, 0.0]);
        o1.entity_tags = vec!["alpha-only".to_string()];
        let mut o2 = make_observation("omega");
        // Orthogonal embedding (cosine 0) and disjoint tags → near-zero seed.
        o2.embedding = Some(vec![0.0, 1.0, 0.0]);
        o2.entity_tags = vec!["omega-only".to_string()];

        engine.ingest(o1).unwrap();
        engine.ingest(o2).unwrap();

        // The sub-threshold seed gate held: no auto-edge was created.
        assert_eq!(
            engine.graph().edge_count(),
            0,
            "sub-threshold coupling must not create an edge"
        );
        // And the gate predicate agrees on a clearly sub-threshold seed.
        assert!(!coupling_clears_threshold(0.0));
        assert!(!coupling_clears_threshold(CONDUCTANCE_THRESHOLD - 1e-6));
        assert!(coupling_clears_threshold(CONDUCTANCE_THRESHOLD));
    }

    #[test]
    fn cold_start_suprathreshold_coupling_creates_edge() {
        use crate::mechanics::priors::{
            coupling_clears_threshold, initialize_conductance, project_weight,
        };
        // The auto-link must survive BOTH the attraction candidate gate and the
        // cold-start `conductance_threshold` density gate. An identity↔knowledge
        // pair uses the lower attraction threshold (0.65, tau 1.25) which sits below
        // the novelty routing boundary (theta_sep ≈ 0.30 ⇒ allocate when sim < 0.70),
        // so two distinct sites are created and a supra-threshold coupling seed links
        // them. Embedding cosine ≈ 0.67: novelty 0.33 > 0.30 (allocate) and
        // attraction 0.67 * 1.25 ≈ 0.84 ≥ 0.65, and the coupling seed clears 0.05.
        let mut engine = Engine::new();
        let cos = 0.67_f64;
        let mut o1 = make_observation("identity");
        o1.node_type = KnowledgeType::IdentityLearned;
        o1.embedding = Some(vec![1.0, 0.0]);
        o1.entity_tags = vec!["shared".to_string()];
        let mut o2 = make_observation("knowledge");
        o2.node_type = KnowledgeType::Semantic;
        o2.embedding = Some(vec![cos, (1.0 - cos * cos).sqrt()]);
        o2.entity_tags = vec!["shared".to_string()];

        let IngestResult::Created(ids1) = engine.ingest(o1).unwrap() else {
            panic!("first observation should allocate a new site");
        };
        let IngestResult::Created(ids2) = engine.ingest(o2).unwrap() else {
            panic!("second observation should allocate a distinct site (novelty > theta_sep)");
        };
        let (id1, id2) = (ids1[0], ids2[0]);

        assert_eq!(
            engine.graph().edge_count(),
            1,
            "supra-threshold coupling must create the auto-edge"
        );

        // DoD trace: the created edge's conductance is the cold-start coupling seed
        // mapped through `initialize_conductance`, and its public `weight` is the
        // bounded projection of that reservoir — `weight = project_weight(
        // initialize_conductance(coupling_seed))` (conductance.md "Cold Start",
        // ADR-0002). The seed is recomputed via the same engine helper the gate
        // tests, over the new site (id2, ingested second) toward its candidate (id1).
        let new_node = engine.graph().get_node(id2).unwrap().clone();
        let cand_node = engine.graph().get_node(id1).unwrap().clone();
        let seed = engine.cold_start_coupling_seed(&new_node, &cand_node, &EdgeType::Semantic);
        assert!(
            coupling_clears_threshold(seed),
            "fixture seed must clear the conductance_threshold gate"
        );
        let expected_conductance = initialize_conductance(seed);
        let expected_weight = project_weight(expected_conductance);

        let edge_id = engine.graph().edges_from(id2)[0];
        let edge = engine.graph().get_edge(edge_id).unwrap();
        assert_eq!(edge.source, id2);
        assert_eq!(edge.target, id1);
        assert_eq!(edge.edge_type, EdgeType::Semantic);
        assert!(
            (edge.conductance - expected_conductance).abs() < 1e-12,
            "edge conductance must be initialize_conductance(coupling_seed)"
        );
        assert!(
            (edge.weight - expected_weight).abs() < 1e-12,
            "edge weight must be project_weight(initialize_conductance(coupling_seed))"
        );
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
        let eid = engine.link(ids1[0], ids2[0], EdgeType::Semantic).unwrap();
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
    fn touch_appends_trace_and_keeps_salience_bounded() {
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node")).unwrap() else {
            panic!("expected Created");
        };
        let id = ids[0];

        let future = Timestamp(1000 + 30 * 86_400_000);
        engine.touch(id, future).unwrap();

        // A committed access appends a now-stamped trace (raising B_i); salience is
        // the bounded logistic projection of B_i(now) + P_i, never exactly 1.0.
        let node = engine.graph().get_node(id).unwrap();
        assert!(
            node.salience < 1.0,
            "salience projection is strictly bounded: {}",
            node.salience
        );
        assert!(
            node.salience > 0.0,
            "salience should not be zero: {}",
            node.salience
        );
        assert_eq!(node.access_count, 1);
        // The creation trace plus the touch trace are both present.
        assert_eq!(node.access_history.len(), 2);
        assert_eq!(node.access_history.back().unwrap().at, future);
    }

    #[test]
    fn touch_immediate_appends_trace_and_does_not_lower_strength() {
        let mut engine = Engine::new();
        let IngestResult::Created(ids) = engine.ingest(make_observation("node")).unwrap() else {
            panic!("expected Created");
        };
        let id = ids[0];

        let a_before = engine.graph().get_node(id).unwrap().retained_action;

        let now = Timestamp(1000);
        engine.touch(id, now).unwrap();

        // Immediate touch (same now): the appended trace ages prior traces to `now`
        // inside the same B_i sum, so an extra coincident trace can only raise B_i
        // (decay-first is intrinsic, ADR-0008). A_i is the recomputed composite cache.
        let node = engine.graph().get_node(id).unwrap();
        assert!(
            node.retained_action >= a_before,
            "access should not lower A: {} < {a_before}",
            node.retained_action
        );
        assert!(
            node.salience > 0.999 && node.salience <= 1.0,
            "salience={}",
            node.salience
        );
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

        // Capture the surprise-gated initial salience (project_salience(A), not 1.0).
        let initial_salience = engine.graph().storage().get_salience(id).unwrap();

        let future = Timestamp(365 * 86_400_000);
        engine.tick(future).unwrap();

        let salience = engine.graph().storage().get_salience(id).unwrap();
        assert_eq!(
            salience, initial_salience,
            "IdentityCore should not decay (salience unchanged after a year)"
        );
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
        // Budget rejects only when FULL *and* the input is NOT novel (ADR-0009):
        // a novel observation may still enter past a full budget. Make the third
        // observation a near-duplicate of an existing site (low novelty) but tighten
        // the dedup threshold so it does not route-reinforce, and disable conflict
        // surfacing — so it reaches the budget guard as a non-novel input.
        let config = EngineConfig::new()
            .with_max_nodes(2)
            .with_dedup_enabled(false);
        let mut engine = Engine::with_config(config);

        let obs1 = Observation {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            ..make_observation("node 1")
        };
        let _ = engine.ingest(obs1).unwrap();
        let obs2 = Observation {
            embedding: Some(vec![0.0, 1.0, 0.0]),
            ..make_observation("node 2")
        };
        let _ = engine.ingest(obs2).unwrap();

        // Near-identical to node 1 → novelty ≈ 0 ≤ theta_sep → not novel; budget full.
        let obs3 = Observation {
            embedding: Some(vec![1.0, 0.0001, 0.0]),
            ..make_observation("node 3")
        };
        let result = engine.ingest(obs3);
        assert!(matches!(result, Err(Error::Rejected(_))), "got {result:?}");
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

        engine.link(ids1[0], ids2[0], EdgeType::Semantic).unwrap();

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
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::peer::SourceKind::AgentObservation,
                session_id: "session-1".to_string(),
                scope: crate::graph::ScopePath::universal(),
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
            .link(identity_ids[0], semantic_ids[0], EdgeType::Semantic)
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
            .link(ids1[0], ids2[0], EdgeType::Contradicts)
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

    // ── PathUsed / CoReadout conductance learning (conductance.md) ────────────

    /// Build a two-node graph linked by one edge; return (engine, edge_id).
    fn engine_with_edge() -> (Engine, EdgeId) {
        let mut engine = Engine::new();
        let IngestResult::Created(a) = engine.ingest(make_observation("A")).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
            panic!("expected Created");
        };
        // link() seeds conductance at 0.0; weight at the manual 0.5 projection input.
        let eid = engine.link(a[0], b[0], EdgeType::Semantic).unwrap();
        // Reset the reservoir to a clean 0.0 cold-start so the Hebbian step is
        // measured from the documented C_ij = 0 baseline (weight 0.5).
        engine
            .graph
            .storage_mut()
            .set_conductance(eid, 0.0)
            .unwrap();
        (engine, eid)
    }

    #[test]
    fn path_used_raises_conductance_and_weight() {
        let (mut engine, eid) = engine_with_edge();
        let w_before = engine.graph().get_edge(eid).unwrap().weight;
        let c_before = engine.graph().storage().get_conductance(eid).unwrap();
        assert_eq!(c_before, 0.0);

        engine.apply_path_used(vec![eid], vec![1.0]).unwrap();

        let c_after = engine.graph().storage().get_conductance(eid).unwrap();
        let w_after = engine.graph().get_edge(eid).unwrap().weight;
        assert!(c_after > c_before, "C must rise: {c_after} !> {c_before}");
        assert!(
            w_after > w_before,
            "weight projection must rise: {w_after} !> {w_before}"
        );
        // weight is the logistic projection of C — they must stay consistent.
        assert!((w_after - crate::mechanics::priors::project_weight(c_after)).abs() < 1e-12);
    }

    #[test]
    fn path_used_zero_flux_is_identity() {
        let (mut engine, eid) = engine_with_edge();
        engine.apply_path_used(vec![eid], vec![0.0]).unwrap();
        assert_eq!(engine.graph().storage().get_conductance(eid).unwrap(), 0.0);
    }

    #[test]
    fn path_used_saturates_without_runaway() {
        let (mut engine, eid) = engine_with_edge();
        for _ in 0..10_000 {
            engine.apply_path_used(vec![eid], vec![1.0]).unwrap();
        }
        let c = engine.graph().storage().get_conductance(eid).unwrap();
        assert!(c.is_finite(), "conductance must not run away: {c}");
        let w = engine.graph().get_edge(eid).unwrap().weight;
        assert!(w < 1.0, "Oja-bounded weight must stay below 1: {w}");
    }

    #[test]
    fn path_used_length_mismatch_errors() {
        let (mut engine, eid) = engine_with_edge();
        let err = engine.apply_path_used(vec![eid], vec![0.5, 0.5]);
        assert!(matches!(err, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn path_used_non_finite_flux_errors() {
        let (mut engine, eid) = engine_with_edge();
        let err = engine.apply_path_used(vec![eid], vec![f64::NAN]);
        assert!(matches!(err, Err(Error::NonFinite(_))));
    }

    #[test]
    fn path_used_missing_edge_errors() {
        let mut engine = Engine::new();
        let err = engine.apply_path_used(vec![EdgeId(999)], vec![1.0]);
        assert!(err.is_err(), "missing edge must error");
    }

    #[test]
    fn co_readout_strengthens_connecting_edge() {
        let (mut engine, eid) = engine_with_edge();
        let edge = engine.graph().get_edge(eid).unwrap();
        let (a, b) = (edge.source, edge.target);
        let c_before = engine.graph().storage().get_conductance(eid).unwrap();

        engine
            .apply_co_readout(vec![(a, b)], vec![(0.8, 0.9)])
            .unwrap();

        let c_after = engine.graph().storage().get_conductance(eid).unwrap();
        assert!(c_after > c_before, "co-readout must raise C: {c_after}");
    }

    #[test]
    fn co_readout_uses_weaker_activation() {
        // co_flux = min(a_i, a_j): the same min drives identical edges identically.
        let (mut engine_lo, eid_lo) = engine_with_edge();
        let (mut engine_hi, eid_hi) = engine_with_edge();
        let (a_lo, b_lo) = {
            let e = engine_lo.graph().get_edge(eid_lo).unwrap();
            (e.source, e.target)
        };
        let (a_hi, b_hi) = {
            let e = engine_hi.graph().get_edge(eid_hi).unwrap();
            (e.source, e.target)
        };

        // Same minimum (0.3) despite different maxima => identical conductance move.
        engine_lo
            .apply_co_readout(vec![(a_lo, b_lo)], vec![(0.3, 0.9)])
            .unwrap();
        engine_hi
            .apply_co_readout(vec![(a_hi, b_hi)], vec![(0.3, 0.4)])
            .unwrap();

        let c_lo = engine_lo.graph().storage().get_conductance(eid_lo).unwrap();
        let c_hi = engine_hi.graph().storage().get_conductance(eid_hi).unwrap();
        assert!(
            (c_lo - c_hi).abs() < 1e-12,
            "min-flux must drive both: {c_lo} vs {c_hi}"
        );
    }

    #[test]
    fn co_readout_skips_unconnected_pairs() {
        let mut engine = Engine::new();
        let IngestResult::Created(a) = engine.ingest(make_observation("A")).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(b) = engine.ingest(make_observation("B")).unwrap() else {
            panic!("expected Created");
        };
        // No edge between A and B: co-readout strengthens existing paths only.
        let edges_before = engine.graph().edge_count();
        engine
            .apply_co_readout(vec![(a[0], b[0])], vec![(0.9, 0.9)])
            .unwrap();
        assert_eq!(engine.graph().edge_count(), edges_before, "no edge created");
    }

    #[test]
    fn co_readout_length_mismatch_errors() {
        let (mut engine, eid) = engine_with_edge();
        let (a, b) = {
            let e = engine.graph().get_edge(eid).unwrap();
            (e.source, e.target)
        };
        let err = engine.apply_co_readout(vec![(a, b)], vec![(0.5, 0.5), (0.5, 0.5)]);
        assert!(matches!(err, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn co_readout_non_finite_activation_errors() {
        let (mut engine, eid) = engine_with_edge();
        let (a, b) = {
            let e = engine.graph().get_edge(eid).unwrap();
            (e.source, e.target)
        };
        let err = engine.apply_co_readout(vec![(a, b)], vec![(f64::INFINITY, 0.5)]);
        assert!(matches!(err, Err(Error::NonFinite(_))));
    }

    #[test]
    fn path_used_is_deterministic() {
        // Same graph + same committed trace => identical conductance.
        let (mut e1, eid1) = engine_with_edge();
        let (mut e2, eid2) = engine_with_edge();
        for _ in 0..5 {
            e1.apply_path_used(vec![eid1], vec![0.7]).unwrap();
            e2.apply_path_used(vec![eid2], vec![0.7]).unwrap();
        }
        let c1 = e1.graph().storage().get_conductance(eid1).unwrap();
        let c2 = e2.graph().storage().get_conductance(eid2).unwrap();
        assert_eq!(c1, c2, "deterministic: {c1} vs {c2}");
    }
}
