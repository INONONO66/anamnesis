//! `GraphHealth` (nine observability.md metrics), the `InvariantCheck` suite, and
//! `OperationalWarning`s. Rewritten for the conductive-network model: health is
//! now reported as ratios/entropies/distributions over the reservoirs and their
//! projections, never as the legacy force-model counts.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult, OperationalWarning, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::mechanics::observability::InvariantCheck;

fn make_origin(scope: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: ScopePath::new(scope).expect("valid scope"),
        confidence: 0.9,
    }
}

fn make_observation_in(name: &str, node_type: KnowledgeType, scope: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec![],
        origin: make_origin(scope),
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn ingest_in(
    engine: &mut Engine,
    name: &str,
    node_type: KnowledgeType,
    scope: &str,
) -> anamnesis::engine::NodeId {
    let result = engine
        .ingest(make_observation_in(name, node_type, scope))
        .unwrap();
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    }
}

fn ingest_node(
    engine: &mut Engine,
    name: &str,
    node_type: KnowledgeType,
) -> anamnesis::engine::NodeId {
    ingest_in(engine, name, node_type, "project-a")
}

// ── GraphHealth: the nine metrics ───────────────────────────────────────────

#[test]
fn empty_graph_zero_metrics() {
    let engine = Engine::new();
    let health = engine.graph_health_at(Timestamp(0));

    assert_eq!(health.node_count, 0);
    assert_eq!(health.edge_count, 0);
    assert_eq!(health.orphan_ratio, 0.0);
    assert_eq!(health.contradiction_ratio, 0.0);
    assert_eq!(health.salience_entropy, 0.0);
    assert_eq!(health.conductance_entropy, 0.0);
    assert_eq!(health.average_degree, 0.0);
    assert!(health.scope_distribution.is_empty());
    assert_eq!(health.stale_ratio, 0.0);
}

#[test]
fn health_is_read_only_salience_unchanged() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let id1 = ingest_node(&mut engine, "node-1", KnowledgeType::Semantic);
    let id2 = ingest_node(&mut engine, "node-2", KnowledgeType::Episodic);

    let before_1 = engine.graph().storage().get_salience(id1).unwrap();
    let before_2 = engine.graph().storage().get_salience(id2).unwrap();
    let action_before_1 = engine.graph().storage().get_retained_action(id1).unwrap();

    let _health = engine.graph_health_at(Timestamp(2000));

    assert_eq!(
        before_1,
        engine.graph().storage().get_salience(id1).unwrap()
    );
    assert_eq!(
        before_2,
        engine.graph().storage().get_salience(id2).unwrap()
    );
    assert_eq!(
        action_before_1,
        engine.graph().storage().get_retained_action(id1).unwrap()
    );
}

#[test]
fn single_orphan_drives_orphan_ratio() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "lonely", KnowledgeType::Semantic);

    let health = engine.graph_health_at(Timestamp(1000));
    assert_eq!(health.node_count, 1);
    assert_eq!(health.edge_count, 0);
    assert_eq!(health.orphan_ratio, 1.0);
    assert_eq!(health.average_degree, 0.0);
}

#[test]
fn orphan_ratio_and_average_degree_star_topology() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let center = ingest_node(&mut engine, "center", KnowledgeType::Entity);
    let leaf1 = ingest_node(&mut engine, "leaf-1", KnowledgeType::Semantic);
    let leaf2 = ingest_node(&mut engine, "leaf-2", KnowledgeType::Semantic);
    let leaf3 = ingest_node(&mut engine, "leaf-3", KnowledgeType::Semantic);

    engine.link(center, leaf1, EdgeType::Semantic).unwrap();
    engine.link(center, leaf2, EdgeType::Semantic).unwrap();
    engine.link(center, leaf3, EdgeType::Semantic).unwrap();

    let health = engine.graph_health_at(Timestamp(1000));
    assert_eq!(health.node_count, 4);
    assert_eq!(health.edge_count, 3);
    assert_eq!(health.orphan_ratio, 0.0);
    // 2 * 3 edges / 4 nodes = 1.5 mean degree.
    assert!((health.average_degree - 1.5).abs() < 1e-12);
}

#[test]
fn contradiction_ratio_reflects_tension_edges() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    let n3 = ingest_node(&mut engine, "n3", KnowledgeType::Semantic);
    let n4 = ingest_node(&mut engine, "n4", KnowledgeType::Semantic);

    engine.link(n1, n2, EdgeType::Contradicts).unwrap();
    engine.link(n3, n4, EdgeType::Semantic).unwrap();

    let health = engine.graph_health_at(Timestamp(1000));
    // 1 of 2 edges is a Contradicts edge.
    assert!((health.contradiction_ratio - 0.5).abs() < 1e-12);
}

#[test]
fn salience_entropy_zero_for_single_bucket() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n3", KnowledgeType::Semantic);

    // Fresh sites all project near salience 1.0 → single bucket → zero entropy.
    let health = engine.graph_health_at(Timestamp(1000));
    assert_eq!(health.salience_entropy, 0.0);
}

#[test]
fn conductance_entropy_zero_for_no_edges() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);

    let health = engine.graph_health_at(Timestamp(1000));
    assert_eq!(health.conductance_entropy, 0.0);
}

#[test]
fn scope_distribution_counts_by_scope() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    ingest_in(&mut engine, "a1", KnowledgeType::Semantic, "project-a");
    ingest_in(&mut engine, "a2", KnowledgeType::Semantic, "project-a");
    ingest_in(&mut engine, "b1", KnowledgeType::Semantic, "project-b");

    let health = engine.graph_health_at(Timestamp(1000));
    assert_eq!(health.scope_distribution.get("project-a"), Some(&2));
    assert_eq!(health.scope_distribution.get("project-b"), Some(&1));
}

#[test]
fn stale_ratio_grows_with_elapsed_time() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n2", KnowledgeType::Semantic);

    // Just after creation: nothing is stale.
    let fresh = engine.graph_health_at(Timestamp(2000));
    assert_eq!(fresh.stale_ratio, 0.0);

    // 60 days later (window is 30 days): everything is stale.
    let day_ms = 86_400_000u64;
    let stale = engine.graph_health_at(Timestamp(1000 + 60 * day_ms));
    assert_eq!(stale.stale_ratio, 1.0);
}

#[test]
fn node_and_edge_counts_match_graph() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Episodic);
    let n3 = ingest_node(&mut engine, "n3", KnowledgeType::Entity);

    engine.link(n1, n2, EdgeType::Semantic).unwrap();
    engine.link(n2, n3, EdgeType::Causal).unwrap();

    let health = engine.graph_health_at(Timestamp(1000));
    assert_eq!(health.node_count, engine.graph().node_count());
    assert_eq!(health.edge_count, engine.graph().edge_count());
}

#[test]
fn graph_health_is_deterministic() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n2", KnowledgeType::Episodic);

    let h1 = engine.graph_health_at(Timestamp(5000));
    let h2 = engine.graph_health_at(Timestamp(5000));
    assert_eq!(h1, h2);
}

// ── InvariantCheck suite ────────────────────────────────────────────────────

#[test]
fn healthy_graph_passes_all_structural_invariants() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    engine.link(n1, n2, EdgeType::Semantic).unwrap();

    let report = engine.check_invariants(None);
    assert!(
        report.all_passed(),
        "expected clean invariants, got violations: {:?}",
        report.violations().collect::<Vec<_>>()
    );

    // Projection range and finiteness must always hold post-commit-discipline.
    assert!(report.get(InvariantCheck::ProjectionRange).unwrap().passed);
    assert!(
        report
            .get(InvariantCheck::NonFiniteHotFields)
            .unwrap()
            .passed
    );
    assert!(report.get(InvariantCheck::ReservoirFinite).unwrap().passed);
    assert!(
        report
            .get(InvariantCheck::SnapshotRestoreConsistency)
            .unwrap()
            .passed
    );
}

#[test]
fn private_scope_leakage_is_clean_for_hierarchical_scopes() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Two nodes in related (parent/child) scopes linked: not a leak.
    let parent = ingest_in(&mut engine, "parent", KnowledgeType::Semantic, "org");
    let child = ingest_in(&mut engine, "child", KnowledgeType::Semantic, "org/team");
    engine.link(parent, child, EdgeType::Semantic).unwrap();

    let report = engine.check_invariants(None);
    assert!(
        report
            .get(InvariantCheck::PrivateScopeLeakage)
            .unwrap()
            .passed
    );
}

#[test]
fn private_scope_leakage_detects_bridge_across_disjoint_scopes() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Two nodes in disjoint (Unrelated) private scopes, neither universal.
    let personal = ingest_in(
        &mut engine,
        "personal note",
        KnowledgeType::Semantic,
        "personal/foo",
    );
    let work = ingest_in(
        &mut engine,
        "work note",
        KnowledgeType::Semantic,
        "work/bar",
    );

    // A propagating (non-Contradicts) edge bridging them is a leak: it lets
    // private knowledge in one scope light up a node a query in the other,
    // disjoint scope can reach.
    engine.link(personal, work, EdgeType::Semantic).unwrap();

    let report = engine.check_invariants(None);
    let leakage = report
        .get(InvariantCheck::PrivateScopeLeakage)
        .expect("leakage result present");
    assert!(
        !leakage.passed,
        "expected a private-scope leakage violation, got: {leakage:?}"
    );
    assert_eq!(leakage.violation_count, 1);
}

#[test]
fn private_scope_leakage_ignores_contradicts_bridge() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Same disjoint scopes, but a Contradicts edge does not propagate flow, so
    // it is not a leak — tension surfaces across scopes without leaking activation.
    let personal = ingest_in(
        &mut engine,
        "personal note",
        KnowledgeType::Semantic,
        "personal/foo",
    );
    let work = ingest_in(
        &mut engine,
        "work note",
        KnowledgeType::Semantic,
        "work/bar",
    );
    engine.link(personal, work, EdgeType::Contradicts).unwrap();

    let report = engine.check_invariants(None);
    assert!(
        report
            .get(InvariantCheck::PrivateScopeLeakage)
            .unwrap()
            .passed
    );
}

#[test]
fn determinism_invariant_holds_for_repeated_query() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "auth uses factory", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "factory pattern note", KnowledgeType::Semantic);
    engine.link(n1, n2, EdgeType::Semantic).unwrap();

    let probe = anamnesis::engine::SearchInput {
        text: "factory".to_string(),
        scope: ScopePath::new("project-a").unwrap(),
        now: Timestamp(2000),
        limit: 10,
        ..Default::default()
    };

    let report = engine.check_invariants(Some(&probe));
    assert!(
        report.get(InvariantCheck::Determinism).unwrap().passed,
        "determinism violation: {:?}",
        report.get(InvariantCheck::Determinism)
    );
}

// ── OperationalWarnings ─────────────────────────────────────────────────────

#[test]
fn healthy_small_graph_has_no_warnings() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    engine.link(n1, n2, EdgeType::Semantic).unwrap();

    // Reference time just after creation (well inside the stale window).
    assert!(engine.operational_warnings_at(Timestamp(2000)).is_empty());
}

#[test]
fn many_orphans_trigger_orphan_warning() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Ten orphan nodes → orphan_ratio = 1.0 > 0.30 threshold.
    for i in 0..10 {
        ingest_node(&mut engine, &format!("orphan-{i}"), KnowledgeType::Semantic);
    }

    assert!(
        engine
            .operational_warnings_at(Timestamp(2000))
            .contains(&OperationalWarning::HighOrphanRatio)
    );
}
