//! Integration tests for the Phase 1 skeleton.
//!
//! These tests verify the full Engine lifecycle:
//! ingest → link → touch → tick → query

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig};

fn make_observation(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("Summary of {}", name)),
        content: format!("Full content of {}", name),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec!["test-entity".to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::new("anamnesis").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

#[test]
fn engine_full_lifecycle() {
    let mut engine = Engine::new();

    // 1. Ingest two observations
    let IngestResult::Created(ids1) = engine
        .ingest(make_observation(
            "auth uses factory pattern",
            KnowledgeType::Convention,
        ))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = engine
        .ingest(make_observation(
            "race condition in auth middleware",
            KnowledgeType::Episodic,
        ))
        .unwrap()
    else {
        panic!("expected Created");
    };
    assert_eq!(ids1.len(), 1);
    assert_eq!(ids2.len(), 1);
    assert_eq!(engine.graph().node_count(), 2);

    // 2. Link the nodes
    let eid = engine.link(ids1[0], ids2[0], EdgeType::Semantic).unwrap();
    assert_eq!(engine.graph().edge_count(), 1);
    let edge = engine.graph().get_edge(eid).unwrap();
    // link seeds conductance from the cold-start coupling; weight is its bounded
    // projection (ADR-0002), not a caller-supplied value.
    assert!(edge.conductance.is_finite());
    assert!((0.0..=1.0).contains(&edge.weight));

    // 3. Touch a node at the tick time so its decay-checkpoint dt is zero.
    let tick_time = Timestamp(1000 + 30 * 86_400_000); // 30 days after ingest
    engine.touch(ids1[0], tick_time).unwrap();
    engine.touch(ids1[0], tick_time).unwrap();
    let node = engine.graph().get_node(ids1[0]).unwrap();
    assert_eq!(node.access_count, 2);

    // 4. Tick 30 days out: ids2[0] (Episodic, created at t=1000) decays detectably;
    //    ids1[0] was just touched at the tick time so its dt is zero and it does not.
    //    Salience is the projection of the retained-action reservoir (ADR-0009), so a
    //    meaningful elapsed interval is needed for the projection to move past the
    //    salience-change epsilon.
    let report = engine.tick(tick_time).unwrap();
    assert_eq!(report.nodes_decayed, 1);

    // 5. Query — Associative returns real results in Phase 2
    let q = Query::Associative {
        seed: ids1[0],
        budget: 100,
    };
    let pkg = engine.query(&q, &QueryConfig::default()).unwrap();
    assert!(
        pkg.total_fragments() > 0,
        "Associative query should return results"
    );
}

#[test]
fn engine_with_config() {
    let config = EngineConfig::new()
        .with_max_nodes(1000)
        .with_novelty_threshold(0.4)
        .with_confidence_threshold(0.6);
    let mut engine = Engine::with_config(config);

    let IngestResult::Created(ids) = engine
        .ingest(make_observation("test", KnowledgeType::Semantic))
        .unwrap()
    else {
        panic!("expected Created");
    };
    assert_eq!(ids.len(), 1);
}

#[test]
fn node_fields_preserved_after_ingest() {
    let mut engine = Engine::new();
    let obs = Observation {
        name: "physics = edge weight dynamics".to_string(),
        summary: Some("Force-directed simulation rejected".to_string()),
        content: "Full discussion...".to_string(),
        embedding: Some(vec![0.7, 0.3, 0.1]),
        confidence: 0.95,
        node_type: KnowledgeType::Decision,
        entity_tags: vec!["physics".to_string(), "anamnesis".to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "design-session".to_string(),
            scope: anamnesis::graph::ScopePath::new("anamnesis").expect("valid scope"),
            confidence: 0.95,
        },
        timestamp: Timestamp(5000),
        valid_from: None,
        valid_until: None,
    };

    let IngestResult::Created(ids) = engine.ingest(obs).unwrap() else {
        panic!("expected Created");
    };
    let node = engine.graph().get_node(ids[0]).unwrap();

    assert_eq!(node.name, "physics = edge weight dynamics");
    assert_eq!(
        node.summary.as_deref(),
        Some("Force-directed simulation rejected")
    );
    assert_eq!(node.node_type, KnowledgeType::Decision);
    assert_eq!(node.entity_tags, vec!["physics", "anamnesis"]);
    assert_eq!(node.origin.scope.as_str(), "anamnesis");
    // Salience is the projection of the surprise-gated retained-action reservoir
    // (ADR-0009), not a flat 1.0. The first ingested node has no prediction to be
    // surprising against, so it enters near the prior ceiling.
    assert!(
        node.salience > 0.999 && node.salience <= 1.0,
        "salience should be a near-ceiling projection, got {}",
        node.salience
    );
    assert_eq!(node.access_count, 0);
    assert!(node.embedding.is_some());
}

#[test]
fn multiple_edge_types() {
    let mut engine = Engine::new();
    let IngestResult::Created(ids1) = engine
        .ingest(make_observation("decision", KnowledgeType::Decision))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = engine
        .ingest(make_observation("reason", KnowledgeType::Semantic))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids3) = engine
        .ingest(make_observation("rejected", KnowledgeType::Semantic))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let id1 = ids1[0];
    let id2 = ids2[0];
    let id3 = ids3[0];

    engine.link(id1, id2, EdgeType::Reason).unwrap();
    engine
        .link(id1, id3, EdgeType::RejectedAlternative)
        .unwrap();

    assert_eq!(engine.graph().edge_count(), 2);
    assert_eq!(engine.graph().edges_from(id1).len(), 2);
}

#[test]
fn query_all_modes_compile() {
    let mut engine = Engine::new();
    let IngestResult::Created(ids) = engine
        .ingest(make_observation("entity", KnowledgeType::Entity))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let config = QueryConfig::default();

    // Non-Associative modes return Ok(empty). Associative needs a real seed.
    let queries = vec![
        Query::TypeFiltered {
            node_type: KnowledgeType::Convention,
            limit: 5,
        },
        Query::Neighborhood {
            entity: ids[0],
            depth: 2,
        },
        Query::Temporal {
            since: Timestamp(0),
            node_types: None,
            limit: 10,
        },
        Query::List {
            min_salience: 0.5,
            limit: 20,
        },
    ];

    for q in &queries {
        let result = engine.query(q, &config);
        assert!(result.is_ok());
    }
}
