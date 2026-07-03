//! Idle-edge leak checkpoint idempotency (flagship bug #2).
//!
//! The idle-edge conductance leak in `Engine::tick` must be frequency-independent:
//! calling `tick` multiple times at the SAME `now` must charge the idle-window
//! leak only ONCE (via a per-edge `leaked_at` checkpoint), not once per call.
//!
//! Before the fix, the leak baseline was `accessed_at` — which marks committed
//! USE, not leak history, and is never advanced by a leak (correctly: leakage is
//! not a use). With no per-edge leak checkpoint, every repeated `tick` at a fixed
//! idle window re-subtracted the SAME full idle-window leak again, so an idle
//! edge's conductance collapsed toward zero the MORE the graph was ticked (and
//! MCP `recall` ticked the engine twice per call, doubling the effect on every
//! read — see the companion fix in `anamnesis-mcp`).

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{IngestResult, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};

const DAY_MS: u64 = 86_400_000;

fn observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: ScopePath::new("project-1").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp::now(),
        valid_from: None,
        valid_until: None,
    }
}

fn insert(engine: &mut Engine, name: &str) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name)).unwrap() else {
        panic!("expected node creation for {name}");
    };
    ids[0]
}

/// N=5 ticks at the SAME `now` must leak the idle window only ONCE: the
/// per-edge leak checkpoint absorbs repeat calls after the first successful
/// leak. Conductance after tick #2 and after tick #5 must be identical (within
/// float noise), even though `tick` was called 5 times total.
#[test]
fn repeated_tick_at_same_now_leaks_idle_edge_only_once() {
    let mut engine = Engine::new();
    let a = insert(&mut engine, "A");
    let b = insert(&mut engine, "B");

    let eid = engine.link(a, b, EdgeType::Semantic).unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_conductance(eid, 2.0)
        .unwrap();

    // Far enough past the edge's real creation instant (`link()` stamps
    // `created_at`/`accessed_at` at real `Timestamp::now()`) that the first
    // tick has a substantial idle window to leak.
    let now = Timestamp(Timestamp::now().0 + 400 * DAY_MS);

    let before = engine.conductance(eid).unwrap();
    engine.tick(now).unwrap();
    let after_first = engine.conductance(eid).unwrap();
    assert!(
        after_first < before,
        "first tick must leak the idle edge: {after_first} !< {before}"
    );

    engine.tick(now).unwrap();
    let after_second = engine.conductance(eid).unwrap();

    // Three more ticks at the identical `now` — none of them should leak
    // further once the checkpoint has caught up.
    for _ in 0..3 {
        engine.tick(now).unwrap();
    }
    let after_fifth = engine.conductance(eid).unwrap();

    assert!(
        (after_fifth - after_second).abs() < 1e-9,
        "5 ticks at the same `now` must leak the idle window only once: \
         conductance after tick #2 = {after_second}, after tick #5 = {after_fifth} \
         (delta {})",
        (after_fifth - after_second).abs()
    );
}
