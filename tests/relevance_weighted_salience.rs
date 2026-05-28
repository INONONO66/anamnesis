use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, Timestamp};
use anamnesis::{CrystallizeRequest, Engine, IngestResult, StorageAdapter};

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "test-session".to_string(),
        scope: anamnesis::graph::ScopePath::new("test-project").expect("valid scope"),
        confidence: 0.9,
    }
}

fn make_source(engine: &mut Engine, name: &str, salience: f64) -> NodeId {
    let result = engine
        .ingest(Observation {
            name: name.to_string(),
            summary: None,
            content: format!("Content for {name}"),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![],
            origin: origin(),
            timestamp: Timestamp(1000),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    let IngestResult::Created(ids) = result else {
        panic!("expected source creation");
    };
    let id = ids[0];
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(id, salience)
        .unwrap();
    id
}

#[test]
fn opposing_relevances_produce_different_saliences() {
    let mut engine = Engine::new();

    let low_a = make_source(&mut engine, "low-a", 0.3);
    let high_a = make_source(&mut engine, "high-a", 0.9);
    let low_b = make_source(&mut engine, "low-b", 0.3);
    let high_b = make_source(&mut engine, "high-b", 0.9);

    let crystal_a = engine
        .crystallize(CrystallizeRequest {
            name: "crystal-a".to_string(),
            summary: None,
            content: "Synthesis A".to_string(),
            embedding: Some(vec![1.0, 0.0, 0.0]),
            source_ids: vec![low_a, high_a],
            source_relevances: Some(vec![0.9, 0.1]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    let crystal_b = engine
        .crystallize(CrystallizeRequest {
            name: "crystal-b".to_string(),
            summary: None,
            content: "Synthesis B".to_string(),
            embedding: Some(vec![0.0, 1.0, 0.0]),
            source_ids: vec![low_b, high_b],
            source_relevances: Some(vec![0.1, 0.9]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    // Crystal A: s̄ = (0.3 × 0.9 + 0.9 × 0.1) / 1.0 = 0.36
    //   s_c = 0.60 × 0.36 + 0.25 × 0.8 + 0.15 × 0.10 = 0.431
    let expected_a = 0.60 * 0.36 + 0.25 * 0.8 + 0.15 * 0.10;
    assert!(
        (crystal_a.initial_salience - expected_a).abs() < 1e-10,
        "crystal_a salience: expected {expected_a}, got {}",
        crystal_a.initial_salience,
    );

    // Crystal B: s̄ = (0.3 × 0.1 + 0.9 × 0.9) / 1.0 = 0.84
    //   s_c = 0.60 × 0.84 + 0.25 × 0.8 + 0.15 × 0.10 = 0.719
    let expected_b = 0.60 * 0.84 + 0.25 * 0.8 + 0.15 * 0.10;
    assert!(
        (crystal_b.initial_salience - expected_b).abs() < 1e-10,
        "crystal_b salience: expected {expected_b}, got {}",
        crystal_b.initial_salience,
    );

    assert!(
        crystal_a.initial_salience < crystal_b.initial_salience,
        "relevance weighting should shift salience: A={} vs B={}",
        crystal_a.initial_salience,
        crystal_b.initial_salience,
    );

    let difference = crystal_b.initial_salience - crystal_a.initial_salience;
    assert!(
        difference > 0.20,
        "salience difference should be substantial: {difference}",
    );
}

#[test]
fn exact_three_source_weighted_formula() {
    let mut engine = Engine::new();

    let src_1 = make_source(&mut engine, "src-1", 0.4);
    let src_2 = make_source(&mut engine, "src-2", 0.6);
    let src_3 = make_source(&mut engine, "src-3", 0.8);

    let result = engine
        .crystallize(CrystallizeRequest {
            name: "three-source crystal".to_string(),
            summary: None,
            content: "Synthesis from three sources".to_string(),
            embedding: Some(vec![0.5, 0.5, 0.0]),
            source_ids: vec![src_1, src_2, src_3],
            source_relevances: Some(vec![0.5, 0.3, 0.2]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.7,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    // s̄ = (0.4 × 0.5 + 0.6 × 0.3 + 0.8 × 0.2) / (0.5 + 0.3 + 0.2)
    //    = (0.20 + 0.18 + 0.16) / 1.0 = 0.54
    // s_c = 0.60 × 0.54 + 0.25 × 0.7 + 0.15 × 0.10
    //     = 0.324 + 0.175 + 0.015 = 0.514
    let expected = 0.60 * 0.54 + 0.25 * 0.7 + 0.15 * 0.10;
    assert!(
        (result.initial_salience - expected).abs() < 1e-10,
        "expected {expected}, got {}",
        result.initial_salience,
    );
}

#[test]
fn none_relevances_produce_uniform_weighting() {
    let mut engine = Engine::new();

    let src_1 = make_source(&mut engine, "src-1", 0.3);
    let src_2 = make_source(&mut engine, "src-2", 0.7);

    let result = engine
        .crystallize(CrystallizeRequest {
            name: "uniform crystal".to_string(),
            summary: None,
            content: "Synthesis with uniform weights".to_string(),
            embedding: None,
            source_ids: vec![src_1, src_2],
            source_relevances: None,
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    // s̄ = (0.3 + 0.7) / 2.0 = 0.50
    // s_c = 0.60 × 0.50 + 0.25 × 0.8 + 0.15 × 0.10 = 0.515
    let expected = 0.60 * 0.50 + 0.25 * 0.8 + 0.15 * 0.10;
    assert!(
        (result.initial_salience - expected).abs() < 1e-10,
        "expected {expected}, got {}",
        result.initial_salience,
    );
}

#[test]
fn zero_relevances_fall_back_to_uniform_mean() {
    let mut engine = Engine::new();

    let src_1 = make_source(&mut engine, "src-1", 0.4);
    let src_2 = make_source(&mut engine, "src-2", 0.6);

    let result = engine
        .crystallize(CrystallizeRequest {
            name: "zero-weight crystal".to_string(),
            summary: None,
            content: "Synthesis with zero relevance weights".to_string(),
            embedding: None,
            source_ids: vec![src_1, src_2],
            source_relevances: Some(vec![0.0, 0.0]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.9,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    // s̄ = (0.4 + 0.6) / 2.0 = 0.50 (fallback: weight_sum ≤ EPSILON)
    // s_c = 0.60 × 0.50 + 0.25 × 0.9 + 0.15 × 0.10 = 0.540
    let expected = 0.60 * 0.50 + 0.25 * 0.9 + 0.15 * 0.10;
    assert!(
        (result.initial_salience - expected).abs() < 1e-10,
        "expected {expected}, got {}",
        result.initial_salience,
    );
}
