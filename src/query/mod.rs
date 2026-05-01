//! Query layer — retrieval types and pipeline for the Anamnesis engine.

pub mod activation;
pub mod assembly;
pub mod candidate;
pub mod identity;
pub mod packaging;
pub mod rerank;
pub mod rwr;
pub mod scoring;
pub mod types;

pub use activation::{NodeInfo, initial_activation, salience_gate, spread_activation};
pub use assembly::{ScoredNode, assemble_context_package, compute_agent_tension, determine_scope};
pub use candidate::{
    CandidateSource, CandidateTrace, FusedCandidate, GraphRecallTrace, SearchCandidate,
    SearchTraceLevel,
};
pub use identity::compute_identity_prior;
pub(crate) use packaging::decide_packaging;
pub use rwr::{random_walk_restart, random_walk_restart_from_distribution};
pub use scoring::{all_forces, compute_with_forces, final_score, scope_weight};
pub use types::{
    ContextPackage, Fragment, PackagingMode, Query, QueryConfig, SearchInput, SearchResult,
    SearchTrace, Tension, TokenBudget,
};
