//! Composed multi-tier golden fixtures for the regression judges.
//!
//! Phase 6 quality-gates fixtures (benchmarks.md "Fixture Graphs" /
//! "Search Scenario"). Each tier is a self-contained, deterministic
//! [`Engine`] that exercises the full content variety the spec requires:
//!
//! - identity sites (an agent persona),
//! - semantic and procedural knowledge,
//! - episodic fragments (provenance memories),
//! - entity hubs (shared `entity_tags`),
//! - contradiction pairs (`Contradicts` edges between co-active sites),
//! - scoped private and universal knowledge,
//! - stale and recently accessed sites.
//!
//! Three composed tiers are provided so the regression gates measure the
//! same model at growing graph sizes (benchmarks.md tiers: `small` …
//! larger). The tiers reuse one cluster template and scale the count of
//! filler nodes; the *golden* (named) nodes are identical across tiers so
//! the quality judge can assert the same expected sets at every size.
//!
//! Determinism: no embeddings, no random, fixed ingest order and fixed
//! timestamps, so `NodeId` allocation is stable across runs and machines.

use std::collections::HashMap;

use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::engine::SourceKind;
use anamnesis::engine::{EngineConfig, NodeId};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::storage::SqliteStorage;

/// The agent peer id whose identity sites are addressed by the quality judge.
pub const AGENT_PEER_ID: u64 = 7;

/// One composed tier: an engine plus a symbolic name → NodeId lookup.
pub struct ComposedTier {
    /// Tier label (`small` / `medium` / `large`), for reporting.
    pub label: &'static str,
    /// Number of filler nodes added on top of the golden core.
    pub filler_count: usize,
    /// The built engine.
    pub engine: Engine<SqliteStorage>,
    /// Symbolic key → NodeId for the golden (named) core sites.
    pub ids: HashMap<&'static str, NodeId>,
}

impl ComposedTier {
    /// Look up a golden node id by symbolic key. Panics on drift.
    pub fn id(&self, key: &str) -> NodeId {
        *self
            .ids
            .get(key)
            .unwrap_or_else(|| panic!("[{}] unknown fixture key: {key}", self.label))
    }

    /// Resolve a slice of symbolic keys to a `Vec<NodeId>`.
    pub fn ids_for(&self, keys: &[&str]) -> Vec<NodeId> {
        keys.iter().map(|key| self.id(key)).collect()
    }
}

/// The three composed tiers, in ascending size. Each is built once and is
/// fully deterministic.
pub fn build_composed_tiers() -> Vec<ComposedTier> {
    vec![
        build_tier("small", 0),
        build_tier("medium", 400),
        build_tier("large", 2_000),
    ]
}

/// Reference "now" for the fixtures. Recent sites are accessed at this time;
/// stale sites are left far in the past. Chosen so the 30-day stale window
/// (observability) cleanly separates the two cohorts.
const NOW_MS: u64 = 5_000 * 86_400_000; // day 5000 in ms
/// A timestamp ~90 days before `NOW_MS` — past the stale window.
const STALE_MS: u64 = NOW_MS - 90 * 86_400_000;

fn build_tier(label: &'static str, filler_count: usize) -> ComposedTier {
    let config = EngineConfig::new()
        .with_max_nodes((filler_count + 256) * 2)
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0)
        .with_dedup_enabled(false);

    let mut b = TierBuilder {
        engine: Engine::with_config(config),
        ids: HashMap::new(),
        next_ts: 1_000,
    };

    seed_identity(&mut b);
    seed_knowledge(&mut b);
    seed_memories(&mut b);
    seed_private_universal(&mut b);
    seed_stale_recent(&mut b);
    seed_filler(&mut b, filler_count);
    wire_edges(&mut b);

    ComposedTier {
        label,
        filler_count,
        engine: b.engine,
        ids: b.ids,
    }
}

struct TierBuilder {
    engine: Engine<SqliteStorage>,
    ids: HashMap<&'static str, NodeId>,
    next_ts: u64,
}

impl TierBuilder {
    #[allow(clippy::too_many_arguments)]
    fn add_at(
        &mut self,
        key: &'static str,
        name: &str,
        content: &str,
        node_type: KnowledgeType,
        scope: &str,
        peer_id: u64,
        entity_tags: &[&str],
        timestamp: u64,
    ) -> NodeId {
        let scope_path = if scope.is_empty() {
            ScopePath::universal()
        } else {
            ScopePath::new(scope).expect("valid scope path")
        };
        let observation = Observation {
            name: name.to_string(),
            summary: None,
            content: content.to_string(),
            embedding: None,
            confidence: 0.9,
            node_type,
            entity_tags: entity_tags.iter().map(|t| (*t).to_string()).collect(),
            origin: Origin {
                peer_id: PeerId(peer_id),
                source_kind: SourceKind::AgentObservation,
                session_id: "composed".to_string(),
                scope: scope_path,
                confidence: 0.9,
            },
            timestamp: Timestamp(timestamp),
            valid_from: None,
            valid_until: None,
        };
        let id = match self.engine.ingest(observation).expect("ingest") {
            IngestResult::Created(ids) => *ids.first().expect("created id"),
            IngestResult::Reinforced { existing_id, .. } => existing_id,
        };
        if !key.is_empty() {
            assert!(
                self.ids.insert(key, id).is_none(),
                "duplicate fixture key: {key}"
            );
        }
        id
    }

    fn add(
        &mut self,
        key: &'static str,
        name: &str,
        content: &str,
        node_type: KnowledgeType,
        scope: &str,
        entity_tags: &[&str],
    ) -> NodeId {
        let ts = self.next_ts;
        self.next_ts += 1;
        self.add_at(key, name, content, node_type, scope, 0, entity_tags, ts)
    }

    fn link(&mut self, from: &str, to: &str, edge_type: EdgeType) {
        let from_id = *self.ids.get(from).expect("link from key");
        let to_id = *self.ids.get(to).expect("link to key");
        self.engine
            .link(from_id, to_id, edge_type)
            .expect("link should succeed");
    }

    fn touch(&mut self, key: &str, now: u64) {
        let id = *self.ids.get(key).expect("touch key");
        self.engine.touch(id, Timestamp(now)).expect("touch");
    }
}

// ---------------------------------------------------------------------------
// Identity (agent persona) — addressed via SearchInput.agent_id.
// ---------------------------------------------------------------------------

fn seed_identity(b: &mut TierBuilder) {
    let ts = b.next_ts;
    b.next_ts += 3;
    b.add_at(
        "id.core",
        "I am a rust systems architect",
        "persona identity architect rust systems careful reviewer",
        KnowledgeType::IdentityCore,
        "agent/self",
        AGENT_PEER_ID,
        &["persona"],
        ts,
    );
    b.add_at(
        "id.learned",
        "prefers explicit error handling",
        "persona identity learned prefers explicit error handling no panics",
        KnowledgeType::IdentityLearned,
        "agent/self",
        AGENT_PEER_ID,
        &["persona"],
        ts + 1,
    );
    b.add_at(
        "id.state",
        "currently focused on retrieval quality",
        "persona identity state focused retrieval quality this week",
        KnowledgeType::IdentityState,
        "agent/self",
        AGENT_PEER_ID,
        &["persona"],
        ts + 2,
    );
}

// ---------------------------------------------------------------------------
// Knowledge — semantic / procedural / decision / convention, plus an entity hub.
// ---------------------------------------------------------------------------

fn seed_knowledge(b: &mut TierBuilder) {
    // Cluster K1 — caching. Five members spanning semantic/procedural/decision/
    // convention/gotcha. Broad keyword "caching" appears in all five and nowhere
    // else, so it returns the cluster as a contiguous set (golden-style).
    b.add(
        "k.cache.semantic",
        "caching layer overview",
        "caching layer overview store hot keys reduce latency semantic",
        KnowledgeType::Semantic,
        "dev/backend",
        &["cache"],
    );
    b.add(
        "k.cache.procedure",
        "how to warm the cache",
        "caching procedure warm cache on deploy preload hot keys steps",
        KnowledgeType::Procedural,
        "dev/backend",
        &["cache"],
    );
    b.add(
        "k.cache.decision",
        "chose redis for caching",
        "caching decision chose redis over memcached eviction policy",
        KnowledgeType::Decision,
        "dev/backend",
        &["cache"],
    );
    b.add(
        "k.cache.convention",
        "cache key naming convention",
        "caching convention cache key naming colon namespaces ttl",
        KnowledgeType::Convention,
        "dev/backend",
        &["cache"],
    );
    b.add(
        "k.cache.gotcha",
        "cache stampede gotcha",
        "caching gotcha cache stampede thundering herd lock single flight",
        KnowledgeType::Gotcha,
        "dev/backend",
        &["cache"],
    );

    // Cluster K2 — auth, anchored by an entity hub. Broad keyword "auth" appears
    // in all five and nowhere else.
    b.add(
        "k.auth.hub",
        "auth module entity hub",
        "auth module entity hub authentication central reference",
        KnowledgeType::Entity,
        "dev/backend",
        &["auth"],
    );
    b.add(
        "k.auth.jwt",
        "JWT token validation",
        "auth jwt token validation signature expiry claims",
        KnowledgeType::Semantic,
        "dev/backend",
        &["auth"],
    );
    b.add(
        "k.auth.session",
        "session rotation policy",
        "auth session rotation policy refresh sliding window",
        KnowledgeType::Procedural,
        "dev/backend",
        &["auth"],
    );
    b.add(
        "k.auth.oauth",
        "oauth provider choice",
        "auth oauth provider choice authorization code flow pkce",
        KnowledgeType::Decision,
        "dev/backend",
        &["auth"],
    );
    b.add(
        "k.auth.mfa",
        "mfa enrollment convention",
        "auth mfa enrollment convention totp backup codes policy",
        KnowledgeType::Convention,
        "dev/backend",
        &["auth"],
    );
}

// ---------------------------------------------------------------------------
// Memories — episodic fragments that provide provenance.
// ---------------------------------------------------------------------------

fn seed_memories(b: &mut TierBuilder) {
    b.add(
        "m.deploy",
        "deploy incident postmortem",
        "episodic deploy incident cache stampede postmortem caching",
        KnowledgeType::Episodic,
        "dev/backend",
        &["cache"],
    );
    b.add(
        "m.review",
        "auth review session notes",
        "episodic auth review session notes jwt expiry bug found",
        KnowledgeType::Episodic,
        "dev/backend",
        &["auth"],
    );
    b.add(
        "m.event.outage",
        "redis outage event",
        "event redis outage caching failover degraded mode",
        KnowledgeType::Event,
        "dev/backend",
        &["cache"],
    );
    // Provenance for the logging contradiction: the session in which the async
    // logging decision was made. Surfaces in the tension case via the
    // KnowledgeWithProvenance packaging (ExtractedFrom source memory).
    b.add(
        "m.logging",
        "logging design session",
        "episodic logging design session async decision debate notes",
        KnowledgeType::Episodic,
        "dev/backend",
        &["logging"],
    );
}

// ---------------------------------------------------------------------------
// Scoped private vs universal knowledge.
// ---------------------------------------------------------------------------

fn seed_private_universal(b: &mut TierBuilder) {
    // Universal convention visible to every scope.
    let ts = b.next_ts;
    b.next_ts += 1;
    b.add_at(
        "u.style",
        "universal style guide",
        "universal style guide formatting conventions shared everywhere",
        KnowledgeType::Convention,
        "", // empty scope path == universal
        0,
        &["style"],
        ts,
    );
    // Private knowledge in a disjoint scope — must not leak into other scopes.
    b.add(
        "p.secret",
        "private client secret note",
        "private client secret deploy key rotation internal only",
        KnowledgeType::Semantic,
        "client/acme",
        &["secret"],
    );
}

// ---------------------------------------------------------------------------
// Stale vs recently accessed sites (contradiction pair lives here too).
// ---------------------------------------------------------------------------

fn seed_stale_recent(b: &mut TierBuilder) {
    // A contradiction pair: two co-scoped claims about the same decision that
    // cannot both hold. Both are recently accessed so both stay active and the
    // tension surfaces (frustration.md), neither is suppressed.
    b.add_at(
        "x.claim.old",
        "logging is synchronous",
        "logging decision synchronous blocking simple inline writes",
        KnowledgeType::Decision,
        "dev/backend",
        0,
        &["logging"],
        STALE_MS, // created long ago; will be touched recent below
    );
    b.add(
        "x.claim.new",
        "logging is asynchronous",
        "logging decision asynchronous non-blocking queue batched flush",
        KnowledgeType::Decision,
        "dev/backend",
        &["logging"],
    );

    // A genuinely stale site: created and last accessed long ago, never touched.
    // Distinct keyword so it never collides with a golden query's relevant set.
    b.add_at(
        "s.stale",
        "deprecated soap endpoint",
        "stale deprecated soap endpoint legacy xml rarely used",
        KnowledgeType::Semantic,
        "dev/backend",
        0,
        &["legacy"],
        STALE_MS,
    );

    // Recently accessed sites — touch the contradiction pair and a hot knowledge node.
    b.touch("x.claim.old", NOW_MS);
    b.touch("x.claim.new", NOW_MS);
    b.touch("k.cache.decision", NOW_MS);
}

// ---------------------------------------------------------------------------
// Filler — scale the graph without disturbing the golden core. Filler nodes
// use distinct keywords and tags so they never collide with golden queries.
// ---------------------------------------------------------------------------

fn seed_filler(b: &mut TierBuilder, count: usize) {
    for i in 0..count {
        let ts = b.next_ts;
        b.next_ts += 1;
        b.add_at(
            "",
            &format!("filler topic {i}"),
            &format!("filler unrelated topic zzz{i} noise corpus padding"),
            KnowledgeType::Semantic,
            "dev/filler",
            0,
            &["filler"],
            ts,
        );
    }
}

// ---------------------------------------------------------------------------
// Edges — every required type exercised; intra-cluster + the contradiction.
// ---------------------------------------------------------------------------

fn wire_edges(b: &mut TierBuilder) {
    // Knowledge cohesion.
    b.link("k.cache.semantic", "k.cache.procedure", EdgeType::Causal);
    b.link("k.cache.semantic", "k.cache.decision", EdgeType::Reason);
    b.link("k.cache.decision", "k.cache.convention", EdgeType::Semantic);

    // Entity hub fans to its members.
    b.link("k.auth.hub", "k.auth.jwt", EdgeType::Entity);
    b.link("k.auth.hub", "k.auth.session", EdgeType::Entity);

    // Provenance: episodic memories extracted-from knowledge.
    b.link("m.deploy", "k.cache.decision", EdgeType::ExtractedFrom);
    b.link("m.review", "k.auth.jwt", EdgeType::ExtractedFrom);
    b.link("m.logging", "x.claim.new", EdgeType::ExtractedFrom);

    // The contradiction pair — both claims stay temporally co-valid, so the
    // Contradicts edge surfaces a tension at query time and neither side is
    // suppressed (frustration.md / ADR-0006). (Supersession lineage is exercised
    // separately by the `redis/cache` decision chain, not on this pair, because a
    // Supersedes edge would expire the older claim and close the temporal gate.)
    b.link("x.claim.old", "x.claim.new", EdgeType::Contradicts);
}
