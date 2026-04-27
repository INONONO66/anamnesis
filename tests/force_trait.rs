use anamnesis::mechanics::forces::{
    ActivationForce, Force, IdentityForce, MassForce, NodeContext, QueryContext, RepulsionForce,
    SalienceForce, SimilarityForce,
};

fn sample_contexts() -> (NodeContext, QueryContext) {
    (
        NodeContext {
            activation: 0.8,
            vector_similarity: 0.7,
            salience: 0.6,
            mass: 0.5,
            repulsion: 0.25,
            identity_prior: 0.4,
        },
        QueryContext::default(),
    )
}

#[test]
fn all_forces_compute_without_panic() {
    let (node, query) = sample_contexts();
    let forces: [&dyn Force; 6] = [
        &ActivationForce,
        &SimilarityForce,
        &SalienceForce,
        &MassForce,
        &RepulsionForce,
        &IdentityForce,
    ];

    for force in forces {
        let value = force.compute(&node, &query);
        assert!(
            value.is_finite(),
            "force returned non-finite value: {value}"
        );
    }
}

#[test]
fn all_weights_are_positive() {
    let forces: [&dyn Force; 6] = [
        &ActivationForce,
        &SimilarityForce,
        &SalienceForce,
        &MassForce,
        &RepulsionForce,
        &IdentityForce,
    ];

    for force in forces {
        assert!(force.weight() > 0.0, "force weight must be positive");
    }
}

#[test]
fn activation_force_weight_matches_current_formula() {
    let weight = ActivationForce.weight();
    assert!((weight - 0.50).abs() < 1e-9, "expected 0.50, got {weight}");
}

#[test]
fn current_final_score_components_keep_existing_weights() {
    assert!((SimilarityForce.weight() - 0.20).abs() < 1e-9);
    assert!((SalienceForce.weight() - 0.15).abs() < 1e-9);
    assert!((MassForce.weight() - 0.15).abs() < 1e-9);
}
