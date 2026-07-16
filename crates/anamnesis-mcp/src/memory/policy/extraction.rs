use rusqlite::Transaction;

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

pub(super) fn create_schema(transaction: &Transaction<'_>) -> Result<(), PolicyStoreError> {
    transaction
        .execute_batch(EXTRACTION_TABLES_SQL)
        .map_err(|error| PolicyStoreError::sqlite("create extraction policy tables", error))
}
