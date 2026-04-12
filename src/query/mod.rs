//! Query layer — retrieval types and pipeline for the Anamnesis engine.

pub mod activation;
pub mod assembly;
pub mod identity;
pub mod scoring;
pub mod types;

pub use activation::{NodeInfo, initial_activation, salience_gate, spread_activation};
pub use assembly::{ScoredNode, assemble_context_package, compute_agent_tension, determine_scope};
pub use identity::compute_identity_prior;
pub use scoring::{final_score, scope_weight};
pub use types::{ContextPackage, Fragment, Query, QueryConfig, Tension, TokenBudget};
