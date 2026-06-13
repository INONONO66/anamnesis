//! Cognitive dynamics — pure scoring and propagation functions.
//!
//! Each submodule contains pure functions with no side effects.
//!
//! - `attraction`: Cosine similarity + type-affinity candidate selection (no mass)
//! - `perception`: Two-stage observation gate (confidence/budget, then novelty
//!   Allocate|Route) with surprise-gated initial charge (ADR-0009)
//! - `frustration`: Query-local contradiction stress `sigma_ij` — contradictions
//!   are surfaced as tension, never suppressed or deleted (ADR-0006)
//! - `energy`: Query-local readout energy `E(S|Q)` — an interpretive descent
//!   objective with `+/-1` structural signs; the RWR stationary vector is the true
//!   fixed point (ADR-0007)
//! - `forgetting`: ACT-R base-level activation kernel (power-law forgetting shape)
//! - `interactions`: Reservoir dynamics — power-law decay, Rescorla-Wagner,
//!   access gain, and Hebbian-Oja conductance on `A_i`/`C_ij` (ADR-0002/0003/0008)
//! - `projection`: Reservoir → bounded-projection mapping (`project_salience`/`project_weight`)
//! - `topology`: Graph structure analysis (degree, bridge score, support score)
//! - `health`: The nine read-only `GraphHealth` metrics (observability.md)
//! - `observability`: `InvariantCheck` suite + `OperationalWarning`s (observability.md)
//! - `social`: Social reinforcement scoring (multi-agent corroboration, feedback signals)
//! - `priors`: Calibrated priors — the single home for the engine's numeric constants (ADR-0010)

pub mod attraction;
pub mod energy;
pub mod forgetting;
pub mod frustration;
pub mod health;
pub mod interactions;
pub mod observability;
pub mod perception;
pub mod priors;
pub mod projection;
pub mod social;
pub mod topology;
