//! Activation-flow stage — additive directed RWR over conductance.
//!
//! Read-only. Converts the fused-candidate seed distribution into a query
//! potential field, derives the softmax restart seed, and runs the additive RWR
//! ([`crate::query::rwr`]). Returns the settled response plus a recall trace.
//!
//! # Temporal score coverage
//!
//! Seeds have their `temporal_score` field set here from the parsed query time
//! cues and the node's `created_at` timestamp (query-local, never stored).
//! Graph-reached non-seed nodes get the `FieldSignals` default (`0.0` temporal).
//! That asymmetry mirrors how text/entity signals are handled and is acceptable
//! for v1; per-node temporal computation in assemble is intentionally omitted to
//! keep the change minimal.

use std::collections::HashMap;

use crate::graph::NodeId;
use crate::query::field::{FieldSignals, QueryField};
use crate::query::temporal::TimeRange;
use crate::query::{
    ActivationResponse, CandidateSource, FusedCandidate, GraphRecallTrace, QueryConfig,
    additive_rwr,
};
use crate::storage::StorageAdapter;

pub(crate) fn run_graph_recalls<S: StorageAdapter>(
    storage: &S,
    fused_seeds: &[FusedCandidate],
    query_config: &QueryConfig,
    identity_prior: Option<&HashMap<NodeId, f64>>,
    time_cues: &[TimeRange],
) -> (ActivationResponse, GraphRecallTrace, QueryField) {
    let now = query_config
        .now
        .unwrap_or_else(crate::graph::Timestamp::now);

    // Build the query potential field from the fused candidate signals plus any
    // identity prior, then derive the L1-normalized softmax restart seed.
    let mut field = QueryField::new();
    for seed in fused_seeds {
        let retained_action = storage.get_retained_action(seed.node_id).unwrap_or(0.0);
        let mut signals = field_signals(seed, retained_action);
        if !time_cues.is_empty() {
            if let Ok(node) = storage.get_node(seed.node_id) {
                signals.temporal_score =
                    crate::query::temporal::temporal_proximity(node.created_at.0, time_cues);
            }
        }
        field.set(seed.node_id, signals);
    }
    if let Some(prior) = identity_prior {
        for (&node_id, &bias) in prior {
            let entry = field.entry(node_id);
            entry.identity_bias += bias;
            if entry.retained_action == 0.0 {
                entry.retained_action = storage.get_retained_action(node_id).unwrap_or(0.0);
            }
        }
    }

    let seed = field.seed_distribution();
    let response = additive_rwr(&seed, storage, now);

    let trace = GraphRecallTrace {
        invocation_count: 1,
        activated_count: response.activation.len(),
        iterations: response.iterations,
        residual: response.residual,
        truncated: response.truncated,
        excluded_edge_count: response.excluded_edges.len(),
    };

    (response, trace, field)
}

/// Map a fused candidate's per-source raw scores onto the potential-field
/// signals of potential-landscape.md: text match, embedding similarity, and
/// entity overlap each enter `phi_i` through their own `beta` term. Multiple
/// contributions from the same source (e.g. several text sub-queries) keep the
/// strongest score.
fn field_signals(seed: &FusedCandidate, retained_action: f64) -> FieldSignals {
    let mut signals = FieldSignals {
        retained_action,
        ..Default::default()
    };
    for (source, _rank, raw_score) in &seed.contributing {
        match source {
            CandidateSource::Text => signals.text_score = signals.text_score.max(*raw_score),
            CandidateSource::Vector => {
                signals.embedding_score = signals.embedding_score.max(*raw_score)
            }
            CandidateSource::Entity => {
                signals.entity_overlap = signals.entity_overlap.max(*raw_score)
            }
        }
    }
    signals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(contributing: Vec<(CandidateSource, usize, f64)>) -> FusedCandidate {
        FusedCandidate {
            node_id: NodeId(1),
            fused_score: 0.05,
            contributing,
        }
    }

    #[test]
    fn vector_contribution_enters_embedding_score() {
        let seed = candidate(vec![(CandidateSource::Vector, 0, 0.82)]);
        let signals = field_signals(&seed, 0.0);
        assert_eq!(signals.embedding_score, 0.82);
        assert_eq!(signals.text_score, 0.0);
        assert_eq!(signals.entity_overlap, 0.0);
    }

    #[test]
    fn per_source_scores_map_to_their_own_signals() {
        let seed = candidate(vec![
            (CandidateSource::Text, 0, 0.4),
            (CandidateSource::Vector, 1, 0.7),
            (CandidateSource::Entity, 2, 2.0),
        ]);
        let signals = field_signals(&seed, 1.5);
        assert_eq!(signals.text_score, 0.4);
        assert_eq!(signals.embedding_score, 0.7);
        assert_eq!(signals.entity_overlap, 2.0);
        assert_eq!(signals.retained_action, 1.5);
    }

    #[test]
    fn repeated_source_keeps_strongest_score() {
        let seed = candidate(vec![
            (CandidateSource::Text, 0, 0.3),
            (CandidateSource::Text, 1, 0.6),
        ]);
        let signals = field_signals(&seed, 0.0);
        assert_eq!(signals.text_score, 0.6);
    }

    #[test]
    fn fused_rrf_score_no_longer_leaks_into_text_signal() {
        let seed = candidate(vec![(CandidateSource::Vector, 0, 0.9)]);
        let signals = field_signals(&seed, 0.0);
        assert_eq!(
            signals.text_score, 0.0,
            "the RRF fused score must not masquerade as a lexical signal"
        );
    }
}
