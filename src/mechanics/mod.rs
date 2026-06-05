//! Cognitive dynamics — pure scoring and propagation functions.
//!
//! Each submodule contains pure functions with no side effects.
//!
//! - `attraction`: Cosine similarity, merge candidate detection
//! - `gravity`: PageRank-like centrality scoring
//! - `perception`: Novelty, confidence, and budget gating
//! - `forgetting`: Exponential decay + reinforcement on access
//! - `topology`: Graph structure analysis (degree, bridge score, support score)
//! - `social`: Social reinforcement scoring (multi-agent corroboration, feedback signals)
//! - `priors`: Calibrated priors — the single home for the engine's numeric constants (ADR-0010)

pub mod attraction;
pub mod forces;
pub mod forgetting;
pub mod gravity;
pub mod health;
pub mod hopfield;
pub mod perception;
pub mod priors;
pub mod repulsion;
pub mod social;
pub mod topology;
