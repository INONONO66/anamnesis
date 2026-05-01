use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
use anamnesis::storage::{InMemoryStorage, StorageAdapter};
use std::collections::{HashMap, VecDeque};

fn insert_node_with_name(s: &mut InMemoryStorage, name: &str) -> NodeId {
    let id = s.next_node_id();
    let node = Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: name.to_string(),
        summary: None,
        content: name.to_string(),
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
            agent_id: "agent-1".to_string(),
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
fn exact_match_returns_score_1() {
    let mut s = InMemoryStorage::new();
    let id = insert_node_with_name(&mut s, "factory pattern");

    let r = s.text_search("factory pattern", 10);

    let score = r
        .iter()
        .find(|(nid, _)| *nid == id)
        .map(|(_, sc)| *sc)
        .unwrap_or(0.0);
    assert!(
        (score - 1.0).abs() < 1e-9,
        "exact match score should be 1.0, got {score}"
    );
}

#[test]
fn idf_weighted_score_in_range() {
    let mut s = InMemoryStorage::new();
    for i in 0..10 {
        insert_node_with_name(&mut s, &format!("auth common {i}"));
    }
    let id_special = insert_node_with_name(&mut s, "auth specific factory");

    let r = s.text_search("specific", 5);

    let score = r
        .iter()
        .find(|(nid, _)| *nid == id_special)
        .map(|(_, sc)| *sc)
        .unwrap_or(0.0);
    assert!(score > 0.0, "IDF match should return non-zero score");
    assert!(
        (0.5..=1.0).contains(&score),
        "IDF match score should be clamped to 0.5..=1.0, got {score}"
    );
}

#[test]
fn fuzzy_match_partial_string() {
    let mut s = InMemoryStorage::new();
    let id = insert_node_with_name(&mut s, "authentication module");

    let r = s.text_search("auth", 5);

    assert!(
        r.iter().any(|(nid, _)| *nid == id),
        "fuzzy match should find 'authentication' for query 'auth'"
    );
}

#[test]
fn stage1_sufficient_skips_later_stages() {
    let mut s = InMemoryStorage::new();
    let id = insert_node_with_name(&mut s, "exact query");
    insert_node_with_name(&mut s, "exact query partial");

    let r = s.text_search("exact query", 1);

    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, id);
    assert!((r[0].1 - 1.0).abs() < 1e-9);
}
