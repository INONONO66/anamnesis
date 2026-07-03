//! Bug #1a: legacy rows can carry an empty `access_history` (`decode_access_history("")`
//! decodes to an empty `VecDeque`), which makes `compute_base_level` return
//! `NEG_INFINITY` downstream. The v8 -> v9 migration hop backfills exactly one
//! creation `AccessTrace` — stamped at the row's `created_at`, at the same
//! floor decay (`m_type * DECAY_INTERCEPT`) used at ingest — for every node whose
//! `access_history` is empty.

use anamnesis::engine::{SourceKind, StorageAdapter};
use anamnesis::graph::node::{Node, Origin};
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{AccessTrace, KnowledgeType, MemoryTier, ScopePath, Timestamp};
use anamnesis::mechanics::priors::{DECAY_INTERCEPT, decay_multiplier_for_type};
use anamnesis::storage::SqliteStorage;
use rusqlite::Connection;
use std::collections::{HashMap, VecDeque};

#[test]
fn v9_backfills_creation_trace_for_legacy_empty_access_history() {
    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_backfill_creation_trace_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    // 1. Plant a node whose `access_history` is empty — the shape a legacy
    //    pre-ACT-R row left on disk.
    let node_id = {
        let mut storage = SqliteStorage::open(&tmp).expect("open fresh db");
        let id = storage.next_node_id();
        let node = Node {
            id,
            node_type: KnowledgeType::Semantic,
            name: "legacy row".into(),
            summary: None,
            content: "a node persisted before access-trace history existed".into(),
            embedding: None,
            created_at: Timestamp(42_000),
            updated_at: Timestamp(42_000),
            accessed_at: Timestamp(42_000),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(), // empty — the legacy defect
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: PeerId(0),
                source_kind: SourceKind::AgentObservation,
                session_id: "s1".into(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: vec![],
            metadata: HashMap::new(),
        };
        storage
            .set_node(node)
            .expect("plant legacy empty-history node");
        storage.flush().expect("flush");
        id
    };

    // 2. Rewind `schema_version` to 8 (pre-backfill) so reopening runs the
    //    v8 -> v9 hop; the on-disk row already carries `access_history = ''`.
    {
        let conn = Connection::open(&tmp).expect("raw conn opens");
        conn.execute_batch("UPDATE schema_version SET version = 8;")
            .expect("rewind schema_version to 8");
    }

    // 3. Reopen: the v8 -> v9 migration must backfill exactly one creation
    //    trace, stamped at the row's created_at, at the ingest floor decay.
    let reopened = SqliteStorage::open(&tmp).expect("reopen runs v8->v9 backfill");
    let history = reopened
        .get_access_history(node_id)
        .expect("history accessible");
    assert_eq!(
        history.len(),
        1,
        "legacy empty-history row must be backfilled with exactly one creation trace"
    );
    let trace = history.front().expect("one trace");
    assert_eq!(
        trace.at,
        Timestamp(42_000),
        "creation trace must be stamped at the row's created_at"
    );
    let expected_decay = decay_multiplier_for_type(&KnowledgeType::Semantic) * DECAY_INTERCEPT;
    assert!(
        (trace.decay - expected_decay).abs() < 1e-12,
        "backfilled trace must use the ingest floor decay m_type*alpha; got {}, expected {}",
        trace.decay,
        expected_decay
    );

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn v9_backfill_is_idempotent_and_leaves_populated_history_untouched() {
    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_backfill_idempotent_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    // A node that already has a real access trace must survive the backfill
    // hop completely unchanged (the selector only ever matches empty rows).
    let node_id = {
        let mut storage = SqliteStorage::open(&tmp).expect("open fresh db");
        let id = storage.next_node_id();
        let mut history = VecDeque::new();
        history.push_back(AccessTrace {
            at: Timestamp(7_000),
            decay: 0.99,
        });
        let node = Node {
            id,
            node_type: KnowledgeType::Episodic,
            name: "already-seeded row".into(),
            summary: None,
            content: "a node that already carries a real access trace".into(),
            embedding: None,
            created_at: Timestamp(7_000),
            updated_at: Timestamp(7_000),
            accessed_at: Timestamp(7_000),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 1,
            access_history: history,
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: PeerId(0),
                source_kind: SourceKind::AgentObservation,
                session_id: "s1".into(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: vec![],
            metadata: HashMap::new(),
        };
        storage.set_node(node).expect("plant already-seeded node");
        storage.flush().expect("flush");
        id
    };

    {
        let conn = Connection::open(&tmp).expect("raw conn opens");
        conn.execute_batch("UPDATE schema_version SET version = 8;")
            .expect("rewind schema_version to 8");
    }

    let reopened = SqliteStorage::open(&tmp).expect("reopen runs v8->v9 backfill");
    let history = reopened
        .get_access_history(node_id)
        .expect("history accessible");
    assert_eq!(
        history.len(),
        1,
        "a row with real history must not gain a spurious second trace"
    );
    let trace = history.front().expect("one trace");
    assert_eq!(trace.at, Timestamp(7_000), "original trace must survive");
    assert!(
        (trace.decay - 0.99).abs() < 1e-12,
        "original decay must survive untouched, got {}",
        trace.decay
    );

    let _ = std::fs::remove_file(&tmp);
}
