//! Reproduction test for the free-ID allocation seam.
//!
//! `next_node_id`/`next_edge_id` popped an id from the in-memory free list and
//! then deleted the durable `free_ids` row with `let _ =` — any failure (lock
//! contention, poisoned lock) was swallowed, leaving the id recorded free in
//! the table while a live node held it in RAM. On reopen, `load_free_ids`
//! resurrects the id, and `set_node`'s `INSERT OR REPLACE` silently overwrites
//! the live node.
//!
//! Pin: while the DELETE cannot commit, allocation must fall back to a fresh
//! counter id (never assign an id still recorded free); the queued id is
//! consumed once its DELETE can commit. The reopen case is the sharp edge:
//! the restored counter is `max(live)+1`, which can itself collide with the
//! freed id, so the fallback must skip ids still queued as free.

use std::collections::{HashMap, VecDeque};

use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
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
fn next_node_id_never_assigns_an_id_still_recorded_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph.db");

    let mut storage = SqliteStorage::open(&path).expect("open");
    let id0 = storage.next_node_id();
    storage.set_node(make_node(id0)).expect("node 0 stored");
    let id1 = storage.next_node_id();
    storage.set_node(make_node(id1)).expect("node 1 stored");
    storage.delete_node(id1).expect("node 1 freed");
    storage.flush().expect("flush");
    drop(storage);

    // Reopen so the free list is loaded from the durable free_ids table.
    let mut storage = SqliteStorage::open(&path).expect("reopen");

    // A second writer holds BEGIN IMMEDIATE, so the free-id DELETE cannot
    // commit (SQLITE_BUSY).
    let blocker = rusqlite::Connection::open(&path).expect("blocker connection");
    blocker
        .execute_batch("BEGIN IMMEDIATE;")
        .expect("write lock held");

    let assigned = storage.next_node_id();
    blocker.execute_batch("ROLLBACK;").expect("lock released");

    assert_eq!(
        assigned,
        NodeId(2),
        "while the free-id DELETE cannot commit, allocation must skip the still-\
         recorded-free id 1 (counter starts at 1 after reopen) and fall back to a \
         provably unused id; assigning id 1 here leaves it recorded free, and a \
         later reopen + INSERT OR REPLACE silently overwrites the live node"
    );

    let reused = storage.next_node_id();
    assert_eq!(
        reused, id1,
        "the freed id stays queued and is consumed once its DELETE commits"
    );
}

/// Happy path, no contention: after a reopen the counter restores as
/// `max(live)+1`, which can equal the popped free id. The pop branch must
/// advance the counter past any id it hands out, or the counter path later
/// re-issues the same now-live id and `INSERT OR REPLACE` silently overwrites
/// the node.
#[test]
fn popped_free_id_advances_the_counter_past_itself() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph.db");

    let mut storage = SqliteStorage::open(&path).expect("open");
    let id0 = storage.next_node_id();
    storage.set_node(make_node(id0)).expect("node 0 stored");
    let id1 = storage.next_node_id();
    storage.set_node(make_node(id1)).expect("node 1 stored");
    storage.delete_node(id1).expect("node 1 freed");
    storage.flush().expect("flush");
    drop(storage);

    let mut storage = SqliteStorage::open(&path).expect("reopen");
    let popped = storage.next_node_id();
    assert_eq!(popped, id1, "the free id is consumed uncontended");
    storage
        .set_node(make_node(popped))
        .expect("node recreated at the freed id");

    let next = storage.next_node_id();
    assert_eq!(
        next,
        NodeId(2),
        "the counter must not re-issue the just-popped id; re-issuing it lets \
         INSERT OR REPLACE silently overwrite the live node"
    );
}
