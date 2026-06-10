//! Default packaging must preserve the readout's bucket shape: episodic
//! memories that won readout must survive into the package
//! (readout-scoring.md "Bucket Handling").

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::{PackagingMode, SearchInput};
use anamnesis::{Engine, EngineConfig};

fn origin(session: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session.into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    }
}

#[test]
fn default_packaging_keeps_episodic_memories() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    engine
        .ingest(Observation {
            name: "episode".into(),
            summary: Some("what happened with the factory".into()),
            content: "the factory pattern broke in the deploy".into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![],
            origin: origin("s1"),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    // Plain query: no temporal keyword, no tensions, no persona.
    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert_eq!(
        result.trace.packaging_mode,
        Some(PackagingMode::Balanced),
        "plain queries must default to Balanced packaging"
    );
    assert!(
        !result.package.memories.is_empty(),
        "episodic memory that won readout must not be cleared by default packaging"
    );
}
