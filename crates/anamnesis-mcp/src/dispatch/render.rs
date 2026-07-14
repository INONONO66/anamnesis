//! Response rendering: turns engine results (`MemoryStats`, `MemoryView`,
//! `PackagedRecall`) into the exact wire-text `dispatch` returns, byte-for-byte
//! identical to what the daemon, `serve` adapter, and `--embedded` CLI path
//! have always produced. Split out of `dispatch.rs` verbatim (behavior-preserving
//! move only).

use anamnesis::memory::{MemoryStats, MemoryView};

use crate::memory;

/// Render `list`'s compact JSON array: one object per node with the fields an
/// agent needs to pick a target for `get`/`update`/`forget`/`supersede`.
pub(crate) fn render_list(views: &[MemoryView]) -> String {
    let items: Vec<_> = views.iter().map(list_item_json).collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

fn list_item_json(v: &MemoryView) -> serde_json::Value {
    const PREVIEW_LEN: usize = 120;
    let preview: String = v.content.chars().take(PREVIEW_LEN).collect();
    let preview = if v.content.chars().count() > PREVIEW_LEN {
        format!("{preview}…")
    } else {
        preview
    };
    serde_json::json!({
        "node_id": v.node_id.0,
        "content_preview": preview,
        "salience": v.salience,
        "tier": format!("{:?}", v.tier),
        "node_type": memory::knowledge_type_label(&v.node_type),
        "created_at": v.created_at.0,
        "retracted": v.retracted,
        "valid_until": v.valid_until.map(|t| t.0),
        "peer_id": v.peer_id,
        "session_id": v.session_id,
        "scope": v.scope,
        "confidence": v.confidence,
    })
}

/// Render `get`'s compact JSON object: the full [`MemoryView`] a management
/// consumer needs, without the internal `Node` fields (access-history
/// reservoirs, …) `MemoryView` already omits. Includes provenance
/// (`peer_id`/`session_id`/`scope`/`confidence`).
pub(crate) fn render_view(v: &MemoryView) -> String {
    let value = serde_json::json!({
        "node_id": v.node_id.0,
        "content": v.content,
        "metadata": v.metadata,
        "entity_tags": v.entity_tags,
        "salience": v.salience,
        "tier": format!("{:?}", v.tier),
        "node_type": memory::knowledge_type_label(&v.node_type),
        "created_at": v.created_at.0,
        "updated_at": v.updated_at.0,
        "valid_from": v.valid_from.map(|t| t.0),
        "valid_until": v.valid_until.map(|t| t.0),
        "retracted": v.retracted,
        "peer_id": v.peer_id,
        "session_id": v.session_id,
        "scope": v.scope,
        "confidence": v.confidence,
    });
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

/// Render a [`PackagedRecall`](crate::memory::PackagedRecall) to the `recall`
/// payload: the readable context block (or the `(no relevant memory)` sentinel
/// when nothing packaged) followed by the compact `{node_id, score, cosine}` NODES list
/// the agent feeds to `relate`. The hook keys off this exact shape.
pub(crate) fn render_recall(
    packaged: &crate::memory::PackagedRecall,
) -> Result<String, serde_json::Error> {
    let refs: Vec<_> = packaged
        .hits
        .iter()
        .map(
            |h| serde_json::json!({ "node_id": h.node_id.0, "score": h.score, "cosine": h.cosine }),
        )
        .collect();
    let refs_json = serde_json::to_string(&refs)?;
    let context = if packaged.context.trim().is_empty() {
        "(no relevant memory)\n".to_string()
    } else {
        packaged.context.clone()
    };
    Ok(format!("{context}## NODES (for `relate`)\n{refs_json}"))
}

/// Render a [`MemoryStats`] snapshot to the human-readable block. Shared so the
/// daemon, the `stats` MCP tool (via the daemon), and the `--embedded` CLI path
/// produce byte-identical output.
pub fn format_stats(s: &MemoryStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "nodes:                {}", s.node_count);
    let _ = writeln!(out, "edges:                {}", s.edge_count);
    let _ = writeln!(
        out,
        "orphans:              {} ({:.1}%)",
        s.orphan_count,
        s.orphan_ratio * 100.0
    );
    let _ = writeln!(
        out,
        "contradictions:       {} ({:.1}%)",
        s.contradiction_count,
        s.contradiction_ratio * 100.0
    );
    let _ = writeln!(out, "supersedes:           {}", s.supersede_count);
    let _ = writeln!(out, "retracted:            {}", s.retracted_count);
    let _ = writeln!(out, "missing embeddings:   {}", s.missing_embedding_count);
    let _ = writeln!(out, "avg salience:         {:.3}", s.avg_salience);
    let _ = writeln!(out, "avg degree:           {:.2}", s.average_degree);
    let _ = writeln!(out, "stale (>30d):         {:.1}%", s.stale_ratio * 100.0);
    let _ = writeln!(out, "salience entropy:     {:.3} bits", s.salience_entropy);
    let _ = writeln!(out, "peers:                {}", s.peer_count);
    let _ = writeln!(out, "health grade:         {:?}", s.grade);
    if !s.scope_distribution.is_empty() {
        let _ = writeln!(out, "scope distribution:");
        for (scope, count) in &s.scope_distribution {
            let _ = writeln!(out, "  {scope}: {count}");
        }
    }
    out
}
