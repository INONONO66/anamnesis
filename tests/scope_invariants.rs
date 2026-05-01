//! Tier A scope relation + scope_index invariant tests.
//!
//! Locked T16 weights asserted here:
//! - Exact: 1.0
//! - Universal: 0.95
//! - Ancestor: 0.85
//! - Descendant: 0.85
//! - Sibling: 0.50
//! - Unrelated: 0.05 + shared-entity bonus capped at +0.20

use std::collections::{HashMap, VecDeque};

use anamnesis::api::Observation;
use anamnesis::error::Error;
use anamnesis::graph::node::Origin;
use anamnesis::graph::scope::{ScopePath, ScopeRelation};
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, Timestamp};
use anamnesis::query::{SearchInput, scope_weight};
use anamnesis::{Engine, EngineConfig, InMemoryStorage, IngestResult, NodeId, StorageAdapter};

// ===== 6 Relation Cases =====

#[test]
fn relation_exact_returns_full_weight() {
    let a = ScopePath::new("personal/foo").unwrap();
    let b = ScopePath::new("personal/foo").unwrap();
    assert_eq!(a.relation_to(&b), ScopeRelation::Exact);
    let w = scope_weight(&a, &b, 0);
    assert!(
        (w - 1.0).abs() < 1e-10,
        "expected Exact weight 1.0, got {w}"
    );
}

#[test]
fn relation_ancestor_returns_locked_weight() {
    let query = ScopePath::new("personal").unwrap();
    let node = ScopePath::new("personal/foo").unwrap();
    assert_eq!(query.relation_to(&node), ScopeRelation::Ancestor);
    let w = scope_weight(&query, &node, 0);
    assert!(
        (w - 0.85).abs() < 1e-10,
        "expected Ancestor weight 0.85, got {w}"
    );
}

#[test]
fn relation_descendant_returns_locked_weight() {
    let query = ScopePath::new("personal/foo").unwrap();
    let node = ScopePath::new("personal").unwrap();
    assert_eq!(query.relation_to(&node), ScopeRelation::Descendant);
    let w = scope_weight(&query, &node, 0);
    assert!(
        (w - 0.85).abs() < 1e-10,
        "expected Descendant weight 0.85, got {w}"
    );
}

#[test]
fn relation_sibling_returns_locked_weight() {
    let query = ScopePath::new("personal/foo").unwrap();
    let node = ScopePath::new("personal/bar").unwrap();
    assert_eq!(query.relation_to(&node), ScopeRelation::Sibling);
    let w = scope_weight(&query, &node, 0);
    assert!(
        (w - 0.50).abs() < 1e-10,
        "expected Sibling weight 0.50, got {w}"
    );
}

#[test]
fn relation_universal_returns_locked_weight() {
    let query = ScopePath::new("personal").unwrap();
    let universal = ScopePath::universal();

    assert_eq!(query.relation_to(&universal), ScopeRelation::Universal);
    assert_eq!(universal.relation_to(&query), ScopeRelation::Universal);

    let w_qn = scope_weight(&query, &universal, 0);
    let w_nq = scope_weight(&universal, &query, 0);
    assert!(
        (w_qn - 0.95).abs() < 1e-10,
        "expected Universal weight 0.95 (query→node), got {w_qn}"
    );
    assert!(
        (w_nq - 0.95).abs() < 1e-10,
        "expected Universal weight 0.95 (node→query), got {w_nq}"
    );
}

#[test]
fn relation_unrelated_returns_locked_weight_with_bonus_cap() {
    let query = ScopePath::new("work").unwrap();
    let node = ScopePath::new("personal").unwrap();
    assert_eq!(query.relation_to(&node), ScopeRelation::Unrelated);

    let base = scope_weight(&query, &node, 0);
    assert!(
        (base - 0.05).abs() < 1e-10,
        "expected Unrelated base weight 0.05, got {base}"
    );

    let bonus_max = scope_weight(&query, &node, 100);
    assert!(
        bonus_max <= 0.05 + 0.20 + 1e-10,
        "expected Unrelated bonus capped at +0.20 (<= 0.25), got {bonus_max}"
    );
    assert!(
        bonus_max > base,
        "shared-entity bonus must increase weight above base 0.05, got {bonus_max}"
    );
}

// ===== 6 Path-Shape Cases =====

#[test]
fn path_universal_is_empty_string() {
    let p = ScopePath::universal();
    assert!(p.is_universal());
    assert_eq!(p.as_str(), "");
}

#[test]
fn path_single_segment_accepted() {
    let p = ScopePath::new("personal").unwrap();
    assert_eq!(p.as_str(), "personal");
    assert!(!p.is_universal());
}

#[test]
fn path_deeply_nested_accepted() {
    let p = ScopePath::new("a/b/c/d/e/f/g/h/i/j").unwrap();
    assert_eq!(p.as_str(), "a/b/c/d/e/f/g/h/i/j");
}

#[test]
fn path_trailing_slash_trimmed() {
    let p = ScopePath::new("personal/foo/").unwrap();
    assert_eq!(p.as_str(), "personal/foo");
    let p2 = ScopePath::new("personal/foo///").unwrap();
    assert_eq!(p2.as_str(), "personal/foo");
}

#[test]
fn path_double_slash_rejected() {
    let r = ScopePath::new("a//b");
    match r {
        Err(Error::InvalidInput(msg)) => {
            assert!(
                msg.contains("consecutive slashes"),
                "error message must mention consecutive slashes, got {msg}"
            );
        }
        other => panic!("expected InvalidInput for double slash, got {other:?}"),
    }
}

#[test]
fn path_leading_slash_rejected_as_empty_segment() {
    let r = ScopePath::new("/personal/foo");
    match r {
        Err(Error::InvalidInput(_)) => {}
        other => panic!("expected InvalidInput for leading slash, got {other:?}"),
    }
}

// ===== 3 scope_index Lifecycle Tests =====

fn make_indexed_node(id: NodeId, scope: &str) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id.0),
        summary: None,
        content: "scope invariant fixture".to_string(),
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
fn scope_index_lookup_o1_after_insert() {
    let mut storage = InMemoryStorage::new();
    let id_a = storage.next_node_id();
    let id_b = storage.next_node_id();
    let id_c = storage.next_node_id();

    storage
        .set_node(make_indexed_node(id_a, "personal/foo"))
        .unwrap();
    storage
        .set_node(make_indexed_node(id_b, "personal/foo"))
        .unwrap();
    storage
        .set_node(make_indexed_node(id_c, "work/bar"))
        .unwrap();

    let target = ScopePath::new("personal/foo").unwrap();
    let other = ScopePath::new("work/bar").unwrap();
    let absent = ScopePath::new("never/inserted").unwrap();

    assert_eq!(storage.nodes_by_scope(&target), vec![id_a, id_b]);
    assert_eq!(storage.nodes_by_scope(&other), vec![id_c]);
    assert!(storage.nodes_by_scope(&absent).is_empty());
}

#[test]
fn scope_index_clone_round_trip() {
    let mut storage = InMemoryStorage::new();
    let a = storage.next_node_id();
    let b = storage.next_node_id();
    storage
        .set_node(make_indexed_node(a, "personal/foo"))
        .unwrap();
    storage.set_node(make_indexed_node(b, "work/bar")).unwrap();

    let cloned = storage.clone();

    let pscope = ScopePath::new("personal/foo").unwrap();
    let wscope = ScopePath::new("work/bar").unwrap();

    assert_eq!(
        cloned.nodes_by_scope(&pscope),
        storage.nodes_by_scope(&pscope)
    );
    assert_eq!(
        cloned.nodes_by_scope(&wscope),
        storage.nodes_by_scope(&wscope)
    );
    assert_eq!(cloned.nodes_by_scope(&pscope), vec![a]);
    assert_eq!(cloned.nodes_by_scope(&wscope), vec![b]);
}

#[test]
fn scope_index_delete_then_reuse_node_id() {
    let mut storage = InMemoryStorage::new();
    let id = storage.next_node_id();
    storage
        .set_node(make_indexed_node(id, "personal/foo"))
        .unwrap();
    storage.delete_node(id).unwrap();

    let old_scope = ScopePath::new("personal/foo").unwrap();
    let new_scope = ScopePath::new("work/bar").unwrap();

    assert!(
        storage.nodes_by_scope(&old_scope).is_empty(),
        "deleted id must not remain under old scope"
    );

    let reused = storage.next_node_id();
    assert_eq!(reused, id, "id should be recycled after delete");
    storage
        .set_node(make_indexed_node(reused, "work/bar"))
        .unwrap();

    assert!(
        storage.nodes_by_scope(&old_scope).is_empty(),
        "reused id must not appear under old scope"
    );
    assert_eq!(
        storage.nodes_by_scope(&new_scope),
        vec![reused],
        "reused id must appear only under new scope"
    );
}

// ===== Cross-Search Scope Downweighting =====

#[test]
fn search_personal_foo_downweights_work_bar_vs_personal_ancestor() {
    let config = EngineConfig::default()
        .with_dedup_enabled(false)
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let ancestor_origin = Origin {
        agent_id: "agent".to_string(),
        session_id: "session".to_string(),
        scope: ScopePath::new("personal").unwrap(),
        confidence: 1.0,
    };
    let unrelated_origin = Origin {
        agent_id: "agent".to_string(),
        session_id: "session".to_string(),
        scope: ScopePath::new("work/bar").unwrap(),
        confidence: 1.0,
    };

    let make_obs = |name: &str, origin: Origin| Observation {
        name: name.to_string(),
        summary: Some(format!("summary {name}")),
        content: "widget gadget knowledge fragment".to_string(),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: Vec::new(),
        origin,
        timestamp: Timestamp(0),
    };

    let ancestor_id = match engine
        .ingest(make_obs("ancestor-knowledge", ancestor_origin))
        .expect("ingest ancestor should succeed")
    {
        IngestResult::Created(ids) => ids[0],
        other => panic!("expected Created for ancestor ingest, got {other:?}"),
    };
    let unrelated_id = match engine
        .ingest(make_obs("unrelated-knowledge", unrelated_origin))
        .expect("ingest unrelated should succeed")
    {
        IngestResult::Created(ids) => ids[0],
        other => panic!("expected Created for unrelated ingest, got {other:?}"),
    };

    let query_scope = ScopePath::new("personal/foo").unwrap();
    let ancestor_scope = ScopePath::new("personal").unwrap();
    let unrelated_scope = ScopePath::new("work/bar").unwrap();
    assert_eq!(
        query_scope.relation_to(&ancestor_scope),
        ScopeRelation::Descendant
    );
    assert_eq!(
        query_scope.relation_to(&unrelated_scope),
        ScopeRelation::Unrelated
    );

    let result = engine
        .search(SearchInput {
            text: "widget gadget".to_string(),
            scope: query_scope,
            limit: 10,
            seed_limit: Some(5),
            ..Default::default()
        })
        .expect("search should succeed");

    let ancestor_relevance = result
        .package
        .knowledge
        .iter()
        .find(|f| f.node_id == ancestor_id)
        .map(|f| f.relevance);
    let unrelated_relevance = result
        .package
        .knowledge
        .iter()
        .find(|f| f.node_id == unrelated_id)
        .map(|f| f.relevance);

    match (ancestor_relevance, unrelated_relevance) {
        (Some(anc), Some(unr)) => {
            assert!(
                anc > unr,
                "ancestor scope (personal) relevance {anc} must exceed unrelated (work/bar) relevance {unr}"
            );
        }
        (Some(_), None) => {}
        (None, _) => panic!(
            "ancestor-related fragment must be present in package; got ancestor={ancestor_relevance:?}, unrelated={unrelated_relevance:?}"
        ),
    }

    let positions: HashMap<NodeId, usize> = result
        .package
        .knowledge
        .iter()
        .enumerate()
        .map(|(i, f)| (f.node_id, i))
        .collect();
    if let (Some(&anc_pos), Some(&unr_pos)) =
        (positions.get(&ancestor_id), positions.get(&unrelated_id))
    {
        assert!(
            anc_pos < unr_pos,
            "ancestor fragment at position {anc_pos} must precede unrelated at {unr_pos}"
        );
    }

    if let Some(frag) = result
        .package
        .knowledge
        .iter()
        .find(|f| f.node_id == ancestor_id)
    {
        assert_eq!(
            frag.scope,
            ScopeRelation::Descendant,
            "ancestor fragment must be tagged Descendant relative to query"
        );
    }
}
