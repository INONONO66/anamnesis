//! Activation-flow stage — additive directed RWR over conductance.
//!
//! Read-only. Converts the fused-candidate seed distribution into a query
//! potential field, derives the softmax restart seed, and runs the additive RWR
//! ([`crate::query::rwr`]). Returns the settled response plus a recall trace.

use std::collections::HashMap;

use crate::graph::NodeId;
use crate::query::field::{FieldSignals, QueryField};
use crate::query::{
    ActivationResponse, FusedCandidate, GraphRecallTrace, QueryConfig, additive_rwr,
};
use crate::storage::StorageAdapter;

pub(crate) fn run_graph_recalls<S: StorageAdapter>(
    storage: &S,
    fused_seeds: &[FusedCandidate],
    query_config: &QueryConfig,
    identity_prior: Option<&HashMap<NodeId, f64>>,
) -> (ActivationResponse, GraphRecallTrace) {
    let now = query_config
        .now
        .unwrap_or_else(crate::graph::Timestamp::now);

    // Build the query potential field from the fused candidate signals plus any
    // identity prior, then derive the L1-normalized softmax restart seed.
    let mut field = QueryField::new();
    for seed in fused_seeds {
        let retained_action = storage.get_retained_action(seed.node_id).unwrap_or(0.0);
        field.set(
            seed.node_id,
            FieldSignals {
                text_score: seed.fused_score,
                retained_action,
                ..Default::default()
            },
        );
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

    (response, trace)
}
