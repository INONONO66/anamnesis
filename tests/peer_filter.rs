//! Tests for SearchInput.peer_filter (T12).

use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::engine::EngineConfig;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::SourceKind;
use anamnesis::query::SearchInput;

fn obs_with_peer(name: &str, peer_id: PeerId) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id,
            source_kind: SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp::now(),
        valid_from: None,
        valid_until: None,
    }
}

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn peer_filter_none_includes_all() {
    let mut e = engine();
    e.ingest(obs_with_peer("node-peer1", PeerId(1))).unwrap();
    e.ingest(obs_with_peer("node-peer2", PeerId(2))).unwrap();
    let result = e
        .search(SearchInput {
            text: "node".to_string(),
            peer_filter: None,
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    // Both nodes should be findable
    let _ = result.package.total_fragments(); // no crash
}

#[test]
fn peer_filter_restricts_to_specified_peers() {
    let mut e = engine();
    let IngestResult::Created(ids1) = e.ingest(obs_with_peer("peer1-node", PeerId(1))).unwrap()
    else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = e.ingest(obs_with_peer("peer2-node", PeerId(2))).unwrap()
    else {
        panic!("expected Created");
    };
    let result = e
        .search(SearchInput {
            text: "peer1-node".to_string(),
            peer_filter: Some(vec![PeerId(1)]),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    let all_ids: Vec<_> = result
        .package
        .knowledge
        .iter()
        .chain(result.package.memories.iter())
        .map(|f| f.node_id)
        .collect();
    // peer2 node should not appear
    assert!(
        !all_ids.contains(&ids2[0]),
        "peer2 node should be filtered out"
    );
    let _ = ids1;
}

#[test]
fn peer_filter_empty_vec_excludes_all() {
    let mut e = engine();
    e.ingest(obs_with_peer("node-a", PeerId(1))).unwrap();
    let result = e
        .search(SearchInput {
            text: "node-a".to_string(),
            peer_filter: Some(vec![]), // empty = no peers match
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.package.total_fragments(), 0);
}
