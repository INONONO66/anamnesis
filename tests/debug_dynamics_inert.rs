use anamnesis::mechanics::interactions::decay_default;
use anamnesis::mechanics::priors::{decay_multiplier_for_type, edge_type_factor};
use anamnesis::mechanics::gravity::{compute_mass, mass_prior};
use anamnesis::mechanics::repulsion::rigidity;
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
    // Debug-lifecycle nodes are inert: their per-type decay multiplier is 0, so
    // power-law dissipation leaves the retained-action reservoir A_i unchanged
    // regardless of elapsed time (no [0,1] floor on the reservoir path).
    for node_type in debug_node_types() {
        assert_eq!(decay_multiplier_for_type(&node_type), 0.0);
        assert_eq!(decay_default(0.7, 365.0, &node_type), 0.7);
        assert_eq!(decay_default(-2.0, 365.0, &node_type), -2.0);
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
    // Refutes is a weak supportive relation in the additive-RWR conductance matrix:
    // its within-row edge-type factor is positive (it propagates), unlike Contradicts
    // which is excluded (factor 0).
    assert!(edge_type_factor(&EdgeType::Refutes, true) > 0.0);
    assert!(edge_type_factor(&EdgeType::Refutes, false) > 0.0);
    assert_eq!(edge_type_factor(&EdgeType::Contradicts, true), 0.0);
    // Refutes is the weakest supportive type, below Semantic.
    assert!(edge_type_factor(&EdgeType::Refutes, true) < edge_type_factor(&EdgeType::Semantic, true));
}

#[test]
fn only_contradicts_is_inhibitory_for_kappa() {
    assert_eq!(EdgeType::Contradicts.kappa(true), 0.0);
    assert!(EdgeType::Refutes.kappa(true) > 0.0);
}
