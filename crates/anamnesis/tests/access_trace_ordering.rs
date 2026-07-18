//! Reproduction test for the `append_access_trace` ordering seam.
//!
//! The trace was appended to the in-memory bounded window BEFORE the
//! write-through UPDATE on the nodes row. When the UPDATE failed (lock
//! contention), the caller received `Err` but the trace was already applied —
//! a retry then appended it a second time, double-counting `B_i` (the ACT-R
//! base level recomputed from `access_history`).
//!
//! Pin: a failed UPDATE leaves memory and DB in the same pre-call state, and
//! a retry lands exactly one trace.

use std::collections::{HashMap, VecDeque};

use anamnesis::graph::node::Origin;
use anamnesis::graph::{AccessTrace, KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};

fn make_node(id: NodeId) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id.0),
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
        entity_tags: vec![],
        metadata: HashMap::new(),
    }
}

#[test]
fn failed_access_trace_write_leaves_no_trace_to_double_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph.db");

    let mut storage = SqliteStorage::open(&path).expect("open");
    let id = storage.next_node_id();
    storage.set_node(make_node(id)).expect("node stored");
    storage.flush().expect("flush");

    let trace = AccessTrace {
        at: Timestamp(1_000),
        decay: 0.5,
    };

    // A second writer holds BEGIN IMMEDIATE, so the write-through UPDATE fails.
    let blocker = rusqlite::Connection::open(&path).expect("blocker connection");
    blocker
        .execute_batch("BEGIN IMMEDIATE;")
        .expect("write lock held");

    let result = storage.append_access_trace(id, trace);
    assert!(result.is_err(), "UPDATE must fail while the lock is held");

    blocker.execute_batch("ROLLBACK;").expect("lock released");

    let history = storage.get_access_history(id).expect("history readable");
    assert_eq!(
        history.len(),
        0,
        "the failed call must not apply the trace in memory; otherwise a retry \
         double-counts B_i"
    );
    drop(history);

    storage
        .append_access_trace(id, trace)
        .expect("retry succeeds after the lock clears");
    let history = storage.get_access_history(id).expect("history readable");
    assert_eq!(
        history.len(),
        1,
        "the retry must land exactly one trace, not a duplicate of the failed call"
    );
}
