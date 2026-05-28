//! Demonstrates a custom `EmbeddingProvider` with the Anamnesis engine.
//!
//! This example creates a dummy 384-dimensional provider, ingests nodes
//! with generated embeddings, links them, and queries the graph.
//!
//! Run: `cargo run --example custom_embedding`

use anamnesis::api::Observation;
use anamnesis::embedding::{EmbeddingProvider, widen};
use anamnesis::graph::node::Origin;
use anamnesis::{
    EdgeType, Engine, EngineConfig, Error, IngestResult, KnowledgeType, Query, QueryConfig,
    Timestamp,
};

/// A dummy embedding provider that maps text length to a 384-dimensional vector.
///
/// Each dimension is set to `(text_len * (i + 1)) mod 100 / 100.0` so that
/// different texts produce different but deterministic embeddings.
struct DummyProvider;

impl EmbeddingProvider for DummyProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts
            .iter()
            .map(|t| {
                let len = t.len() as f32;
                (0..384)
                    .map(|i| ((len * (i as f32 + 1.0)) % 100.0) / 100.0)
                    .collect()
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        384
    }

    fn model_name(&self) -> &str {
        "dummy-384"
    }
}

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".into(),
        scope: anamnesis::graph::ScopePath::new("demo").expect("valid scope"),
        confidence: 0.9,
    }
}

fn main() -> Result<(), Error> {
    let provider = DummyProvider;
    println!(
        "Using provider: {} ({}-d)",
        provider.model_name(),
        provider.dimensions()
    );

    let embeddings = provider.embed_f64(&[
        "auth module uses factory pattern",
        "race condition in auth middleware",
    ])?;
    println!(
        "Generated {} embeddings, each {}-d",
        embeddings.len(),
        embeddings[0].len()
    );

    let f32_vec = provider.embed_single("factory pattern for handlers")?;
    let f64_vec = widen(&f32_vec);
    println!(
        "Manual widen: f32[{}] → f64[{}]",
        f32_vec.len(),
        f64_vec.len()
    );

    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let result1 = engine.ingest(Observation {
        name: "auth uses factory pattern".into(),
        summary: Some("Confirmed across sessions".into()),
        content: "The auth module uses factory pattern for handler creation".into(),
        embedding: Some(embeddings[0].clone()),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["auth".into(), "factory-pattern".into()],
        origin: origin(),
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    })?;

    let id1 = match &result1 {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => *existing_id,
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    };
    println!("Ingested node {}: {:?}", id1.0, result1);

    let result2 = engine.ingest(Observation {
        name: "race condition in auth middleware".into(),
        summary: None,
        content: "Found a race condition in the auth middleware during session #5".into(),
        embedding: Some(embeddings[1].clone()),
        confidence: 0.85,
        node_type: KnowledgeType::Episodic,
        entity_tags: vec!["auth".into()],
        origin: origin(),
        timestamp: Timestamp(2000),
        valid_from: None,
        valid_until: None,
    })?;

    let id2 = match &result2 {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => *existing_id,
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    };
    println!("Ingested node {}: {:?}", id2.0, result2);

    let edge_id = engine.link(id1, id2, EdgeType::Semantic, 0.78)?;
    println!("Linked {} → {} (edge {})", id1.0, id2.0, edge_id.0);

    engine.touch(id1, Timestamp(3000))?;
    println!("Touched node {}", id1.0);

    let query = Query::Associative {
        seed: id1,
        budget: 10,
    };
    let mut qconfig = QueryConfig::default();
    qconfig.query_embedding = Some(embeddings[0].clone());
    qconfig.scope = anamnesis::graph::ScopePath::new("demo").expect("valid scope");
    let package = engine.query(&query, &qconfig)?;
    println!(
        "Query returned {} knowledge + {} memory fragments, {} tensions",
        package.knowledge.len(),
        package.memories.len(),
        package.tensions.len(),
    );

    for frag in package.knowledge.iter().chain(package.memories.iter()) {
        println!(
            "  [{:.2}] {} ({:?})",
            frag.relevance, frag.name, frag.node_type
        );
    }

    println!("Done.");
    Ok(())
}
