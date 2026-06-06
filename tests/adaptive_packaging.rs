use std::collections::{HashMap, HashSet};

use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::query::assembly::{
    ContradictionPair, ModeContext, ScoredNode, assemble_context_package,
    assemble_context_package_for_mode,
};
use anamnesis::query::types::Query;

fn make_origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: ScopePath::new("proj-a").expect("valid scope"),
        confidence: 0.9,
    }
}

fn make_scored_node(id: u64, kt: KnowledgeType, name: &str, relevance: f64) -> ScoredNode {
    ScoredNode {
        node_id: NodeId(id),
        name: name.to_string(),
        summary: Some(format!("Summary of {name}")),
        content: format!("Full content of {name}"),
        node_type: kt,
        relevance,
        origin: make_origin(),
    }
}

#[test]
fn temporal_query_elevates_memories_to_l1() {
    let nodes = vec![
        make_scored_node(0, KnowledgeType::Episodic, "recent event", 0.9),
        make_scored_node(1, KnowledgeType::Event, "deploy event", 0.8),
        make_scored_node(2, KnowledgeType::Semantic, "a fact", 0.7),
    ];

    let query = Query::Temporal {
        since: Timestamp(0),
        node_types: None,
        limit: 10,
    };

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &HashMap::new(),
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    for frag in &pkg.memories {
        assert!(
            frag.summary.is_some(),
            "Temporal mode: memory node {} should have L1 summary",
            frag.node_id.0,
        );
    }

    for frag in &pkg.knowledge {
        assert!(
            frag.content.is_none(),
            "Temporal mode: knowledge node {} should be at L0 (no content)",
            frag.node_id.0,
        );
    }
}

#[test]
fn neighborhood_shows_adjacent_at_l2() {
    let entity_id = NodeId(0);
    let adjacent_id = NodeId(1);
    let distant_id = NodeId(2);

    let nodes = vec![
        make_scored_node(0, KnowledgeType::Entity, "auth module", 1.0),
        make_scored_node(1, KnowledgeType::Semantic, "adjacent fact", 0.8),
        make_scored_node(2, KnowledgeType::Semantic, "distant fact", 0.5),
    ];

    let query = Query::Neighborhood {
        entity: entity_id,
        depth: 3,
    };

    let mut adjacent_ids = HashSet::new();
    adjacent_ids.insert(adjacent_id);

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &HashMap::new(),
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext { adjacent_ids },
    );

    let adjacent_frag = pkg
        .knowledge
        .iter()
        .find(|f| f.node_id == adjacent_id)
        .expect("adjacent node should be in knowledge");
    assert!(
        adjacent_frag.content.is_some(),
        "Adjacent node should have L2 content"
    );

    let distant_frag = pkg
        .knowledge
        .iter()
        .find(|f| f.node_id == distant_id)
        .expect("distant node should be in knowledge");
    assert!(
        distant_frag.content.is_none(),
        "Distant (non-adjacent) node should be at L1 (no content)"
    );
    assert!(
        distant_frag.summary.is_some(),
        "Distant (non-adjacent) node should have L1 summary"
    );
}

#[test]
fn tension_involved_nodes_include_provenance() {
    let node_a = NodeId(0);
    let node_b = NodeId(1);

    let nodes = vec![
        make_scored_node(0, KnowledgeType::Semantic, "claim A", 0.9),
        make_scored_node(1, KnowledgeType::Semantic, "claim B", 0.8),
        make_scored_node(2, KnowledgeType::Semantic, "unrelated", 0.7),
    ];

    let contradiction_pairs = vec![ContradictionPair {
        node_a,
        node_b,
        edge_weight: 0.9,
        stress: 0.54,
        scope_overlap: 1.0,
        temporal_overlap: 1.0,
    }];
    let mut activations = HashMap::new();
    activations.insert(node_a, 0.8);
    activations.insert(node_b, 0.6);
    activations.insert(NodeId(2), 0.5);

    let query = Query::Temporal {
        since: Timestamp(0),
        node_types: None,
        limit: 10,
    };

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &contradiction_pairs,
        &activations,
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    let tension_frag_a = pkg
        .knowledge
        .iter()
        .find(|f| f.node_id == node_a)
        .expect("tension node A should be present");
    assert!(
        tension_frag_a.summary.is_some(),
        "Tension node should be elevated to at least L1"
    );

    let tension_frag_b = pkg
        .knowledge
        .iter()
        .find(|f| f.node_id == node_b)
        .expect("tension node B should be present");
    assert!(
        tension_frag_b.summary.is_some(),
        "Tension node should be elevated to at least L1"
    );
}

#[test]
fn budget_partitioned_correctly() {
    let mut nodes = Vec::new();
    for i in 0..5 {
        nodes.push(make_scored_node(
            i,
            KnowledgeType::IdentityCore,
            &format!("identity-{i}"),
            0.9 - i as f64 * 0.1,
        ));
    }
    for i in 5..20 {
        nodes.push(make_scored_node(
            i,
            KnowledgeType::Semantic,
            &format!("knowledge-{i}"),
            0.8 - (i - 5) as f64 * 0.05,
        ));
    }
    for i in 20..30 {
        nodes.push(make_scored_node(
            i,
            KnowledgeType::Episodic,
            &format!("memory-{i}"),
            0.7 - (i - 20) as f64 * 0.05,
        ));
    }

    let query = Query::Neighborhood {
        entity: NodeId(0),
        depth: 2,
    };
    let token_budget = 1000;

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &HashMap::new(),
        token_budget,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    let identity_limit = (token_budget as f64 * 0.10) as usize;
    let knowledge_limit = (token_budget as f64 * 0.65) as usize;
    let memory_limit = (token_budget as f64 * 0.20) as usize;

    assert!(
        pkg.token_usage.identity_used <= identity_limit + 50,
        "Identity tokens {} should be near budget limit {}",
        pkg.token_usage.identity_used,
        identity_limit,
    );
    assert!(
        pkg.token_usage.knowledge_used <= knowledge_limit + 50,
        "Knowledge tokens {} should be near budget limit {}",
        pkg.token_usage.knowledge_used,
        knowledge_limit,
    );
    assert!(
        pkg.token_usage.memories_used <= memory_limit + 50,
        "Memory tokens {} should be near budget limit {}",
        pkg.token_usage.memories_used,
        memory_limit,
    );
}

#[test]
fn associative_behavior_preserved() {
    let nodes = vec![
        make_scored_node(0, KnowledgeType::IdentityCore, "identity", 0.95),
        make_scored_node(1, KnowledgeType::Semantic, "knowledge", 0.85),
        make_scored_node(2, KnowledgeType::Episodic, "memory", 0.75),
    ];

    let query = Query::Associative {
        seed: NodeId(0),
        budget: 100,
    };

    let nodes_clone = vec![
        make_scored_node(0, KnowledgeType::IdentityCore, "identity", 0.95),
        make_scored_node(1, KnowledgeType::Semantic, "knowledge", 0.85),
        make_scored_node(2, KnowledgeType::Episodic, "memory", 0.75),
    ];

    let base_pkg =
        assemble_context_package(nodes_clone, &[], &[], 10000, 4, &ScopePath::universal());

    let mode_pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &HashMap::new(),
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    assert_eq!(base_pkg.identity.len(), mode_pkg.identity.len());
    assert_eq!(base_pkg.knowledge.len(), mode_pkg.knowledge.len());
    assert_eq!(base_pkg.memories.len(), mode_pkg.memories.len());

    for (base, mode) in base_pkg.knowledge.iter().zip(mode_pkg.knowledge.iter()) {
        assert_eq!(base.content, mode.content);
        assert_eq!(base.summary, mode.summary);
    }
}

#[test]
fn type_filtered_shows_target_type_at_l2() {
    let nodes = vec![
        make_scored_node(0, KnowledgeType::Convention, "naming convention", 0.9),
        make_scored_node(
            1,
            KnowledgeType::Convention,
            "error handling convention",
            0.8,
        ),
        make_scored_node(2, KnowledgeType::Semantic, "unrelated fact", 0.7),
    ];

    let query = Query::TypeFiltered {
        node_type: KnowledgeType::Convention,
        limit: 10,
    };

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &HashMap::new(),
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    for frag in &pkg.knowledge {
        if frag.node_type == KnowledgeType::Convention {
            assert!(
                frag.content.is_some(),
                "TypeFiltered: target type Convention should have L2 content"
            );
        } else {
            assert!(
                frag.content.is_none(),
                "TypeFiltered: non-target type should not have L2 content"
            );
        }
    }
}

#[test]
fn salience_conditional_memory_elevation() {
    let nodes = vec![
        make_scored_node(0, KnowledgeType::Episodic, "high salience memory", 0.9),
        make_scored_node(1, KnowledgeType::Episodic, "low salience memory", 0.3),
    ];

    let mut activations = HashMap::new();
    activations.insert(NodeId(0), 0.8);
    activations.insert(NodeId(1), 0.2);

    let query = Query::TypeFiltered {
        node_type: KnowledgeType::Semantic,
        limit: 10,
    };

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &activations,
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    let high_salience_mem = pkg
        .memories
        .iter()
        .find(|f| f.node_id == NodeId(0))
        .expect("high salience memory should be present");
    assert!(
        high_salience_mem.summary.is_some(),
        "High salience (>0.7) + strong activation (>0.5) memory should be elevated to L1"
    );

    let low_salience_mem = pkg
        .memories
        .iter()
        .find(|f| f.node_id == NodeId(1))
        .expect("low salience memory should be present");
    assert!(
        low_salience_mem.summary.is_none(),
        "Low salience memory should remain at L0"
    );
}

#[test]
fn list_mode_preserves_base_behavior() {
    let nodes = vec![
        make_scored_node(0, KnowledgeType::Semantic, "fact A", 0.9),
        make_scored_node(1, KnowledgeType::Semantic, "fact B", 0.8),
    ];

    let query = Query::List {
        min_salience: 0.5,
        limit: 10,
    };

    let pkg = assemble_context_package_for_mode(
        nodes,
        &query,
        &[],
        &[],
        &HashMap::new(),
        10000,
        4,
        &ScopePath::universal(),
        &ModeContext::default(),
    );

    assert!(
        pkg.knowledge[0].content.is_some(),
        "List mode with ample budget should have L2 content (base assembler behavior)"
    );
}
