use rusqlite::{Connection, Transaction, params};

use crate::proto::RecallEventKind;

use super::{
    AbstentionStats, AutoExposureStats, CosineStats, EventKindStats, PolicyStoreError, RecallStats,
    SweepPoint,
};

pub(crate) const RECALL_EVENT_RETENTION: u64 = 10_000;

/// Data-minimized telemetry for one recall decision. It intentionally contains
/// no query text, transcript, or rendered context.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecallEvent {
    pub at_ms: u64,
    pub namespace: String,
    pub event_kind: RecallEventKind,
    pub query_chars: u64,
    pub scope: Option<String>,
    pub knowledge_only: bool,
    pub has_hits: bool,
    pub readout_pass: bool,
    pub cosine_pass: bool,
    pub eligible: bool,
    pub top_score: Option<f64>,
    pub top_cosine: Option<f64>,
    pub gate_threshold: Option<f64>,
    pub cosine_gate: Option<f64>,
    pub result_node_ids: Vec<u64>,
    pub auto_extract_node_count: u64,
}

pub(super) fn stats(connection: &Connection) -> Result<RecallStats, PolicyStoreError> {
    let (
        total_attempts,
        empty,
        readout_only,
        cosine_only,
        both,
        eligible_events,
        events_with_auto,
        result_slots,
        auto_slots,
        non_null_cosines,
    ) = connection
        .query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN has_hits = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN has_hits = 1 AND readout_pass = 0 AND cosine_pass = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN has_hits = 1 AND readout_pass = 1 AND cosine_pass = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN has_hits = 1 AND readout_pass = 0 AND cosine_pass = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN eligible = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN eligible = 1 AND auto_extract_node_count > 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN eligible = 1 THEN json_array_length(result_node_ids) ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN eligible = 1 THEN auto_extract_node_count ELSE 0 END), 0),
                COUNT(top_cosine)
             FROM recall_events",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .map_err(|_| PolicyStoreError::operation("aggregate recall telemetry"))?;

    let total_attempts = stats_integer(total_attempts, "recall stats total attempts")?;
    let non_null_cosines = stats_integer(non_null_cosines, "recall stats cosine samples")?;
    let cosine_nulls = total_attempts
        .checked_sub(non_null_cosines)
        .ok_or_else(|| PolicyStoreError::invalid_value("recall stats cosine nulls"))?;

    let by_event_kind = event_kind_stats(connection)?;
    let cosine_values = cosine_values(connection)?;
    let samples = u64::try_from(cosine_values.len())
        .map_err(|_| PolicyStoreError::invalid_value("recall stats cosine samples"))?;
    if samples != non_null_cosines {
        return Err(PolicyStoreError::invalid_value(
            "recall stats cosine samples",
        ));
    }

    let sweep = sweep(connection, total_attempts)?;

    Ok(RecallStats {
        total_attempts,
        by_event_kind,
        abstentions: AbstentionStats {
            empty: stats_integer(empty, "recall stats empty attempts")?,
            readout_only: stats_integer(readout_only, "recall stats readout-only attempts")?,
            cosine_only: stats_integer(cosine_only, "recall stats cosine-only attempts")?,
            both: stats_integer(both, "recall stats both-gate attempts")?,
        },
        cosine: CosineStats {
            samples,
            nulls: cosine_nulls,
            p50: percentile(&cosine_values, 0.50),
            p90: percentile(&cosine_values, 0.90),
            p95: percentile(&cosine_values, 0.95),
        },
        auto_exposure: AutoExposureStats {
            eligible_events: stats_integer(eligible_events, "recall stats eligible events")?,
            events_with_auto: stats_integer(events_with_auto, "recall stats auto events")?,
            result_slots: stats_integer(result_slots, "recall stats result slots")?,
            auto_slots: stats_integer(auto_slots, "recall stats auto slots")?,
        },
        sweep,
    })
}

fn event_kind_stats(connection: &Connection) -> Result<Vec<EventKindStats>, PolicyStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT event_kind, COUNT(*), COALESCE(SUM(CASE WHEN eligible = 1 THEN 1 ELSE 0 END), 0)
             FROM recall_events
             GROUP BY event_kind
             ORDER BY event_kind",
        )
        .map_err(|_| PolicyStoreError::operation("prepare recall event-kind telemetry"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| PolicyStoreError::operation("query recall event-kind telemetry"))?;

    rows.map(|row| {
        let (event_kind, attempts, eligible) =
            row.map_err(|_| PolicyStoreError::operation("read recall event-kind telemetry"))?;
        let event_kind = serde_json::from_value(serde_json::Value::String(event_kind))
            .map_err(|_| PolicyStoreError::operation("deserialize recall event kind"))?;
        Ok(EventKindStats {
            event_kind,
            attempts: stats_integer(attempts, "recall stats event-kind attempts")?,
            eligible: stats_integer(eligible, "recall stats event-kind eligible")?,
        })
    })
    .collect()
}

fn cosine_values(connection: &Connection) -> Result<Vec<f64>, PolicyStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT top_cosine FROM recall_events
             WHERE top_cosine IS NOT NULL
             ORDER BY top_cosine",
        )
        .map_err(|_| PolicyStoreError::operation("prepare recall cosine telemetry"))?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .map_err(|_| PolicyStoreError::operation("query recall cosine telemetry"))?;

    rows.map(|row| row.map_err(|_| PolicyStoreError::operation("read recall cosine telemetry")))
        .collect()
}

fn sweep(connection: &Connection, attempts: u64) -> Result<Vec<SweepPoint>, PolicyStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT COALESCE(SUM(CASE
                WHEN has_hits = 1
                 AND readout_pass = 1
                 AND top_cosine IS NOT NULL
                 AND top_cosine >= ?1
                THEN 1 ELSE 0
             END), 0)
             FROM recall_events",
        )
        .map_err(|_| PolicyStoreError::operation("prepare recall threshold sweep"))?;

    (80_u8..=90)
        .map(|hundredths| {
            let threshold = f64::from(hundredths) / 100.0;
            let eligible = statement
                .query_row([threshold], |row| row.get::<_, i64>(0))
                .map_err(|_| PolicyStoreError::operation("query recall threshold sweep"))?;
            Ok(SweepPoint {
                threshold,
                eligible: stats_integer(eligible, "recall stats sweep eligible")?,
                attempts,
            })
        })
        .collect()
}

fn percentile(values: &[f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }

    let index = (percentile * values.len() as f64).ceil() as usize - 1;
    values.get(index).copied()
}

fn stats_integer(value: i64, field: &'static str) -> Result<u64, PolicyStoreError> {
    u64::try_from(value).map_err(|_| PolicyStoreError::invalid_value(field))
}

pub(super) fn insert(
    transaction: &Transaction<'_>,
    event: &RecallEvent,
) -> Result<(), PolicyStoreError> {
    let at_ms = sqlite_integer(event.at_ms, "recall event timestamp")?;
    let query_chars = sqlite_integer(event.query_chars, "recall event query length")?;
    let auto_extract_node_count = sqlite_integer(
        event.auto_extract_node_count,
        "recall event auto-extract node count",
    )?;
    let event_kind = serde_json::to_string(&event.event_kind)
        .map_err(|_| PolicyStoreError::operation("serialize recall event kind"))?;
    let event_kind = event_kind.trim_matches('"');
    let result_node_ids = serde_json::to_string(&event.result_node_ids)
        .map_err(|_| PolicyStoreError::operation("serialize recall event node ids"))?;
    let retention = sqlite_integer(RECALL_EVENT_RETENTION, "recall event retention")?;

    transaction
        .execute(
            "INSERT INTO recall_events (
                at_ms, namespace, event_kind, query_chars, scope, knowledge_only,
                has_hits, readout_pass, cosine_pass, eligible, top_score, top_cosine,
                gate_threshold, cosine_gate, result_node_ids, auto_extract_node_count
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
            )",
            params![
                at_ms,
                event.namespace,
                event_kind,
                query_chars,
                event.scope,
                i64::from(event.knowledge_only),
                i64::from(event.has_hits),
                i64::from(event.readout_pass),
                i64::from(event.cosine_pass),
                i64::from(event.eligible),
                event.top_score,
                event.top_cosine,
                event.gate_threshold,
                event.cosine_gate,
                result_node_ids,
                auto_extract_node_count,
            ],
        )
        .map_err(|_| PolicyStoreError::operation("insert recall event"))?;

    transaction
        .execute(
            "DELETE FROM recall_events
             WHERE id <= COALESCE(
                 (SELECT id FROM recall_events ORDER BY id DESC LIMIT 1 OFFSET ?1),
                 -1
             )",
            [retention],
        )
        .map_err(|_| PolicyStoreError::operation("prune recall events"))?;

    Ok(())
}

#[cfg(test)]
pub(super) fn count(connection: &Connection) -> Result<u64, PolicyStoreError> {
    connection
        .query_row("SELECT COUNT(*) FROM recall_events", [], |row| row.get(0))
        .map_err(|_| PolicyStoreError::operation("count recall events"))
}

#[cfg(test)]
pub(super) fn install_insert_failure_trigger(
    connection: &Connection,
) -> Result<(), PolicyStoreError> {
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS recall_events_force_insert_failure;
             CREATE TRIGGER recall_events_force_insert_failure
             BEFORE INSERT ON recall_events
             BEGIN
                 SELECT RAISE(FAIL, 'forced');
             END;",
        )
        .map_err(|_| PolicyStoreError::operation("install recall event insert failure trigger"))
}

#[cfg(test)]
pub(super) fn read_all(connection: &Connection) -> Result<Vec<RecallEvent>, PolicyStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT
                at_ms, namespace, event_kind, query_chars, scope, knowledge_only,
                has_hits, readout_pass, cosine_pass, eligible, top_score, top_cosine,
                gate_threshold, cosine_gate, result_node_ids, auto_extract_node_count
             FROM recall_events
             ORDER BY id",
        )
        .map_err(|_| PolicyStoreError::operation("prepare recall event read"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, bool>(9)?,
                row.get::<_, Option<f64>>(10)?,
                row.get::<_, Option<f64>>(11)?,
                row.get::<_, Option<f64>>(12)?,
                row.get::<_, Option<f64>>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, i64>(15)?,
            ))
        })
        .map_err(|_| PolicyStoreError::operation("read recall events"))?;

    rows.map(|row| {
        let (
            at_ms,
            namespace,
            event_kind,
            query_chars,
            scope,
            knowledge_only,
            has_hits,
            readout_pass,
            cosine_pass,
            eligible,
            top_score,
            top_cosine,
            gate_threshold,
            cosine_gate,
            result_node_ids,
            auto_extract_node_count,
        ) = row.map_err(|_| PolicyStoreError::operation("read recall events"))?;
        Ok(RecallEvent {
            at_ms: u64::try_from(at_ms)
                .map_err(|_| PolicyStoreError::operation("read recall event timestamp"))?,
            namespace,
            event_kind: serde_json::from_str(&format!("\"{event_kind}\""))
                .map_err(|_| PolicyStoreError::operation("deserialize recall event kind"))?,
            query_chars: u64::try_from(query_chars)
                .map_err(|_| PolicyStoreError::operation("read recall event query length"))?,
            scope,
            knowledge_only,
            has_hits,
            readout_pass,
            cosine_pass,
            eligible,
            top_score,
            top_cosine,
            gate_threshold,
            cosine_gate,
            result_node_ids: serde_json::from_str(&result_node_ids)
                .map_err(|_| PolicyStoreError::operation("deserialize recall event node ids"))?,
            auto_extract_node_count: u64::try_from(auto_extract_node_count).map_err(|_| {
                PolicyStoreError::operation("read recall event auto-extract node count")
            })?,
        })
    })
    .collect()
}

#[cfg(test)]
pub(super) fn contains_value(
    connection: &Connection,
    value: &str,
) -> Result<bool, PolicyStoreError> {
    if value.is_empty() {
        return Ok(false);
    }

    connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM recall_events
                WHERE instr(CAST(at_ms AS TEXT), ?1) > 0
                   OR instr(namespace, ?1) > 0
                   OR instr(event_kind, ?1) > 0
                   OR instr(CAST(query_chars AS TEXT), ?1) > 0
                   OR instr(COALESCE(scope, ''), ?1) > 0
                   OR instr(CAST(knowledge_only AS TEXT), ?1) > 0
                   OR instr(CAST(has_hits AS TEXT), ?1) > 0
                   OR instr(CAST(readout_pass AS TEXT), ?1) > 0
                   OR instr(CAST(cosine_pass AS TEXT), ?1) > 0
                   OR instr(CAST(eligible AS TEXT), ?1) > 0
                   OR instr(COALESCE(CAST(top_score AS TEXT), ''), ?1) > 0
                   OR instr(COALESCE(CAST(top_cosine AS TEXT), ''), ?1) > 0
                   OR instr(COALESCE(CAST(gate_threshold AS TEXT), ''), ?1) > 0
                   OR instr(COALESCE(CAST(cosine_gate AS TEXT), ''), ?1) > 0
                   OR instr(result_node_ids, ?1) > 0
                   OR instr(CAST(auto_extract_node_count AS TEXT), ?1) > 0
            )",
            [value],
            |row| row.get(0),
        )
        .map_err(|_| PolicyStoreError::operation("inspect recall event values"))
}

fn sqlite_integer(value: u64, field: &'static str) -> Result<i64, PolicyStoreError> {
    i64::try_from(value).map_err(|_| PolicyStoreError::invalid_value(field))
}
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn minimized_event(at_ms: u64) -> RecallEvent {
        RecallEvent {
            at_ms,
            namespace: "canonical".into(),
            event_kind: RecallEventKind::UserPrompt,
            query_chars: 8,
            scope: Some("project/anamnesis".into()),
            knowledge_only: true,
            has_hits: true,
            readout_pass: true,
            cosine_pass: true,
            eligible: true,
            top_score: Some(0.95),
            top_cosine: Some(0.9),
            gate_threshold: Some(0.8),
            cosine_gate: Some(0.7),
            result_node_ids: vec![7, 11],
            auto_extract_node_count: 0,
        }
    }

    fn initialized_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        super::super::schema::initialize(&mut connection).unwrap();
        connection
    }

    #[test]
    fn recall_event_retention_keeps_latest_ten_thousand() {
        let mut connection = initialized_connection();
        let transaction = connection.transaction().unwrap();

        for at_ms in 0..=RECALL_EVENT_RETENTION {
            insert(&transaction, &minimized_event(at_ms)).unwrap();
        }

        let (count, oldest_id, oldest_at_ms, newest_id): (i64, i64, i64, i64) = transaction
            .query_row(
                "SELECT COUNT(*), MIN(id), MIN(at_ms), MAX(id) FROM recall_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(count, 10_000);
        assert_eq!(oldest_id, 2, "retention must discard the oldest row ID");
        assert_eq!(oldest_at_ms, 1);
        assert_eq!(newest_id, 10_001);
    }

    #[test]
    fn recall_events_store_only_minimized_values() {
        let secret_query = "sëcret 🔐";
        assert_eq!(secret_query.chars().count(), 8);

        let mut connection = initialized_connection();
        let transaction = connection.transaction().unwrap();
        insert(&transaction, &minimized_event(42)).unwrap();

        let columns = {
            let mut statement = transaction
                .prepare("PRAGMA table_info(recall_events)")
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            columns,
            [
                "id",
                "at_ms",
                "namespace",
                "event_kind",
                "query_chars",
                "scope",
                "knowledge_only",
                "has_hits",
                "readout_pass",
                "cosine_pass",
                "eligible",
                "top_score",
                "top_cosine",
                "gate_threshold",
                "cosine_gate",
                "result_node_ids",
                "auto_extract_node_count",
            ],
            "recall-events schema must not retain raw query, transcript, or rendered context"
        );

        let (namespace, event_kind, query_chars, scope, node_ids): (
            String,
            String,
            i64,
            Option<String>,
            String,
        ) = transaction
            .query_row(
                "SELECT namespace, event_kind, query_chars, scope, result_node_ids
                 FROM recall_events",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(namespace, "canonical");
        assert_eq!(event_kind, "user-prompt");
        assert_eq!(query_chars, 8);
        assert_eq!(scope.as_deref(), Some("project/anamnesis"));
        assert_eq!(node_ids, "[7,11]");
        assert!(
            !format!("{namespace}{event_kind}{query_chars}{scope:?}{node_ids}")
                .contains(secret_query),
            "the secret query must not be persisted"
        );
    }
}
