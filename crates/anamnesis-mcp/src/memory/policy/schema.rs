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
        .map_err(|_| PolicyStoreError::operation("start policy schema transaction"))?;
    migrate(&transaction)?;
    transaction
        .commit()
        .map_err(|_| PolicyStoreError::operation("commit policy schema transaction"))
}

fn migrate(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(VERSION_TABLE_SQL)
        .map_err(|_| PolicyStoreError::operation("create policy schema version table"))?;

    let version = transaction
        .query_row(
            "SELECT version FROM mcp_schema_version WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|_| PolicyStoreError::operation("read policy schema version"))?;

    match version {
        None => {
            create_v1_schema(transaction)?;
            transaction
                .execute(
                    "INSERT INTO mcp_schema_version (id, version) VALUES (1, ?1)",
                    [SCHEMA_VERSION],
                )
                .map_err(|_| PolicyStoreError::operation("initialize policy schema version"))?;
        }
        Some(0) => {
            create_v1_schema(transaction)?;
            transaction
                .execute(
                    "UPDATE mcp_schema_version SET version = ?1 WHERE id = 1",
                    [SCHEMA_VERSION],
                )
                .map_err(|_| PolicyStoreError::operation("upgrade policy schema version"))?;
        }
        Some(SCHEMA_VERSION) => create_v1_schema(transaction)?,
        Some(version) => return Err(PolicyStoreError::UnsupportedVersion { version }),
    }

    Ok(())
}

fn create_v1_schema(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(RECALL_EVENTS_TABLE_SQL)
        .map_err(|_| PolicyStoreError::operation("create recall events table"))
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
