//! Recall seed selection stage — choose which seeds to activate.

use crate::query::FusedCandidate;

/// Select a limited number of fused candidates for graph recall expansion.
///
/// Takes the first `seed_limit.unwrap_or(3)` candidates from the fused list.
/// If `seed_limit` is `Some(0)`, returns an empty vector without panic.
pub(crate) fn select_recall_seeds(
    fused: Vec<FusedCandidate>,
    seed_limit: Option<usize>,
) -> Vec<FusedCandidate> {
    let n = seed_limit.unwrap_or(3);
    fused.into_iter().take(n).collect()
}
