use std::collections::HashMap;

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{Edge, EdgeId, EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::{ActivationEdge, NodeInfo, SearchInput, spread_activation};
use anamnesis::query::{
    spread_activation_with_convergence, spread_activation_with_model_and_convergence,
};
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId, SpreadingModel};

fn activation_edge(
    source: NodeId,
    target: NodeId,
    weight: f64,
    valid_until: Option<Timestamp>,
) -> ActivationEdge {
    ActivationEdge {
        target_id: target,
        edge: Edge {
            id: EdgeId(source.0.saturating_mul(1_000) + target.0),
            source,
            target,
            edge_type: EdgeType::Semantic,
            weight,
            conductance: 0.0,
            edge_source: anamnesis::graph::edge::EdgeSource::Auto,
            created_at: Timestamp(0),
            accessed_at: Timestamp(0),
            valid_from: None,
            valid_until,
            metadata: HashMap::new(),
        },
        is_forward: true,
    }
}

fn inert_node() -> NodeInfo {
    NodeInfo {
        salience: 1.0,
        mass: 0.0,
        outgoing_edges: Vec::new(),
    }
}

fn initial_activation(node_id: NodeId) -> HashMap<NodeId, f64> {
    HashMap::from([(node_id, 1.0)])
}

fn single_edge_info() -> impl Fn(NodeId) -> Option<NodeInfo> {
    move |node_id| match node_id.0 {
        0 => Some(NodeInfo {
            salience: 1.0,
            mass: 0.0,
            outgoing_edges: vec![activation_edge(NodeId(0), NodeId(1), 1.0, None)],
        }),
        1 => Some(inert_node()),
        _ => None,
    }
}

fn search_observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("normalized activation search fixture: {name}"),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

#[test]
fn hub_edges_receive_less_activation_than_leaf_edges() {
    let hub = NodeId(0);
    let leaf = NodeId(1);
    let leaf_child = NodeId(2);
    let hub_targets: Vec<NodeId> = (100..164).map(NodeId).collect();
    let hub_child = hub_targets[0];
    let mut initial = HashMap::new();
    initial.insert(hub, 1.0);
    initial.insert(leaf, 1.0);

    let info_fn = move |node_id: NodeId| -> Option<NodeInfo> {
        if node_id == hub {
            Some(NodeInfo {
                salience: 1.0,
                mass: 0.0,
                outgoing_edges: hub_targets
                    .iter()
                    .map(|&target| activation_edge(hub, target, 1.0, None))
                    .collect(),
            })
        } else if node_id == leaf {
            Some(NodeInfo {
                salience: 1.0,
                mass: 0.0,
                outgoing_edges: vec![activation_edge(leaf, leaf_child, 1.0, None)],
            })
        } else {
            Some(inert_node())
        }
    };

    let result = spread_activation_with_model_and_convergence(
        initial,
        info_fn,
        100,
        0.0,
        0.65,
        1,
        Timestamp(0),
        SpreadingModel::NormalizedPriorityQueueBfs,
        None,
    );

    let hub_child_activation = result.activations[&hub_child];
    let leaf_child_activation = result.activations[&leaf_child];
    assert!(
        hub_child_activation < leaf_child_activation / 4.0,
        "hub child activation {hub_child_activation} should be substantially below leaf child activation {leaf_child_activation}"
    );
}

#[test]
fn single_edge_normalized_matches_priority_queue_bfs() {
    let legacy = spread_activation(
        initial_activation(NodeId(0)),
        single_edge_info(),
        10,
        0.0,
        0.65,
        1,
    );
    let normalized = spread_activation_with_model_and_convergence(
        initial_activation(NodeId(0)),
        single_edge_info(),
        10,
        0.0,
        0.65,
        1,
        Timestamp(0),
        SpreadingModel::NormalizedPriorityQueueBfs,
        None,
    )
    .activations;

    let delta = (legacy[&NodeId(1)] - normalized[&NodeId(1)]).abs();
    assert!(
        delta < 1e-12,
        "single-edge normalization changed activation by {delta}"
    );
}

#[test]
fn default_priority_queue_bfs_behavior_is_unchanged() {
    assert_eq!(
        EngineConfig::default().spreading_model,
        SpreadingModel::PriorityQueueBfs
    );

    let legacy = spread_activation(
        initial_activation(NodeId(0)),
        single_edge_info(),
        10,
        0.0,
        0.65,
        1,
    );
    let explicit = spread_activation_with_convergence(
        initial_activation(NodeId(0)),
        single_edge_info(),
        10,
        0.0,
        0.65,
        1,
        Timestamp(0),
        None,
    )
    .activations;

    assert_eq!(legacy, explicit);
}

#[test]
fn fan_out_counts_only_temporally_valid_edges() {
    let now = Timestamp(10);
    let source = NodeId(0);
    let valid_target = NodeId(1);
    let expired_target = NodeId(2);

    let normalized_info = move |node_id: NodeId| -> Option<NodeInfo> {
        if node_id == source {
            Some(NodeInfo {
                salience: 1.0,
                mass: 0.0,
                outgoing_edges: vec![
                    activation_edge(source, valid_target, 1.0, None),
                    activation_edge(source, expired_target, 1.0, Some(Timestamp(5))),
                ],
            })
        } else {
            Some(inert_node())
        }
    };

    let normalized = spread_activation_with_model_and_convergence(
        initial_activation(source),
        normalized_info,
        10,
        0.0,
        0.65,
        1,
        now,
        SpreadingModel::NormalizedPriorityQueueBfs,
        None,
    );

    let legacy = spread_activation(
        initial_activation(source),
        single_edge_info(),
        10,
        0.0,
        0.65,
        1,
    );
    let delta = (normalized.activations[&valid_target] - legacy[&valid_target]).abs();

    assert!(
        delta < 1e-12,
        "expired edge was counted in fan-out normalization"
    );
    assert!(!normalized.activations.contains_key(&expired_target));
    assert_eq!(normalized.edge_count_skipped_invalid, 1);
}

#[test]
fn search_trace_records_normalized_spreading_model() {
    let mut config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    config.spreading_model = SpreadingModel::NormalizedPriorityQueueBfs;
    let mut engine = Engine::with_config(config);
    let IngestResult::Created(ids) = engine
        .ingest(search_observation("normalized seed"))
        .unwrap()
    else {
        panic!("expected created seed");
    };
    let _seed = ids[0];

    let result = engine
        .search(SearchInput {
            text: "normalized".to_string(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        result.trace.spreading_model,
        Some(SpreadingModel::NormalizedPriorityQueueBfs)
    );
}
