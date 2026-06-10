//! Readout `phi_i` must be the full query-field potential
//! (potential-landscape.md), not embedding cosine alone: a text-matched seed
//! must out-rank an embedding-identical non-matched node.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;
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

fn ingest(engine: &mut Engine, name: &str, content: &str, embedding: Vec<f64>) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: Some(embedding),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

#[test]
fn text_match_is_credited_in_readout_phi() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    // Identical embeddings: cosine ties. Only "ownership" text differs.
    let shared = vec![1.0, 0.0, 0.0, 0.0];
    ingest(
        &mut engine,
        "matched",
        "rust ownership semantics",
        shared.clone(),
    );
    ingest(
        &mut engine,
        "unmatched",
        "completely different topic",
        shared.clone(),
    );

    let result = engine
        .search(SearchInput {
            text: "ownership".into(),
            query_embedding: Some(shared),
            limit: 5,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(
        readout.len() >= 2,
        "both nodes must be scored, got {}",
        readout.len()
    );

    // The top-ranked candidate must be the text-matched node, and its phi must
    // strictly exceed the embedding-identical unmatched node's phi.
    let top_name = result
        .package
        .knowledge
        .first()
        .map(|f| f.name.clone())
        .unwrap_or_default();
    assert_eq!(top_name, "matched", "text-matched node must rank first");

    let phis: Vec<f64> = readout.iter().map(|c| c.phi).collect();
    assert!(
        phis[0] > phis[1],
        "text match must be credited in readout phi: {phis:?}"
    );
}
