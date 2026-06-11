//! Readout/packaging/temporal integration behavior — consolidated into one
//! binary to keep CI link jobs bounded.
//!
//! Source groups:
//!   - readout_trace.rs          – ranked candidate list with per-term components
//!   - readout_full_phi.rs       – phi must credit text-match, not cosine alone
//!   - packaging_balanced.rs     – default packaging preserves episodic memories
//!   - temporal_field.rs         – date cues bias retrieval by timestamp
//!   - readout_phi_excludes_prior.rs – committed prior must not enter readout phi

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn origin(session: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session.into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    }
}

/// Ingest a node with an explicit `node_type` (used by readout_trace tests).
fn ingest_obs(engine: &mut Engine, name: &str, content: &str, node_type: KnowledgeType) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: None,
            confidence: 0.9,
            node_type,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

/// Ingest a Semantic node with a pre-computed embedding
/// (used by readout_full_phi tests).
fn ingest_with_embedding(engine: &mut Engine, name: &str, content: &str, embedding: Vec<f64>) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: Some(embedding),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

/// Ingest an Episodic node at an explicit Unix timestamp
/// (used by temporal_field tests).
fn ingest_at(engine: &mut Engine, name: &str, content: &str, timestamp: u64) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(timestamp),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

/// Ingest a minimal Semantic node (name + content only)
/// (used by readout_phi_excludes_prior tests).
fn ingest_semantic(engine: &mut Engine, name: &str, content: &str) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(name),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
}

fn engine_with(setup: impl FnOnce(&mut Engine)) -> Engine {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    setup(&mut engine);
    engine
}

// ---------------------------------------------------------------------------
// readout_trace.rs — ranked pre-packaging candidate list with score components
// ---------------------------------------------------------------------------

#[test]
fn readout_trace_lists_ranked_candidates_with_components() {
    let engine = engine_with(|e| {
        ingest_obs(
            e,
            "alpha",
            "alpha factory pattern handler",
            KnowledgeType::Semantic,
        );
        ingest_obs(
            e,
            "beta",
            "beta factory utility helper",
            KnowledgeType::Semantic,
        );
        ingest_obs(e, "gamma", "gamma unrelated text", KnowledgeType::Semantic);
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(
        !readout.is_empty(),
        "trace.readout must list scored candidates"
    );

    // Ranked descending by score.
    for pair in readout.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "readout trace must be ranked: {} < {}",
            pair[0].score,
            pair[1].score
        );
    }

    // Components are finite.
    for candidate in readout {
        for (label, value) in [
            ("score", candidate.score),
            ("activation", candidate.activation),
            ("phi", candidate.phi),
            ("salience", candidate.salience),
            ("impedance", candidate.impedance),
            ("scope_weight", candidate.scope_weight),
            ("trust_weight", candidate.trust_weight),
            ("stress", candidate.stress),
        ] {
            assert!(value.is_finite(), "{label} must be finite, got {value}");
        }
    }
}

#[test]
fn readout_trace_is_a_superset_of_the_packaged_surface() {
    let engine = engine_with(|e| {
        for i in 0..8 {
            ingest_obs(
                e,
                &format!("node-{i}"),
                &format!("factory variant {i} shared topic"),
                KnowledgeType::Semantic,
            );
        }
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 3,
            ..Default::default()
        })
        .expect("search must succeed");

    let packaged: Vec<_> = result
        .package
        .identity
        .iter()
        .chain(result.package.knowledge.iter())
        .chain(result.package.memories.iter())
        .map(|f| f.node_id)
        .collect();

    assert!(
        result.trace.readout.len() >= packaged.len(),
        "pre-package readout ({}) must not be smaller than the package ({})",
        result.trace.readout.len(),
        packaged.len()
    );
    for node_id in packaged {
        assert!(
            result.trace.readout.iter().any(|c| c.node_id == node_id),
            "packaged node {node_id:?} missing from readout trace"
        );
    }
}

// ---------------------------------------------------------------------------
// readout_full_phi.rs — text-match must be credited in phi (not cosine alone)
// ---------------------------------------------------------------------------

#[test]
fn text_match_is_credited_in_readout_phi() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    // Identical embeddings: cosine ties. Only "ownership" text differs.
    let shared = vec![1.0, 0.0, 0.0, 0.0];
    ingest_with_embedding(
        &mut engine,
        "matched",
        "rust ownership semantics",
        shared.clone(),
    );
    ingest_with_embedding(
        &mut engine,
        "unmatched",
        "completely different topic",
        shared.clone(),
    );

    let result = engine
        .search(SearchInput {
            text: "ownership".into(),
            query_embedding: Some(shared),
            limit: 5,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(
        readout.len() >= 2,
        "both nodes must be scored, got {}",
        readout.len()
    );

    // The top-ranked candidate must be the text-matched node, and its phi must
    // strictly exceed the embedding-identical unmatched node's phi.
    let top_name = result
        .package
        .knowledge
        .first()
        .map(|f| f.name.clone())
        .unwrap_or_default();
    assert_eq!(top_name, "matched", "text-matched node must rank first");

    let phis: Vec<f64> = readout.iter().map(|c| c.phi).collect();
    assert!(
        phis[0] > phis[1],
        "text match must be credited in readout phi: {phis:?}"
    );
}

// ---------------------------------------------------------------------------
// packaging_balanced.rs — default packaging keeps episodic memories
// ---------------------------------------------------------------------------

#[test]
fn default_packaging_keeps_episodic_memories() {
    use anamnesis::query::PackagingMode;

    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    engine
        .ingest(Observation {
            name: "episode".into(),
            summary: Some("what happened with the factory".into()),
            content: "the factory pattern broke in the deploy".into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![],
            origin: origin("s1"),
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    // Plain query: no temporal keyword, no tensions, no persona.
    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert_eq!(
        result.trace.packaging_mode,
        Some(PackagingMode::Balanced),
        "plain queries must default to Balanced packaging"
    );
    assert!(
        !result.package.memories.is_empty(),
        "episodic memory that won readout must not be cleared by default packaging"
    );
}

// ---------------------------------------------------------------------------
// temporal_field.rs — date cues bias retrieval toward matching timestamps
// ---------------------------------------------------------------------------

const MAY_8_2023: u64 = 1_683_504_000; // 2023-05-08 00:00 UTC
const DEC_1_2023: u64 = 1_701_388_800; // 2023-12-01 00:00 UTC

#[test]
fn date_cue_prefers_matching_timestamp() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    // Same lexical content; only the timestamp differs.
    ingest_at(
        &mut engine,
        "on-date",
        "beach trip planning notes",
        MAY_8_2023,
    );
    ingest_at(
        &mut engine,
        "off-date",
        "beach trip planning notes",
        DEC_1_2023,
    );

    let result = engine
        .search(SearchInput {
            text: "beach trip on 8 May 2023".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let phis: Vec<f64> = result.trace.readout.iter().map(|c| c.phi).collect();
    assert!(phis.len() >= 2, "both sites must be scored, got {phis:?}");
    assert!(
        phis[0] > phis[1],
        "temporal proximity must separate equal-content sites: {phis:?}"
    );
    // And the winner must actually be the on-date node.
    let top = &result.trace.readout[0];
    let top_node = engine
        .graph()
        .get_node(top.node_id)
        .expect("node must exist");
    assert_eq!(top_node.created_at.0, MAY_8_2023);
}

#[test]
fn last_summer_cue_prefers_summer_node() {
    // 2023-06-15 00:00 UTC (inside summer)
    const JUNE_15_2023: u64 = 1_686_787_200;
    // 2023-12-01 00:00 UTC (winter)
    const DEC_1_2023_TS: u64 = 1_701_388_800;
    // 2023-09-15 00:00 UTC — now (summer ended Aug 31, so "last summer" = Jun-Aug 2023)
    const NOW_SEPT_15: u64 = 1_694_736_000;

    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    ingest_at(
        &mut engine,
        "summer-node",
        "beach trip planning notes",
        JUNE_15_2023,
    );
    ingest_at(
        &mut engine,
        "winter-node",
        "beach trip planning notes",
        DEC_1_2023_TS,
    );

    let result = engine
        .search(SearchInput {
            text: "What did we plan last summer?".into(),
            now: Timestamp(NOW_SEPT_15),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(
        readout.len() >= 2,
        "both nodes must be in readout, got {}",
        readout.len()
    );

    // Find phis by node timestamp.
    let summer_phi = readout
        .iter()
        .filter_map(|c| {
            let node = engine.graph().get_node(c.node_id).ok()?;
            (node.created_at.0 == JUNE_15_2023).then_some(c.phi)
        })
        .next()
        .expect("summer node must be in readout");
    let winter_phi = readout
        .iter()
        .filter_map(|c| {
            let node = engine.graph().get_node(c.node_id).ok()?;
            (node.created_at.0 == DEC_1_2023_TS).then_some(c.phi)
        })
        .next()
        .expect("winter node must be in readout");

    assert!(
        summer_phi > winter_phi,
        "summer node phi ({summer_phi}) must exceed winter node phi ({winter_phi}) with 'last summer' cue and now=2023-09-15"
    );
}

#[test]
fn no_cue_means_no_temporal_separation() {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    ingest_at(&mut engine, "a", "beach trip planning notes", MAY_8_2023);
    ingest_at(&mut engine, "b", "beach trip planning notes", DEC_1_2023);

    let result = engine
        .search(SearchInput {
            text: "beach trip".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");
    let phis: Vec<f64> = result.trace.readout.iter().map(|c| c.phi).collect();
    assert!(
        (phis[0] - phis[1]).abs() < 1e-9,
        "without a time cue the temporal term must be inert: {phis:?}"
    );
}

// ---------------------------------------------------------------------------
// readout_phi_excludes_prior.rs — committed prior must not enter readout phi
// ---------------------------------------------------------------------------

#[test]
fn committed_prior_does_not_enter_readout_phi() {
    use anamnesis::ConfidenceLevel;

    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    ingest_semantic(&mut engine, "first", "factory pattern incident report");
    ingest_semantic(&mut engine, "second", "factory pattern incident report");

    // Boost one node's retained action through the explicit commit path
    // (limit=1 packages only the top node, so only it is strengthened).
    for _ in 0..3 {
        let result = engine
            .search(SearchInput {
                text: "factory".into(),
                limit: 1,
                ..Default::default()
            })
            .expect("search must succeed");
        engine
            .commit(result.package, Some(ConfidenceLevel::High))
            .expect("commit must succeed");
    }

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 2,
            ..Default::default()
        })
        .expect("search must succeed");

    let readout = &result.trace.readout;
    assert!(readout.len() >= 2, "both nodes must be scored");
    let phi_spread = (readout[0].phi - readout[1].phi).abs();
    assert!(
        phi_spread < 1e-9,
        "equal-alignment nodes must have equal readout phi regardless of \
         committed prior; got spread {phi_spread} ({} vs {})",
        readout[0].phi,
        readout[1].phi
    );
}
