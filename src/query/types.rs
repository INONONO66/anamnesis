//! Query types for the Anamnesis cognitive graph engine.

use crate::graph::Origin;
use crate::graph::scope::{ScopePath, ScopeRelation};
use crate::graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Timestamp};

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

/// Configuration for top-k convergence termination in spreading activation.
///
/// When enabled, spreading activation stops early if the top-k node rankings
/// stabilize for N consecutive rounds.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvergenceConfig {
    /// Number of consecutive rounds where top-k must remain stable to trigger convergence.
    pub stable_rounds: usize,
    /// Number of top nodes to compare for stability.
    pub compare_top_k: usize,
    /// Minimum change in activation score to consider a node "different" (prevents noise).
    pub min_delta: f64,
}

impl Default for ConvergenceConfig {
    fn default() -> Self {
        ConvergenceConfig {
            stable_rounds: 3,
            compare_top_k: 10,
            min_delta: 0.01,
        }
    }
}

/// Configuration for a query execution.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
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
    /// Embedding vector for the query. Used for vector similarity in initial activation (eq 10).
    pub query_embedding: Option<Vec<f64>>,
    /// Scope context for scope weighting (eq 13). `ScopePath::universal()` = universal query.
    pub scope: ScopePath,
    /// Characters per token for budget estimation. Default: 4.
    pub chars_per_token: usize,
    /// Goal context for reranking results. None = no goal weighting.
    pub context: Option<String>,
    /// Domain timestamp for edge validity filtering. None = current system time.
    pub now: Option<Timestamp>,
    /// Optional convergence termination config. None = no early stopping.
    pub convergence: Option<ConvergenceConfig>,
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
            query_embedding: None,
            scope: ScopePath::universal(),
            chars_per_token: 4,
            context: None,
            now: None,
            convergence: None,
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
    /// Scope of this fragment relative to the query context.
    pub scope: ScopeRelation,
}

/// An active contradiction between two nodes, surfaced as query-local stress.
///
/// Surfaced when a `Contradicts` edge connects two *active* nodes whose scopes and
/// fact-times overlap. Per [frustration.md] / [ADR-0006] the conflict is **surfaced,
/// never suppressed**: neither endpoint's activation is reduced and nothing is
/// deleted. The `stress` field carries the multiplicative gate product
/// `sigma_ij = contradiction_weight * min(a_i, a_j) * scope_overlap * temporal_overlap`,
/// which feeds the `-w_stress` readout term and encourages conflicting bundles to
/// separate without judging either side true.
///
/// [frustration.md]: ../../docs/04-cognitive-dynamics/frustration.md
/// [ADR-0006]: ../../docs/adr/0006-frustration-not-deletion.md
#[derive(Debug, Clone, PartialEq)]
pub struct Tension {
    /// First node in the contradiction (`primary`).
    pub node_a: NodeId,
    /// Second node in the contradiction (`conflicting`).
    pub node_b: NodeId,
    /// Weight of the `Contradicts` edge [0, 1] (the `contradiction_weight` gate).
    pub edge_weight: f64,
    /// Query-local stress `sigma_ij` (the multiplicative gate product).
    pub stress: f64,
    /// Scope-overlap gate contribution `[0, 1]`.
    pub scope_overlap: f64,
    /// Temporal-overlap gate contribution `[0, 1]`.
    pub temporal_overlap: f64,
    /// Evidence sources for the contradiction — the two endpoints whose activation
    /// raised this tension (`[primary, conflicting]`), for caller adjudication.
    pub evidence_sources: Vec<NodeId>,
    /// Optional human-readable explanation of the contradiction.
    pub description: Option<String>,
}

/// A site that was read out, recorded for an `Accessed` commit interaction.
///
/// Captured during read-only retrieval (the site appeared in the packaged result)
/// so that a later [`Engine::commit`](crate::api::Engine::commit) can apply
/// decay-then-`access_gain` on its retained-action reservoir (interactions.md).
/// `readout_work` is the bounded `[0, 1]` work the site delivered to the answer —
/// the settled query-local activation `a_i`, used as the readout-work proxy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccessedSite {
    /// The site read out.
    pub node_id: NodeId,
    /// Bounded `[0, 1]` readout work delivered (the settled activation `a_i`).
    pub readout_work: f64,
}

/// A pair of sites read out together, recorded for a `CoReadout` commit interaction.
///
/// The co-readout flux at commit time is `min(a_i, a_j)` (conductance.md); the
/// activations are captured here so commit can reconstruct that flux deterministically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoReadoutPair {
    /// First site.
    pub node_a: NodeId,
    /// Second site.
    pub node_b: NodeId,
    /// Settled query-local activation of `node_a`.
    pub activation_a: f64,
    /// Settled query-local activation of `node_b`.
    pub activation_b: f64,
}

/// An edge that carried committed path current `I_ij`, recorded for a `PathUsed`
/// commit interaction (interactions.md / conductance.md).
///
/// The edge's topology snapshot (`source`/`target`/`edge_type`) is captured so
/// [`Engine::commit`](crate::api::Engine::commit) can verify the trace still matches
/// the graph (a moved/retyped/deleted edge makes the trace stale — a hard error).
#[derive(Debug, Clone, PartialEq)]
pub struct PathUsedEdge {
    /// The edge that carried current.
    pub edge_id: EdgeId,
    /// Recorded source endpoint at retrieval time (topology snapshot).
    pub source: NodeId,
    /// Recorded target endpoint at retrieval time (topology snapshot).
    pub target: NodeId,
    /// Recorded edge type at retrieval time (topology snapshot).
    pub edge_type: EdgeType,
    /// Path current `I_ij = a_i * g_ij` at the settled response (the Hebbian flux).
    pub flux: f64,
}

/// A presented contradiction, recorded for a `TensionActivated` commit interaction
/// (frustration.md, ADR-0006).
///
/// Records that the conflict was surfaced to the caller; commit logs the tension
/// (`S_frustration = tension_presented_ij * sigma_ij`). It never reduces either
/// endpoint's activation and never picks a winner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActivatedTension {
    /// `primary` endpoint.
    pub node_a: NodeId,
    /// `conflicting` endpoint.
    pub node_b: NodeId,
    /// Query-local stress `sigma_ij` that was presented.
    pub stress: f64,
}

/// The read-only retrieval trace required to commit a [`ContextPackage`].
///
/// Per [ADR-0004](../../docs/adr/0004-query-as-field-and-commit.md) /
/// [interactions.md](../../docs/04-cognitive-dynamics/interactions.md), retrieval is
/// read-only and returns this trace alongside the package; an explicit
/// [`Engine::commit`](crate::api::Engine::commit) consumes it and integrates the
/// committed work into the reservoirs. Commit MUST validate that the trace still
/// matches the graph state it updates: every referenced node/edge must exist and the
/// `path_used` topology snapshot must still match (a stale/mismatched trace is a hard
/// error). The trace is transient and carries no persistent quantity itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitTrace {
    /// Sites read out into the package (`Accessed` candidates).
    pub accessed: Vec<AccessedSite>,
    /// Site pairs read out together (`CoReadout` candidates).
    pub co_readout: Vec<CoReadoutPair>,
    /// Edges that carried committed path current (`PathUsed` candidates).
    pub path_used: Vec<PathUsedEdge>,
    /// Contradictions presented to the caller (`TensionActivated` candidates).
    pub tensions_activated: Vec<ActivatedTension>,
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
    /// Read-only retrieval trace required to commit this package (ADR-0004).
    ///
    /// Captured during retrieval; consumed by
    /// [`Engine::commit`](crate::api::Engine::commit), which validates it against the
    /// current graph before integrating the committed work into the reservoirs. It
    /// carries no persistent quantity — retrieval remains read-only.
    pub commit_trace: CommitTrace,
    /// Node ids whose reservoirs were mutated by a successful commit of this package.
    ///
    /// Empty for a freshly returned (uncommitted) package; populated by
    /// [`Engine::commit`](crate::api::Engine::commit) with the sites it actually
    /// updated (`Accessed` + feedback targets), so the caller can attribute every
    /// persistent delta to committed use.
    pub committed_ids: Vec<NodeId>,
}

impl ContextPackage {
    /// Create an empty ContextPackage with no fragments or tensions.
    ///
    /// Used by non-Associative query modes (TypeFiltered, Neighborhood, Temporal, List)
    /// which currently return empty results, and by callers with no assembled fragments.
    pub fn empty() -> Self {
        ContextPackage {
            identity: vec![],
            knowledge: vec![],
            memories: vec![],
            tensions: vec![],
            token_usage: TokenBudget::default(),
            agent_tension: 0.0,
            commit_trace: CommitTrace::default(),
            committed_ids: vec![],
        }
    }

    /// Total number of fragments across all categories.
    pub fn total_fragments(&self) -> usize {
        self.identity.len() + self.knowledge.len() + self.memories.len()
    }
}

/// Unified search input for the cognitive graph engine.
#[derive(Debug, Clone)]
pub struct SearchInput {
    /// Natural language query text.
    pub text: String,
    /// Agent ID for identity-biased retrieval. None = no identity bias.
    pub agent_id: Option<String>,
    /// Peer filter — restrict results to nodes produced by these peers.
    ///
    /// `None` = include all peers (default). Combined with `scope` as AND.
    pub peer_filter: Option<Vec<crate::graph::types::PeerId>>,
    /// Scope filter. `ScopePath::universal()` = universal.
    pub scope: ScopePath,
    /// Current timestamp for temporal filtering.
    pub now: Timestamp,
    /// Optional query embedding for vector similarity.
    pub query_embedding: Option<Vec<f64>>,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Optional goal context for reranking.
    pub context: Option<String>,
    /// Optional entity tags to seed retrieval from. Empty = no entity-tag retrieval.
    pub entity_tags: Vec<String>,
    /// Optional override for the number of seeds to expand with graph recall.
    /// `None` falls back to the engine default (3).
    pub seed_limit: Option<usize>,
}

impl Default for SearchInput {
    fn default() -> Self {
        SearchInput {
            text: String::new(),
            agent_id: None,
            peer_filter: None,
            scope: ScopePath::universal(),
            now: Timestamp(0),
            query_embedding: None,
            limit: 10,
            context: None,
            entity_tags: Vec::new(),
            seed_limit: None,
        }
    }
}

/// Packaging mode for ContextPackage assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackagingMode {
    /// Knowledge fragments only (default).
    KnowledgeOnly,
    /// Knowledge + provenance (source episodes).
    KnowledgeWithProvenance,
    /// Persona-weighted (identity nodes boosted).
    PersonaWeighted,
    /// Timeline-ordered (temporal emphasis).
    Timeline,
}

/// Trace of strategies used during a search operation.
///
/// Carries the additive-RWR activation-flow diagnostics (iterations, residual,
/// truncation, excluded `Contradicts` edges, and path-current count) for the
/// authoritative read-only retrieval path.
#[derive(Debug, Clone, Default)]
pub struct SearchTrace {
    /// Names of retrieval strategies used (e.g., "text_search", "activation_flow").
    pub strategies_used: Vec<String>,
    /// Number of seed nodes found.
    pub seed_count: usize,
    /// Number of RWR iterations performed before convergence (or the bound).
    pub iterations: usize,
    /// Final residual `||a_next - a||_1` of the activation flow.
    pub residual: f64,
    /// Whether the iteration bound stopped convergence.
    pub truncated: bool,
    /// Number of edges split off to frustration (excluded `Contradicts`).
    pub excluded_edge_count: usize,
    /// Number of edges carrying captured path current `I_ij`.
    pub path_current_count: usize,
    /// Packaging mode selected.
    pub packaging_mode: Option<PackagingMode>,
    /// Query-local readout energy `E(S | Q)` decomposed over the packaged active
    /// subsystem (energy.md / ADR-0007). This is an *interpretive* objective that
    /// explains why the bundle was selected; it is query-local and never stored, and
    /// the RWR stationary vector (captured by `residual`/`iterations` above) remains
    /// the true fixed point. Default (`E = 0`) for an empty result.
    pub energy: crate::mechanics::energy::EnergyTerms,
}

/// Internal search plan — auto-derived from SearchInput.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SearchPlan {
    /// Trimmed query text (raw `SearchInput.text` with leading/trailing whitespace removed).
    pub text: String,
    /// Use text search for seed retrieval.
    pub use_text: bool,
    /// Use vector similarity for seed retrieval.
    pub use_vector: bool,
    /// Use entity-tag retrieval for seed nodes.
    pub use_entity: bool,
    /// Use graph spreading activation.
    pub use_graph: bool,
    /// Apply persona/identity bias.
    pub use_persona_bias: bool,
    /// Resolved number of seeds to expand with graph recall.
    pub seed_limit: usize,
    /// Packaging mode for result assembly.
    pub packaging_mode: PackagingMode,
}

/// Result of a unified search operation.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Structured context package ready for LLM injection.
    pub package: ContextPackage,
    /// Trace of strategies used.
    pub trace: SearchTrace,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Origin;
    use crate::graph::{KnowledgeType, NodeId, Timestamp};

    fn make_origin() -> Origin {
        Origin {
            peer_id: crate::graph::types::PeerId(0),
            source_kind: crate::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        }
    }

    #[test]
    fn all_query_variants_constructable() {
        let queries = [
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
    fn scope_variants() {
        let s1 = ScopeRelation::Exact;
        let s2 = ScopeRelation::Universal;
        let s3 = ScopeRelation::Unrelated;
        assert_ne!(s1, s2);
        assert_ne!(s2, s3);
    }

    #[test]
    fn query_config_default() {
        let config = QueryConfig::default();
        assert_eq!(config.budget, 500);
        assert_eq!(config.decay_per_hop, 0.65);
        assert_eq!(config.min_activation, 0.02);
        assert!(config.agent_id.is_none());
        assert!(config.context.is_none());
        assert!(config.now.is_none());
    }

    #[test]
    fn query_config_new_fields() {
        let config = QueryConfig::default();
        assert!(config.query_embedding.is_none());
        assert!(config.scope.is_universal());
        assert_eq!(config.chars_per_token, 4);
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
            scope: ScopeRelation::Universal,
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
            stress: 0.54,
            scope_overlap: 1.0,
            temporal_overlap: 1.0,
            evidence_sources: vec![NodeId(1), NodeId(2)],
            description: Some("factory pattern vs DI refactor".to_string()),
        };
        assert_eq!(tension.edge_weight, 0.9);
        assert!(tension.stress > 0.0);
        assert_eq!(tension.evidence_sources, vec![NodeId(1), NodeId(2)]);
    }
}
