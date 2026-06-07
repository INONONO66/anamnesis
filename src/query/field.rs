//! Query potential field — the restart-seed construction for additive RWR.
//!
//! A query acts like a battery: it imposes a semantic potential difference over
//! memory sites ([potential-landscape.md](../../docs/04-cognitive-dynamics/potential-landscape.md)).
//! Sites aligned with the query receive higher initial potential `phi_i`; the
//! softmax of `phi_i / tau` is the L1-normalized RWR restart distribution.
//!
//! ```text
//! phi_i = beta_text*text + beta_embed*embed + beta_entity*entity
//!       + beta_scope*scope + beta_prior*A_i + beta_identity*identity
//! seed_i = softmax(phi_i / tau)
//! ```
//!
//! `beta_prior = 1` by design: `A_i` (retained action) is already log prior-odds,
//! so it enters with unit coefficient (ACT-R/Bayes odds-additivity). This module is
//! query-local and read-only; it never mutates retained action or conductance.

use std::collections::HashMap;

use crate::graph::NodeId;
use crate::mechanics::priors::{
    BETA_EMBED, BETA_ENTITY, BETA_IDENTITY, BETA_PRIOR, BETA_SCOPE, BETA_TEXT, SEED_SOFTMAX_TAU,
};

/// Per-site bias inputs to the potential field.
///
/// All scores are the candidate-collection signals for one site. Absent signals
/// are `0.0`. `retained_action` is the authoritative log prior-odds reservoir.
#[derive(Debug, Clone, Copy, Default)]
pub struct FieldSignals {
    /// Lexical cue strength from query text.
    pub text_score: f64,
    /// Semantic alignment from embedding similarity.
    pub embedding_score: f64,
    /// Shared-entity-tag overlap.
    pub entity_overlap: f64,
    /// Scope compatibility / visibility weight.
    pub scope_weight: f64,
    /// Prior need-odds `A_i` (retained action). Unit coefficient by design.
    pub retained_action: f64,
    /// Active-agent identity prior.
    pub identity_bias: f64,
}

/// A query potential field over candidate sites.
///
/// Holds the per-site [`FieldSignals`] gathered during candidate collection. The
/// field is built before activation flow and produces the restart seed.
#[derive(Debug, Clone, Default)]
pub struct QueryField {
    signals: HashMap<NodeId, FieldSignals>,
}

impl QueryField {
    /// Create an empty field.
    pub fn new() -> Self {
        Self {
            signals: HashMap::new(),
        }
    }

    /// Insert or replace the signals for a candidate site.
    pub fn set(&mut self, node_id: NodeId, signals: FieldSignals) {
        self.signals.insert(node_id, signals);
    }

    /// Mutable access to a site's signals, inserting a default if absent.
    pub fn entry(&mut self, node_id: NodeId) -> &mut FieldSignals {
        self.signals.entry(node_id).or_default()
    }

    /// Number of candidate sites in the field.
    pub fn len(&self) -> usize {
        self.signals.len()
    }

    /// Whether the field has no candidate sites.
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }

    /// Compute the potential bias `phi_i` for every candidate site.
    pub fn potential_bias(&self) -> HashMap<NodeId, f64> {
        self.signals
            .iter()
            .map(|(&id, s)| (id, potential_bias(s)))
            .collect()
    }

    /// Build the L1-normalized RWR restart distribution `seed_i = softmax(phi_i / tau)`.
    ///
    /// Uses the default temperature [`SEED_SOFTMAX_TAU`]. Returns an empty map when
    /// the field is empty.
    pub fn seed_distribution(&self) -> HashMap<NodeId, f64> {
        self.seed_distribution_with_tau(SEED_SOFTMAX_TAU)
    }

    /// Build the restart distribution with an explicit softmax temperature `tau`.
    pub fn seed_distribution_with_tau(&self, tau: f64) -> HashMap<NodeId, f64> {
        softmax(&self.potential_bias(), tau)
    }
}

/// The log-linear potential bias for one site.
///
/// `phi_i = beta_text*text + beta_embed*embed + beta_entity*entity
///        + beta_scope*scope + beta_prior*A_i + beta_identity*identity`
pub fn potential_bias(s: &FieldSignals) -> f64 {
    let phi = BETA_TEXT * finite(s.text_score)
        + BETA_EMBED * finite(s.embedding_score)
        + BETA_ENTITY * finite(s.entity_overlap)
        + BETA_SCOPE * finite(s.scope_weight)
        + BETA_PRIOR * finite(s.retained_action)
        + BETA_IDENTITY * finite(s.identity_bias);
    if phi.is_finite() { phi } else { 0.0 }
}

/// Numerically stable softmax over `phi / tau`, producing an L1-normalized
/// distribution. Non-positive or non-finite `tau` falls back to `1.0`.
fn softmax(phi: &HashMap<NodeId, f64>, tau: f64) -> HashMap<NodeId, f64> {
    if phi.is_empty() {
        return HashMap::new();
    }
    let tau = if tau.is_finite() && tau > 0.0 {
        tau
    } else {
        1.0
    };

    // Stable iteration order for determinism in the max/sum reductions.
    let mut entries: Vec<(NodeId, f64)> = phi
        .iter()
        .filter(|(_, v)| v.is_finite())
        .map(|(&id, &v)| (id, v / tau))
        .collect();
    if entries.is_empty() {
        return HashMap::new();
    }
    entries.sort_by_key(|(id, _)| id.0);

    let max = entries
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::NEG_INFINITY, f64::max);

    let mut sum = 0.0;
    let mut exps: Vec<(NodeId, f64)> = Vec::with_capacity(entries.len());
    for (id, v) in &entries {
        let e = (v - max).exp();
        sum += e;
        exps.push((*id, e));
    }
    if !sum.is_finite() || sum <= 0.0 {
        // Degenerate; fall back to uniform over candidates.
        let uniform = 1.0 / entries.len() as f64;
        return entries.into_iter().map(|(id, _)| (id, uniform)).collect();
    }

    exps.into_iter().map(|(id, e)| (id, e / sum)).collect()
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_field_empty_seed() {
        let field = QueryField::new();
        assert!(field.seed_distribution().is_empty());
    }

    #[test]
    fn seed_is_l1_normalized() {
        let mut field = QueryField::new();
        field.set(
            NodeId(0),
            FieldSignals {
                text_score: 1.0,
                ..Default::default()
            },
        );
        field.set(
            NodeId(1),
            FieldSignals {
                embedding_score: 0.5,
                ..Default::default()
            },
        );
        let seed = field.seed_distribution();
        let total: f64 = seed.values().sum();
        assert!(
            (total - 1.0).abs() < 1e-12,
            "seed must sum to 1, got {total}"
        );
    }

    #[test]
    fn higher_potential_gets_more_mass() {
        let mut field = QueryField::new();
        field.set(
            NodeId(0),
            FieldSignals {
                text_score: 3.0,
                ..Default::default()
            },
        );
        field.set(
            NodeId(1),
            FieldSignals {
                text_score: 0.0,
                ..Default::default()
            },
        );
        let seed = field.seed_distribution();
        assert!(seed[&NodeId(0)] > seed[&NodeId(1)]);
    }

    #[test]
    fn retained_action_enters_with_unit_coefficient() {
        let s = FieldSignals {
            retained_action: 2.5,
            ..Default::default()
        };
        assert!((potential_bias(&s) - 2.5).abs() < 1e-12);
    }

    #[test]
    fn lower_tau_sharpens() {
        let mut field = QueryField::new();
        field.set(
            NodeId(0),
            FieldSignals {
                text_score: 2.0,
                ..Default::default()
            },
        );
        field.set(
            NodeId(1),
            FieldSignals {
                text_score: 0.0,
                ..Default::default()
            },
        );
        let sharp = field.seed_distribution_with_tau(0.1);
        let soft = field.seed_distribution_with_tau(10.0);
        assert!(sharp[&NodeId(0)] > soft[&NodeId(0)]);
    }
}
