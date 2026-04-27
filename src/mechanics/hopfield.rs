//! Modern Hopfield retrieval mechanics.
//!
//! Pure, dependency-free helpers for small vector pattern completion.

/// Inverse temperature for attention over stored patterns.
///
/// A moderately sharp default keeps retrieval deterministic for clearly separated
/// low-dimensional patterns while still producing a smooth weighted sum.
const BETA: f64 = 8.0;

/// Computes Modern Hopfield-style energy for a state against stored patterns.
///
/// Uses the continuous energy form:
/// `E(x) = 0.5 * ||x||^2 - (1 / beta) * log(sum_i exp(beta * <x, p_i>))`.
///
/// The log-sum-exp term subtracts the maximum scaled similarity before
/// exponentiation for numerical stability. Patterns with mismatched dimensions
/// or non-finite values are ignored. If no valid pattern remains, the finite
/// quadratic term is returned as a conservative fallback.
pub fn energy(state: &[f64], patterns: &[Vec<f64>]) -> f64 {
    if state.is_empty() || !is_finite_vector(state) {
        return 0.0;
    }

    let norm = half_norm_squared(state);
    let mut max_scaled = f64::NEG_INFINITY;
    let mut scaled_similarities = Vec::new();

    for pattern in patterns {
        if pattern.len() != state.len() || !is_finite_vector(pattern) {
            continue;
        }

        let scaled = BETA * dot(state, pattern);
        if scaled.is_finite() {
            max_scaled = max_scaled.max(scaled);
            scaled_similarities.push(scaled);
        }
    }

    if scaled_similarities.is_empty() || !max_scaled.is_finite() {
        return norm;
    }

    let exp_sum: f64 = scaled_similarities
        .iter()
        .map(|similarity| (similarity - max_scaled).exp())
        .sum();

    if !exp_sum.is_finite() || exp_sum <= f64::EPSILON {
        return norm;
    }

    norm - (max_scaled + exp_sum.ln()) / BETA
}

/// Retrieves a completed pattern from a seed by iterative softmax attention.
///
/// Each iteration computes similarities between the current state and valid
/// stored patterns, applies a numerically stable softmax, and returns the
/// weighted sum of patterns. Empty inputs, dimension mismatches, non-finite
/// values, and degenerate softmax sums fall back to `seed.to_vec()`.
pub fn retrieve(seed: &[f64], patterns: &[Vec<f64>], iterations: usize) -> Vec<f64> {
    let original = seed.to_vec();

    if seed.is_empty() || patterns.is_empty() || !is_finite_vector(seed) {
        return original;
    }

    let mut state = original.clone();

    for _ in 0..iterations {
        let Some(next) = retrieve_once(&state, patterns) else {
            return original;
        };

        state = next;
    }

    state
}

fn retrieve_once(state: &[f64], patterns: &[Vec<f64>]) -> Option<Vec<f64>> {
    let mut valid_patterns = Vec::new();
    let mut max_scaled = f64::NEG_INFINITY;
    let mut scaled_similarities = Vec::new();

    for pattern in patterns {
        if pattern.len() != state.len() || !is_finite_vector(pattern) {
            continue;
        }

        let scaled = BETA * dot(state, pattern);
        if scaled.is_finite() {
            max_scaled = max_scaled.max(scaled);
            scaled_similarities.push(scaled);
            valid_patterns.push(pattern.as_slice());
        }
    }

    if valid_patterns.is_empty() || !max_scaled.is_finite() {
        return None;
    }

    let mut weights = Vec::with_capacity(scaled_similarities.len());
    let mut weight_sum = 0.0;

    for similarity in scaled_similarities {
        let weight = (similarity - max_scaled).exp();
        weight_sum += weight;
        weights.push(weight);
    }

    if !weight_sum.is_finite() || weight_sum <= f64::EPSILON {
        return None;
    }

    let mut next = vec![0.0; state.len()];

    for (weight, pattern) in weights.iter().zip(valid_patterns) {
        let normalized = weight / weight_sum;
        for (value, pattern_value) in next.iter_mut().zip(pattern) {
            *value += normalized * pattern_value;
        }
    }

    if is_finite_vector(&next) {
        Some(next)
    } else {
        None
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn half_norm_squared(values: &[f64]) -> f64 {
    0.5 * values.iter().map(|value| value * value).sum::<f64>()
}

fn is_finite_vector(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}
