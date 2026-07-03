//! The shared op→text core. One pure function maps a [`proto::Request`](crate::proto::Request) to a
//! [`proto::Response`](crate::proto::Response) by calling the registry and formatting the result exactly
//! as the MCP tools used to — so every path (the daemon serving the bespoke
//! socket, and the `--embedded serve` in-process path) produces byte-identical
//! output. This module has NO rmcp dependency; MCP lives only in `server.rs`.
//!
//! # Two-phase locking (registry-lock-starvation fix, O2)
//!
//! `dispatch` takes `&Arc<Mutex<MemoryRegistry>>`, NOT a held `&mut
//! MemoryRegistry` — that distinction is the whole fix. Every arm below runs in
//! up to three phases:
//!
//!   1. **Phase 1** (brief global lock): resolve the namespace's
//!      `Arc<Mutex<Memory>>` handle via [`MemoryRegistry::namespace_handle`],
//!      do any fast pre-op bookkeeping that reads/writes registry-shared state
//!      (an `ops` counter bump, the turn-key dedup filter), then DROP the
//!      global lock.
//!   2. **Phase 2** (namespace lock only): do the expensive embed/ingest/
//!      recall work against the locked `Memory`. The global registry lock is
//!      NOT held here — a concurrent request against a DIFFERENT namespace can
//!      run phase 1/2/3 concurrently with this one.
//!   3. **Phase 3** (brief global lock): re-lock the registry to commit
//!      result-dependent shared state (`recalls`/`remembers`/`relates`, O1's
//!      `dispatch_errors`/`ingest_errors`/`empty_recalls`, `seen_turn_keys`/
//!      `unextracted`) and format the reply.
//!
//! LOCK-ORDERING INVARIANT: always acquire the global registry lock THEN a
//! per-namespace lock, NEVER the reverse, and NEVER hold both across blocking
//! work. Every arm below locks `registry`, extracts an `Arc` handle (or the
//! data it needs), and drops the `MutexGuard` (each `{ ... }` block below ends
//! with the guard going out of scope) BEFORE locking the per-namespace handle
//! in phase 2. This makes the two mutexes strictly hierarchical — a thread can
//! never be waiting on the global lock while holding a namespace lock, so a
//! cycle (and therefore a deadlock) is structurally impossible. Namespace
//! isolation follows from the same split: two different namespaces' phase-2
//! work never contends on any lock at all.

use std::sync::{Arc, Mutex};

use anamnesis::graph::Timestamp;
use anamnesis::memory::MemoryStats;

use crate::memory::{self, MemoryRegistry, Turn};
use crate::proto::{Request, Response};

/// Run one request against the shared `registry` and return the
/// consumer-ready reply, following the two-phase locking discipline documented
/// on this module.
///
/// The caller owns serialization: the daemon and the embedded `Backend::Local`
/// path both call this inside `spawn_blocking` on a cloned `Arc` — never while
/// holding a `MutexGuard` themselves — so this function's own brief internal
/// locks are the only locks in play. Caller errors (a bad relation label /
/// missing endpoint) map to [`Response::invalid_params`]; everything else to
/// [`Response::internal`].
pub fn dispatch(registry: &Arc<Mutex<MemoryRegistry>>, req: Request) -> Response {
    match req {
        Request::Recall {
            query,
            limit,
            namespace,
            reinforce,
            gate_threshold,
        } => {
            let limit = limit.unwrap_or(20) as usize;

            // Phase 1: bump intent counters, resolve the namespace handle.
            let (handle, effective_reinforce) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.recalls += 1;
                if reinforce == Some(true) || (reinforce.is_none() && reg.reinforce_on_recall) {
                    reg.ops.reinforcing_recalls += 1;
                }
                let effective_reinforce = reinforce.unwrap_or(reg.reinforce_on_recall);
                match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => (h, effective_reinforce),
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                }
            };

            // Phase 2: namespace lock only — the expensive search/tick work.
            let result = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                memory::mem_recall_packaged_gated(
                    &mut mem,
                    &query,
                    limit,
                    effective_reinforce,
                    gate_threshold,
                )
            };

            // Phase 3: commit result-dependent counters, format the reply.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            let packaged = match result {
                Ok(p) => p,
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    return Response::internal(e);
                }
            };
            // An empty package is the daemon's "nothing to inject" signal (τ-gate
            // trip or no hits): the same condition render_recall collapses to the
            // "(no relevant memory)" sentinel.
            if packaged.context.trim().is_empty() {
                reg.ops.empty_recalls += 1;
            }
            match render_recall(&packaged) {
                Ok(text) => Response::ok(text),
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    Response::internal(e)
                }
            }
        }
        Request::Remember { content, namespace } => {
            // Phase 1: bump the intent counter, resolve the namespace handle.
            let handle = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.remembers += 1;
                match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => h,
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                }
            };
            // Phase 2: namespace lock only.
            let result = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                memory::mem_remember(&mut mem, &content)
            };
            // Phase 3: commit / format.
            match result {
                Ok(id) => Response::ok(format!("stored node {id}")),
                Err(e) => {
                    let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                    reg.ops.dispatch_errors += 1;
                    Response::internal(e)
                }
            }
        }
        Request::Relate {
            from_id,
            to_id,
            relation,
            namespace,
        } => {
            // Phase 1: bump the intent counter, parse the relation label
            // (a caller error here never touches any `Memory`), resolve handle.
            let handle = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.relates += 1;
                let parsed = match memory::parse_relation(&relation) {
                    Ok(r) => r,
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::invalid_params(e);
                    }
                };
                match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => (h, parsed),
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::invalid_params(e);
                    }
                }
            };
            let (handle, parsed) = handle;
            // Phase 2: namespace lock only.
            let result = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                memory::mem_relate(&mut mem, from_id, to_id, parsed)
            };
            // Phase 3: commit / format. A bad relation label / missing endpoint
            // is a caller error, but still a failed tool call the daemon handled.
            match result {
                Ok(edge) => Response::ok(format!(
                    "linked node {from_id} -> node {to_id} ({relation}) as edge {edge}"
                )),
                Err(e) => {
                    let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                    reg.ops.dispatch_errors += 1;
                    Response::invalid_params(e)
                }
            }
        }
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
            let capture = capture.unwrap_or(false);

            // Phase 1: dedup-filter against seen_turn_keys (registry state,
            // read-only + fast), resolve the namespace handle.
            let (handle, decisions) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                let decisions = memory::filter_capture_decisions(
                    &reg.seen_turn_keys,
                    &session,
                    &turns,
                    capture,
                );
                match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => (h, decisions),
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        reg.ops.ingest_errors += 1;
                        return Response::internal(e);
                    }
                }
            };

            // Phase 2: namespace lock only — the expensive embed/ingest work.
            let phase2 = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                memory::mem_ingest_conversation(&mut mem, &session, decisions)
            };

            // Phase 3: commit registry-shared state (captured_turns,
            // seen_turn_keys, unextracted) regardless of overall outcome, then
            // format the reply.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            reg.ops.captured_turns += phase2.committed.len() as u64;
            for (epi_id, key) in phase2.committed {
                reg.seen_turn_keys.insert(key);
                reg.unextracted.push(epi_id);
            }
            match phase2.outcome {
                Ok(summary) => Response::ok(format!(
                    "ingested {} turns ({} semantic nodes)",
                    summary.episodic, summary.semantic
                )),
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    reg.ops.ingest_errors += 1;
                    Response::internal(e)
                }
            }
        }
        Request::Stats { namespace } => {
            // Phase 1: resolve the namespace handle.
            let handle = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => h,
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                }
            };
            // Phase 2: namespace lock only — flush, full stats, usage totals.
            let result = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                mem.flush_all().and_then(|()| mem.stats()).map(|stats| {
                    let (total, stale) = memory::mem_usage_totals(&mem);
                    (stats, total, stale)
                })
            };
            // Phase 3: commit / format using the registry's live counters.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            match result {
                Ok((stats, total, stale)) => {
                    let usage = memory::format_usage_report(
                        &reg.ops,
                        reg.unextracted.len(),
                        reg.seen_turn_keys.len(),
                        total,
                        stale,
                    );
                    Response::ok(format!("{}\n{}", format_stats(&stats), usage))
                }
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    Response::internal(e)
                }
            }
        }
        Request::PullPending {
            limit,
            namespace: _,
        } => {
            // Phase 1: bump the intent counter, CLAIM (drain) up to `limit` ids
            // from the front of the shared queue, resolve the (always-default)
            // namespace handle. Claiming here — not just peeking — means two
            // concurrent pulls can never deliver the same node twice.
            let (handle, claimed) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.extraction_pulls += 1;
                let take = limit
                    .map(|l| l as usize)
                    .unwrap_or(crate::capture::DEFAULT_PULL_LIMIT)
                    .min(reg.unextracted.len());
                let claimed: Vec<_> = reg.unextracted.drain(..take).collect();
                match reg.namespace_handle(None) {
                    Ok(h) => (h, claimed),
                    Err(e) => {
                        reg.unextracted.splice(0..0, claimed);
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                }
            };
            // Phase 2: namespace lock only.
            let now_ms = Timestamp::now().0;
            let (items, unprocessed) = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                crate::capture::pull_claimed(&mut mem, &claimed, now_ms)
            };
            // Phase 3: restore anything not durably marked, format the reply.
            if !unprocessed.is_empty() {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.unextracted.splice(0..0, unprocessed);
            }
            Response::ok(serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string()))
        }
        Request::ExtractionStatus { namespace } => {
            // Pure registry-state read — no `Memory` access, one brief lock.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            match reg.extraction_status(namespace.as_deref()) {
                Ok(text) => Response::ok(text),
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    Response::internal(e)
                }
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
    use std::sync::{Arc, Mutex};

    /// A stub-backed registry on a tempdir DB — no model download, no socket.
    /// Wrapped in the same `Arc<Mutex<_>>` shape `dispatch` expects from its
    /// real callers (daemon.rs / server.rs), so these tests exercise the exact
    /// two-phase locking path, not a bypass of it.
    fn stub_registry() -> (Arc<Mutex<MemoryRegistry>>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reg = MemoryRegistry::file_backed_unlocked_with(
            Arc::new(StubProvider),
            dir.path().join("memory.db"),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        (Arc::new(Mutex::new(reg)), dir)
    }

    fn ok_text(resp: Response) -> String {
        match resp {
            Response::Ok { text } => text,
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    /// C1 regression (registry-lock starvation): while namespace A's `Memory`
    /// lock is HELD (standing in for a slow PreCompact embed of ~50 turns), a
    /// namespace-B request must still COMPLETE.
    ///
    /// Deterministic by construction, no sleep: the test thread holds `_guard_a`
    /// — namespace A's `Arc<Mutex<Memory>>` guard, obtained via the SAME
    /// `namespace_handle` primitive `dispatch` uses internally — for the ENTIRE
    /// test. `dispatch` for namespace B runs on another thread and its result is
    /// observed via `std::sync::mpsc::Receiver::recv_timeout`: under the
    /// two-phase design this returns almost immediately (B's phase 2 never
    /// touches A's lock at all); under the OLD single-global-lock design (see
    /// `red_check_single_global_lock_blocks_unrelated_namespace` in the sibling
    /// crate history / this PR's description) an analogous hold of the ONE
    /// shared lock blocks a different namespace's request for as long as the
    /// hold lasts — which is exactly the registry-lock-starvation bug (C1).
    ///
    /// Structural assertion: `registry.try_lock()` succeeds WHILE `_guard_a` is
    /// still held — proving the global lock was already released before this
    /// per-namespace lock was taken (the two are never held together), which is
    /// the concrete, checkable form of the lock-ordering invariant documented
    /// on this module.
    #[test]
    fn namespace_b_dispatch_completes_while_namespace_a_memory_lock_is_held() {
        let (registry, _dir) = stub_registry();

        // Resolve namespace A's handle via a brief global-lock hold (phase 1's
        // own primitive), matching exactly what `dispatch` does internally.
        let handle_a = {
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            reg.namespace_handle(Some("a")).unwrap()
        };

        // HOLD namespace A's Memory lock for the rest of this test — simulating
        // a slow ingest/embed in flight on namespace A.
        let _guard_a = handle_a.lock().unwrap_or_else(|p| p.into_inner());

        // Structural check: the global registry lock is free RIGHT NOW, even
        // though a per-namespace lock is held — the two never nest.
        assert!(
            registry.try_lock().is_ok(),
            "the global registry lock must already be released while a \
             per-namespace Memory lock is held"
        );

        // Namespace B's request, run on another thread, through the real
        // production `dispatch` entry point.
        let (tx, rx) = std::sync::mpsc::channel();
        let registry_b = registry.clone();
        std::thread::spawn(move || {
            let resp = dispatch(
                &registry_b,
                Request::Remember {
                    content: "namespace b write while a is locked".into(),
                    namespace: Some("b".into()),
                },
            );
            tx.send(resp).unwrap();
        });

        // Bounded wait: the assertion is "it completed at all", not "how fast" —
        // under the fixed design this resolves almost instantly since namespace
        // B's phase 2 never needs namespace A's lock; a regression back to the
        // old design would make this hang until `_guard_a` is dropped, which
        // never happens inside this test, so it would time out here instead of
        // depending on any fragile timing assumption.
        let resp = rx.recv_timeout(std::time::Duration::from_secs(5)).expect(
            "namespace B's dispatch must complete while namespace A's \
                 Memory lock is held — a timeout here means the global \
                 registry lock is being held across per-namespace work again \
                 (the C1 registry-lock-starvation regression)",
        );
        let text = ok_text(resp);
        assert!(text.starts_with("stored node "), "got: {text}");

        // `_guard_a` is still held here — the whole namespace-B round trip
        // above happened while it was alive.
        drop(_guard_a);
    }

    #[test]
    fn remember_then_recall_round_trips_through_dispatch() {
        let (reg, _dir) = stub_registry();
        let stored = ok_text(dispatch(
            &reg,
            Request::Remember {
                content: "the gate is on raw activation scale".into(),
                namespace: None,
            },
        ));
        assert!(stored.starts_with("stored node "), "got: {stored}");

        let recalled = ok_text(dispatch(
            &reg,
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
        let (reg, _dir) = stub_registry();
        let text = ok_text(dispatch(
            &reg,
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
        let (reg, _dir) = stub_registry();
        // Two real nodes so only the relation label is wrong.
        ok_text(dispatch(
            &reg,
            Request::Remember {
                content: "node a".into(),
                namespace: None,
            },
        ));
        ok_text(dispatch(
            &reg,
            Request::Remember {
                content: "node b".into(),
                namespace: None,
            },
        ));
        let resp = dispatch(
            &reg,
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
        let (reg, _dir) = stub_registry();
        let text = ok_text(dispatch(&reg, Request::Stats { namespace: None }));
        assert!(text.contains("nodes:"));
        assert!(text.contains("health grade:"));
        // The dogfood usage section is appended after the stats block.
        assert!(text.contains("usage (this daemon)"), "got: {text}");
        assert!(text.contains("extraction backlog:"), "got: {text}");
    }

    #[test]
    fn ingest_reports_counts() {
        let (reg, _dir) = stub_registry();
        let text = ok_text(dispatch(
            &reg,
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

    /// O1 (silent-failure observability): the `stats` usage surface must render
    /// the daemon-observed failure counters, and they must increment when the
    /// daemon handles a failing request or a recall that returns nothing to
    /// inject. RED until the counters exist + are wired into dispatch.
    #[test]
    fn stats_renders_failure_counters_that_increment() {
        let (reg, _dir) = stub_registry();

        // Baseline: the failure section exists and starts at zero.
        let s0 = ok_text(dispatch(&reg, Request::Stats { namespace: None }));
        assert!(
            s0.contains("failures (this daemon):"),
            "stats must render a failures section:\n{s0}"
        );
        assert!(
            s0.contains("dispatch errors: 0 (0 ingest)"),
            "baseline dispatch errors must be zero:\n{s0}"
        );
        assert!(
            s0.contains("empty recalls: 0"),
            "baseline empty recalls must be zero:\n{s0}"
        );

        // (a) A recall against an empty graph returns an empty package ("nothing
        // to inject") — a daemon-observed anomaly ⇒ empty_recalls++.
        let recalled = ok_text(dispatch(
            &reg,
            Request::Recall {
                query: "nothing stored yet".into(),
                limit: Some(5),
                namespace: None,
                reinforce: Some(false),
                gate_threshold: None,
            },
        ));
        assert!(
            recalled.starts_with("(no relevant memory)"),
            "expected the empty-recall sentinel:\n{recalled}"
        );

        // (b) A relate to a non-existent endpoint errors — a daemon-observed
        // failure ⇒ dispatch_errors++ (not an ingest, so the subset stays 0).
        let stored = ok_text(dispatch(
            &reg,
            Request::Remember {
                content: "only node".into(),
                namespace: None,
            },
        ));
        assert!(stored.starts_with("stored node "), "got: {stored}");
        let resp = dispatch(
            &reg,
            Request::Relate {
                from_id: 1,
                to_id: u64::MAX,
                relation: "related".into(),
                namespace: None,
            },
        );
        assert!(
            matches!(resp, Response::Err { .. }),
            "relate to a missing endpoint must error: {resp:?}"
        );

        // GREEN expectation: the rendered counters reflect exactly one of each.
        let s1 = ok_text(dispatch(&reg, Request::Stats { namespace: None }));
        assert!(
            s1.contains("dispatch errors: 1 (0 ingest)"),
            "one failed request must show as one dispatch error:\n{s1}"
        );
        assert!(
            s1.contains("empty recalls: 1"),
            "one empty recall must be counted:\n{s1}"
        );
    }
}
