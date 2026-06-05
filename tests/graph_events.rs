use anamnesis::api::{GraphEvent, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, MemoryTier, ScopePath, Timestamp};
use anamnesis::{Engine, EngineConfig, IngestResult, StorageAdapter};

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: ScopePath::universal(),
        confidence: 0.9,
    }
}

fn observation(name: &str, embedding: Option<Vec<f64>>, timestamp: Timestamp) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["events".to_string()],
        origin: origin(),
        timestamp,
        valid_from: None,
        valid_until: None,
    }
}

fn created_id(result: IngestResult) -> anamnesis::NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    }
}

#[test]
fn ingest_emits_node_created_and_attraction_edge_created() {
    let config = EngineConfig::new()
        .with_dedup_threshold(0.95)
        .with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let first = created_id(
        engine
            .ingest(observation("first", Some(vec![1.0, 0.0]), Timestamp(0)))
            .expect("first ingest succeeds"),
    );
    let second = created_id(
        engine
            .ingest(observation("second", Some(vec![0.8, 0.6]), Timestamp(1)))
            .expect("second ingest succeeds"),
    );

    let events = engine.drain_events();
    // Verify NodeCreated events for both nodes
    let node_created_first = events.iter().any(|e| {
        matches!(e, GraphEvent::NodeCreated { node_id, node_type: KnowledgeType::Semantic } if *node_id == first)
    });
    let node_created_second = events.iter().any(|e| {
        matches!(e, GraphEvent::NodeCreated { node_id, node_type: KnowledgeType::Semantic } if *node_id == second)
    });
    // Verify EdgeCreated event for attraction link
    let edge_created = events.iter().any(|e| {
        matches!(e, GraphEvent::EdgeCreated { source, target, edge_type: EdgeType::Semantic, .. } if *source == second && *target == first)
    });
    assert!(
        node_created_first,
        "NodeCreated for first should be emitted"
    );
    assert!(
        node_created_second,
        "NodeCreated for second should be emitted"
    );
    assert!(
        edge_created,
        "EdgeCreated for attraction link should be emitted"
    );
}

#[test]
fn tick_emits_salience_changed_for_decayed_nodes() {
    let mut engine = Engine::new();
    let node_id = created_id(
        engine
            .ingest(observation("decays", None, Timestamp(0)))
            .expect("ingest succeeds"),
    );
    engine.drain_events();

    engine.tick(Timestamp(86_400_000)).expect("tick succeeds");

    let events = engine.drain_events();
    // The pre-decay salience is the surprise-gated projection (near, but not exactly,
    // 1.0 per ADR-0009); decay only requires that the new salience is lower.
    assert!(events.iter().any(|event| {
        matches!(
            event,
            GraphEvent::SalienceChanged { node_id: changed_id, old, new }
                if *changed_id == node_id && *new < *old
        )
    }));
}

#[test]
fn touch_emits_salience_changed_and_node_revived_when_crossing_archive_threshold() {
    let mut engine = Engine::new();
    let node_id = created_id(
        engine
            .ingest(observation("revives", None, Timestamp(0)))
            .expect("ingest succeeds"),
    );
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(node_id, 0.05)
        .expect("salience can be adjusted for test setup");
    engine.drain_events();

    engine.touch(node_id, Timestamp(1)).expect("touch succeeds");

    let events = engine.drain_events();
    assert!(matches!(
        events.as_slice(),
        [
            GraphEvent::SalienceChanged { node_id: changed_id, old, new },
            GraphEvent::NodeRevived { node_id: revived_id, new_salience },
        ] if *changed_id == node_id
            && *revived_id == node_id
            && (*old - 0.05).abs() < f64::EPSILON
            && *new >= 0.1
            && (*new_salience - *new).abs() < f64::EPSILON
    ));
}

#[test]
fn drain_clears_buffer_and_preserves_chronological_order() {
    let mut engine = Engine::new();
    let first = created_id(
        engine
            .ingest(observation("first", None, Timestamp(0)))
            .expect("first ingest succeeds"),
    );
    let second = created_id(
        engine
            .ingest(observation("second", None, Timestamp(1)))
            .expect("second ingest succeeds"),
    );
    engine
        .link(first, second, EdgeType::Causal)
        .expect("link succeeds");

    assert!(engine.has_events());
    let events = engine.drain_events();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], GraphEvent::NodeCreated { node_id, .. } if node_id == first));
    assert!(matches!(events[1], GraphEvent::NodeCreated { node_id, .. } if node_id == second));
    assert!(
        matches!(events[2], GraphEvent::EdgeCreated { source, target, edge_type: EdgeType::Causal, .. } if source == first && target == second)
    );
    assert!(!engine.has_events());
    assert!(engine.drain_events().is_empty());
}

#[test]
fn bounded_buffer_drops_oldest_events() {
    let config = EngineConfig::new().with_max_events(2);
    let mut engine = Engine::with_config(config);

    let first = created_id(
        engine
            .ingest(observation("first", None, Timestamp(0)))
            .expect("first ingest succeeds"),
    );
    let second = created_id(
        engine
            .ingest(observation("second", None, Timestamp(1)))
            .expect("second ingest succeeds"),
    );
    let third = created_id(
        engine
            .ingest(observation("third", None, Timestamp(2)))
            .expect("third ingest succeeds"),
    );

    let events = engine.drain_events();
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], GraphEvent::NodeCreated { node_id, .. } if node_id == second));
    assert!(matches!(events[1], GraphEvent::NodeCreated { node_id, .. } if node_id == third));
    assert_ne!(first, second);
}

#[test]
fn tick_emits_archive_and_tier_transition_events() {
    let mut engine = Engine::new();
    let node_id = created_id(
        engine
            .ingest(observation("archives", None, Timestamp(0)))
            .expect("ingest succeeds"),
    );
    // Salience is a pure projection of the retained-action reservoir (ADR-0002):
    // seed the *reservoir* just above the archive threshold so a year of power-law
    // dissipation pushes its projection below it. (Poking salience directly would
    // be overwritten by the reservoir on the next tick.)
    let just_above = anamnesis::mechanics::priors::salience_to_action(0.11);
    engine
        .graph_mut()
        .storage_mut()
        .set_retained_action(node_id, just_above)
        .expect("reservoir can be adjusted for test setup");
    engine.drain_events();

    engine
        .tick(Timestamp(31_536_000_000))
        .expect("tick succeeds");

    let events = engine.drain_events();
    assert!(events
        .iter()
        .any(|event| matches!(event, GraphEvent::NodeArchived { node_id: archived_id } if *archived_id == node_id)));
    assert!(events.iter().any(|event| matches!(
        event,
        GraphEvent::TierTransition {
            node_id: transitioned_id,
            from_tier: MemoryTier::Recall,
            to_tier: MemoryTier::Archival,
        } if *transitioned_id == node_id
    )));
}
