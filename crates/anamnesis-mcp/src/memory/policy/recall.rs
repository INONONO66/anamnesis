use rusqlite::{Transaction, params};

use crate::proto::RecallEventKind;

use super::PolicyStoreError;

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

fn sqlite_integer(value: u64, field: &'static str) -> Result<i64, PolicyStoreError> {
    i64::try_from(value).map_err(|_| PolicyStoreError::invalid_value(field))
}
