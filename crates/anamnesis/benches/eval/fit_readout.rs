//! Offline readout-coefficient fit (ADR-0010): coordinate search over
//! (w_a, w_phi, w_s, w_z) maximizing mean novelty-deduped NDCG@20 on a train
//! split of dumped feature rows. Even sample_index = train, odd = dev. Never
//! fit on eval data you intend to report.
//!
//! Objective: replayed novelty-deduped NDCG@20 computed directly from
//! `matched_units` and `total_relevant` stored in each row, mirroring the
//! semantics in `benches/eval_common/real_bench/metrics.rs` exactly. This
//! eliminates the per-node-label proxy divergence where a fitted point could
//! improve proxy MRR while live novelty-deduped MRR dropped.
//!
//! Remaining caveat: the feature rows capture only the top-200 nodes from the
//! live readout surface. An optimal weight vector can promote nodes from
//! *outside* that surface that were never scored, so fitted points must still
//! be confirmed by a live evaluation run.
//!
//! Backward compatibility: rows produced by older dumps lack `matched_units`
//! and `total_relevant`. Serde will error on those rows rather than silently
//! misuse stale data. Re-dump features with the updated eval binary before
//! running this tool.
//!
//! Usage: cargo bench --bench fit_readout -- <features.jsonl> [top_k]

use std::collections::{BTreeMap, HashSet};
use std::io::BufRead;

const EPS: f64 = 1e-6;
const GRID: [f64; 9] = [0.0, 0.25, 0.5, 1.0, 1.5, 2.0, 4.0, 8.0, 16.0];

#[derive(Debug, Clone, serde::Deserialize)]
struct Row {
    question_id: String,
    sample_index: usize,
    /// Kept for schema presence validation only; the objective uses matched_units.
    #[allow(dead_code)]
    label: bool,
    matched_units: Vec<String>,
    total_relevant: usize,
    activation: f64,
    phi: f64,
    salience: f64,
    impedance: f64,
    scope_weight: f64,
    trust_weight: f64,
    stress: f64,
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(EPS, 1.0 - EPS);
    (p / (1.0 - p)).ln()
}

fn score(row: &Row, w: [f64; 4]) -> f64 {
    w[0] * logit(row.activation) + w[1] * row.phi + w[2] * logit(row.salience)
        - w[3] * row.impedance
        + row.scope_weight
        + row.trust_weight
        - row.stress
}

/// Replay novelty dedup in rank order (mirrors `metrics.rs::novelty_gains`).
/// Returns a Vec<usize> of per-position gains (newly seen gold units).
fn novelty_gains(sorted_rows: &[&Row], top_k: usize) -> Vec<usize> {
    let mut seen: HashSet<String> = HashSet::new();
    sorted_rows
        .iter()
        .take(top_k)
        .map(|row| {
            let mut gained = 0usize;
            for unit in &row.matched_units {
                if seen.insert(unit.clone()) {
                    gained += 1;
                }
            }
            gained
        })
        .collect()
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

fn ndcg_at_k(gains: &[usize], total_relevant: usize, k: usize) -> f64 {
    let dcg = dcg_at_k(gains, k);
    let ig = ideal_gains(gains, total_relevant, k);
    if ig.is_empty() {
        return 0.0;
    }
    dcg / dcg_at_k(&ig, k)
}

fn mrr(gains: &[usize]) -> f64 {
    gains
        .iter()
        .position(|gained| *gained > 0)
        .map_or(0.0, |index| 1.0 / (index + 1) as f64)
}

fn recall_at_k(gains: &[usize], total_relevant: usize) -> f64 {
    if total_relevant == 0 {
        return 0.0;
    }
    let hits: usize = gains.iter().sum();
    hits.min(total_relevant) as f64 / total_relevant as f64
}

/// Compute all three deduped metrics for a question's rows under weight vector `w`.
fn question_metrics(rows: &[Row], w: [f64; 4], top_k: usize) -> (f64, f64, f64) {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| score(b, w).total_cmp(&score(a, w)));
    let total_relevant = rows.first().map_or(0, |r| r.total_relevant);
    let gains = novelty_gains(&sorted, top_k);
    (
        ndcg_at_k(&gains, total_relevant, top_k),
        mrr(&gains),
        recall_at_k(&gains, total_relevant),
    )
}

/// Primary objective: mean NDCG@k over all questions.
fn mean_ndcg(questions: &BTreeMap<String, Vec<Row>>, w: [f64; 4], top_k: usize) -> f64 {
    if questions.is_empty() {
        return 0.0;
    }
    let total: f64 = questions
        .values()
        .map(|rows| question_metrics(rows, w, top_k).0)
        .sum();
    total / questions.len() as f64
}

/// Compute mean deduped MRR and mean recall@k alongside NDCG for reporting.
fn mean_metrics(
    questions: &BTreeMap<String, Vec<Row>>,
    w: [f64; 4],
    top_k: usize,
) -> (f64, f64, f64) {
    if questions.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let n = questions.len() as f64;
    let mut sum_ndcg = 0.0;
    let mut sum_mrr = 0.0;
    let mut sum_recall = 0.0;
    for rows in questions.values() {
        let (ndcg, m, recall) = question_metrics(rows, w, top_k);
        sum_ndcg += ndcg;
        sum_mrr += m;
        sum_recall += recall;
    }
    (sum_ndcg / n, sum_mrr / n, sum_recall / n)
}

fn fit(train: &BTreeMap<String, Vec<Row>>, top_k: usize) -> ([f64; 4], f64) {
    let mut best = [1.0, 1.0, 1.0, 1.0];
    let mut best_ndcg = mean_ndcg(train, best, top_k);
    loop {
        let mut improved = false;
        for coordinate in 0..4 {
            for &value in &GRID {
                let mut candidate = best;
                candidate[coordinate] = value;
                let ndcg = mean_ndcg(train, candidate, top_k);
                if ndcg > best_ndcg + 1e-9 {
                    best = candidate;
                    best_ndcg = ndcg;
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
    (best, best_ndcg)
}

fn main() {
    let mut args = std::env::args().skip(1).filter(|a| a != "--bench");
    let path = args
        .next()
        .expect("usage: fit_readout <features.jsonl> [top_k]");
    let top_k: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(20);

    let file = std::fs::File::open(&path).expect("open features file");
    let mut train: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut dev: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        let row: Row = serde_json::from_str(&line).expect("parse row");
        let bucket = if row.sample_index.is_multiple_of(2) {
            &mut train
        } else {
            &mut dev
        };
        bucket.entry(row.question_id.clone()).or_default().push(row);
    }
    eprintln!(
        "train questions={} dev questions={}",
        train.len(),
        dev.len()
    );

    let unit = [1.0, 1.0, 1.0, 1.0];
    let (best, _best_ndcg) = fit(&train, top_k);

    let (unit_train_ndcg, unit_train_mrr, unit_train_recall) = mean_metrics(&train, unit, top_k);
    let (unit_dev_ndcg, unit_dev_mrr, unit_dev_recall) = mean_metrics(&dev, unit, top_k);
    let (train_ndcg, train_mrr, train_recall) = mean_metrics(&train, best, top_k);
    let (dev_ndcg, dev_mrr, dev_recall) = mean_metrics(&dev, best, top_k);

    println!(
        "{}",
        serde_json::json!({
            "w_a": best[0], "w_phi": best[1], "w_s": best[2], "w_z": best[3],
            "top_k": top_k,
            "unit": {
                "train_ndcg": unit_train_ndcg,
                "train_mrr": unit_train_mrr,
                "train_recall_at_k": unit_train_recall,
                "dev_ndcg": unit_dev_ndcg,
                "dev_mrr": unit_dev_mrr,
                "dev_recall_at_k": unit_dev_recall,
            },
            "fitted": {
                "train_ndcg": train_ndcg,
                "train_mrr": train_mrr,
                "train_recall_at_k": train_recall,
                "dev_ndcg": dev_ndcg,
                "dev_mrr": dev_mrr,
                "dev_recall_at_k": dev_recall,
            },
        })
    );
}
