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
//!   1. **Phase 1** (brief global lock): resolve namespace handles and do fast
//!      registry bookkeeping, then DROP the global lock.
//!   2. **Phase 2** (namespace locks only): do graph work under `Memory`; recall
//!      telemetry may then lock its `PolicyStore` while retaining that `Memory`
//!      lock. The global registry lock is never held in this phase.
//!   3. **Phase 3** (brief global lock): after all namespace locks are dropped,
//!      commit result-dependent shared counters and return the rendered reply.
//!
//! LOCK-ORDERING INVARIANT: global resolution must be dropped before any
//! per-namespace lock. Recall telemetry acquires `Memory` then `PolicyStore`.
//! No path may acquire the global registry lock while either namespace lock is
//! held. This prevents cycles while allowing distinct namespaces' phase-2 work
//! to proceed independently.

use std::sync::{Arc, Mutex};

use anamnesis::graph::{ScopePath, Timestamp};

use crate::memory::migration::MigrationRuntime;
use crate::memory::{
    self, MemoryRegistry, NamespaceCompatibility, NamespaceProbe, NamespaceResolution,
    PolicyStoreState, RecallEvent, Turn,
};
use crate::proto::{RecallEventKind, Request, Response};

mod enrich;
mod extract;
mod graph;
mod mgmt;
mod render;
#[cfg(test)]
mod tests;

pub use render::format_stats;

#[derive(Clone)]
pub(crate) struct DaemonRuntimeContext {
    pub(crate) registry: Arc<Mutex<MemoryRegistry>>,
    pub(crate) migrations: Arc<MigrationRuntime>,
}

#[derive(Clone)]
pub struct DispatchRuntime {
    registry: Arc<Mutex<MemoryRegistry>>,
    migrations: Option<Arc<MigrationRuntime>>,
}

impl DispatchRuntime {
    pub fn without_auto_migration(registry: Arc<Mutex<MemoryRegistry>>) -> Self {
        Self {
            registry,
            migrations: None,
        }
    }

    pub(crate) fn daemon(context: DaemonRuntimeContext) -> Self {
        Self {
            registry: context.registry,
            migrations: Some(context.migrations),
        }
    }
}

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
    let runtime = DispatchRuntime::without_auto_migration(Arc::clone(registry));
    dispatch_two_phase(&runtime, req)
}

pub(crate) fn dispatch_two_phase(runtime: &DispatchRuntime, req: Request) -> Response {
    let Some(migrations) = runtime.migrations.as_ref() else {
        return dispatch_registry(&runtime.registry, req);
    };
    let namespace = request_namespace(&req);
    let probe = {
        runtime
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .prepare_namespace_probe(namespace)
    };
    let resolution = match probe {
        Ok(NamespaceProbe::Resolved(resolution)) => Ok(resolution),
        Ok(NamespaceProbe::Inspect { key, pending }) => {
            match memory::inspect_pending_embedding_compatibility(&pending) {
                Ok(NamespaceCompatibility::Ready) => {
                    return dispatch_registry(&runtime.registry, req);
                }
                Ok(mismatch) => Ok(runtime
                    .registry
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .schedule_namespace_migration(key, pending, mismatch)),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    match resolution {
        Ok(NamespaceResolution::Ready(handle)) => {
            drop(handle);
            dispatch_registry(&runtime.registry, req)
        }
        Ok(NamespaceResolution::StartMigration(pending)) => {
            let key = pending.namespace.clone();
            match migrations.spawn_once(key.clone(), pending) {
                Ok(()) => Response::internal(format!(
                    "namespace {key:?} is migrating its embedding space; retry after migration"
                )),
                Err(error) => {
                    let message = error.to_string();
                    runtime
                        .registry
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .finish_namespace_migration(&key, Err(message.clone()));
                    Response::internal(message)
                }
            }
        }
        Ok(NamespaceResolution::Migrating) => Response::internal(
            "namespace embedding migration is in progress; retry after migration",
        ),
        Ok(NamespaceResolution::MigrationFailed(message)) => Response::internal(message),
        Err(error) => Response::internal(error),
    }
}

fn request_namespace(req: &Request) -> Option<&str> {
    match req {
        Request::Recall { namespace, .. }
        | Request::Remember { namespace, .. }
        | Request::Relate { namespace, .. }
        | Request::Ingest { namespace, .. }
        | Request::Stats { namespace, .. }
        | Request::PullPending { namespace, .. }
        | Request::ExtractionStatus { namespace }
        | Request::ExtractionScan { namespace, .. }
        | Request::StageExtraction { namespace, .. }
        | Request::RecordExtractionFailure { namespace, .. }
        | Request::ExtractionAuditList { namespace, .. }
        | Request::UpdateExtractionCandidateAudit { namespace, .. }
        | Request::UpdateExtractionRelationAudit { namespace, .. }
        | Request::Update { namespace, .. }
        | Request::Forget { namespace, .. }
        | Request::Supersede { namespace, .. }
        | Request::List { namespace, .. }
        | Request::Get { namespace, .. }
        | Request::Graph { namespace, .. } => namespace.as_deref(),
    }
}
fn persist_recall_event(
    namespace: &str,
    policy: &memory::PolicyStoreHandle,
    event_kind: RecallEventKind,
    query: &str,
    scope: Option<String>,
    knowledge_only: bool,
    trace: memory::RecallGateTrace,
) {
    let query_chars = match u64::try_from(query.chars().count()) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                namespace,
                ?event_kind,
                %error,
                "recall telemetry event construction failed"
            );
            return;
        }
    };
    let auto_extract_node_count = match u64::try_from(trace.auto_extract_node_count) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                namespace,
                ?event_kind,
                %error,
                "recall telemetry event construction failed"
            );
            return;
        }
    };
    let event = RecallEvent {
        at_ms: Timestamp::now().0,
        namespace: namespace.to_owned(),
        event_kind,
        query_chars,
        scope,
        knowledge_only,
        has_hits: trace.has_hits,
        readout_pass: trace.readout_pass,
        cosine_pass: trace.cosine_pass,
        eligible: trace.eligible,
        top_score: trace.top_score,
        top_cosine: trace.top_cosine,
        gate_threshold: trace.gate_threshold,
        cosine_gate: trace.cosine_gate,
        result_node_ids: trace.result_node_ids,
        auto_extract_node_count,
    };

    match MemoryRegistry::policy_store(policy) {
        Ok(mut state) => match &mut *state {
            PolicyStoreState::Ready(store) => {
                if let Err(error) = store.insert_recall_event(&event) {
                    tracing::warn!(
                        namespace,
                        event_kind = ?event.event_kind,
                        %error,
                        "recall telemetry persistence failed"
                    );
                }
            }
            PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Disabled { .. } => {
                tracing::warn!(
                    namespace,
                    event_kind = ?event.event_kind,
                    error = "policy store was not ready after initialization",
                    "recall telemetry policy store was not ready after initialization"
                );
            }
        },
        Err(error) => {
            tracing::warn!(
                namespace,
                event_kind = ?event.event_kind,
                %error,
                "recall telemetry policy store initialization failed"
            );
        }
    }
}

fn dispatch_registry(registry: &Arc<Mutex<MemoryRegistry>>, req: Request) -> Response {
    match req {
        Request::Recall {
            query,
            limit,
            namespace,
            reinforce,
            gate_threshold,
            cosine_gate,
            knowledge_only,
            scope,
            tag,
            event_kind,
        } => {
            let limit = limit.unwrap_or(20) as usize;

            // Phase 1: bump intent counters and resolve both namespace handles.
            // `namespace_handles` creates no policy database connection or schema.
            let (handles, effective_reinforce) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                reg.ops.recalls += 1;
                if reinforce == Some(true) || (reinforce.is_none() && reg.reinforce_on_recall) {
                    reg.ops.reinforcing_recalls += 1;
                }
                let effective_reinforce = reinforce.unwrap_or(reg.reinforce_on_recall);
                match reg.namespace_handles(namespace.as_deref()) {
                    Ok(handles) => (handles, effective_reinforce),
                    Err(e) => {
                        reg.ops.dispatch_errors += 1;
                        return Response::internal(e);
                    }
                }
            };

            // Phase 2: perform graph recall, render its response, then persist
            // telemetry while preserving the Memory -> PolicyStore lock order.
            let phase2 = {
                let mut mem = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
                match memory::mem_recall_packaged_gated_filtered(
                    &mut mem,
                    &query,
                    limit,
                    effective_reinforce,
                    memory::RecallFilters {
                        gate: gate_threshold,
                        cosine_gate,
                        scope: scope.as_deref(),
                        tag: tag.as_deref(),
                        knowledge_only: knowledge_only.unwrap_or(false),
                    },
                ) {
                    Ok(memory::RecallOutcome { packaged, trace }) => {
                        match render::render_recall(&packaged) {
                            Ok(text) => {
                                persist_recall_event(
                                    &handles.key,
                                    &handles.policy,
                                    event_kind.unwrap_or(RecallEventKind::Unknown),
                                    &query,
                                    scope,
                                    knowledge_only.unwrap_or(false),
                                    trace,
                                );
                                Ok((Response::ok(text), packaged.context.trim().is_empty()))
                            }
                            Err(e) => Err(Response::internal(e)),
                        }
                    }
                    Err(e) => Err(Response::internal(e)),
                }
            };

            // Phase 3: both namespace locks are dropped before updating global
            // counters or returning the already-rendered response.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            match phase2 {
                Ok((response, empty)) => {
                    if empty {
                        reg.ops.empty_recalls += 1;
                    }
                    response
                }
                Err(response) => {
                    reg.ops.dispatch_errors += 1;
                    response
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
            scope,
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
            let scope = match scope
                .as_deref()
                .map(ScopePath::new)
                .transpose()
                .map(|scope| scope.unwrap_or_else(ScopePath::universal))
            {
                Ok(scope) => scope,
                Err(e) => {
                    let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                    reg.ops.dispatch_errors += 1;
                    reg.ops.ingest_errors += 1;
                    return Response::invalid_params(e);
                }
            };

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
                    scope,
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
        Request::Stats { namespace, recall } => {
            let recall_requested = recall == Some(true);
            // Phase 1: resolve all needed namespace handles. Policy resolution
            // only creates an uninitialized handle; opening it occurs in phase 2.
            let (handle, policy) = {
                let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
                if recall_requested {
                    match reg.namespace_handles(namespace.as_deref()) {
                        Ok(handles) => (handles.memory, Some(handles.policy)),
                        Err(e) => {
                            reg.ops.dispatch_errors += 1;
                            return Response::internal(e);
                        }
                    }
                } else {
                    match reg.namespace_handle(namespace.as_deref()) {
                        Ok(handle) => (handle, None),
                        Err(e) => {
                            reg.ops.dispatch_errors += 1;
                            return Response::internal(e);
                        }
                    }
                }
            };
            // Phase 2: flush, graph stats, and usage totals under Memory. Recall
            // telemetry opens and queries PolicyStore only while retaining Memory,
            // preserving the Memory -> PolicyStore order without the global lock.
            let result = {
                let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
                mem.flush_all().and_then(|()| mem.stats()).map(|stats| {
                    let (total, stale) = memory::mem_usage_totals(&mem);
                    let recall_stats = policy.as_ref().and_then(|policy| {
                        match MemoryRegistry::policy_store(policy) {
                            Ok(mut state) => match &mut *state {
                                PolicyStoreState::Ready(store) => {
                                    match store.recall_stats() {
                                        Ok(stats) => Some(stats),
                                        Err(error) => {
                                            tracing::warn!(
                                                %error,
                                                "recall telemetry stats query failed"
                                            );
                                            None
                                        }
                                    }
                                }
                                PolicyStoreState::Uninitialized { .. }
                                | PolicyStoreState::Disabled { .. } => {
                                    tracing::warn!(
                                        "recall telemetry policy store was not ready after initialization"
                                    );
                                    None
                                }
                            },
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "recall telemetry policy store initialization failed"
                                );
                                None
                            }
                        }
                    });
                    (stats, total, stale, recall_stats)
                })
            };
            // Phase 3: all namespace locks and policy I/O are complete before
            // reading live registry counters and formatting the response.
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            match result {
                Ok((stats, total, stale, recall_stats)) => {
                    let ns_key = reg.canonical_ns_key(namespace.as_deref());
                    let backlog = reg.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0);
                    let usage = memory::format_usage_report(
                        &reg.ops,
                        backlog,
                        reg.seen_turn_keys.len(),
                        total,
                        stale,
                    );
                    if recall_requested {
                        Response::ok(format!(
                            "{}\n{}\n{}",
                            render::format_stats(&stats),
                            usage,
                            render::format_recall_stats(recall_stats.as_ref())
                        ))
                    } else {
                        Response::ok(format!("{}\n{}", render::format_stats(&stats), usage))
                    }
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
        Request::ExtractionScan {
            namespace,
            profile,
            min_turns,
            max_turns,
        } => extract::dispatch_scan(registry, namespace, profile, min_turns, max_turns),
        Request::StageExtraction {
            namespace,
            profile,
            llm_duration_ms,
            sources,
            extraction,
        } => extract::dispatch_stage(
            registry,
            namespace,
            profile,
            llm_duration_ms,
            sources,
            extraction,
        ),
        Request::RecordExtractionFailure {
            namespace,
            profile,
            turn_count,
            llm_invoked,
            error_kind,
            duration_ms,
        } => extract::dispatch_record_failure(
            registry,
            namespace,
            profile,
            turn_count,
            llm_invoked,
            error_kind,
            duration_ms,
        ),
        Request::ExtractionAuditList { namespace, limit } => {
            extract::dispatch_audit_list(registry, namespace, limit)
        }
        Request::UpdateExtractionCandidateAudit {
            namespace,
            candidate_id,
            support,
            contamination,
            reviewer,
        } => extract::dispatch_update_candidate_audit(
            registry,
            namespace,
            candidate_id,
            support,
            contamination,
            reviewer,
        ),
        Request::UpdateExtractionRelationAudit {
            namespace,
            relation_id,
            verdict,
            reviewer,
        } => extract::dispatch_update_relation_audit(
            registry,
            namespace,
            relation_id,
            verdict,
            reviewer,
        ),
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
        Request::Graph {
            seeds,
            query,
            depth,
            limit,
            namespace,
        } => mgmt::dispatch_graph(registry, seeds, query, depth, limit, namespace.as_deref()),
    }
}
