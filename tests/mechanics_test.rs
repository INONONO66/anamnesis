use anamnesis::mechanics::{Attraction, Forgetting, Gravity, Perception};

#[test]
fn test_attraction_similarity() {
    let emb1 = vec![1.0, 0.0, 0.0];
    let emb2 = vec![1.0, 0.0, 0.0];
    let similarity = Attraction::similarity(&emb1, &emb2);
    assert!((similarity - 1.0).abs() < 0.001);
}

#[test]
fn test_gravity_in_degree() {
    let edges = vec![(1, 2, 1.0), (1, 3, 1.0), (2, 3, 1.0)];
    assert_eq!(Gravity::in_degree(2, &edges), 1);
    assert_eq!(Gravity::in_degree(3, &edges), 2);
}

#[test]
fn test_perception_novelty() {
    let obs = vec![1.0, 0.0];
    let existing_vec = vec![0.0, 1.0];
    let existing_slice: &[f64] = &existing_vec;
    let existing = vec![existing_slice];
    let novelty = Perception::novelty_score(&obs, existing.as_slice(), 0.5);
    assert!(novelty > 0.0);
}

#[test]
fn test_forgetting_decay() {
    let salience = 1.0;
    let decayed = Forgetting::exponential_decay(salience, 0.1, 10);
    assert!(decayed < salience);
    assert!(decayed > 0.0);
}

#[test]
fn test_forgetting_reinforce() {
    let salience = 0.5;
    let reinforced = Forgetting::reinforce(salience, 0.3);
    assert!(reinforced > salience);
    assert!(reinforced <= 1.0);
}
