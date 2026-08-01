//! Candidate collection stage — text, vector, and entity-based retrieval.
//!
//! Each collector wraps an existing storage primitive and returns
//! `Vec<SearchCandidate>` with per-source scores preserved exactly. No score
//! normalization, no thresholding beyond the existing vector positive-score
//! filter and an explicit `limit` cap.

use crate::api::top_n_by_score;
use crate::error::Error;
use crate::mechanics::attraction::cosine_similarity;
use crate::query::{CandidateSource, SearchCandidate};
use crate::storage::StorageAdapter;

/// Collect text-based candidates from `storage.text_search`.
///
/// Preserves the storage-returned score exactly. `source_rank` is the
/// 0-indexed position in the (already ordered) `text_search` result.
pub(crate) fn collect_text_candidates<S: StorageAdapter>(
    storage: &S,
    query: &str,
    limit: usize,
) -> Vec<SearchCandidate> {
    storage
        .text_search(query, limit)
        .into_iter()
        .enumerate()
        .map(|(rank, (node_id, score))| SearchCandidate {
            node_id,
            score,
            source: CandidateSource::Text,
            source_rank: rank,
        })
        .collect()
}

/// Collect vector-based candidates via brute-force cosine similarity.
///
/// Iterates every node, computes cosine similarity against `query_embedding`,
/// drops non-positive scores (matching prior `Engine::search` behavior), then
/// keeps the top-`limit` by score using the same heap-based selector used in
/// the rest of the engine. The cosine score is preserved exactly on the
/// returned `SearchCandidate`.
pub(crate) fn collect_vector_candidates<S: StorageAdapter>(
    storage: &S,
    query_embedding: &[f64],
    limit: usize,
) -> Result<Vec<SearchCandidate>, Error> {
    Ok(
        collect_vector_candidate_lists(storage, &[query_embedding], limit)?
            .into_iter()
            .next()
            .unwrap_or_default(),
    )
}

/// Collect one ranked vector lane per query embedding with one storage scan.
///
/// Complex recall uses up to three query surfaces. Reading every SQLite node
/// once keeps that expansion bounded; only the cosine arithmetic is repeated.
pub(crate) fn collect_vector_candidate_lists<S: StorageAdapter>(
    storage: &S,
    query_embeddings: &[&[f64]],
    limit: usize,
) -> Result<Vec<Vec<SearchCandidate>>, Error> {
    let mut scores = vec![Vec::new(); query_embeddings.len()];
    for node_id in storage.all_node_ids() {
        let node = storage.get_node(node_id)?;
        let Some(embedding) = node.embedding.as_ref() else {
            continue;
        };
        for (lane, query_embedding) in query_embeddings.iter().enumerate() {
            let score = cosine_similarity(query_embedding, embedding);
            if score > 0.0 {
                scores[lane].push((node_id, score));
            }
        }
    }

    Ok(scores
        .into_iter()
        .map(|lane| {
            top_n_by_score(&lane, limit)
                .into_iter()
                .enumerate()
                .map(|(rank, (node_id, score))| SearchCandidate {
                    node_id,
                    score,
                    source: CandidateSource::Vector,
                    source_rank: rank,
                })
                .collect()
        })
        .collect())
}

/// Merge auxiliary vector lanes into one bounded union.
///
/// Each node keeps its best auxiliary rank and strongest cosine. Reassigning
/// one contiguous rank sequence ensures that two entity anchors do not become
/// two independent votes for the same node during hybrid fusion.
pub(crate) fn merge_auxiliary_vector_candidates(
    lanes: &[Vec<SearchCandidate>],
    limit: usize,
) -> Vec<SearchCandidate> {
    let mut by_node: std::collections::HashMap<crate::graph::NodeId, (usize, f64)> =
        std::collections::HashMap::new();
    for candidate in lanes.iter().flatten() {
        let entry = by_node
            .entry(candidate.node_id)
            .or_insert((candidate.source_rank, candidate.score));
        entry.0 = entry.0.min(candidate.source_rank);
        entry.1 = entry.1.max(candidate.score);
    }
    let mut merged: Vec<_> = by_node
        .into_iter()
        .map(|(node_id, (best_rank, score))| (node_id, best_rank, score))
        .collect();
    merged.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| right.2.total_cmp(&left.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    merged
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(source_rank, (node_id, _, score))| SearchCandidate {
            node_id,
            score,
            source: CandidateSource::Vector,
            source_rank,
        })
        .collect()
}

/// Collect entity-tag candidates by union over `nodes_by_entity_tag(tag)`.
///
/// The score is the number of input tags this node matches, expressed as
/// `f64`. Nodes are ordered by score descending, breaking ties by `NodeId`
/// ascending for determinism. The result is capped at `limit` and
/// `source_rank` reflects the final ordering after the cap.
pub(crate) fn collect_entity_candidates<S: StorageAdapter>(
    storage: &S,
    tags: &[String],
    limit: usize,
) -> Vec<SearchCandidate> {
    if tags.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut counts: std::collections::HashMap<crate::graph::NodeId, usize> =
        std::collections::HashMap::new();
    let unique_tags: std::collections::BTreeSet<&str> = tags.iter().map(String::as_str).collect();
    for tag in unique_tags {
        for node_id in storage.nodes_by_entity_tag(tag) {
            *counts.entry(node_id).or_insert(0) += 1;
        }
    }

    let mut scored: Vec<(crate::graph::NodeId, usize)> = counts.into_iter().collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(limit);

    scored
        .into_iter()
        .enumerate()
        .map(|(rank, (node_id, count))| SearchCandidate {
            node_id,
            score: count as f64,
            source: CandidateSource::Entity,
            source_rank: rank,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Origin;
    use crate::graph::{Graph, KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
    use crate::storage::SqliteStorage;
    use std::collections::{HashMap, VecDeque};

    fn make_node(
        id: NodeId,
        name: &str,
        content: &str,
        embedding: Option<Vec<f64>>,
        entity_tags: Vec<String>,
    ) -> Node {
        Node {
            id,
            node_type: KnowledgeType::Semantic,
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            accessed_at: Timestamp(0),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::graph::types::SourceKind::AgentObservation,
                session_id: "session-1".into(),
                scope: crate::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags,
            metadata: HashMap::new(),
        }
    }

    fn add(
        graph: &mut Graph<SqliteStorage>,
        name: &str,
        content: &str,
        embedding: Option<Vec<f64>>,
        entity_tags: Vec<String>,
    ) -> NodeId {
        let id = graph.next_node_id();
        graph
            .add_node(make_node(id, name, content, embedding, entity_tags))
            .unwrap();
        id
    }

    fn build_graph<F: FnOnce(&mut Graph<SqliteStorage>)>(seed: F) -> Graph<SqliteStorage> {
        let mut graph = Graph::new();
        seed(&mut graph);
        graph
    }

    #[test]
    fn collect_text_preserves_score() {
        let graph = build_graph(|g| {
            add(g, "alpha", "alpha factory pattern handler", None, vec![]);
            add(g, "beta", "beta factory utility helper", None, vec![]);
            add(g, "gamma", "gamma unrelated text", None, vec![]);
        });
        let storage = graph.storage();

        let primitive = storage.text_search("factory", 10);
        assert!(
            !primitive.is_empty(),
            "test setup expects matching text nodes"
        );

        let candidates = collect_text_candidates(storage, "factory", 10);
        assert_eq!(candidates.len(), primitive.len());

        for (rank, (candidate, expected)) in candidates.iter().zip(primitive.iter()).enumerate() {
            assert_eq!(candidate.node_id, expected.0);
            assert!(
                (candidate.score - expected.1).abs() < 1e-12,
                "score must match storage primitive within 1e-12"
            );
            assert_eq!(candidate.source, CandidateSource::Text);
            assert_eq!(candidate.source_rank, rank);
        }
    }

    #[test]
    fn collect_vector_preserves_cosine() {
        let query = vec![1.0_f64, 0.0, 0.0];
        let graph = build_graph(|g| {
            add(g, "v1", "v1 content", Some(vec![1.0, 0.0, 0.0]), vec![]);
            add(g, "v2", "v2 content", Some(vec![0.7, 0.7, 0.0]), vec![]);
            add(g, "v3", "v3 content", Some(vec![0.0, 1.0, 0.0]), vec![]);
            add(g, "v4", "v4 content", Some(vec![-1.0, 0.0, 0.0]), vec![]);
            add(g, "v5", "v5 content", None, vec![]);
        });
        let storage = graph.storage();

        let candidates = collect_vector_candidates(storage, &query, 10).unwrap();

        for candidate in &candidates {
            let node = storage.get_node(candidate.node_id).unwrap();
            let embedding = node.embedding.as_ref().expect("embedding required");
            let expected = cosine_similarity(&query, embedding);
            assert!(
                (candidate.score - expected).abs() < 1e-12,
                "cosine score must be preserved exactly"
            );
            assert_eq!(candidate.source, CandidateSource::Vector);
            assert!(
                candidate.score > 0.0,
                "non-positive scores must be filtered"
            );
        }

        for (i, candidate) in candidates.iter().enumerate() {
            assert_eq!(candidate.source_rank, i, "ranks must be monotone 0,1,2,...");
        }

        for window in candidates.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "candidates must be ordered by score descending"
            );
        }
    }

    #[test]
    fn collect_multiple_vector_queries_returns_one_ranked_lane_per_query() {
        let first_query = vec![1.0_f64, 0.0, 0.0];
        let second_query = vec![0.0_f64, 1.0, 0.0];
        let graph = build_graph(|g| {
            add(g, "x", "x content", Some(vec![1.0, 0.0, 0.0]), vec![]);
            add(g, "y", "y content", Some(vec![0.0, 1.0, 0.0]), vec![]);
            add(g, "xy", "xy content", Some(vec![0.7, 0.7, 0.0]), vec![]);
        });

        let lanes = collect_vector_candidate_lists(
            graph.storage(),
            &[first_query.as_slice(), second_query.as_slice()],
            3,
        )
        .unwrap();

        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0][0].node_id, NodeId(0));
        assert_eq!(lanes[1][0].node_id, NodeId(1));
        assert!(lanes.iter().flatten().all(|candidate| {
            candidate.source == CandidateSource::Vector && candidate.score > 0.0
        }));
    }

    #[test]
    fn auxiliary_vector_union_deduplicates_nodes_and_reassigns_one_rank_sequence() {
        let lanes = vec![
            vec![
                SearchCandidate {
                    node_id: NodeId(1),
                    score: 0.8,
                    source: CandidateSource::Vector,
                    source_rank: 0,
                },
                SearchCandidate {
                    node_id: NodeId(2),
                    score: 0.7,
                    source: CandidateSource::Vector,
                    source_rank: 1,
                },
            ],
            vec![
                SearchCandidate {
                    node_id: NodeId(2),
                    score: 0.9,
                    source: CandidateSource::Vector,
                    source_rank: 0,
                },
                SearchCandidate {
                    node_id: NodeId(3),
                    score: 0.6,
                    source: CandidateSource::Vector,
                    source_rank: 1,
                },
            ],
        ];

        let merged = merge_auxiliary_vector_candidates(&lanes, 3);

        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].node_id, NodeId(2));
        assert_eq!(merged[0].score, 0.9);
        assert_eq!(merged[1].node_id, NodeId(1));
        assert_eq!(merged[2].node_id, NodeId(3));
        assert_eq!(
            merged
                .iter()
                .map(|candidate| candidate.source_rank)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn collect_entity_returns_correct_source_rank() {
        let graph = build_graph(|g| {
            add(g, "a", "node a", None, vec!["x".into(), "y".into()]);
            add(g, "b", "node b", None, vec!["x".into()]);
            add(g, "c", "node c", None, vec!["y".into()]);
            add(g, "d", "node d", None, vec!["z".into()]);
        });
        let storage = graph.storage();

        let tags = vec!["x".into(), "y".into()];
        let candidates = collect_entity_candidates(storage, &tags, 10);

        assert!(
            !candidates.is_empty(),
            "expected entity candidates for shared tags"
        );

        for (i, candidate) in candidates.iter().enumerate() {
            assert_eq!(candidate.source, CandidateSource::Entity);
            assert_eq!(candidate.source_rank, i);
        }

        for window in candidates.windows(2) {
            assert!(
                window[0].score > window[1].score
                    || (window[0].score == window[1].score
                        && window[0].node_id < window[1].node_id),
                "ordering must be score desc, NodeId asc"
            );
        }

        let top = candidates.first().unwrap();
        assert!((top.score - 2.0).abs() < 1e-12, "node a matches both tags");
    }

    #[test]
    fn collect_entity_deduplicates_query_tags_before_counting() {
        let graph = build_graph(|g| {
            add(g, "a", "node a", None, vec!["x".into(), "y".into()]);
            add(g, "b", "node b", None, vec!["x".into()]);
            add(g, "c", "node c", None, vec!["y".into()]);
        });
        let storage = graph.storage();

        let tags = vec!["x".into(), "x".into(), "y".into()];
        let candidates = collect_entity_candidates(storage, &tags, 10);

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].node_id, NodeId(0));
        assert_eq!(candidates[0].score, 2.0);
        assert_eq!(candidates[1].score, 1.0);
        assert_eq!(candidates[2].score, 1.0);
    }

    #[test]
    fn collect_entity_empty_tags_returns_empty() {
        let graph = build_graph(|g| {
            add(g, "a", "node a", None, vec!["x".into()]);
        });
        let storage = graph.storage();
        assert!(collect_entity_candidates(storage, &[], 10).is_empty());
    }

    #[test]
    fn collect_text_respects_limit() {
        let graph = build_graph(|g| {
            for i in 0..5 {
                add(
                    g,
                    &format!("n{i}"),
                    &format!("alpha node {i}"),
                    None,
                    vec![],
                );
            }
        });
        let storage = graph.storage();
        let candidates = collect_text_candidates(storage, "alpha", 3);
        assert!(candidates.len() <= 3);
    }
}
