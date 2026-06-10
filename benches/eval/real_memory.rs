#[path = "../eval_common/mod.rs"]
mod eval_common;
mod real_memory_cli;

use std::process;

use anamnesis::EmbeddingProvider;
use eval_common::real_bench::dataset::{
    BenchDatasetName, load_benchmark_dataset, restrict_to_questions, split_by_sample,
};
use eval_common::real_bench::graph::{
    GraphBuildStats, build_memory_graph, evaluate_questions, run_warmup,
};
use eval_common::real_bench::graph::{QuestionEvaluation, WarmupReport};
use eval_common::real_bench::report::{
    ReportInput, build_report, prepare_report_output, write_prepared_report,
};
use eval_common::real_bench::{BenchError, BenchResult};
use real_memory_cli::{parse_args, print_usage};

#[cfg(not(feature = "embed"))]
compile_error!("real_memory requires: cargo bench --features embed --bench real_memory");

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> BenchResult<()> {
    let Some(args) = parse_args(std::env::args().skip(1))? else {
        print_usage();
        return Ok(());
    };
    let output = prepare_report_output(&args.output, args.force)?;

    eprintln!("LOAD {}", args.dataset.as_str());
    let loader_limit = (args.dataset == BenchDatasetName::LongMemEval)
        .then_some(args.samples)
        .flatten();
    let mut loaded = load_benchmark_dataset(args.dataset, &args.data_dir, loader_limit)?;
    if args.skip_adversarial {
        let before = loaded.questions.len();
        loaded
            .questions
            .retain(|question| question.question_type != "adversarial");
        eprintln!(
            "FILTER adversarial: {} -> {} questions",
            before,
            loaded.questions.len()
        );
    }
    let loaded = restrict_to_questions(loaded, args.samples);
    if loaded.questions.is_empty() {
        return Err(BenchError::InvalidInput(
            "selected dataset contains no questions".to_string(),
        ));
    }

    eprintln!("EMBED init FastEmbed");
    if !args.allow_download {
        return Err(BenchError::InvalidInput(
            "FastEmbed may download model weights on first use; pass --allow-download to run"
                .to_string(),
        ));
    }
    let provider = make_provider()?;
    eprintln!(
        "EMBED model={} dimensions={}",
        provider.model_name(),
        provider.dimensions()
    );

    // One memory store per sample (LoCoMo conversation / LongMemEval haystack),
    // matching the standard per-conversation evaluation protocol. Warmup commits
    // the first N questions of each sample against that sample's graph.
    let groups = split_by_sample(loaded);
    if let Some(too_small) = groups
        .iter()
        .find(|group| args.warmup >= group.questions.len())
    {
        return Err(BenchError::InvalidInput(format!(
            "--warmup ({}) must be smaller than every sample's question count \
             (sample {} has {})",
            args.warmup,
            too_small.questions[0].sample_index,
            too_small.questions.len()
        )));
    }
    eprintln!(
        "GRAPH samples={} questions={}",
        groups.len(),
        groups.iter().map(|g| g.questions.len()).sum::<usize>()
    );

    let mut node_count = 0usize;
    let mut edge_count = 0usize;
    let mut graph_stats = GraphBuildStats::default();
    let mut warmup_total = WarmupReport::default();
    let mut evaluations: Vec<QuestionEvaluation> = Vec::new();

    for group in &groups {
        let sample_index = group.questions[0].sample_index;
        let mut graph = build_memory_graph(group, &provider)?;
        let (warmup_questions, eval_questions) = group.questions.split_at(args.warmup);
        let warmup = run_warmup(&mut graph, warmup_questions, &provider, args.top_k, args.seed_limit)?;
        let sample_evals = evaluate_questions(&graph, eval_questions, &provider, args.top_k, args.seed_limit)?;
        eprintln!(
            "SAMPLE {} sessions={} warmup={} eval={}",
            sample_index,
            group.sessions.len(),
            warmup_questions.len(),
            sample_evals.len()
        );

        node_count += graph.engine.graph().node_count();
        edge_count += graph.engine.graph().edge_count();
        graph_stats.nodes_created += graph.stats.nodes_created;
        graph_stats.temporal_edges_created += graph.stats.temporal_edges_created;
        graph_stats.extracted_edges_created += graph.stats.extracted_edges_created;
        graph_stats.embedded_texts += graph.stats.embedded_texts;
        warmup_total.questions += warmup.questions;
        warmup_total.sites_accessed += warmup.sites_accessed;
        warmup_total.paths_strengthened += warmup.paths_strengthened;
        evaluations.extend(sample_evals);
    }

    let report = build_report(ReportInput {
        dataset: args.dataset,
        embedding_model: provider.model_name().to_string(),
        embedding_dimensions: provider.dimensions(),
        sample_limit: args.samples,
        top_k: args.top_k,
        node_count,
        edge_count,
        graph_stats,
        warmup: warmup_total,
        questions: evaluations,
    });
    write_prepared_report(&report, &output, args.force)?;

    eprintln!(
        "REPORT {} questions={} recall@{}={:.4} mrr={:.4}",
        output.path().display(),
        report.evaluated_questions,
        args.top_k,
        report.retrieval_metrics.recall_at_k,
        report.retrieval_metrics.mrr
    );
    Ok(())
}

#[cfg(feature = "embed")]
fn make_provider() -> BenchResult<anamnesis::FastEmbedProvider> {
    anamnesis::FastEmbedProvider::new()
        .map_err(|err| BenchError::Embedding(format!("FastEmbed init failed: {err}")))
}
