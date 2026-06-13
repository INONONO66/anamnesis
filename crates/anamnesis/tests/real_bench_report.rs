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
    // Four questions across two types:
    //   "single-session-user": q1 hits at rank 1, q2 misses entirely
    //   "temporal": q3 hits at rank 3, q4 misses entirely
    let questions = vec![
        question_eval("q1", "single-session-user", Some(1)),
        question_eval("q2", "single-session-user", None),
        question_eval("q3", "temporal", Some(3)),
        question_eval("q4", "temporal", None),
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

    // hit_at_1: only q1 hits at rank <=1 → 1/4
    assert_eq!(report.diagnostics.hit_at_1, 0.25, "1 of 4 hit at rank 1");
    // hit_at_3: q1 (rank 1) and q3 (rank 3) → 2/4
    assert_eq!(report.diagnostics.hit_at_3, 0.5, "2 of 4 hit at rank <=3");
    // hit_at_5: same two hits at rank <=5 → 2/4
    assert_eq!(report.diagnostics.hit_at_5, 0.5, "2 of 4 hit at rank <=5");
    // hit_at_10 and hit_at_20: same two hits → 2/4
    assert_eq!(report.diagnostics.hit_at_10, 0.5, "2 of 4 hit at rank <=10");
    assert_eq!(report.diagnostics.hit_at_20, 0.5, "2 of 4 hit at rank <=20");
    // mean_first_hit_rank: only ranks 1 and 3 are present → (1 + 3) / 2 = 2.0
    assert_eq!(
        report.diagnostics.mean_first_hit_rank, 2.0,
        "mean of ranks 1 and 3"
    );
    assert_eq!(report.diagnostics.avg_returned_fragments, 0.0);

    // Pin per-type averaging math for "single-session-user":
    //   q1: recall_at_k=1.0, mrr=1.0   q2: recall_at_k=0.0, mrr=0.0
    //   averaged: recall_at_k=0.5, mrr=0.5
    let ssu = report
        .diagnostics
        .per_type
        .get("single-session-user")
        .expect("single-session-user breakdown present");
    assert_eq!(ssu.questions, 2);
    assert_eq!(ssu.recall_at_k, 0.5, "avg recall for single-session-user");
    assert_eq!(ssu.mrr, 0.5, "avg mrr for single-session-user");

    // Pin per-type averaging math for "temporal":
    //   q3: recall_at_k=1.0, mrr=1/3   q4: recall_at_k=0.0, mrr=0.0
    //   averaged: recall_at_k=0.5, mrr=1/6
    let temporal = report
        .diagnostics
        .per_type
        .get("temporal")
        .expect("temporal breakdown present");
    assert_eq!(temporal.questions, 2);
    assert_eq!(temporal.recall_at_k, 0.5, "avg recall for temporal");
    assert!(
        (temporal.mrr - 1.0 / 6.0).abs() < 1e-10,
        "avg mrr for temporal: expected {}, got {}",
        1.0_f64 / 6.0,
        temporal.mrr
    );
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
        features: Vec::new(),
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
