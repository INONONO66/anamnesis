use anamnesis::Node;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, NodeId, Timestamp};
use std::collections::{HashMap, VecDeque};

fn make_test_node() -> Node {
    Node {
        id: NodeId(0),
        node_type: KnowledgeType::Semantic,
        name: "test".to_string(),
        summary: None,
        content: "test content".to_string(),
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
            session_id: "s".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 1.0,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    }
}

#[test]
fn access_history_starts_empty() {
    let node = make_test_node();
    assert_eq!(node.access_history.len(), 0);
}

#[test]
fn access_history_caps_at_32() {
    let mut node = make_test_node();
    for i in 0..35u64 {
        node.record_access(Timestamp(i));
    }
    assert_eq!(node.access_history.len(), 32);
    assert!(
        node.access_history.front().unwrap().0 >= 3,
        "oldest entry should be at index 3 or later, got {}",
        node.access_history.front().unwrap().0
    );
}

#[test]
fn access_history_preserves_order() {
    let mut node = make_test_node();
    node.record_access(Timestamp(10));
    node.record_access(Timestamp(20));
    node.record_access(Timestamp(30));
    assert_eq!(node.access_history.front().unwrap().0, 10);
    assert_eq!(node.access_history.back().unwrap().0, 30);
}
