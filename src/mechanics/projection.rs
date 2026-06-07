//! Reservoir → projection mapping (ADR-0002).
//!
//! The authoritative state is the unbounded log-odds reservoir; `salience` and
//! `weight` are bounded, derived *projections* of it. Per the ADR-0002 standing
//! invariant, only `commit`/`tick` may *store* a projection — these functions just
//! compute it deterministically from a reservoir.
//!
//! The canonical implementations live in [`crate::mechanics::priors`] (the single
//! home for the engine's numeric forms). This module re-exports them under the
//! `projection` name the dynamics substrate refers to, so call sites read as
//! `project_salience(A)` / `project_weight(C)` / `project_conductance(C)`.

pub use crate::mechanics::priors::{
    project_conductance, project_salience, project_weight, salience_to_action,
    weight_to_conductance,
};
