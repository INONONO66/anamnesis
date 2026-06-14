use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::IngestResult;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig};

fn make_obs(name: &str, ts: u64) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
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
        timestamp: Timestamp(ts),
        valid_from: None,
        valid_until: None,
    }
}

#[test]
fn temporal_filters_by_since() {
    let mut e = Engine::new();
    e.ingest(make_obs("old", 100)).unwrap();
    e.ingest(make_obs("new", 200)).unwrap();

    let pkg = e
        .query(
            &Query::Temporal {
                since: Timestamp(150),
                node_types: None,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(pkg.knowledge.len(), 1);
    assert!(pkg.knowledge[0].name.contains("new"));
}

#[test]
fn neighborhood_returns_n_hop_subgraph() {
    let mut e = Engine::new();
    let a = match e.ingest(make_obs("a", 0)).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!(),
    };
    let b = match e.ingest(make_obs("b", 0)).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!(),
    };
    e.link(a, b, EdgeType::Semantic).unwrap();

    let pkg = e
        .query(
            &Query::Neighborhood {
                entity: a,
                depth: 1,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert!(pkg.knowledge.iter().any(|f| f.node_id == b));
}

#[test]
fn temporal_returns_all_when_since_is_zero() {
    let mut e = Engine::new();
    e.ingest(make_obs("a", 100)).unwrap();
    e.ingest(make_obs("b", 200)).unwrap();

    let pkg = e
        .query(
            &Query::Temporal {
                since: Timestamp(0),
                node_types: None,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(pkg.knowledge.len(), 2);
}
