use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use crate::extract::audit::{
    ExtractionAuditCandidateRow, ExtractionAuditRelationRow, ExtractionAuditResult,
};
use crate::extract::types::{
    AuditSupport, CandidateKind, ContaminationCategory, ExtractionSource,
    ExtractorProfileComponents, RelationKind, RelationVerdict, ValidatedExtraction,
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
        kind TEXT NOT NULL CHECK(kind IN (
            'fact', 'entity', 'event', 'preference',
            'decision', 'causal', 'lesson', 'convention', 'gotcha'
        )),
        confidence REAL,
        ground_subject TEXT,
        ground_relation TEXT,
        ground_object TEXT,
        evidence_object TEXT,
        evidence_span_start INTEGER,
        evidence_span_end INTEGER,
        evidence_span_sha256 TEXT,
        evidence_source_node_id INTEGER,
        entity_tags TEXT NOT NULL DEFAULT '[]',
        valid_from_ms INTEGER,
        valid_until_ms INTEGER,
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

pub(super) fn migrate_v3(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(
            "
            ALTER TABLE extract_candidates RENAME TO extract_candidates_v2;
            ALTER TABLE extract_relations RENAME TO extract_relations_v2;
            ",
        )
        .map_err(|error| PolicyStoreError::sqlite("rename v2 extraction tables", error))?;
    create_schema(transaction)?;
    transaction
        .execute_batch(
            "
            INSERT INTO extract_candidates (
                id, run_id, item_local_id, content, kind, confidence,
                entity_tags, valid_from_ms, valid_until_ms,
                source_turn_keys, source_session_id, source_scope,
                source_content_hashes, source_node_ids, idempotency_key,
                audit_support, contamination_category, reviewed_by, reviewed_at,
                committed_node_id
            )
            SELECT
                id, run_id, item_local_id, content, kind, confidence,
                '[]', NULL, NULL,
                source_turn_keys, source_session_id, source_scope,
                source_content_hashes, source_node_ids, idempotency_key,
                audit_support, contamination_category, reviewed_by, reviewed_at,
                committed_node_id
            FROM extract_candidates_v2;

            INSERT INTO extract_relations (
                id, candidate_from, candidate_to, relation_type, idempotency_key,
                audit_status, reviewed_by, reviewed_at, committed_edge_id
            )
            SELECT
                id, candidate_from, candidate_to, relation_type, idempotency_key,
                audit_status, reviewed_by, reviewed_at, committed_edge_id
            FROM extract_relations_v2;

            DROP TABLE extract_relations_v2;
            DROP TABLE extract_candidates_v2;
            ",
        )
        .map_err(|error| PolicyStoreError::sqlite("migrate v2 extraction rows", error))
}

pub(super) fn migrate_v4(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(
            "
            ALTER TABLE extract_candidates ADD COLUMN ground_subject TEXT;
            ALTER TABLE extract_candidates ADD COLUMN ground_relation TEXT;
            ALTER TABLE extract_candidates ADD COLUMN ground_object TEXT;
            ALTER TABLE extract_candidates ADD COLUMN evidence_object TEXT;
            ALTER TABLE extract_candidates ADD COLUMN evidence_span_start INTEGER;
            ALTER TABLE extract_candidates ADD COLUMN evidence_span_end INTEGER;
            ALTER TABLE extract_candidates ADD COLUMN evidence_span_sha256 TEXT;
            ALTER TABLE extract_candidates ADD COLUMN evidence_source_node_id INTEGER;
            ",
        )
        .map_err(|error| PolicyStoreError::sqlite("migrate v3 grounded extraction rows", error))
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
        let entity_tags = serialize(&candidate.entity_tags, "staged candidate entity tags")?;
        let ground_object = candidate.object.as_ref();
        let evidence_object = candidate
            .evidence_object
            .as_ref()
            .or(candidate.object.as_ref());
        let valid_from_ms = candidate
            .valid_from_ms
            .map(|value| sqlite_u64(value, "candidate valid_from_ms"))
            .transpose()?;
        let valid_until_ms = candidate
            .valid_until_ms
            .map(|value| sqlite_u64(value, "candidate valid_until_ms"))
            .transpose()?;
        let evidence_source_node_id = candidate
            .evidence_source_node_id
            .map(|value| sqlite_u64(value, "candidate evidence_source_node_id"))
            .transpose()?;
        let (evidence_span_start, evidence_span_end, evidence_span_sha256) = match (
            candidate.evidence_source_node_id,
            candidate.evidence_span.as_deref(),
        ) {
            (Some(source_node_id), Some(evidence_span)) => {
                let source = sources
                    .iter()
                    .find(|source| source.node_id == source_node_id)
                    .ok_or_else(|| {
                        PolicyStoreError::operation("resolve grounded extraction source")
                    })?;
                let start = source.content.find(evidence_span).ok_or_else(|| {
                    PolicyStoreError::operation("resolve grounded extraction span")
                })?;
                let end = start.checked_add(evidence_span.len()).ok_or_else(|| {
                    PolicyStoreError::invalid_value("grounded extraction span end")
                })?;
                (
                    Some(sqlite_usize(start, "candidate evidence_span_start")?),
                    Some(sqlite_usize(end, "candidate evidence_span_end")?),
                    Some(format!("{:x}", Sha256::digest(evidence_span.as_bytes()))),
                )
            }
            (None, None) => (None, None, None),
            _ => {
                return Err(PolicyStoreError::operation(
                    "grounded extraction metadata is incomplete",
                ));
            }
        };
        transaction
            .execute(
                "INSERT INTO extract_candidates
                 (run_id, item_local_id, content, kind, confidence,
                  ground_subject, ground_relation, ground_object, evidence_object,
                  evidence_span_start, evidence_span_end, evidence_span_sha256,
                  evidence_source_node_id, entity_tags, valid_from_ms, valid_until_ms, source_turn_keys,
                  source_session_id, source_scope, source_content_hashes, source_node_ids,
                  idempotency_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    run_id_sql,
                    candidate.item_local_id,
                    candidate.content,
                    candidate_kind(&candidate.kind),
                    candidate.confidence,
                    candidate.subject,
                    candidate.relation,
                    ground_object,
                    evidence_object,
                    evidence_span_start,
                    evidence_span_end,
                    evidence_span_sha256,
                    evidence_source_node_id,
                    entity_tags,
                    valid_from_ms,
                    valid_until_ms,
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

pub(super) fn list_audit(
    connection: &Connection,
    limit: u32,
) -> Result<ExtractionAuditResult, PolicyStoreError> {
    let limit = i64::from(limit);
    let mut candidate_statement = connection
        .prepare(
            "SELECT c.id, c.run_id, r.profile_id, c.item_local_id, c.content, c.kind,
                    c.confidence, c.ground_subject, c.ground_relation, c.ground_object,
                    c.evidence_object, c.evidence_span_start, c.evidence_span_end, c.evidence_span_sha256,
                    c.evidence_source_node_id,
                    c.entity_tags, c.valid_from_ms, c.valid_until_ms,
                    c.source_turn_keys, c.source_content_hashes,
                    c.source_session_id, c.source_scope, c.source_node_ids,
                    c.idempotency_key, c.audit_support, c.contamination_category,
                    c.reviewed_by, c.reviewed_at, c.committed_node_id
             FROM extract_candidates c JOIN extract_runs r ON r.id = c.run_id
             ORDER BY c.id LIMIT ?1",
        )
        .map_err(|error| PolicyStoreError::sqlite("prepare extraction audit candidates", error))?;
    let mut candidate_rows = candidate_statement
        .query([limit])
        .map_err(|error| PolicyStoreError::sqlite("query extraction audit candidates", error))?;
    let mut candidates = Vec::new();
    while let Some(row) = candidate_rows
        .next()
        .map_err(|error| PolicyStoreError::sqlite("read extraction audit candidate", error))?
    {
        let kind: String = row
            .get(5)
            .map_err(|error| PolicyStoreError::sqlite("read candidate kind", error))?;
        let support: Option<String> = row
            .get(24)
            .map_err(|error| PolicyStoreError::sqlite("read candidate audit support", error))?;
        let contamination: Option<String> = row
            .get(25)
            .map_err(|error| PolicyStoreError::sqlite("read candidate contamination", error))?;
        let reviewed_at: Option<i64> = row
            .get(27)
            .map_err(|error| PolicyStoreError::sqlite("read candidate reviewed_at", error))?;
        candidates.push(ExtractionAuditCandidateRow {
            id: sqlite_row_id(
                row.get(0)
                    .map_err(|error| PolicyStoreError::sqlite("read candidate id", error))?,
                "candidate id",
            )?,
            run_id: sqlite_row_id(
                row.get(1)
                    .map_err(|error| PolicyStoreError::sqlite("read candidate run_id", error))?,
                "candidate run_id",
            )?,
            profile_id: row
                .get(2)
                .map_err(|error| PolicyStoreError::sqlite("read candidate profile_id", error))?,
            item_local_id: row
                .get(3)
                .map_err(|error| PolicyStoreError::sqlite("read candidate item local id", error))?,
            content: row
                .get(4)
                .map_err(|error| PolicyStoreError::sqlite("read candidate content", error))?,
            kind: parse_candidate_kind(&kind)?,
            confidence: row
                .get(6)
                .map_err(|error| PolicyStoreError::sqlite("read candidate confidence", error))?,
            subject: row
                .get(7)
                .map_err(|error| PolicyStoreError::sqlite("read candidate subject", error))?,
            relation: row
                .get(8)
                .map_err(|error| PolicyStoreError::sqlite("read candidate relation", error))?,
            object: row
                .get(9)
                .map_err(|error| PolicyStoreError::sqlite("read candidate object", error))?,
            evidence_object: row.get(10).map_err(|error| {
                PolicyStoreError::sqlite("read candidate evidence object", error)
            })?,
            evidence_span: None,
            evidence_span_start: row
                .get::<_, Option<i64>>(11)
                .map_err(|error| {
                    PolicyStoreError::sqlite("read candidate evidence span start", error)
                })?
                .map(|value| sqlite_row_id(value, "candidate evidence_span_start"))
                .transpose()?,
            evidence_span_end: row
                .get::<_, Option<i64>>(12)
                .map_err(|error| {
                    PolicyStoreError::sqlite("read candidate evidence span end", error)
                })?
                .map(|value| sqlite_row_id(value, "candidate evidence_span_end"))
                .transpose()?,
            evidence_span_sha256: row.get(13).map_err(|error| {
                PolicyStoreError::sqlite("read candidate evidence span sha256", error)
            })?,
            evidence_source_node_id: row
                .get::<_, Option<i64>>(14)
                .map_err(|error| {
                    PolicyStoreError::sqlite("read candidate evidence source node id", error)
                })?
                .map(|value| sqlite_row_id(value, "candidate evidence_source_node_id"))
                .transpose()?,
            entity_tags: deserialize(
                &row.get::<_, String>(15).map_err(|error| {
                    PolicyStoreError::sqlite("read candidate entity tags", error)
                })?,
                "candidate entity tags",
            )?,
            valid_from_ms: row
                .get::<_, Option<i64>>(16)
                .map_err(|error| PolicyStoreError::sqlite("read candidate valid_from_ms", error))?
                .map(|value| sqlite_row_id(value, "candidate valid_from_ms"))
                .transpose()?,
            valid_until_ms: row
                .get::<_, Option<i64>>(17)
                .map_err(|error| PolicyStoreError::sqlite("read candidate valid_until_ms", error))?
                .map(|value| sqlite_row_id(value, "candidate valid_until_ms"))
                .transpose()?,
            source_turn_keys: deserialize(
                &row.get::<_, String>(18).map_err(|error| {
                    PolicyStoreError::sqlite("read candidate source turn keys", error)
                })?,
                "candidate source turn keys",
            )?,
            source_content_hashes: deserialize(
                &row.get::<_, String>(19).map_err(|error| {
                    PolicyStoreError::sqlite("read candidate source content hashes", error)
                })?,
                "candidate source content hashes",
            )?,
            source_session_id: row.get(20).map_err(|error| {
                PolicyStoreError::sqlite("read candidate source session", error)
            })?,
            source_scope: row
                .get(21)
                .map_err(|error| PolicyStoreError::sqlite("read candidate source scope", error))?,
            source_node_ids: deserialize(
                &row.get::<_, String>(22).map_err(|error| {
                    PolicyStoreError::sqlite("read candidate source node ids", error)
                })?,
                "candidate source node ids",
            )?,
            idempotency_key: row.get(23).map_err(|error| {
                PolicyStoreError::sqlite("read candidate idempotency key", error)
            })?,
            support: support.as_deref().map(parse_audit_support).transpose()?,
            contamination: contamination
                .as_deref()
                .map(parse_contamination_category)
                .transpose()?,
            reviewed_by: row
                .get(26)
                .map_err(|error| PolicyStoreError::sqlite("read candidate reviewer", error))?,
            reviewed_at: reviewed_at
                .map(|value| sqlite_row_id(value, "candidate reviewed_at"))
                .transpose()?,
            committed_node_id: row
                .get::<_, Option<i64>>(28)
                .map_err(|error| {
                    PolicyStoreError::sqlite("read candidate committed_node_id", error)
                })?
                .map(|value| sqlite_row_id(value, "candidate committed_node_id"))
                .transpose()?,
            sources: Vec::new(),
        });
    }

    let mut relation_statement = connection
        .prepare(
            "SELECT r.id, source.run_id, run.profile_id, r.candidate_from, r.candidate_to,
                    source.item_local_id, target.item_local_id, r.relation_type,
                    r.audit_status, r.reviewed_by, r.reviewed_at, r.committed_edge_id
             FROM extract_relations r
             JOIN extract_candidates source ON source.id = r.candidate_from
             JOIN extract_candidates target ON target.id = r.candidate_to
             JOIN extract_runs run ON run.id = source.run_id
             ORDER BY r.id LIMIT ?1",
        )
        .map_err(|error| PolicyStoreError::sqlite("prepare extraction audit relations", error))?;
    let mut relation_rows = relation_statement
        .query([limit])
        .map_err(|error| PolicyStoreError::sqlite("query extraction audit relations", error))?;
    let mut relations = Vec::new();
    while let Some(row) = relation_rows
        .next()
        .map_err(|error| PolicyStoreError::sqlite("read extraction audit relation", error))?
    {
        let relation_type: String = row
            .get(7)
            .map_err(|error| PolicyStoreError::sqlite("read relation type", error))?;
        let verdict: Option<String> = row
            .get(8)
            .map_err(|error| PolicyStoreError::sqlite("read relation audit verdict", error))?;
        let reviewed_at: Option<i64> = row
            .get(10)
            .map_err(|error| PolicyStoreError::sqlite("read relation reviewed_at", error))?;
        relations.push(ExtractionAuditRelationRow {
            id: sqlite_row_id(
                row.get(0)
                    .map_err(|error| PolicyStoreError::sqlite("read relation id", error))?,
                "relation id",
            )?,
            run_id: sqlite_row_id(
                row.get(1)
                    .map_err(|error| PolicyStoreError::sqlite("read relation run_id", error))?,
                "relation run_id",
            )?,
            profile_id: row
                .get(2)
                .map_err(|error| PolicyStoreError::sqlite("read relation profile_id", error))?,
            candidate_from: sqlite_row_id(
                row.get(3)
                    .map_err(|error| PolicyStoreError::sqlite("read relation source", error))?,
                "relation source",
            )?,
            candidate_to: sqlite_row_id(
                row.get(4)
                    .map_err(|error| PolicyStoreError::sqlite("read relation target", error))?,
                "relation target",
            )?,
            from_item_local_id: row.get(5).map_err(|error| {
                PolicyStoreError::sqlite("read relation source item local id", error)
            })?,
            to_item_local_id: row.get(6).map_err(|error| {
                PolicyStoreError::sqlite("read relation target item local id", error)
            })?,
            relation_type: parse_relation_kind(&relation_type)?,
            verdict: verdict.as_deref().map(parse_relation_verdict).transpose()?,
            reviewed_by: row
                .get(9)
                .map_err(|error| PolicyStoreError::sqlite("read relation reviewer", error))?,
            reviewed_at: reviewed_at
                .map(|value| sqlite_row_id(value, "relation reviewed_at"))
                .transpose()?,
            committed_edge_id: row
                .get::<_, Option<i64>>(11)
                .map_err(|error| {
                    PolicyStoreError::sqlite("read relation committed_edge_id", error)
                })?
                .map(|value| sqlite_row_id(value, "relation committed_edge_id"))
                .transpose()?,
        });
    }

    Ok(ExtractionAuditResult {
        candidates,
        relations,
    })
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
pub(super) fn update_candidate_audit(
    connection: &mut Connection,
    id: u64,
    support: AuditSupport,
    contamination: Option<ContaminationCategory>,
    reviewer: &str,
    reviewed_at: u64,
) -> Result<(), PolicyStoreError> {
    let id = sqlite_u64(id, "extraction candidate audit id")?;
    let reviewed_at = sqlite_u64(reviewed_at, "extraction candidate reviewed_at")?;
    let transaction = connection
        .transaction()
        .map_err(|error| PolicyStoreError::sqlite("start candidate audit transaction", error))?;
    let updated = transaction
        .execute(
            "UPDATE extract_candidates
             SET audit_support = ?1, contamination_category = ?2,
                 reviewed_by = ?3, reviewed_at = ?4
             WHERE id = ?5",
            params![
                audit_support(support),
                contamination.map(contamination_category),
                reviewer,
                reviewed_at,
                id,
            ],
        )
        .map_err(|error| PolicyStoreError::sqlite("update extraction candidate audit", error))?;
    if updated != 1 {
        return Err(PolicyStoreError::operation(
            "update extraction candidate audit",
        ));
    }
    transaction
        .commit()
        .map_err(|error| PolicyStoreError::sqlite("commit candidate audit transaction", error))
}

pub(super) fn update_relation_audit(
    connection: &mut Connection,
    id: u64,
    verdict: RelationVerdict,
    reviewer: &str,
    reviewed_at: u64,
) -> Result<(), PolicyStoreError> {
    let id = sqlite_u64(id, "extraction relation audit id")?;
    let reviewed_at = sqlite_u64(reviewed_at, "extraction relation reviewed_at")?;
    let transaction = connection
        .transaction()
        .map_err(|error| PolicyStoreError::sqlite("start relation audit transaction", error))?;
    let updated = transaction
        .execute(
            "UPDATE extract_relations
             SET audit_status = ?1, reviewed_by = ?2, reviewed_at = ?3
             WHERE id = ?4",
            params![relation_verdict(verdict), reviewer, reviewed_at, id],
        )
        .map_err(|error| PolicyStoreError::sqlite("update extraction relation audit", error))?;
    if updated != 1 {
        return Err(PolicyStoreError::operation(
            "update extraction relation audit",
        ));
    }
    transaction
        .commit()
        .map_err(|error| PolicyStoreError::sqlite("commit relation audit transaction", error))
}

pub(super) fn mark_candidate_committed(
    connection: &mut Connection,
    id: u64,
    node_id: u64,
) -> Result<(), PolicyStoreError> {
    let id = sqlite_u64(id, "extraction candidate commit id")?;
    let node_id = sqlite_u64(node_id, "extraction candidate committed node id")?;
    let changed = connection
        .execute(
            "UPDATE extract_candidates
             SET committed_node_id = ?2
             WHERE id = ?1 AND (committed_node_id IS NULL OR committed_node_id = ?2)",
            params![id, node_id],
        )
        .map_err(|error| PolicyStoreError::sqlite("mark extraction candidate committed", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(PolicyStoreError::operation(
            "extraction candidate commit target conflicts",
        ))
    }
}

pub(super) fn mark_relation_committed(
    connection: &mut Connection,
    id: u64,
    edge_id: u64,
) -> Result<(), PolicyStoreError> {
    let id = sqlite_u64(id, "extraction relation commit id")?;
    let edge_id = sqlite_u64(edge_id, "extraction relation committed edge id")?;
    let changed = connection
        .execute(
            "UPDATE extract_relations
             SET committed_edge_id = ?2
             WHERE id = ?1 AND (committed_edge_id IS NULL OR committed_edge_id = ?2)",
            params![id, edge_id],
        )
        .map_err(|error| PolicyStoreError::sqlite("mark extraction relation committed", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(PolicyStoreError::operation(
            "extraction relation commit target conflicts",
        ))
    }
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
fn deserialize<T: serde::de::DeserializeOwned>(
    value: &str,
    field: &'static str,
) -> Result<T, PolicyStoreError> {
    serde_json::from_str(value).map_err(|_| PolicyStoreError::invalid_value(field))
}

fn parse_audit_support(value: &str) -> Result<AuditSupport, PolicyStoreError> {
    match value {
        "supported" => Ok(AuditSupport::Supported),
        "partial" => Ok(AuditSupport::Partial),
        "unsupported" => Ok(AuditSupport::Unsupported),
        _ => Err(PolicyStoreError::invalid_value("candidate audit support")),
    }
}

fn parse_contamination_category(value: &str) -> Result<ContaminationCategory, PolicyStoreError> {
    match value {
        "unsupported-claim" => Ok(ContaminationCategory::UnsupportedClaim),
        "prompt-injection" => Ok(ContaminationCategory::PromptInjection),
        "secret-reexposure" => Ok(ContaminationCategory::SecretReexposure),
        "foreign-scope" => Ok(ContaminationCategory::ForeignScope),
        "contradicts-source" => Ok(ContaminationCategory::ContradictsSource),
        _ => Err(PolicyStoreError::invalid_value(
            "candidate contamination category",
        )),
    }
}

fn parse_relation_verdict(value: &str) -> Result<RelationVerdict, PolicyStoreError> {
    match value {
        "correct" => Ok(RelationVerdict::Correct),
        "wrong-type" => Ok(RelationVerdict::WrongType),
        "wrong-direction" => Ok(RelationVerdict::WrongDirection),
        "invalid" => Ok(RelationVerdict::Invalid),
        _ => Err(PolicyStoreError::invalid_value("relation audit verdict")),
    }
}

fn parse_candidate_kind(value: &str) -> Result<CandidateKind, PolicyStoreError> {
    match value {
        "fact" => Ok(CandidateKind::Fact),
        "entity" => Ok(CandidateKind::Entity),
        "event" => Ok(CandidateKind::Event),
        "preference" => Ok(CandidateKind::Preference),
        "decision" => Ok(CandidateKind::Decision),
        "causal" => Ok(CandidateKind::Causal),
        "lesson" => Ok(CandidateKind::Lesson),
        "convention" => Ok(CandidateKind::Convention),
        "gotcha" => Ok(CandidateKind::Gotcha),
        _ => Err(PolicyStoreError::invalid_value("candidate kind")),
    }
}

fn parse_relation_kind(value: &str) -> Result<RelationKind, PolicyStoreError> {
    match value {
        "reason" => Ok(RelationKind::Reason),
        "causal" => Ok(RelationKind::Causal),
        "contradicts" => Ok(RelationKind::Contradicts),
        "supports" => Ok(RelationKind::Supports),
        _ => Err(PolicyStoreError::invalid_value("relation type")),
    }
}
fn audit_support(support: AuditSupport) -> &'static str {
    match support {
        AuditSupport::Supported => "supported",
        AuditSupport::Partial => "partial",
        AuditSupport::Unsupported => "unsupported",
    }
}

fn contamination_category(category: ContaminationCategory) -> &'static str {
    match category {
        ContaminationCategory::UnsupportedClaim => "unsupported-claim",
        ContaminationCategory::PromptInjection => "prompt-injection",
        ContaminationCategory::SecretReexposure => "secret-reexposure",
        ContaminationCategory::ForeignScope => "foreign-scope",
        ContaminationCategory::ContradictsSource => "contradicts-source",
    }
}

fn relation_verdict(verdict: RelationVerdict) -> &'static str {
    match verdict {
        RelationVerdict::Correct => "correct",
        RelationVerdict::WrongType => "wrong-type",
        RelationVerdict::WrongDirection => "wrong-direction",
        RelationVerdict::Invalid => "invalid",
    }
}
fn candidate_kind(kind: &CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Fact => "fact",
        CandidateKind::Entity => "entity",
        CandidateKind::Event => "event",
        CandidateKind::Preference => "preference",
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
        ExtractionErrorKind::Stdin => "stdin",
        ExtractionErrorKind::ProviderRequest => "provider-request",
        ExtractionErrorKind::ProviderResponse => "provider-response",
        ExtractionErrorKind::Timeout => "timeout",
        ExtractionErrorKind::StdoutTooLarge => "stdout-too-large",
        ExtractionErrorKind::StderrTooLarge => "stderr-too-large",
        ExtractionErrorKind::NonZero => "non-zero",
        ExtractionErrorKind::InvalidJson => "invalid-json",
        ExtractionErrorKind::InvalidUtf8 => "invalid-utf8",
        ExtractionErrorKind::SchemaReject => "schema-reject",
        ExtractionErrorKind::StageReject => "stage-reject",
    }
}
