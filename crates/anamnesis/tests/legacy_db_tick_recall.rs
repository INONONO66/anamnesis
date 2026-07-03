//! Bug #1 end-to-end (disk-path smoke — migration policy point 4, "smoke a real
//! pre-upgrade database"): a legacy database carrying an empty-`access_history`
//! row, once migrated on open, must `tick()` WITHOUT dying, and the migrated node
//! must remain recall-able (finite, bounded salience).
//!
//! Before the fix, `compute_base_level` returned `NEG_INFINITY` for the trace-less
//! row, so the first `tick()` (which MCP recall calls every cycle) returned
//! `Err(NonFinite)` and aborted the whole batch — bricking recall for the entire
//! session after an upgrade. The fix pairs a v8->v9 creation-trace backfill
//! (`migration_backfill.rs`) with a defensive tick finite-guard
//! (`tick_finite_guard.rs`); this test proves the two together on the real on-disk
//! open->tick->retrieve path.

use anamnesis::Engine;
use anamnesis::engine::{EngineConfig, SourceKind, StorageAdapter};
use anamnesis::graph::node::{Node, Origin};
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, MemoryTier, ScopePath, Timestamp};
use anamnesis::storage::SqliteStorage;
use rusqlite::Connection;
use std::collections::{HashMap, VecDeque};

#[test]
fn migrated_legacy_empty_history_db_ticks_and_stays_recallable() {
    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_legacy_tick_recall_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    // 1. Plant a legacy node whose `access_history` is empty — the on-disk shape
    //    a pre-ACT-R release left behind.
    let node_id = {
        let mut storage = SqliteStorage::open(&tmp).expect("open fresh db");
        let id = storage.next_node_id();
        let node = Node {
            id,
            node_type: KnowledgeType::Semantic,
            name: "legacy decision".into(),
            summary: None,
            content: "we chose sqlite for the single-node deploy".into(),
            embedding: None,
            created_at: Timestamp(1_000_000),
            updated_at: Timestamp(1_000_000),
            accessed_at: Timestamp(1_000_000),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(), // the legacy defect
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: PeerId(0),
                source_kind: SourceKind::AgentObservation,
                session_id: "s1".into(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: vec!["sqlite".into()],
            metadata: HashMap::new(),
        };
        storage.set_node(node).expect("plant legacy node");
        storage.flush().expect("flush");
        id
    };

    // 2. Rewind the recorded version so reopening runs the v8->v9 creation-trace
    //    backfill against the on-disk empty-history row.
    {
        let conn = Connection::open(&tmp).expect("raw conn");
        conn.execute_batch("UPDATE schema_version SET version = 8;")
            .expect("rewind to v8");
    }

    // 3. Reopen (migrates + backfills), build an engine, and TICK — the exact
    //    call MCP recall makes every cycle. Pre-fix this returned Err(NonFinite)
    //    on the trace-less node and killed recall; it must now be Ok.
    let storage = SqliteStorage::open(&tmp).expect("reopen migrates");
    let mut engine = Engine::with_storage(EngineConfig::new(), storage);
    let report = engine.tick(Timestamp(1_000_000 + 86_400_000)); // +1 day
    assert!(
        report.is_ok(),
        "tick on a migrated legacy empty-history DB must not die: {report:?}"
    );

    // 4. The migrated node survived with a finite, bounded salience, so a recall
    //    over the graph can surface it (pre-fix, nothing surfaced at all because
    //    the whole tick aborted).
    let history = engine
        .graph()
        .storage()
        .get_access_history(node_id)
        .expect("node present after migration");
    assert_eq!(
        history.len(),
        1,
        "the v8->v9 backfill must have seeded exactly one creation trace"
    );
    let salience = engine
        .graph()
        .storage()
        .get_salience(node_id)
        .expect("salience present");
    assert!(
        salience.is_finite() && (0.0..=1.0).contains(&salience),
        "migrated legacy node must carry a finite, bounded salience, got {salience}"
    );

    let _ = std::fs::remove_file(&tmp);
}
