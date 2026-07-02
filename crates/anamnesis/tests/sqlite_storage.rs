use anamnesis::Error;
use anamnesis::engine::{Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, MemoryTier, ScopePath};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use std::collections::{HashMap, VecDeque};

fn make_node(id: NodeId, salience: f64) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id.0),
        summary: None,
        content: format!("content for node {}", id.0),
        embedding: None,
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        salience,
        retained_action: 0.0,
        evidence_prior: 0.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "test-session".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    }
}

fn make_edge(id: EdgeId, source: NodeId, target: NodeId) -> Edge {
    Edge {
        id,
        source,
        target,
        edge_type: EdgeType::Semantic,
        weight: 0.8,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: HashMap::new(),
    }
}

fn make_node_indexed(
    id: NodeId,
    entity_tags: Vec<&str>,
    node_type: KnowledgeType,
    _agent_id: &str,
    scope: Option<&str>,
) -> Node {
    let scope = match scope {
        Some(path) => ScopePath::new(path).expect("valid scope"),
        None => ScopePath::universal(),
    };
    Node {
        id,
        node_type,
        name: format!("node-{}", id.0),
        summary: None,
        content: "test".to_string(),
        embedding: None,
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        salience: 0.5,
        retained_action: 0.0,
        evidence_prior: 0.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "session".to_string(),
            scope,
            confidence: 0.9,
        },
        entity_tags: entity_tags.iter().map(|s| s.to_string()).collect(),
        metadata: HashMap::new(),
    }
}

fn storage() -> SqliteStorage {
    SqliteStorage::new().expect("sqlite storage initializes")
}

#[test]
fn new_storage_is_empty() {
    let s = storage();
    assert_eq!(s.node_count(), 0);
    assert_eq!(s.edge_count(), 0);
    assert!(s.all_node_ids().is_empty());
    assert!(s.all_edge_ids().is_empty());
}

#[test]
fn set_node_populates_indexed_queries() {
    let mut s = storage();
    let id = s.next_node_id();
    let node = make_node_indexed(
        id,
        vec!["auth"],
        KnowledgeType::Convention,
        "agent-A",
        Some("proj-P"),
    );

    s.set_node(node).expect("node stored");

    assert_eq!(s.nodes_by_entity_tag("auth"), vec![id]);
    assert_eq!(s.nodes_by_type(&KnowledgeType::Convention), vec![id]);
    assert_eq!(
        s.nodes_by_peer(anamnesis::graph::types::PeerId(0)),
        vec![id]
    );
    assert_eq!(
        s.nodes_by_scope(&ScopePath::new("proj-P").expect("valid scope")),
        vec![id]
    );
}

#[test]
fn delete_node_removes_from_indexed_queries() {
    let mut s = storage();
    let id = s.next_node_id();
    s.set_node(make_node_indexed(
        id,
        vec!["auth"],
        KnowledgeType::Convention,
        "A",
        Some("P"),
    ))
    .expect("node stored");

    s.delete_node(id).expect("node deleted");

    assert!(s.nodes_by_entity_tag("auth").is_empty());
    assert!(s.nodes_by_type(&KnowledgeType::Convention).is_empty());
    assert!(
        s.nodes_by_peer(anamnesis::graph::types::PeerId(0))
            .is_empty()
    );
}

#[test]
fn set_node_update_refreshes_indexes() {
    let mut s = storage();
    let id = s.next_node_id();
    s.set_node(make_node_indexed(
        id,
        vec!["old-tag"],
        KnowledgeType::Semantic,
        "agent",
        None,
    ))
    .expect("node stored");

    s.set_node(make_node_indexed(
        id,
        vec!["new-tag"],
        KnowledgeType::Semantic,
        "agent",
        None,
    ))
    .expect("node updated");

    assert!(s.nodes_by_entity_tag("old-tag").is_empty());
    assert_eq!(s.nodes_by_entity_tag("new-tag"), vec![id]);
}

#[test]
fn allocate_and_store_node() {
    let mut s = storage();
    let id = s.next_node_id();
    assert_eq!(id, NodeId(0));

    s.set_node(make_node(id, 0.7)).expect("node stored");

    let retrieved = s.get_node(id).expect("node exists");
    assert_eq!(retrieved.id, id);
    assert_eq!(retrieved.salience, 0.7);
    assert_eq!(s.node_count(), 1);
}

#[test]
fn delete_node_frees_id() {
    let mut s = storage();
    let id0 = s.next_node_id();
    s.set_node(make_node(id0, 0.5)).expect("node stored");
    s.delete_node(id0).expect("node deleted");

    assert_eq!(s.node_count(), 0);
    assert_eq!(s.next_node_id(), id0);
}

#[test]
fn allocate_and_store_edge() {
    let mut s = storage();
    let n0 = s.next_node_id();
    let n1 = s.next_node_id();
    s.set_node(make_node(n0, 0.5)).expect("node stored");
    s.set_node(make_node(n1, 0.5)).expect("node stored");

    let eid = s.next_edge_id();
    assert_eq!(eid, EdgeId(0));

    s.set_edge(make_edge(eid, n0, n1)).expect("edge stored");
    let retrieved = s.get_edge(eid).expect("edge exists");
    assert_eq!(retrieved.source, n0);
    assert_eq!(retrieved.target, n1);
    assert_eq!(s.edge_count(), 1);
}

#[test]
fn delete_edge_frees_id() {
    let mut s = storage();
    let n0 = s.next_node_id();
    let n1 = s.next_node_id();
    s.set_node(make_node(n0, 0.5)).expect("node stored");
    s.set_node(make_node(n1, 0.5)).expect("node stored");

    let eid = s.next_edge_id();
    s.set_edge(make_edge(eid, n0, n1)).expect("edge stored");
    s.delete_edge(eid).expect("edge deleted");

    assert_eq!(s.edge_count(), 0);
    assert_eq!(s.next_edge_id(), eid);
}

#[test]
fn adjacency_out_correct() {
    let mut s = storage();
    let a = s.next_node_id();
    let b = s.next_node_id();
    let c = s.next_node_id();
    s.set_node(make_node(a, 0.5)).expect("node stored");
    s.set_node(make_node(b, 0.5)).expect("node stored");
    s.set_node(make_node(c, 0.5)).expect("node stored");

    let e0 = s.next_edge_id();
    let e1 = s.next_edge_id();
    s.set_edge(make_edge(e0, a, b)).expect("edge stored");
    s.set_edge(make_edge(e1, a, c)).expect("edge stored");

    let out = s.edges_from(a);
    assert_eq!(out.len(), 2);
    assert!(out.contains(&e0));
    assert!(out.contains(&e1));
}

#[test]
fn adjacency_in_correct() {
    let mut s = storage();
    let a = s.next_node_id();
    let b = s.next_node_id();
    let c = s.next_node_id();
    s.set_node(make_node(a, 0.5)).expect("node stored");
    s.set_node(make_node(b, 0.5)).expect("node stored");
    s.set_node(make_node(c, 0.5)).expect("node stored");

    let e0 = s.next_edge_id();
    let e1 = s.next_edge_id();
    s.set_edge(make_edge(e0, a, c)).expect("edge stored");
    s.set_edge(make_edge(e1, b, c)).expect("edge stored");

    let inc = s.edges_to(c);
    assert_eq!(inc.len(), 2);
    assert!(inc.contains(&e0));
    assert!(inc.contains(&e1));
}

#[test]
fn adjacency_updated_on_delete() {
    let mut s = storage();
    let a = s.next_node_id();
    let b = s.next_node_id();
    s.set_node(make_node(a, 0.5)).expect("node stored");
    s.set_node(make_node(b, 0.5)).expect("node stored");

    let eid = s.next_edge_id();
    s.set_edge(make_edge(eid, a, b)).expect("edge stored");
    assert_eq!(s.edges_from(a).len(), 1);

    s.delete_edge(eid).expect("edge deleted");
    assert!(s.edges_from(a).is_empty());
    assert!(s.edges_to(b).is_empty());
}

#[test]
fn hot_fields_synced() {
    let mut s = storage();
    let id = s.next_node_id();
    s.set_node(make_node(id, 0.5)).expect("node stored");

    s.set_salience(id, 0.9).expect("salience updated");
    assert_eq!(s.get_salience(id).expect("salience exists"), 0.9);
    assert_eq!(s.get_node(id).expect("node exists").salience, 0.9);

    s.set_accessed_at(id, Timestamp(5000))
        .expect("accessed_at updated");
    assert_eq!(
        s.get_accessed_at(id).expect("accessed_at exists"),
        Timestamp(5000)
    );
    assert_eq!(
        s.get_node(id).expect("node exists").accessed_at,
        Timestamp(5000)
    );

    s.set_decay_checkpoint(id, Timestamp(6000))
        .expect("decay checkpoint updated");
    assert_eq!(
        s.get_decay_checkpoint(id).expect("decay checkpoint exists"),
        Timestamp(6000)
    );
}

#[test]
fn all_node_ids_excludes_deleted() {
    let mut s = storage();
    let n0 = s.next_node_id();
    let n1 = s.next_node_id();
    let n2 = s.next_node_id();
    s.set_node(make_node(n0, 0.5)).expect("node stored");
    s.set_node(make_node(n1, 0.5)).expect("node stored");
    s.set_node(make_node(n2, 0.5)).expect("node stored");

    s.delete_node(n1).expect("node deleted");

    let ids = s.all_node_ids();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&n0));
    assert!(ids.contains(&n2));
    assert!(!ids.contains(&n1));
}

#[test]
fn get_node_type_from_soa() {
    let mut s = storage();
    let id = s.next_node_id();
    s.set_node(make_node(id, 0.5)).expect("node stored");

    assert_eq!(
        s.get_node_type(id).expect("node type exists"),
        &KnowledgeType::Semantic
    );
}

#[test]
fn get_nonexistent_node_returns_error() {
    let s = storage();
    assert_eq!(s.get_node(NodeId(99)), Err(Error::NodeNotFound(NodeId(99))));
}

#[test]
fn get_nonexistent_edge_returns_error() {
    let s = storage();
    assert_eq!(s.get_edge(EdgeId(99)), Err(Error::EdgeNotFound(EdgeId(99))));
}

#[test]
fn edges_from_nonexistent_node_returns_empty() {
    let s = storage();
    assert!(s.edges_from(NodeId(99)).is_empty());
}

#[test]
fn set_edge_twice_no_duplicate_adjacency() {
    let mut s = storage();
    let n0 = s.next_node_id();
    let n1 = s.next_node_id();
    s.set_node(make_node(n0, 0.5)).expect("node stored");
    s.set_node(make_node(n1, 0.5)).expect("node stored");
    let eid = s.next_edge_id();
    s.set_edge(make_edge(eid, n0, n1)).expect("edge stored");
    s.set_edge(make_edge(eid, n0, n1)).expect("edge updated");
    assert_eq!(s.edges_from(n0).len(), 1);
    assert_eq!(s.edges_to(n1).len(), 1);
    assert_eq!(s.edge_count(), 1);
}

#[test]
fn delete_node_clears_adjacency() {
    let mut s = storage();
    let n0 = s.next_node_id();
    let n1 = s.next_node_id();
    s.set_node(make_node(n0, 0.5)).expect("node stored");
    s.set_node(make_node(n1, 0.5)).expect("node stored");
    let eid = s.next_edge_id();
    s.set_edge(make_edge(eid, n0, n1)).expect("edge stored");
    s.delete_edge(eid).expect("edge deleted");
    s.delete_node(n0).expect("node deleted");
    let reused = s.next_node_id();
    assert_eq!(reused, n0);
    s.set_node(make_node(reused, 0.8)).expect("node stored");
    assert!(s.edges_from(reused).is_empty());
    assert!(s.edges_to(reused).is_empty());
}

#[test]
fn nodes_by_entity_tag_returns_correct_set() {
    let mut s = storage();
    let id1 = s.next_node_id();
    let id2 = s.next_node_id();
    let id3 = s.next_node_id();
    s.set_node(make_node_indexed(
        id1,
        vec!["auth"],
        KnowledgeType::Semantic,
        "A",
        None,
    ))
    .expect("node stored");
    s.set_node(make_node_indexed(
        id2,
        vec!["auth", "db"],
        KnowledgeType::Semantic,
        "A",
        None,
    ))
    .expect("node stored");
    s.set_node(make_node_indexed(
        id3,
        vec!["db"],
        KnowledgeType::Convention,
        "B",
        None,
    ))
    .expect("node stored");
    let auth_set: std::collections::HashSet<_> =
        s.nodes_by_entity_tag("auth").into_iter().collect();
    assert_eq!(auth_set, [id1, id2].iter().copied().collect());
}

#[test]
fn nodes_by_type_returns_correct_set() {
    let mut s = storage();
    let id1 = s.next_node_id();
    let id2 = s.next_node_id();
    s.set_node(make_node_indexed(
        id1,
        vec![],
        KnowledgeType::Semantic,
        "A",
        None,
    ))
    .expect("node stored");
    s.set_node(make_node_indexed(
        id2,
        vec![],
        KnowledgeType::Convention,
        "A",
        None,
    ))
    .expect("node stored");
    assert_eq!(s.nodes_by_type(&KnowledgeType::Semantic), vec![id1]);
}

#[test]
fn nodes_by_agent_returns_correct_set() {
    use anamnesis::engine::SourceKind;
    use anamnesis::graph::types::PeerId;
    let mut s = storage();
    let id1 = s.next_node_id();
    let id2 = s.next_node_id();
    // Create node with PeerId(1)
    let mut node1 = make_node_indexed(id1, vec![], KnowledgeType::Semantic, "agent-A", None);
    node1.origin.peer_id = PeerId(1);
    node1.origin.source_kind = SourceKind::AgentObservation;
    s.set_node(node1).expect("node stored");
    // Create node with PeerId(2)
    let mut node2 = make_node_indexed(id2, vec![], KnowledgeType::Semantic, "agent-B", None);
    node2.origin.peer_id = PeerId(2);
    node2.origin.source_kind = SourceKind::AgentObservation;
    s.set_node(node2).expect("node stored");
    assert_eq!(s.nodes_by_peer(PeerId(1)), vec![id1]);
    assert_eq!(s.nodes_by_peer(PeerId(2)), vec![id2]);
    assert!(s.nodes_by_peer(PeerId(99)).is_empty());
}

#[test]
fn nodes_by_scope_returns_correct_set() {
    let mut s = storage();
    let id1 = s.next_node_id();
    let id2 = s.next_node_id();
    s.set_node(make_node_indexed(
        id1,
        vec![],
        KnowledgeType::Semantic,
        "A",
        Some("proj-X"),
    ))
    .expect("node stored");
    s.set_node(make_node_indexed(
        id2,
        vec![],
        KnowledgeType::Semantic,
        "A",
        None,
    ))
    .expect("node stored");
    let scope = ScopePath::new("proj-X").expect("valid scope");
    assert_eq!(s.nodes_by_scope(&scope), vec![id1]);
}

#[test]
fn node_ids_descending_returns_sorted() {
    let mut s = storage();
    let id0 = s.next_node_id();
    let id1 = s.next_node_id();
    let id2 = s.next_node_id();
    s.set_node(make_node(id0, 0.5)).expect("node stored");
    s.set_node(make_node(id1, 0.5)).expect("node stored");
    s.set_node(make_node(id2, 0.5)).expect("node stored");
    assert_eq!(s.node_ids_descending(), vec![id2, id1, id0]);
}

#[test]
fn get_node_mut_modifies_in_place() {
    let mut s = storage();
    let id = s.next_node_id();
    s.set_node(make_node(id, 0.5)).expect("node stored");

    let node = s.get_node_mut(id).expect("node exists");
    node.name = "modified".to_string();

    assert_eq!(s.get_node(id).expect("node exists").name, "modified");
}

#[test]
fn fts5_text_search_returns_matches() {
    let mut s = storage();
    let id1 = s.next_node_id();
    let id2 = s.next_node_id();
    let mut first = make_node(id1, 0.5);
    first.name = "auth factory".to_string();
    first.content = "handler construction uses a factory pattern".to_string();
    let mut second = make_node(id2, 0.5);
    second.name = "database migration".to_string();
    second.content = "schema change for persistence".to_string();
    s.set_node(first).expect("node stored");
    s.set_node(second).expect("node stored");

    let results = s.text_search("factory", 10);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, id1);
    assert!(results[0].1 > 0.0);
}

#[test]
fn clone_preserves_graph_state() {
    let mut s = storage();
    let n0 = s.next_node_id();
    let n1 = s.next_node_id();
    s.set_node(make_node(n0, 0.4)).expect("node stored");
    s.set_node(make_node(n1, 0.6)).expect("node stored");
    let edge = s.next_edge_id();
    s.set_edge(make_edge(edge, n0, n1)).expect("edge stored");
    s.set_decay_checkpoint(n0, Timestamp(7777))
        .expect("decay checkpoint updated");

    let cloned = s.clone();

    assert_eq!(cloned.node_count(), 2);
    assert_eq!(cloned.edge_count(), 1);
    assert_eq!(cloned.edges_from(n0), &[edge]);
    assert_eq!(
        cloned
            .get_decay_checkpoint(n0)
            .expect("decay checkpoint exists"),
        Timestamp(7777)
    );
}

#[test]
fn file_backed_storage_reopens_existing_rows() {
    let path = std::env::temp_dir().join(format!(
        "anamnesis-sqlite-storage-{}-{}.db",
        std::process::id(),
        Timestamp::now().0
    ));
    {
        let mut s = SqliteStorage::open(&path).expect("file storage opens");
        let id = s.next_node_id();
        let mut node = make_node(id, 0.8);
        node.embedding = Some(vec![0.1, 0.2, 0.3]);
        node.metadata.insert("key".to_string(), "value".to_string());
        s.set_node(node).expect("node stored");
    }
    {
        let s = SqliteStorage::open(&path).expect("file storage reopens");
        assert_eq!(s.node_count(), 1);
        let node = s.get_node(NodeId(0)).expect("node exists");
        assert_eq!(node.embedding, Some(vec![0.1, 0.2, 0.3]));
        assert_eq!(node.metadata.get("key"), Some(&"value".to_string()));
    }
    let _ = std::fs::remove_file(path);
}
