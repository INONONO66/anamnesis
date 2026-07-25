//! Offline readout-coefficient fit (ADR-0010): coordinate search over
//! (w_a, w_phi, w_s, w_z, w_cosine, w_text) maximizing mean
//! novelty-deduped NDCG@20 on a train split of dumped feature rows. It also
//! screens reciprocal-rank fusion of the shipped cognitive rank, embedding
//! rank, and lexical rank as a shadow candidate. Even sample_index = train,
//! odd = dev. Never fit on eval data you intend to report.
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
const RRF_DAMPING: f64 = 60.0;
const RRF_GRID: [f64; 7] = [0.0, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
const UNIT_WEIGHTS: [f64; 6] = [1.0, 1.0, 1.0, 1.0, 0.0, 0.0];
/// Shipped calibration from calibration-records.md (2026-06-11 v2).
const SHIPPED_WEIGHTS: [f64; 6] = [0.25, 16.0, 0.0, 0.0, 0.0, 0.0];

#[derive(Debug, Clone, serde::Deserialize)]
struct Row {
    question_id: String,
    sample_index: usize,
    rank: usize,
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
    embedding_cosine: f64,
    text_score: f64,
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(EPS, 1.0 - EPS);
    (p / (1.0 - p)).ln()
}

fn score(row: &Row, w: [f64; 6]) -> f64 {
    w[0] * logit(row.activation) + w[1] * row.phi + w[2] * logit(row.salience)
        - w[3] * row.impedance
        + w[4] * row.embedding_cosine
        + w[5] * row.text_score
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
fn question_metrics(rows: &[Row], w: [f64; 6], top_k: usize) -> (f64, f64, f64) {
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

fn descending_ranks(rows: &[Row], value: impl Fn(&Row) -> f64) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..rows.len()).collect();
    indices.sort_by(|left, right| {
        value(&rows[*right])
            .total_cmp(&value(&rows[*left]))
            .then_with(|| rows[*left].rank.cmp(&rows[*right].rank))
    });
    let mut ranks = vec![0; rows.len()];
    for (rank, index) in indices.into_iter().enumerate() {
        ranks[index] = rank + 1;
    }
    ranks
}

fn question_rrf_metrics(
    rows: &[Row],
    embedding_weight: f64,
    text_weight: f64,
    top_k: usize,
) -> (f64, f64, f64) {
    let embedding_ranks = descending_ranks(rows, |row| row.embedding_cosine);
    let text_ranks = descending_ranks(rows, |row| row.text_score);
    let mut indices: Vec<usize> = (0..rows.len()).collect();
    indices.sort_by(|left, right| {
        let rrf = |index: usize| {
            let row = &rows[index];
            let cognitive = 1.0 / (RRF_DAMPING + row.rank as f64);
            let embedding = if row.embedding_cosine > 0.0 {
                embedding_weight / (RRF_DAMPING + embedding_ranks[index] as f64)
            } else {
                0.0
            };
            // A zero score means FTS did not return the node. Do not grant an
            // arbitrary tail rank to absent lexical evidence.
            let text = if row.text_score > 0.0 {
                text_weight / (RRF_DAMPING + text_ranks[index] as f64)
            } else {
                0.0
            };
            cognitive + embedding + text
        };
        rrf(*right)
            .total_cmp(&rrf(*left))
            .then_with(|| rows[*left].rank.cmp(&rows[*right].rank))
    });
    let sorted: Vec<&Row> = indices.into_iter().map(|index| &rows[index]).collect();
    let total_relevant = rows.first().map_or(0, |row| row.total_relevant);
    let gains = novelty_gains(&sorted, top_k);
    (
        ndcg_at_k(&gains, total_relevant, top_k),
        mrr(&gains),
        recall_at_k(&gains, total_relevant),
    )
}

fn mean_rrf_metrics(
    questions: &BTreeMap<String, Vec<Row>>,
    embedding_weight: f64,
    text_weight: f64,
    top_k: usize,
) -> (f64, f64, f64) {
    if questions.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let n = questions.len() as f64;
    let mut sum = (0.0, 0.0, 0.0);
    for rows in questions.values() {
        let metrics = question_rrf_metrics(rows, embedding_weight, text_weight, top_k);
        sum.0 += metrics.0;
        sum.1 += metrics.1;
        sum.2 += metrics.2;
    }
    (sum.0 / n, sum.1 / n, sum.2 / n)
}

fn fit_rrf(questions: &BTreeMap<String, Vec<Row>>, top_k: usize) -> (f64, f64, (f64, f64, f64)) {
    let mut best = (0.0, 0.0, mean_rrf_metrics(questions, 0.0, 0.0, top_k));
    for embedding_weight in RRF_GRID {
        for text_weight in RRF_GRID {
            let metrics = mean_rrf_metrics(questions, embedding_weight, text_weight, top_k);
            if metrics.0 > best.2.0 + 1e-12 {
                best = (embedding_weight, text_weight, metrics);
            }
        }
    }
    best
}

/// Primary objective: mean NDCG@k over all questions.
fn mean_ndcg(questions: &BTreeMap<String, Vec<Row>>, w: [f64; 6], top_k: usize) -> f64 {
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
    w: [f64; 6],
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

fn fit(
    train: &BTreeMap<String, Vec<Row>>,
    top_k: usize,
    initial: [f64; 6],
    coordinates: &[usize],
) -> ([f64; 6], f64) {
    let mut best = initial;
    let mut best_ndcg = mean_ndcg(train, best, top_k);
    loop {
        let mut improved = false;
        for &coordinate in coordinates {
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

    let (base_only, _) = fit(&train, top_k, SHIPPED_WEIGHTS, &[0, 1, 2, 3]);
    let (signal_only, _) = fit(&train, top_k, SHIPPED_WEIGHTS, &[4, 5]);
    let (best, _) = fit(&train, top_k, SHIPPED_WEIGHTS, &[0, 1, 2, 3, 4, 5]);

    let (unit_train_ndcg, unit_train_mrr, unit_train_recall) =
        mean_metrics(&train, UNIT_WEIGHTS, top_k);
    let (unit_dev_ndcg, unit_dev_mrr, unit_dev_recall) = mean_metrics(&dev, UNIT_WEIGHTS, top_k);
    let (shipped_train_ndcg, shipped_train_mrr, shipped_train_recall) =
        mean_metrics(&train, SHIPPED_WEIGHTS, top_k);
    let (shipped_dev_ndcg, shipped_dev_mrr, shipped_dev_recall) =
        mean_metrics(&dev, SHIPPED_WEIGHTS, top_k);
    let (base_train_ndcg, base_train_mrr, base_train_recall) =
        mean_metrics(&train, base_only, top_k);
    let (base_dev_ndcg, base_dev_mrr, base_dev_recall) = mean_metrics(&dev, base_only, top_k);
    let (signal_train_ndcg, signal_train_mrr, signal_train_recall) =
        mean_metrics(&train, signal_only, top_k);
    let (signal_dev_ndcg, signal_dev_mrr, signal_dev_recall) =
        mean_metrics(&dev, signal_only, top_k);
    let (train_ndcg, train_mrr, train_recall) = mean_metrics(&train, best, top_k);
    let (dev_ndcg, dev_mrr, dev_recall) = mean_metrics(&dev, best, top_k);
    let (rrf_embedding_weight, rrf_text_weight, rrf_train) = fit_rrf(&train, top_k);
    let rrf_dev = mean_rrf_metrics(&dev, rrf_embedding_weight, rrf_text_weight, top_k);

    println!(
        "{}",
        serde_json::json!({
            "w_a": best[0],
            "w_phi": best[1],
            "w_s": best[2],
            "w_z": best[3],
            "w_cosine": best[4],
            "w_text": best[5],
            "top_k": top_k,
            "unit": {
                "train_ndcg": unit_train_ndcg,
                "train_mrr": unit_train_mrr,
                "train_recall_at_k": unit_train_recall,
                "dev_ndcg": unit_dev_ndcg,
                "dev_mrr": unit_dev_mrr,
                "dev_recall_at_k": unit_dev_recall,
            },
            "shipped": {
                "w_a": SHIPPED_WEIGHTS[0],
                "w_phi": SHIPPED_WEIGHTS[1],
                "w_s": SHIPPED_WEIGHTS[2],
                "w_z": SHIPPED_WEIGHTS[3],
                "w_cosine": SHIPPED_WEIGHTS[4],
                "w_text": SHIPPED_WEIGHTS[5],
                "train_ndcg": shipped_train_ndcg,
                "train_mrr": shipped_train_mrr,
                "train_recall_at_k": shipped_train_recall,
                "dev_ndcg": shipped_dev_ndcg,
                "dev_mrr": shipped_dev_mrr,
                "dev_recall_at_k": shipped_dev_recall,
            },
            "base_only_fit": {
                "weights": base_only,
                "train_ndcg": base_train_ndcg,
                "train_mrr": base_train_mrr,
                "train_recall_at_k": base_train_recall,
                "dev_ndcg": base_dev_ndcg,
                "dev_mrr": base_dev_mrr,
                "dev_recall_at_k": base_dev_recall,
            },
            "signal_only_fit": {
                "weights": signal_only,
                "train_ndcg": signal_train_ndcg,
                "train_mrr": signal_train_mrr,
                "train_recall_at_k": signal_train_recall,
                "dev_ndcg": signal_dev_ndcg,
                "dev_mrr": signal_dev_mrr,
                "dev_recall_at_k": signal_dev_recall,
            },
            "fitted": {
                "train_ndcg": train_ndcg,
                "train_mrr": train_mrr,
                "train_recall_at_k": train_recall,
                "dev_ndcg": dev_ndcg,
                "dev_mrr": dev_mrr,
                "dev_recall_at_k": dev_recall,
            },
            "shadow_rank_fusion": {
                "damping": RRF_DAMPING,
                "embedding_weight": rrf_embedding_weight,
                "text_weight": rrf_text_weight,
                "train_ndcg": rrf_train.0,
                "train_mrr": rrf_train.1,
                "train_recall_at_k": rrf_train.2,
                "dev_ndcg": rrf_dev.0,
                "dev_mrr": rrf_dev.1,
                "dev_recall_at_k": rrf_dev.2,
                "lexical_absence_contribution": 0.0,
            },
        })
    );
}
