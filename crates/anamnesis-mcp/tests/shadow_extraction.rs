//! R2 shadow extraction E2E tests. Capture turns are ingested through the real
//! daemon protocol and a debug-only stub embedding seam, so no model download
//! is needed.
use anamnesis::graph::Edge;
use anamnesis::graph::node::Node;
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SESSION: &str = "r2-shadow-session";
const SCOPE: &str = "project/r2-shadow";
const PROVIDER_RAW_OUTPUT_MARKER: &str = "PROVIDER_RAW_OUTPUT_MARKER";
const PROVIDER_RAW_ERROR_MARKER: &str = "PROVIDER_RAW_ERROR_MARKER";
const STAGE1_SOURCE_MARKER: &str = "RAW_STAGE1_SOURCE_MARKER";

type GraphSnapshot = (Vec<Node>, Vec<Edge>);

struct Fixture {
    _temp: tempfile::TempDir,
    db: PathBuf,
    script: PathBuf,
    calls: PathBuf,
    fallback_calls: PathBuf,
    provider_path: String,
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
        let script = temp.path().join("fake-extractor.sh");
        let calls = temp.path().join("provider.calls");
        let fallback_calls = temp.path().join("default-provider.calls");
        let fallback = temp.path().join("claude");
        std::fs::write(&script, fake_provider_script()).expect("write fake provider");
        std::fs::write(
            &fallback,
            "#!/bin/sh\nprintf '%s\\n' fallback >> \"$EXTRACT_FALLBACK_CALLS\"\n",
        )
        .expect("write default-provider sentinel");
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o755))
            .expect("make default-provider sentinel executable");
        let provider_path = format!(
            "{}:{}",
            temp.path().display(),
            std::env::var("PATH").expect("PATH")
        );
        let daemon = Command::new(env!("CARGO_BIN_EXE_anamnesis"))
            .arg("daemon")
            .env("ANAMNESIS_DB", &db)
            .env("ANAMNESIS_TEST_STUB_EMBEDDINGS", "1")
            .env("ANAMNESIS_DAEMON_GRACE_SECS", "180")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start real daemon");

        // Construct ownership before readiness checks: a failed assertion still kills and
        // reaps the daemon before the temporary database and socket are removed.
        let mut fixture = Self {
            _temp: temp,
            db,
            script,
            calls,
            fallback_calls,
            provider_path,
            daemon,
        };
        let db_dir = fixture.db.parent().expect("database directory");
        assert!(
            wait_until(Duration::from_secs(5), || socket_path(db_dir).is_some()),
            "daemon did not bind a socket"
        );
        assert!(
            fixture.daemon.try_wait().expect("poll daemon").is_none(),
            "daemon exited during readiness"
        );
        fixture.seed_captured_turns();
        fixture
    }

    fn worker(&self, mode: &str) -> std::process::Output {
        self.worker_with(
            Some("shadow"),
            Some(&format!("/bin/sh {}", self.script.display())),
            mode,
        )
    }

    fn worker_with(
        &self,
        extract_mode: Option<&str>,
        extract_command: Option<&str>,
        provider_mode: &str,
    ) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anamnesis"));
        command
            .arg("extract")
            .env("ANAMNESIS_DB", &self.db)
            .env("EXTRACT_MODE", provider_mode)
            .env("EXTRACT_CALLS", &self.calls)
            .env("EXTRACT_FALLBACK_CALLS", &self.fallback_calls)
            .env("PATH", &self.provider_path);
        if extract_mode == Some("shadow") {
            let source_ids = self.scan_source_ids();
            command.env(
                "EXTRACT_SOURCE_IDS",
                source_ids
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        } else {
            command.env_remove("EXTRACT_SOURCE_IDS");
        }
        match extract_mode {
            Some(value) => command.env("ANAMNESIS_EXTRACT_MODE", value),
            None => command.env_remove("ANAMNESIS_EXTRACT_MODE"),
        };
        match extract_command {
            Some(value) => command.env("ANAMNESIS_EXTRACT_CMD", value),
            None => command.env_remove("ANAMNESIS_EXTRACT_CMD"),
        };
        command.output().expect("run extract worker")
    }

    fn provider_calls(&self) -> usize {
        line_count(&self.calls)
    }

    fn fallback_calls(&self) -> usize {
        line_count(&self.fallback_calls)
    }

    fn graph_snapshot(&self) -> GraphSnapshot {
        let storage = SqliteStorage::open(&self.db).expect("open graph snapshot");
        let mut node_ids = storage.all_node_ids();
        node_ids.sort_by_key(|id| id.0);
        let nodes = node_ids
            .into_iter()
            .map(|id| storage.get_node(id).expect("snapshot node").clone())
            .collect();
        let mut edge_ids = storage.all_edge_ids();
        edge_ids.sort_by_key(|id| id.0);
        let edges = edge_ids
            .into_iter()
            .map(|id| storage.get_edge(id).expect("snapshot edge").clone())
            .collect();
        (nodes, edges)
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
    fn seed_captured_turns(&self) {
        let turns = (0..10)
            .map(|index| {
                json!({
                    "speaker": "user",
                    "text": format!(
                        "turn {index}: deterministic shadow extraction source {}",
                        if index == 0 { STAGE1_SOURCE_MARKER } else { "" }
                    ),
                    "at_ms": 1_000 + index
                })
            })
            .collect::<Vec<_>>();
        let response = daemon_request(
            &self.db,
            json!({
                "op": "ingest",
                "session": SESSION,
                "turns": turns,
                "capture": true,
                "scope": SCOPE
            }),
        );
        assert_eq!(
            response["status"].as_str(),
            Some("ok"),
            "capture ingestion failed: {response}"
        );
        assert_eq!(
            self.scan_source_ids().len(),
            10,
            "capture ingestion must create ten eligible episodic sources"
        );
    }
}

#[test]
fn shadow_extract_stages_once_without_mutating_graph_or_reinvoking_provider() {
    let fixture = Fixture::new();
    let before = fixture.graph_snapshot();
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "initial graph");
    assert_eq!(fixture.scan_source_ids().len(), 10);

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
    assert_graph_unchanged(
        &before,
        &fixture.graph_snapshot(),
        "successful shadow staging",
    );

    let audit = fixture.audit();
    assert!(
        audit.status.success(),
        "audit stderr: {}",
        String::from_utf8_lossy(&audit.stderr)
    );
    let audit_stdout = String::from_utf8_lossy(&audit.stdout);
    assert!(audit_stdout.contains("first staged candidate"));
    assert!(
        audit_stdout.contains(STAGE1_SOURCE_MARKER),
        "audit may read retained Stage-1 source content"
    );
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "audit");
    assert_policy_has_no_provider_raw(&fixture.db);

    let stats = fixture.stats();
    assert!(
        stats.status.success(),
        "stats stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stats_stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(stats_stdout.contains(&format!("nodes:                {}", before.0.len())));
    assert!(stats_stdout.contains(&format!("edges:                {}", before.1.len())));
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "stats");

    let second = fixture.worker("valid");
    assert!(second.status.success());
    assert!(String::from_utf8_lossy(&second.stdout).contains("insufficient captured turns"));
    assert_eq!(
        fixture.provider_calls(),
        1,
        "ledgered sources must not be resent"
    );
    assert_eq!(policy_counts(&fixture.db), (1, 10, 2, 1));
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "successful retry noop");
}

#[test]
fn shadow_extract_zero_output_ledgers_every_source_and_is_not_resent() {
    let fixture = Fixture::new();
    let before = fixture.graph_snapshot();
    let output = fixture.worker("zero");
    assert!(
        output.status.success(),
        "worker stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("candidate_count=0"));
    assert_eq!(policy_counts(&fixture.db), (1, 10, 0, 0));
    assert_eq!(fixture.provider_calls(), 1);
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "zero-output staging");

    assert!(fixture.worker("zero").status.success());
    assert_eq!(
        fixture.provider_calls(),
        1,
        "zero-output success must be ledgered"
    );
    assert_eq!(policy_counts(&fixture.db), (1, 10, 0, 0));
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "zero-output retry noop");
}

#[test]
fn invalid_schema_records_no_source_ledger_and_retries_identical_sources() {
    let fixture = Fixture::new();
    let before = fixture.graph_snapshot();
    let expected_sources = fixture.scan_source_ids();
    let invalid = fixture.worker("invalid");
    assert!(
        !invalid.status.success(),
        "invalid schema must fail the worker"
    );
    assert_no_provider_raw_in_cli(&invalid);
    assert_eq!(fixture.provider_calls(), 1);
    assert_eq!(fixture.fallback_calls(), 0);
    assert_eq!(policy_counts(&fixture.db), (1, 0, 0, 0));
    assert_policy_has_no_provider_raw(&fixture.db);
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "invalid-schema failure");
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
    assert_graph_unchanged(&before, &fixture.graph_snapshot(), "invalid-schema retry");
}

#[test]
fn timeout_records_no_source_ledger_and_manual_pull_does_not_change_shadow_scan() {
    let fixture = Fixture::new();
    let before = fixture.graph_snapshot();
    let before_pull = fixture.scan_source_ids();
    fixture.pull_pending();
    let after_pull = fixture.graph_snapshot();
    assert_manual_pull_only_updates_extraction_state(&before, &after_pull);
    assert_eq!(
        fixture.scan_source_ids(),
        before_pull,
        "PullPending must not affect shadow selection"
    );

    // The fake emits valid output only after 121 seconds, so only the real 120-second
    // provider deadline can make this invocation fail.
    let started = Instant::now();
    let timeout = fixture.worker("timeout");
    let elapsed = started.elapsed();
    assert!(
        !timeout.status.success(),
        "provider must time out at the production boundary"
    );
    assert!(
        elapsed >= Duration::from_secs(115),
        "timeout returned too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(130),
        "timeout exceeded bounded deadline: {elapsed:?}"
    );
    assert_no_provider_raw_in_cli(&timeout);
    assert_eq!(fixture.provider_calls(), 1);
    assert_eq!(fixture.fallback_calls(), 0);
    assert_eq!(policy_counts(&fixture.db), (1, 0, 0, 0));
    assert_eq!(latest_error_kind(&fixture.db), Some("timeout".to_owned()));
    assert_policy_has_no_provider_raw(&fixture.db);
    assert_snapshot_unchanged(&after_pull, &fixture.graph_snapshot(), "timeout failure");
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
    assert_snapshot_unchanged(&after_pull, &fixture.graph_snapshot(), "timeout retry");
}

#[test]
fn extract_cli_requires_explicit_shadow_opt_in_without_policy_or_graph_changes() {
    let fixture = Fixture::new();
    let before = fixture.graph_snapshot();
    for (label, mode) in [
        ("unset", None),
        ("off", Some("off")),
        ("auto", Some("auto")),
        ("boolean", Some("true")),
        ("invalid", Some("not-a-mode")),
    ] {
        let output = fixture.worker_with(
            mode,
            Some(&format!("/bin/sh {}", fixture.script.display())),
            "valid",
        );
        assert!(
            output.status.success(),
            "{label} mode stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fixture.provider_calls(), 0, "{label} mode invoked provider");
        assert_eq!(
            fixture.fallback_calls(),
            0,
            "{label} mode invoked default provider"
        );
        assert_eq!(
            policy_counts_if_present(&fixture.db),
            (0, 0, 0, 0),
            "{label} mode changed policy"
        );
        assert_graph_unchanged(&before, &fixture.graph_snapshot(), label);
    }
}

#[test]
fn invalid_or_empty_shadow_command_fails_without_default_provider_fallback() {
    let fixture = Fixture::new();
    let before = fixture.graph_snapshot();
    for (label, configured_command) in [("empty", ""), ("invalid", "'unterminated")] {
        let output = fixture.worker_with(Some("shadow"), Some(configured_command), "valid");
        assert!(!output.status.success(), "{label} command must fail");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("could not construct extraction profile"),
            "{label} command returned an untyped error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fixture.provider_calls(),
            0,
            "{label} command invoked configured provider"
        );
        assert_eq!(
            fixture.fallback_calls(),
            0,
            "{label} command fell back to default provider"
        );
        assert_eq!(
            policy_counts_if_present(&fixture.db),
            (0, 0, 0, 0),
            "{label} command changed policy"
        );
        assert_graph_unchanged(&before, &fixture.graph_snapshot(), label);
    }

    let missing = fixture.worker_with(
        Some("shadow"),
        Some("/definitely-not-anamnesis-extractor"),
        "valid",
    );
    assert!(
        !missing.status.success(),
        "missing configured provider must fail"
    );
    assert_no_provider_raw_in_cli(&missing);
    assert_eq!(fixture.provider_calls(), 0);
    assert_eq!(
        fixture.fallback_calls(),
        0,
        "spawn failure must not invoke the default provider"
    );
    assert_eq!(policy_counts(&fixture.db), (1, 0, 0, 0));
    assert_eq!(latest_error_kind(&fixture.db), Some("spawn".to_owned()));
    assert_graph_unchanged(
        &before,
        &fixture.graph_snapshot(),
        "missing configured provider",
    );
}

fn assert_graph_unchanged(before: &GraphSnapshot, after: &GraphSnapshot, operation: &str) {
    assert_eq!(
        after, before,
        "{operation} must preserve every node and edge field"
    );
    assert!(
        after
            .0
            .iter()
            .filter(|node| node.metadata.contains_key("anamnesis:turn_key"))
            .all(|node| { node.metadata.get("anamnesis:extracted") == Some(&"false".to_owned()) }),
        "{operation} must retain anamnesis:extracted=false for every captured turn"
    );
}

fn assert_snapshot_unchanged(before: &GraphSnapshot, after: &GraphSnapshot, operation: &str) {
    assert_eq!(
        after, before,
        "{operation} must preserve every node and edge field"
    );
}

fn assert_manual_pull_only_updates_extraction_state(before: &GraphSnapshot, after: &GraphSnapshot) {
    let mut expected = before.clone();
    for (expected_node, actual_node) in expected.0.iter_mut().zip(&after.0) {
        if !actual_node.metadata.contains_key("anamnesis:turn_key") {
            continue;
        }
        let state = actual_node
            .metadata
            .get("anamnesis:extracted")
            .expect("manual PullPending retains extraction metadata");
        assert!(
            state.starts_with("pending:"),
            "manual PullPending must mark each source pending"
        );
        expected_node
            .metadata
            .insert("anamnesis:extracted".to_owned(), state.clone());
    }
    assert_eq!(
        after, &expected,
        "manual PullPending may change only extraction-state metadata"
    );
}

fn assert_no_provider_raw_in_cli(output: &std::process::Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !text.contains(PROVIDER_RAW_OUTPUT_MARKER),
        "CLI exposed provider raw output"
    );
    assert!(
        !text.contains(PROVIDER_RAW_ERROR_MARKER),
        "CLI exposed provider raw error"
    );
}

fn assert_policy_has_no_provider_raw(db: &Path) {
    let connection = Connection::open(db).expect("open policy database");
    for (table, columns) in [
        ("extractor_profiles", "profile_id, components, status"),
        ("extract_runs", "profile_id, mode, error_kind"),
        ("extract_run_sources", "profile_id, turn_key"),
        (
            "extract_candidates",
            "item_local_id, content, kind, source_turn_keys, source_session_id, source_scope, source_content_hashes, source_node_ids, idempotency_key",
        ),
        ("extract_relations", "relation_type, idempotency_key"),
    ] {
        let mut statement = connection
            .prepare(&format!("SELECT {columns} FROM {table}"))
            .expect("read policy rows");
        let mut rows = statement.query([]).expect("query policy rows");
        while let Some(row) = rows.next().expect("read policy row") {
            for index in 0..row.as_ref().column_count() {
                let value: Option<String> = row.get(index).expect("read policy text");
                let value = value.unwrap_or_default();
                assert!(
                    !value.contains(PROVIDER_RAW_OUTPUT_MARKER),
                    "{table} retained provider raw output"
                );
                assert!(
                    !value.contains(PROVIDER_RAW_ERROR_MARKER),
                    "{table} retained provider raw error"
                );
            }
        }
    }
}

fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn policy_counts(db: &Path) -> (u64, u64, u64, u64) {
    let connection = Connection::open(db).expect("open policy database");
    policy_counts_from(&connection)
}

fn policy_counts_if_present(db: &Path) -> (u64, u64, u64, u64) {
    let connection = Connection::open(db).expect("open policy database");
    let exists: bool = connection
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'extract_runs')", [], |row| row.get(0))
        .expect("inspect policy schema");
    if exists {
        policy_counts_from(&connection)
    } else {
        (0, 0, 0, 0)
    }
}

fn policy_counts_from(connection: &Connection) -> (u64, u64, u64, u64) {
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

fn latest_error_kind(db: &Path) -> Option<String> {
    Connection::open(db)
        .expect("open policy database")
        .query_row(
            "SELECT error_kind FROM extract_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read latest extraction error kind")
}

fn profile() -> Value {
    json!({"provider_id":"shadow-e2e","model_id":"provider-default","prompt_version":2,"schema_version":1,"normalization_version":1,"relation_policy_version":1,"command_hash":"shadow-e2e"})
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
    : "${EXTRACT_SOURCE_IDS:?}"
    OLDIFS=$IFS
    IFS=,
    set -- $EXTRACT_SOURCE_IDS
    IFS=$OLDIFS
    [ "$#" -ge 2 ]
    cat >/dev/null
    printf '{"items":[{"item_local_id":"one","content":"first staged candidate","kind":"decision","confidence":0.9,"source_node_ids":[%s]},{"item_local_id":"two","content":"second staged candidate","kind":"lesson","confidence":0.8,"source_node_ids":[%s]}],"relations":[{"from_item_local_id":"one","to_item_local_id":"two","relation_type":"supports"}]}\n' "$1" "$2"
    ;;
  zero)
    cat >/dev/null
    printf '%s\n' '{"items":[],"relations":[]}'
    ;;
  invalid)
    cat >/dev/null
    printf '%s\n' 'PROVIDER_RAW_ERROR_MARKER' >&2
    printf '%s\n' '{"items":"PROVIDER_RAW_OUTPUT_MARKER","relations":[]}'
    ;;
  timeout)
    cat >/dev/null
    sleep 121
    printf '%s\n' '{"items":[],"relations":[]}'
    ;;
esac
"#
}
