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
pub mod types;

pub use activation::edge_valid_at;
pub use assembly::{
    ContradictionPair, ModeContext, ScoredNode, assemble_context_package,
    assemble_context_package_for_mode, compute_agent_tension, determine_scope,
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
    ContextPackage, ConvergenceConfig, Fragment, PackagingMode, Query, QueryConfig, SearchInput,
    SearchResult, SearchTrace, Tension, TokenBudget,
};
