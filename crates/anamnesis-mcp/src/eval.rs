//! Insight-recall quality gate (`remember` = `add_note` mode).
//!
//! The published LoCoMo/LongMemEval numbers measure *conversation* ingest. The
//! MCP server's `remember` path stores distilled insights via `add_note`, which
//! is a different memory mode with no existing benchmark. This module is its
//! gate: a small labeled set of insights + queries, scored with the real bge
//! model through the same `MemoryRegistry::recall` the server uses.
//!
//! Run it (the model lives in the repo cache):
//! ```sh
//! FASTEMBED_CACHE_DIR=$PWD/.fastembed_cache \
//!   cargo test -p anamnesis-mcp --bin anamnesis -- --ignored insight_recall
//! ```

use crate::memory::MemoryRegistry;

/// Distilled engineering insights, one per `remember` call.
const INSIGHTS: &[&str] = &[
    "the auth race condition was fixed by adding a mutex in the login middleware",
    "we chose postgres over mysql mainly for jsonb support and row-level security",
    "the nightly deploy flaked because the CI cache key omitted the lockfile hash",
    "p99 latency dropped 40% after we added a connection pool to the redis client",
    "the mobile app crashes on android 12 when camera permission is denied mid-session",
    "we migrated the internal billing service from REST to gRPC to cut serialization overhead",
    "the memory leak in the worker was a tokio task that was never aborted on shutdown",
    "stripe webhooks must be idempotent because they retry with the same event id",
    "we set the JWT expiry to 15 minutes and rotate the refresh token on every use",
    "the search index rebuild takes 6 hours so we run it on a weekly cron, not per-deploy",
    "onboarding conversion improved 12% after we removed the mandatory phone number field",
    "the flaky integration test was a timing assumption about kafka consumer lag",
    "we cache embeddings in sqlite because recomputing them per query was the latency bottleneck",
    "the gdpr deletion job must cascade to the analytics warehouse, not just the primary db",
    "rate limiting is per-api-key with a sliding window in redis at 1000 requests per minute",
    "pdf export broke on unicode filenames until we switched to content-disposition rfc 5987",
];

/// `(query, index-into-INSIGHTS of the gold answer)`.
const QUERIES: &[(&str, usize)] = &[
    ("how did we solve the login concurrency bug", 0),
    ("why did we pick our database", 1),
    ("what caused the CI deploy to fail intermittently", 2),
    ("how did we improve redis tail latency", 3),
    ("android camera permission crash", 4),
    ("why did billing move off REST", 5),
    ("what was the worker memory leak", 6),
    ("do stripe webhooks need to be idempotent", 7),
    ("what is our access token expiry policy", 8),
    ("how often do we rebuild the search index", 9),
    ("what change improved signup conversion", 10),
    ("why was the kafka test flaky", 11),
    ("where do we store computed embeddings", 12),
    ("what must the data deletion job include", 13),
    ("how is api rate limiting implemented", 14),
    ("the unicode filename export bug", 15),
];

/// Gate: minimum Recall@5 the insight path must clear. Measured baseline on this
/// set is 0.94 (R@1 0.375, R@10 1.0, MRR 0.51) with the dedup recall; 0.80 is the
/// regression floor. Raise it as the recipe improves, never silently lower it.
const GATE_RECALL_AT_5: f64 = 0.80;

#[test]
#[ignore = "requires the bge model; run with --ignored and FASTEMBED_CACHE_DIR set"]
fn insight_recall_quality_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("eval.db");
    // reinforce=false so each query is an independent retrieval measurement.
    let mut reg =
        MemoryRegistry::file_backed(db, dir.path().to_path_buf(), "default".to_string(), false);

    for insight in INSIGHTS {
        reg.remember(insight, None).expect("remember");
    }

    // `recall` already collapses the Episodic+Semantic copies (see memory.rs),
    // so we score its real, distinct output directly.
    let ks = [1usize, 5, 10];
    let mut at = [0usize; 3];
    let mut mrr_sum = 0.0_f64;
    let mut found = 0usize;

    for (query, gold) in QUERIES {
        let gold_text = INSIGHTS[*gold];
        let hits = reg.recall(query, 10, None).expect("recall");
        match hits.iter().position(|h| h.text == gold_text) {
            Some(rank) => {
                found += 1;
                mrr_sum += 1.0 / (rank as f64 + 1.0);
                for (i, k) in ks.iter().enumerate() {
                    if rank < *k {
                        at[i] += 1;
                    }
                }
            }
            None => eprintln!("  MISS  q={query:?}  gold={gold_text:?}"),
        }
    }

    let n = QUERIES.len() as f64;
    let (r1, r5, r10, mrr) = (
        at[0] as f64 / n,
        at[1] as f64 / n,
        at[2] as f64 / n,
        mrr_sum / n,
    );
    eprintln!("\n=== insight-recall eval (n={}) ===", QUERIES.len());
    eprintln!("Recall@1  = {r1:.3}");
    eprintln!("Recall@5  = {r5:.3}");
    eprintln!("Recall@10 = {r10:.3}");
    eprintln!("MRR       = {mrr:.3}");
    eprintln!("found     = {found}/{}", QUERIES.len());

    assert!(
        r5 >= GATE_RECALL_AT_5,
        "insight Recall@5 {r5:.3} below gate {GATE_RECALL_AT_5:.3}"
    );
}
