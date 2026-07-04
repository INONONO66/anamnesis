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

use crate::memory::{self, MemoryRegistry, Turn};
use crate::proto::{Request, Response};

mod mgmt;
mod render;
#[cfg(test)]
mod tests;

pub use render::format_stats;

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
            scope,
            tag,
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
                memory::mem_recall_packaged_gated_filtered(
                    &mut mem,
                    &query,
                    limit,
                    effective_reinforce,
                    gate_threshold,
                    scope.as_deref(),
                    tag.as_deref(),
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
            match render::render_recall(&packaged) {
                Ok(text) => Response::ok(text),
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    Response::internal(e)
                }
            }
        }
        Request::Remember {
            content,
            namespace,
            tags,
            metadata,
            scope,
        } => {
            // Phase 1: bump the intent counter, parse tags/metadata/scope (a
            // caller error here never touches any `Memory`), resolve the
            // namespace handle.
            let handle = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.remembers += 1;
                let opts = match memory::build_note_options(tags, metadata, scope) {
                    Ok(o) => o,
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::invalid_params(e);
                    }
                };
                match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => (h, opts),
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                }
            };
            let (handle, opts) = handle;
            // Phase 2: namespace lock only.
            let result = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                memory::mem_remember_with(&mut mem, &content, opts)
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

            // Phase 1: resolve the namespace handle FIRST — on first open this
            // rebuilds `seen_turn_keys` for this namespace, so the dedup filter
            // below sees restart-durable state — then dedup-filter against
            // seen_turn_keys (registry state, read-only + fast).
            let (handle, decisions) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                let handle = match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => h,
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        reg.ops.ingest_errors += 1;
                        return Response::internal(e);
                    }
                };
                let decisions = memory::filter_capture_decisions(
                    &reg.seen_turn_keys,
                    &session,
                    &turns,
                    capture,
                );
                (handle, decisions)
            };

            // Phase 2: namespace lock only — the expensive embed/ingest work.
            let phase2 = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                memory::mem_ingest_conversation(&mut mem, &session, decisions)
            };

            // Phase 3: commit registry-shared state (captured_turns,
            // seen_turn_keys, unextracted) regardless of overall outcome, then
            // format the reply. The queue slot is enqueued under the SAME
            // canonical key the ingest actually wrote into (P1-T4: isolated
            // per namespace, not a single global queue).
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            reg.ops.captured_turns += phase2.committed.len() as u64;
            let ns_key = reg.canonical_ns_key(namespace.as_deref());
            let mut newly_queued = Vec::with_capacity(phase2.committed.len());
            for (epi_id, key) in phase2.committed {
                reg.seen_turn_keys.insert(key);
                newly_queued.push(epi_id);
            }
            reg.unextracted
                .entry(ns_key)
                .or_default()
                .extend(newly_queued);
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
            // Phase 3: commit / format using the registry's live counters. The
            // extraction backlog is THIS request's own namespace's queue
            // length, not a count summed across every namespace.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            match result {
                Ok((stats, total, stale)) => {
                    let ns_key = reg.canonical_ns_key(namespace.as_deref());
                    let backlog = reg.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0);
                    let usage = memory::format_usage_report(
                        &reg.ops,
                        backlog,
                        reg.seen_turn_keys.len(),
                        total,
                        stale,
                    );
                    Response::ok(format!("{}\n{}", render::format_stats(&stats), usage))
                }
                Err(e) => {
                    reg.ops.dispatch_errors += 1;
                    Response::internal(e)
                }
            }
        }
        Request::PullPending { limit, namespace } => {
            // Phase 1: bump the intent counter, resolve the REQUESTED
            // namespace's handle FIRST — on first open this rebuilds that
            // namespace's `unextracted` bucket from durable node metadata —
            // then CLAIM (drain) up to `limit` ids from the front of THAT
            // namespace's (now rebuilt) queue. Claiming here — not just
            // peeking — means two concurrent pulls can never deliver the same
            // node twice.
            let (handle, ns_key, claimed) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.extraction_pulls += 1;
                let handle = match reg.namespace_handle(namespace.as_deref()) {
                    Ok(h) => h,
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                };
                let ns_key = reg.canonical_ns_key(namespace.as_deref());
                let take = limit
                    .map(|l| l as usize)
                    .unwrap_or(crate::capture::DEFAULT_PULL_LIMIT)
                    .min(reg.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0));
                let claimed: Vec<_> = reg
                    .unextracted
                    .get_mut(&ns_key)
                    .map(|q| q.drain(..take).collect())
                    .unwrap_or_default();
                (handle, ns_key, claimed)
            };
            // Phase 2: namespace lock only.
            let now_ms = Timestamp::now().0;
            let (items, unprocessed) = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                crate::capture::pull_claimed(&mut mem, &claimed, now_ms)
            };
            // Phase 3: restore anything not durably marked, into the SAME
            // namespace's queue, format the reply.
            if !unprocessed.is_empty() {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.unextracted
                    .entry(ns_key)
                    .or_default()
                    .splice(0..0, unprocessed);
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
        Request::Update {
            id,
            new_content,
            namespace,
        } => mgmt::dispatch_update(registry, id, &new_content, namespace.as_deref()),
        Request::Forget {
            id,
            reason,
            hard,
            namespace,
        } => mgmt::dispatch_forget(registry, id, reason, hard, namespace.as_deref()),
        Request::Supersede {
            new_id,
            old_id,
            namespace,
        } => mgmt::dispatch_supersede(registry, new_id, old_id, namespace.as_deref()),
        Request::List {
            min_salience,
            limit,
            node_type,
            tag,
            namespace,
            scope,
            metadata,
        } => mgmt::dispatch_list(
            registry,
            min_salience,
            limit,
            node_type,
            tag,
            namespace.as_deref(),
            scope,
            metadata,
        ),
        Request::Get { id, namespace } => mgmt::dispatch_get(registry, id, namespace.as_deref()),
    }
}
