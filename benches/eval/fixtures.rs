//! Tier B golden fixture builder.
//!
//! Constructs a deterministic 50-node cognitive graph spanning three scopes
//! (`dev/rust`, `travel/japan`, `research/llm`) for retrieval evaluation. The
//! fixture is purely text-driven: no embeddings, no random, no wall-clock
//! timestamps. Ingestion order is fixed, so `NodeId` allocation is stable
//! across runs and across machines.
//!
//! The fixture is structured as ten thematic clusters of five nodes each.
//! Every cluster has two unique broad keywords that appear in all five
//! cluster members (and nowhere else). This makes single-keyword text
//! queries return the cluster as a contiguous result set, which lets the
//! golden test lock precision/recall floors against the true cluster
//! membership.
//!
//! Edges cover the five required types — `Semantic`, `Causal`, `Reason`,
//! `Supersedes`, and `Contradicts` — and stay intra-cluster so that
//! spreading activation does not pull unrelated clusters into a query's
//! result set.

use std::collections::HashMap;

use anamnesis::api::{IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Engine, EngineConfig, NodeId};

/// Captured fixture state — engine plus a symbolic name → NodeId lookup.
///
/// The name keys mirror the cluster layout in [`build_golden_fixture`]
/// and are used by the golden test to spell out expected relevant sets
/// without hard-coding raw `NodeId` integers.
pub struct GoldenFixture {
    pub engine: Engine<SqliteStorage>,
    pub ids: HashMap<&'static str, NodeId>,
}

impl GoldenFixture {
    /// Look up a node id by symbolic key. Panics if the key is unknown so
    /// fixture/test drift surfaces immediately.
    pub fn id(&self, key: &str) -> NodeId {
        *self
            .ids
            .get(key)
            .unwrap_or_else(|| panic!("unknown fixture key: {key}"))
    }

    /// Resolve a slice of symbolic keys to a `Vec<NodeId>` for use as an
    /// expected set.
    pub fn ids_for(&self, keys: &[&str]) -> Vec<NodeId> {
        keys.iter().map(|key| self.id(key)).collect()
    }
}

/// Build the deterministic golden engine.
///
/// Returns the engine alone, matching the locked Tier B contract. Tests
/// that need to address nodes by symbolic name should call
/// [`build_golden_fixture`] instead.
#[allow(dead_code)]
pub fn build_golden_engine() -> Engine<SqliteStorage> {
    build_golden_fixture().engine
}

/// Build the deterministic golden engine and capture symbolic NodeIds.
pub fn build_golden_fixture() -> GoldenFixture {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0)
        .with_dedup_enabled(false);

    let mut builder = FixtureBuilder {
        engine: Engine::with_config(config),
        ids: HashMap::new(),
        next_ts: 1_000,
    };

    seed_dev_rust(&mut builder);
    seed_travel_japan(&mut builder);
    seed_research_llm(&mut builder);
    wire_edges(&mut builder);

    GoldenFixture {
        engine: builder.engine,
        ids: builder.ids,
    }
}

struct FixtureBuilder {
    engine: Engine<SqliteStorage>,
    ids: HashMap<&'static str, NodeId>,
    next_ts: u64,
}

impl FixtureBuilder {
    fn add(
        &mut self,
        key: &'static str,
        name: &str,
        content: &str,
        node_type: KnowledgeType,
        scope: &str,
        entity_tags: &[&str],
    ) -> NodeId {
        let observation = Observation {
            name: name.to_string(),
            summary: None,
            content: content.to_string(),
            embedding: None,
            confidence: 0.9,
            node_type,
            entity_tags: entity_tags.iter().map(|tag| (*tag).to_string()).collect(),
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "golden".to_string(),
                scope: ScopePath::new(scope).expect("valid scope path"),
                confidence: 0.9,
            },
            timestamp: Timestamp(self.next_ts),
            valid_from: None,
            valid_until: None,
        };
        self.next_ts += 1;

        let id = match self.engine.ingest(observation).expect("ingest") {
            IngestResult::Created(node_ids) => *node_ids.first().expect("created node id"),
            IngestResult::Reinforced { existing_id, .. } => existing_id,
            IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
        };
        let prior = self.ids.insert(key, id);
        assert!(prior.is_none(), "duplicate fixture key: {key}");
        id
    }

    fn link(&mut self, from: &str, to: &str, edge_type: EdgeType, weight: f64) {
        let from_id = *self.ids.get(from).expect("link from key");
        let to_id = *self.ids.get(to).expect("link to key");
        self.engine
            .link(from_id, to_id, edge_type, weight)
            .expect("link should succeed");
    }
}

// ---------------------------------------------------------------------------
// dev/rust scope (15 nodes, 3 clusters)
// ---------------------------------------------------------------------------

fn seed_dev_rust(b: &mut FixtureBuilder) {
    // Cluster A1 — error handling. Broad keywords: "Result" + "errors".
    b.add(
        "rust.result.type",
        "Result type for errors",
        "rust Result type errors handling pattern",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "errors"],
    );
    b.add(
        "rust.result.unwrap",
        "avoid unwrap in libraries",
        "rust Result errors unwrap library discouraged",
        KnowledgeType::Convention,
        "dev/rust",
        &["rust", "errors"],
    );
    b.add(
        "rust.result.panic",
        "panic for unrecoverable errors",
        "rust Result errors panic unrecoverable invariants",
        KnowledgeType::Decision,
        "dev/rust",
        &["rust", "errors"],
    );
    b.add(
        "rust.result.question",
        "question mark operator threading errors",
        "rust Result errors question mark operator early return",
        KnowledgeType::Procedural,
        "dev/rust",
        &["rust", "errors"],
    );
    b.add(
        "rust.result.anyhow",
        "anyhow context for application errors",
        "rust Result errors anyhow context application boundary",
        KnowledgeType::Convention,
        "dev/rust",
        &["rust", "errors"],
    );

    // Cluster A2 — async runtime. Broad keywords: "tokio" + "executor".
    b.add(
        "rust.tokio.runtime",
        "tokio runtime executor",
        "rust tokio executor async runtime preferred default",
        KnowledgeType::Decision,
        "dev/rust",
        &["rust", "tokio"],
    );
    b.add(
        "rust.tokio.async_std",
        "async-std rejected for tokio",
        "rust tokio executor async-std rejected unmaintained",
        KnowledgeType::Decision,
        "dev/rust",
        &["rust", "tokio"],
    );
    b.add(
        "rust.tokio.axum",
        "axum web framework on tokio",
        "rust tokio executor axum web framework hyper",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "tokio"],
    );
    b.add(
        "rust.tokio.futures",
        "futures combinators with tokio",
        "rust tokio executor futures combinators stream",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "tokio"],
    );
    b.add(
        "rust.tokio.scheduler",
        "tokio scheduler internals",
        "rust tokio executor scheduler work stealing internals",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "tokio"],
    );

    // Cluster A3 — type system. Broad keywords: "ownership" + "borrow".
    b.add(
        "rust.types.ownership",
        "ownership move semantics",
        "rust ownership borrow move semantics single owner",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "ownership"],
    );
    b.add(
        "rust.types.borrow",
        "borrow checker enforcement",
        "rust ownership borrow checker enforcement compile time",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "ownership"],
    );
    b.add(
        "rust.types.lifetime",
        "lifetime annotations explained",
        "rust ownership borrow lifetime annotation reference",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "ownership"],
    );
    b.add(
        "rust.types.shared",
        "shared references and aliasing",
        "rust ownership borrow shared reference aliasing rule",
        KnowledgeType::Semantic,
        "dev/rust",
        &["rust", "ownership"],
    );
    b.add(
        "rust.types.interior",
        "interior mutability cells",
        "rust ownership borrow interior mutability cell pattern",
        KnowledgeType::Gotcha,
        "dev/rust",
        &["rust", "ownership"],
    );
}

// ---------------------------------------------------------------------------
// travel/japan scope (15 nodes, 3 clusters)
// ---------------------------------------------------------------------------

fn seed_travel_japan(b: &mut FixtureBuilder) {
    // Cluster B1 — cities. Broad keywords: "city" + "prefecture".
    b.add(
        "japan.city.tokyo",
        "tokyo metropolis",
        "tokyo city prefecture metropolis japan capital",
        KnowledgeType::Entity,
        "travel/japan",
        &["japan", "city"],
    );
    b.add(
        "japan.city.kyoto",
        "kyoto former capital",
        "kyoto city prefecture former capital japan culture",
        KnowledgeType::Entity,
        "travel/japan",
        &["japan", "city"],
    );
    b.add(
        "japan.city.osaka",
        "osaka commercial hub",
        "osaka city prefecture commercial hub japan kansai",
        KnowledgeType::Entity,
        "travel/japan",
        &["japan", "city"],
    );
    b.add(
        "japan.city.sapporo",
        "sapporo northern hub",
        "sapporo city prefecture northern hub japan hokkaido",
        KnowledgeType::Entity,
        "travel/japan",
        &["japan", "city"],
    );
    b.add(
        "japan.city.fukuoka",
        "fukuoka kyushu gateway",
        "fukuoka city prefecture kyushu gateway japan",
        KnowledgeType::Entity,
        "travel/japan",
        &["japan", "city"],
    );

    // Cluster B2 — transport. Broad keywords: "shinkansen" + "rail".
    b.add(
        "japan.transport.shinkansen",
        "shinkansen high speed",
        "shinkansen rail high speed japan trunk lines",
        KnowledgeType::Semantic,
        "travel/japan",
        &["japan", "transport"],
    );
    b.add(
        "japan.transport.jrpass.tourist",
        "JR pass for tourists",
        "shinkansen rail JR pass tourist japan unlimited",
        KnowledgeType::Convention,
        "travel/japan",
        &["japan", "transport"],
    );
    b.add(
        "japan.transport.jrpass.abroad",
        "buy JR pass abroad",
        "shinkansen rail JR pass abroad japan cheaper before arrival",
        KnowledgeType::Convention,
        "travel/japan",
        &["japan", "transport"],
    );
    b.add(
        "japan.transport.station",
        "central station connection hub",
        "shinkansen rail central station hub japan transfer",
        KnowledgeType::Semantic,
        "travel/japan",
        &["japan", "transport"],
    );
    b.add(
        "japan.transport.express",
        "limited express trains",
        "shinkansen rail limited express japan regional service",
        KnowledgeType::Semantic,
        "travel/japan",
        &["japan", "transport"],
    );

    // Cluster B3 — cuisine. Broad keywords: "cuisine" + "dish".
    b.add(
        "japan.cuisine.ramen",
        "ramen origin disputed",
        "ramen cuisine dish japan disputed origin noodle",
        KnowledgeType::Semantic,
        "travel/japan",
        &["japan", "food"],
    );
    b.add(
        "japan.cuisine.sushi",
        "sushi etiquette tips",
        "sushi cuisine dish japan etiquette omakase counter",
        KnowledgeType::Convention,
        "travel/japan",
        &["japan", "food"],
    );
    b.add(
        "japan.cuisine.tempura",
        "tempura preparation",
        "tempura cuisine dish japan preparation light batter",
        KnowledgeType::Procedural,
        "travel/japan",
        &["japan", "food"],
    );
    b.add(
        "japan.cuisine.okonomiyaki",
        "okonomiyaki regional style",
        "okonomiyaki cuisine dish japan regional style hiroshima osaka",
        KnowledgeType::Semantic,
        "travel/japan",
        &["japan", "food"],
    );
    b.add(
        "japan.cuisine.izakaya",
        "izakaya pub experience",
        "izakaya cuisine dish japan pub experience small plates",
        KnowledgeType::Semantic,
        "travel/japan",
        &["japan", "food"],
    );
}

// ---------------------------------------------------------------------------
// research/llm scope (20 nodes, 4 clusters)
// ---------------------------------------------------------------------------

fn seed_research_llm(b: &mut FixtureBuilder) {
    // Cluster C1 — transformer architecture. Broad keywords: "transformer" + "attention".
    b.add(
        "llm.transformer.architecture",
        "transformer architecture overview",
        "transformer attention architecture overview deep stack",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "transformer"],
    );
    b.add(
        "llm.transformer.self",
        "self-attention mechanism",
        "transformer attention self mechanism qkv projection",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "transformer"],
    );
    b.add(
        "llm.transformer.multihead",
        "multi-head attention",
        "transformer attention multi head parallel projection",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "transformer"],
    );
    b.add(
        "llm.transformer.vaswani",
        "attention is all you need 2017",
        "transformer attention vaswani 2017 paper foundation",
        KnowledgeType::Entity,
        "research/llm",
        &["llm", "transformer"],
    );
    b.add(
        "llm.transformer.scaled",
        "scaled dot-product attention",
        "transformer attention scaled dot product softmax",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "transformer"],
    );

    // Cluster C2 — positional encoding. Broad keywords: "positional" + "encoding".
    b.add(
        "llm.positional.sinusoidal",
        "sinusoidal positional encoding",
        "positional encoding sinusoidal classic transformer",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "positional"],
    );
    b.add(
        "llm.positional.rope",
        "rotary positional embedding rope",
        "positional encoding rotary rope embedding modern",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "positional"],
    );
    b.add(
        "llm.positional.alibi",
        "alibi positional bias",
        "positional encoding alibi bias linear extrapolation",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "positional"],
    );
    b.add(
        "llm.positional.learned",
        "learned positional embedding",
        "positional encoding learned bert style absolute",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "positional"],
    );
    b.add(
        "llm.positional.nope",
        "no positional encoding nope",
        "positional encoding no nope decoder only experiments",
        KnowledgeType::Semantic,
        "research/llm",
        &["llm", "positional"],
    );

    // Cluster C3 — alignment methods. Broad keywords: "alignment" + "human".
    b.add(
        "llm.align.rlhf",
        "RLHF reinforcement from human feedback",
        "RLHF alignment human feedback reinforcement learning",
        KnowledgeType::Procedural,
        "research/llm",
        &["llm", "alignment"],
    );
    b.add(
        "llm.align.dpo",
        "DPO direct preference optimization",
        "DPO alignment human preference optimization simpler",
        KnowledgeType::Decision,
        "research/llm",
        &["llm", "alignment"],
    );
    b.add(
        "llm.align.constitutional",
        "constitutional AI principles",
        "constitutional alignment human AI principles anthropic",
        KnowledgeType::Procedural,
        "research/llm",
        &["llm", "alignment"],
    );
    b.add(
        "llm.align.sft",
        "supervised fine-tuning",
        "SFT alignment human supervised fine tuning labeled",
        KnowledgeType::Procedural,
        "research/llm",
        &["llm", "alignment"],
    );
    b.add(
        "llm.align.rlaif",
        "RLAIF feedback from AI evaluator",
        "RLAIF alignment human feedback synthetic AI evaluator",
        KnowledgeType::Procedural,
        "research/llm",
        &["llm", "alignment"],
    );

    // Cluster C4 — open weight families. Broad keywords: "open" + "weights".
    b.add(
        "llm.open.llama2",
        "llama 2 open weights",
        "llama 2 open weights model meta release",
        KnowledgeType::Entity,
        "research/llm",
        &["llm", "open-weights"],
    );
    b.add(
        "llm.open.llama3",
        "llama 3 open weights",
        "llama 3 open weights model meta successor",
        KnowledgeType::Entity,
        "research/llm",
        &["llm", "open-weights"],
    );
    b.add(
        "llm.open.mistral",
        "mistral 7b open weights",
        "mistral 7b open weights model dense",
        KnowledgeType::Entity,
        "research/llm",
        &["llm", "open-weights"],
    );
    b.add(
        "llm.open.mixtral",
        "mixtral mixture of experts open",
        "mixtral mixture experts open weights model sparse",
        KnowledgeType::Entity,
        "research/llm",
        &["llm", "open-weights"],
    );
    b.add(
        "llm.open.qwen",
        "qwen 2 open weights",
        "qwen 2 open weights model alibaba multilingual",
        KnowledgeType::Entity,
        "research/llm",
        &["llm", "open-weights"],
    );
}

// ---------------------------------------------------------------------------
// Edges — every required type is exercised at least once
// ---------------------------------------------------------------------------

fn wire_edges(b: &mut FixtureBuilder) {
    // dev/rust edges
    b.link(
        "rust.result.type",
        "rust.result.unwrap",
        EdgeType::Reason,
        0.9,
    );
    b.link(
        "rust.result.type",
        "rust.result.question",
        EdgeType::Causal,
        0.85,
    );
    b.link(
        "rust.tokio.runtime",
        "rust.tokio.async_std",
        EdgeType::Contradicts,
        0.9,
    );
    b.link(
        "rust.tokio.runtime",
        "rust.tokio.async_std",
        EdgeType::Supersedes,
        0.85,
    );
    b.link(
        "rust.types.ownership",
        "rust.types.borrow",
        EdgeType::Semantic,
        0.95,
    );
    b.link(
        "rust.types.ownership",
        "rust.types.lifetime",
        EdgeType::Semantic,
        0.85,
    );

    // travel/japan edges
    b.link(
        "japan.city.tokyo",
        "japan.city.kyoto",
        EdgeType::Semantic,
        0.7,
    );
    b.link(
        "japan.transport.shinkansen",
        "japan.transport.jrpass.tourist",
        EdgeType::Causal,
        0.8,
    );
    b.link(
        "japan.transport.jrpass.tourist",
        "japan.transport.jrpass.abroad",
        EdgeType::Reason,
        0.85,
    );
    b.link(
        "japan.cuisine.ramen",
        "japan.cuisine.sushi",
        EdgeType::Semantic,
        0.6,
    );

    // research/llm edges
    b.link(
        "llm.transformer.architecture",
        "llm.transformer.self",
        EdgeType::Causal,
        0.85,
    );
    b.link(
        "llm.transformer.self",
        "llm.transformer.multihead",
        EdgeType::Semantic,
        0.9,
    );
    b.link(
        "llm.positional.sinusoidal",
        "llm.positional.rope",
        EdgeType::Supersedes,
        0.85,
    );
    b.link("llm.align.rlhf", "llm.align.dpo", EdgeType::Reason, 0.8);
    b.link(
        "llm.open.llama2",
        "llm.open.llama3",
        EdgeType::Supersedes,
        0.95,
    );
}
