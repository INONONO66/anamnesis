//! Explicit date cues in the query must bias retrieval toward sites whose
//! timestamps match — a query-local potential term (potential-landscape.md),
//! never a persistent mutation.

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

fn ingest_at(engine: &mut Engine, name: &str, content: &str, timestamp: u64) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(timestamp),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

const MAY_8_2023: u64 = 1_683_504_000; // 2023-05-08 00:00 UTC
const DEC_1_2023: u64 = 1_701_388_800; // 2023-12-01 00:00 UTC

#[test]
fn date_cue_prefers_matching_timestamp() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    // Same lexical content; only the timestamp differs.
    ingest_at(
        &mut engine,
        "on-date",
        "beach trip planning notes",
        MAY_8_2023,
    );
    ingest_at(
        &mut engine,
        "off-date",
        "beach trip planning notes",
        DEC_1_2023,
    );

    let result = engine
        .search(SearchInput {
            text: "beach trip on 8 May 2023".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let phis: Vec<f64> = result.trace.readout.iter().map(|c| c.phi).collect();
    assert!(phis.len() >= 2, "both sites must be scored, got {phis:?}");
    assert!(
        phis[0] > phis[1],
        "temporal proximity must separate equal-content sites: {phis:?}"
    );
    // And the winner must actually be the on-date node.
    let top = &result.trace.readout[0];
    let top_node = engine
        .graph()
        .get_node(top.node_id)
        .expect("node must exist");
    assert_eq!(top_node.created_at.0, MAY_8_2023);
}

#[test]
fn no_cue_means_no_temporal_separation() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    ingest_at(&mut engine, "a", "beach trip planning notes", MAY_8_2023);
    ingest_at(&mut engine, "b", "beach trip planning notes", DEC_1_2023);

    let result = engine
        .search(SearchInput {
            text: "beach trip".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");
    let phis: Vec<f64> = result.trace.readout.iter().map(|c| c.phi).collect();
    assert!(
        (phis[0] - phis[1]).abs() < 1e-9,
        "without a time cue the temporal term must be inert: {phis:?}"
    );
}
