use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use std::collections::{HashMap, VecDeque};

fn insert_node_with_content(s: &mut SqliteStorage, content: &str) -> NodeId {
    let id = s.next_node_id();
    let node = Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: content.to_string(),
        summary: None,
        content: content.to_string(),
        embedding: None,
        created_at: Timestamp(0),
        updated_at: Timestamp(0),
        accessed_at: Timestamp(0),
        valid_from: None,
        valid_until: None,
        salience: 1.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    };
    s.set_node(node).unwrap();
    id
}

#[test]
fn default_text_search_substring_match() {
    let mut s = SqliteStorage::new().unwrap();
    let id = insert_node_with_content(&mut s, "auth uses factory");
    let results = s.text_search("factory", 10);
    assert!(results.iter().any(|(nid, _)| *nid == id));
}

#[test]
fn text_search_case_insensitive() {
    let mut s = SqliteStorage::new().unwrap();
    let id = insert_node_with_content(&mut s, "Auth Uses Factory");
    let results = s.text_search("factory", 10);
    assert!(results.iter().any(|(nid, _)| *nid == id));
}

#[test]
fn text_search_limit_respected() {
    let mut s = SqliteStorage::new().unwrap();
    for i in 0..5 {
        insert_node_with_content(&mut s, &format!("factory node {}", i));
    }
    let results = s.text_search("factory", 3);
    assert!(results.len() <= 3);
}

#[test]
fn text_search_no_match() {
    let mut s = SqliteStorage::new().unwrap();
    insert_node_with_content(&mut s, "auth uses factory");
    let results = s.text_search("nonexistent", 10);
    assert!(results.is_empty());
}

#[test]
fn text_search_matches_content_field() {
    let mut s = SqliteStorage::new().unwrap();
    let id = s.next_node_id();
    let node = Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: "some name".to_string(),
        summary: None,
        content: "this is the content with keyword".to_string(),
        embedding: None,
        created_at: Timestamp(0),
        updated_at: Timestamp(0),
        accessed_at: Timestamp(0),
        valid_from: None,
        valid_until: None,
        salience: 1.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    };
    s.set_node(node).unwrap();
    let results = s.text_search("keyword", 10);
    assert!(results.iter().any(|(nid, _)| *nid == id));
}

#[test]
fn text_search_substring_score_is_fuzzy() {
    let mut s = SqliteStorage::new().unwrap();
    insert_node_with_content(&mut s, "factory pattern");
    let results = s.text_search("factory", 10);
    assert!(!results.is_empty());
    let score = results[0].1;
    assert!(
        score > 0.0 && score <= 1.0,
        "score should be in (0, 1], got {score}"
    );
}
