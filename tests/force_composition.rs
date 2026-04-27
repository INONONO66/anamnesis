use anamnesis::mechanics::forces::{
    ActivationForce, Force, MassForce, NodeContext, QueryContext, SalienceForce, SimilarityForce,
};
use anamnesis::query::{all_forces, compute_with_forces, final_score};

fn old_formula(activation: f64, vector_sim: f64, salience: f64, mass: f64, scope_w: f64) -> f64 {
    let raw = 0.50 * activation + 0.20 * vector_sim + 0.15 * salience + 0.15 * mass;
    (raw * scope_w).clamp(0.0, 1.0)
}

#[test]
fn composition_matches_old_formula() {
    let node = NodeContext::scoring(0.72, 0.33, 0.91, 0.44);
    let query = QueryContext { scope_weight: 0.85 };
    let forces = all_forces();

    let composed = compute_with_forces(&node, &query, &forces);
    let expected = old_formula(0.72, 0.33, 0.91, 0.44, 0.85);

    assert!((composed - expected).abs() < 1e-9);
}

#[test]
fn final_score_matches_old_formula() {
    let score = final_score(0.41, 0.82, 0.63, 0.27, 0.55);
    let expected = old_formula(0.41, 0.82, 0.63, 0.27, 0.55);

    assert!((score - expected).abs() < 1e-9);
}

#[test]
fn out_of_range_finite_inputs_match_old_formula() {
    let node = NodeContext::scoring(1.5, -0.5, 0.2, 0.3);
    let query = QueryContext { scope_weight: 0.8 };
    let forces = all_forces();

    let composed = compute_with_forces(&node, &query, &forces);
    let final_path = final_score(1.5, -0.5, 0.2, 0.3, 0.8);
    let expected = old_formula(1.5, -0.5, 0.2, 0.3, 0.8);

    assert!((composed - expected).abs() < 1e-9);
    assert!((final_path - expected).abs() < 1e-9);
}

#[test]
fn disabling_one_scoring_force_changes_score() {
    let node = NodeContext::scoring(0.7, 0.6, 0.5, 0.4);
    let query = QueryContext { scope_weight: 1.0 };
    let default_forces = all_forces();
    let without_mass: [&dyn Force; 3] = [&ActivationForce, &SimilarityForce, &SalienceForce];

    let default_score = compute_with_forces(&node, &query, &default_forces);
    let ablated_score = compute_with_forces(&node, &query, &without_mass);

    assert!((default_score - ablated_score).abs() > 1e-9);
}

#[test]
fn explicit_four_force_list_matches_default_list() {
    let node = NodeContext::scoring(0.25, 0.5, 0.75, 1.0);
    let query = QueryContext { scope_weight: 0.45 };
    let explicit_forces: [&dyn Force; 4] = [
        &ActivationForce,
        &SimilarityForce,
        &SalienceForce,
        &MassForce,
    ];
    let default_forces = all_forces();

    let explicit_score = compute_with_forces(&node, &query, &explicit_forces);
    let default_score = compute_with_forces(&node, &query, &default_forces);

    assert!((explicit_score - default_score).abs() < 1e-9);
}
