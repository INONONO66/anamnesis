use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig};
use anamnesis::{Engine, EngineConfig, IngestResult};

fn make_obs_typed(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type,
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

fn created_id(result: IngestResult) -> anamnesis::NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    }
}

#[test]
fn type_filtered_returns_only_matching_type() {
    let mut engine = Engine::new();

    engine
        .ingest(make_obs_typed("convention", KnowledgeType::Convention))
        .unwrap();
    engine
        .ingest(make_obs_typed("semantic", KnowledgeType::Semantic))
        .unwrap();

    let package = engine
        .query(
            &Query::TypeFiltered {
                node_type: KnowledgeType::Convention,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(package.knowledge.len(), 1);
    assert_eq!(package.knowledge[0].node_type, KnowledgeType::Convention);
}

#[test]
fn type_filtered_applies_limit() {
    let mut engine = Engine::new();

    engine
        .ingest(make_obs_typed("first", KnowledgeType::Gotcha))
        .unwrap();
    engine
        .ingest(make_obs_typed("second", KnowledgeType::Gotcha))
        .unwrap();

    let package = engine
        .query(
            &Query::TypeFiltered {
                node_type: KnowledgeType::Gotcha,
                limit: 1,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(package.knowledge.len(), 1);
}

#[test]
fn list_filters_by_min_salience() {
    let mut engine = Engine::new();

    engine
        .ingest(make_obs_typed("semantic", KnowledgeType::Semantic))
        .unwrap();

    let package = engine
        .query(
            &Query::List {
                min_salience: 0.5,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert!(!package.knowledge.is_empty());
}

#[test]
fn list_excludes_nodes_below_min_salience() {
    let mut engine = Engine::new();

    engine
        .ingest(make_obs_typed("semantic", KnowledgeType::Semantic))
        .unwrap();

    let package = engine
        .query(
            &Query::List {
                min_salience: 1.001,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(package.total_fragments(), 0);
}

#[test]
fn list_orders_by_salience_descending() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let first = created_id(
        engine
            .ingest(make_obs_typed("older", KnowledgeType::Semantic))
            .unwrap(),
    );
    engine
        .ingest(make_obs_typed("newer", KnowledgeType::Semantic))
        .unwrap();
    engine.tick(Timestamp(86_401_000)).unwrap();
    engine.touch(first, Timestamp(86_402_000)).unwrap();

    let package = engine
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 2,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(package.knowledge[0].node_id, first);
}
