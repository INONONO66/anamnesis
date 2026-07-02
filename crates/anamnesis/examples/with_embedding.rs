//! Demonstrates `FastEmbedProvider` for real text embedding → ingest → query.
//!
//! Requires the `embed` feature flag (downloads a model on first run).
//!
//! Run: `cargo run --features embed --example with_embedding`

#[cfg(not(feature = "embed"))]
fn main() {
    println!("This example requires the `embed` feature.");
    println!("Run: cargo run --features embed --example with_embedding");
}

#[cfg(feature = "embed")]
fn main() -> Result<(), anamnesis::Error> {
    use anamnesis::Engine;
    use anamnesis::engine::{
        EdgeType, EmbeddingProvider, EngineConfig, FastEmbedProvider, IngestResult, KnowledgeType,
        Observation, Origin, Query, QueryConfig, Timestamp,
    };

    println!("Initializing FastEmbedProvider (may download model on first run)...");
    let provider = FastEmbedProvider::new()?;
    println!(
        "Provider ready: {} ({}-d)\n",
        provider.model_name(),
        provider.dimensions()
    );

    fn origin() -> Origin {
        Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "session-1".into(),
            scope: anamnesis::graph::ScopePath::new("demo").expect("valid scope"),
            confidence: 0.9,
        }
    }

    let texts = [
        "The authentication module uses the factory pattern for handler creation",
        "A race condition was found in the auth middleware during load testing",
    ];
    let embeddings = provider.embed_f64(&texts.map(|s| s))?;
    println!(
        "Embedded {} texts into {}-d vectors",
        texts.len(),
        embeddings[0].len()
    );

    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let r1 = engine.ingest(Observation {
        name: "auth uses factory pattern".into(),
        summary: Some("Confirmed pattern across sessions".into()),
        content: texts[0].into(),
        embedding: Some(embeddings[0].clone()),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["auth".into(), "factory-pattern".into()],
        origin: origin(),
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    })?;
    let id1 = match &r1 {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => *existing_id,
    };
    println!("Ingested node {}", id1.0);

    let r2 = engine.ingest(Observation {
        name: "auth race condition".into(),
        summary: None,
        content: texts[1].into(),
        embedding: Some(embeddings[1].clone()),
        confidence: 0.85,
        node_type: KnowledgeType::Episodic,
        entity_tags: vec!["auth".into()],
        origin: origin(),
        timestamp: Timestamp(2000),
        valid_from: None,
        valid_until: None,
    })?;
    let id2 = match &r2 {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => *existing_id,
    };
    println!("Ingested node {}", id2.0);

    engine.link(id1, id2, EdgeType::Semantic)?;
    println!("Linked {} → {}\n", id1.0, id2.0);

    let query = Query::Associative {
        seed: id1,
        budget: 10,
    };
    let mut qconfig = QueryConfig::default();
    qconfig.query_embedding = Some(embeddings[0].clone());
    qconfig.scope = anamnesis::graph::ScopePath::new("demo").expect("valid scope");
    let package = engine.query(&query, &qconfig)?;

    println!(
        "Query results: {} knowledge, {} memories, {} tensions",
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

    println!("\nDone.");
    Ok(())
}
