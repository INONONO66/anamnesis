#[path = "../benches/eval_common/mod.rs"]
mod eval_common;

use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::{Error, graph::EdgeType};
use serde_json::json;

use eval_common::real_bench::dataset::{
    BenchDatasetName, load_benchmark_dataset, parse_benchmark_dataset,
};
use eval_common::real_bench::graph::{
    build_memory_graph, evaluate_questions, run_warmup, speaker_cue_tags,
};

#[derive(Clone, Default)]
struct CountingEmbedder {
    calls: Arc<AtomicUsize>,
}

impl CountingEmbedder {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl EmbeddingProvider for CountingEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        self.calls.fetch_add(texts.len(), Ordering::Relaxed);
        Ok(texts.iter().map(|text| embed_text(text)).collect())
    }

    fn dimensions(&self) -> usize {
        4
    }

    fn model_name(&self) -> &str {
        "counting-test-embedder"
    }
}

#[test]
fn locomo_loader_preserves_evidence_turn_ids() {
    let loaded = parse_benchmark_dataset(
        BenchDatasetName::Locomo,
        &json!([{
            "session_1": [
                {"dia_id": "D1:1", "speaker": "Caroline", "text": "Opening"},
                {"dia_id": "D1:2", "speaker": "Melanie", "text": "Caroline adopted a corgi named Pixel."},
                {"dia_id": "D1:3", "speaker": "Caroline", "text": "Pixel joined agility class."}
            ],
            "qa": [{
                "question": "What dog did Caroline adopt?",
                "answer": "Pixel",
                "category": 1,
                "evidence": ["D1:2; D1:3"]
            }]
        }]),
        Some(1),
    )
    .expect("LoCoMo JSON should parse");

    assert_eq!(loaded.sessions.len(), 1);
    assert_eq!(loaded.questions.len(), 1);
    assert_eq!(
        loaded.questions[0].gold.evidence_turn_ids,
        vec!["D1:2".to_string(), "D1:3".to_string()]
    );
    assert_eq!(
        loaded.sessions[0].turns[1].raw_turn_id.as_deref(),
        Some("D1:2")
    );
    assert_eq!(
        loaded.questions[0]
            .gold
            .matched_units("session_1", Some("D1:3"), "ignored"),
        vec!["turn:D1:3".to_string()]
    );
    assert_eq!(loaded.questions[0].gold.answer_needles, vec!["pixel"]);
}

#[test]
fn longmemeval_loader_preserves_answer_session_ids() {
    let loaded = parse_benchmark_dataset(
        BenchDatasetName::LongMemEval,
        &json!([{
            "question_id": "q1",
            "question": "What degree did I graduate with?",
            "answer": "Business Administration",
            "question_type": "single-session-user",
            "haystack_session_ids": ["distractor_1", "answer_1"],
            "answer_session_ids": ["answer_1"],
            "haystack_sessions": [
                [{"role": "user", "content": "A distractor conversation."}],
                [{"role": "user", "content": "I graduated with Business Administration."}]
            ]
        }]),
        Some(1),
    )
    .expect("LongMemEval JSON should parse");

    assert_eq!(loaded.sessions.len(), 2);
    assert_eq!(
        loaded.sessions[1].raw_session_id, "answer_1",
        "raw haystack ids must be preserved for answer_session scoring"
    );
    assert_eq!(
        loaded.questions[0].gold.answer_session_ids,
        vec!["answer_1".to_string()]
    );
    assert_eq!(
        loaded.questions[0].gold.answer_needles,
        vec!["business administration"]
    );
}

#[test]
fn graph_build_warmup_and_evaluation_use_embeddings_and_commit() {
    let loaded = parse_benchmark_dataset(
        BenchDatasetName::Locomo,
        &json!([{
            "session_1": [
                {"dia_id": "D1:1", "speaker": "Caroline", "text": "Caroline adopted a corgi named Pixel."},
                {"dia_id": "D1:2", "speaker": "Melanie", "text": "Pixel likes agility practice."}
            ],
            "qa": [{
                "question": "What is Caroline's corgi named?",
                "answer": "Pixel",
                "category": 1,
                "evidence": ["D1:1"]
            }, {
                "question": "What practice does Pixel like?",
                "answer": "agility practice",
                "category": 1,
                "evidence": ["D1:2"]
            }]
        }]),
        Some(1),
    )
    .expect("LoCoMo JSON should parse");

    let embedder = CountingEmbedder::default();
    let mut built = build_memory_graph(&loaded, &embedder).expect("graph builds");

    assert_eq!(built.stats.nodes_created, 4);
    assert_eq!(built.stats.extracted_edges_created, 2);
    assert_eq!(built.engine.graph().node_count(), 4);
    assert_eq!(
        built.stats.embedded_texts, 4,
        "each turn embeds a speaker-prefixed view and a context-window view"
    );
    assert_eq!(embedder.calls(), 4);

    // Episodic nodes embed/index the speaker-prefixed turn; semantic nodes
    // embed/index the +/-1-turn context window. Provenance keeps the raw text.
    let mut episodic_contents = Vec::new();
    let mut semantic_contents = Vec::new();
    for (&node_id, provenance) in &built.provenance_by_node {
        let node = built.engine.graph().get_node(node_id).expect("node exists");
        assert!(
            !provenance.content.contains(':'),
            "provenance content must stay the raw turn text"
        );
        match &node.node_type {
            anamnesis::graph::KnowledgeType::Episodic => {
                episodic_contents.push(node.content.clone())
            }
            anamnesis::graph::KnowledgeType::Semantic => {
                semantic_contents.push(node.content.clone())
            }
            other => panic!("unexpected node type {other:?}"),
        }
    }
    episodic_contents.sort();
    semantic_contents.sort();
    assert_eq!(
        episodic_contents,
        vec![
            "Caroline: Caroline adopted a corgi named Pixel.".to_string(),
            "Melanie: Pixel likes agility practice.".to_string(),
        ]
    );
    assert_eq!(
        semantic_contents,
        vec![
            "Caroline: Caroline adopted a corgi named Pixel.\nMelanie: Pixel likes agility practice."
                .to_string(),
            "Caroline: Caroline adopted a corgi named Pixel.\nMelanie: Pixel likes agility practice."
                .to_string(),
        ]
    );

    // Ingest timestamps are anchored to the wall clock so Pavlik-Anderson decay
    // sees realistic ages: sessions a day apart, turns a minute apart, all in
    // the recent past.
    let now = anamnesis::graph::Timestamp::now().0;
    for &node_id in built.provenance_by_node.keys() {
        let node = built.engine.graph().get_node(node_id).expect("node exists");
        assert!(
            node.created_at.0 <= now,
            "ingest timestamps must be in the past"
        );
        assert!(
            now - node.created_at.0 <= 30 * 86_400,
            "ingest timestamps must be recent (got age {}s)",
            now - node.created_at.0
        );
    }

    let temporal_edges = built
        .engine
        .graph()
        .all_edge_ids()
        .into_iter()
        .filter(|edge_id| {
            built
                .engine
                .graph()
                .get_edge(*edge_id)
                .is_ok_and(|edge| edge.edge_type == EdgeType::Temporal)
        })
        .count();
    assert_eq!(temporal_edges, 1);

    let warmup = run_warmup(&mut built, &loaded.questions[..1], &embedder, 3, None)
        .expect("warmup commits search packages");
    assert_eq!(warmup.questions, 1);
    assert!(warmup.sites_accessed > 0);

    let evaluated = evaluate_questions(&built, &loaded.questions[1..], &embedder, 3, None)
        .expect("held-out retrieval evaluates");
    assert_eq!(evaluated.len(), 1);
    assert_eq!(evaluated[0].question_id, loaded.questions[1].question_id);
    // Balanced packaging preserves both knowledge and memory fragments in the
    // package, so all top-k slots are filled (4 nodes, top_k=3 → 3 retrievals).
    assert_eq!(evaluated[0].retrievals.len(), 3);
    assert!(
        evaluated[0].retrieval_metrics.mrr > 0.0,
        "the relevant evidence turn should be recovered in the top-k"
    );
    assert_eq!(
        embedder.calls(),
        6,
        "2 speaker views + 2 window views + 1 warmup query + 1 eval query"
    );

    let unique_nodes: HashSet<_> = built.provenance_by_node.keys().copied().collect();
    assert_eq!(unique_nodes.len(), 4);
}

#[test]
fn split_by_sample_separates_conversations() {
    let loaded = parse_benchmark_dataset(
        BenchDatasetName::Locomo,
        &json!([
            {
                "session_1": [{"dia_id": "D1:1", "speaker": "A", "text": "conv zero turn"}],
                "qa": [{"question": "q0?", "answer": "a0", "category": 1, "evidence": ["D1:1"]}]
            },
            {
                "session_1": [{"dia_id": "D1:1", "speaker": "B", "text": "conv one turn"}],
                "qa": [
                    {"question": "q1?", "answer": "a1", "category": 1, "evidence": ["D1:1"]},
                    {"question": "q2?", "answer": "a2", "category": 1, "evidence": ["D1:1"]}
                ]
            }
        ]),
        None,
    )
    .expect("LoCoMo JSON should parse");

    let groups = eval_common::real_bench::dataset::split_by_sample(loaded);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].questions.len(), 1);
    assert_eq!(groups[0].sessions.len(), 1);
    assert_eq!(groups[0].questions[0].sample_index, 0);
    assert_eq!(groups[1].questions.len(), 2);
    assert_eq!(groups[1].sessions.len(), 1);
    assert!(
        groups[1]
            .sessions
            .iter()
            .all(|session| session.sample_index == 1),
        "each group must only contain its own conversation's sessions"
    );
}

#[test]
fn missing_dataset_path_returns_clear_error() {
    let err = load_benchmark_dataset(
        BenchDatasetName::LongMemEval,
        std::path::Path::new("/definitely/missing/anamnesis-data"),
        None,
    )
    .expect_err("missing dataset should fail");

    assert!(err.to_string().contains("Dataset not found"));
}

#[test]
fn speaker_cue_tags_match_question_mentions() {
    let speakers = vec!["Caroline".to_string(), "Melanie".to_string(), "user".to_string()];
    let tags = speaker_cue_tags(&speakers, "What did Caroline say about the trip?");
    assert_eq!(tags, vec!["speaker-caroline".to_string()]);
    // Generic roles never become cues.
    assert!(speaker_cue_tags(&speakers, "what did the user say").is_empty());
}

#[test]
fn speaker_cue_tags_require_whole_token_matches() {
    let speakers = vec!["Tim".to_string(), "Sam".to_string(), "Nate".to_string()];
    // Substrings inside words must never fire ("times", "same", "donate").
    assert!(speaker_cue_tags(&speakers, "How many times did they meet?").is_empty());
    assert!(speaker_cue_tags(&speakers, "Did they order the same dish?").is_empty());
    assert!(speaker_cue_tags(&speakers, "Did Melanie donate to charity?").is_empty());
    // Whole-token mentions still match, including with punctuation, and the
    // output preserves the input speaker order.
    assert_eq!(
        speaker_cue_tags(&speakers, "What did Tim, not Sam, decide?"),
        vec!["speaker-tim".to_string(), "speaker-sam".to_string()]
    );
}

#[test]
fn speaker_cue_tags_handle_multi_word_and_short_names() {
    let speakers = vec!["Mary Jane".to_string(), "Jo".to_string()];
    // Multi-word names round-trip through the same normalize_tag as ingest.
    assert_eq!(
        speaker_cue_tags(&speakers, "Where did Mary Jane travel last summer?"),
        vec!["speaker-mary-jane".to_string()]
    );
    // Names shorter than 3 chars are skipped even when mentioned.
    assert!(speaker_cue_tags(&speakers, "What does Jo think?").is_empty());
}

fn embed_text(text: &str) -> Vec<f32> {
    let normalized = text.to_lowercase();
    [
        normalized.matches("pixel").count() as f32,
        normalized.matches("agility").count() as f32,
        normalized.matches("business").count() as f32,
        1.0,
    ]
    .to_vec()
}
