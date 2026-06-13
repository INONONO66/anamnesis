//! Candidate fusion stage — Reciprocal Rank Fusion (RRF).
//!
//! Aggregates per-source candidate lists into a single ranked list using
//! the standard RRF formula `1 / (k + rank + 1)` with `k = 60`. This
//! constant matches Microsoft Azure AI Search and Weaviate hybrid search.
//!
//! The damping constant is locked. Fusion exposes no tuning struct by
//! design — see the module-level invariants for details.

use std::collections::HashMap;

use crate::graph::NodeId;
use crate::query::{CandidateSource, FusedCandidate, SearchCandidate};

/// RRF damping constant: `RRF_K = 60` matches Microsoft Azure AI Search and Weaviate.
pub(crate) const RRF_K: usize = 60;

type Contribution = (CandidateSource, usize, f64);
type Accumulator = HashMap<NodeId, (f64, Vec<Contribution>)>;

/// Fuse per-source candidate lists into a single ranked list via RRF.
///
/// For each candidate, contributes `1.0 / ((RRF_K + candidate.source_rank + 1) as f64)`
/// to that node's fused score, summing across sources. Output is sorted by
/// `fused_score` descending, breaking ties by `node_id` ascending.
///
/// `FusedCandidate.contributing` retains the per-source `(source, source_rank,
/// raw_score)` triples and is itself sorted by `(source, source_rank)` so that
/// ordering does not leak `HashMap` iteration nondeterminism.
pub(crate) fn fuse_candidates(per_source: Vec<Vec<SearchCandidate>>) -> Vec<FusedCandidate> {
    let mut accumulator: Accumulator = HashMap::new();

    for source_list in per_source {
        for candidate in source_list {
            let contribution = 1.0 / ((RRF_K + candidate.source_rank + 1) as f64);
            let entry = accumulator
                .entry(candidate.node_id)
                .or_insert_with(|| (0.0, Vec::new()));
            entry.0 += contribution;
            entry
                .1
                .push((candidate.source, candidate.source_rank, candidate.score));
        }
    }

    let mut fused: Vec<FusedCandidate> = accumulator
        .into_iter()
        .map(|(node_id, (fused_score, mut contributing))| {
            // Deterministic contributing order: by source, then rank, then score.
            contributing.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            });
            FusedCandidate {
                node_id,
                fused_score,
                contributing,
            }
        })
        .collect();

    fused.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(node_id: u64, score: f64, source: CandidateSource, rank: usize) -> SearchCandidate {
        SearchCandidate {
            node_id: NodeId(node_id),
            score,
            source,
            source_rank: rank,
        }
    }

    #[test]
    fn rrf_k_is_locked_at_60() {
        assert_eq!(RRF_K, 60);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(fuse_candidates(Vec::new()).is_empty());
        assert!(fuse_candidates(vec![Vec::new(), Vec::new()]).is_empty());
    }

    #[test]
    fn single_source_three_candidates_preserves_rank_order() {
        let candidates = vec![
            cand(7, 0.9, CandidateSource::Text, 0),
            cand(3, 0.8, CandidateSource::Text, 1),
            cand(5, 0.7, CandidateSource::Text, 2),
        ];
        let fused = fuse_candidates(vec![candidates]);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].node_id, NodeId(7));
        assert_eq!(fused[1].node_id, NodeId(3));
        assert_eq!(fused[2].node_id, NodeId(5));

        let expected = [1.0 / 61.0, 1.0 / 62.0, 1.0 / 63.0];
        for (entry, want) in fused.iter().zip(expected.iter()) {
            assert!(
                (entry.fused_score - *want).abs() < 1e-12,
                "fused_score must match RRF formula"
            );
        }
    }

    #[test]
    fn two_sources_node_a_rank_zero_in_both_yields_2_over_61() {
        let text = vec![cand(1, 0.9, CandidateSource::Text, 0)];
        let vector = vec![cand(1, 0.85, CandidateSource::Vector, 0)];
        let fused = fuse_candidates(vec![text, vector]);
        assert_eq!(fused.len(), 1);

        let expected = 2.0 / 61.0;
        assert!(
            (fused[0].fused_score - expected).abs() < 1e-12,
            "rank-zero contributions from two sources must sum to exactly 2/61"
        );

        assert_eq!(fused[0].contributing.len(), 2);
        assert!(
            fused[0]
                .contributing
                .iter()
                .any(|c| c.0 == CandidateSource::Text && (c.2 - 0.9).abs() < 1e-12)
        );
        assert!(
            fused[0]
                .contributing
                .iter()
                .any(|c| c.0 == CandidateSource::Vector && (c.2 - 0.85).abs() < 1e-12)
        );
    }

    #[test]
    fn tie_break_smaller_node_id_first() {
        let text = vec![cand(5, 0.9, CandidateSource::Text, 0)];
        let vector = vec![cand(2, 0.85, CandidateSource::Vector, 0)];
        let fused = fuse_candidates(vec![text, vector]);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].node_id, NodeId(2));
        assert_eq!(fused[1].node_id, NodeId(5));
        assert!(
            (fused[0].fused_score - fused[1].fused_score).abs() < 1e-12,
            "scores must be tied for the tie-break to apply"
        );
    }

    #[test]
    fn fused_order_differs_from_node_id_sort() {
        // NodeId(1) is rank 5 (low contribution); NodeId(7) is rank 0 (high contribution).
        // NodeId-ascending order would be 1, 7. Fused order must be 7, 1.
        let text = vec![
            cand(7, 0.9, CandidateSource::Text, 0),
            cand(1, 0.1, CandidateSource::Text, 5),
        ];
        let fused = fuse_candidates(vec![text]);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].node_id, NodeId(7), "high-rank node must be first");
        assert_eq!(fused[1].node_id, NodeId(1));
        let mut node_id_sorted = vec![NodeId(1), NodeId(7)];
        node_id_sorted.sort();
        assert_ne!(
            fused.iter().map(|f| f.node_id).collect::<Vec<_>>(),
            node_id_sorted,
            "fused order must not coincide with NodeId-ascending sort here"
        );
    }

    #[test]
    fn duplicate_node_in_same_source_aggregates_both_contributions() {
        // Edge case: a NodeId can appear twice if two text sub-queries both
        // produce it. Each contribution must count.
        let text_q1 = vec![cand(4, 0.9, CandidateSource::Text, 0)];
        let text_q2 = vec![cand(4, 0.7, CandidateSource::Text, 2)];
        let fused = fuse_candidates(vec![text_q1, text_q2]);
        assert_eq!(fused.len(), 1);
        let expected = 1.0 / 61.0 + 1.0 / 63.0;
        assert!((fused[0].fused_score - expected).abs() < 1e-12);
        assert_eq!(fused[0].contributing.len(), 2);
    }

    #[test]
    fn contributing_order_is_deterministic() {
        // Reverse insertion order of sources; contributing should still be
        // (Text, 0) then (Vector, 0) because we sort by source then rank.
        let vector = vec![cand(9, 0.85, CandidateSource::Vector, 0)];
        let text = vec![cand(9, 0.9, CandidateSource::Text, 0)];
        let fused = fuse_candidates(vec![vector, text]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].contributing[0].0, CandidateSource::Text);
        assert_eq!(fused[0].contributing[1].0, CandidateSource::Vector);
    }
}
