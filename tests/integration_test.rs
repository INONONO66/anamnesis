use anamnesis::api::{Engine, EngineConfig, Observation};
use anamnesis::graph::edge::EdgeType;

#[test]
fn test_engine_creation() {
    let engine = Engine::new();
    assert_eq!(engine.graph().nodes().count(), 0);
}

#[test]
fn test_engine_ingest() {
    let mut engine = Engine::new();
    let observation = Observation {
        content: "test observation".to_string(),
        embedding: vec![1.0, 0.0, 0.0],
        confidence: 0.8,
        node_type: "concept".to_string(),
    };

    let result = engine.ingest(observation);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1);
}

#[test]
fn test_engine_link() {
    let mut engine = Engine::new();

    let obs1 = Observation {
        content: "concept1".to_string(),
        embedding: vec![1.0, 0.0],
        confidence: 0.8,
        node_type: "concept".to_string(),
    };

    let obs2 = Observation {
        content: "concept2".to_string(),
        embedding: vec![0.0, 1.0],
        confidence: 0.8,
        node_type: "concept".to_string(),
    };

    let nodes1 = engine.ingest(obs1).unwrap();
    let nodes2 = engine.ingest(obs2).unwrap();

    let result = engine.link(nodes1[0], nodes2[0], EdgeType::Semantic, 0.8);
    assert!(result.is_ok());
}

#[test]
fn test_engine_touch() {
    let mut engine = Engine::new();
    let observation = Observation {
        content: "test".to_string(),
        embedding: vec![1.0],
        confidence: 0.8,
        node_type: "concept".to_string(),
    };

    let nodes = engine.ingest(observation).unwrap();
    let result = engine.touch(nodes[0]);
    assert!(result.is_ok());
}

#[test]
fn test_engine_with_config() {
    let config = EngineConfig {
        max_nodes: 1000,
        novelty_threshold: 0.4,
        confidence_threshold: 0.6,
        decay_rate: 0.15,
        use_exponential_decay: false,
    };

    let engine = Engine::with_config(config);
    assert_eq!(engine.graph().nodes().count(), 0);
}
