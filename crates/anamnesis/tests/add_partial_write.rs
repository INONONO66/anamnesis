//! Add-retry partial-write semantics (engine-level contract).
//!
//! `Memory::add` / `Engine::ingest` is a multi-step write (episodic node →
//! semantic finalize → edges) with NO enclosing transaction: if a step after the
//! first node insert fails, that first node is already durable while the session
//! buffer is untouched (the deliberate "Phase E buffer-mutation-last" ordering).
//! A retry then re-presents a byte-identical observation.
//!
//! Retry-idempotency is a CONSUMER / capture-layer responsibility, NOT an engine
//! one. `Engine::ingest` deliberately allocates per observation (its dedup is
//! embedding-similarity only, and `Memory::add` runs with it OFF so genuinely
//! distinct turns always allocate). An engine-level "same content + type +
//! session + timestamp ⇒ reinforce" idempotency guard was tried and REJECTED in
//! review: keyed on a partial identity it can silently COLLAPSE two legitimately
//! distinct same-session turns that share short text at the same millisecond
//! (data loss), and scanning by a broad `speaker-*` entity tag on every ingest is
//! an O(N) cliff on long graphs. The realistic capture path is instead made
//! retry-idempotent upstream, in the MCP daemon, which content-hash-dedupes each
//! raw turn by `turn_key` (`session\0speaker\0text\0at_ms`) and persists the seen
//! set across restarts.
//!
//! These tests therefore pin the engine's HONEST contract: it allocates per
//! observation and — critically — never FALSELY collapses genuinely distinct
//! observations.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};

/// The exact engine config `Memory::add` uses (`memory_engine_config`):
/// dedup off + zero novelty/confidence thresholds so distinct turns allocate.
fn memory_like_config() -> EngineConfig {
    EngineConfig::new()
        .with_dedup_enabled(false)
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0)
}

/// An observation shaped exactly like the one `Memory`'s `ingest_node` builds
/// for a conversational turn: session/speaker entity tags, an embedding, and an
/// origin session id.
fn turn_obs(
    session: &str,
    speaker: &str,
    content: &str,
    node_type: KnowledgeType,
    at: Timestamp,
) -> Observation {
    Observation {
        name: content.chars().take(50).collect(),
        summary: None,
        content: content.to_string(),
        // Deterministic non-empty embedding (a retry re-embeds identically).
        embedding: Some(vec![0.5, 0.25, 0.125, 0.0625]),
        confidence: 0.95,
        node_type,
        entity_tags: vec![format!("session-{session}"), format!("speaker-{speaker}")],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: session.to_string(),
            scope: ScopePath::universal(),
            confidence: 0.95,
        },
        timestamp: at,
        valid_from: None,
        valid_until: None,
    }
}

/// Engine contract: a re-presented byte-identical observation allocates a NEW
/// node at the engine level. Retry-idempotency is provided upstream by the MCP
/// capture layer's `turn_key` content-hash dedup, NOT by an engine content-
/// identity guard (which a review found data-loss-risky — it could collapse two
/// legitimately distinct same-session turns sharing short text at one ms).
#[test]
fn retried_identical_observation_allocates_a_new_node_at_engine_level() {
    let mut engine = Engine::with_config(memory_like_config());

    let first = engine
        .ingest(turn_obs(
            "s",
            "A",
            "A: turn one content",
            KnowledgeType::Episodic,
            Timestamp(2),
        ))
        .expect("first ingest");
    assert!(
        matches!(first, IngestResult::Created(_)),
        "first ingest must allocate, got {first:?}"
    );

    // Retry re-presents the SAME turn verbatim; the engine allocates again.
    engine
        .ingest(turn_obs(
            "s",
            "A",
            "A: turn one content",
            KnowledgeType::Episodic,
            Timestamp(2),
        ))
        .expect("retry ingest");

    assert_eq!(
        engine.graph().node_count(),
        2,
        "engine allocates per observation — retry-idempotency is a capture-layer \
         (turn_key) responsibility, not an engine content-identity guard"
    );
}

/// INVARIANT (the one that matters): genuinely distinct turns (different content)
/// must always allocate distinct nodes — the engine must never falsely collapse.
#[test]
fn distinct_turns_still_allocate() {
    let mut engine = Engine::with_config(memory_like_config());

    engine
        .ingest(turn_obs(
            "s",
            "A",
            "A: first turn",
            KnowledgeType::Episodic,
            Timestamp(2),
        ))
        .expect("turn 1");
    engine
        .ingest(turn_obs(
            "s",
            "A",
            "A: second turn",
            KnowledgeType::Episodic,
            Timestamp(2),
        ))
        .expect("turn 2");

    assert_eq!(
        engine.graph().node_count(),
        2,
        "distinct turns must allocate distinct nodes"
    );
}

/// INVARIANT: identical content with a DIFFERENT knowledge type must not be
/// collapsed. `Memory::add_note` writes an Episodic and a Semantic node with the
/// same content, session and timestamp; the engine keeps both.
#[test]
fn identical_content_different_type_not_collapsed() {
    let mut engine = Engine::with_config(memory_like_config());

    engine
        .ingest(turn_obs(
            "note-1",
            "note",
            "same content both types",
            KnowledgeType::Episodic,
            Timestamp(9),
        ))
        .expect("episodic");
    engine
        .ingest(turn_obs(
            "note-1",
            "note",
            "same content both types",
            KnowledgeType::Semantic,
            Timestamp(9),
        ))
        .expect("semantic");

    assert_eq!(
        engine.graph().node_count(),
        2,
        "same content but different knowledge type must not collapse"
    );
}
