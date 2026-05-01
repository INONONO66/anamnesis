use anamnesis::api::{IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::snapshot::{SnapshotId, SnapshotStore};
use anamnesis::storage::{InMemoryStorage, StorageAdapter};
use anamnesis::{Engine, Error};

fn observation(name: &str, timestamp: Timestamp) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("summary for {name}")),
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["snapshot".to_string()],
        origin: Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::new("project-1").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp,
    }
}

fn ingest_node(engine: &mut Engine, name: &str, timestamp: Timestamp) -> NodeId {
    match engine.ingest(observation(name, timestamp)).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("test observations should create nodes"),
    }
}

#[test]
fn restore_reverts_created_nodes_and_edges() {
    let mut engine = Engine::new();
    let first = ingest_node(&mut engine, "first", Timestamp(10));
    let second = ingest_node(&mut engine, "second", Timestamp(11));
    let original_edge = engine.link(first, second, EdgeType::Semantic, 0.8).unwrap();

    let snapshot = engine.snapshot("baseline");

    let third = ingest_node(&mut engine, "third", Timestamp(13));
    engine.link(second, third, EdgeType::Causal, 0.7).unwrap();
    assert_eq!(engine.graph().node_count(), 3);
    assert_eq!(engine.graph().edge_count(), 2);

    engine.restore(&snapshot).unwrap();

    assert_eq!(engine.graph().node_count(), 2);
    assert_eq!(engine.graph().edge_count(), 1);
    assert!(engine.graph().get_node(first).is_ok());
    assert!(engine.graph().get_node(second).is_ok());
    assert!(engine.graph().get_node(third).is_err());
    assert!(engine.graph().get_edge(original_edge).is_ok());
}

#[test]
fn restore_preserves_soa_hot_field_consistency() {
    let mut engine = Engine::new();
    let node = ingest_node(&mut engine, "hot-field", Timestamp(20));

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(node, 0.42)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_accessed_at(node, Timestamp(21))
        .unwrap();

    let snapshot = engine.snapshot("hot-fields");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(node, 0.91)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_accessed_at(node, Timestamp(99))
        .unwrap();

    engine.restore(&snapshot).unwrap();

    let storage = engine.graph().storage();
    assert_eq!(storage.get_salience(node).unwrap(), 0.42);
    assert_eq!(storage.get_accessed_at(node).unwrap(), Timestamp(21));
    assert_eq!(storage.get_node(node).unwrap().salience, 0.42);
    assert_eq!(storage.get_node(node).unwrap().accessed_at, Timestamp(21));
}

#[test]
fn multiple_snapshots_coexist_independently() {
    let mut engine = Engine::new();
    let first = ingest_node(&mut engine, "first", Timestamp(30));
    let snapshot_one = engine.snapshot("one-node");

    let second = ingest_node(&mut engine, "second", Timestamp(32));
    let snapshot_two = engine.snapshot("two-nodes");

    let third = ingest_node(&mut engine, "third", Timestamp(34));
    assert_eq!(engine.graph().node_count(), 3);

    engine.restore(&snapshot_one).unwrap();
    assert_eq!(engine.graph().node_count(), 1);
    assert!(engine.graph().get_node(first).is_ok());
    assert!(engine.graph().get_node(second).is_err());
    assert!(engine.graph().get_node(third).is_err());

    engine.restore(&snapshot_two).unwrap();
    assert_eq!(engine.graph().node_count(), 2);
    assert!(engine.graph().get_node(first).is_ok());
    assert!(engine.graph().get_node(second).is_ok());
    assert!(engine.graph().get_node(third).is_err());
}

#[test]
fn list_snapshots_exposes_ids_labels_and_timestamps() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "first", Timestamp(40));

    let first = engine.snapshot("before-mutation");
    ingest_node(&mut engine, "second", Timestamp(42));
    let second = engine.snapshot("after-mutation");

    let snapshots = engine.list_snapshots();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].0, first);
    assert_eq!(snapshots[0].1, "before-mutation");
    assert!(snapshots[0].2 > Timestamp(0));
    assert_eq!(snapshots[1].0, second);
    assert_eq!(snapshots[1].1, "after-mutation");
    assert!(snapshots[1].2 >= snapshots[0].2);
}

#[test]
fn restore_missing_snapshot_returns_error() {
    let mut engine = Engine::new();

    let err = engine.restore(&SnapshotId(404)).unwrap_err();

    assert!(
        matches!(err, Error::InvalidInput(message) if message.contains("snapshot not found: 404"))
    );
}

#[test]
fn snapshot_store_drop_removes_only_requested_entry() {
    let storage = InMemoryStorage::new();
    let mut store = SnapshotStore::new();

    let first = store.take("first", &storage, Timestamp(50));
    let mut engine = Engine::new();
    ingest_node(&mut engine, "stored", Timestamp(51));
    let second = store.take("second", engine.graph().storage(), Timestamp(52));

    let dropped = store.drop_snapshot(first).unwrap();

    assert_eq!(dropped.id, first);
    assert!(store.restore(&first).is_err());
    assert_eq!(store.restore(&second).unwrap().node_count(), 1);
    assert_eq!(store.list().len(), 1);
    assert_eq!(store.list()[0].0, second);
}
