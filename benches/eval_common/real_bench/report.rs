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
    let metrics = average_metrics(&input.questions);
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
        retrieval_metrics: metrics,
        latency_ms: latency,
        questions: input.questions,
    }
}

fn average_metrics(questions: &[QuestionEvaluation]) -> RetrievalMetrics {
    if questions.is_empty() {
        return RetrievalMetrics::default();
    }
    let denom = questions.len() as f64;
    let mut sum = RetrievalMetrics::default();
    for question in questions {
        sum.precision_at_k += question.retrieval_metrics.precision_at_k;
        sum.recall_at_k += question.retrieval_metrics.recall_at_k;
        sum.mrr += question.retrieval_metrics.mrr;
        sum.ndcg_at_k += question.retrieval_metrics.ndcg_at_k;
    }
    RetrievalMetrics {
        precision_at_k: sum.precision_at_k / denom,
        recall_at_k: sum.recall_at_k / denom,
        mrr: sum.mrr / denom,
        ndcg_at_k: sum.ndcg_at_k / denom,
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
