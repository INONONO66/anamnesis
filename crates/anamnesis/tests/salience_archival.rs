//! #4 salience recalibration: `SURPRISE_GAIN_K = 12.0` (decoupled from the cold-
//! start ceiling `INITIAL_RETAINED_ACTION = 13.8`) so ordinary captured turns can
//! actually reach the archive floor as they age.
//!
//! With the prior `k = 13.8`, a typical distinct turn was seeded with a decay-
//! exempt prior `P_i ≈ 8.28`, so `salience = logistic(B_i + P_i)` could not cross
//! the 0.10 archive floor for years — "old, unused memories sink" was inert for the
//! capture path. With `k = 12.0` the same turn enters at `P_i ≈ 7.2` and archives
//! after a year of disuse, while a maximal-surprise / cold-start memory
//! (`P_i ≈ 13.8`) persists. This test is RED under the prior `k = 13.8`.

use anamnesis::Engine;
use anamnesis::api::{GraphEvent, Observation};
use anamnesis::engine::{IngestResult, NodeId, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::mechanics::priors::{
    ENCODER_DISTINCT_PAIR_Q95, INITIAL_RETAINED_ACTION, SURPRISE_GAIN_K,
};

fn obs(name: &str, at: Timestamp) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Episodic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: at,
        valid_from: None,
        valid_until: None,
    }
}

fn created_id(result: IngestResult) -> NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    }
}

#[test]
fn recalibrated_prior_lets_a_typical_turn_archive_but_a_high_prior_persists() {
    let mut engine = Engine::new();
    let typical = created_id(
        engine
            .ingest(obs("typical", Timestamp(0)))
            .expect("ingest typical"),
    );
    let anchor = created_id(
        engine
            .ingest(obs("anchor", Timestamp(0)))
            .expect("ingest anchor"),
    );

    // Seed the decay-exempt prior a typical distinct turn (eps = 2(1 - q95)) would
    // get under the current k, and leave the anchor at the cold-start ceiling.
    let eps = 2.0 * (1.0 - ENCODER_DISTINCT_PAIR_Q95);
    let typical_prior = SURPRISE_GAIN_K * eps;
    engine
        .graph_mut()
        .storage_mut()
        .set_evidence_prior(typical, typical_prior)
        .expect("seed typical prior");
    engine
        .graph_mut()
        .storage_mut()
        .set_evidence_prior(anchor, INITIAL_RETAINED_ACTION)
        .expect("seed anchor prior");
    engine.drain_events();

    // One year of base-level forgetting for an Episodic single creation trace:
    // B_i ≈ -m_type·α·ln(Δt_ms) = -0.40·ln(3.1536e10) ≈ -9.67.
    engine.tick(Timestamp(31_536_000_000)).expect("tick a year");
    let events = engine.drain_events();

    let typical_salience = engine
        .graph()
        .storage()
        .get_salience(typical)
        .expect("typical salience");
    assert!(
        typical_salience < 0.10,
        "a typical turn (P_i ≈ {typical_prior:.2}) must archive (salience < 0.10) after a \
         year of disuse; got {typical_salience:.4} — RED under the prior k = 13.8 (P_i ≈ 8.28)"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GraphEvent::NodeArchived { node_id } if *node_id == typical)),
        "NodeArchived must fire for the typical turn"
    );

    let anchor_salience = engine
        .graph()
        .storage()
        .get_salience(anchor)
        .expect("anchor salience");
    assert!(
        anchor_salience > 0.5,
        "a maximal-surprise / cold-start memory (P_i = {INITIAL_RETAINED_ACTION}) must persist, \
         not archive; got {anchor_salience:.4}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GraphEvent::NodeArchived { node_id } if *node_id == anchor)),
        "the high-prior anchor must not archive"
    );
}
