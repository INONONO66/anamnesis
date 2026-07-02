//! Social trust-as-evidence (social.md "Peer Trust").
//!
//! Peer trust is a calibrated evidence signal, not an authorization decision. It
//! is an authoritative log-odds reservoir on the peer profile that corroboration
//! (multi-agent agreement on an entity) and feedback move through the single
//! traceable update path. These tests prove:
//!
//! - evidence raises/lowers the trust reservoir a bounded, slow fraction per event;
//! - the move is traceable (a `PeerTrustChanged` event + the persisted reservoir);
//! - origin/provenance is never erased (coarse `trust_level`, name, count intact);
//! - commit feedback nudges the originating peer's trust;
//! - the moved trust reservoir feeds the readout `trust_weight` term.

use anamnesis::Engine;
use anamnesis::api::{GraphEvent, Observation};
use anamnesis::engine::{ConfidenceLevel, EngineConfig, IngestResult};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::mechanics::priors::{
    CORROBORATION_LOG_ODDS, TRUST_LEARNING_RATE, project_trust, trust_evidence_target,
    update_trust_reservoir,
};
use anamnesis::peer::{SourceKind, TrustLevel};
use anamnesis::query::{Query, QueryConfig};

fn observation(name: &str, peer_id: PeerId, session_id: &str, tags: &[&str]) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.iter().map(|t| (*t).to_string()).collect(),
        origin: Origin {
            peer_id,
            source_kind: SourceKind::AgentObservation,
            session_id: session_id.to_string(),
            scope: ScopePath::new("project-1").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn insert_node(
    engine: &mut Engine,
    name: &str,
    peer_id: PeerId,
    session_id: &str,
    tags: &[&str],
) -> NodeId {
    let IngestResult::Created(ids) = engine
        .ingest(observation(name, peer_id, session_id, tags))
        .unwrap()
    else {
        panic!("expected node creation");
    };
    ids[0]
}

// ── Direct evidence path: corroboration raises trust, traceably ──────────────

#[test]
fn positive_evidence_raises_trust_reservoir_traceably() {
    let mut engine = Engine::new();
    // Neutral Agent prior = 0.0, so any movement is purely evidence-driven.
    let alice = engine.register_peer("alice", TrustLevel::Agent).unwrap();

    let before = engine.get_peer(alice).unwrap().trust_reservoir;
    assert_eq!(
        before, 0.0,
        "Agent prior is the neutral no-evidence reservoir"
    );

    let _ = engine.drain_events();
    let new = engine.update_peer_trust_evidence(alice, 1.0).unwrap();

    // Corroboration RAISES the trust reservoir.
    assert!(
        new > before,
        "positive evidence must raise trust: {new} !> {before}"
    );
    let after = engine.get_peer(alice).unwrap().trust_reservoir;
    assert_eq!(after, new, "the returned reservoir is the persisted one");

    // The change is TRACEABLE: a PeerTrustChanged event records old -> new.
    let events = engine.drain_events();
    let trace = events
        .iter()
        .find_map(|e| match e {
            GraphEvent::PeerTrustChanged { peer_id, old, new } if *peer_id == alice => {
                Some((*old, *new))
            }
            _ => None,
        })
        .expect("a PeerTrustChanged trace event must be emitted");
    assert_eq!(trace, (before, after), "trace records the exact move");

    // The move is the declared minimal RW step toward the corroboration target.
    let expected = update_trust_reservoir(before, trust_evidence_target(1.0), TRUST_LEARNING_RATE);
    assert!((after - expected).abs() < 1e-12);
}

#[test]
fn negative_evidence_lowers_trust_reservoir() {
    let mut engine = Engine::new();
    let bob = engine.register_peer("bob", TrustLevel::Agent).unwrap();

    let before = engine.get_peer(bob).unwrap().trust_reservoir;
    let new = engine.update_peer_trust_evidence(bob, -1.0).unwrap();
    assert!(new < before, "negative feedback must lower trust");
}

#[test]
fn trust_evidence_never_erases_origin() {
    let mut engine = Engine::new();
    let alice = engine.register_peer("alice", TrustLevel::Member).unwrap();
    engine.add_peer_alias(alice, "ali").unwrap();

    // Move trust by evidence repeatedly.
    for _ in 0..5 {
        engine.update_peer_trust_evidence(alice, 1.0).unwrap();
    }

    let profile = engine.get_peer(alice).unwrap();
    // Coarse level, name, and aliases are untouched — only the estimate moved.
    assert_eq!(profile.trust_level, TrustLevel::Member);
    assert_eq!(profile.name, "alice");
    assert!(profile.aliases.contains(&"ali".to_string()));
    assert_eq!(
        profile.trust_evidence_count, 5,
        "evidence count is part of the trace"
    );
    assert_eq!(engine.resolve_peer("ali"), Some(alice));
}

#[test]
fn single_event_is_a_slow_durable_nudge_not_a_swing() {
    let mut engine = Engine::new();
    let p = engine.register_peer("p", TrustLevel::Agent).unwrap();

    let after_one = engine.update_peer_trust_evidence(p, 1.0).unwrap();
    // One event closes only a fraction TRUST_LEARNING_RATE of the gap to target.
    assert!(
        after_one < 0.5 * CORROBORATION_LOG_ODDS,
        "one event must not swing trust"
    );

    // Accumulated consistent evidence converges toward the target (durable).
    for _ in 0..500 {
        engine.update_peer_trust_evidence(p, 1.0).unwrap();
    }
    let saturated = engine.get_peer(p).unwrap().trust_reservoir;
    assert!((saturated - CORROBORATION_LOG_ODDS).abs() < 1e-3);
    assert!(
        saturated < CORROBORATION_LOG_ODDS + 1e-9,
        "bounded — never overshoots"
    );
}

// ── commit feedback nudges the originating peer's trust ──────────────────────

#[test]
fn commit_positive_feedback_raises_originating_peer_trust() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    let alice = engine.register_peer("alice", TrustLevel::Agent).unwrap();

    let seed = insert_node(&mut engine, "auth uses factory", alice, "s1", &["auth"]);

    let trust_before = engine.get_peer(alice).unwrap().trust_reservoir;

    let mut qconfig = QueryConfig::default();
    qconfig.scope = ScopePath::new("project-1").expect("scope");
    qconfig.min_activation = 0.0;
    let pkg = engine
        .query(&Query::Associative { seed, budget: 100 }, &qconfig)
        .unwrap();

    engine.commit(pkg, Some(ConfidenceLevel::High)).unwrap();

    let trust_after = engine.get_peer(alice).unwrap().trust_reservoir;
    assert!(
        trust_after > trust_before,
        "useful commit feedback should raise the source peer's trust: {trust_after} !> {trust_before}"
    );
}

// ── trust reservoir is wired into the readout trust_weight term ──────────────

#[test]
fn trust_reservoir_feeds_the_readout_trust_weight() {
    // The readout trust term is `scope_weight_bonus + project_trust(reservoir)`.
    // A peer with positive evidence projects a positive bonus; the no-evidence
    // prior projects zero, so evidence movement directly enters ranking.
    assert_eq!(project_trust(0.0), 0.0);
    let raised = update_trust_reservoir(0.0, trust_evidence_target(1.0), TRUST_LEARNING_RATE);
    assert!(
        project_trust(raised) > 0.0,
        "raised trust contributes a positive readout term"
    );

    let lowered = update_trust_reservoir(0.0, trust_evidence_target(-1.0), TRUST_LEARNING_RATE);
    assert!(
        project_trust(lowered) < 0.0,
        "lowered trust contributes a negative readout term"
    );
}
