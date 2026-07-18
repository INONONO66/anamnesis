//! Crash-recovery gate for the write-behind storage engine.
//!
//! Base rows and their initial hot-field rows are written eagerly inside one
//! `BEGIN IMMEDIATE` by `set_node`; LATER hot-field mutations are write-behind
//! and committed only by `flush`. A kill between the two must leave a
//! reopenable database: flushed state exact, unflushed dirty state lost (never
//! resurrected), never a parse failure or id collision.
//!
//! The child process signals each durable seam on stdout; the parent kills it
//! with SIGKILL only after the second marker — an event-synchronized kill
//! point, not a timing guess.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};

const CHILD_ENV: &str = "ANAMNESIS_CRASH_RECOVERY_CHILD";
const DB_ENV: &str = "ANAMNESIS_CRASH_RECOVERY_DB";
const FLUSH_MARKER: &str = "BATCH1_FLUSHED";
const ROW_MARKER: &str = "BATCH2_ROW_WRITTEN";

fn make_node(id: NodeId, name: &str, tags: Vec<String>) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: name.to_string(),
        summary: None,
        content: "content".to_string(),
        embedding: None,
        created_at: Timestamp(0),
        updated_at: Timestamp(0),
        accessed_at: Timestamp(0),
        valid_from: None,
        valid_until: None,
        salience: 0.5,
        retained_action: 0.0,
        evidence_prior: 0.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 1.0,
        },
        entity_tags: tags,
        metadata: HashMap::new(),
    }
}

fn child_main() -> ! {
    let path = std::env::var(DB_ENV).expect("db path env");
    let mut storage = SqliteStorage::open(&path).expect("child opens db");

    let a = storage.next_node_id();
    storage
        .set_node(make_node(a, "alpha", vec![]))
        .expect("node a");
    let b = storage.next_node_id();
    storage
        .set_node(make_node(b, "beta", vec!["tag-beta".to_string()]))
        .expect("node b");
    storage.set_salience(a, 0.77).expect("salience a");
    storage.flush().expect("batch 1 flushed");
    println!("{FLUSH_MARKER}");
    std::io::stdout().flush().expect("marker flushed");

    // A kill at this seam must discard everything after the flush: node c's
    // eager rows (salience 0.5 from set_node) are durable, but the dirty
    // hot-field writes below never reach the database.
    let c = storage.next_node_id();
    storage
        .set_node(make_node(c, "gamma", vec![]))
        .expect("node c row written");
    storage.set_salience(c, 0.99).expect("dirty salience c");
    storage.set_salience(a, 0.01).expect("dirty salience a");
    println!("{ROW_MARKER}");
    std::io::stdout().flush().expect("marker flushed");

    loop {
        std::thread::park();
    }
}

#[test]
fn kill_dash_9_mid_write_preserves_reopenable_consistent_state() {
    if std::env::var(CHILD_ENV).is_ok() {
        child_main();
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph.db");

    let exe = std::env::current_exe().expect("current test binary");
    let mut child = Command::new(exe)
        .args([
            "kill_dash_9_mid_write_preserves_reopenable_consistent_state",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env(DB_ENV, &path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("child spawned");

    let stdout = child.stdout.take().expect("child stdout piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut saw_flush = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.contains(FLUSH_MARKER) {
                saw_flush = true;
            }
            if saw_flush && line.contains(ROW_MARKER) {
                let _ = tx.send(());
                return;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("child must reach the second durable seam (bounded event wait)");

    child.kill().expect("SIGKILL delivered");
    let status = child.wait().expect("child reaped");
    assert!(!status.success(), "killed child must not exit cleanly");

    // Reopen in the parent: the database must load and every durable fact
    // must be exactly the flushed one.
    let mut storage = SqliteStorage::open(&path).expect("reopen after kill");

    assert_eq!(
        storage.all_node_ids().len(),
        3,
        "all write-through rows load"
    );
    let node_a = storage.get_node(NodeId(0)).expect("node a loads");
    assert_eq!(node_a.name, "alpha");
    assert_eq!(
        storage.get_salience(NodeId(0)).expect("salience a loads"),
        0.77,
        "flushed hot field must survive the kill exactly"
    );
    let node_b = storage.get_node(NodeId(1)).expect("node b loads");
    assert_eq!(node_b.entity_tags, vec!["tag-beta".to_string()]);

    let node_c = storage.get_node(NodeId(2)).expect("node c row loads");
    assert_eq!(node_c.name, "gamma");
    assert_eq!(
        storage.get_salience(NodeId(2)).expect("salience c loads"),
        0.5,
        "node c keeps the eagerly persisted value; the unflushed dirty write \
         (0.99) must be lost, not resurrected"
    );

    let allocated = storage.next_node_id();
    assert_eq!(
        allocated,
        NodeId(3),
        "id allocation resumes after the highest live id with no free-list collision"
    );
}
