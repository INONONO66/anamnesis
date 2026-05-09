//! Search pipeline candidate and trace types for the Anamnesis cognitive graph engine.

use std::collections::BTreeMap;

use crate::api::SpreadingModel;
use crate::graph::NodeId;

/// Source of a search candidate — indicates which retrieval strategy produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CandidateSource {
    /// Candidate from text search (keyword matching).
    Text,
    /// Candidate from vector similarity (embedding-based).
    Vector,
    /// Candidate from entity tag matching (cross-agent linking).
    Entity,
}

/// A single candidate node retrieved by one search strategy.
///
/// Carries the node ID, strategy-specific score, and source rank (position in that strategy's results).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCandidate {
    /// The node ID of the candidate.
    pub node_id: NodeId,
    /// Strategy-specific score [0, 1].
    pub score: f64,
    /// Which retrieval strategy produced this candidate.
    pub source: CandidateSource,
    /// Rank within the source strategy's results (0-indexed).
    pub source_rank: usize,
}

/// A candidate after fusion across multiple retrieval strategies.
///
/// Combines scores from multiple sources (text, vector, entity) into a single fused score.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedCandidate {
    /// The node ID of the candidate.
    pub node_id: NodeId,
    /// Fused score combining all contributing sources [0, 1].
    pub fused_score: f64,
    /// Contributing sources with their rank and individual score.
    /// Format: (source, source_rank, individual_score)
    pub contributing: Vec<(CandidateSource, usize, f64)>,
}

/// Trace of candidate retrieval during a search operation.
///
/// Records per-source candidate counts and nodes dropped during filtering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CandidateTrace {
    /// Count of candidates retrieved per source.
    pub per_source_counts: BTreeMap<CandidateSource, usize>,
    /// Nodes dropped during filtering with reason.
    pub dropped: Vec<(NodeId, &'static str)>,
}

/// Trace of graph spreading activation during a search operation.
///
/// Records invocation count, activated node count, and the spreading model used.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphRecallTrace {
    /// Number of spreading activation invocations.
    pub invocation_count: u32,
    /// Total number of nodes activated during spreading.
    pub activated_count: usize,
    /// Spreading activation model used.
    pub model_used: SpreadingModel,
    /// Number of invalid temporal edges skipped during graph recall.
    pub edge_count_skipped_invalid: usize,
    /// Number of convergence check rounds performed.
    pub convergence_rounds: usize,
    /// Whether spreading activation converged early.
    pub converged: bool,
}

/// Verbosity level for search trace output.
///
/// Controls how much detail is captured during search operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchTraceLevel {
    /// No trace information captured.
    None,
    /// Summary-level trace (counts, high-level decisions).
    Summary,
    /// Full trace (all candidates, dropped nodes, detailed decisions).
    Full,
}
