use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::extract::types::{
    CandidateKind, ExtractionSource, ExtractorProfileComponents, RelationKind, ValidatedExtraction,
};
use crate::proto::{ExtractionErrorKind, StageExtractionResult};

use super::PolicyStoreError;

const EXTRACTION_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS extractor_profiles (
        profile_id TEXT PRIMARY KEY,
        components TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('shadow', 'approved', 'revoked')),
        created_at INTEGER NOT NULL,
        approved_at INTEGER
    );

    CREATE TABLE IF NOT EXISTS extract_runs (
        id INTEGER PRIMARY KEY,
        at_ms INTEGER NOT NULL,
        profile_id TEXT NOT NULL,
        mode TEXT NOT NULL CHECK(mode = 'shadow'),
        turn_count INTEGER NOT NULL CHECK(turn_count >= 0),
        candidate_count INTEGER NOT NULL CHECK(candidate_count >= 0),
        relation_count INTEGER NOT NULL CHECK(relation_count >= 0),
        schema_valid INTEGER NOT NULL CHECK(schema_valid IN (0, 1)),
        llm_invoked INTEGER NOT NULL CHECK(llm_invoked IN (0, 1)),
        error_kind TEXT,
        duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0)
    );

    CREATE TABLE IF NOT EXISTS extract_run_sources (
        profile_id TEXT NOT NULL,
        turn_key TEXT NOT NULL,
        run_id INTEGER NOT NULL REFERENCES extract_runs(id),
        PRIMARY KEY (profile_id, turn_key)
    );

    CREATE TABLE IF NOT EXISTS extract_candidates (
        id INTEGER PRIMARY KEY,
        run_id INTEGER NOT NULL REFERENCES extract_runs(id),
        item_local_id TEXT NOT NULL,
        content TEXT NOT NULL,
        kind TEXT NOT NULL CHECK(kind IN ('decision', 'causal', 'lesson', 'convention', 'gotcha')),
        confidence REAL,
        source_turn_keys TEXT NOT NULL,
        source_session_id TEXT NOT NULL,
        source_scope TEXT NOT NULL,
        source_content_hashes TEXT NOT NULL,
        source_node_ids TEXT NOT NULL,
        idempotency_key TEXT NOT NULL UNIQUE,
        audit_support TEXT CHECK(audit_support IN ('supported', 'partial', 'unsupported')),
        contamination_category TEXT CHECK(contamination_category IN (
            'unsupported-claim', 'prompt-injection', 'secret-reexposure', 'foreign-scope', 'contradicts-source'
        )),
        reviewed_by TEXT,
        reviewed_at INTEGER,
        committed_node_id INTEGER
    );

    CREATE TABLE IF NOT EXISTS extract_relations (
        id INTEGER PRIMARY KEY,
        candidate_from INTEGER NOT NULL REFERENCES extract_candidates(id),
        candidate_to INTEGER NOT NULL REFERENCES extract_candidates(id),
        relation_type TEXT NOT NULL CHECK(relation_type IN ('reason', 'causal', 'contradicts', 'supports')),
        idempotency_key TEXT NOT NULL UNIQUE,
        audit_status TEXT CHECK(audit_status IN ('correct', 'wrong-type', 'wrong-direction', 'invalid')),
        reviewed_by TEXT,
        reviewed_at INTEGER,
        committed_edge_id INTEGER
    );
";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtractionProfileStatus {
    Shadow,
    Approved,
    Revoked,
}

pub(super) fn ensure_shadow_profile(
    connection: &Connection,
    profile_id: &str,
    components: &ExtractorProfileComponents,
    created_at: u64,
) -> Result<ExtractionProfileStatus, PolicyStoreError> {
    let components = serde_json::to_string(components)
        .map_err(|_| PolicyStoreError::operation("serialize extraction profile components"))?;
    let created_at = i64::try_from(created_at)
        .map_err(|_| PolicyStoreError::invalid_value("extraction profile created_at"))?;

    connection
        .execute(
            "INSERT OR IGNORE INTO extractor_profiles
             (profile_id, components, status, created_at, approved_at)
             VALUES (?1, ?2, 'shadow', ?3, NULL)",
            params![profile_id, components, created_at],
        )
        .map_err(|error| PolicyStoreError::sqlite("ensure shadow extraction profile", error))?;

    profile_status(connection, profile_id)
}

pub(super) fn processed_turn_keys(
    connection: &Connection,
    profile_id: &str,
) -> Result<HashSet<String>, PolicyStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT turn_key
             FROM extract_run_sources
             WHERE profile_id = ?1",
        )
        .map_err(|error| {
            PolicyStoreError::sqlite("prepare processed extraction turn keys", error)
        })?;
    let rows = statement
        .query_map([profile_id], |row| row.get::<_, String>(0))
        .map_err(|error| PolicyStoreError::sqlite("query processed extraction turn keys", error))?;

    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|error| PolicyStoreError::sqlite("read processed extraction turn key", error))
}

fn profile_status(
    connection: &Connection,
    profile_id: &str,
) -> Result<ExtractionProfileStatus, PolicyStoreError> {
    let status = connection
        .query_row(
            "SELECT status FROM extractor_profiles WHERE profile_id = ?1",
            [profile_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| PolicyStoreError::sqlite("read extraction profile status", error))?;

    match status.as_str() {
        "shadow" => Ok(ExtractionProfileStatus::Shadow),
        "approved" => Ok(ExtractionProfileStatus::Approved),
        "revoked" => Ok(ExtractionProfileStatus::Revoked),
        _ => Err(PolicyStoreError::operation(
            "read extraction profile status",
        )),
    }
}

pub(super) fn create_schema(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(EXTRACTION_TABLES_SQL)
        .map_err(|error| PolicyStoreError::sqlite("create extraction policy tables", error))
}
pub(super) fn stage(
    connection: &mut Connection,
    profile_id: &str,
    profile_components: &ExtractorProfileComponents,
    llm_duration_ms: u64,
    sources: &[ExtractionSource],
    extraction: &ValidatedExtraction,
) -> Result<StageExtractionResult, PolicyStoreError> {
    let duration_ms = sqlite_u64(llm_duration_ms, "extraction llm_duration_ms")?;
    let at_ms = now_ms("read extraction staging time", "extraction staging time")?;
    let candidate_count = sqlite_usize(extraction.items.len(), "extraction candidate_count")?;
    let relation_count = sqlite_usize(extraction.relations.len(), "extraction relation_count")?;
    let turn_count = sqlite_usize(sources.len(), "extraction turn_count")?;

    let transaction = connection
        .transaction()
        .map_err(|error| PolicyStoreError::sqlite("start extraction staging transaction", error))?;
    ensure_shadow_profile_in_transaction(&transaction, profile_id, profile_components, at_ms)?;

    if let Some(run_id) = exact_replay_run_id(&transaction, profile_id, sources)? {
        transaction.commit().map_err(|error| {
            PolicyStoreError::sqlite("commit extraction replay transaction", error)
        })?;
        return Ok(StageExtractionResult::AlreadyStaged { run_id });
    }
    if has_source_ledger_conflict(&transaction, profile_id, sources)? {
        return Err(PolicyStoreError::operation("source-ledger-conflict"));
    }

    transaction
        .execute(
            "INSERT INTO extract_runs
             (at_ms, profile_id, mode, turn_count, candidate_count, relation_count,
              schema_valid, llm_invoked, error_kind, duration_ms)
             VALUES (?1, ?2, 'shadow', ?3, ?4, ?5, 1, 1, NULL, ?6)",
            params![
                at_ms,
                profile_id,
                turn_count,
                candidate_count,
                relation_count,
                duration_ms
            ],
        )
        .map_err(|error| PolicyStoreError::sqlite("insert staged extraction run", error))?;
    let run_id_sql = transaction.last_insert_rowid();
    let run_id = sqlite_row_id(run_id_sql, "staged extraction run_id")?;

    let mut candidate_ids = HashMap::with_capacity(extraction.items.len());
    for candidate in &extraction.items {
        let source = candidate
            .sources
            .first()
            .and_then(|reference| {
                sources.iter().find(|source| {
                    source.node_id == reference.node_id
                        && source.turn_key == reference.turn_key
                        && source.content_hash == reference.content_hash
                })
            })
            .ok_or_else(|| PolicyStoreError::operation("resolve staged candidate source"))?;
        let source_turn_keys = serialize(
            candidate
                .sources
                .iter()
                .map(|source| &source.turn_key)
                .collect::<Vec<_>>(),
            "staged candidate source turn keys",
        )?;
        let source_content_hashes = serialize(
            candidate
                .sources
                .iter()
                .map(|source| &source.content_hash)
                .collect::<Vec<_>>(),
            "staged candidate source content hashes",
        )?;
        let source_node_ids = serialize(
            candidate
                .sources
                .iter()
                .map(|source| source.node_id)
                .collect::<Vec<_>>(),
            "staged candidate source node ids",
        )?;
        transaction
            .execute(
                "INSERT INTO extract_candidates
                 (run_id, item_local_id, content, kind, confidence, source_turn_keys,
                  source_session_id, source_scope, source_content_hashes, source_node_ids,
                  idempotency_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    run_id_sql,
                    candidate.item_local_id,
                    candidate.content,
                    candidate_kind(&candidate.kind),
                    candidate.confidence,
                    source_turn_keys,
                    source.session_id,
                    source.scope,
                    source_content_hashes,
                    source_node_ids,
                    candidate.idempotency_key,
                ],
            )
            .map_err(|error| {
                PolicyStoreError::sqlite("insert staged extraction candidate", error)
            })?;
        let candidate_id = transaction.last_insert_rowid();
        if candidate_ids
            .insert(candidate.item_local_id.as_str(), candidate_id)
            .is_some()
        {
            return Err(PolicyStoreError::operation(
                "duplicate staged candidate local identifier",
            ));
        }
    }

    for relation in &extraction.relations {
        let candidate_from = candidate_ids
            .get(relation.from_item_local_id.as_str())
            .copied()
            .ok_or_else(|| PolicyStoreError::operation("resolve staged relation source"))?;
        let candidate_to = candidate_ids
            .get(relation.to_item_local_id.as_str())
            .copied()
            .ok_or_else(|| PolicyStoreError::operation("resolve staged relation target"))?;
        transaction
            .execute(
                "INSERT INTO extract_relations
                 (candidate_from, candidate_to, relation_type, idempotency_key)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    candidate_from,
                    candidate_to,
                    relation_kind(&relation.relation_type),
                    relation.idempotency_key,
                ],
            )
            .map_err(|error| {
                PolicyStoreError::sqlite("insert staged extraction relation", error)
            })?;
    }

    for source in sources {
        transaction
            .execute(
                "INSERT INTO extract_run_sources (profile_id, turn_key, run_id)
                 VALUES (?1, ?2, ?3)",
                params![profile_id, source.turn_key, run_id_sql],
            )
            .map_err(|error| PolicyStoreError::sqlite("insert extraction source ledger", error))?;
    }

    transaction.commit().map_err(|error| {
        PolicyStoreError::sqlite("commit extraction staging transaction", error)
    })?;
    Ok(StageExtractionResult::Staged { run_id })
}

pub(super) fn record_failure(
    connection: &mut Connection,
    profile_id: &str,
    turn_count: u32,
    llm_invoked: bool,
    error_kind: ExtractionErrorKind,
    duration_ms: u64,
) -> Result<(), PolicyStoreError> {
    connection
        .execute(
            "INSERT INTO extract_runs
             (at_ms, profile_id, mode, turn_count, candidate_count, relation_count,
              schema_valid, llm_invoked, error_kind, duration_ms)
             VALUES (?1, ?2, 'shadow', ?3, 0, 0, 0, ?4, ?5, ?6)",
            params![
                now_ms("read extraction failure time", "extraction failure time")?,
                profile_id,
                i64::from(turn_count),
                llm_invoked,
                extraction_error_kind(&error_kind),
                sqlite_u64(duration_ms, "extraction failure duration_ms")?,
            ],
        )
        .map_err(|error| PolicyStoreError::sqlite("insert extraction failure run", error))?;
    Ok(())
}

fn ensure_shadow_profile_in_transaction(
    transaction: &Transaction<'_>,
    profile_id: &str,
    components: &ExtractorProfileComponents,
    created_at: i64,
) -> Result<(), PolicyStoreError> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO extractor_profiles
             (profile_id, components, status, created_at, approved_at)
             VALUES (?1, ?2, 'shadow', ?3, NULL)",
            params![
                profile_id,
                serialize(components, "serialize extraction profile components")?,
                created_at
            ],
        )
        .map_err(|error| PolicyStoreError::sqlite("ensure shadow extraction profile", error))?;
    Ok(())
}

fn exact_replay_run_id(
    transaction: &Transaction<'_>,
    profile_id: &str,
    sources: &[ExtractionSource],
) -> Result<Option<u64>, PolicyStoreError> {
    if sources.is_empty() {
        return Ok(None);
    }

    let mut run_id = None;
    let mut source_keys = HashSet::with_capacity(sources.len());
    for source in sources {
        if !source_keys.insert(source.turn_key.as_str()) {
            return Ok(None);
        }
        let ledger_run_id = transaction
            .query_row(
                "SELECT run_id FROM extract_run_sources
                 WHERE profile_id = ?1 AND turn_key = ?2",
                params![profile_id, source.turn_key],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| PolicyStoreError::sqlite("read extraction source ledger", error))?;
        let Some(ledger_run_id) = ledger_run_id else {
            return Ok(None);
        };
        let ledger_run_id = sqlite_row_id(ledger_run_id, "extraction replay run_id")?;
        match run_id {
            Some(existing) if existing != ledger_run_id => return Ok(None),
            Some(_) => {}
            None => run_id = Some(ledger_run_id),
        }
    }

    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let source_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM extract_run_sources WHERE profile_id = ?1 AND run_id = ?2",
            params![
                profile_id,
                i64::try_from(run_id)
                    .map_err(|_| PolicyStoreError::invalid_value("extraction replay run_id"))?
            ],
            |row| row.get(0),
        )
        .map_err(|error| PolicyStoreError::sqlite("count extraction replay sources", error))?;
    if source_count == sqlite_usize(sources.len(), "extraction replay source_count")? {
        Ok(Some(run_id))
    } else {
        Ok(None)
    }
}

fn has_source_ledger_conflict(
    transaction: &Transaction<'_>,
    profile_id: &str,
    sources: &[ExtractionSource],
) -> Result<bool, PolicyStoreError> {
    let mut source_keys = HashSet::with_capacity(sources.len());
    for source in sources {
        if !source_keys.insert(source.turn_key.as_str()) {
            return Ok(true);
        }
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM extract_run_sources
                    WHERE profile_id = ?1 AND turn_key = ?2
                )",
                params![profile_id, source.turn_key],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(|error| PolicyStoreError::sqlite("check extraction source ledger", error))?;
        if exists {
            return Ok(true);
        }
    }
    Ok(false)
}

fn now_ms(operation: &'static str, field: &'static str) -> Result<i64, PolicyStoreError> {
    let milliseconds: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PolicyStoreError::operation(operation))?
        .as_millis()
        .try_into()
        .map_err(|_| PolicyStoreError::invalid_value(field))?;
    sqlite_u64(milliseconds, field)
}

fn sqlite_u64(value: u64, field: &'static str) -> Result<i64, PolicyStoreError> {
    i64::try_from(value).map_err(|_| PolicyStoreError::invalid_value(field))
}

fn sqlite_usize(value: usize, field: &'static str) -> Result<i64, PolicyStoreError> {
    i64::try_from(value).map_err(|_| PolicyStoreError::invalid_value(field))
}

fn sqlite_row_id(value: i64, field: &'static str) -> Result<u64, PolicyStoreError> {
    u64::try_from(value).map_err(|_| PolicyStoreError::invalid_value(field))
}

fn serialize<T: serde::Serialize>(
    value: T,
    operation: &'static str,
) -> Result<String, PolicyStoreError> {
    serde_json::to_string(&value).map_err(|_| PolicyStoreError::operation(operation))
}

fn candidate_kind(kind: &CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Decision => "decision",
        CandidateKind::Causal => "causal",
        CandidateKind::Lesson => "lesson",
        CandidateKind::Convention => "convention",
        CandidateKind::Gotcha => "gotcha",
    }
}

fn relation_kind(kind: &RelationKind) -> &'static str {
    match kind {
        RelationKind::Reason => "reason",
        RelationKind::Causal => "causal",
        RelationKind::Contradicts => "contradicts",
        RelationKind::Supports => "supports",
    }
}

fn extraction_error_kind(kind: &ExtractionErrorKind) -> &'static str {
    match kind {
        ExtractionErrorKind::Spawn => "spawn",
        ExtractionErrorKind::Timeout => "timeout",
        ExtractionErrorKind::InvalidJson => "invalid-json",
        ExtractionErrorKind::SchemaReject => "schema-reject",
    }
}
