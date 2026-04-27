//! Cognitive dynamics — pure scoring and propagation functions.
//!
//! All mechanics will be implemented in Phase 2.
//! Each submodule contains pure functions with no side effects.
//!
//! - `attraction`: Cosine similarity, merge candidate detection
//! - `gravity`: PageRank-like centrality scoring
//! - `perception`: Novelty, confidence, and budget gating
//! - `forgetting`: Exponential decay + reinforcement on access

pub mod attraction;
pub mod forces;
pub mod forgetting;
pub mod gravity;
pub mod perception;
pub mod repulsion;
