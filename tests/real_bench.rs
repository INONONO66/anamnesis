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
    BenchDatasetName, load_benchmark_dataset, parse_benchmark_dataset, stratify_questions,
};
use eval_common::real_bench::graph::{
    EvalOptions, build_memory_graph, evaluate_questions, run_warmup, speaker_cue_tags,
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
            "session_1_date_time": "1:56 pm on 8 May, 2023",
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
    // 2023-05-08 13:56:00 UTC = 1683504000 + 13*3600 + 56*60
    assert_eq!(
        loaded.sessions[0].start_timestamp,
        Some(1_683_504_000 + 13 * 3600 + 56 * 60),
        "session_1_date_time must be parsed to epoch seconds"
    );
    assert_eq!(
        loaded.questions[0].question_date,
        Some(1_683_554_160 + 86_400),
        "LoCoMo question_date must be set to max(session start) + 1 day when session is dated"
    );
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
            "question_date": "2023/05/30 (Tue) 23:59",
            "haystack_session_ids": ["distractor_1", "answer_1"],
            "haystack_dates": ["2023/05/20 (Sat) 02:21", "2023/05/25 (Thu) 10:00"],
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
    // 2023-05-20 02:21 UTC = 1684540800 + 2*3600 + 21*60
    assert_eq!(
        loaded.sessions[0].start_timestamp,
        Some(1_684_540_800 + 2 * 3600 + 21 * 60),
        "first haystack_dates entry must be parsed to epoch seconds"
    );
    // 2023-05-25 10:00 UTC = 1684972800 + 10*3600
    assert_eq!(
        loaded.sessions[1].start_timestamp,
        Some(1_684_972_800 + 10 * 3600),
        "second haystack_dates entry must be parsed to epoch seconds"
    );
    // 2023-05-30 23:59 UTC = 1685404800 + 23*3600 + 59*60
    assert_eq!(
        loaded.questions[0].question_date,
        Some(1_685_404_800 + 23 * 3600 + 59 * 60),
        "question_date must be parsed to epoch seconds"
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
    let mut built = build_memory_graph(&loaded, &embedder, None).expect("graph builds");

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

    let warmup = run_warmup(
        &mut built,
        &loaded.questions[..1],
        &embedder,
        &EvalOptions {
            top_k: 3,
            ..Default::default()
        },
    )
    .expect("warmup commits search packages");
    assert_eq!(warmup.questions, 1);
    assert!(warmup.sites_accessed > 0);

    let evaluated = evaluate_questions(
        &built,
        &loaded.questions[1..],
        &embedder,
        &EvalOptions {
            top_k: 3,
            ..Default::default()
        },
    )
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
    let speakers = vec![
        "Caroline".to_string(),
        "Melanie".to_string(),
        "user".to_string(),
    ];
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

#[test]
fn stratify_questions_caps_per_type_and_preserves_order() {
    // Build a Vec<BenchQuestion> with three types: A×3, B×2, C×1.
    // After stratify(2) we expect A×2, B×2, C×1 (5 total), in original order.
    use eval_common::real_bench::dataset::{BenchQuestion, GoldEvidence};

    fn make_question(id: &str, question_type: &str, sample_index: usize) -> BenchQuestion {
        BenchQuestion {
            question_id: id.to_string(),
            question: id.to_string(),
            expected_answer: String::new(),
            question_type: question_type.to_string(),
            sample_index,
            session_ids: Vec::new(),
            gold: GoldEvidence::default(),
            question_date: None,
        }
    }

    let mut questions = vec![
        make_question("a1", "A", 0),
        make_question("b1", "B", 1),
        make_question("a2", "A", 2),
        make_question("c1", "C", 3),
        make_question("b2", "B", 4),
        make_question("a3", "A", 5),
    ];

    stratify_questions(&mut questions, 2);

    // Exactly 5 survivors
    assert_eq!(questions.len(), 5);
    // a3 (third A) must be dropped
    assert!(!questions.iter().any(|q| q.question_id == "a3"));
    // All others must be present
    for id in ["a1", "b1", "a2", "c1", "b2"] {
        assert!(
            questions.iter().any(|q| q.question_id == id),
            "question {id} must survive"
        );
    }
    // Original order must be preserved
    let ids: Vec<&str> = questions.iter().map(|q| q.question_id.as_str()).collect();
    assert_eq!(ids, vec!["a1", "b1", "a2", "c1", "b2"]);

    // At most 2 of each type
    let a_count = questions.iter().filter(|q| q.question_type == "A").count();
    let b_count = questions.iter().filter(|q| q.question_type == "B").count();
    let c_count = questions.iter().filter(|q| q.question_type == "C").count();
    assert_eq!(a_count, 2);
    assert_eq!(b_count, 2);
    assert_eq!(c_count, 1);
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

#[test]
fn embed_cache_second_build_makes_zero_provider_calls() {
    use eval_common::real_bench::embed_cache::EmbedCache;

    let loaded = parse_benchmark_dataset(
        BenchDatasetName::Locomo,
        &json!([{
            "session_1": [
                {"dia_id": "D1:1", "speaker": "Alice", "text": "Alice likes hiking."},
                {"dia_id": "D1:2", "speaker": "Bob", "text": "Bob prefers swimming."}
            ],
            "qa": [{
                "question": "What does Alice like?",
                "answer": "hiking",
                "category": 1,
                "evidence": ["D1:1"]
            }]
        }]),
        Some(1),
    )
    .expect("LoCoMo JSON should parse");

    let cache_dir =
        std::env::temp_dir().join(format!("embed-cache-hit-test-{}", std::process::id()));
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache_path = cache_dir.join("cache.sqlite");

    // First build — populates the cache.
    let embedder1 = CountingEmbedder::default();
    let cache1 = EmbedCache::open(&cache_path, embedder1.model_name()).unwrap();
    build_memory_graph(&loaded, &embedder1, Some(&cache1)).expect("first graph builds");
    let calls_after_first_build = embedder1.calls();
    assert!(
        calls_after_first_build > 0,
        "first build must call provider"
    );

    // Second build — all embeddings are already cached, provider must not be called.
    let embedder2 = CountingEmbedder::default();
    let cache2 = EmbedCache::open(&cache_path, embedder2.model_name()).unwrap();
    build_memory_graph(&loaded, &embedder2, Some(&cache2)).expect("second graph builds");
    assert_eq!(
        embedder2.calls(),
        0,
        "second build with warm cache must make 0 provider calls"
    );

    std::fs::remove_dir_all(cache_dir).ok();
}

#[test]
fn dump_features_populates_matched_units_and_total_relevant() {
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
            }]
        }]),
        Some(1),
    )
    .expect("LoCoMo JSON should parse");

    let embedder = CountingEmbedder::default();
    let built = build_memory_graph(&loaded, &embedder, None).expect("graph builds");

    let evaluated = evaluate_questions(
        &built,
        &loaded.questions,
        &embedder,
        &EvalOptions {
            top_k: 10,
            dump_features: true,
            ..Default::default()
        },
    )
    .expect("evaluation with dump_features succeeds");

    assert_eq!(evaluated.len(), 1);
    let eval = &evaluated[0];
    assert!(
        !eval.features.is_empty(),
        "dump_features=true must produce feature rows"
    );
    for row in &eval.features {
        // total_relevant must be non-zero (one evidence turn exists)
        assert!(
            row.total_relevant > 0,
            "total_relevant must be set on every row"
        );
        // label must be consistent with matched_units
        assert_eq!(
            row.label,
            !row.matched_units.is_empty(),
            "label must equal !matched_units.is_empty() for row at rank {}",
            row.rank
        );
    }
    // At least one row must have a non-empty matched_units (the evidence turn)
    assert!(
        eval.features
            .iter()
            .any(|row| !row.matched_units.is_empty()),
        "at least one feature row must match the evidence turn"
    );
}
