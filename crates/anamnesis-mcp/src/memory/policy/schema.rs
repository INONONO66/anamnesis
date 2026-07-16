use rusqlite::{Connection, OptionalExtension, Transaction};

use super::{PolicyStoreError, SCHEMA_VERSION};

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
        None => {
            create_v1_schema(transaction)?;
            transaction
                .execute(
                    "INSERT INTO mcp_schema_version (id, version) VALUES (1, ?1)",
                    [SCHEMA_VERSION],
                )
                .map_err(|error| {
                    PolicyStoreError::sqlite("initialize policy schema version", error)
                })?;
        }
        Some(0) => {
            create_v1_schema(transaction)?;
            transaction
                .execute(
                    "UPDATE mcp_schema_version SET version = ?1 WHERE id = 1",
                    [SCHEMA_VERSION],
                )
                .map_err(|error| {
                    PolicyStoreError::sqlite("upgrade policy schema version", error)
                })?;
        }
        Some(SCHEMA_VERSION) => create_v1_schema(transaction)?,
        Some(version) => return Err(PolicyStoreError::UnsupportedVersion { version }),
    }

    Ok(())
}

fn create_v1_schema(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(RECALL_EVENTS_TABLE_SQL)
        .map_err(|error| PolicyStoreError::sqlite("create recall events table", error))
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
    for table_name in ["mcp_schema_version", "recall_events"] {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table_name})"))
            .map_err(|_| PolicyStoreError::operation("prepare policy schema fingerprint"))?;
        let columns = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|_| PolicyStoreError::operation("read policy schema fingerprint"))?;
        fingerprint.push_str(table_name);
        fingerprint.push('\n');
        for column in columns {
            let (name, ty, not_null, default, primary_key) = column
                .map_err(|_| PolicyStoreError::operation("read policy schema fingerprint"))?;
            fingerprint.push_str(&format!(
                "{name}|{ty}|{not_null}|{default:?}|{primary_key}\n"
            ));
        }
    }
    Ok(fingerprint)
}
#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::{PolicyStoreError, SCHEMA_VERSION, initialize};

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
                INSERT INTO mcp_schema_version (id, version) VALUES (1, 2);
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
