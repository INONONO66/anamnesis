//! Cognitive dynamics — pure scoring and propagation functions.
//!
//! Each submodule contains pure functions with no side effects.
//!
//! - `attraction`: Cosine similarity, merge candidate detection
//! - `gravity`: PageRank-like centrality scoring
//! - `perception`: Novelty, confidence, and budget gating
//! - `forgetting`: ACT-R base-level activation kernel (power-law forgetting shape)
//! - `interactions`: Reservoir dynamics — power-law decay, Rescorla-Wagner,
//!   access gain, and Hebbian-Oja conductance on `A_i`/`C_ij` (ADR-0002/0003/0008)
//! - `projection`: Reservoir → bounded-projection mapping (`project_salience`/`project_weight`)
//! - `topology`: Graph structure analysis (degree, bridge score, support score)
//! - `social`: Social reinforcement scoring (multi-agent corroboration, feedback signals)
//! - `priors`: Calibrated priors — the single home for the engine's numeric constants (ADR-0010)

pub mod attraction;
pub mod forgetting;
pub mod gravity;
pub mod health;
pub mod interactions;
pub mod perception;
pub mod priors;
pub mod projection;
pub mod repulsion;
pub mod social;
pub mod topology;
