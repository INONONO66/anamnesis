#[path = "../benches/eval_common/mod.rs"]
mod eval_common;

use eval_common::real_bench::dataset::GoldEvidence;
use eval_common::real_bench::graph::ranked_fragments_for_test;
use eval_common::real_bench::metrics::{RankedRetrieval, retrieval_metrics};

use anamnesis::graph::scope::ScopeRelation;
use anamnesis::graph::{KnowledgeType, NodeId, Origin, PeerId};
use anamnesis::query::{ContextPackage, Fragment};

#[test]
fn retrieval_metrics_score_ranked_gold_hits() {
    let ranked = [
        RankedRetrieval {
            matched_gold_units: Vec::new(),
            score: 0.9,
        },
        RankedRetrieval {
            matched_gold_units: vec!["turn:D1:2".to_string()],
            score: 0.8,
        },
        RankedRetrieval {
            matched_gold_units: vec!["turn:D1:3".to_string()],
            score: 0.7,
        },
    ];

    let metrics = retrieval_metrics(&ranked, 2, 3);

    assert_eq!(metrics.precision_at_k, 2.0 / 3.0);
    assert_eq!(metrics.recall_at_k, 1.0);
    assert_eq!(metrics.mrr, 0.5);
    assert!(metrics.ndcg_at_k > 0.0);
}

#[test]
fn retrieval_metrics_dedupe_repeated_gold_units_and_use_total_relevant_for_ndcg() {
    let ranked = [
        RankedRetrieval {
            matched_gold_units: vec!["session:answer_1".to_string()],
            score: 0.9,
        },
        RankedRetrieval {
            matched_gold_units: vec!["session:answer_1".to_string()],
            score: 0.8,
        },
    ];

    let metrics = retrieval_metrics(&ranked, 2, 2);

    assert_eq!(metrics.precision_at_k, 0.5);
    assert_eq!(metrics.recall_at_k, 0.5);
    assert_eq!(metrics.mrr, 1.0);
    assert!(
        metrics.ndcg_at_k < 1.0,
        "ideal DCG must include the missing second gold unit"
    );
}

#[test]
fn retrieval_metrics_count_multiple_unique_units_at_one_rank() {
    let ranked = [RankedRetrieval {
        matched_gold_units: vec!["turn:D1:2".to_string(), "turn:D1:3".to_string()],
        score: 0.9,
    }];

    let metrics = retrieval_metrics(&ranked, 2, 1);

    assert_eq!(metrics.precision_at_k, 1.0);
    assert_eq!(metrics.recall_at_k, 1.0);
    assert_eq!(metrics.mrr, 1.0);
    assert_eq!(metrics.ndcg_at_k, 1.0);
}

#[test]
fn retrieval_ranking_sorts_across_context_buckets_by_score() {
    let mut package = ContextPackage::empty();
    package.identity = vec![fragment(1, KnowledgeType::IdentityCore, 0.1)];
    package.knowledge = vec![fragment(2, KnowledgeType::Semantic, 0.9)];
    package.memories = vec![fragment(3, KnowledgeType::Episodic, 0.8)];

    let node_ids: Vec<_> = ranked_fragments_for_test(&package)
        .into_iter()
        .map(|fragment| fragment.node_id.0)
        .collect();

    assert_eq!(node_ids, vec![2, 3, 1]);
}

#[test]
fn gold_evidence_prefers_exact_turns_and_dedupes_answer_sessions() {
    let locomo = GoldEvidence {
        answer_needles: vec!["pixel".to_string()],
        evidence_turn_ids: vec!["D1:2".to_string()],
        evidence_session_ids: vec!["session_1".to_string()],
        answer_session_ids: Vec::new(),
    };
    assert!(
        locomo
            .matched_units("session_1", Some("D1:2"), "unrelated text")
            .contains(&"turn:D1:2".to_string())
    );
    assert!(
        locomo
            .matched_units("session_1", Some("D1:1"), "Pixel appears here too")
            .is_empty(),
        "exact LoCoMo evidence turns must not inflate to the whole session"
    );

    let lme = GoldEvidence {
        answer_needles: vec!["business administration".to_string()],
        evidence_turn_ids: Vec::new(),
        evidence_session_ids: Vec::new(),
        answer_session_ids: vec!["answer_1".to_string()],
    };
    assert_eq!(
        lme.matched_units("answer_1", None, "first turn"),
        vec!["session:answer_1".to_string()]
    );
    assert_eq!(
        lme.matched_units("answer_1", None, "second turn"),
        vec!["session:answer_1".to_string()]
    );
}

fn fragment(id: u64, node_type: KnowledgeType, relevance: f64) -> Fragment {
    Fragment {
        node_id: NodeId(id),
        name: format!("node-{id}"),
        summary: None,
        content: None,
        node_type,
        relevance,
        origin: Origin::test_default(PeerId(1)),
        scope: ScopeRelation::Universal,
    }
}
