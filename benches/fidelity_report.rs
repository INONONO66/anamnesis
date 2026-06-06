#[path = "fidelity_common/mod.rs"]
mod fidelity_common;

use std::fs;
use std::io::Write;
use std::path::Path;

fn main() {
    let out = Path::new("target/fidelity");
    fs::create_dir_all(out).expect("create target/fidelity");

    let mut summary = Vec::new();
    for p in fidelity_common::all() {
        let r = p.measure();
        // one CSV per paradigm: long format `series,x,y`
        let mut csv = String::from("series,x,y\n");
        for s in &r.series {
            for (x, y) in s.xs.iter().zip(&s.ys) {
                csv.push_str(&format!("{},{},{}\n", s.name, x, y));
            }
        }
        let csv_path = out.join(format!("{}.csv", r.name));
        fs::File::create(&csv_path)
            .unwrap()
            .write_all(csv.as_bytes())
            .unwrap();
        eprintln!("[{}] passed={} -> {}", r.name, r.passed, csv_path.display());
        summary.push(serde_json::json!({
            "name": r.name, "passed": r.passed,
            "metrics": r.metrics, "explanation": r.explanation,
        }));
    }
    let report = serde_json::json!({
        "all_passed": summary.iter().all(|s| s["passed"].as_bool().unwrap_or(false)),
        "paradigms": summary,
    });
    fs::write(
        out.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("wrote {}", out.join("report.json").display());
}
