use std::collections::{HashMap, VecDeque};

use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, ScopePath, Timestamp};
use anamnesis::{NodeId, SqliteStorage, StorageAdapter};

fn node(id: NodeId, scope: &str) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id.0),
        summary: None,
        content: "scope index fixture".to_string(),
        embedding: None,
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        salience: 0.5,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            agent_id: "agent".to_string(),
            session_id: "session".to_string(),
            scope: ScopePath::new(scope).expect("valid scope"),
            confidence: 0.9,
        },
        entity_tags: Vec::new(),
        metadata: HashMap::new(),
    }
}

#[test]
fn scope_index_populated_on_set_node() {
    let mut storage = SqliteStorage::new().unwrap();
    let id = storage.next_node_id();
    storage.set_node(node(id, "project/a")).unwrap();

    let scope = ScopePath::new("project/a").expect("valid scope");
    assert_eq!(storage.nodes_by_scope(&scope), vec![id]);
}

#[test]
fn scope_index_pruned_on_delete_node() {
    let mut storage = SqliteStorage::new().unwrap();
    let id = storage.next_node_id();
    storage.set_node(node(id, "project/a")).unwrap();
    storage.delete_node(id).unwrap();

    let scope = ScopePath::new("project/a").expect("valid scope");
    assert!(storage.nodes_by_scope(&scope).is_empty());
}

#[test]
fn scope_index_updated_on_set_node_with_new_scope() {
    let mut storage = SqliteStorage::new().unwrap();
    let id = storage.next_node_id();
    storage.set_node(node(id, "project/a")).unwrap();
    storage.set_node(node(id, "project/b")).unwrap();

    let old_scope = ScopePath::new("project/a").expect("valid scope");
    let new_scope = ScopePath::new("project/b").expect("valid scope");
    assert!(storage.nodes_by_scope(&old_scope).is_empty());
    assert_eq!(storage.nodes_by_scope(&new_scope), vec![id]);
}

#[test]
fn scope_index_clone_round_trip() {
    let mut storage = SqliteStorage::new().unwrap();
    let id = storage.next_node_id();
    storage.set_node(node(id, "project/a")).unwrap();

    let cloned = storage.clone();
    let scope = ScopePath::new("project/a").expect("valid scope");
    assert_eq!(
        cloned.nodes_by_scope(&scope),
        storage.nodes_by_scope(&scope)
    );
}

#[test]
fn nodes_by_scope_returns_insertion_order() {
    let mut storage = SqliteStorage::new().unwrap();
    let first = storage.next_node_id();
    let second = storage.next_node_id();
    let third = storage.next_node_id();

    storage.set_node(node(first, "project/a")).unwrap();
    storage.set_node(node(second, "project/a")).unwrap();
    storage.set_node(node(third, "project/a")).unwrap();

    let scope = ScopePath::new("project/a").expect("valid scope");
    assert_eq!(storage.nodes_by_scope(&scope), vec![first, second, third]);
}
