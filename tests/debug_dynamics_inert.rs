use std::collections::HashMap;

use anamnesis::graph::{Edge, EdgeId, Timestamp};
use anamnesis::mechanics::forgetting::{decay_salience, floor_for_type, lambda_for_type};
use anamnesis::mechanics::gravity::{compute_mass, mass_prior};
use anamnesis::mechanics::repulsion::rigidity;
use anamnesis::query::assembly::{is_identity_type, is_memory_type};
use anamnesis::query::identity::pi_tier;
use anamnesis::query::{ActivationEdge, NodeInfo, spread_activation};
use anamnesis::{EdgeType, KnowledgeType, NodeId};

fn debug_node_types() -> [KnowledgeType; 3] {
    [
        KnowledgeType::Hypothesis,
        KnowledgeType::Evidence,
        KnowledgeType::DebugSession,
    ]
}

#[test]
fn debug_node_types_have_inert_decay_values() {
    for node_type in debug_node_types() {
        assert_eq!(lambda_for_type(&node_type), 0.0);
        assert_eq!(floor_for_type(&node_type), 1.0);
        assert_eq!(decay_salience(0.7, 365.0, &node_type), 0.7);
        assert_eq!(decay_salience(1.0, 365.0, &node_type), 1.0);
    }
}

#[test]
fn debug_node_types_have_low_mass_and_rigidity() {
    for node_type in debug_node_types() {
        assert_eq!(mass_prior(&node_type), 0.10);
        assert_eq!(rigidity(&node_type), 0.10);

        let mass = compute_mass(0.0, 0, &node_type);
        assert!((mass - 0.015).abs() < 1e-10, "mass={mass}");
    }
}

#[test]
fn debug_node_types_are_not_identity_or_memory() {
    for node_type in debug_node_types() {
        assert!(!is_identity_type(&node_type));
        assert!(!is_memory_type(&node_type));
        assert_eq!(pi_tier(&node_type), 0.0);
    }
}

#[test]
fn debug_edge_kappa_values_match_plan() {
    assert_eq!(EdgeType::Supports.kappa(true), 1.10);
    assert_eq!(EdgeType::Supports.kappa(false), 1.10);
    assert_eq!(EdgeType::Refutes.kappa(true), 0.30);
    assert_eq!(EdgeType::Refutes.kappa(false), 0.30);
    assert_eq!(EdgeType::BelongsTo.kappa(true), 0.95);
    assert_eq!(EdgeType::BelongsTo.kappa(false), 0.95);
}

#[test]
fn refutes_is_supportive_and_propagates_activation() {
    let source = NodeId(1);
    let target = NodeId(2);
    let mut initial = HashMap::new();
    initial.insert(source, 1.0);

    let info_fn = move |node_id: NodeId| -> Option<NodeInfo> {
        if node_id == source {
            Some(NodeInfo {
                salience: 1.0,
                mass: 0.0,
                outgoing_edges: vec![ActivationEdge {
                    target_id: target,
                    edge: Edge {
                        id: EdgeId(1),
                        source,
                        target,
                        edge_type: EdgeType::Refutes,
                        weight: 1.0,
                        created_at: Timestamp(0),
                        valid_from: None,
                        valid_until: None,
                        metadata: HashMap::new(),
                    },
                    is_forward: true,
                }],
            })
        } else if node_id == target {
            Some(NodeInfo {
                salience: 1.0,
                mass: 0.0,
                outgoing_edges: vec![],
            })
        } else {
            None
        }
    };

    let activations = spread_activation(initial, info_fn, 10, 0.01, 1.0, 1);

    assert_eq!(activations.get(&target).copied(), Some(0.30));
}

#[test]
fn only_contradicts_is_inhibitory_for_kappa() {
    assert_eq!(EdgeType::Contradicts.kappa(true), 0.0);
    assert!(EdgeType::Refutes.kappa(true) > 0.0);
}
