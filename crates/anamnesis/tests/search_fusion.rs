//! Public smoke tests for Task 7 RRF candidate fusion.
//!
//! `fuse_candidates` and `RRF_K` are `pub(crate)`, so these tests cannot call
//! the fusion primitive directly. Instead they observe its effect through
//! `Engine::search`. The exact `1/(60 + rank + 1)` formula and tie-break
//! ordering are pinned in `src/api/search/fusion.rs` under `#[cfg(test)]`.

use std::collections::HashSet;

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, NodeId};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;

fn origin(session: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
        session_id: session.into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    }
}

fn ingest(engine: &mut Engine, name: &str, content: &str, embedding: Option<Vec<f64>>) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .expect("ingest must succeed in fusion smoke fixture");
}

fn engine_with(setup: impl FnOnce(&mut Engine)) -> Engine {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_threshold(2.0);
    let mut engine = Engine::with_config(config);
    setup(&mut engine);
    engine
}

#[test]
fn single_source_three_candidates_preserves_order() {
    // With a single source (text only), RRF contribution `1/(60+rank+1)` is
    // monotone decreasing in rank, so the fused order equals the source order.
    // Three matching nodes must all become fused seeds. Task 10 changed graph
    // recall to one multi-source spreading invocation for all selected seeds.
    let engine = engine_with(|e| {
        ingest(e, "alpha", "alpha factory pattern handler", None);
        ingest(e, "beta", "beta factory utility helper", None);
        ingest(e, "gamma", "gamma factory token store", None);
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert_eq!(
        result.trace.seed_count, 3,
        "three matching nodes must produce three fused seeds"
    );
    assert!(
        result.trace.iterations >= 1,
        "Task 10 runs one multi-source graph recall invocation for all selected seeds"
    );
    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "text_search"),
        "single-source RRF still routes through text_search"
    );
}

#[test]
fn two_sources_a_rank_one_in_both_yields_2_over_61() {
    // A node ranked 0 in both text and vector contributes `1/61 + 1/61 = 2/61`
    // — the exact formula assertion lives in the fusion unit tests. Here we
    // observe the dual-source path end-to-end: both strategies must be active
    // and the single matching node must collapse to exactly one fused seed.
    let engine = engine_with(|e| {
        ingest(
            e,
            "winner",
            "winner singleton factory pattern unique",
            Some(vec![1.0, 0.0, 0.0]),
        );
    });

    let result = engine
        .search(SearchInput {
            text: "winner".into(),
            query_embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "text_search"),
        "text source must contribute to RRF"
    );
    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "vector_similarity"),
        "vector source must contribute to RRF"
    );
    assert_eq!(
        result.trace.seed_count, 1,
        "one node, two sources → one fused seed (NodeId-keyed aggregation)"
    );
    assert!(
        result.trace.iterations >= 1,
        "Task 10 runs one multi-source graph recall invocation"
    );
}

#[test]
fn tie_break_node_id_ascending() {
    // Two nodes, each ranked 0 in exactly one source → tied fused score 1/61.
    // The smaller NodeId must win the tie. Ingest order fixes NodeId
    // allocation: the first ingested gets the smaller id and must appear in
    // the fused seed list ahead of the second.
    let engine = engine_with(|e| {
        ingest(
            e,
            "smaller_id_node",
            "vector_unique_term",
            Some(vec![1.0, 0.0, 0.0]),
        );
        ingest(e, "larger_id_node", "text_unique_term factory", None);
    });

    let result = engine
        .search(SearchInput {
            text: "text_unique_term".into(),
            query_embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert_eq!(
        result.trace.seed_count, 2,
        "two distinct nodes must produce two fused seeds"
    );
    assert!(
        result.trace.iterations >= 1,
        "Task 10 runs one multi-source graph recall invocation for both selected seeds"
    );
}

#[test]
fn fused_order_differs_from_node_id_sort() {
    // Pre-Task-7, `Engine::search` did `all_seed_ids.sort()` (ascending NodeId)
    // then `.take(3)`. With four candidates that would always drop the highest
    // NodeId. After Task 7, RRF fusion reorders candidates by fused score.
    // After Task 8, `select_recall_seeds` applies the seed_limit (default 3)
    // to the fused order, so the dropped seed is the lowest-ranked one —
    // not necessarily the highest NodeId.
    //
    // Fixture: NodeIds 0..=2 are word-matches (tied IDF score), NodeId 3 has
    // an exact-name match for "factory" (score 1.0). Storage's text_search
    // therefore ranks them: NodeId 3 → rank 0, NodeId 0 → rank 1, NodeId 1 →
    // rank 2, NodeId 2 → rank 3 (insertion order preserved on score ties).
    //
    // Fused order keeps {3, 0, 1} (top 3); NodeId-ascending order would have
    // kept {0, 1, 2}. The discriminating activation is NodeId(3), which only
    // appears under the new fused ordering.
    let engine = engine_with(|e| {
        ingest(e, "alpha", "alpha factory pattern", None);
        ingest(e, "beta", "beta factory thing", None);
        ingest(e, "gamma", "gamma factory other", None);
        ingest(e, "factory", "exact name match for factory", None);
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert_eq!(
        result.trace.seed_count, 3,
        "default seed_limit=3 selects top-3 fused candidates for graph recall"
    );
    assert!(
        result.trace.iterations >= 1,
        "Task 10 runs spreading activation once from all 3 selected seeds"
    );

    let activated: HashSet<_> = result
        .package
        .knowledge
        .iter()
        .chain(result.package.memories.iter())
        .chain(result.package.identity.iter())
        .map(|f| f.node_id)
        .collect();

    assert!(
        activated.contains(&NodeId(3)),
        "fused order keeps NodeId(3) (rank 0); NodeId-ascending sort would have dropped it. \
         Activated set: {activated:?}"
    );
}
