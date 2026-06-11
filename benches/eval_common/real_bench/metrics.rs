use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedRetrieval {
    pub matched_gold_units: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RetrievalMetrics {
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub ndcg_at_k: f64,
}

/// One-based rank of the first retrieval that matches any gold unit.
pub fn first_hit_rank(ranked: &[RankedRetrieval]) -> Option<usize> {
    ranked
        .iter()
        .position(|item| !item.matched_gold_units.is_empty())
        .map(|index| index + 1)
}

pub fn retrieval_metrics(
    ranked: &[RankedRetrieval],
    total_relevant: usize,
    k: usize,
) -> RetrievalMetrics {
    if k == 0 {
        return RetrievalMetrics::default();
    }

    let gains = novelty_gains(ranked, k);
    RetrievalMetrics {
        precision_at_k: precision_at_k(&gains, k),
        recall_at_k: recall_at_k(&gains, total_relevant),
        mrr: mrr(&gains),
        ndcg_at_k: ndcg_at_k(&gains, total_relevant, k),
    }
}

fn novelty_gains(ranked: &[RankedRetrieval], k: usize) -> Vec<usize> {
    let mut seen = HashSet::new();
    ranked
        .iter()
        .take(k)
        .map(|item| {
            let mut gained = 0usize;
            for unit in &item.matched_gold_units {
                if seen.insert(unit.clone()) {
                    gained += 1;
                }
            }
            gained
        })
        .collect()
}

fn precision_at_k(gains: &[usize], k: usize) -> f64 {
    let hits = gains.iter().filter(|gain| **gain > 0).count();
    hits as f64 / k as f64
}

fn recall_at_k(gains: &[usize], total_relevant: usize) -> f64 {
    if total_relevant == 0 {
        return 0.0;
    }
    let hits: usize = gains.iter().sum();
    hits.min(total_relevant) as f64 / total_relevant as f64
}

fn mrr(gains: &[usize]) -> f64 {
    gains
        .iter()
        .position(|gained| *gained > 0)
        .map_or(0.0, |index| 1.0 / (index + 1) as f64)
}

fn ndcg_at_k(gains: &[usize], total_relevant: usize, k: usize) -> f64 {
    let dcg = dcg_at_k(gains, k);
    let ideal_gains = ideal_gains(gains, total_relevant, k);
    if ideal_gains.is_empty() {
        return 0.0;
    }
    dcg / dcg_at_k(&ideal_gains, k)
}

fn dcg_at_k(gains: &[usize], k: usize) -> f64 {
    gains
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, gain)| **gain > 0)
        .map(|(index, gain)| *gain as f64 / ((index + 2) as f64).log2())
        .sum()
}

fn ideal_gains(gains: &[usize], total_relevant: usize, k: usize) -> Vec<usize> {
    let observed_hits: usize = gains.iter().sum();
    let mut ideal: Vec<_> = gains.iter().copied().filter(|gain| *gain > 0).collect();
    ideal.sort_unstable_by(|left, right| right.cmp(left));
    if observed_hits < total_relevant {
        ideal.extend(std::iter::repeat_n(1, total_relevant - observed_hits));
    }
    ideal.truncate(k);
    ideal
}
