//! Snapshot round-trip invariant tests.
//!
//! These tests prove that clone-based snapshot/restore preserves the
//! observable state surface that downstream tasks (T15 scope_index,
//! T3 decay_checkpoint, ID recycling, hot-field SoA coherence)
//! depend on.

use anamnesis::api::{IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::storage::StorageAdapter;
use anamnesis::{Engine, EngineConfig};

const DAY_MS: u64 = 86_400_000;
const DEFAULT_SCOPE: &str = "project/default";

fn test_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn observation_at(name: &str, scope: &str, ts: Timestamp) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: Vec::new(),
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::new(scope).expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: ts,
        valid_from: None,
        valid_until: None,
    }
}

fn ingest_at(engine: &mut Engine, name: &str, scope: &str, ts: Timestamp) -> NodeId {
    match engine.ingest(observation_at(name, scope, ts)).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("test fixture should always create a fresh node"),
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    }
}

#[test]
fn snapshot_preserves_scope_index() {
    let mut engine = test_engine();

    let scopes = ["scope/a", "scope/b", "scope/c", "scope/d", "scope/e"];

    // Ingest 10 nodes deterministically across 5 scopes (2 per scope).
    // Insertion pattern: scope[0], scope[1], scope[2], scope[3], scope[4],
    //                    scope[0], scope[1], scope[2], scope[3], scope[4]
    let mut expected_per_scope: Vec<Vec<NodeId>> = vec![Vec::new(); scopes.len()];
    for i in 0..10u64 {
        let scope_idx = (i as usize) % scopes.len();
        let id = ingest_at(
            &mut engine,
            &format!("node-{i}"),
            scopes[scope_idx],
            Timestamp(i * 10),
        );
        expected_per_scope[scope_idx].push(id);
    }

    // Snapshot the pre-snapshot scope_index by querying nodes_by_scope.
    let pre_snapshot: Vec<Vec<NodeId>> = scopes
        .iter()
        .map(|s| {
            engine
                .graph()
                .storage()
                .nodes_by_scope(&ScopePath::new(*s).expect("valid scope"))
        })
        .collect();

    // Sanity check: the storage index already reflects deterministic insertion order.
    for (i, scope) in scopes.iter().enumerate() {
        assert_eq!(
            pre_snapshot[i], expected_per_scope[i],
            "pre-snapshot scope_index must preserve insertion order for {scope}"
        );
    }

    let snap = engine.snapshot("ten-nodes-five-scopes").unwrap();

    // Mutate after snapshot so that restore has something to revert.
    ingest_at(&mut engine, "extra-a", scopes[0], Timestamp(1000));
    ingest_at(&mut engine, "extra-b", scopes[1], Timestamp(1001));
    ingest_at(&mut engine, "extra-c", scopes[2], Timestamp(1002));

    let scope0 = ScopePath::new(scopes[0]).expect("valid scope");
    assert_eq!(
        engine.graph().storage().nodes_by_scope(&scope0).len(),
        3,
        "post-mutation scope[0] should hold its two original nodes plus the extra"
    );

    engine.restore(&snap).unwrap();

    // After restore, scope_index for every scope must match pre-snapshot exactly,
    // including ordering — `Vec::retain` preserves insertion order, so the
    // restored map must hand back the same NodeIds in the same positions.
    for (i, scope) in scopes.iter().enumerate() {
        let post = engine
            .graph()
            .storage()
            .nodes_by_scope(&ScopePath::new(*scope).expect("valid scope"));
        assert_eq!(
            post, pre_snapshot[i],
            "scope_index must be restored to pre-snapshot ordering for {scope}"
        );
    }
}

#[test]
fn snapshot_preserves_decay_checkpoint() {
    let mut engine = test_engine();

    let id_a = ingest_at(&mut engine, "alpha", DEFAULT_SCOPE, Timestamp(0));
    let id_b = ingest_at(&mut engine, "bravo", DEFAULT_SCOPE, Timestamp(0));

    // Tick at +3d so decay_checkpoint advances away from accessed_at on both nodes.
    engine.tick(Timestamp(3 * DAY_MS)).unwrap();

    let pre_checkpoint_a = engine.graph().storage().get_decay_checkpoint(id_a).unwrap();
    let pre_checkpoint_b = engine.graph().storage().get_decay_checkpoint(id_b).unwrap();
    let pre_accessed_a = engine.graph().storage().get_accessed_at(id_a).unwrap();
    let pre_accessed_b = engine.graph().storage().get_accessed_at(id_b).unwrap();

    assert_eq!(pre_checkpoint_a, Timestamp(3 * DAY_MS));
    assert_eq!(pre_checkpoint_b, Timestamp(3 * DAY_MS));
    assert_eq!(pre_accessed_a, Timestamp(0));
    assert_eq!(pre_accessed_b, Timestamp(0));

    let snap = engine.snapshot("post-tick").unwrap();

    // Mutate: another tick + a touch on one node should diverge both checkpoints
    // and accessed_at on id_a from the snapshot baseline.
    engine.tick(Timestamp(7 * DAY_MS)).unwrap();
    engine.touch(id_a, Timestamp(8 * DAY_MS)).unwrap();

    assert_ne!(
        engine.graph().storage().get_decay_checkpoint(id_a).unwrap(),
        pre_checkpoint_a,
        "post-mutation checkpoint must differ from snapshot baseline"
    );

    engine.restore(&snap).unwrap();

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_decay_checkpoint(id_a).unwrap(),
        pre_checkpoint_a,
        "decay_checkpoint for id_a must be restored to pre-snapshot value"
    );
    assert_eq!(
        storage.get_decay_checkpoint(id_b).unwrap(),
        pre_checkpoint_b,
        "decay_checkpoint for id_b must be restored to pre-snapshot value"
    );
    assert_eq!(
        storage.get_accessed_at(id_a).unwrap(),
        pre_accessed_a,
        "accessed_at must follow the snapshot, not the post-snapshot touch"
    );
    assert_eq!(
        storage.get_accessed_at(id_b).unwrap(),
        pre_accessed_b,
        "accessed_at for untouched node must also match pre-snapshot"
    );
}

#[test]
fn snapshot_after_node_delete() {
    let mut engine = test_engine();

    let id_a = ingest_at(&mut engine, "alpha", DEFAULT_SCOPE, Timestamp(0));
    let id_b = ingest_at(&mut engine, "bravo", DEFAULT_SCOPE, Timestamp(1));
    let id_c = ingest_at(&mut engine, "charlie", DEFAULT_SCOPE, Timestamp(2));

    // Delete the middle node first so free_node_ids = [id_b] (LIFO source).
    engine.graph_mut().storage_mut().delete_node(id_b).unwrap();
    assert_eq!(engine.graph().node_count(), 2);
    assert!(engine.graph().get_node(id_b).is_err());

    let snap = engine.snapshot("post-delete").unwrap();

    // Mutate: re-ingest. With LIFO free_node_ids, the next allocation reuses id_b,
    // so the new node fills the freed slot. This proves the snapshot must remember
    // the freed-id stack, not just the live nodes.
    let id_reused = ingest_at(&mut engine, "delta", DEFAULT_SCOPE, Timestamp(3));
    assert_eq!(
        id_reused, id_b,
        "free_node_ids is LIFO; delta should occupy the freed slot"
    );
    assert_eq!(engine.graph().node_count(), 3);
    assert_eq!(engine.graph().get_node(id_b).unwrap().name, "delta");

    engine.restore(&snap).unwrap();

    // Live state matches the snapshot: only id_a and id_c remain.
    assert_eq!(engine.graph().node_count(), 2);
    assert!(
        engine.graph().get_node(id_b).is_err(),
        "deleted node stays deleted after restore; the post-snapshot ingest is gone"
    );
    assert!(engine.graph().get_node(id_a).is_ok());
    assert!(engine.graph().get_node(id_c).is_ok());

    // scope_index reflects the restored deletion: id_b is absent.
    let scope = ScopePath::new(DEFAULT_SCOPE).expect("valid scope");
    let scope_nodes = engine.graph().storage().nodes_by_scope(&scope);
    assert!(
        !scope_nodes.contains(&id_b),
        "scope_index must not retain a freed NodeId after restore"
    );
    assert!(scope_nodes.contains(&id_a));
    assert!(scope_nodes.contains(&id_c));

    // ID recycling state survives the snapshot: the freed id_b is back at the
    // top of free_node_ids, so the next allocation should hand it out again.
    let next_id = engine.graph_mut().storage_mut().next_node_id();
    assert_eq!(
        next_id, id_b,
        "free_node_ids stack must be restored so LIFO recycling stays coherent"
    );
}

#[test]
fn snapshot_then_ingest_then_restore() {
    let mut engine = test_engine();

    let id_a = ingest_at(&mut engine, "alpha", DEFAULT_SCOPE, Timestamp(0));
    assert_eq!(engine.graph().node_count(), 1);

    let snap = engine.snapshot("only-alpha").unwrap();

    let id_b = ingest_at(&mut engine, "bravo", DEFAULT_SCOPE, Timestamp(1));
    assert_eq!(engine.graph().node_count(), 2);
    assert!(engine.graph().get_node(id_b).is_ok());

    engine.restore(&snap).unwrap();

    assert_eq!(engine.graph().node_count(), 1);
    assert!(engine.graph().get_node(id_a).is_ok());
    assert!(
        engine.graph().get_node(id_b).is_err(),
        "post-snapshot ingest must be reverted on restore"
    );

    // scope_index reflects the restored state: only id_a remains.
    let scope = ScopePath::new(DEFAULT_SCOPE).expect("valid scope");
    assert_eq!(engine.graph().storage().nodes_by_scope(&scope), vec![id_a]);
}

#[test]
fn snapshot_preserves_hot_fields_atomically() {
    let mut engine = test_engine();

    let id = ingest_at(&mut engine, "alpha", DEFAULT_SCOPE, Timestamp(0));

    // Drive the hot fields away from their initial values:
    //   - tick() at +2d advances decay_checkpoint and lowers salience.
    //   - touch() at +3d advances accessed_at and reinforces salience.
    engine.tick(Timestamp(2 * DAY_MS)).unwrap();
    engine.touch(id, Timestamp(3 * DAY_MS)).unwrap();

    // Snapshot the four hot-field invariants we care about.
    let pre_salience = engine.graph().storage().get_salience(id).unwrap();
    let pre_accessed_at = engine.graph().storage().get_accessed_at(id).unwrap();
    let pre_decay_checkpoint = engine.graph().storage().get_decay_checkpoint(id).unwrap();
    let pre_node_type = engine.graph().storage().get_node_type(id).unwrap().clone();
    let pre_node_name = engine.graph().get_node(id).unwrap().name.clone();

    let snap = engine.snapshot("hot-field-baseline").unwrap();

    // Mutate every hot field aggressively so the restore actually has to revert.
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(id, 0.123)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_accessed_at(id, Timestamp(99 * DAY_MS))
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_decay_checkpoint(id, Timestamp(99 * DAY_MS))
        .unwrap();

    // Confirm mutation took effect before restoring.
    assert_ne!(
        engine.graph().storage().get_salience(id).unwrap(),
        pre_salience
    );
    assert_ne!(
        engine.graph().storage().get_accessed_at(id).unwrap(),
        pre_accessed_at
    );
    assert_ne!(
        engine.graph().storage().get_decay_checkpoint(id).unwrap(),
        pre_decay_checkpoint
    );

    engine.restore(&snap).unwrap();

    let storage = engine.graph().storage();

    // Each SoA hot-field array is restored individually...
    assert_eq!(
        storage.get_salience(id).unwrap(),
        pre_salience,
        "salience must be restored from the snapshot"
    );
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        pre_accessed_at,
        "accessed_at must be restored from the snapshot"
    );
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        pre_decay_checkpoint,
        "decay_checkpoint must be restored from the snapshot"
    );
    assert_eq!(
        storage.get_node_type(id).unwrap(),
        &pre_node_type,
        "node_type must be restored from the snapshot"
    );

    // ...AND the SoA arrays remain coherent with the corresponding fields on
    // the dense Node record. The Engine's set_salience/set_accessed_at writes
    // both halves; the snapshot must not leave them disagreeing after restore.
    let node = storage.get_node(id).unwrap();
    assert_eq!(
        node.salience, pre_salience,
        "Node.salience must agree with the SoA salience array after restore"
    );
    assert_eq!(
        node.accessed_at, pre_accessed_at,
        "Node.accessed_at must agree with the SoA accessed_at array after restore"
    );
    assert_eq!(
        node.node_type, pre_node_type,
        "Node.node_type must agree with the SoA node_type array after restore"
    );
    assert_eq!(node.id, id, "Node identity must be preserved");
    assert_eq!(node.name, pre_node_name, "Node.name must be preserved");
}

#[test]
fn snapshot_preserves_reservoirs() {
    use anamnesis::graph::{Edge, EdgeType, MemoryTier, Node};
    use anamnesis::mechanics::priors::{project_salience, project_weight};
    use anamnesis::snapshot::SnapshotStore;
    use anamnesis::storage::SqliteStorage;
    use std::collections::{HashMap, VecDeque};

    fn make_node(id: NodeId) -> Node {
        Node {
            id,
            node_type: KnowledgeType::Semantic,
            name: format!("node-{}", id.0),
            summary: None,
            content: "reservoir content".to_string(),
            embedding: None,
            created_at: Timestamp(1000),
            updated_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            retained_action: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "s".to_string(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    let mut storage = SqliteStorage::new().expect("storage init");

    let n0 = storage.next_node_id();
    let n1 = storage.next_node_id();
    storage.set_node(make_node(n0)).expect("node 0");
    storage.set_node(make_node(n1)).expect("node 1");

    let e0 = storage.next_edge_id();
    storage
        .set_edge(Edge {
            id: e0,
            source: n0,
            target: n1,
            edge_type: EdgeType::Semantic,
            weight: 0.5,
            conductance: 0.0,
            edge_source: anamnesis::graph::edge::EdgeSource::Auto,
            created_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        })
        .expect("edge 0");

    // Drive the reservoirs to non-trivial log-odds values via the commit-style
    // setters; these recompute the bounded projections.
    let ra = 1.75_f64;
    let cond = -0.625_f64;
    storage.set_retained_action(n0, ra).expect("set A_0");
    storage.set_conductance(e0, cond).expect("set C_0");
    storage
        .set_edge_accessed_at(e0, Timestamp(4242))
        .expect("set edge accessed_at");

    // Reservoir is authoritative; the projection must track it.
    assert_eq!(storage.get_retained_action(n0).unwrap(), ra);
    assert!((storage.get_salience(n0).unwrap() - project_salience(ra)).abs() < 1e-12);
    assert_eq!(storage.get_conductance(e0).unwrap(), cond);
    assert!((storage.get_edge(e0).unwrap().weight - project_weight(cond)).abs() < 1e-12);

    // Snapshot -> mutate -> restore -> identical.
    let mut store: SnapshotStore<SqliteStorage> = SnapshotStore::new();
    let snap = store.take("reservoir-baseline", &storage, Timestamp(0));

    storage.set_retained_action(n0, -3.0).expect("mutate A_0");
    storage.set_conductance(e0, 2.0).expect("mutate C_0");
    storage
        .set_edge_accessed_at(e0, Timestamp(9999))
        .expect("mutate edge accessed_at");
    assert_ne!(storage.get_retained_action(n0).unwrap(), ra);

    let restored = store.restore(&snap).expect("restore");

    assert_eq!(
        restored.get_retained_action(n0).unwrap(),
        ra,
        "retained_action reservoir must round-trip through snapshot"
    );
    assert!(
        (restored.get_salience(n0).unwrap() - project_salience(ra)).abs() < 1e-12,
        "salience projection must round-trip consistently with the reservoir"
    );
    assert_eq!(
        restored.get_conductance(e0).unwrap(),
        cond,
        "conductance reservoir must round-trip through snapshot"
    );
    assert!(
        (restored.get_edge(e0).unwrap().weight - project_weight(cond)).abs() < 1e-12,
        "edge weight projection must round-trip consistently with the reservoir"
    );
    assert_eq!(
        restored.get_edge_accessed_at(e0).unwrap(),
        Timestamp(4242),
        "edge accessed_at must round-trip through snapshot"
    );
}
