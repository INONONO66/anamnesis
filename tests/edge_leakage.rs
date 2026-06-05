//! Idle-edge leakage end-to-end (`TimeElapsed` on conductance).
//!
//! Proves the documented `TimeElapsed`-on-conductance flow wired into
//! `Engine::tick` (conductance.md "Post-Commit Plasticity" leak term
//! `- eta_leak * idle_edge_leakage_ij`; interactions.md `TimeElapsed`
//! `C_ij' = leak_idle_edge(C_ij, idle_days)`; dissipation.md edge leakage):
//!
//! - an edge unused for a long idle interval has its authoritative conductance
//!   reservoir `C_ij` (and therefore its bounded `weight` projection) strictly
//!   decrease after `tick`;
//! - a recently-committed edge (`accessed_at ≈ now`, zero idle days) is left
//!   unchanged;
//! - reservoirs and projections stay finite throughout.
//!
//! Idle time is `now - edge.accessed_at` (the committed-use timestamp). `tick`
//! is an allowed projection writer (ADR-0002): it leaks the reservoir and
//! re-projects `weight = project_weight(C_ij')`. Conductance is never authored
//! directly — the test seeds the reservoir through the storage layer and lets
//! `tick` perform the documented leak.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeId, EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::mechanics::priors::project_weight;
use anamnesis::{Engine, IngestResult, StorageAdapter};

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
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: ScopePath::new("project-1").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
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

/// Create a `Semantic` edge and pin its authoritative conductance reservoir and
/// committed-use timestamp deterministically, so the leak assertion does not
/// depend on cold-start coupling magnitudes. `link()` creates the edge;
/// `set_conductance` re-projects `weight` (ADR-0002), `set_edge_accessed_at`
/// fixes the idle baseline.
fn seed_edge(
    engine: &mut Engine,
    from: NodeId,
    to: NodeId,
    conductance: f64,
    accessed_at: Timestamp,
) -> EdgeId {
    let eid = engine.link(from, to, EdgeType::Semantic).unwrap();
    let storage = engine.graph_mut().storage_mut();
    storage.set_conductance(eid, conductance).unwrap();
    storage.set_edge_accessed_at(eid, accessed_at).unwrap();
    eid
}

#[test]
fn idle_edge_conductance_and_weight_strictly_decrease_after_tick() {
    let mut engine = Engine::new();
    let a = insert(&mut engine, "A");
    let b = insert(&mut engine, "B");

    // Edge last committed at t=1000, conductance well above the zero-coupling
    // floor so there is coupling to drain.
    let base = Timestamp(1000);
    let eid = seed_edge(&mut engine, a, b, 2.0, base);

    let before_c = engine.conductance(eid).unwrap();
    let before_w = engine.graph().get_edge(eid).unwrap().weight;

    // Tick after a long idle interval — the edge is unused, so it must leak.
    let later = Timestamp(1000 + 365 * DAY_MS);
    let report = engine.tick(later).unwrap();

    let after_c = engine.conductance(eid).unwrap();
    let edge_after = engine.graph().get_edge(eid).unwrap();
    let after_w = edge_after.weight;

    // Conductance reservoir strictly decreased toward the zero-coupling floor.
    assert!(
        after_c < before_c,
        "idle edge conductance must strictly decrease: {after_c} !< {before_c}"
    );
    // The bounded weight projection dropped with it.
    assert!(
        after_w < before_w,
        "idle edge weight must strictly decrease: {after_w} !< {before_w}"
    );
    // Reservoir and projection remain finite.
    assert!(after_c.is_finite(), "conductance must stay finite: {after_c}");
    assert!(after_w.is_finite(), "weight must stay finite: {after_w}");
    // ADR-0002: tick re-projects weight = project_weight(C_ij') (it is an
    // allowed projection writer); the stored weight matches the reservoir.
    assert!(
        (after_w - project_weight(after_c)).abs() < 1e-12,
        "weight must be re-projected from the leaked reservoir: {after_w} vs {}",
        project_weight(after_c)
    );
    // The tick report accounts the leak.
    assert_eq!(report.edges_leaked, 1, "exactly the idle edge leaked");
    assert!(
        report.total_conductance_delta > 0.0,
        "leak must register a positive conductance-projection delta"
    );
}

#[test]
fn recently_committed_edge_is_unchanged_after_tick() {
    let mut engine = Engine::new();
    let a = insert(&mut engine, "A");
    let b = insert(&mut engine, "B");

    // Edge committed AT the tick time → zero idle days → no leak.
    let tick_time = Timestamp(1000 + 365 * DAY_MS);
    let eid = seed_edge(&mut engine, a, b, 2.0, tick_time);

    let before_c = engine.conductance(eid).unwrap();
    let before_w = engine.graph().get_edge(eid).unwrap().weight;

    let report = engine.tick(tick_time).unwrap();

    let after_c = engine.conductance(eid).unwrap();
    let after_w = engine.graph().get_edge(eid).unwrap().weight;

    assert_eq!(
        after_c, before_c,
        "recently-committed edge conductance must not change"
    );
    assert_eq!(
        after_w, before_w,
        "recently-committed edge weight must not change"
    );
    assert_eq!(report.edges_leaked, 0, "no edge should leak");
    assert!(after_c.is_finite() && after_w.is_finite());
}

#[test]
fn idle_and_fresh_edges_diverge_in_one_tick() {
    // Side-by-side: identical seeded conductance, different committed-use times.
    // Only the idle edge leaks; reservoirs stay finite.
    let mut engine = Engine::new();
    let a = insert(&mut engine, "A");
    let b = insert(&mut engine, "B");
    let c = insert(&mut engine, "C");

    let tick_time = Timestamp(1000 + 200 * DAY_MS);
    // Idle since t=1000; fresh as of the tick time.
    let idle = seed_edge(&mut engine, a, b, 1.5, Timestamp(1000));
    let fresh = seed_edge(&mut engine, a, c, 1.5, tick_time);

    let idle_before = engine.conductance(idle).unwrap();
    let fresh_before = engine.conductance(fresh).unwrap();

    let report = engine.tick(tick_time).unwrap();

    let idle_after = engine.conductance(idle).unwrap();
    let fresh_after = engine.conductance(fresh).unwrap();

    assert!(
        idle_after < idle_before,
        "idle edge must leak: {idle_after} !< {idle_before}"
    );
    assert_eq!(
        fresh_after, fresh_before,
        "fresh edge must be unchanged: {fresh_after} != {fresh_before}"
    );
    assert_eq!(report.edges_leaked, 1, "only the idle edge leaked");
    assert!(idle_after.is_finite() && fresh_after.is_finite());
}

#[test]
fn longer_idle_leaks_at_least_as_much() {
    // More idle time leaks at least as much conductance (monotone in idle days),
    // and never raises it (no reverse flow). Two independent engines, identical
    // seed, different idle intervals.
    let leaked_after = |idle_days: u64| -> f64 {
        let mut engine = Engine::new();
        let a = insert(&mut engine, "A");
        let b = insert(&mut engine, "B");
        let eid = seed_edge(&mut engine, a, b, 2.0, Timestamp(1000));
        engine
            .tick(Timestamp(1000 + idle_days * DAY_MS))
            .unwrap();
        engine.conductance(eid).unwrap()
    };

    let short = leaked_after(30);
    let long = leaked_after(365);

    assert!(short <= 2.0, "30-day idle must not raise conductance: {short}");
    assert!(
        long <= short,
        "365-day idle must leak at least as much as 30-day: {long} !<= {short}"
    );
    assert!(short.is_finite() && long.is_finite());
}
