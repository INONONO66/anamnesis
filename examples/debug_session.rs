//! Demonstrates the OBSERVE → HYPOTHESIZE → EXPERIMENT → CONCLUDE debug lifecycle.
//!
//! Uses the structured debugging API to track hypotheses, gather evidence,
//! reject or confirm hypotheses, and search rejected hypotheses later.
//!
//! Run: `cargo run --example debug_session`

use anamnesis::graph::node::Origin;
use anamnesis::{DebugOutcome, Engine, Error, EvidenceResult, Timestamp};

fn origin() -> Origin {
    Origin {
        agent_id: "debugger-agent".into(),
        session_id: "debug-session-1".into(),
        scope: anamnesis::graph::ScopePath::new("webapp").expect("valid scope"),
        confidence: 0.9,
    }
}

fn main() -> Result<(), Error> {
    let mut engine = Engine::new();

    // ── OBSERVE: start a debug session for the problem ───────────────
    let session = engine.start_debug(
        "API returns 500 on large file uploads",
        origin(),
        Timestamp(1000),
    )?;
    println!("Started debug session (node {})", session.0);

    // ── HYPOTHESIZE: log candidate explanations ──────────────────────
    let h_parser = engine.log_hypothesis(
        session,
        "Multipart parser fails on payloads > 10 MB",
        origin(),
        Timestamp(1100),
    )?;
    println!("  Hypothesis A (node {}): parser size limit", h_parser.0);

    let h_oom = engine.log_hypothesis(
        session,
        "Worker OOM from unbounded request buffer",
        origin(),
        Timestamp(1200),
    )?;
    println!("  Hypothesis B (node {}): OOM on buffer", h_oom.0);

    let h_timeout = engine.log_hypothesis(
        session,
        "Upload timeout triggers incomplete write",
        origin(),
        Timestamp(1300),
    )?;
    println!("  Hypothesis C (node {}): upload timeout", h_timeout.0);

    // ── EXPERIMENT: gather evidence for each hypothesis ───────────────

    let ev1 = engine.log_evidence(
        h_parser,
        "Parser handles 50 MB in local test harness without error",
        EvidenceResult::Contradicts,
        origin(),
        Timestamp(1400),
    )?;
    println!("  Evidence {} contradicts hypothesis A", ev1.0);

    let ev2 = engine.log_evidence(
        h_oom,
        "dmesg shows OOM kill for the API process at 14:32",
        EvidenceResult::Supports,
        origin(),
        Timestamp(1500),
    )?;
    println!("  Evidence {} supports hypothesis B", ev2.0);

    let ev3 = engine.log_evidence(
        h_timeout,
        "Access logs show no timeout entries near failure window",
        EvidenceResult::Neutral,
        origin(),
        Timestamp(1600),
    )?;
    println!("  Evidence {} is neutral for hypothesis C", ev3.0);

    // ── CONCLUDE: reject or confirm each hypothesis ──────────────────

    engine.reject_hypothesis(
        h_parser,
        "Parser handles large payloads correctly in isolation",
        Timestamp(1700),
    )?;
    println!("  Rejected hypothesis A");

    engine.confirm_hypothesis(
        h_oom,
        "Unbounded buffer causes OOM on files > 8 MB; fix: streaming parser",
        Timestamp(1800),
    )?;
    println!("  Confirmed hypothesis B");

    engine.reject_hypothesis(
        h_timeout,
        "No timeout correlation found in logs",
        Timestamp(1900),
    )?;
    println!("  Rejected hypothesis C");

    engine.end_debug(
        session,
        DebugOutcome::Resolved(
            "OOM from unbounded request buffer; switched to streaming parser".into(),
        ),
        Timestamp(2000),
    )?;
    println!("  Session resolved.\n");

    // ── SEARCH: find rejected hypotheses for future reference ─────────

    let all_rejected = engine.search_rejected_hypotheses("", 10)?;
    println!("All rejected hypotheses ({} total):", all_rejected.len());
    for id in &all_rejected {
        let node = engine.graph().get_node(*id)?;
        let reason = node
            .metadata
            .get("rejection_reason")
            .map(String::as_str)
            .unwrap_or("?");
        println!("  [{}] {} — reason: {}", id.0, node.name, reason);
    }

    let parser_rejected = engine.search_rejected_hypotheses("parser", 10)?;
    println!(
        "\nRejected hypotheses matching 'parser': {} found",
        parser_rejected.len()
    );
    for id in &parser_rejected {
        let node = engine.graph().get_node(*id)?;
        println!("  [{}] {}", id.0, node.name);
    }

    let oom_rejected = engine.search_rejected_hypotheses("OOM", 10)?;
    println!(
        "\nRejected hypotheses matching 'OOM': {} (confirmed hypothesis excluded)",
        oom_rejected.len()
    );

    println!("\nDone.");
    Ok(())
}
