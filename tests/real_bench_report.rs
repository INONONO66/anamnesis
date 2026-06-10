#[path = "../benches/eval_common/mod.rs"]
mod eval_common;

use std::path::{Path, PathBuf};

use eval_common::real_bench::dataset::BenchDatasetName;
use eval_common::real_bench::graph::{GraphBuildStats, QuestionEvaluation, WarmupReport};
use eval_common::real_bench::metrics::RetrievalMetrics;
use eval_common::real_bench::report::{
    DiagnosticsReport, LatencyReport, RealBenchReport, ReportGraphStats, ReportInput, build_report,
    write_report,
};

#[test]
fn write_report_refuses_existing_output_without_force() {
    let path = test_path("no-clobber/report.json");
    cleanup_root(&path);
    let report = sample_report("first");

    write_report(&report, &path, false).expect("first write succeeds");
    let err = write_report(&sample_report("second"), &path, false)
        .expect_err("second write without force should fail");

    assert!(err.to_string().contains("already exists"));
    let text = std::fs::read_to_string(&path).expect("read report");
    assert!(text.contains("first"));
    cleanup_root(&path);
}

#[test]
fn write_report_force_overwrites_and_repeated_writes_do_not_collide() {
    let path = test_path("force/report.json");
    cleanup_root(&path);

    write_report(&sample_report("first"), &path, false).expect("first write succeeds");
    write_report(&sample_report("second"), &path, true).expect("force overwrite succeeds");
    write_report(&sample_report("third"), &path, true).expect("second force write succeeds");

    let text = std::fs::read_to_string(&path).expect("read report");
    assert!(text.contains("third"));
    cleanup_root(&path);
}

#[test]
fn write_report_rejects_absolute_output_path() {
    let err = write_report(
        &sample_report("absolute"),
        Path::new("/tmp/anamnesis-real-bench-report.json"),
        true,
    )
    .expect_err("absolute path should fail");

    assert!(err.to_string().contains("output path must be relative"));
}

#[cfg(unix)]
#[test]
fn write_report_rejects_symlinked_allowed_parent() {
    let outside = std::env::temp_dir().join(format!(
        "anamnesis-real-bench-outside-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&outside).expect("create outside dir");
    let link = PathBuf::from(format!(
        ".omo/evidence/real-bench-report-link-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&outside, &link).expect("create symlink");

    let err = write_report(&sample_report("symlink"), &link.join("report.json"), true)
        .expect_err("symlink parent should fail");

    assert!(err.to_string().contains("symlink component"));
    std::fs::remove_file(&link).expect("remove symlink");
    std::fs::remove_dir_all(outside).expect("remove outside dir");
}

#[test]
fn diagnostics_hit_at_k_and_mean_first_hit_rank() {
    // Two questions: first has first_hit_rank=Some(1), second has None.
    let questions = vec![
        question_eval("q1", "single-session-user", Some(1)),
        question_eval("q2", "single-session-user", None),
    ];
    let input = ReportInput {
        dataset: BenchDatasetName::Locomo,
        embedding_model: "test".to_string(),
        embedding_dimensions: 4,
        sample_limit: None,
        top_k: 5,
        node_count: 0,
        edge_count: 0,
        graph_stats: GraphBuildStats::default(),
        warmup: WarmupReport::default(),
        questions,
    };
    let report = build_report(input);

    assert_eq!(report.diagnostics.hit_at_1, 0.5, "1 of 2 hit at rank 1");
    assert_eq!(report.diagnostics.hit_at_3, 0.5, "same — only rank-1 hit");
    assert_eq!(report.diagnostics.mean_first_hit_rank, 1.0, "only one rank, it is 1");
    assert_eq!(report.diagnostics.avg_returned_fragments, 0.0);
}

fn question_eval(id: &str, qtype: &str, first_hit: Option<usize>) -> QuestionEvaluation {
    // Build a minimal QuestionEvaluation with the given first_hit_rank.
    // We construct retrieval_metrics consistent with whether there was a hit.
    let mrr = first_hit.map_or(0.0, |r| 1.0 / r as f64);
    QuestionEvaluation {
        question_id: id.to_string(),
        question_type: qtype.to_string(),
        sample_index: 0,
        search_latency_ms: 1.0,
        total_relevant: 1,
        retrieval_metrics: RetrievalMetrics {
            precision_at_k: if first_hit.is_some() { 1.0 } else { 0.0 },
            recall_at_k: if first_hit.is_some() { 1.0 } else { 0.0 },
            mrr,
            ndcg_at_k: if first_hit.is_some() { 1.0 } else { 0.0 },
        },
        package_metrics: RetrievalMetrics::default(),
        first_hit_rank: first_hit,
        returned_fragments: 0,
        retrievals: Vec::new(),
    }
}

fn sample_report(model: &str) -> RealBenchReport {
    RealBenchReport {
        dataset: "locomo".to_string(),
        embedding_model: model.to_string(),
        embedding_dimensions: 4,
        sample_limit: Some(1),
        top_k: 1,
        warmup_questions: 0,
        evaluated_questions: 0,
        graph: ReportGraphStats {
            node_count: 0,
            edge_count: 0,
            nodes_created: 0,
            temporal_edges_created: 0,
            extracted_edges_created: 0,
            embedded_texts: 0,
        },
        warmup: WarmupReport::default(),
        retrieval_metrics: Default::default(),
        package_metrics: Default::default(),
        diagnostics: DiagnosticsReport::default(),
        latency_ms: LatencyReport::default(),
        questions: Vec::new(),
    }
}

fn test_path(suffix: &str) -> PathBuf {
    let safe_suffix = suffix.replace('/', "-");
    PathBuf::from(format!(
        ".omo/evidence/real-bench-report-test-{}-{}/{}",
        std::process::id(),
        safe_suffix,
        suffix
    ))
}

fn cleanup_root(path: &Path) {
    let Some(root) = path.ancestors().nth(2) else {
        return;
    };
    let _ = std::fs::remove_dir_all(root);
}
