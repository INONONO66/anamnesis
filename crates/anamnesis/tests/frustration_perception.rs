//! Phase 4 integration: frustration surfaces (never suppresses) and perception is
//! surprise-gated (familiar routes, novel allocates with surprise charge).
//!
//! Proves the migration-design P4 MUST-invariants:
//! - a Contradicts pair both appear in the result with `stress > 0`, neither has
//!   reduced activation (ADR-0006 surface-not-suppress);
//! - `sigma = 0` when a gate (scope) is closed;
//! - more-surprising input → higher initial `A_i`;
//! - familiar input routes-and-reinforces, never rejects (ADR-0009).

use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::engine::StorageAdapter;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::peer::SourceKind;
use anamnesis::query::{Query, QueryConfig};

fn obs_scoped(name: &str, embedding: Vec<f64>, scope: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: Some(embedding),
        confidence: 0.95,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: ScopePath::new(scope).expect("valid scope"),
            confidence: 0.95,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn created(result: Result<IngestResult, anamnesis::Error>) -> NodeId {
    match result.expect("ingest should succeed") {
        IngestResult::Created(ids) => ids[0],
        other => panic!("expected a created node, got {other:?}"),
    }
}

/// A Contradicts pair, both reached by activation, surfaces as a tension with
/// stress > 0 — and neither side is removed from the package (surfaced, not
/// suppressed; ADR-0006).
#[test]
fn contradiction_surfaces_with_stress_and_is_not_suppressed() {
    let mut e = engine();

    // hub -> a, hub -> b (Semantic propagating); a <-> b Contradicts.
    let hub = created(e.ingest(obs_scoped("hub topic", vec![1.0, 0.0, 0.0], "proj")));
    let a = created(e.ingest(obs_scoped("claim a", vec![0.0, 1.0, 0.0], "proj")));
    let b = created(e.ingest(obs_scoped("claim b", vec![0.0, 0.0, 1.0], "proj")));

    e.link(hub, a, EdgeType::Semantic).unwrap();
    e.link(hub, b, EdgeType::Semantic).unwrap();
    e.link(a, b, EdgeType::Contradicts).unwrap();

    let pkg = e
        .query(
            &Query::Associative {
                seed: hub,
                budget: 100,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    // Both contradicting claims survive in the output (never suppressed/deleted).
    let knowledge_ids: Vec<NodeId> = pkg.knowledge.iter().map(|f| f.node_id).collect();
    assert!(
        knowledge_ids.contains(&a) && knowledge_ids.contains(&b),
        "both contradicting claims must survive, got {knowledge_ids:?}"
    );

    // The contradiction surfaces as a tension carrying positive stress and gates.
    let tension = pkg
        .tensions
        .iter()
        .find(|t| (t.node_a == a && t.node_b == b) || (t.node_a == b && t.node_b == a))
        .expect("the Contradicts pair must surface as a tension");
    assert!(
        tension.stress > 0.0,
        "stress must be positive: {}",
        tension.stress
    );
    assert!(tension.scope_overlap > 0.0);
    assert!(tension.temporal_overlap > 0.0);
    assert_eq!(tension.evidence_sources.len(), 2);
    assert!(
        tension.evidence_sources.contains(&a) && tension.evidence_sources.contains(&b),
        "evidence sources must name both endpoints"
    );
}

/// A Contradicts edge whose endpoints live in disjoint scopes generates no stress:
/// the scope gate is closed, so the tension is not surfaced (private contradictions
/// do not leak). The nodes still both survive.
#[test]
fn disjoint_scope_contradiction_produces_no_stress() {
    let mut e = engine();

    let hub = created(e.ingest(obs_scoped("hub topic", vec![1.0, 0.0, 0.0], "proj-a")));
    let a = created(e.ingest(obs_scoped("claim a", vec![0.0, 1.0, 0.0], "proj-a")));
    // b lives in an unrelated scope → scope_overlap gate is 0.
    let b = created(e.ingest(obs_scoped("claim b", vec![0.0, 0.0, 1.0], "proj-b")));

    e.link(hub, a, EdgeType::Semantic).unwrap();
    e.link(hub, b, EdgeType::Semantic).unwrap();
    e.link(a, b, EdgeType::Contradicts).unwrap();

    let pkg = e
        .query(
            &Query::Associative {
                seed: hub,
                budget: 100,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    let surfaced = pkg
        .tensions
        .iter()
        .any(|t| (t.node_a == a && t.node_b == b) || (t.node_a == b && t.node_b == a));
    assert!(
        !surfaced,
        "disjoint-scope contradiction must not surface (scope gate closed)"
    );
}

/// More-surprising input receives a higher initial evidence prior `P_i` (ADR-0009):
/// `P_i ← k * eps`, monotone in the prediction error against the nearest site. Both
/// fresh sites share an equal creation-trace base level `B_i`, so the higher
/// composite `A_i = B_i + P_i` reflects the larger surprise prior directly.
#[test]
fn more_surprising_input_gets_higher_initial_retained_action() {
    let mut e = engine();

    // Anchor site at axis 0.
    let _anchor = created(e.ingest(obs_scoped("anchor", vec![1.0, 0.0, 0.0], "proj")));

    // A near-duplicate (small angle) — low surprise.
    let near = created(e.ingest(obs_scoped("near", vec![0.98, 0.2, 0.0], "proj")));
    // An orthogonal observation — high surprise.
    let far = created(e.ingest(obs_scoped("far", vec![0.0, 0.0, 1.0], "proj")));

    let near_a = e.graph().storage().get_retained_action(near).unwrap();
    let far_a = e.graph().storage().get_retained_action(far).unwrap();

    assert!(
        far_a > near_a,
        "more-surprising input should get higher initial A_i: far={far_a} near={near_a}"
    );

    // The surprise difference lives in the decay-exempt evidence prior P_i (ADR-0008).
    let near_p = e.graph().storage().get_evidence_prior(near).unwrap();
    let far_p = e.graph().storage().get_evidence_prior(far).unwrap();
    assert!(
        far_p > near_p,
        "more-surprising input should get higher initial P_i: far={far_p} near={near_p}"
    );
}

/// Familiar input routes-and-reinforces rather than being rejected (ADR-0009).
/// Re-ingesting a near-identical embedding with dedup on reinforces the existing
/// site instead of rejecting on similarity.
#[test]
fn familiar_input_routes_not_rejects() {
    let mut e = Engine::with_config(EngineConfig::new().with_novelty_threshold(0.30));

    let first = created(e.ingest(obs_scoped("topic", vec![1.0, 0.0, 0.0], "proj")));
    let result = e.ingest(obs_scoped("topic again", vec![1.0, 0.0001, 0.0], "proj"));

    match result {
        Ok(IngestResult::Reinforced { existing_id, .. }) => {
            assert_eq!(existing_id, first, "should route to the existing site");
        }
        other => panic!("familiar input must route-reinforce, not reject: {other:?}"),
    }
}
