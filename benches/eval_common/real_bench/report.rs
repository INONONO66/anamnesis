use serde::{Deserialize, Serialize};

use super::dataset::BenchDatasetName;
use super::graph::{GraphBuildStats, QuestionEvaluation, WarmupReport};
use super::metrics::RetrievalMetrics;

mod output;

pub use output::{prepare_report_output, write_prepared_report, write_report};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealBenchReport {
    pub dataset: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub sample_limit: Option<usize>,
    pub top_k: usize,
    pub warmup_questions: usize,
    pub evaluated_questions: usize,
    pub graph: ReportGraphStats,
    pub warmup: WarmupReport,
    pub retrieval_metrics: RetrievalMetrics,
    pub package_metrics: RetrievalMetrics,
    pub diagnostics: DiagnosticsReport,
    pub latency_ms: LatencyReport,
    pub questions: Vec<QuestionEvaluation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportGraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub nodes_created: usize,
    pub temporal_edges_created: usize,
    pub extracted_edges_created: usize,
    pub embedded_texts: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LatencyReport {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    pub hit_at_1: f64,
    pub hit_at_3: f64,
    pub hit_at_5: f64,
    pub hit_at_10: f64,
    pub hit_at_20: f64,
    pub mean_first_hit_rank: f64,
    pub avg_returned_fragments: f64,
    pub per_type: std::collections::BTreeMap<String, TypeBreakdown>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeBreakdown {
    pub questions: usize,
    pub recall_at_k: f64,
    pub mrr: f64,
}

pub struct ReportInput {
    pub dataset: BenchDatasetName,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub sample_limit: Option<usize>,
    pub top_k: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub graph_stats: GraphBuildStats,
    pub warmup: WarmupReport,
    pub questions: Vec<QuestionEvaluation>,
}

pub fn build_report(input: ReportInput) -> RealBenchReport {
    let retrieval_metrics = average_metrics(&input.questions, |q| &q.retrieval_metrics);
    let package_metrics = average_metrics(&input.questions, |q| &q.package_metrics);
    let diagnostics = diagnostics(&input.questions);
    let latency = latency_report(&input.questions);
    RealBenchReport {
        dataset: input.dataset.as_str().to_string(),
        embedding_model: input.embedding_model,
        embedding_dimensions: input.embedding_dimensions,
        sample_limit: input.sample_limit,
        top_k: input.top_k,
        warmup_questions: input.warmup.questions,
        evaluated_questions: input.questions.len(),
        graph: ReportGraphStats {
            node_count: input.node_count,
            edge_count: input.edge_count,
            nodes_created: input.graph_stats.nodes_created,
            temporal_edges_created: input.graph_stats.temporal_edges_created,
            extracted_edges_created: input.graph_stats.extracted_edges_created,
            embedded_texts: input.graph_stats.embedded_texts,
        },
        warmup: input.warmup,
        retrieval_metrics,
        package_metrics,
        diagnostics,
        latency_ms: latency,
        questions: input.questions,
    }
}

fn average_metrics(
    questions: &[QuestionEvaluation],
    accessor: impl Fn(&QuestionEvaluation) -> &RetrievalMetrics,
) -> RetrievalMetrics {
    if questions.is_empty() {
        return RetrievalMetrics::default();
    }
    let denom = questions.len() as f64;
    let mut sum = RetrievalMetrics::default();
    for question in questions {
        let m = accessor(question);
        sum.precision_at_k += m.precision_at_k;
        sum.recall_at_k += m.recall_at_k;
        sum.mrr += m.mrr;
        sum.ndcg_at_k += m.ndcg_at_k;
    }
    RetrievalMetrics {
        precision_at_k: sum.precision_at_k / denom,
        recall_at_k: sum.recall_at_k / denom,
        mrr: sum.mrr / denom,
        ndcg_at_k: sum.ndcg_at_k / denom,
    }
}

fn diagnostics(questions: &[QuestionEvaluation]) -> DiagnosticsReport {
    let total = questions.len().max(1) as f64;
    let hit_at = |k: usize| {
        questions
            .iter()
            .filter(|q| q.first_hit_rank.is_some_and(|r| r <= k))
            .count() as f64
            / total
    };
    let ranks: Vec<f64> = questions
        .iter()
        .filter_map(|q| q.first_hit_rank.map(|r| r as f64))
        .collect();
    let mut per_type: std::collections::BTreeMap<String, TypeBreakdown> = Default::default();
    for q in questions {
        let entry = per_type.entry(q.question_type.clone()).or_default();
        entry.questions += 1;
        entry.recall_at_k += q.retrieval_metrics.recall_at_k;
        entry.mrr += q.retrieval_metrics.mrr;
    }
    for entry in per_type.values_mut() {
        let n = entry.questions.max(1) as f64;
        entry.recall_at_k /= n;
        entry.mrr /= n;
    }
    DiagnosticsReport {
        hit_at_1: hit_at(1),
        hit_at_3: hit_at(3),
        hit_at_5: hit_at(5),
        hit_at_10: hit_at(10),
        hit_at_20: hit_at(20),
        mean_first_hit_rank: if ranks.is_empty() {
            0.0
        } else {
            ranks.iter().sum::<f64>() / ranks.len() as f64
        },
        avg_returned_fragments: questions
            .iter()
            .map(|q| q.returned_fragments as f64)
            .sum::<f64>()
            / total,
        per_type,
    }
}

fn latency_report(questions: &[QuestionEvaluation]) -> LatencyReport {
    let mut values: Vec<_> = questions
        .iter()
        .map(|question| question.search_latency_ms)
        .filter(|value| value.is_finite())
        .collect();
    values.sort_by(f64::total_cmp);
    LatencyReport {
        p50: percentile(&values, 50),
        p95: percentile(&values, 95),
        p99: percentile(&values, 99),
    }
}

fn percentile(values: &[f64], percentile: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values[index.min(values.len() - 1)]
}
