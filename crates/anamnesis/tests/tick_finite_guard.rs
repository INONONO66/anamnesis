//! #1b — `tick()` must not hard-crash on a trace-less node (empty access_history).
//!
//! A legacy node whose `access_history` is empty makes `compute_base_level`
//! return `NEG_INFINITY`, so its composite action (`base_level + prior`) is
//! non-finite. Before the fix, `tick()` returned `Err(NonFinite)` on the FIRST
//! such node, aborting the ENTIRE batch and bricking recall (MCP recall ticks
//! every cycle). After the fix, `tick()` degrades that one node to the archive
//! floor (`logistic(-inf) = 0`) and keeps ticking every other node, returning
//! `Ok`.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult, Node};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, NodeId, ScopePath, Timestamp};
use anamnesis::storage::StorageAdapter;
use std::collections::{HashMap, VecDeque};

fn obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1_000),
        valid_from: None,
        valid_until: None,
    }
}

/// Insert a hand-built node with an EMPTY `access_history` (no creation trace),
/// bypassing `ingest` (which seeds a creation trace). This reproduces a legacy
/// node written before the creation-trace invariant existed.
fn insert_traceless_node(engine: &mut Engine, salience: f64, created: u64) -> NodeId {
    let id = engine.graph_mut().next_node_id();
    let node = Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: "traceless-legacy".to_string(),
        summary: None,
        content: "legacy node with no access traces".to_string(),
        embedding: None,
        created_at: Timestamp(created),
        updated_at: Timestamp(created),
        accessed_at: Timestamp(created),
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
            session_id: "legacy".to_string(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    };
    engine
        .graph_mut()
        .add_node(node)
        .expect("insert traceless node");
    id
}

#[test]
fn tick_does_not_abort_on_traceless_node() {
    let mut engine = Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    );

    // A normal ingested node — carries a creation trace, so its base level is
    // finite and it ticks normally.
    let IngestResult::Created(normal_ids) = engine.ingest(obs("healthy")).unwrap() else {
        panic!("expected Created");
    };
    let normal_id = normal_ids[0];

    // A trace-less legacy node — empty access_history ⇒ compute_base_level = -inf.
    let traceless_id = insert_traceless_node(&mut engine, 0.7, 1_000);

    // Tick well past creation so the healthy node measurably decays.
    let now = Timestamp(1_000 + 30 * 86_400_000);
    let report = engine.tick(now);

    // One trace-less node must NOT abort the whole batch tick.
    assert!(
        report.is_ok(),
        "tick must not abort on a trace-less node, got {report:?}"
    );

    // The trace-less node degrades to the archive floor (logistic(-inf) = 0).
    let traceless_salience = engine
        .graph()
        .storage()
        .get_salience(traceless_id)
        .expect("traceless node still exists");
    assert!(
        traceless_salience.abs() < 1e-9,
        "trace-less node should be floored to archive salience ~0, got {traceless_salience}"
    );

    // The healthy node still exists and ticked normally (finite salience in [0,1]).
    let normal_salience = engine
        .graph()
        .storage()
        .get_salience(normal_id)
        .expect("healthy node still exists");
    assert!(
        normal_salience.is_finite() && (0.0..=1.0).contains(&normal_salience),
        "healthy node should have a finite salience in [0,1], got {normal_salience}"
    );
}
