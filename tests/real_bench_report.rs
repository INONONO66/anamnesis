#[path = "../benches/eval_common/mod.rs"]
mod eval_common;

use std::path::{Path, PathBuf};

use eval_common::real_bench::graph::WarmupReport;
use eval_common::real_bench::report::{
    LatencyReport, RealBenchReport, ReportGraphStats, write_report,
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
