use anamnesis::mechanics::attraction::cosine_similarity;
use anamnesis::mechanics::hopfield::{energy, retrieve};

#[test]
fn pattern_completion_from_partial_cue() {
    let intended = vec![1.0, 1.0, 1.0, 1.0];
    let distractor = vec![1.0, -1.0, 1.0, -1.0];
    let opposite = vec![-1.0, -1.0, -1.0, -1.0];
    let patterns = vec![intended.clone(), distractor.clone(), opposite.clone()];
    let seed = vec![1.0, 1.0, 0.0, 0.0];

    let retrieved = retrieve(&seed, &patterns, 3);
    let intended_similarity = cosine_similarity(&retrieved, &intended);
    let distractor_similarity = cosine_similarity(&retrieved, &distractor);
    let opposite_similarity = cosine_similarity(&retrieved, &opposite);

    assert_eq!(retrieved.len(), seed.len());
    assert!(intended_similarity > 0.99);
    assert!(intended_similarity > distractor_similarity + 0.20);
    assert!(intended_similarity > opposite_similarity + 0.20);
}

#[test]
fn energy_decreases_during_retrieval() {
    let patterns = vec![
        vec![1.0, 1.0, 1.0, 1.0],
        vec![1.0, -1.0, 1.0, -1.0],
        vec![-1.0, -1.0, -1.0, -1.0],
    ];
    let seed = vec![1.0, 1.0, 0.0, 0.0];

    let once = retrieve(&seed, &patterns, 1);
    let twice = retrieve(&once, &patterns, 1);

    let initial_energy = energy(&seed, &patterns);
    let once_energy = energy(&once, &patterns);
    let twice_energy = energy(&twice, &patterns);

    assert!(initial_energy.is_finite());
    assert!(once_energy.is_finite());
    assert!(twice_energy.is_finite());
    assert!(once_energy <= initial_energy + 1e-10);
    assert!(twice_energy <= once_energy + 1e-10);
}
