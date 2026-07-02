use anamnesis::engine::EdgeType;
use anamnesis::mechanics::priors::edge_type_factor;

// The former Hypothesis/Evidence/DebugSession "inert debug-lifecycle" KnowledgeType
// variants were removed in the 15→4 collapse (they now ride `Custom(_)` and decay at
// the ordinary-knowledge rate). The node-type decay/partition assertions that used to
// live here went away with them; the edge-type propagation invariants below are on
// `EdgeType` (untouched) and still hold.

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
