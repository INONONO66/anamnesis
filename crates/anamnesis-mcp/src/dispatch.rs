//! The shared op→text core. One pure function maps a [`proto::Request`](crate::proto::Request) to a
//! [`proto::Response`](crate::proto::Response) by calling the registry and formatting the result exactly
//! as the MCP tools used to — so every path (the daemon serving the bespoke
//! socket, and the `--embedded serve` in-process path) produces byte-identical
//! output. This module has NO rmcp dependency; MCP lives only in `server.rs`.

use anamnesis::memory::MemoryStats;

use crate::memory::{MemoryRegistry, Turn};
use crate::proto::{Request, Response};

/// Run one request against `reg` and return the consumer-ready reply.
///
/// The caller owns serialization: the daemon wraps this in `spawn_blocking`
/// under its single registry `Mutex`; the embedded path calls it directly on an
/// owned registry. Caller errors (a bad relation label / missing endpoint) map
/// to [`Response::invalid_params`]; everything else to [`Response::internal`].
pub fn dispatch(reg: &mut MemoryRegistry, req: Request) -> Response {
    match req {
        Request::Recall {
            query,
            limit,
            namespace,
            reinforce,
            gate_threshold,
        } => {
            let limit = limit.unwrap_or(20) as usize;
            let packaged = match reg.recall_packaged_gated(
                &query,
                limit,
                namespace.as_deref(),
                reinforce,
                gate_threshold,
            ) {
                Ok(p) => p,
                Err(e) => return Response::internal(e),
            };
            match render_recall(&packaged) {
                Ok(text) => Response::ok(text),
                Err(e) => Response::internal(e),
            }
        }
        Request::Remember { content, namespace } => {
            match reg.remember(&content, namespace.as_deref()) {
                Ok(id) => Response::ok(format!("stored node {id}")),
                Err(e) => Response::internal(e),
            }
        }
        Request::Relate {
            from_id,
            to_id,
            relation,
            namespace,
        } => match reg.relate(from_id, to_id, &relation, namespace.as_deref()) {
            Ok(edge) => Response::ok(format!(
                "linked node {from_id} -> node {to_id} ({relation}) as edge {edge}"
            )),
            // A bad relation label / missing endpoint is a caller error.
            Err(e) => Response::invalid_params(e),
        },
        Request::Ingest {
            session,
            turns,
            namespace,
            capture,
        } => {
            let turns: Vec<Turn> = turns
                .into_iter()
                .map(|t| Turn {
                    speaker: t.speaker,
                    text: t.text,
                    at_ms: t.at_ms,
                })
                .collect();
            match reg.ingest_conversation(
                &session,
                &turns,
                namespace.as_deref(),
                capture.unwrap_or(false),
            ) {
                Ok(summary) => Response::ok(format!(
                    "ingested {} turns ({} semantic nodes)",
                    summary.episodic, summary.semantic
                )),
                Err(e) => Response::internal(e),
            }
        }
        Request::Stats { namespace } => match reg.stats(namespace.as_deref()) {
            Ok(stats) => Response::ok(format_stats(&stats)),
            Err(e) => Response::internal(e),
        },
        Request::PullPending { limit, namespace } => {
            match reg.pull_pending(limit.map(|l| l as usize), namespace.as_deref()) {
                Ok(text) => Response::ok(text),
                Err(e) => Response::internal(e),
            }
        }
        Request::ExtractionStatus { namespace } => {
            match reg.extraction_status(namespace.as_deref()) {
                Ok(text) => Response::ok(text),
                Err(e) => Response::internal(e),
            }
        }
    }
}

/// Render a [`PackagedRecall`](crate::memory::PackagedRecall) to the `recall`
/// payload: the readable context block (or the `(no relevant memory)` sentinel
/// when nothing packaged) followed by the compact `{node_id, score}` NODES list
/// the agent feeds to `relate`. The hook keys off this exact shape.
fn render_recall(packaged: &crate::memory::PackagedRecall) -> Result<String, serde_json::Error> {
    let refs: Vec<_> = packaged
        .hits
        .iter()
        .map(|h| serde_json::json!({ "node_id": h.node_id.0, "score": h.score }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryRegistry, StubProvider};
    use crate::proto::{ErrKind, Response, TurnInput};
    use std::sync::Arc;

    /// A stub-backed registry on a tempdir DB — no model download, no socket.
    fn stub_registry() -> (MemoryRegistry, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reg = MemoryRegistry::file_backed_unlocked_with(
            Arc::new(StubProvider),
            dir.path().join("memory.db"),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        (reg, dir)
    }

    fn ok_text(resp: Response) -> String {
        match resp {
            Response::Ok { text } => text,
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn remember_then_recall_round_trips_through_dispatch() {
        let (mut reg, _dir) = stub_registry();
        let stored = ok_text(dispatch(
            &mut reg,
            Request::Remember {
                content: "the gate is on raw activation scale".into(),
                namespace: None,
            },
        ));
        assert!(stored.starts_with("stored node "), "got: {stored}");

        let recalled = ok_text(dispatch(
            &mut reg,
            Request::Recall {
                query: "gate activation scale".into(),
                limit: Some(5),
                namespace: None,
                reinforce: Some(false),
                gate_threshold: None,
            },
        ));
        // Same shape the MCP tool produced: readable block + NODES trailer.
        assert!(
            recalled.contains("## NODES (for `relate`)"),
            "got: {recalled}"
        );
    }

    #[test]
    fn recall_with_no_matches_renders_sentinel_and_empty_nodes() {
        let (mut reg, _dir) = stub_registry();
        let text = ok_text(dispatch(
            &mut reg,
            Request::Recall {
                query: "nothing has been stored yet".into(),
                limit: Some(5),
                namespace: None,
                reinforce: Some(false),
                gate_threshold: None,
            },
        ));
        assert!(text.starts_with("(no relevant memory)"), "got: {text}");
        assert!(text.contains("## NODES (for `relate`)\n[]"), "got: {text}");
    }

    #[test]
    fn relate_bad_label_is_invalid_params_not_internal() {
        let (mut reg, _dir) = stub_registry();
        // Two real nodes so only the relation label is wrong.
        ok_text(dispatch(
            &mut reg,
            Request::Remember {
                content: "node a".into(),
                namespace: None,
            },
        ));
        ok_text(dispatch(
            &mut reg,
            Request::Remember {
                content: "node b".into(),
                namespace: None,
            },
        ));
        let resp = dispatch(
            &mut reg,
            Request::Relate {
                from_id: 1,
                to_id: 2,
                relation: "definitely-not-a-relation".into(),
                namespace: None,
            },
        );
        assert!(
            matches!(
                resp,
                Response::Err {
                    kind: ErrKind::InvalidParams,
                    ..
                }
            ),
            "a bad relation label must be a caller error: {resp:?}"
        );
    }

    #[test]
    fn stats_dispatch_matches_format_stats() {
        let (mut reg, _dir) = stub_registry();
        let text = ok_text(dispatch(&mut reg, Request::Stats { namespace: None }));
        assert!(text.contains("nodes:"));
        assert!(text.contains("health grade:"));
    }

    #[test]
    fn ingest_reports_counts() {
        let (mut reg, _dir) = stub_registry();
        let text = ok_text(dispatch(
            &mut reg,
            Request::Ingest {
                session: "s1".into(),
                turns: vec![
                    TurnInput {
                        speaker: "user".into(),
                        text: "we decided to use a daemon".into(),
                        at_ms: None,
                    },
                    TurnInput {
                        speaker: "assistant".into(),
                        text: "agreed, on-demand shared daemon".into(),
                        at_ms: None,
                    },
                ],
                namespace: None,
                capture: None,
            },
        ));
        assert!(text.starts_with("ingested "), "got: {text}");
    }
}
