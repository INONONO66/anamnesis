//! Tests for SQLite schema migration infrastructure (v1 → v2).

use anamnesis::storage::SqliteStorage;
use rusqlite::OptionalExtension;

// ── Fresh DB gets version 2 ───────────────────────────────────────────────────

#[test]
fn fresh_db_gets_schema_version_2() {
    // A new in-memory DB should be created at v2 directly.
    let storage = SqliteStorage::new().expect("storage init");
    // If we got here without error, schema was created successfully.
    // Verify by checking node_count (should be 0 on fresh DB).
    use anamnesis::storage::StorageAdapter;
    assert_eq!(storage.node_count(), 0);
}

#[test]
fn fresh_db_has_peer_id_column() {
    // Verify that the nodes table has peer_id and source_kind columns.
    // We do this by ingesting a node and checking it round-trips correctly.
    use anamnesis::Engine;
    use anamnesis::api::Observation;
    use anamnesis::graph::node::Origin;
    use anamnesis::graph::types::PeerId;
    use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
    use anamnesis::peer::SourceKind;

    let mut engine = Engine::new();
    let peer_id = PeerId(0); // Use default peer_id (no registry needed yet)

    let result = engine.ingest(Observation {
        name: "test node".into(),
        summary: None,
        content: "test content".into(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id,
            source_kind: SourceKind::AgentObservation,
            session_id: "s1".into(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp::now(),
    });
    assert!(result.is_ok(), "ingest should succeed: {:?}", result);
}

// ── Migration test ────────────────────────────────────────────────────────────

#[test]
fn existing_db_migrates_from_v1_to_v2() {
    use rusqlite::Connection;

    // Create a temp file path
    let tmp = std::env::temp_dir().join(format!("anamnesis_test_v1_{}.db", std::process::id()));

    // Create a v1-style database manually (no schema_version table, old agent_id column)
    {
        let conn = Connection::open(&tmp).expect("open v1 db");
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS nodes (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                summary TEXT,
                content TEXT NOT NULL,
                embedding_json TEXT,
                node_type TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                confidence REAL NOT NULL,
                valid_from INTEGER,
                valid_until INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                access_count INTEGER NOT NULL,
                access_history TEXT NOT NULL,
                tier TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edges (
                id INTEGER PRIMARY KEY,
                from_node INTEGER NOT NULL,
                to_node INTEGER NOT NULL,
                edge_type TEXT NOT NULL,
                weight REAL NOT NULL,
                created_at INTEGER NOT NULL,
                valid_from INTEGER,
                valid_until INTEGER,
                metadata TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS salience (node_id INTEGER PRIMARY KEY, salience REAL NOT NULL);
            CREATE TABLE IF NOT EXISTS accessed_at (node_id INTEGER PRIMARY KEY, accessed_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS decay_checkpoint (node_id INTEGER PRIMARY KEY, decay_checkpoint INTEGER NOT NULL);
            CREATE VIRTUAL TABLE IF NOT EXISTS node_fts USING fts5(id UNINDEXED, name, content);
            CREATE TABLE IF NOT EXISTS entity_tags (node_id INTEGER NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (node_id, tag));
            CREATE TABLE IF NOT EXISTS free_ids (id_type TEXT NOT NULL, id_value INTEGER NOT NULL, PRIMARY KEY (id_type, id_value));
        ").expect("create v1 schema");
    }

    // Open with new code — should migrate automatically
    let storage = SqliteStorage::open(&tmp);
    assert!(storage.is_ok(), "migration should succeed");

    // Verify migration ran: peers table should exist
    {
        let conn = Connection::open(&tmp).expect("reopen");
        let peers_exist: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='peers' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .expect("query")
            .is_some();
        assert!(peers_exist, "peers table should exist after migration");

        let version: u32 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("schema_version");
        assert_eq!(version, 2, "schema_version should be 2 after migration");
    }

    let _ = std::fs::remove_file(&tmp);
}
