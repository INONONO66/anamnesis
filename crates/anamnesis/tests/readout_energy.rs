//! Behavioral proof that the query-local readout energy `E(S | Q)` is computed
//! over the active subsystem and behaves per energy.md / ADR-0007.
//!
//! The energy is an *interpretive* objective surfaced on the `SearchTrace`: it
//! explains why a bundle was selected, it is query-local and never stored, and the
//! RWR stationary vector (reported as `iterations`/`residual`) remains the true
//! fixed point. These tests assert the wiring and the structural-sign semantics,
//! not specific magnitudes.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::SourceKind;
use anamnesis::engine::{EdgeType, EngineConfig, IngestResult, NodeId};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::SearchInput;

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_confidence_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn obs(name: &str, content: &str, node_type: KnowledgeType, tags: &[&str]) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: content.to_string(),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: tags.iter().map(|t| (*t).to_string()).collect(),
        origin: Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1_000),
        valid_from: None,
        valid_until: None,
    }
}

fn ingest(e: &mut Engine, name: &str, content: &str, kt: KnowledgeType, tags: &[&str]) -> NodeId {
    match e.ingest(obs(name, content, kt, tags)).expect("ingest") {
        IngestResult::Created(ids) => *ids.first().expect("created id"),
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    }
}

#[test]
fn energy_is_computed_over_a_nonempty_result() {
    let mut e = engine();
    let a = ingest(
        &mut e,
        "cache overview",
        "caching layer overview hot keys latency",
        KnowledgeType::Semantic,
        &["cache"],
    );
    let b = ingest(
        &mut e,
        "warm cache",
        "caching procedure warm cache preload steps",
        KnowledgeType::Procedural,
        &["cache"],
    );
    e.link(a, b, EdgeType::Causal).expect("link");

    let result = e
        .search(SearchInput {
            text: "caching".into(),
            limit: 10,
            seed_limit: Some(5),
            ..Default::default()
        })
        .expect("search");

    assert!(
        result.package.total_fragments() > 0,
        "expected a non-empty result"
    );

    // The energy decomposition is populated and finite. Lit, query-aligned,
    // mutually conductive sites produce non-negative alignment and support
    // magnitudes; the composed total is finite.
    let energy = result.trace.energy;
    assert!(energy.field_alignment >= 0.0);
    assert!(energy.conductive_support >= 0.0);
    assert!(energy.impedance_regularization >= 0.0);
    assert!(energy.frustration_penalty >= 0.0);
    assert!(energy.total().is_finite());
    // No contradiction was surfaced, so the frustration penalty is exactly zero.
    assert_eq!(energy.frustration_penalty, 0.0);
}

#[test]
fn surfaced_contradiction_raises_frustration_penalty() {
    let mut e = engine();
    let old = ingest(
        &mut e,
        "logging sync",
        "logging decision synchronous blocking inline",
        KnowledgeType::Decision,
        &["logging"],
    );
    let new = ingest(
        &mut e,
        "logging async",
        "logging decision asynchronous non-blocking queue",
        KnowledgeType::Decision,
        &["logging"],
    );
    // A Contradicts edge between two co-valid, co-active claims must surface a
    // tension (ADR-0006) and lift the frustration penalty term of the energy.
    e.link(old, new, EdgeType::Contradicts).expect("link");

    let result = e
        .search(SearchInput {
            text: "logging".into(),
            limit: 10,
            seed_limit: Some(5),
            ..Default::default()
        })
        .expect("search");

    assert!(
        result.package.tensions.iter().any(|t| t.stress > 0.0),
        "a contradiction between active sites must surface a tension"
    );
    assert!(
        result.trace.energy.frustration_penalty > 0.0,
        "the surfaced tension must raise the frustration penalty (the +1 structural sign)"
    );
}

#[test]
fn energy_is_query_local_and_not_stored() {
    let mut e = engine();
    let a = ingest(
        &mut e,
        "cache overview",
        "caching layer overview hot keys",
        KnowledgeType::Semantic,
        &["cache"],
    );
    let b = ingest(
        &mut e,
        "warm cache",
        "caching procedure warm cache preload",
        KnowledgeType::Procedural,
        &["cache"],
    );
    e.link(a, b, EdgeType::Causal).expect("link");

    let input = SearchInput {
        text: "caching".into(),
        limit: 10,
        seed_limit: Some(5),
        ..Default::default()
    };

    // Reservoirs before any read-only search.
    let action_before = e.retained_action(a).expect("retained_action");
    let salience_before = e.graph().get_node(a).expect("node").salience;

    let first = e.search(input.clone()).expect("search");
    let second = e.search(input).expect("search");

    // Query-local: the same graph + query yields an identical energy decomposition
    // (determinism MUST). Energy is recomputed, never read from storage.
    assert_eq!(first.trace.energy, second.trace.energy);

    // Not stored: a read-only search never mutates the authoritative reservoir or
    // its projection — energy lives only on the transient trace.
    assert_eq!(
        e.retained_action(a).expect("retained_action"),
        action_before
    );
    assert_eq!(
        e.graph().get_node(a).expect("node").salience,
        salience_before
    );
}
