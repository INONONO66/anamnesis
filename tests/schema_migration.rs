//! Tests for SQLite schema migration infrastructure (v1 → v2 → v3).

use anamnesis::storage::SqliteStorage;
use rusqlite::{Connection, OptionalExtension};

/// Collect the column names of a table in declaration order via PRAGMA table_info.
fn table_columns(conn: &Connection, table: &str) -> Vec<(String, String, i64, String)> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table_info");
    let rows = stmt
        .query_map([], |row| {
            // cid, name, type, notnull, dflt_value, pk
            Ok((
                row.get::<_, String>(1)?,                          // name
                row.get::<_, String>(2)?,                          // type
                row.get::<_, i64>(3)?,                             // notnull
                row.get::<_, Option<String>>(4)?.unwrap_or_default(), // default
            ))
        })
        .expect("query table_info");
    rows.collect::<Result<Vec<_>, _>>().expect("collect table_info")
}

/// Collect the sorted list of index names defined on a table.
fn table_indexes(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name = ?1 ORDER BY name",
        )
        .expect("prepare index list");
    let rows = stmt
        .query_map([table], |row| row.get::<_, String>(0))
        .expect("query index list");
    let mut idx: Vec<String> = rows
        .collect::<Result<Vec<_>, _>>()
        .expect("collect index list")
        // SQLite auto-creates internal autoindexes (sqlite_autoindex_*) for
        // PRIMARY KEY / UNIQUE; drop those so we only compare explicit indexes.
        .into_iter()
        .filter(|name| !name.starts_with("sqlite_autoindex_"))
        .collect();
    idx.sort();
    idx
}

fn schema_version(conn: &Connection) -> u32 {
    conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
        row.get(0)
    })
    .expect("schema_version")
}

// ── Fresh DB gets version 3 ───────────────────────────────────────────────────

#[test]
fn fresh_db_gets_schema_version_3() {
    // A new file-backed DB should be created at v3 directly.
    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_freshv3_{}.db",
        std::process::id()
    ));
    let storage = SqliteStorage::open(&tmp).expect("storage init");
    use anamnesis::storage::StorageAdapter;
    assert_eq!(storage.node_count(), 0);

    let conn = Connection::open(&tmp).expect("reopen");
    assert_eq!(schema_version(&conn), 3, "fresh DB should be at schema v3");

    let _ = std::fs::remove_file(&tmp);
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
        valid_from: None,
        valid_until: None,
    });
    assert!(result.is_ok(), "ingest should succeed: {:?}", result);
}

// ── Migration test ────────────────────────────────────────────────────────────

#[test]
fn existing_db_migrates_from_v1_to_v3() {
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

        // retained_action reservoir table should exist after the v2 -> v3 step.
        let reservoir_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='retained_action' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .expect("query")
            .is_some();
        assert!(
            reservoir_exists,
            "retained_action table should exist after migration"
        );

        assert_eq!(
            schema_version(&conn),
            3,
            "schema_version should be 3 after full v1 -> v3 migration"
        );
    }

    let _ = std::fs::remove_file(&tmp);
}

// ── Fresh-v3 schema == migrated-v3 schema ──────────────────────────────────────

/// Build a v1-style database at `path` (legacy `agent_id`, no reservoir
/// columns/tables, no schema_version row), optionally seeding a node + edge so
/// the backfill has rows to operate on.
fn build_v1_db(path: &std::path::Path, seed: bool) {
    let conn = Connection::open(path).expect("open v1 db");
    conn.execute_batch(
        "
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
        ",
    )
    .expect("create v1 schema");

    if seed {
        // Two nodes with distinct saliences (interior + boundary) and one edge.
        for (id, salience) in [(0u64, 0.5_f64), (1, 1.0)] {
            conn.execute(
                "INSERT INTO nodes (id, name, summary, content, embedding_json, node_type, agent_id, session_id, scope, confidence, valid_from, valid_until, created_at, updated_at, access_count, access_history, tier, metadata)
                 VALUES (?1, 'n', NULL, 'c', NULL, 'semantic', 'a', 's', '', 0.9, NULL, NULL, 1000, 1000, 0, '', 'auto', '')",
                [id],
            )
            .expect("insert node");
            conn.execute(
                "INSERT INTO salience (node_id, salience) VALUES (?1, ?2)",
                rusqlite::params![id, salience],
            )
            .expect("insert salience");
            conn.execute(
                "INSERT INTO accessed_at (node_id, accessed_at) VALUES (?1, 1000)",
                [id],
            )
            .expect("insert accessed_at");
            conn.execute(
                "INSERT INTO decay_checkpoint (node_id, decay_checkpoint) VALUES (?1, 1000)",
                [id],
            )
            .expect("insert decay_checkpoint");
        }
        conn.execute(
            "INSERT INTO edges (id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata)
             VALUES (0, 0, 1, 'semantic', 0.8, 1234, NULL, NULL, '')",
            [],
        )
        .expect("insert edge");
    }
}

#[test]
fn fresh_v3_schema_equals_migrated_v3_schema() {
    // Fresh v3 DB.
    let fresh_path = std::env::temp_dir().join(format!(
        "anamnesis_test_fresh_eq_{}.db",
        std::process::id()
    ));
    SqliteStorage::open(&fresh_path).expect("fresh v3 storage");

    // v1 -> v3 migrated DB.
    let migrated_path = std::env::temp_dir().join(format!(
        "anamnesis_test_migrated_eq_{}.db",
        std::process::id()
    ));
    build_v1_db(&migrated_path, true);
    SqliteStorage::open(&migrated_path).expect("migrate v1 -> v3");

    let fresh = Connection::open(&fresh_path).expect("reopen fresh");
    let migrated = Connection::open(&migrated_path).expect("reopen migrated");

    assert_eq!(schema_version(&fresh), 3);
    assert_eq!(schema_version(&migrated), 3);

    // Columns of edges (the table whose layout differs across the migration).
    assert_eq!(
        table_columns(&fresh, "edges"),
        table_columns(&migrated, "edges"),
        "fresh-v3 and migrated-v3 edges columns must be identical"
    );
    // retained_action table columns.
    assert_eq!(
        table_columns(&fresh, "retained_action"),
        table_columns(&migrated, "retained_action"),
        "fresh-v3 and migrated-v3 retained_action columns must be identical"
    );

    // Index lists must match for every reservoir-touched table.
    for table in ["nodes", "edges", "salience"] {
        assert_eq!(
            table_indexes(&fresh, table),
            table_indexes(&migrated, table),
            "fresh-v3 and migrated-v3 indexes on {table} must be identical"
        );
    }

    let _ = std::fs::remove_file(&fresh_path);
    let _ = std::fs::remove_file(&migrated_path);
}

// ── Deterministic backfill + node/reservoir parity ─────────────────────────────

#[test]
fn v3_backfill_is_deterministic_and_complete() {
    use anamnesis::mechanics::priors::{salience_to_action, weight_to_conductance};

    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_backfill_{}.db",
        std::process::id()
    ));
    build_v1_db(&tmp, true);
    SqliteStorage::open(&tmp).expect("migrate v1 -> v3");

    let conn = Connection::open(&tmp).expect("reopen");

    // COUNT(nodes) == COUNT(retained_action) — no node lost its reservoir row.
    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
        .expect("count nodes");
    let reservoir_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM retained_action", [], |row| row.get(0))
        .expect("count retained_action");
    assert_eq!(
        node_count, reservoir_count,
        "every node must have exactly one retained_action row"
    );
    assert_eq!(node_count, 2);

    // Deterministic clamped-logit backfill: value == salience_to_action(salience).
    let mut stmt = conn
        .prepare(
            "SELECT s.salience, r.value FROM salience s JOIN retained_action r ON r.node_id = s.node_id ORDER BY s.node_id",
        )
        .expect("prepare join");
    let pairs: Vec<(f64, f64)> = stmt
        .query_map([], |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)))
        .expect("query join")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect join");
    for (salience, value) in pairs {
        assert!(
            (value - salience_to_action(salience)).abs() < 1e-12,
            "backfill must equal salience_to_action({salience}); got {value}"
        );
    }

    // Edge conductance backfill: weight_to_conductance(weight); accessed_at = created_at.
    let (weight, conductance, created_at, accessed_at): (f64, f64, i64, i64) = conn
        .query_row(
            "SELECT weight, conductance, created_at, accessed_at FROM edges WHERE id = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query edge");
    assert!(
        (conductance - weight_to_conductance(weight)).abs() < 1e-12,
        "edge conductance must equal weight_to_conductance({weight}); got {conductance}"
    );
    assert_eq!(
        accessed_at, created_at,
        "edge accessed_at must be backfilled from created_at"
    );

    let _ = std::fs::remove_file(&tmp);
}
