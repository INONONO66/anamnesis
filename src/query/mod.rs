//! Query layer — retrieval types and pipeline for the Anamnesis engine.
//!
//! Phase 1: Type definitions only. Query execution logic is Phase 2.

pub mod types;

pub use types::{ContextPackage, Fragment, Query, QueryConfig, Tension, TokenBudget};
