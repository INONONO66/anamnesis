//! Integration tests for the Phase 1 skeleton.
//!
//! These tests verify the full Engine lifecycle:
//! ingest → link → touch → tick → query → merge_candidates → reflect_batch

use anamnesis::api::{Observation, SessionSummary};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig};
use anamnesis::{Engine, EngineConfig};

fn make_observation(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("Summary of {}", name)),
        content: format!("Full content of {}", name),
        embedding: Some(vec![0.1, 0.2, 0.3, 0.4]),
        confidence: 0.9,
        node_type,
        entity_tags: vec!["test-entity".to_string()],
        origin: Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: Some("anamnesis".to_string()),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
    }
}

#[test]
fn engine_full_lifecycle() {
    let mut engine = Engine::new();

    // 1. Ingest two observations
    let ids1 = engine
        .ingest(make_observation(
            "auth uses factory pattern",
            KnowledgeType::Convention,
        ))
        .unwrap();
    let ids2 = engine
        .ingest(make_observation(
            "race condition in auth middleware",
            KnowledgeType::Episodic,
        ))
        .unwrap();
    assert_eq!(ids1.len(), 1);
    assert_eq!(ids2.len(), 1);
    assert_eq!(engine.graph().node_count(), 2);

    // 2. Link the nodes
    let eid = engine
        .link(ids1[0], ids2[0], EdgeType::Semantic, 0.78)
        .unwrap();
    assert_eq!(engine.graph().edge_count(), 1);
    let edge = engine.graph().get_edge(eid).unwrap();
    assert_eq!(edge.weight, 0.78);

    // 3. Touch a node
    engine.touch(ids1[0], Timestamp(2000)).unwrap();
    engine.touch(ids1[0], Timestamp(2000)).unwrap();
    let node = engine.graph().get_node(ids1[0]).unwrap();
    assert_eq!(node.access_count, 2);

    // 4. Tick — ids2[0] has dt=1s from ingest, ids1[0] was just touched so dt=0
    let report = engine.tick(Timestamp(2000)).unwrap();
    assert_eq!(report.nodes_decayed, 1);

    // 5. Query (placeholder — returns empty in Phase 1)
    let q = Query::Associative {
        seed: ids1[0],
        budget: 100,
    };
    let pkg = engine.query(&q, &QueryConfig::default()).unwrap();
    assert_eq!(pkg.total_fragments(), 0);
    assert_eq!(pkg.agent_tension, 0.0);

    // 6. Merge candidates (placeholder)
    let candidates = engine.merge_candidates(0.9).unwrap();
    assert!(candidates.is_empty());

    // 7. Auto merge (placeholder)
    let log = engine.auto_merge(0.9).unwrap();
    assert_eq!(log.merges_performed, 0);

    // 8. Reflect batch (placeholder)
    let sessions = vec![SessionSummary {
        agent_id: "agent-1".to_string(),
        session_id: "session-1".to_string(),
        node_ids: vec![ids1[0], ids2[0]],
    }];
    let reflect_report = engine.reflect_batch(&sessions).unwrap();
    assert_eq!(reflect_report.entity_edges_created, 0);
}

#[test]
fn engine_with_config() {
    let config = EngineConfig::new()
        .with_max_nodes(1000)
        .with_novelty_threshold(0.4)
        .with_confidence_threshold(0.6);
    let mut engine = Engine::with_config(config);

    let ids = engine
        .ingest(make_observation("test", KnowledgeType::Semantic))
        .unwrap();
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
            agent_id: "user".to_string(),
            session_id: "design-session".to_string(),
            project_id: Some("anamnesis".to_string()),
            confidence: 0.95,
        },
        timestamp: Timestamp(5000),
    };

    let ids = engine.ingest(obs).unwrap();
    let node = engine.graph().get_node(ids[0]).unwrap();

    assert_eq!(node.name, "physics = edge weight dynamics");
    assert_eq!(
        node.summary.as_deref(),
        Some("Force-directed simulation rejected")
    );
    assert_eq!(node.node_type, KnowledgeType::Decision);
    assert_eq!(node.entity_tags, vec!["physics", "anamnesis"]);
    assert_eq!(node.origin.project_id.as_deref(), Some("anamnesis"));
    assert_eq!(node.salience, 1.0);
    assert_eq!(node.access_count, 0);
    assert!(node.embedding.is_some());
}

#[test]
fn multiple_edge_types() {
    let mut engine = Engine::new();
    let id1 = engine
        .ingest(make_observation("decision", KnowledgeType::Decision))
        .unwrap()[0];
    let id2 = engine
        .ingest(make_observation("reason", KnowledgeType::Semantic))
        .unwrap()[0];
    let id3 = engine
        .ingest(make_observation("rejected", KnowledgeType::Semantic))
        .unwrap()[0];

    engine.link(id1, id2, EdgeType::Reason, 1.0).unwrap();
    engine
        .link(id1, id3, EdgeType::RejectedAlternative, 0.6)
        .unwrap();

    assert_eq!(engine.graph().edge_count(), 2);
    assert_eq!(engine.graph().edges_from(id1).len(), 2);
}

#[test]
fn query_all_modes_compile() {
    let engine = Engine::new();
    let config = QueryConfig::default();

    // All 5 query modes should compile and return Ok
    let queries = vec![
        Query::Associative {
            seed: anamnesis::NodeId(0),
            budget: 10,
        },
        Query::TypeFiltered {
            node_type: KnowledgeType::Convention,
            limit: 5,
        },
        Query::Neighborhood {
            entity: anamnesis::NodeId(0),
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
