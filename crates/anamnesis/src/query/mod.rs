//! Query layer — retrieval types and pipeline for the Anamnesis engine.

pub mod activation;
pub mod assembly;
pub mod candidate;
pub mod field;
pub mod identity;
pub mod packaging;
pub mod rerank;
pub mod rwr;
pub mod scoring;
pub(crate) mod temporal;
pub mod types;

pub use activation::edge_valid_at;
pub use assembly::{
    ContradictionPair, ModeContext, ScoredNode, assemble_context_package,
    assemble_context_package_for_mode, compute_agent_tension,
};
pub use candidate::{
    CandidateSource, CandidateTrace, FusedCandidate, GraphRecallTrace, SearchCandidate,
    SearchTraceLevel,
};
pub use field::{FieldSignals, QueryField, potential_bias};
pub use identity::compute_identity_prior;
pub(crate) use packaging::decide_packaging;
pub use rwr::{ActivationResponse, PathCurrentMap, additive_rwr, additive_rwr_with_alpha};
pub use scoring::{ReadoutInputs, TieBreakKey, rank, readout_score, scope_weight, tie_break};
pub use types::{
    AccessedSite, ActivatedTension, CoReadoutPair, CommitTrace, ContextPackage, ConvergenceConfig,
    Fragment, PackagingMode, PathUsedEdge, Query, QueryConfig, ReadoutCandidate, SearchDiagnostics,
    SearchInput, SearchResult, SearchTrace, Tension, TokenBudget,
};

/// Return the deterministic lexical query surface used by unified search.
///
/// The original query is always first. Any count, relationship, frequency, or
/// entity-anchor decomposition is additive and follows the same core policy as
/// [`crate::Engine::search`]. Consumers can use this for an optional reranker
/// without copying the engine's query parser.
pub fn search_query_variants(query: &str) -> Vec<String> {
    crate::api::planned_query_variants(query)
}
