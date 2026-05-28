use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::{Engine, EngineConfig};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn make_observation(i: u64) -> Observation {
    Observation {
        name: format!("node-{i}"),
        summary: None,
        content: format!("content {i}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "bench".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000 + i),
    }
}

fn print_stats(label: &str, stats: &dhat::HeapStats) {
    eprintln!(
        "| {:<30} | {:>12} | {:>12} | {:>12} | {:>12} |",
        label, stats.total_bytes, stats.total_blocks, stats.curr_bytes, stats.curr_blocks
    );
}

fn main() {
    let _profiler = dhat::Profiler::builder().testing().build();

    eprintln!(
        "| {:<30} | {:>12} | {:>12} | {:>12} | {:>12} |",
        "Stage", "total_bytes", "total_blocks", "curr_bytes", "curr_blocks"
    );
    eprintln!(
        "|{:-<32}|{:-<14}|{:-<14}|{:-<14}|{:-<14}|",
        "", "", "", "", ""
    );

    // Baseline: empty engine
    let stats = dhat::HeapStats::get();
    print_stats("baseline (pre-engine)", &stats);

    let config = EngineConfig::new()
        .with_max_nodes(200_000)
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    let stats = dhat::HeapStats::get();
    print_stats("Engine::new()", &stats);

    // Ingest 1K nodes
    for i in 0..1_000u64 {
        engine
            .ingest(make_observation(i))
            .expect("ingest should succeed");
    }
    let stats = dhat::HeapStats::get();
    print_stats("after 1K nodes", &stats);

    // Ingest 10K nodes
    for i in 1_000..10_000u64 {
        engine
            .ingest(make_observation(i))
            .expect("ingest should succeed");
    }
    let stats = dhat::HeapStats::get();
    print_stats("after 10K nodes", &stats);

    // Ingest 100K nodes
    for i in 10_000..100_000u64 {
        engine
            .ingest(make_observation(i))
            .expect("ingest should succeed");
    }
    let stats = dhat::HeapStats::get();
    print_stats("after 100K nodes", &stats);

    // Link 1K edges
    let all_ids: Vec<_> = (0..1_000).collect();
    for i in 0..999usize {
        let _ = engine.link(
            NodeId(all_ids[i] as u64 + 1),
            NodeId(all_ids[i + 1] as u64 + 1),
            EdgeType::Semantic,
            0.75,
        );
    }
    let stats = dhat::HeapStats::get();
    print_stats("after 1K edges", &stats);

    // tick() on 100K nodes
    engine
        .tick(Timestamp(2_000_000))
        .expect("tick should succeed");
    let stats = dhat::HeapStats::get();
    print_stats("after tick(100K)", &stats);

    eprintln!("\nDone. dhat heap stats emitted above.");
}
