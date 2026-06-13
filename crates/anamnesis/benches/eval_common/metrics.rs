use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::checkpoint::QuestionResult;
use super::datasets::UnifiedQuestion;

pub fn precision_at_k(relevant: &[bool], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }

    let hits = relevant
        .iter()
        .take(k)
        .filter(|is_relevant| **is_relevant)
        .count();
    hits as f64 / k as f64
}

pub fn recall_at_k(relevant: &[bool], total_relevant: usize, k: usize) -> f64 {
    if total_relevant == 0 || k == 0 {
        return 0.0;
    }

    let hits = relevant
        .iter()
        .take(k)
        .filter(|is_relevant| **is_relevant)
        .count();
    hits as f64 / total_relevant as f64
}

pub fn mrr(relevant: &[bool]) -> f64 {
    relevant
        .iter()
        .position(|is_relevant| *is_relevant)
        .map_or(0.0, |index| 1.0 / (index + 1) as f64)
}

pub fn ndcg_at_k(relevant: &[bool], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }

    let dcg = dcg_at_k(relevant, k);
    let ideal_relevant = relevant
        .iter()
        .filter(|is_relevant| **is_relevant)
        .count()
        .min(k);
    if ideal_relevant == 0 {
        return 0.0;
    }

    let ideal = vec![true; ideal_relevant];
    let idcg = dcg_at_k(&ideal, ideal_relevant);
    dcg / idcg
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub accuracy: f64,
    pub accuracy_by_type: HashMap<String, f64>,
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub ndcg: f64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub mem_score: String,
}

pub fn mem_score(accuracy: f64, latency_ms: f64, context_tokens: usize) -> String {
    format!("{accuracy:.1}% / {latency_ms:.0}ms / {context_tokens}tok")
}

#[derive(Debug)]
pub enum MetricsError {
    IoError(String),
    SerdeError(String),
}

impl fmt::Display for MetricsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetricsError::IoError(message) => write!(f, "I/O error: {message}"),
            MetricsError::SerdeError(message) => write!(f, "serialization error: {message}"),
        }
    }
}

impl Error for MetricsError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonSummary {
    pub accuracy_delta: f64,
    pub accuracy_by_type_delta: HashMap<String, f64>,
    pub precision_at_k_delta: f64,
    pub recall_at_k_delta: f64,
    pub mrr_delta: f64,
    pub ndcg_delta: f64,
    pub latency_p50_ms_delta: f64,
    pub latency_p95_ms_delta: f64,
    pub latency_p99_ms_delta: f64,
}

pub fn compute_report(
    results: &HashMap<String, QuestionResult>,
    questions: &[UnifiedQuestion],
) -> EvalReport {
    let mut correct_count = 0usize;
    let mut type_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut latencies = Vec::new();
    let mut token_total = 0usize;
    let mut token_count = 0usize;

    for question in questions {
        let result = results.get(&question.question_id);
        let is_correct = result
            .and_then(|question_result| question_result.judge_result.as_ref())
            .is_some_and(|judge_result| judge_result.correct);

        if is_correct {
            correct_count += 1;
        }

        let counts = type_counts
            .entry(question.question_type.clone())
            .or_insert((0, 0));
        counts.0 += 1;
        if is_correct {
            counts.1 += 1;
        }

        if let Some(question_result) = result {
            if let Some(latency) = question_result.search_latency_ms {
                latencies.push(latency);
            }
            if let Some(tokens) = question_result.context_tokens {
                token_total += tokens;
                token_count += 1;
            }
        }
    }

    latencies.sort_by(f64::total_cmp);

    let total_questions = questions.len();
    let accuracy = percentage(correct_count, total_questions);
    let avg_latency_ms = average_f64(&latencies);
    let avg_tokens = token_total.checked_div(token_count).unwrap_or(0);

    let accuracy_by_type = type_counts
        .into_iter()
        .map(|(question_type, (total, correct))| (question_type, percentage(correct, total)))
        .collect();

    EvalReport {
        accuracy,
        accuracy_by_type,
        precision_at_k: 0.0,
        recall_at_k: 0.0,
        mrr: 0.0,
        ndcg: 0.0,
        latency_p50_ms: percentile(&latencies, 50),
        latency_p95_ms: percentile(&latencies, 95),
        latency_p99_ms: percentile(&latencies, 99),
        mem_score: mem_score(accuracy * 100.0, avg_latency_ms, avg_tokens),
    }
}

pub fn write_json_report(report: &EvalReport, path: &Path) -> Result<(), MetricsError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|err| MetricsError::IoError(err.to_string()))?;
    }

    let json = serde_json::to_string_pretty(report)
        .map_err(|err| MetricsError::SerdeError(err.to_string()))?;
    let temp_path = tmp_path_for(path);

    std::fs::write(&temp_path, json).map_err(|err| MetricsError::IoError(err.to_string()))?;
    std::fs::rename(&temp_path, path).map_err(|err| {
        let _ = std::fs::remove_file(&temp_path);
        MetricsError::IoError(err.to_string())
    })?;

    Ok(())
}

pub fn print_summary(report: &EvalReport) {
    eprintln!("{:<24} {:>12}", "Metric", "Value");
    eprintln!("{:-<24} {:-<12}", "", "");
    eprintln!("{:<24} {:>11.2}%", "Accuracy", report.accuracy * 100.0);
    eprintln!("{:<24} {:>12.4}", "Precision@K", report.precision_at_k);
    eprintln!("{:<24} {:>12.4}", "Recall@K", report.recall_at_k);
    eprintln!("{:<24} {:>12.4}", "MRR", report.mrr);
    eprintln!("{:<24} {:>12.4}", "NDCG", report.ndcg);
    eprintln!("{:<24} {:>11.2}", "Latency P50 ms", report.latency_p50_ms);
    eprintln!("{:<24} {:>11.2}", "Latency P95 ms", report.latency_p95_ms);
    eprintln!("{:<24} {:>11.2}", "Latency P99 ms", report.latency_p99_ms);
    eprintln!("{:<24} {:>12}", "MemScore", report.mem_score);

    if !report.accuracy_by_type.is_empty() {
        eprintln!();
        eprintln!("{:<24} {:>12}", "Question Type", "Accuracy");
        eprintln!("{:-<24} {:-<12}", "", "");
        let mut type_rows: Vec<_> = report.accuracy_by_type.iter().collect();
        type_rows.sort_by_key(|(left, _)| *left);
        for (question_type, accuracy) in type_rows {
            eprintln!("{:<24} {:>11.2}%", question_type, accuracy * 100.0);
        }
    }
}

pub fn compare_reports(baseline: &EvalReport, current: &EvalReport) -> ComparisonSummary {
    let mut accuracy_by_type_delta = HashMap::new();
    for question_type in baseline.accuracy_by_type.keys() {
        let baseline_accuracy = baseline
            .accuracy_by_type
            .get(question_type)
            .copied()
            .unwrap_or_default();
        let current_accuracy = current
            .accuracy_by_type
            .get(question_type)
            .copied()
            .unwrap_or_default();
        accuracy_by_type_delta.insert(question_type.clone(), current_accuracy - baseline_accuracy);
    }
    for question_type in current.accuracy_by_type.keys() {
        if !accuracy_by_type_delta.contains_key(question_type) {
            let current_accuracy = current
                .accuracy_by_type
                .get(question_type)
                .copied()
                .unwrap_or_default();
            accuracy_by_type_delta.insert(question_type.clone(), current_accuracy);
        }
    }

    ComparisonSummary {
        accuracy_delta: current.accuracy - baseline.accuracy,
        accuracy_by_type_delta,
        precision_at_k_delta: current.precision_at_k - baseline.precision_at_k,
        recall_at_k_delta: current.recall_at_k - baseline.recall_at_k,
        mrr_delta: current.mrr - baseline.mrr,
        ndcg_delta: current.ndcg - baseline.ndcg,
        latency_p50_ms_delta: current.latency_p50_ms - baseline.latency_p50_ms,
        latency_p95_ms_delta: current.latency_p95_ms - baseline.latency_p95_ms,
        latency_p99_ms_delta: current.latency_p99_ms - baseline.latency_p99_ms,
    }
}

fn dcg_at_k(relevant: &[bool], k: usize) -> f64 {
    relevant
        .iter()
        .take(k)
        .enumerate()
        .filter_map(|(index, is_relevant)| {
            if *is_relevant {
                let rank = index + 1;
                Some(1.0 / ((rank + 1) as f64).log2())
            } else {
                None
            }
        })
        .sum()
}

fn percentage(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn average_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile(sorted_values: &[f64], pct: usize) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }

    let index = sorted_values.len() * pct / 100;
    sorted_values
        .get(index)
        .copied()
        .unwrap_or_else(|| sorted_values[sorted_values.len() - 1])
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut temp_path = PathBuf::from(path);
    let mut file_name = path.file_name().map_or_else(
        || std::ffi::OsString::from("report"),
        |name| name.to_os_string(),
    );
    file_name.push(".tmp");
    temp_path.set_file_name(file_name);
    temp_path
}

#[cfg(test)]
mod tests {
    use super::super::judge::JudgeResult;
    use super::*;

    #[test]
    fn eval_common_precision_at_k_counts_hits_in_top_k() {
        let relevant = [true, false, true, true];
        assert_eq!(precision_at_k(&relevant, 2), 0.5);
        assert_eq!(precision_at_k(&relevant, 4), 0.75);
        assert_eq!(precision_at_k(&relevant, 0), 0.0);
    }

    #[test]
    fn eval_common_recall_at_k_counts_hits_against_total_relevant() {
        let relevant = [true, false, true, true];
        assert_eq!(recall_at_k(&relevant, 3, 2), 1.0 / 3.0);
        assert_eq!(recall_at_k(&relevant, 3, 4), 1.0);
        assert_eq!(recall_at_k(&relevant, 0, 4), 0.0);
    }

    #[test]
    fn eval_common_mrr_returns_reciprocal_first_relevant_rank() {
        let relevant = [false, false, true, true];
        assert_eq!(mrr(&relevant), 1.0 / 3.0);
        assert_eq!(mrr(&[false, false]), 0.0);
    }

    #[test]
    fn eval_common_ndcg_at_k_uses_discounted_gain() {
        let relevant = [true, false, true];
        let expected = (1.0 + 1.0 / 4.0_f64.log2()) / (1.0 + 1.0 / 3.0_f64.log2());
        assert!((ndcg_at_k(&relevant, 3) - expected).abs() < 1e-12);
        assert_eq!(ndcg_at_k(&[false, false], 2), 0.0);
    }

    #[test]
    fn eval_common_mem_score_formats_summary() {
        assert_eq!(mem_score(87.45, 123.6, 4096), "87.5% / 124ms / 4096tok");
    }

    #[test]
    fn eval_common_compute_report_groups_accuracy_and_latency_percentiles() {
        let questions: Vec<_> = (0..10)
            .map(|index| UnifiedQuestion {
                question_id: format!("q{index}"),
                question: format!("question {index}"),
                expected_answer: format!("answer {index}"),
                question_type: if index < 5 {
                    "single-hop".to_string()
                } else {
                    "multi-hop".to_string()
                },
                session_ids: vec!["session-1".to_string()],
            })
            .collect();

        let mut results = HashMap::new();
        for index in 0..10 {
            let judge_result = if index < 7 {
                JudgeResult::correct("ok")
            } else {
                JudgeResult::incorrect("no")
            };
            results.insert(
                format!("q{index}"),
                QuestionResult {
                    answer: Some(format!("answer {index}")),
                    judge_result: Some(judge_result),
                    search_latency_ms: Some((index + 1) as f64 * 10.0),
                    context_tokens: Some(100 + index * 10),
                },
            );
        }

        let report = compute_report(&results, &questions);

        assert_eq!(report.accuracy, 0.7);
        assert_eq!(report.accuracy_by_type.get("single-hop"), Some(&1.0));
        assert_eq!(report.accuracy_by_type.get("multi-hop"), Some(&0.4));
        assert_eq!(report.latency_p50_ms, 60.0);
        assert_eq!(report.latency_p95_ms, 100.0);
        assert_eq!(report.latency_p99_ms, 100.0);
        assert_eq!(report.mem_score, "70.0% / 55ms / 145tok");
        assert_eq!(report.precision_at_k, 0.0);
        assert_eq!(report.recall_at_k, 0.0);
        assert_eq!(report.mrr, 0.0);
        assert_eq!(report.ndcg, 0.0);
    }
}
