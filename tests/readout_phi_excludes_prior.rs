//! Readout `phi_i` must carry only query-alignment terms. The prior `A_i`
//! reaches the readout score through `logit(s_i)` (`s_i = logistic(A_i)`) and
//! the tie-breaker (readout-scoring.md lists `A_i` as "read input and
//! tie-breaker", not a scored term); folding it into phi double-counts the
//! same reservoir and lets encoding-time priors dominate query alignment.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{ConfidenceLevel, Engine, EngineConfig};

fn origin(session: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session.into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    }
}

fn ingest(engine: &mut Engine, name: &str, content: &str) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: None,
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
fn committed_prior_does_not_enter_readout_phi() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    ingest(&mut engine, "first", "factory pattern incident report");
    ingest(&mut engine, "second", "factory pattern incident report");

    // Boost one node's retained action through the explicit commit path
    // (limit=1 packages only the top node, so only it is strengthened).
    for _ in 0..3 {
        let result = engine
            .search(SearchInput {
                text: "factory".into(),
                limit: 1,
                ..Default::default()
            })
            .expect("search must succeed");
        engine
            .commit(result.package, Some(ConfidenceLevel::High))
            .expect("commit must succeed");
    }

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(readout.len() >= 2, "both nodes must be scored");
    let phi_spread = (readout[0].phi - readout[1].phi).abs();
    assert!(
        phi_spread < 1e-9,
        "equal-alignment nodes must have equal readout phi regardless of \
         committed prior; got spread {phi_spread} ({} vs {})",
        readout[0].phi,
        readout[1].phi
    );
}
