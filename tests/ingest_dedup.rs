use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::{Engine, EngineConfig, IngestResult};

fn make_obs(name: &str, embedding: Vec<f64>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: Some(embedding),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

#[test]
fn duplicate_ingest_reinforces_instead_of_creating() {
    let config = EngineConfig::new()
        .with_dedup_threshold(0.92)
        .with_dedup_enabled(true);
    let mut e = Engine::with_config(config);
    let r1 = e.ingest(make_obs("a", vec![1.0, 0.0])).unwrap();
    let id1 = match r1 {
        IngestResult::Created(ref ids) => ids[0],
        _ => panic!("expected Created"),
    };

    let mut obs_dup = make_obs("a-dup", vec![1.0, 0.0]);
    obs_dup.timestamp = Timestamp(2000);
    let r2 = e.ingest(obs_dup).unwrap();
    match r2 {
        IngestResult::Reinforced {
            existing_id,
            similarity,
        } => {
            assert_eq!(existing_id, id1);
            assert!(
                similarity > 0.92,
                "similarity should be > 0.92, got {similarity}"
            );
        }
        _ => panic!("expected Reinforced, got {r2:?}"),
    }

    assert_eq!(e.graph().node_count(), 1);
}

#[test]
fn different_embedding_creates_new_node() {
    let mut e = Engine::new();
    e.ingest(make_obs("a", vec![1.0, 0.0])).unwrap();
    let r = e.ingest(make_obs("b", vec![0.0, 1.0])).unwrap();
    assert!(matches!(r, IngestResult::Created(_)));
    assert_eq!(e.graph().node_count(), 2);
}

#[test]
fn dedup_disabled_creates_new_node() {
    let config = EngineConfig::new()
        .with_dedup_enabled(false)
        .with_novelty_threshold(0.0);
    let mut e = Engine::with_config(config);
    e.ingest(make_obs("a", vec![1.0, 0.0])).unwrap();
    let r = e.ingest(make_obs("a-dup", vec![1.0, 0.0])).unwrap();
    assert!(matches!(r, IngestResult::Created(_)));
}
