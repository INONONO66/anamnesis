use rusqlite::{Connection, OptionalExtension, Transaction};

use super::{PolicyStoreError, SCHEMA_VERSION, extraction};

const VERSION_TABLE_SQL: &str = "
    CREATE TABLE IF NOT EXISTS mcp_schema_version (
        id INTEGER PRIMARY KEY CHECK(id = 1),
        version INTEGER NOT NULL
    );
";

const RECALL_EVENTS_TABLE_SQL: &str = "
    CREATE TABLE IF NOT EXISTS recall_events (
        id INTEGER PRIMARY KEY,
        at_ms INTEGER NOT NULL,
        namespace TEXT NOT NULL,
        event_kind TEXT NOT NULL,
        query_chars INTEGER NOT NULL,
        scope TEXT,
        knowledge_only INTEGER NOT NULL CHECK(knowledge_only IN (0, 1)),
        has_hits INTEGER NOT NULL CHECK(has_hits IN (0, 1)),
        readout_pass INTEGER NOT NULL CHECK(readout_pass IN (0, 1)),
        cosine_pass INTEGER NOT NULL CHECK(cosine_pass IN (0, 1)),
        eligible INTEGER NOT NULL CHECK(eligible IN (0, 1)),
        top_score REAL,
        top_cosine REAL,
        gate_threshold REAL,
        cosine_gate REAL,
        result_node_ids TEXT NOT NULL,
        auto_extract_node_count INTEGER NOT NULL
    );
";

pub(super) fn initialize(connection: &mut Connection) -> Result<(), PolicyStoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| PolicyStoreError::sqlite("start policy schema transaction", error))?;
    migrate(&transaction)?;
    transaction
        .commit()
        .map_err(|error| PolicyStoreError::sqlite("commit policy schema transaction", error))
}

fn migrate(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(VERSION_TABLE_SQL)
        .map_err(|error| PolicyStoreError::sqlite("create policy schema version table", error))?;

    let version = transaction
        .query_row(
            "SELECT version FROM mcp_schema_version WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| PolicyStoreError::sqlite("read policy schema version", error))?;

    match version {
        None | Some(0) => {
            create_v1_schema(transaction)?;
            extraction::create_schema(transaction)?;
            set_version(transaction)?;
        }
        Some(1) => {
            create_v1_schema(transaction)?;
            extraction::create_schema(transaction)?;
            set_version(transaction)?;
        }
        Some(2) => {
            create_v1_schema(transaction)?;
            extraction::migrate_v3(transaction)?;
            set_version(transaction)?;
        }
        Some(3) => {
            create_v1_schema(transaction)?;
            extraction::migrate_v4(transaction)?;
            extraction::migrate_v5(transaction)?;
            set_version(transaction)?;
        }
        Some(4) => {
            create_v1_schema(transaction)?;
            extraction::migrate_v5(transaction)?;
            set_version(transaction)?;
        }
        Some(SCHEMA_VERSION) => {
            create_v1_schema(transaction)?;
            extraction::create_schema(transaction)?;
        }
        Some(version) => return Err(PolicyStoreError::UnsupportedVersion { version }),
    }

    Ok(())
}

fn create_v1_schema(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(RECALL_EVENTS_TABLE_SQL)
        .map_err(|error| PolicyStoreError::sqlite("create recall events table", error))
}
fn set_version(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute(
            "INSERT INTO mcp_schema_version (id, version) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET version = excluded.version",
            [SCHEMA_VERSION],
        )
        .map(|_| ())
        .map_err(|error| PolicyStoreError::sqlite("update policy schema version", error))
}

#[cfg(test)]
pub(super) fn schema_version(connection: &Connection) -> Result<i64, PolicyStoreError> {
    connection
        .query_row(
            "SELECT version FROM mcp_schema_version WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|_| PolicyStoreError::operation("read policy schema version"))
}

#[cfg(test)]
pub(super) fn schema_fingerprint(connection: &Connection) -> Result<String, PolicyStoreError> {
    let mut fingerprint = String::new();
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, sql
             FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .map_err(|_| PolicyStoreError::operation("prepare policy schema fingerprint"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|_| PolicyStoreError::operation("read policy schema fingerprint"))?;
    for row in rows {
        let (object_type, name, table_name, sql) =
            row.map_err(|_| PolicyStoreError::operation("read policy schema fingerprint"))?;
        fingerprint.push_str(&format!(
            "{object_type}|{name}|{table_name}|{}\n",
            normalize_schema_sql(sql.as_deref().unwrap_or_default())
        ));
    }
    Ok(fingerprint)
}

#[cfg(test)]
fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}
#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::{PolicyStoreError, SCHEMA_VERSION, initialize, schema_version};

    #[test]
    fn schema_rejects_invalid_singleton_and_boolean_values() {
        let mut connection = Connection::open_in_memory().expect("open policy database");
        initialize(&mut connection).expect("initialize policy schema");

        assert!(
            connection
                .execute(
                    "INSERT INTO mcp_schema_version (id, version) VALUES (2, 1)",
                    [],
                )
                .is_err(),
            "the version table must permit only id = 1"
        );

        for (field, values) in [
            ("knowledge_only", [2, 0, 0, 0, 0]),
            ("has_hits", [0, 2, 0, 0, 0]),
            ("readout_pass", [0, 0, 2, 0, 0]),
            ("cosine_pass", [0, 0, 0, 2, 0]),
            ("eligible", [0, 0, 0, 0, 2]),
        ] {
            assert!(
                connection
                    .execute(
                        "INSERT INTO recall_events (
                            id, at_ms, namespace, event_kind, query_chars,
                            knowledge_only, has_hits, readout_pass, cosine_pass, eligible,
                            result_node_ids, auto_extract_node_count
                        ) VALUES (?1, 0, 'test', 'tool', 0, ?2, ?3, ?4, ?5, ?6, '[]', 0)",
                        params![1, values[0], values[1], values[2], values[3], values[4]],
                    )
                    .is_err(),
                "{field} must accept only 0 or 1"
            );
        }
    }

    #[test]
    fn unsupported_future_schema_initialization_preserves_schema_and_data() {
        let mut connection = Connection::open_in_memory().expect("open policy database");
        connection
            .execute_batch(
                "
                CREATE TABLE mcp_schema_version (
                    id INTEGER PRIMARY KEY CHECK(id = 1),
                    version INTEGER NOT NULL
                );
                INSERT INTO mcp_schema_version (id, version) VALUES (1, 6);
                CREATE TABLE retained_metadata (id INTEGER PRIMARY KEY, marker INTEGER NOT NULL);
                INSERT INTO retained_metadata (id, marker) VALUES (1, 7);
                ",
            )
            .expect("seed future policy schema");

        let schema_before = schema_snapshot(&connection);
        let marker_before: i64 = connection
            .query_row(
                "SELECT marker FROM retained_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("read retained marker");
        let version_before: i64 = connection
            .query_row(
                "SELECT version FROM mcp_schema_version WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("read future schema version");

        assert_eq!(
            initialize(&mut connection),
            Err(PolicyStoreError::UnsupportedVersion {
                version: SCHEMA_VERSION + 1
            })
        );

        assert_eq!(schema_snapshot(&connection), schema_before);
        assert_eq!(
            connection
                .query_row(
                    "SELECT marker FROM retained_metadata WHERE id = 1",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .expect("read retained marker after failed initialization"),
            marker_before
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT version FROM mcp_schema_version WHERE id = 1",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .expect("read future schema version after failed initialization"),
            version_before
        );
    }

    #[test]
    fn v2_extraction_rows_migrate_to_generic_temporal_schema() {
        let mut connection = Connection::open_in_memory().expect("open policy database");
        initialize(&mut connection).expect("initialize current schema");
        connection
            .execute(
                "INSERT INTO extractor_profiles
                 (profile_id, components, status, created_at, approved_at)
                 VALUES ('profile', '{}', 'shadow', 1, NULL)",
                [],
            )
            .expect("seed profile");
        connection
            .execute(
                "INSERT INTO extract_runs
                 (id, at_ms, profile_id, mode, turn_count, candidate_count, relation_count,
                  schema_valid, llm_invoked, error_kind, duration_ms)
                 VALUES (1, 1, 'profile', 'shadow', 1, 1, 0, 1, 1, NULL, 1)",
                [],
            )
            .expect("seed run");
        connection
            .execute(
                "INSERT INTO extract_candidates
                 (id, run_id, item_local_id, content, kind, confidence, entity_tags,
                  valid_from_ms, valid_until_ms, source_turn_keys, source_session_id,
                  source_scope, source_content_hashes, source_node_ids, idempotency_key)
                 VALUES (1, 1, 'item', 'content', 'lesson', 0.9, '[]',
                         NULL, NULL, '[\"turn\"]', 'session', '', '[\"hash\"]', '[7]', 'key')",
                [],
            )
            .expect("seed candidate");
        connection
            .execute("UPDATE mcp_schema_version SET version = 2 WHERE id = 1", [])
            .expect("simulate v2 marker");

        initialize(&mut connection).expect("migrate v2 schema");

        assert_eq!(
            connection
                .query_row(
                    "SELECT version FROM mcp_schema_version WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("schema version"),
            SCHEMA_VERSION
        );
        let row = connection
            .query_row(
                "SELECT content, entity_tags, valid_from_ms, valid_until_ms
                 FROM extract_candidates WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .expect("migrated candidate");
        assert_eq!(row, ("content".to_owned(), "[]".to_owned(), None, None));
        connection
            .execute(
                "INSERT INTO extract_candidates
                 (run_id, item_local_id, content, kind, confidence, entity_tags,
                  source_turn_keys, source_session_id, source_scope,
                  source_content_hashes, source_node_ids, idempotency_key)
                 VALUES (1, 'fact', 'fact', 'fact', 0.9, '[\"Alice\"]',
                         '[\"turn\"]', 'session', '', '[\"hash\"]', '[7]', 'fact-key')",
                [],
            )
            .expect("generic fact kind accepted after migration");
    }

    #[test]
    fn v3_extraction_rows_gain_grounding_ranges_without_copying_source_text() {
        let mut connection = Connection::open_in_memory().expect("open policy database");
        connection
            .execute_batch(
                "
                CREATE TABLE mcp_schema_version (
                    id INTEGER PRIMARY KEY CHECK(id = 1),
                    version INTEGER NOT NULL
                );
                INSERT INTO mcp_schema_version (id, version) VALUES (1, 3);
                CREATE TABLE extract_candidates (
                    id INTEGER PRIMARY KEY,
                    content TEXT NOT NULL
                );
                INSERT INTO extract_candidates (id, content) VALUES (1, 'legacy claim');
                ",
            )
            .expect("seed v3 extraction row");

        initialize(&mut connection).expect("migrate v3 schema");

        let columns: Vec<String> = connection
            .prepare(
                "SELECT name FROM pragma_table_info('extract_candidates')
                 WHERE name LIKE 'ground_%' OR name LIKE 'evidence_%'
                 ORDER BY name",
            )
            .expect("inspect grounded columns")
            .query_map([], |row| row.get(0))
            .expect("query grounded columns")
            .collect::<Result<_, _>>()
            .expect("read grounded columns");
        assert_eq!(
            columns,
            [
                "evidence_object",
                "evidence_source_node_id",
                "evidence_span_end",
                "evidence_span_sha256",
                "evidence_span_start",
                "ground_object",
                "ground_relation",
                "ground_subject",
            ]
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT content FROM extract_candidates WHERE id = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("read migrated claim"),
            "legacy claim"
        );
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn v4_extraction_rows_gain_fail_closed_source_incarnation_bindings() {
        let mut connection = Connection::open_in_memory().expect("open policy database");
        initialize(&mut connection).expect("initialize current schema");
        connection
            .execute_batch(
                "
                INSERT INTO extractor_profiles
                    (profile_id, components, status, created_at, approved_at)
                VALUES ('profile', '{}', 'shadow', 1, NULL);
                INSERT INTO extract_runs
                    (id, at_ms, profile_id, mode, turn_count, candidate_count,
                     relation_count, schema_valid, llm_invoked, error_kind, duration_ms)
                VALUES (1, 1, 'profile', 'shadow', 1, 1, 0, 1, 1, NULL, 1);
                ",
            )
            .expect("seed legacy run");
        connection
            .execute(
                "INSERT INTO extract_candidates
                 (id, run_id, item_local_id, content, kind, source_turn_keys,
                  source_session_id, source_scope, source_content_hashes,
                  source_node_ids, idempotency_key)
                 VALUES (1, 1, 'legacy', 'legacy claim', 'fact', '[\"turn\"]',
                         'session', '', '[\"hash\"]', '[7]', 'legacy-key')",
                [],
            )
            .expect("seed legacy candidate");
        connection
            .execute_batch(
                "
                ALTER TABLE extract_candidates DROP COLUMN source_incarnations;
                UPDATE mcp_schema_version SET version = 4 WHERE id = 1;
                ",
            )
            .expect("simulate v4 policy schema");

        initialize(&mut connection).expect("migrate v4 schema");

        assert_eq!(
            connection
                .query_row(
                    "SELECT source_incarnations FROM extract_candidates WHERE id = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("read migrated source binding"),
            "[]"
        );
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
    }

    fn schema_snapshot(connection: &Connection) -> Vec<(String, String, String, Option<String>)> {
        let mut statement = connection
            .prepare(
                "SELECT type, name, tbl_name, sql
                 FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )
            .expect("prepare schema snapshot");
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query schema snapshot")
            .collect::<Result<Vec<_>, _>>()
            .expect("read schema snapshot")
    }
}
