//! Offline readout-coefficient fit (ADR-0010): coordinate search over
//! (w_a, w_phi, w_s, w_z) maximizing mean MRR on a train split of dumped
//! feature rows. Even sample_index = train, odd = dev. Never fit on eval data
//! you intend to report.
//!
//! Objective caveat: this optimizes per-node label MRR (no gold-unit novelty
//! dedup), a close proxy for — but not identical to — the benchmark report's
//! novelty-deduped MRR surface.
//!
//! Usage: cargo bench --bench fit_readout -- <features.jsonl> [top_k]

use std::collections::BTreeMap;
use std::io::BufRead;

const EPS: f64 = 1e-6;
const GRID: [f64; 7] = [0.0, 0.25, 0.5, 1.0, 1.5, 2.0, 4.0];

#[derive(Debug, Clone, serde::Deserialize)]
struct Row {
    question_id: String,
    sample_index: usize,
    label: bool,
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

fn mean_mrr(questions: &BTreeMap<String, Vec<Row>>, w: [f64; 4], top_k: usize) -> f64 {
    if questions.is_empty() {
        return 0.0;
    }
    let total: f64 = questions
        .values()
        .map(|rows| {
            let mut scored: Vec<(f64, bool)> =
                rows.iter().map(|r| (score(r, w), r.label)).collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            scored
                .iter()
                .take(top_k)
                .position(|(_, label)| *label)
                .map_or(0.0, |index| 1.0 / (index + 1) as f64)
        })
        .sum();
    total / questions.len() as f64
}

fn fit(train: &BTreeMap<String, Vec<Row>>, top_k: usize) -> ([f64; 4], f64) {
    let mut best = [1.0, 1.0, 1.0, 1.0];
    let mut best_mrr = mean_mrr(train, best, top_k);
    loop {
        let mut improved = false;
        for coordinate in 0..4 {
            for &value in &GRID {
                let mut candidate = best;
                candidate[coordinate] = value;
                let mrr = mean_mrr(train, candidate, top_k);
                if mrr > best_mrr + 1e-9 {
                    best = candidate;
                    best_mrr = mrr;
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
    (best, best_mrr)
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
        let bucket = if row.sample_index % 2 == 0 {
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
    let (best, best_mrr) = fit(&train, top_k);
    println!(
        "{}",
        serde_json::json!({
            "w_a": best[0], "w_phi": best[1], "w_s": best[2], "w_z": best[3],
            "train_mrr": best_mrr,
            "dev_mrr": mean_mrr(&dev, best, top_k),
            "unit_train_mrr": mean_mrr(&train, unit, top_k),
            "unit_dev_mrr": mean_mrr(&dev, unit, top_k),
            "top_k": top_k,
        })
    );
}
