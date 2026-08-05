use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anamnesis::engine::{
    EdgeType, EmbeddingProvider, KnowledgeType, Origin, PeerId, ScopePath, SourceKind,
    StorageAdapter, Timestamp,
};
use anamnesis::memory::{AtomicFactInput, SourceFragmentInput};
use anamnesis::{Error, Memory};

#[derive(Clone)]
struct CountingProvider {
    calls: Arc<AtomicUsize>,
    dimensions: usize,
}

impl EmbeddingProvider for CountingProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|text| {
                let seed = (text.len() as f32 + 1.0).recip();
                vec![seed; self.dimensions]
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_name(&self) -> &str {
        "source-fragment-test"
    }
}

fn provider() -> (Arc<dyn EmbeddingProvider>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(CountingProvider {
        calls: calls.clone(),
        dimensions: 4,
    });
    (provider, calls)
}

fn origin(session_id: &str) -> Origin {
    Origin {
        peer_id: PeerId(17),
        source_kind: SourceKind::DocumentExtract,
        session_id: session_id.to_owned(),
        scope: ScopePath::new("project/attachments").expect("valid scope"),
        confidence: 0.87,
    }
}

#[test]
fn source_fragment_is_one_exact_episodic_node_with_explicit_provenance() {
    let (provider, calls) = provider();
    let mut memory = Memory::in_memory_with_provider(provider).expect("memory");
    let content = "  OCR line one\nred and purple lighting  ";
    let expected_origin = origin("visual-session");

    let node_id = memory
        .add_source_fragment(
            SourceFragmentInput::new(content, expected_origin.clone(), Timestamp(700))
                .with_embedding(vec![0.1, 0.2, 0.3, 0.4])
                .with_entity_tags(vec![
                    " gaming room ".to_owned(),
                    "Nate".to_owned(),
                    "Nate".to_owned(),
                    " ".to_owned(),
                ])
                .with_validity(Some(Timestamp(600)), Some(Timestamp(900)))
                .with_metadata(vec![
                    ("attachment:sha256".to_owned(), "abc123".to_owned()),
                    ("processor:digest".to_owned(), "local-v1".to_owned()),
                ]),
        )
        .expect("source fragment");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "supplied embeddings are not regenerated"
    );
    assert_eq!(memory.engine().graph().node_count(), 1);
    assert_eq!(memory.engine().graph().edge_count(), 0);
    let node = memory
        .engine()
        .graph()
        .get_node(node_id)
        .expect("source node");
    assert_eq!(node.node_type, KnowledgeType::Episodic);
    assert_eq!(node.content, content, "evidence bytes must be preserved");
    assert_eq!(node.summary, None);
    assert_eq!(node.embedding.as_deref(), Some(&[0.1, 0.2, 0.3, 0.4][..]));
    assert_eq!(node.origin, expected_origin);
    assert_eq!(node.created_at, Timestamp(700));
    assert_eq!(node.valid_from, Some(Timestamp(600)));
    assert_eq!(node.valid_until, Some(Timestamp(900)));
    assert_eq!(node.entity_tags, vec!["Nate", "gaming room"]);
    assert_eq!(
        node.metadata.get("attachment:sha256").map(String::as_str),
        Some("abc123")
    );
    assert_eq!(
        node.metadata.get("processor:digest").map(String::as_str),
        Some("local-v1")
    );
}

#[test]
fn source_fragment_does_not_touch_pending_conversation_state() {
    let (provider, _) = provider();
    let mut memory = Memory::in_memory_with_provider(provider).expect("memory");

    let first = memory
        .add("chat", "Alice", "first turn", Timestamp(10))
        .expect("first turn");
    assert!(first.finalized_semantic.is_none());

    let source = memory
        .add_source_fragment(SourceFragmentInput::new(
            "standalone attachment transcript",
            origin("attachment-session"),
            Timestamp(15),
        ))
        .expect("source fragment");

    let second = memory
        .add("chat", "Bob", "second turn", Timestamp(20))
        .expect("second turn");
    assert!(
        second.finalized_semantic.is_some(),
        "the original first turn must remain pending across source admission"
    );

    let graph = memory.engine().graph();
    assert!(graph.edges_from(source).is_empty());
    assert!(graph.edges_to(source).is_empty());
    assert!(graph.edges_from(first.episodic).iter().any(|edge_id| {
        graph.get_edge(*edge_id).is_ok_and(|edge| {
            edge.target == second.episodic && edge.edge_type == EdgeType::Temporal
        })
    }));
}

#[test]
fn source_fragment_is_eligible_evidence_until_retracted() {
    let (provider, _) = provider();
    let mut memory = Memory::in_memory_with_provider(provider).expect("memory");
    let source = memory
        .add_source_fragment(
            SourceFragmentInput::new(
                "The room has red and purple lighting.",
                origin("visual-session"),
                Timestamp(100),
            )
            .with_embedding(vec![0.4, 0.3, 0.2, 0.1]),
        )
        .expect("source fragment");
    let fact_id = memory
        .add_atomic_fact(AtomicFactInput::new(
            "Nate's gaming room has red and purple lighting",
            vec![source],
        ))
        .expect("atomic fact");

    let fact = memory
        .engine()
        .graph()
        .storage()
        .get_atomic_fact(fact_id)
        .expect("stored fact")
        .clone();
    let source_node = memory
        .engine()
        .graph()
        .get_node(source)
        .expect("source")
        .clone();
    assert!(
        memory
            .engine()
            .graph()
            .storage()
            .atomic_fact_source_is_current(&fact, &source_node)
            .expect("incarnation check")
    );

    memory
        .engine_mut()
        .retract(source, "attachment withdrawn", Timestamp(110))
        .expect("retract");
    let retracted = memory
        .engine()
        .graph()
        .get_node(source)
        .expect("retracted source");
    assert!(
        memory
            .engine()
            .is_retracted(source)
            .expect("retraction state")
    );
    assert!(
        !memory
            .engine()
            .graph()
            .storage()
            .atomic_fact_source_is_current(&fact, retracted)
            .expect("stale incarnation check"),
        "retraction changes source authority and invalidates the fact binding"
    );
}

#[test]
fn source_fragment_rejects_invalid_inputs_without_writing() {
    let (provider, calls) = provider();
    let mut memory = Memory::in_memory_with_provider(provider).expect("memory");

    let mut blank_session = origin("valid");
    blank_session.session_id = "   ".to_owned();
    let mut padded_session = origin("valid");
    padded_session.session_id = " padded ".to_owned();
    let mut nan_confidence = origin("valid");
    nan_confidence.confidence = f64::NAN;
    let mut excessive_confidence = origin("valid");
    excessive_confidence.confidence = 1.01;

    let invalid = vec![
        SourceFragmentInput::new("   ", origin("valid"), Timestamp(1)),
        SourceFragmentInput::new("evidence", blank_session, Timestamp(1)),
        SourceFragmentInput::new("evidence", padded_session, Timestamp(1)),
        SourceFragmentInput::new("evidence", nan_confidence, Timestamp(1)),
        SourceFragmentInput::new("evidence", excessive_confidence, Timestamp(1)),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1))
            .with_embedding(Vec::new()),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1)).with_embedding(vec![
            0.0,
            f64::INFINITY,
            0.0,
            0.0,
        ]),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1))
            .with_embedding(vec![0.0, 0.0, 0.0]),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1))
            .with_validity(Some(Timestamp(10)), Some(Timestamp(10))),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1))
            .with_metadata(vec![(" ".to_owned(), "value".to_owned())]),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1)).with_metadata(vec![(
            "anamnesis:node-incarnation".to_owned(),
            "spoofed".to_owned(),
        )]),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1)).with_metadata(vec![(
            "anamnesis:source-incarnation:7".to_owned(),
            "spoofed".to_owned(),
        )]),
        SourceFragmentInput::new("evidence", origin("valid"), Timestamp(1)).with_metadata(vec![
            ("duplicate".to_owned(), "one".to_owned()),
            ("duplicate".to_owned(), "two".to_owned()),
        ]),
    ];

    for input in invalid {
        assert!(
            matches!(
                memory.add_source_fragment(input),
                Err(Error::InvalidInput(_))
            ),
            "invalid source input must be rejected"
        );
    }
    assert_eq!(memory.engine().graph().node_count(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn source_fragment_node_and_metadata_survive_reopen() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("source-fragment.sqlite");
    let (provider, _) = provider();
    let node_id;
    {
        let mut memory = Memory::with_provider(&path, provider.clone()).expect("memory");
        node_id = memory
            .add_source_fragment(
                SourceFragmentInput::new(
                    "immutable visual transcript",
                    origin("durable-session"),
                    Timestamp(42),
                )
                .with_metadata(vec![(
                    "attachment:sha256".to_owned(),
                    "durable-hash".to_owned(),
                )]),
            )
            .expect("source fragment");
        memory.flush_all().expect("flush");
    }

    let reopened = Memory::with_provider(&path, provider).expect("reopen");
    let node = reopened
        .engine()
        .graph()
        .get_node(node_id)
        .expect("durable source");
    assert_eq!(node.node_type, KnowledgeType::Episodic);
    assert_eq!(node.content, "immutable visual transcript");
    assert_eq!(
        node.metadata.get("attachment:sha256").map(String::as_str),
        Some("durable-hash")
    );
    assert!(
        !node.metadata.contains_key("anamnesis:node-incarnation"),
        "storage-owned generations must stay hidden from public metadata"
    );
}
