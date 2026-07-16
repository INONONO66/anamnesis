//! R2 shadow extraction E2E tests. The graph is seeded through the public core
//! storage API so the real daemon and worker exercise durable capture-shaped data
//! without initializing FastEmbed or requiring a model download.

use anamnesis::engine::SourceKind;
use anamnesis::graph::node::{Node, Origin};
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, MemoryTier, ScopePath, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SESSION: &str = "r2-shadow-session";
const SCOPE: &str = "project/r2-shadow";

struct Fixture {
    _temp: tempfile::TempDir,
    db: PathBuf,
    script: PathBuf,
    calls: PathBuf,
    daemon: Child,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let db = temp.path().join("memory.db");
        seed_captured_turns(&db);
        let script = temp.path().join("fake-extractor.sh");
        let calls = temp.path().join("provider.calls");
        std::fs::write(&script, fake_provider_script()).expect("write fake provider");
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_anamnesis"))
            .arg("daemon")
            .env("ANAMNESIS_DB", &db)
            .env("ANAMNESIS_DAEMON_GRACE_SECS", "60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start real daemon");
        let db_dir = db.parent().expect("database directory");
        assert!(
            wait_until(Duration::from_secs(5), || socket_path(db_dir).is_some()),
            "daemon did not bind a socket"
        );
        // Keep the compiler honest that this is an owned child, not a detached
        // launcher process; Drop terminates it before its temporary DB is removed.
        assert!(daemon.try_wait().expect("poll daemon").is_none());
        Self {
            _temp: temp,
            db,
            script,
            calls,
            daemon,
        }
    }

    fn worker(&self, mode: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_anamnesis"))
            .arg("extract")
            .env("ANAMNESIS_DB", &self.db)
            .env("ANAMNESIS_EXTRACT_MODE", "shadow")
            .env(
                "ANAMNESIS_EXTRACT_CMD",
                format!("/bin/sh {}", self.script.display()),
            )
            .env("EXTRACT_MODE", mode)
            .env("EXTRACT_CALLS", &self.calls)
            .output()
            .expect("run extract worker")
    }

    fn provider_calls(&self) -> usize {
        std::fs::read_to_string(&self.calls)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn graph_counts(&self) -> (usize, usize) {
        let storage = SqliteStorage::open(&self.db).expect("open graph for count");
        (storage.node_count(), storage.edge_count())
    }

    fn audit(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_anamnesis"))
            .args(["extract", "--audit"])
            .env("ANAMNESIS_DB", &self.db)
            .output()
            .expect("run extraction audit")
    }

    fn stats(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_anamnesis"))
            .arg("stats")
            .env("ANAMNESIS_DB", &self.db)
            .output()
            .expect("run graph stats")
    }

    fn scan_source_ids(&self) -> Vec<u64> {
        let response = daemon_request(
            &self.db,
            json!({
                "op": "extraction_scan",
                "profile": profile(),
                "min_turns": 10,
                "max_turns": 20
            }),
        );
        response["text"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|scan| scan["sources"].as_array().cloned())
            .expect("scan sources")
            .iter()
            .map(|source| source["node_id"].as_u64().expect("source node id"))
            .collect()
    }

    fn pull_pending(&self) {
        let response = daemon_request(&self.db, json!({"op":"pull_pending", "limit": 50}));
        let items: Vec<Value> = serde_json::from_str(response["text"].as_str().expect("pull text"))
            .expect("pull payload");
        assert_eq!(
            items.len(),
            10,
            "manual PullPending must see seeded captures"
        );
    }
}

#[test]
fn shadow_extract_stages_once_without_mutating_graph_or_reinvoking_provider() {
    let fixture = Fixture::new();
    let before = fixture.graph_counts();
    assert_eq!(fixture.scan_source_ids(), (0..10).collect::<Vec<_>>());

    let first = fixture.worker("valid");
    assert!(
        first.status.success(),
        "worker stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("run_id=1"));
    assert!(String::from_utf8_lossy(&first.stdout).contains("candidate_count=2"));
    assert!(String::from_utf8_lossy(&first.stdout).contains("relation_count=1"));
    assert_eq!(fixture.provider_calls(), 1);
    assert_eq!(policy_counts(&fixture.db), (1, 10, 2, 1));
    assert_eq!(
        fixture.graph_counts(),
        before,
        "shadow staging must not change graph"
    );

    let audit = fixture.audit();
    assert!(
        audit.status.success(),
        "audit stderr: {}",
        String::from_utf8_lossy(&audit.stderr)
    );
    let audit_stdout = String::from_utf8_lossy(&audit.stdout);
    assert!(audit_stdout.contains("first staged candidate"));
    assert!(audit_stdout.contains("turn 0: deterministic shadow extraction source"));

    let stats = fixture.stats();
    assert!(
        stats.status.success(),
        "stats stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stats_stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(stats_stdout.contains("nodes:                10"));
    assert!(stats_stdout.contains("edges:                0"));

    let second = fixture.worker("valid");
    assert!(second.status.success());
    assert!(String::from_utf8_lossy(&second.stdout).contains("insufficient captured turns"));
    assert_eq!(
        fixture.provider_calls(),
        1,
        "ledgered sources must not be resent"
    );
    assert_eq!(policy_counts(&fixture.db), (1, 10, 2, 1));
}

#[test]
fn shadow_extract_zero_output_ledgers_every_source_and_is_not_resent() {
    let fixture = Fixture::new();
    let before = fixture.graph_counts();
    let output = fixture.worker("zero");
    assert!(
        output.status.success(),
        "worker stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("candidate_count=0"));
    assert_eq!(policy_counts(&fixture.db), (1, 10, 0, 0));
    assert_eq!(fixture.provider_calls(), 1);
    assert_eq!(fixture.graph_counts(), before);

    assert!(fixture.worker("zero").status.success());
    assert_eq!(
        fixture.provider_calls(),
        1,
        "zero-output success must be ledgered"
    );
    assert_eq!(policy_counts(&fixture.db), (1, 10, 0, 0));
}

#[test]
fn invalid_schema_records_no_source_ledger_and_retries_identical_sources() {
    let fixture = Fixture::new();
    let expected_sources = fixture.scan_source_ids();
    let invalid = fixture.worker("invalid");
    assert!(
        !invalid.status.success(),
        "invalid schema must fail the worker"
    );
    assert_eq!(fixture.provider_calls(), 1);
    assert_eq!(policy_counts(&fixture.db), (1, 0, 0, 0));
    assert_eq!(
        fixture.scan_source_ids(),
        expected_sources,
        "failed sources must remain selectable"
    );

    let retry = fixture.worker("zero");
    assert!(
        retry.status.success(),
        "retry stderr: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(fixture.provider_calls(), 2);
    assert_eq!(policy_counts(&fixture.db), (2, 10, 0, 0));
}

#[test]
fn timeout_records_no_source_ledger_and_manual_pull_does_not_change_shadow_scan() {
    let fixture = Fixture::new();
    let before_pull = fixture.scan_source_ids();
    fixture.pull_pending();
    assert_eq!(
        fixture.scan_source_ids(),
        before_pull,
        "PullPending must not affect shadow selection"
    );

    // The fake provider exceeds the worker's fixed 120-second production timeout.
    let timeout = fixture.worker("timeout");
    assert!(
        !timeout.status.success(),
        "provider must time out at the 120-second production boundary"
    );
    assert_eq!(fixture.provider_calls(), 1);
    assert_eq!(policy_counts(&fixture.db), (1, 0, 0, 0));
    assert_eq!(
        fixture.scan_source_ids(),
        before_pull,
        "timeout sources must remain selectable"
    );

    let retry = fixture.worker("zero");
    assert!(
        retry.status.success(),
        "retry stderr: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(fixture.provider_calls(), 2);
    assert_eq!(policy_counts(&fixture.db), (2, 10, 0, 0));
}

fn seed_captured_turns(db: &Path) {
    let mut storage = SqliteStorage::open(db).expect("create fixture database");
    for index in 0..10 {
        let id = storage.next_node_id();
        let mut metadata = HashMap::new();
        metadata.insert("capture".to_owned(), "true".to_owned());
        metadata.insert("anamnesis:extracted".to_owned(), "false".to_owned());
        metadata.insert("anamnesis:turn_key".to_owned(), format!("turn-{index:02}"));
        storage
            .set_node(Node {
                id,
                node_type: KnowledgeType::Episodic,
                name: format!("captured turn {index}"),
                summary: None,
                content: format!("turn {index}: deterministic shadow extraction source"),
                embedding: None,
                created_at: Timestamp(1_000 + index),
                updated_at: Timestamp(1_000 + index),
                accessed_at: Timestamp(1_000 + index),
                valid_from: None,
                valid_until: None,
                salience: 0.5,
                retained_action: 0.0,
                evidence_prior: 0.0,
                access_count: 0,
                access_history: VecDeque::new(),
                tier: MemoryTier::Auto,
                origin: Origin {
                    peer_id: PeerId(0),
                    source_kind: SourceKind::AgentObservation,
                    session_id: SESSION.to_owned(),
                    scope: ScopePath::new(SCOPE).expect("fixture scope"),
                    confidence: 1.0,
                },
                entity_tags: vec![],
                metadata,
            })
            .expect("seed captured source");
    }
    storage.flush().expect("flush seeded graph");
}

fn profile() -> Value {
    json!({
        "provider_id": "shadow-e2e",
        "model_id": "provider-default",
        "prompt_version": 1,
        "schema_version": 1,
        "normalization_version": 1,
        "relation_policy_version": 1,
        "command_hash": "shadow-e2e"
    })
}

fn policy_counts(db: &Path) -> (u64, u64, u64, u64) {
    let connection = Connection::open(db).expect("open policy database");
    let count = |table: &str| {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count policy table")
    };
    (
        count("extract_runs"),
        count("extract_run_sources"),
        count("extract_candidates"),
        count("extract_relations"),
    )
}

fn daemon_request(db: &Path, request: Value) -> Value {
    let socket = socket_path(db.parent().expect("database directory")).expect("daemon socket");
    let mut stream = UnixStream::connect(socket).expect("connect daemon protocol");
    let line = serde_json::to_string(&request).expect("encode protocol request");
    stream
        .write_all(line.as_bytes())
        .expect("write protocol request");
    stream.write_all(b"\n").expect("frame protocol request");
    stream.flush().expect("flush protocol request");
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("read protocol response");
    serde_json::from_str(&line).expect("decode protocol response")
}

fn socket_path(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sock"))
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    condition()
}

fn fake_provider_script() -> &'static str {
    r#"#!/bin/sh
set -eu
: "${EXTRACT_MODE:?}"
: "${EXTRACT_CALLS:?}"
printf '%s\n' call >> "$EXTRACT_CALLS"
case "$EXTRACT_MODE" in
  valid)
    cat >/dev/null
    printf '%s\n' '{"items":[{"item_local_id":"one","content":"first staged candidate","kind":"decision","confidence":0.9,"source_node_ids":[0]},{"item_local_id":"two","content":"second staged candidate","kind":"lesson","confidence":0.8,"source_node_ids":[1]}],"relations":[{"from_item_local_id":"one","to_item_local_id":"two","relation_type":"supports"}]}'
    ;;
  zero)
    cat >/dev/null
    printf '%s\n' '{"items":[],"relations":[]}'
    ;;
  invalid)
    cat >/dev/null
    printf '%s\n' '{"items":"not-an-array","relations":[]}'
    ;;
  timeout)
    cat >/dev/null
    sleep 121
    ;;
esac
"#
}
