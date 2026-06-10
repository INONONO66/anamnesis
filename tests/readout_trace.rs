//! The readout trace must expose the ranked pre-packaging candidate list with
//! per-term score components (readout-scoring.md "Trace").

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

fn ingest(engine: &mut Engine, name: &str, content: &str, node_type: KnowledgeType) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: None,
            confidence: 0.9,
            node_type,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

fn engine_with(setup: impl FnOnce(&mut Engine)) -> Engine {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    setup(&mut engine);
    engine
}

#[test]
fn readout_trace_lists_ranked_candidates_with_components() {
    let engine = engine_with(|e| {
        ingest(e, "alpha", "alpha factory pattern handler", KnowledgeType::Semantic);
        ingest(e, "beta", "beta factory utility helper", KnowledgeType::Semantic);
        ingest(e, "gamma", "gamma unrelated text", KnowledgeType::Semantic);
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(!readout.is_empty(), "trace.readout must list scored candidates");

    // Ranked descending by score.
    for pair in readout.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "readout trace must be ranked: {} < {}",
            pair[0].score,
            pair[1].score
        );
    }

    // Components are finite.
    for candidate in readout {
        for (label, value) in [
            ("score", candidate.score),
            ("activation", candidate.activation),
            ("phi", candidate.phi),
            ("salience", candidate.salience),
            ("impedance", candidate.impedance),
            ("scope_weight", candidate.scope_weight),
            ("trust_weight", candidate.trust_weight),
            ("stress", candidate.stress),
        ] {
            assert!(value.is_finite(), "{label} must be finite, got {value}");
        }
    }
}

#[test]
fn readout_trace_is_a_superset_of_the_packaged_surface() {
    let engine = engine_with(|e| {
        for i in 0..8 {
            ingest(
                e,
                &format!("node-{i}"),
                &format!("factory variant {i} shared topic"),
                KnowledgeType::Semantic,
            );
        }
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 3,
            ..Default::default()
        })
        .expect("search must succeed");

    let packaged: Vec<_> = result
        .package
        .identity
        .iter()
        .chain(result.package.knowledge.iter())
        .chain(result.package.memories.iter())
        .map(|f| f.node_id)
        .collect();

    assert!(
        result.trace.readout.len() >= packaged.len(),
        "pre-package readout ({}) must not be smaller than the package ({})",
        result.trace.readout.len(),
        packaged.len()
    );
    for node_id in packaged {
        assert!(
            result.trace.readout.iter().any(|c| c.node_id == node_id),
            "packaged node {node_id:?} missing from readout trace"
        );
    }
}
