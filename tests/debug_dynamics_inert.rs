use anamnesis::mechanics::priors::{DECAY_EXPONENT_D, decay_multiplier_for_type, edge_type_factor};
use anamnesis::query::assembly::{is_identity_type, is_memory_type};
use anamnesis::query::identity::pi_tier;
use anamnesis::{EdgeType, KnowledgeType};

fn debug_node_types() -> [KnowledgeType; 3] {
    [
        KnowledgeType::Hypothesis,
        KnowledgeType::Evidence,
        KnowledgeType::DebugSession,
    ]
}

#[test]
fn debug_node_types_have_inert_decay_values() {
    // Debug-lifecycle nodes are inert: their per-type decay multiplier is 0, so the
    // base-level exponent `d·m_type` collapses to 0. Every trace ages as `dt^0 = 1`,
    // so `B_i` never falls with elapsed time (ADR-0008) — the node is decay-exempt.
    for node_type in debug_node_types() {
        assert_eq!(decay_multiplier_for_type(&node_type), 0.0);
        assert_eq!(
            DECAY_EXPONENT_D * decay_multiplier_for_type(&node_type),
            0.0
        );
    }
}

#[test]
fn debug_node_types_are_inert_under_dissipation() {
    // The legacy gravity/mass force is gone (overview.md / conductance.md): importance
    // is emergent, there is no separate mass boost. Debug-lifecycle nodes are instead
    // characterized as *inert*: their per-type decay multiplier is exactly 0, so the
    // base-level decay exponent is 0 and elapsed time never lowers their base level
    // `B_i` (forgetting lives in `B_i`; `P_i` is decay-exempt regardless).
    for node_type in debug_node_types() {
        assert_eq!(decay_multiplier_for_type(&node_type), 0.0);
        assert_eq!(
            DECAY_EXPONENT_D * decay_multiplier_for_type(&node_type),
            0.0
        );
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
fn debug_edge_type_factors_match_plan() {
    assert_eq!(edge_type_factor(&EdgeType::Supports, true), 1.10);
    assert_eq!(edge_type_factor(&EdgeType::Supports, false), 1.10);
    assert_eq!(edge_type_factor(&EdgeType::Refutes, true), 0.30);
    assert_eq!(edge_type_factor(&EdgeType::Refutes, false), 0.30);
    assert_eq!(edge_type_factor(&EdgeType::BelongsTo, true), 0.95);
    assert_eq!(edge_type_factor(&EdgeType::BelongsTo, false), 0.95);
}

#[test]
fn refutes_is_supportive_and_propagates_activation() {
    // Refutes is a weak supportive relation in the additive-RWR conductance matrix:
    // its within-row edge-type factor is positive (it propagates), unlike Contradicts
    // which is excluded (factor 0).
    assert!(edge_type_factor(&EdgeType::Refutes, true) > 0.0);
    assert!(edge_type_factor(&EdgeType::Refutes, false) > 0.0);
    assert_eq!(edge_type_factor(&EdgeType::Contradicts, true), 0.0);
    // Refutes is the weakest supportive type, below Semantic.
    assert!(
        edge_type_factor(&EdgeType::Refutes, true) < edge_type_factor(&EdgeType::Semantic, true)
    );
}

#[test]
fn only_contradicts_is_excluded_from_propagation() {
    assert_eq!(edge_type_factor(&EdgeType::Contradicts, true), 0.0);
    assert!(edge_type_factor(&EdgeType::Refutes, true) > 0.0);
}
