//! Reproduction test for the non-transactional `delete_node` seam.
//!
//! `delete_node` runs 8 SQL statements in autocommit (entity_tags, node_fts,
//! salience, accessed_at, decay_checkpoint, retained_action, nodes, free_ids
//! insert) while `flush`/`clone` wrap their writes in `BEGIN IMMEDIATE`.
//! A failure mid-sequence leaves earlier tables deleted and later ones intact:
//! the DB diverges from the in-memory state (which is only cleared on
//! success), and the next `flush` can write orphan hot-field rows for a node
//! whose base row was already deleted.
//!
//! Pin: a failure at the 4th statement must roll back statements 1-3, leaving
//! the pre-delete state fully intact.

use std::collections::{HashMap, VecDeque};

use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Node, NodeId, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};

fn tagged_node(id: NodeId) -> Node {
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
        entity_tags: vec!["alpha".to_string()],
        metadata: HashMap::new(),
    }
}

#[test]
fn delete_node_rolls_back_when_a_later_statement_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph.db");

    let mut storage = SqliteStorage::open(&path).expect("open");
    let id = storage.next_node_id();
    storage.set_node(tagged_node(id)).expect("node stored");
    storage.set_salience(id, 0.9).expect("salience written");
    storage.flush().expect("flush");

    let sabotage = rusqlite::Connection::open(&path).expect("sabotage connection");
    sabotage
        .execute_batch("BEGIN IMMEDIATE; DROP TABLE accessed_at; COMMIT;")
        .expect("accessed_at dropped");

    let result = storage.delete_node(id);
    assert!(result.is_err(), "delete_node must fail on the sabotaged table");

    let probe = rusqlite::Connection::open(&path).expect("probe connection");
    let tag_rows: i64 = probe
        .query_row(
            "SELECT COUNT(*) FROM entity_tags WHERE node_id = ?1",
            [id.0],
            |row| row.get(0),
        )
        .expect("entity_tags query");
    let salience_rows: i64 = probe
        .query_row(
            "SELECT COUNT(*) FROM salience WHERE node_id = ?1",
            [id.0],
            |row| row.get(0),
        )
        .expect("salience query");
    let node_rows: i64 = probe
        .query_row("SELECT COUNT(*) FROM nodes WHERE id = ?1", [id.0], |row| {
            row.get(0)
        })
        .expect("nodes query");

    assert_eq!(
        (tag_rows, salience_rows, node_rows),
        (1, 1, 1),
        "a mid-sequence failure must roll back every earlier statement; \
         (entity_tags, salience, nodes) rows must all survive"
    );
}
