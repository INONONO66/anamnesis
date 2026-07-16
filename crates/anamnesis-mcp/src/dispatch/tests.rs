use super::*;
use crate::capture::META_CAPTURE;
use crate::memory::{
    AbstentionStats, AutoExposureStats, CosineStats, EventKindStats, MemoryRegistry, PolicyStore,
    PolicyStoreState, RecallStats, StubProvider, SweepPoint,
};
use crate::proto::{
    ErrKind, ExtractionErrorKind, RecallEventKind, Response, StageExtractionResult, TurnInput,
};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

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
fn stub_registry_with_policy_migration_failure() -> (Arc<Mutex<MemoryRegistry>>, tempfile::TempDir)
{
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("memory.db");
    let connection = rusqlite::Connection::open(&db).expect("open policy migration database");
    connection
        .execute_batch(
            "
            CREATE TABLE mcp_schema_version (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                version INTEGER NOT NULL CHECK(version = 0)
            );
            CREATE TABLE recall_events (id INTEGER PRIMARY KEY);
            ",
        )
        .expect("seed policy schema that rejects the v1 version write");
    drop(connection);

    let reg = MemoryRegistry::file_backed_unlocked_with(
        Arc::new(StubProvider),
        db,
        dir.path().to_path_buf(),
        "default".to_string(),
        false,
    );
    (Arc::new(Mutex::new(reg)), dir)
}

fn stub_registry_with_future_policy_schema() -> (Arc<Mutex<MemoryRegistry>>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("memory.db");
    let connection = rusqlite::Connection::open(&db).expect("open future policy database");
    connection
        .execute_batch(
            "
            CREATE TABLE mcp_schema_version (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                version INTEGER NOT NULL
            );
            INSERT INTO mcp_schema_version (id, version) VALUES (1, 3);
            CREATE TABLE recall_events (id INTEGER PRIMARY KEY);
            ",
        )
        .expect("seed future policy schema");
    drop(connection);

    let reg = MemoryRegistry::file_backed_unlocked_with(
        Arc::new(StubProvider),
        db,
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
                tags: None,
                metadata: None,
                scope: None,
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
            tags: None,
            metadata: None,
            scope: None,
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
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        },
    ));
    // Same shape the MCP tool produced: readable block + NODES trailer.
    assert!(
        recalled.contains("## NODES (for `relate`)"),
        "got: {recalled}"
    );
    let nodes_json = recalled
        .split("## NODES (for `relate`)\n")
        .nth(1)
        .expect("recall reply has a NODES section");
    let nodes: Vec<serde_json::Value> =
        serde_json::from_str(nodes_json.trim()).expect("NODES section is a JSON array");
    assert!(
        nodes.iter().all(|node| node["cosine"].is_number()),
        "NODES refs must include cosine for calibration: {nodes:?}"
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
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        },
    ));
    assert!(text.starts_with("(no relevant memory)"), "got: {text}");
    assert!(text.contains("## NODES (for `relate`)\n[]"), "got: {text}");
}
#[test]
fn recall_telemetry_write_failure_preserves_response_and_dispatch_counters() {
    let (registry, _dir) = stub_registry();
    let request = Request::Recall {
        query: "stable recall output despite telemetry failure".into(),
        limit: Some(5),
        namespace: None,
        reinforce: Some(false),
        gate_threshold: None,
        cosine_gate: None,
        knowledge_only: None,
        scope: None,
        tag: None,
        event_kind: None,
    };

    let before = ok_text(dispatch(&registry, request.clone()));
    let (handles, dispatch_errors) = {
        let mut registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        let handles = registry_guard
            .namespace_handles(None)
            .expect("resolve default graph and policy handles");
        (handles, registry_guard.ops.dispatch_errors)
    };
    let event_count = {
        let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("open default policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("policy store must be ready after the successful recall");
        };
        let event_count = store
            .recall_event_count_for_test()
            .expect("count successful recall telemetry");
        store
            .install_recall_event_insert_failure_trigger_for_test()
            .expect("install approved recall_events failure trigger");
        event_count
    };

    let after = ok_text(dispatch(&registry, request));
    assert_eq!(
        after, before,
        "telemetry persistence is best-effort and must not alter the recall reply"
    );

    let handles = {
        let mut registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        registry_guard
            .namespace_handles(None)
            .expect("resolve default graph and policy handles")
    };
    let post_event_count = {
        let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("reopen default policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("policy store must remain ready after a failed telemetry insert");
        };
        store
            .recall_event_count_for_test()
            .expect("count recall telemetry after failed insert")
    };
    assert_eq!(
        post_event_count, event_count,
        "a failed transactional insert must not leave a recall_events row behind"
    );
    assert_eq!(
        registry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ops
            .dispatch_errors,
        dispatch_errors,
        "a best-effort telemetry failure must not be reported as a dispatch failure"
    );
}
#[test]
fn r1_recall_telemetry_deploy_gate_demo() {
    let (registry, _dir) = stub_registry();
    let namespace = "r1-deploy-gate";
    let scope = "project/r1-deploy-gate";
    let query = "r1 filtered top rollout proof";
    let canonical_namespace = {
        let registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        registry_guard.canonical_ns_key(Some(namespace))
    };
    assert_eq!(
        canonical_namespace, namespace,
        "the deterministic deploy-gate namespace must already be canonical"
    );

    let mut capture_metadata = std::collections::HashMap::new();
    capture_metadata.insert(META_CAPTURE.to_string(), "true".to_string());

    ok_text(dispatch(
        &registry,
        Request::Remember {
            content: query.into(),
            namespace: Some(namespace.into()),
            tags: None,
            metadata: Some(capture_metadata),
            scope: Some(scope.into()),
        },
    ));

    // `Remember` creates an Episodic + Semantic pair. Mark only the Semantic
    // fixture as durable so the identical, high-scoring captured Episodic node
    // must be removed by `knowledge_only` before the final top is traced.
    let (captured_episodic_id, surviving_semantic_id) = {
        let handle = {
            let mut registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
            registry_guard
                .namespace_handle(Some(namespace))
                .expect("resolve deploy-gate memory handle")
        };
        let mut memory_guard = handle.lock().unwrap_or_else(|p| p.into_inner());
        let graph = memory_guard.engine().graph();
        let mut captured_episodic_id = None;
        let mut surviving_semantic_id = None;
        for id in graph.all_node_ids() {
            let node = graph.get_node(id).expect("fixture node exists");
            if node.content == query
                && node.metadata.get(META_CAPTURE).map(String::as_str) == Some("true")
            {
                match node.node_type {
                    anamnesis::graph::KnowledgeType::Episodic => captured_episodic_id = Some(id),
                    anamnesis::graph::KnowledgeType::Semantic => surviving_semantic_id = Some(id),
                    _ => {}
                }
            }
        }
        let captured_episodic_id =
            captured_episodic_id.expect("fixture must include a captured Episodic node");
        let surviving_semantic_id =
            surviving_semantic_id.expect("fixture must include a captured Semantic node");
        memory_guard
            .set_metadata(surviving_semantic_id, META_CAPTURE, "false")
            .expect("mark only the Semantic fixture as durable");
        (captured_episodic_id.0, surviving_semantic_id.0)
    };

    let unfiltered_response = ok_text(dispatch(
        &registry,
        Request::Recall {
            query: query.into(),
            limit: Some(5),
            namespace: Some(namespace.into()),
            reinforce: Some(false),
            gate_threshold: Some(0.0),
            cosine_gate: Some(0.99),
            knowledge_only: Some(false),
            scope: Some(scope.into()),
            tag: None,
            event_kind: Some(RecallEventKind::UserPrompt),
        },
    ));
    let unfiltered_nodes_json = unfiltered_response
        .split_once("## NODES (for `relate`)\n")
        .expect("unfiltered recall response has a NODES trailer")
        .1;
    let unfiltered_nodes: Vec<serde_json::Value> =
        serde_json::from_str(unfiltered_nodes_json.trim()).expect("NODES trailer is a JSON array");
    let unfiltered_top_id = unfiltered_nodes
        .first()
        .and_then(|node| node["node_id"].as_u64())
        .expect("unfiltered recall must expose a top candidate id");
    assert_eq!(
        unfiltered_top_id, captured_episodic_id,
        "the captured Episodic node must be the actual unfiltered top candidate"
    );

    let request = Request::Recall {
        query: query.into(),
        limit: Some(5),
        namespace: Some(namespace.into()),
        reinforce: Some(false),
        gate_threshold: Some(0.0),
        cosine_gate: Some(0.99),
        knowledge_only: Some(true),
        scope: Some(scope.into()),
        tag: None,
        event_kind: Some(RecallEventKind::UserPrompt),
    };
    let first_response = dispatch(&registry, request);
    assert!(
        matches!(&first_response, Response::Ok { .. }),
        "the successful recall must return a healthy core response"
    );

    let handles = {
        let mut registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        registry_guard
            .namespace_handles(Some(namespace))
            .expect("resolve deploy-gate graph and policy handles")
    };
    assert_eq!(
        handles.key, canonical_namespace,
        "test must inspect the same canonical namespace dispatch resolved"
    );
    {
        let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("open deploy-gate policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("successful recall must initialize the telemetry policy store");
        };
        let events = store
            .read_recall_events_for_test()
            .expect("read persisted minimized recall events");
        assert_eq!(
            events.len(),
            2,
            "the unfiltered baseline and knowledge-only recall must write exactly two events"
        );
        let event = &events[1];
        assert_eq!(event.event_kind, RecallEventKind::UserPrompt);
        assert_eq!(event.namespace, canonical_namespace);
        assert_eq!(event.scope.as_deref(), Some(scope));
        assert_eq!(event.query_chars, query.chars().count() as u64);
        assert!(event.has_hits && event.readout_pass && event.cosine_pass && event.eligible);
        assert_eq!(
            event.result_node_ids.first().copied(),
            Some(surviving_semantic_id),
            "persisted filtered top must be the surviving Semantic node, not captured Episodic node {captured_episodic_id}"
        );
        assert!(
            !event.result_node_ids.contains(&captured_episodic_id),
            "knowledge_only must exclude the captured high-scoring Episodic node"
        );
        assert!(
            event.top_score.is_some() && event.top_cosine.is_some(),
            "eligible filtered result must persist top score and cosine"
        );
        assert!(
            !store
                .recall_events_contain_raw_value_for_test(query)
                .expect("inspect minimized event for raw query data"),
            "recall telemetry must not retain the raw query"
        );
        println!(
            "R1_DEPLOY_GATE filtered_top node_id={} node_type=semantic prefilter_top_node_id={} namespace={} scope={} top_score={:?} top_cosine={:?} event_kind={:?} query_chars={} has_hits={} readout_pass={} cosine_pass={} eligible={} result_node_ids={:?} event_rows={} auto_extract_node_count={}",
            surviving_semantic_id,
            unfiltered_top_id,
            event.namespace,
            event
                .scope
                .as_deref()
                .expect("deploy-gate event must retain scope"),
            event.top_score,
            event.top_cosine,
            event.event_kind,
            event.query_chars,
            event.has_hits,
            event.readout_pass,
            event.cosine_pass,
            event.eligible,
            event.result_node_ids,
            events.len(),
            event.auto_extract_node_count,
        );
    }

    let fail_open_probe = Request::Recall {
        query: "deterministic fail-open response probe".into(),
        limit: Some(5),
        namespace: Some(namespace.into()),
        reinforce: Some(false),
        gate_threshold: None,
        cosine_gate: None,
        knowledge_only: Some(true),
        scope: Some(scope.into()),
        tag: None,
        event_kind: Some(RecallEventKind::UserPrompt),
    };
    let control_graph_snapshot = {
        let mut memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        memory_guard
            .engine_mut()
            .snapshot("r1 deploy-gate fail-open control")
            .expect("snapshot graph state before the control recall")
    };
    let event_count_before_control = {
        let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("reopen deploy-gate policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("policy store must remain ready before the control probe");
        };
        store
            .recall_event_count_for_test()
            .expect("count recall telemetry before control probe")
    };
    let control_response = dispatch(&registry, fail_open_probe.clone());
    assert!(
        matches!(&control_response, Response::Ok { .. }),
        "the control fail-open probe must return a healthy core response"
    );
    let control_response_bytes =
        serde_json::to_vec(&control_response).expect("serialize control recall response");
    {
        let mut memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        memory_guard
            .engine_mut()
            .restore(&control_graph_snapshot)
            .expect("restore identical graph state before forced telemetry failure");
    }
    let event_count_after_control = {
        let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("reopen deploy-gate policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("policy store must remain ready after the control probe");
        };
        let event_count = store
            .recall_event_count_for_test()
            .expect("count recall telemetry after control probe");
        assert_eq!(
            event_count,
            event_count_before_control + 1,
            "the healthy control probe must persist exactly one telemetry row"
        );
        store
            .install_recall_event_insert_failure_trigger_for_test()
            .expect("install approved recall_events failure trigger");
        event_count
    };

    let observed_insert_attempts = Arc::new(AtomicUsize::new(0));
    let observer_attempts = Arc::clone(&observed_insert_attempts);
    let _observer = PolicyStore::install_operation_observer_for_test(Arc::new(move || {
        observer_attempts.fetch_add(1, Ordering::SeqCst);
    }));

    let failed_telemetry_response = dispatch(&registry, fail_open_probe);
    assert!(
        matches!(&failed_telemetry_response, Response::Ok { .. }),
        "forced telemetry failure must leave core recall healthy"
    );
    assert_eq!(
        observed_insert_attempts.load(Ordering::SeqCst),
        1,
        "the triggered dispatch must attempt exactly one telemetry insert"
    );
    assert_eq!(
        serde_json::to_vec(&failed_telemetry_response)
            .expect("serialize recall response after telemetry failure"),
        control_response_bytes,
        "forced telemetry write failure must preserve the control response bytes"
    );

    let post_failure_event_count = {
        let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy_guard =
            MemoryRegistry::policy_store(&handles.policy).expect("reopen deploy-gate policy store");
        let PolicyStoreState::Ready(store) = &mut *policy_guard else {
            panic!("policy store must remain ready after a failed telemetry insert");
        };
        store
            .recall_event_count_for_test()
            .expect("count recall telemetry after forced failure")
    };
    assert_eq!(
        post_failure_event_count, event_count_after_control,
        "forced telemetry write failure must not leave an event row behind"
    );
    println!(
        "R1_DEPLOY_GATE fail_open response_bytes_identical=true core_response_healthy=true control_row_increment={} insert_attempts={} event_rows_unchanged={} event_count={}",
        event_count_after_control == event_count_before_control + 1,
        observed_insert_attempts.load(Ordering::SeqCst),
        post_failure_event_count == event_count_after_control,
        post_failure_event_count,
    );
}

#[test]
fn recall_telemetry_uses_canonical_namespace_unicode_char_count_and_unknown_kind() {
    let (registry, _dir) = stub_registry();
    let raw_namespace = "Telemetry/Namespace";
    let query = "한국어 기억";
    let expected_namespace = {
        let registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        registry_guard.canonical_ns_key(Some(raw_namespace))
    };

    ok_text(dispatch(
        &registry,
        Request::Recall {
            query: query.into(),
            limit: Some(5),
            namespace: Some(raw_namespace.into()),
            reinforce: Some(false),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        },
    ));

    let handles = {
        let mut registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        registry_guard
            .namespace_handles(Some(raw_namespace))
            .expect("resolve telemetry namespace handles")
    };
    assert_eq!(
        handles.key, expected_namespace,
        "test must inspect the same canonical namespace dispatch resolved"
    );
    let _memory_guard = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
    let mut policy_guard =
        MemoryRegistry::policy_store(&handles.policy).expect("open telemetry policy store");
    let PolicyStoreState::Ready(store) = &mut *policy_guard else {
        panic!("recall must initialize the telemetry policy store");
    };
    let events = store
        .read_recall_events_for_test()
        .expect("read persisted recall telemetry");
    assert_eq!(
        events.len(),
        1,
        "one recall must persist one telemetry event"
    );
    let event = &events[0];
    assert_eq!(event.namespace, expected_namespace);
    assert_eq!(
        event.query_chars, 6,
        "count Unicode scalar values, not UTF-8 bytes"
    );
    assert_eq!(event.event_kind, RecallEventKind::Unknown);
    assert!(
        !store
            .recall_events_contain_raw_value_for_test(query)
            .expect("inspect recall_events values for raw query data"),
        "minimized recall telemetry must never retain the raw query"
    );
}

#[test]
fn recall_policy_sql_runs_after_registry_release_with_memory_then_policy_lock_order() {
    let (registry, _dir) = stub_registry();
    let (memory, policy) = {
        let mut registry_guard = registry.lock().unwrap_or_else(|p| p.into_inner());
        let handles = registry_guard
            .namespace_handles(Some("lock-order"))
            .expect("phase 1 resolves graph and policy handles");
        assert!(
            matches!(
                *handles.policy.lock().unwrap_or_else(|p| p.into_inner()),
                PolicyStoreState::Uninitialized { .. }
            ),
            "phase 1 must create only an uninitialized policy handle"
        );
        (handles.memory, handles.policy)
    };

    let observed_operations = Arc::new(AtomicUsize::new(0));
    let observer_registry = Arc::clone(&registry);
    let observer_memory = Arc::clone(&memory);
    let observer_operations = Arc::clone(&observed_operations);
    let _observer = PolicyStore::install_operation_observer_for_test(Arc::new(move || {
        assert!(
            observer_registry.try_lock().is_ok(),
            "policy SQL must run only after the registry guard is released"
        );
        assert!(
            observer_memory.try_lock().is_err(),
            "policy SQL must run while its corresponding Memory lock is held \
             (Memory -> PolicyStore order)"
        );
        observer_operations.fetch_add(1, Ordering::SeqCst);
    }));

    ok_text(dispatch(
        &registry,
        Request::Recall {
            query: "lock ordering telemetry".into(),
            limit: Some(5),
            namespace: Some("lock-order".into()),
            reinforce: Some(false),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        },
    ));

    assert_eq!(
        observed_operations.load(Ordering::SeqCst),
        2,
        "the dispatch must perform exactly policy initialization then recall-event insertion"
    );
    assert!(
        matches!(
            *policy.lock().unwrap_or_else(|p| p.into_inner()),
            PolicyStoreState::Ready(_)
        ),
        "phase 2 must initialize the policy store before inserting telemetry"
    );
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
            tags: None,
            metadata: None,
            scope: None,
        },
    ));
    ok_text(dispatch(
        &reg,
        Request::Remember {
            content: "node b".into(),
            namespace: None,
            tags: None,
            metadata: None,
            scope: None,
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
    let text = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: None,
        },
    ));
    assert!(text.contains("nodes:"));
    assert!(text.contains("health grade:"));
    // The dogfood usage section is appended after the stats block.
    assert!(text.contains("usage (this daemon)"), "got: {text}");
    assert!(text.contains("extraction backlog:"), "got: {text}");
}
#[test]
fn stats_without_recall_is_byte_identical_to_existing_graph_stats_and_usage() {
    let (reg, _dir) = stub_registry();

    let no_flag = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: None,
        },
    ));
    let explicitly_disabled = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: Some(false),
        },
    ));

    assert_eq!(
        no_flag, explicitly_disabled,
        "stats without --recall must preserve the graph-stats-plus-usage bytes"
    );
    assert!(
        !no_flag.contains("recall telemetry"),
        "stats without --recall must not append telemetry: {no_flag}"
    );
}

#[test]
fn stats_recall_unavailable_policy_preserves_graph_stats_and_appends_unavailable() {
    let (reg, _dir) = stub_registry_with_future_policy_schema();

    let text = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: Some(true),
        },
    ));

    assert!(
        text.contains("nodes:"),
        "graph stats must still render: {text}"
    );
    assert!(
        text.contains("usage (this daemon)"),
        "usage must still render: {text}"
    );
    assert!(
        text.contains("telemetry unavailable"),
        "future policy state must be reported without failing stats: {text}"
    );
}

#[test]
fn recall_policy_migration_failure_is_fail_open_and_disables_telemetry_without_rows() {
    let (registry, dir) = stub_registry_with_policy_migration_failure();
    let database = dir.path().join("memory.db");
    let rows_before: u64 = rusqlite::Connection::open(&database)
        .expect("open seeded policy migration database")
        .query_row("SELECT COUNT(*) FROM recall_events", [], |row| row.get(0))
        .expect("count seeded telemetry rows");
    let dispatch_errors_before = registry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .ops
        .dispatch_errors;

    let text = ok_text(dispatch(
        &registry,
        Request::Recall {
            query: "core recall survives policy migration failure".into(),
            limit: Some(5),
            namespace: None,
            reinforce: Some(false),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        },
    ));
    assert!(
        text.starts_with("(no relevant memory)") && text.contains("## NODES (for `relate`)\n[]"),
        "policy migration failure must not replace the core recall response: {text}"
    );

    let handles = registry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .namespace_handles(None)
        .expect("resolve policy state after fail-open recall");
    let policy_guard = handles.policy.lock().unwrap_or_else(|p| p.into_inner());
    let disabled_reason = match &*policy_guard {
        PolicyStoreState::Disabled { reason } => reason,
        PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Ready(_) => {
            panic!("policy migration failure must disable telemetry")
        }
    };
    assert!(
        disabled_reason.contains("update policy schema version")
            && disabled_reason.contains("sqlite code:")
            && disabled_reason.contains("sqlite category:"),
        "policy migration failure must retain actionable SQLite evidence: {disabled_reason}"
    );
    drop(policy_guard);
    let rows_after: u64 = rusqlite::Connection::open(&database)
        .expect("reopen seeded policy migration database")
        .query_row("SELECT COUNT(*) FROM recall_events", [], |row| row.get(0))
        .expect("count telemetry rows after fail-open recall");
    assert_eq!(
        rows_after, rows_before,
        "disabled telemetry must not create a recall row"
    );
    assert_eq!(
        registry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ops
            .dispatch_errors,
        dispatch_errors_before,
        "best-effort policy migration failure must not become a dispatch failure"
    );

    let stats = ok_text(dispatch(
        &registry,
        Request::Stats {
            namespace: None,
            recall: Some(true),
        },
    ));
    assert!(
        stats.contains("telemetry unavailable"),
        "disabled policy telemetry must be visible in stats: {stats}"
    );
}

#[test]
fn stats_recall_query_failure_preserves_graph_usage_and_telemetry_rows() {
    let (registry, dir) = stub_registry();
    ok_text(dispatch(
        &registry,
        Request::Recall {
            query: "initialize telemetry before corrupting a stats value".into(),
            limit: Some(5),
            namespace: None,
            reinforce: Some(false),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        },
    ));
    let handles = registry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .namespace_handles(None)
        .expect("resolve initialized telemetry store");
    let rows_before = {
        let policy_guard = handles.policy.lock().unwrap_or_else(|p| p.into_inner());
        let PolicyStoreState::Ready(store) = &*policy_guard else {
            panic!("successful recall must initialize telemetry");
        };
        store
            .recall_event_count_for_test()
            .expect("count telemetry before stats-query failure")
    };

    rusqlite::Connection::open(dir.path().join("memory.db"))
        .expect("open telemetry database for deterministic stats corruption")
        .execute(
            "UPDATE recall_events SET top_cosine = 'not-a-numeric-cosine'",
            [],
        )
        .expect("corrupt only the stats-read value");
    let graph_and_usage = ok_text(dispatch(
        &registry,
        Request::Stats {
            namespace: None,
            recall: Some(false),
        },
    ));
    let fail_open = ok_text(dispatch(
        &registry,
        Request::Stats {
            namespace: None,
            recall: Some(true),
        },
    ));
    assert_eq!(
        fail_open,
        format!(
            "{graph_and_usage}\nrecall telemetry (injection eligibility, not delivery)\n  telemetry unavailable\n"
        ),
        "stats-query failure must preserve graph stats and usage while visibly disabling telemetry"
    );

    let policy_guard = handles.policy.lock().unwrap_or_else(|p| p.into_inner());
    let PolicyStoreState::Ready(store) = &*policy_guard else {
        panic!("a stats query failure must not falsely change policy state");
    };
    assert_eq!(
        store
            .recall_event_count_for_test()
            .expect("count telemetry after failed stats query"),
        rows_before,
        "a failed read-only stats query must not alter persisted telemetry rows"
    );
}

#[test]
fn stats_recall_renderer_empty_contract_is_exact() {
    let stats = RecallStats {
        total_attempts: 0,
        by_event_kind: Vec::new(),
        abstentions: AbstentionStats {
            empty: 0,
            readout_only: 0,
            cosine_only: 0,
            both: 0,
        },
        cosine: CosineStats {
            samples: 0,
            nulls: 0,
            p50: None,
            p90: None,
            p95: None,
        },
        auto_exposure: AutoExposureStats {
            eligible_events: 0,
            events_with_auto: 0,
            result_slots: 0,
            auto_slots: 0,
        },
        sweep: (80..=90)
            .map(|hundredths| SweepPoint {
                threshold: hundredths as f64 / 100.0,
                eligible: 0,
                attempts: 0,
            })
            .collect(),
    };

    assert_eq!(
        render::format_recall_stats(Some(&stats)),
        concat!(
            "recall telemetry (injection eligibility, not delivery)\n",
            "  attempts: 0\n",
            "  by event kind:\n",
            "  abstentions:\n",
            "    empty: 0\n",
            "    readout-only: 0\n",
            "    cosine-only: 0\n",
            "    both: 0\n",
            "  cosine:\n",
            "    samples: 0\n",
            "    nulls: 0\n",
            "    p50: N/A\n",
            "    p90: N/A\n",
            "    p95: N/A\n",
            "  auto exposure (exposure, not quality):\n",
            "    event exposure: N/A (0/0)\n",
            "    slot exposure: N/A (0/0)\n",
            "  eligibility sweep:\n",
            "    0.80: eligible 0 / attempts 0\n",
            "    0.81: eligible 0 / attempts 0\n",
            "    0.82: eligible 0 / attempts 0\n",
            "    0.83: eligible 0 / attempts 0\n",
            "    0.84: eligible 0 / attempts 0\n",
            "    0.85: eligible 0 / attempts 0\n",
            "    0.86: eligible 0 / attempts 0\n",
            "    0.87: eligible 0 / attempts 0\n",
            "    0.88: eligible 0 / attempts 0\n",
            "    0.89: eligible 0 / attempts 0\n",
            "    0.90: eligible 0 / attempts 0\n",
        )
    );
}

#[test]
fn stats_recall_renderer_populated_contract_is_exact() {
    let stats = RecallStats {
        total_attempts: 6,
        by_event_kind: vec![
            EventKindStats {
                event_kind: RecallEventKind::SessionStart,
                attempts: 1,
                eligible: 0,
            },
            EventKindStats {
                event_kind: RecallEventKind::Tool,
                attempts: 2,
                eligible: 1,
            },
            EventKindStats {
                event_kind: RecallEventKind::Unknown,
                attempts: 1,
                eligible: 0,
            },
            EventKindStats {
                event_kind: RecallEventKind::UserPrompt,
                attempts: 2,
                eligible: 1,
            },
        ],
        abstentions: AbstentionStats {
            empty: 1,
            readout_only: 1,
            cosine_only: 1,
            both: 1,
        },
        cosine: CosineStats {
            samples: 5,
            nulls: 1,
            p50: Some(0.84),
            p90: Some(0.90),
            p95: Some(0.90),
        },
        auto_exposure: AutoExposureStats {
            eligible_events: 2,
            events_with_auto: 1,
            result_slots: 5,
            auto_slots: 2,
        },
        sweep: (80..=90)
            .zip([3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1])
            .map(|(hundredths, eligible)| SweepPoint {
                threshold: hundredths as f64 / 100.0,
                eligible,
                attempts: 6,
            })
            .collect(),
    };

    assert_eq!(
        render::format_recall_stats(Some(&stats)),
        concat!(
            "recall telemetry (injection eligibility, not delivery)\n",
            "  attempts: 6\n",
            "  by event kind:\n",
            "    SessionStart: attempts 1, eligible 0\n",
            "    Tool: attempts 2, eligible 1\n",
            "    Unknown: attempts 1, eligible 0\n",
            "    UserPrompt: attempts 2, eligible 1\n",
            "  abstentions:\n",
            "    empty: 1\n",
            "    readout-only: 1\n",
            "    cosine-only: 1\n",
            "    both: 1\n",
            "  cosine:\n",
            "    samples: 5\n",
            "    nulls: 1\n",
            "    p50: 0.840\n",
            "    p90: 0.900\n",
            "    p95: 0.900\n",
            "  auto exposure (exposure, not quality):\n",
            "    event exposure: 50.0% (1/2)\n",
            "    slot exposure: 40.0% (2/5)\n",
            "  eligibility sweep:\n",
            "    0.80: eligible 3 / attempts 6\n",
            "    0.81: eligible 3 / attempts 6\n",
            "    0.82: eligible 3 / attempts 6\n",
            "    0.83: eligible 2 / attempts 6\n",
            "    0.84: eligible 2 / attempts 6\n",
            "    0.85: eligible 2 / attempts 6\n",
            "    0.86: eligible 2 / attempts 6\n",
            "    0.87: eligible 2 / attempts 6\n",
            "    0.88: eligible 2 / attempts 6\n",
            "    0.89: eligible 1 / attempts 6\n",
            "    0.90: eligible 1 / attempts 6\n",
        )
    );
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
            scope: None,
        },
    ));
    assert!(text.starts_with("ingested "), "got: {text}");
}

#[test]
fn dispatch_capture_ingest_stamps_scope_and_capture_marker() {
    let (reg, _dir) = stub_registry();
    let text = ok_text(dispatch(
        &reg,
        Request::Ingest {
            session: "scoped-capture".into(),
            turns: vec![
                TurnInput {
                    speaker: "user".into(),
                    text: "scoped capture alpha".into(),
                    at_ms: Some(1),
                },
                TurnInput {
                    speaker: "assistant".into(),
                    text: "scoped capture beta".into(),
                    at_ms: Some(2),
                },
            ],
            namespace: None,
            capture: Some(true),
            scope: Some("project/anamnesis".into()),
        },
    ));
    assert!(text.starts_with("ingested "), "got: {text}");

    let handle = {
        let mut registry = reg.lock().unwrap_or_else(|p| p.into_inner());
        registry.namespace_handle(None).unwrap()
    };
    let mem = handle.lock().unwrap_or_else(|p| p.into_inner());
    let graph = mem.engine().graph();
    let nodes: Vec<_> = graph
        .all_node_ids()
        .into_iter()
        .map(|id| graph.get_node(id).unwrap())
        .filter(|node| {
            matches!(
                node.node_type,
                anamnesis::graph::KnowledgeType::Episodic
                    | anamnesis::graph::KnowledgeType::Semantic
            )
        })
        .collect();
    assert!(nodes.len() >= 4, "expected episodic + semantic nodes");
    for node in nodes {
        assert_eq!(node.origin.scope.as_str(), "project/anamnesis");
        assert_eq!(
            node.metadata.get(META_CAPTURE).map(String::as_str),
            Some("true")
        );
    }
}

#[test]
fn ingest_invalid_scope_returns_invalid_params() {
    let (reg, _dir) = stub_registry();
    let resp = dispatch(
        &reg,
        Request::Ingest {
            session: "bad-scope".into(),
            turns: vec![TurnInput {
                speaker: "user".into(),
                text: "bad scope".into(),
                at_ms: Some(1),
            }],
            namespace: None,
            capture: Some(true),
            scope: Some("project//bad".into()),
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
        "got: {resp:?}"
    );
}

/// O1 (silent-failure observability): the `stats` usage surface must render
/// the daemon-observed failure counters, and they must increment when the
/// daemon handles a failing request or a recall that returns nothing to
/// inject. RED until the counters exist + are wired into dispatch.
// ── management tools: update/forget/supersede/list/get ─────────────────
fn remember_id(reg: &Arc<Mutex<MemoryRegistry>>, content: &str) -> u64 {
    let text = ok_text(dispatch(
        reg,
        Request::Remember {
            content: content.into(),
            namespace: None,
            tags: None,
            metadata: None,
            scope: None,
        },
    ));
    text.strip_prefix("stored node ")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("expected 'stored node <id>', got: {text}"))
}

#[test]
fn update_edits_content_via_dispatch() {
    let (reg, _dir) = stub_registry();
    let id = remember_id(&reg, "the deploy script is bash");

    let resp = ok_text(dispatch(
        &reg,
        Request::Update {
            id,
            new_content: "the deploy script is now written in rust".into(),
            namespace: None,
        },
    ));
    assert_eq!(resp, format!("updated node {id}"));

    let got = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    let view: serde_json::Value = serde_json::from_str(&got).expect("get returns JSON");
    assert_eq!(
        view["content"], "the deploy script is now written in rust",
        "get must reflect the update: {view}"
    );
}

#[test]
fn update_nonexistent_id_is_invalid_params_not_internal() {
    let (reg, _dir) = stub_registry();
    let resp = dispatch(
        &reg,
        Request::Update {
            id: u64::MAX,
            new_content: "x".into(),
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
        "a missing id must be a caller error, not internal: {resp:?}"
    );
}

#[test]
fn forget_soft_hides_then_stats_counts_retracted() {
    let (reg, _dir) = stub_registry();
    let id = remember_id(&reg, "the api key was rotated");

    let resp = ok_text(dispatch(
        &reg,
        Request::Forget {
            id,
            reason: Some("stale credential".into()),
            hard: Some(false),
            namespace: None,
        },
    ));
    assert!(resp.contains("soft delete"), "got: {resp}");

    // Soft-forgotten nodes stay readable via `get` (audit trail).
    let got = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    let view: serde_json::Value = serde_json::from_str(&got).expect("get returns JSON");
    assert_eq!(view["retracted"], true, "got: {view}");

    let stats = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: None,
        },
    ));
    assert!(
        stats.contains("retracted:            1"),
        "stats must count the retraction: {stats}"
    );
}

#[test]
fn forget_hard_removes_node() {
    let (reg, _dir) = stub_registry();
    let id = remember_id(&reg, "a node bound for hard deletion");

    let resp = ok_text(dispatch(
        &reg,
        Request::Forget {
            id,
            reason: None,
            hard: Some(true),
            namespace: None,
        },
    ));
    assert!(resp.contains("hard delete"), "got: {resp}");

    let after = dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    );
    assert!(
        matches!(
            after,
            Response::Err {
                kind: ErrKind::InvalidParams,
                ..
            }
        ),
        "a hard-deleted node must no longer be readable: {after:?}"
    );
}

#[test]
fn forget_nonexistent_id_is_invalid_params() {
    let (reg, _dir) = stub_registry();
    let resp = dispatch(
        &reg,
        Request::Forget {
            id: u64::MAX,
            reason: None,
            hard: Some(false),
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
        "got: {resp:?}"
    );
}

#[test]
fn supersede_tool_links_and_sets_validity() {
    let (reg, _dir) = stub_registry();
    let old_id = remember_id(&reg, "we use postgres for storage");
    let new_id = remember_id(&reg, "we now use sqlite for storage");

    let resp = ok_text(dispatch(
        &reg,
        Request::Supersede {
            new_id,
            old_id,
            namespace: None,
        },
    ));
    assert!(
        resp.starts_with(&format!("superseded {old_id} by {new_id} (edge ")),
        "got: {resp}"
    );

    let old_view = ok_text(dispatch(
        &reg,
        Request::Get {
            id: old_id,
            namespace: None,
        },
    ));
    let old_view: serde_json::Value = serde_json::from_str(&old_view).unwrap();
    assert!(
        !old_view["valid_until"].is_null(),
        "superseded node must have a closed validity window: {old_view}"
    );

    // The underlying `relate` tool must also accept "supersedes" directly.
    let a = remember_id(&reg, "fact A");
    let b = remember_id(&reg, "fact B");
    let relate_resp = ok_text(dispatch(
        &reg,
        Request::Relate {
            from_id: b,
            to_id: a,
            relation: "supersedes".into(),
            namespace: None,
        },
    ));
    assert!(relate_resp.contains("supersedes"), "got: {relate_resp}");
}

#[test]
fn supersede_nonexistent_endpoint_is_invalid_params() {
    let (reg, _dir) = stub_registry();
    let a = remember_id(&reg, "only node");
    let resp = dispatch(
        &reg,
        Request::Supersede {
            new_id: a,
            old_id: u64::MAX,
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
        "got: {resp:?}"
    );
}

#[test]
fn list_returns_ranked_json() {
    let (reg, _dir) = stub_registry();
    let low = remember_id(&reg, "low salience note about weather");
    let high = remember_id(&reg, "high salience note about weather");
    // Reinforce the second node so it ranks above the first.
    for _ in 0..3 {
        ok_text(dispatch(
            &reg,
            Request::Recall {
                query: "high salience note about weather".into(),
                limit: Some(5),
                namespace: None,
                reinforce: Some(true),
                gate_threshold: None,
                cosine_gate: None,
                knowledge_only: None,
                scope: None,
                tag: None,
                event_kind: None,
            },
        ));
    }
    let _ = low;

    let resp = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: None,
            namespace: None,
            scope: None,
            metadata: None,
        },
    ));
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&resp).expect("list returns a JSON array");
    assert!(!items.is_empty(), "expected at least one listed node");
    for item in &items {
        assert!(item["node_id"].is_u64(), "got: {item}");
        assert!(item["content_preview"].is_string(), "got: {item}");
        assert!(item["salience"].is_number(), "got: {item}");
        assert!(item["tier"].is_string(), "got: {item}");
        assert!(item["node_type"].is_string(), "got: {item}");
        assert!(item["created_at"].is_u64(), "got: {item}");
        assert!(item["retracted"].is_boolean(), "got: {item}");
    }
    // Salience-descending order.
    for w in items.windows(2) {
        let a = w[0]["salience"].as_f64().unwrap();
        let b = w[1]["salience"].as_f64().unwrap();
        assert!(a >= b, "list must be salience-descending: {items:?}");
    }
    let ids: Vec<u64> = items
        .iter()
        .map(|v| v["node_id"].as_u64().unwrap())
        .collect();
    assert!(ids.contains(&high), "high-salience node must be listed");
}

#[test]
fn list_with_weird_filter_returns_empty_array_not_panic() {
    let (reg, _dir) = stub_registry();
    remember_id(&reg, "a normal note");
    let resp = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(999.0), // above any real salience
            limit: Some(5),
            node_type: Some("no-such-type-xyz".into()),
            tag: Some("no-such-tag-xyz".into()),
            namespace: None,
            scope: None,
            metadata: None,
        },
    ));
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&resp).expect("still valid JSON, just empty");
    assert!(items.is_empty(), "got: {items:?}");
}

#[test]
fn get_returns_full_node_json() {
    let (reg, _dir) = stub_registry();
    let id = remember_id(&reg, "the outage was caused by a bad migration");

    let resp = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    let view: serde_json::Value = serde_json::from_str(&resp).expect("get returns JSON");
    assert_eq!(view["node_id"], id);
    assert_eq!(view["content"], "the outage was caused by a bad migration");
    assert!(view["metadata"].is_object(), "got: {view}");
    assert!(view["entity_tags"].is_array(), "got: {view}");
    assert!(view["salience"].is_number(), "got: {view}");
    assert!(view["tier"].is_string(), "got: {view}");
    assert!(view["node_type"].is_string(), "got: {view}");
    assert!(view["created_at"].is_u64(), "got: {view}");
    assert!(view["updated_at"].is_u64(), "got: {view}");
    assert!(view["valid_from"].is_null() || view["valid_from"].is_u64());
    assert!(view["valid_until"].is_null() || view["valid_until"].is_u64());
    assert_eq!(view["retracted"], false);
}

#[test]
fn get_json_surfaces_origin_provenance() {
    let (reg, _dir) = stub_registry();
    let stored = ok_text(remember_with(
        &reg,
        "the outage postmortem is scoped to this project",
        None,
        None,
        Some("projA".to_string()),
    ));
    let id = stored
        .strip_prefix("stored node ")
        .and_then(|s| s.parse::<u64>().ok())
        .expect("stored node id");

    let resp = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    let view: serde_json::Value = serde_json::from_str(&resp).expect("get returns JSON");
    println!("get -> {view}");
    assert_eq!(view["scope"], "projA");
    assert!(view["peer_id"].is_string(), "got: {view}");
    assert!(view["session_id"].is_string(), "got: {view}");
    assert!(view["confidence"].is_number(), "got: {view}");
}

/// Manual-QA artifact: drives all 5 management tools through the real
/// `Request` → [`dispatch`] path (the same entry point `server.rs` uses)
/// against a real in-process registry, printing each actual response.
/// Run with `cargo test -p anamnesis-mcp mcp_mgmt_tools_demo -- --nocapture`.
#[test]
fn mcp_mgmt_tools_demo() {
    let (reg, _dir) = stub_registry();

    let stored = ok_text(dispatch(
        &reg,
        Request::Remember {
            content: "we use postgres for storage".into(),
            namespace: None,
            tags: None,
            metadata: None,
            scope: None,
        },
    ));
    println!("remember -> {stored}");
    let id = stored
        .strip_prefix("stored node ")
        .and_then(|s| s.parse::<u64>().ok())
        .expect("stored node id");

    let updated = ok_text(dispatch(
        &reg,
        Request::Update {
            id,
            new_content: "we use postgres for storage (JSONB + RLS)".into(),
            namespace: None,
        },
    ));
    println!("update -> {updated}");

    let got = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    println!("get -> {got}");

    let new_id_stored = ok_text(dispatch(
        &reg,
        Request::Remember {
            content: "we now use sqlite for storage".into(),
            namespace: None,
            tags: None,
            metadata: None,
            scope: None,
        },
    ));
    println!("remember (second node) -> {new_id_stored}");
    let new_id = new_id_stored
        .strip_prefix("stored node ")
        .and_then(|s| s.parse::<u64>().ok())
        .expect("stored node id");

    let listed = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: None,
            namespace: None,
            scope: None,
            metadata: None,
        },
    ));
    println!("list -> {listed}");

    let superseded = ok_text(dispatch(
        &reg,
        Request::Supersede {
            new_id,
            old_id: id,
            namespace: None,
        },
    ));
    println!("supersede -> {superseded}");

    let forgotten_soft = ok_text(dispatch(
        &reg,
        Request::Forget {
            id,
            reason: Some("superseded by sqlite decision".into()),
            hard: Some(false),
            namespace: None,
        },
    ));
    println!("forget (soft) -> {forgotten_soft}");

    let forgotten_hard = ok_text(dispatch(
        &reg,
        Request::Forget {
            id: new_id,
            reason: None,
            hard: Some(true),
            namespace: None,
        },
    ));
    println!("forget (hard) -> {forgotten_hard}");

    // Sanity: every response above actually succeeded (no panics reached
    // this line means all `ok_text` calls unwrapped an `Ok` response).
    assert!(updated.starts_with("updated node "));
}

/// Manual-QA artifact (P1-T4): proves the extraction queue is per-namespace
/// through the REAL `Request` → [`dispatch`] path (the same entry point
/// `server.rs` uses for `ingest_conversation` / `extract_pending` /
/// `extraction_status`). Captures a turn into "projA" and a different
/// turn into "projB", then pulls/status-checks each — the artifact must
/// visibly show A's turn NOT appearing in B's pull (and vice-versa).
/// Run with `cargo test -p anamnesis-mcp extraction_queue_per_namespace_demo -- --nocapture`.
#[test]
fn extraction_queue_per_namespace_demo() {
    let (reg, _dir) = stub_registry();

    let ingest_a = ok_text(dispatch(
        &reg,
        Request::Ingest {
            session: "s-projA".into(),
            turns: vec![TurnInput {
                speaker: "user".into(),
                text: "projA decided to use postgres".into(),
                at_ms: Some(1),
            }],
            namespace: Some("projA".into()),
            capture: Some(true),
            scope: None,
        },
    ));
    println!("projA_ingest={ingest_a}");

    let ingest_b = ok_text(dispatch(
        &reg,
        Request::Ingest {
            session: "s-projB".into(),
            turns: vec![TurnInput {
                speaker: "user".into(),
                text: "projB decided to use sqlite".into(),
                at_ms: Some(2),
            }],
            namespace: Some("projB".into()),
            capture: Some(true),
            scope: None,
        },
    ));
    println!("projB_ingest={ingest_b}");

    let status_a = ok_text(dispatch(
        &reg,
        Request::ExtractionStatus {
            namespace: Some("projA".into()),
        },
    ));
    println!("projA_status={status_a}");
    let status_b = ok_text(dispatch(
        &reg,
        Request::ExtractionStatus {
            namespace: Some("projB".into()),
        },
    ));
    println!("projB_status={status_b}");
    assert!(status_a.contains("\"pending\":1"), "got: {status_a}");
    assert!(status_b.contains("\"pending\":1"), "got: {status_b}");

    let projb_pull = ok_text(dispatch(
        &reg,
        Request::PullPending {
            limit: None,
            namespace: Some("projB".into()),
        },
    ));
    println!("projB_pull={projb_pull}");
    assert!(
        projb_pull.contains("projB decided to use sqlite"),
        "got: {projb_pull}"
    );
    assert!(
        !projb_pull.contains("projA decided to use postgres"),
        "LEAK: A appeared in B's pull: {projb_pull}"
    );

    let proja_pull = ok_text(dispatch(
        &reg,
        Request::PullPending {
            limit: None,
            namespace: Some("projA".into()),
        },
    ));
    println!("projA_pull={proja_pull}");
    assert!(
        proja_pull.contains("projA decided to use postgres"),
        "got: {proja_pull}"
    );
    assert!(
        !proja_pull.contains("projB decided to use sqlite"),
        "LEAK: B appeared in A's pull: {proja_pull}"
    );

    // Post-pull status: both namespaces drained to zero backlog.
    let status_a2 = ok_text(dispatch(
        &reg,
        Request::ExtractionStatus {
            namespace: Some("projA".into()),
        },
    ));
    println!("projA_status_after_pull={status_a2}");
    let status_b2 = ok_text(dispatch(
        &reg,
        Request::ExtractionStatus {
            namespace: Some("projB".into()),
        },
    ));
    println!("projB_status_after_pull={status_b2}");
    assert!(status_a2.contains("\"pending\":0"), "got: {status_a2}");
    assert!(status_b2.contains("\"pending\":0"), "got: {status_b2}");
}

#[test]
fn get_nonexistent_id_is_invalid_params_not_internal() {
    let (reg, _dir) = stub_registry();
    let resp = dispatch(
        &reg,
        Request::Get {
            id: u64::MAX,
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
        "a non-existent id must be a caller error, not internal: {resp:?}"
    );
}

#[test]
fn stats_renders_failure_counters_that_increment() {
    let (reg, _dir) = stub_registry();

    // Baseline: the failure section exists and starts at zero.
    let s0 = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: None,
        },
    ));
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
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
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
            tags: None,
            metadata: None,
            scope: None,
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
    let s1 = ok_text(dispatch(
        &reg,
        Request::Stats {
            namespace: None,
            recall: None,
        },
    ));
    assert!(
        s1.contains("dispatch errors: 1 (0 ingest)"),
        "one failed request must show as one dispatch error:\n{s1}"
    );
    assert!(
        s1.contains("empty recalls: 1"),
        "one empty recall must be counted:\n{s1}"
    );
}

// ── P1-T5: metadata / tags / scope on remember + filtered list/recall ──

/// Helper mirroring [`remember_id`] with tags/metadata/scope set.
fn remember_with(
    reg: &Arc<Mutex<MemoryRegistry>>,
    content: &str,
    tags: Option<Vec<String>>,
    metadata: Option<std::collections::HashMap<String, String>>,
    scope: Option<String>,
) -> Response {
    dispatch(
        reg,
        Request::Remember {
            content: content.into(),
            namespace: None,
            tags,
            metadata,
            scope,
        },
    )
}

#[test]
fn remember_with_tags_then_list_filter_by_tag_returns_only_tagged() {
    let (reg, _dir) = stub_registry();
    // `add_note_with` stamps the tag on both the Episodic and Semantic
    // nodes it creates, so the filtered list legitimately returns 2 items
    // for the one tagged `remember` call — assert by content, not a
    // single id.
    remember_with(
        &reg,
        "the auth bug was a race in the middleware",
        Some(vec!["auth".to_string()]),
        None,
        None,
    );
    remember_id(&reg, "an unrelated note about lunch");

    let resp = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: Some("auth".to_string()),
            namespace: None,
            scope: None,
            metadata: None,
        },
    ));
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&resp).expect("list returns a JSON array");
    assert!(!items.is_empty(), "expected the tagged node in the list");
    for item in &items {
        let preview = item["content_preview"].as_str().unwrap_or_default();
        assert!(
            preview.contains("the auth bug"),
            "tag filter must exclude the untagged note: {items:?}"
        );
    }
}

#[test]
fn remember_with_metadata_persists_and_get_shows_it() {
    let (reg, _dir) = stub_registry();
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("owner".to_string(), "alice".to_string());
    let id = ok_text(remember_with(
        &reg,
        "the deploy runbook lives in ops/",
        None,
        Some(metadata),
        None,
    ))
    .strip_prefix("stored node ")
    .and_then(|s| s.parse::<u64>().ok())
    .expect("stored node id");

    let got = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    let view: serde_json::Value = serde_json::from_str(&got).expect("get returns JSON");
    assert_eq!(view["metadata"]["owner"], "alice", "got: {view}");
}

#[test]
fn remember_with_scope_then_list_filter_by_scope() {
    let (reg, _dir) = stub_registry();
    remember_with(
        &reg,
        "projA-only fact",
        None,
        None,
        Some("projA".to_string()),
    );
    remember_with(
        &reg,
        "projB-only fact",
        None,
        None,
        Some("projB".to_string()),
    );

    let resp = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: None,
            namespace: None,
            scope: Some("projA".to_string()),
            metadata: None,
        },
    ));
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&resp).expect("list returns a JSON array");
    assert!(!items.is_empty(), "expected projA's node in the list");
    for item in &items {
        let preview = item["content_preview"].as_str().unwrap_or_default();
        assert!(
            preview.contains("projA-only"),
            "scope filter must exclude projB's note: {items:?}"
        );
    }
}

#[test]
fn recall_filter_by_scope_excludes_other_scope() {
    let (reg, _dir) = stub_registry();
    remember_with(
        &reg,
        "the outage postmortem for project A",
        None,
        None,
        Some("projA".to_string()),
    );
    remember_with(
        &reg,
        "the outage postmortem for project B",
        None,
        None,
        Some("projB".to_string()),
    );

    let resp = ok_text(dispatch(
        &reg,
        Request::Recall {
            query: "outage postmortem".into(),
            limit: Some(20),
            namespace: None,
            reinforce: Some(true),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: Some("projA".to_string()),
            tag: None,
            event_kind: None,
        },
    ));
    let nodes_json = resp
        .split("## NODES (for `relate`)\n")
        .nth(1)
        .expect("recall reply has a NODES section");
    let nodes: Vec<serde_json::Value> =
        serde_json::from_str(nodes_json.trim()).expect("NODES section is a JSON array");
    assert!(!nodes.is_empty(), "expected at least the projA hit: {resp}");
    for node in &nodes {
        let id = node["node_id"].as_u64().unwrap();
        let view = ok_text(dispatch(
            &reg,
            Request::Get {
                id,
                namespace: None,
            },
        ));
        assert!(
            view.contains("project A"),
            "scope filter must exclude project B's hit, got node text: {view}"
        );
    }

    // The RENDERED context block (## KNOWLEDGE / ## MEMORIES), not just the
    // compact NODES list, must exclude the other-scope hit — an agent reads
    // the rendered block, not the NODES list, so a leak there is the real bug.
    assert!(
        resp.contains("project A"),
        "expected the projA content in the rendered reply: {resp}"
    );
    assert!(
        !resp.contains("project B"),
        "recall scope filter must exclude project B's content from the \
         RENDERED reply (## KNOWLEDGE / ## MEMORIES), not just the NODES list: {resp}"
    );
    assert!(
        !resp.to_lowercase().contains("scope projb"),
        "recall scope filter must not leak project B's origin/scope line \
         into the rendered reply: {resp}"
    );
}

#[test]
fn scope_filter_keeps_universal_and_matching_drops_foreign() {
    let (reg, _dir) = stub_registry();
    remember_with(
        &reg,
        "shared outage postmortem applies everywhere",
        None,
        None,
        None,
    );
    remember_with(
        &reg,
        "project A outage postmortem detail",
        None,
        None,
        Some("projA".to_string()),
    );
    remember_with(
        &reg,
        "project B outage postmortem detail",
        None,
        None,
        Some("projB".to_string()),
    );

    let resp = ok_text(dispatch(
        &reg,
        Request::Recall {
            query: "outage postmortem detail".into(),
            limit: Some(20),
            namespace: None,
            reinforce: Some(false),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: Some("projA".to_string()),
            tag: None,
            event_kind: None,
        },
    ));

    assert!(
        resp.contains("shared outage postmortem"),
        "universal memories must survive scoped recall: {resp}"
    );
    assert!(
        resp.contains("project A outage postmortem"),
        "same-scope memories must survive scoped recall: {resp}"
    );
    assert!(
        !resp.contains("project B outage postmortem"),
        "foreign-scope memories must be dropped: {resp}"
    );
}

// ── adversarial: malformed_input (P1-T5) ────────────────────────────────

#[test]
fn list_with_malformed_metadata_filter_errors_cleanly_not_panic() {
    let (reg, _dir) = stub_registry();
    remember_id(&reg, "a note with no metadata");
    let resp = dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: None,
            namespace: None,
            scope: None,
            metadata: Some("no-equals-sign-here".to_string()),
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
        "malformed \"key=value\" metadata filter must be a clean invalid_params error, \
         not a panic: {resp:?}"
    );
}

#[test]
fn remember_with_empty_tag_does_not_panic_and_is_dropped() {
    let (reg, _dir) = stub_registry();
    let resp = remember_with(
        &reg,
        "a note with a blank tag",
        Some(vec![String::new(), "real-tag".to_string()]),
        None,
        None,
    );
    let id = ok_text(resp)
        .strip_prefix("stored node ")
        .and_then(|s| s.parse::<u64>().ok())
        .expect("empty tag must not panic; note still stores cleanly");

    let got = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    let view: serde_json::Value = serde_json::from_str(&got).unwrap();
    let tags: Vec<&str> = view["entity_tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!tags.contains(&""), "empty tag must be dropped: {tags:?}");
    assert!(tags.contains(&"real-tag"), "got: {tags:?}");
}

#[test]
fn list_with_unknown_scope_returns_empty_not_panic() {
    let (reg, _dir) = stub_registry();
    remember_id(&reg, "a universally-scoped note");
    let resp = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: None,
            namespace: None,
            scope: Some("no-such-scope-xyz".to_string()),
            metadata: None,
        },
    ));
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&resp).expect("still valid JSON, just empty");
    assert!(items.is_empty(), "got: {items:?}");
}

/// Manual-QA artifact (P1-T5): remembers a tagged+scoped+metadata note,
/// `get`s it, `list`s it via a tag filter and a scope filter, and
/// `recall`s it via a scope filter — printing each real response. Run
/// with `cargo test -p anamnesis-mcp p1_t5_manual_qa_demo -- --nocapture`.
#[test]
fn p1_t5_manual_qa_demo() {
    let (reg, _dir) = stub_registry();

    let mut metadata = std::collections::HashMap::new();
    metadata.insert("owner".to_string(), "alice".to_string());
    let stored = ok_text(remember_with(
        &reg,
        "the on-call runbook lives in ops/oncall.md",
        Some(vec!["ops".to_string()]),
        Some(metadata),
        Some("projA".to_string()),
    ));
    println!("remember (tagged+scoped+metadata) -> {stored}");
    let id = stored
        .strip_prefix("stored node ")
        .and_then(|s| s.parse::<u64>().ok())
        .expect("stored node id");

    // Also store a distractor in a different scope/tag so the filters below
    // demonstrably narrow the result set, not just echo everything.
    remember_with(
        &reg,
        "an unrelated projB fact",
        Some(vec!["misc".to_string()]),
        None,
        Some("projB".to_string()),
    );

    let got = ok_text(dispatch(
        &reg,
        Request::Get {
            id,
            namespace: None,
        },
    ));
    println!("get -> {got}");

    let listed_by_tag = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: Some("ops".to_string()),
            namespace: None,
            scope: None,
            metadata: None,
        },
    ));
    println!("list (tag=ops) -> {listed_by_tag}");

    let listed_by_scope = ok_text(dispatch(
        &reg,
        Request::List {
            min_salience: Some(0.0),
            limit: Some(20),
            node_type: None,
            tag: None,
            namespace: None,
            scope: Some("projA".to_string()),
            metadata: None,
        },
    ));
    println!("list (scope=projA) -> {listed_by_scope}");

    let recalled = ok_text(dispatch(
        &reg,
        Request::Recall {
            query: "on-call runbook".into(),
            limit: Some(20),
            namespace: None,
            reinforce: Some(true),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: Some("projA".to_string()),
            tag: None,
            event_kind: None,
        },
    ));
    println!("recall (scope=projA) -> {recalled}");
}

// ── graph-viz: `Request::Graph` dispatch (RED→GREEN, start-work Wave 2) ─────

/// By-seed path: remember two nodes, relate them, then request the subgraph
/// rooted at the first. Asserts the canonical wire shape (`schema`, `nodes`,
/// `edges`) and that the seed + its related neighbor + the edge between them
/// actually appear — real wired data, not a hardcoded shape.
#[test]
fn dispatch_graph_by_seed_returns_canonical_json() {
    let (reg, _dir) = stub_registry();
    let a = remember_id(&reg, "graph-viz seed node alpha");
    let b = remember_id(&reg, "graph-viz seed node beta");
    ok_text(dispatch(
        &reg,
        Request::Relate {
            from_id: a,
            to_id: b,
            relation: "causes".into(),
            namespace: None,
        },
    ));

    let resp = ok_text(dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![a]),
            query: None,
            depth: Some(1),
            limit: Some(100),
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).expect("graph returns JSON");

    assert_eq!(body["schema"], 1, "got: {body}");
    assert_eq!(body["seed_ids"], serde_json::json!([a]), "got: {body}");
    let nodes = body["nodes"].as_array().expect("nodes array");
    assert!(
        nodes.iter().any(|n| n["id"] == a),
        "seed node must appear in nodes: {body}"
    );
    assert!(
        nodes.iter().any(|n| n["id"] == b),
        "1-hop neighbor must appear in nodes: {body}"
    );
    let edges = body["edges"].as_array().expect("edges array");
    assert!(
        edges
            .iter()
            .any(|e| e["source"] == a && e["target"] == b && e["type"] == "causal"),
        "the causes edge must appear: {body}"
    );
    for n in nodes {
        assert!(n["salience"].is_number(), "got: {n}");
        assert!(n["type"].is_string(), "got: {n}");
        assert!(n["depth"].is_u64(), "got: {n}");
        assert!(n["tier"].is_string(), "got: {n}");
        assert!(n["created_at"].is_u64(), "got: {n}");
        assert!(n["retracted"].is_boolean(), "got: {n}");
    }
}

/// By-query path: a query resolves its own seed ids via search, without the
/// caller supplying any `seeds`.
#[test]
fn dispatch_graph_by_query_resolves_seeds() {
    let (reg, _dir) = stub_registry();
    remember_id(&reg, "the wombat migration runbook is documented here");

    let resp = ok_text(dispatch(
        &reg,
        Request::Graph {
            seeds: None,
            query: Some("wombat".into()),
            depth: None,
            limit: None,
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).expect("graph returns JSON");
    assert_eq!(body["schema"], 1, "got: {body}");
    let nodes = body["nodes"].as_array().expect("nodes array");
    assert!(
        !nodes.is_empty(),
        "query-resolved seeds must yield a non-empty subgraph: {body}"
    );
}

/// Neither `seeds` nor `query` is a caller error, not an internal one.
#[test]
fn dispatch_graph_without_seed_or_query_is_invalid_params() {
    let (reg, _dir) = stub_registry();
    let resp = dispatch(
        &reg,
        Request::Graph {
            seeds: None,
            query: None,
            depth: None,
            limit: None,
            namespace: None,
        },
    );
    match resp {
        Response::Err { kind, .. } => assert_eq!(kind, ErrKind::InvalidParams),
        other => panic!("expected invalid_params, got {other:?}"),
    }
}

/// A seed id that doesn't exist in the graph is a caller error (bad id), not
/// an internal one — mirrors `get`/`update`'s missing-id classification.
#[test]
fn dispatch_graph_missing_node_is_invalid_params() {
    let (reg, _dir) = stub_registry();
    let resp = dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![9999]),
            query: None,
            depth: None,
            limit: None,
            namespace: None,
        },
    );
    match resp {
        Response::Err { kind, .. } => assert_eq!(kind, ErrKind::InvalidParams),
        other => panic!("expected invalid_params, got {other:?}"),
    }
}

#[test]
fn adversarial_probe_graph_edge_cases() {
    let (reg, _dir) = stub_registry();
    let a = remember_id(&reg, "adversarial probe alpha node");

    // empty seeds vec, no query -> invalid_params (not silently empty-200).
    let resp = dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![]),
            query: None,
            depth: None,
            limit: None,
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
        "empty seeds: {resp:?}"
    );

    // both seeds + query present -> query wins (precedence).
    let resp = ok_text(dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![9999]), // would 404 if used
            query: Some("adversarial".into()),
            depth: None,
            limit: None,
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(
        body["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == a),
        "query must win: {body}"
    );

    // depth over cap clamps to 3 (no error, no panic).
    let resp = dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![a]),
            query: None,
            depth: Some(999),
            limit: Some(100),
            namespace: None,
        },
    );
    assert!(matches!(resp, Response::Ok { .. }), "depth clamp: {resp:?}");

    // query against an empty store -> 200 with an empty subgraph, not an
    // error (no seeds resolve because there is nothing to search).
    let (empty_reg, _dir2) = stub_registry();
    let resp = ok_text(dispatch(
        &empty_reg,
        Request::Graph {
            seeds: None,
            query: Some("zzz_no_such_term_zzz".into()),
            depth: None,
            limit: None,
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(
        body["nodes"].as_array().unwrap().len(),
        0,
        "no-match query against an empty store: {body}"
    );
}

/// Node-budget contract: omitted `limit` uses the 250 default, and any
/// caller-supplied `limit` above 2000 is clamped rather than honored
/// verbatim. Tested directly against the extracted pure helper, per the
/// contract's own dispatch path being expensive to build a 2000+-node fixture
/// for — the helper IS what `dispatch_graph` calls for this computation.
#[test]
fn graph_budget_defaults_to_250_and_caps_at_2000() {
    assert_eq!(
        mgmt::graph_budget(None),
        250,
        "omitted limit must default to 250"
    );
    assert_eq!(mgmt::graph_budget(Some(250)), 250);
    assert_eq!(
        mgmt::graph_budget(Some(2000)),
        2000,
        "2000 is in-bounds, not clamped"
    );
    assert_eq!(
        mgmt::graph_budget(Some(2001)),
        2000,
        "over-cap limit must clamp to 2000"
    );
    assert_eq!(
        mgmt::graph_budget(Some(u32::MAX)),
        2000,
        "an absurd limit must still clamp to 2000"
    );
}

// ── graph-viz Phase 2: community `cluster` + `doi` enrichment ──────────────

/// Every node in the `/api/graph` payload carries a numeric `cluster` and
/// `doi` — the two Phase-2 derived fields, computed server-side.
#[test]
fn graph_json_includes_cluster_and_doi() {
    let (reg, _dir) = stub_registry();
    let a = remember_id(&reg, "enrich phase2 node alpha");
    let b = remember_id(&reg, "enrich phase2 node beta");
    ok_text(dispatch(
        &reg,
        Request::Relate {
            from_id: a,
            to_id: b,
            relation: "causes".into(),
            namespace: None,
        },
    ));

    let resp = ok_text(dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![a]),
            query: None,
            depth: Some(1),
            limit: Some(100),
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).expect("graph returns JSON");
    let nodes = body["nodes"].as_array().expect("nodes array");
    assert!(!nodes.is_empty(), "got: {body}");
    for n in nodes {
        assert!(n["cluster"].is_u64(), "cluster must be numeric: {n}");
        assert!(n["doi"].is_number(), "doi must be numeric: {n}");
    }
}

/// Leiden runs deterministically (fixed seed): the same fixture queried
/// twice yields identical `{node_id: cluster}` assignments.
#[test]
fn clusters_are_stable() {
    let (reg, _dir) = stub_registry();
    let ids: Vec<u64> = (0..8)
        .map(|i| remember_id(&reg, &format!("stability fixture node {i}")))
        .collect();
    for pair in ids.windows(2) {
        ok_text(dispatch(
            &reg,
            Request::Relate {
                from_id: pair[0],
                to_id: pair[1],
                relation: "causes".into(),
                namespace: None,
            },
        ));
    }

    let request = || Request::Graph {
        seeds: Some(vec![ids[0]]),
        query: None,
        depth: Some(3),
        limit: Some(100),
        namespace: None,
    };
    let first: serde_json::Value =
        serde_json::from_str(&ok_text(dispatch(&reg, request()))).unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&ok_text(dispatch(&reg, request()))).unwrap();

    let clusters_by_id = |body: &serde_json::Value| -> std::collections::BTreeMap<u64, u64> {
        body["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n["id"].as_u64().unwrap(), n["cluster"].as_u64().unwrap()))
            .collect()
    };
    assert_eq!(
        clusters_by_id(&first),
        clusters_by_id(&second),
        "leiden must be deterministic across identical calls: {first} vs {second}"
    );
}

/// The seed (depth 0) outranks a 1-hop neighbor on `doi` — the seed bonus
/// and zero depth penalty dominate.
#[test]
fn doi_ranks_seed_highest() {
    let (reg, _dir) = stub_registry();
    let a = remember_id(&reg, "doi ranking seed node");
    let b = remember_id(&reg, "doi ranking neighbor node");
    ok_text(dispatch(
        &reg,
        Request::Relate {
            from_id: a,
            to_id: b,
            relation: "causes".into(),
            namespace: None,
        },
    ));

    let resp = ok_text(dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![a]),
            query: None,
            depth: Some(1),
            limit: Some(100),
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let nodes = body["nodes"].as_array().unwrap();
    let doi_of = |id: u64| -> f64 {
        nodes
            .iter()
            .find(|n| n["id"] == id)
            .and_then(|n| n["doi"].as_f64())
            .unwrap_or_else(|| panic!("node {id} missing doi: {body}"))
    };
    assert!(
        doi_of(a) > doi_of(b),
        "seed doi ({}) must exceed neighbor doi ({}): {body}",
        doi_of(a),
        doi_of(b)
    );
}

/// Below the Leiden size threshold, every node gets `cluster == 0` — the
/// hybrid-by-size cheap path, not a broken solver.
#[test]
fn tiny_graph_single_cluster() {
    let (reg, _dir) = stub_registry();
    let a = remember_id(&reg, "tiny graph node alpha");
    let b = remember_id(&reg, "tiny graph node beta");
    ok_text(dispatch(
        &reg,
        Request::Relate {
            from_id: a,
            to_id: b,
            relation: "causes".into(),
            namespace: None,
        },
    ));

    let resp = ok_text(dispatch(
        &reg,
        Request::Graph {
            seeds: Some(vec![a]),
            query: None,
            depth: Some(1),
            limit: Some(100),
            namespace: None,
        },
    ));
    let body: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let nodes = body["nodes"].as_array().unwrap();
    assert!(
        nodes.len() < 8,
        "fixture must stay under the threshold: {body}"
    );
    for n in nodes {
        assert_eq!(
            n["cluster"], 0,
            "sub-threshold graph must be cluster 0: {n}"
        );
    }
}

// ── R2 Task 3: daemon-mediated, read-only extraction source scan (RED) ─────

fn capture_turns(
    reg: &Arc<Mutex<MemoryRegistry>>,
    session: &str,
    scope: &str,
    count: usize,
    at_ms: u64,
    content_prefix: &str,
) {
    for offset in 0..count {
        ok_text(dispatch(
            reg,
            Request::Ingest {
                session: session.to_owned(),
                turns: vec![TurnInput {
                    speaker: "user".into(),
                    text: format!("{content_prefix} turn {offset}"),
                    at_ms: Some(at_ms + offset as u64),
                }],
                namespace: None,
                capture: Some(true),
                scope: Some(scope.to_owned()),
            },
        ));
    }
}

fn extraction_scan(
    reg: &Arc<Mutex<MemoryRegistry>>,
    profile: crate::extract::types::ExtractorProfileComponents,
) -> crate::extract::types::ExtractionScanResult {
    let text = ok_text(dispatch(
        reg,
        Request::ExtractionScan {
            namespace: None,
            profile,
            min_turns: 10,
            max_turns: 20,
        },
    ));
    serde_json::from_str(&text).expect("extraction scan must return its canonical JSON result")
}

fn extraction_profile() -> crate::extract::types::ExtractorProfileComponents {
    let config = crate::extract::config::ExtractConfig::from_env()
        .expect("the test environment must have a valid extraction command");
    crate::extract::profile::ExtractorProfile::from_command(&config.command)
        .expect("profile from extraction command")
        .components
}

fn extraction_profile_id(profile: &crate::extract::types::ExtractorProfileComponents) -> String {
    crate::extract::profile::profile_id(profile).expect("profile id")
}

fn record_scanned_sources(
    dir: &tempfile::TempDir,
    profile_id: &str,
    sources: &[crate::extract::types::ExtractionSource],
) {
    let connection =
        rusqlite::Connection::open(dir.path().join("memory.db")).expect("open policy ledger");
    connection
        .execute(
            "INSERT OR IGNORE INTO extractor_profiles
             (profile_id, components, status, created_at, approved_at)
             VALUES (?1, '{}', 'shadow', 0, NULL)",
            [profile_id],
        )
        .expect("record profile");
    connection
        .execute(
            "INSERT INTO extract_runs
             (at_ms, profile_id, mode, turn_count, candidate_count, relation_count,
              schema_valid, llm_invoked, error_kind, duration_ms)
             VALUES (0, ?1, 'shadow', 0, 0, 0, 1, 0, NULL, 0)",
            [profile_id],
        )
        .expect("record extraction run");
    let run_id = connection.last_insert_rowid();
    for source in sources {
        connection
            .execute(
                "INSERT INTO extract_run_sources (profile_id, turn_key, run_id)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![profile_id, source.turn_key, run_id],
            )
            .expect("record scanned source");
    }
}

fn graph_snapshot(
    reg: &Arc<Mutex<MemoryRegistry>>,
) -> (Vec<anamnesis::graph::Node>, Vec<anamnesis::graph::Edge>) {
    let handle = {
        let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.namespace_handle(None).expect("default namespace")
    };
    let memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
    let graph = memory.engine().graph();
    let nodes = graph
        .all_node_ids()
        .into_iter()
        .map(|id| graph.get_node(id).expect("live node").clone())
        .collect();
    let edges = graph
        .all_edge_ids()
        .into_iter()
        .map(|id| graph.get_edge(id).expect("live edge").clone())
        .collect();
    (nodes, edges)
}

#[test]
fn extraction_scan_excludes_current_profile_ledger_without_reading_extracted_metadata() {
    let (reg, dir) = stub_registry();
    capture_turns(
        &reg,
        "scan-session",
        "project/anamnesis",
        12,
        100,
        "captured",
    );

    {
        let handle = {
            let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
            registry.namespace_handle(None).expect("default namespace")
        };
        let mut memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
        let graph = memory.engine_mut().graph_mut();
        for (index, id) in graph.all_node_ids().into_iter().enumerate() {
            let node = graph.get_node_mut(id).expect("live node");
            if node.origin.session_id == "scan-session" {
                node.metadata.insert(
                    "anamnesis:extracted".into(),
                    if index % 2 == 0 { "true" } else { "false" }.into(),
                );
                node.salience = 0.2 + index as f64 / 100.0;
                node.accessed_at = anamnesis::graph::Timestamp(10_000 + index as u64);
            }
        }
    }

    let profile = extraction_profile();
    let before = graph_snapshot(&reg);
    let initial = extraction_scan(&reg, profile.clone());
    assert_eq!(
        initial.sources.len(),
        12,
        "twelve captured turns are eligible"
    );
    assert!(
        initial.sources.iter().all(
            |source| source.session_id == "scan-session" && source.scope == "project/anamnesis"
        ),
        "scan must preserve session and scope provenance"
    );

    record_scanned_sources(
        &dir,
        &extraction_profile_id(&profile),
        &initial.sources[..3],
    );
    assert!(
        extraction_scan(&reg, profile.clone()).sources.is_empty(),
        "the current profile has only 9 unledgered turns, below the minimum batch size"
    );

    let mut other_profile = profile;
    other_profile.model_id.push_str("-other");
    assert_eq!(
        extraction_scan(&reg, other_profile).sources.len(),
        12,
        "a different profile must see all twelve turns"
    );

    let after = graph_snapshot(&reg);
    assert_eq!(
        after, before,
        "scan must not mutate graph node or edge records"
    );
    assert!(
        before.0.iter().any(|node| {
            node.metadata
                .get("anamnesis:extracted")
                .is_some_and(|value| value == "true")
        }),
        "fixture must snapshot anamnesis:extracted metadata"
    );
    assert!(
        before
            .0
            .iter()
            .any(|node| node.salience > 0.2 && node.accessed_at.0 >= 10_000),
        "fixture must snapshot salience and accessed_at"
    );
}

#[test]
fn extraction_scan_groups_by_session_and_scope_selects_oldest_and_caps_at_twenty() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "shared-session", "scope-a", 9, 100, "A");
    capture_turns(&reg, "shared-session", "scope-b", 10, 200, "B");
    capture_turns(&reg, "session-c", "scope-a", 21, 300, "C");

    let profile = extraction_profile();
    let first = extraction_scan(&reg, profile.clone());
    assert_eq!(
        first.sources.len(),
        10,
        "oldest eligible group B is selected"
    );
    assert!(
        first
            .sources
            .iter()
            .all(|source| source.session_id == "shared-session" && source.scope == "scope-b"),
        "the same session in a distinct scope must remain a distinct group"
    );

    record_scanned_sources(&dir, &extraction_profile_id(&profile), &first.sources);
    let second = extraction_scan(&reg, profile);
    assert_eq!(
        second.sources.len(),
        20,
        "group C is capped at twenty sources"
    );
    assert!(
        second
            .sources
            .iter()
            .all(|source| source.session_id == "session-c" && source.scope == "scope-a"),
        "the next eligible session-and-scope group is selected"
    );
}

#[test]
fn extraction_scan_hashes_exact_utf8_source_content() {
    let (reg, _dir) = stub_registry();
    let content = "café 🦀\ncombining: e\u{301}";
    ok_text(dispatch(
        &reg,
        Request::Ingest {
            session: "utf8-session".into(),
            turns: (0..10)
                .map(|at_ms| TurnInput {
                    speaker: "user".into(),
                    text: content.into(),
                    at_ms: Some(at_ms),
                })
                .collect(),
            namespace: None,
            capture: Some(true),
            scope: Some("utf8-scope".into()),
        },
    ));

    let sources = extraction_scan(&reg, extraction_profile()).sources;
    assert_eq!(sources.len(), 10);

    let handle = {
        let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.namespace_handle(None).expect("default namespace")
    };
    let memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
    let graph = memory.engine().graph();
    for source in sources {
        let node = graph
            .get_node(anamnesis::graph::NodeId(source.node_id))
            .expect("scanned source node exists");
        assert_eq!(
            source.content, node.content,
            "scan must return the stored captured node content"
        );
        assert_eq!(
            source.content_hash,
            format!("{:x}", Sha256::digest(node.content.as_bytes())),
            "scan must hash the exact stored UTF-8 node content bytes"
        );
    }
}
#[test]
fn extraction_scan_sends_only_captured_episodic_turns() {
    let (reg, _dir) = stub_registry();
    capture_turns(&reg, "capture-only", "scope", 10, 100, "captured");
    ok_text(remember_with(
        &reg,
        "turn key impostor",
        None,
        Some(
            [("anamnesis:turn_key".to_owned(), "impostor-key".to_owned())]
                .into_iter()
                .collect(),
        ),
        None,
    ));
    {
        let handle = {
            let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
            registry.namespace_handle(None).expect("default namespace")
        };
        let mut memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
        let graph = memory.engine_mut().graph_mut();
        for id in graph.all_node_ids() {
            let node = graph.get_node_mut(id).expect("remember fixture node");
            if node.content == "turn key impostor"
                && matches!(&node.node_type, anamnesis::graph::KnowledgeType::Semantic)
            {
                node.metadata.insert(META_CAPTURE.into(), "true".into());
            }
        }
    }

    let sources = extraction_scan(&reg, extraction_profile()).sources;
    assert_eq!(
        sources.len(),
        10,
        "only captured turns reach the provider boundary"
    );
    assert!(
        sources
            .iter()
            .all(|source| source.turn_key != "impostor-key"),
        "a non-capture node carrying a turn key must not be scanned"
    );
}

#[test]
fn extraction_scan_rejects_non_shadow_status_without_approval_semantics() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "unsupported-status", "scope", 10, 100, "captured");
    let profile = extraction_profile();
    let profile_id = extraction_profile_id(&profile);
    extraction_scan(&reg, profile.clone());

    let connection =
        rusqlite::Connection::open(dir.path().join("memory.db")).expect("open policy database");
    connection
        .execute(
            "UPDATE extractor_profiles SET status = 'approved' WHERE profile_id = ?1",
            [&profile_id],
        )
        .expect("set unsupported profile status");

    let response = dispatch(
        &reg,
        Request::ExtractionScan {
            namespace: None,
            profile,
            min_turns: 10,
            max_turns: 20,
        },
    );
    let Response::Err { kind, message } = response else {
        panic!("non-shadow status must reject scan");
    };
    assert_eq!(kind, ErrKind::InvalidParams);
    assert_eq!(
        message,
        "unsupported extraction profile status for shadow scans: Approved"
    );
}
// ── R2 Task 6: atomic shadow-extraction staging and failure recording (RED) ──

fn policy_counts(dir: &tempfile::TempDir) -> (u64, u64, u64, u64) {
    let connection =
        rusqlite::Connection::open(dir.path().join("memory.db")).expect("open policy database");
    let count = |table: &str| -> u64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count policy rows")
    };
    (
        count("extract_runs"),
        count("extract_run_sources"),
        count("extract_candidates"),
        count("extract_relations"),
    )
}

fn stage_extraction(
    reg: &Arc<Mutex<MemoryRegistry>>,
    profile: crate::extract::types::ExtractorProfileComponents,
    sources: Vec<crate::extract::types::ExtractionSource>,
    extraction: crate::extract::types::ValidatedExtraction,
) -> Response {
    dispatch(
        reg,
        Request::StageExtraction {
            namespace: None,
            profile,
            llm_duration_ms: 17,
            sources,
            extraction,
        },
    )
}

fn staged_result(response: Response) -> crate::proto::StageExtractionResult {
    serde_json::from_str(&ok_text(response)).expect("stage response must be canonical JSON")
}

fn canonical_extraction(
    profile: &crate::extract::types::ExtractorProfileComponents,
    sources: &[crate::extract::types::ExtractionSource],
    item_sources: &[(&str, u64)],
    relations: &[(&str, &str, crate::extract::types::RelationKind)],
) -> crate::extract::types::ValidatedExtraction {
    let items: Vec<_> = item_sources
        .iter()
        .map(|(item_local_id, source_node_id)| {
            serde_json::json!({
                "item_local_id": item_local_id,
                "content": format!("derived {item_local_id}"),
                "kind": crate::extract::types::CandidateKind::Lesson,
                "confidence": 0.9,
                "source_node_ids": [source_node_id],
            })
        })
        .collect();
    let relations: Vec<_> = relations
        .iter()
        .map(|(from_item_local_id, to_item_local_id, relation_type)| {
            serde_json::json!({
                "from_item_local_id": from_item_local_id,
                "to_item_local_id": to_item_local_id,
                "relation_type": relation_type,
            })
        })
        .collect();
    let payload = serde_json::to_vec(&serde_json::json!({
        "items": items,
        "relations": relations,
    }))
    .expect("serialize provider-boundary extraction JSON");
    crate::extract::validate::validate_output(&payload, sources, &extraction_profile_id(profile))
        .expect("provider-boundary extraction must canonically validate")
}
#[test]
fn stage_extraction_rejects_non_capture_turn_key_snapshot() {
    let (reg, _dir) = stub_registry();
    capture_turns(&reg, "stage-capture-only", "scope", 10, 100, "captured");
    let profile = extraction_profile();
    extraction_scan(&reg, profile.clone());
    ok_text(remember_with(
        &reg,
        "stage turn key impostor",
        None,
        Some(
            [(
                "anamnesis:turn_key".to_owned(),
                "stage-impostor-key".to_owned(),
            )]
            .into_iter()
            .collect(),
        ),
        None,
    ));
    {
        let handle = {
            let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
            registry.namespace_handle(None).expect("default namespace")
        };
        let mut memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
        let graph = memory.engine_mut().graph_mut();
        for id in graph.all_node_ids() {
            let node = graph.get_node_mut(id).expect("remember fixture node");
            if node.content == "stage turn key impostor"
                && matches!(&node.node_type, anamnesis::graph::KnowledgeType::Semantic)
            {
                node.metadata.insert(META_CAPTURE.into(), "true".into());
            }
        }
    }

    let impostor = graph_snapshot(&reg)
        .0
        .into_iter()
        .find(|node| {
            node.content == "stage turn key impostor"
                && matches!(&node.node_type, anamnesis::graph::KnowledgeType::Semantic)
        })
        .expect("remember fixture node");
    let source = crate::extract::types::ExtractionSource {
        node_id: impostor.id.0,
        turn_key: "stage-impostor-key".to_owned(),
        session_id: impostor.origin.session_id,
        scope: impostor.origin.scope.as_str().to_owned(),
        content: impostor.content.clone(),
        content_hash: format!("{:x}", Sha256::digest(impostor.content.as_bytes())),
        at_ms: impostor.created_at.0,
    };

    let response = stage_extraction(
        &reg,
        profile,
        vec![source],
        crate::extract::types::ValidatedExtraction {
            items: vec![],
            relations: vec![],
        },
    );
    let Response::Err { kind, message } = response else {
        panic!("non-capture source snapshot must reject");
    };
    assert_eq!(kind, ErrKind::InvalidParams);
    assert_eq!(
        message,
        "extraction source node is not a captured episodic node"
    );
}

#[test]
fn stage_extraction_relation_insert_failure_rolls_back_every_success_table() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "atomic", "scope", 10, 100, "atomic");
    let profile = extraction_profile();
    let sources = extraction_scan(&reg, profile.clone()).sources;
    let before_graph = graph_snapshot(&reg);
    let connection =
        rusqlite::Connection::open(dir.path().join("memory.db")).expect("open policy database");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_staged_relation
             BEFORE INSERT ON extract_relations
             BEGIN SELECT RAISE(ABORT, 'relation insert rejected'); END;",
        )
        .expect("install relation failure trigger");
    drop(connection);

    let response = stage_extraction(
        &reg,
        profile.clone(),
        sources.clone(),
        canonical_extraction(
            &profile,
            &sources,
            &[("one", sources[0].node_id), ("two", sources[1].node_id)],
            &[("one", "two", crate::extract::types::RelationKind::Supports)],
        ),
    );

    let Response::Err { message, .. } = &response else {
        panic!("relation insert failure must reject the whole stage: {response:?}");
    };
    assert!(
        message.contains("relation insert rejected"),
        "the SQLite relation trigger must be reached before rollback: {message}"
    );
    assert_eq!(
        policy_counts(&dir),
        (0, 0, 0, 0),
        "one transaction must roll back run, source ledger, candidates, and relations"
    );
    assert_eq!(
        graph_snapshot(&reg),
        before_graph,
        "failed staging must leave the canonical graph snapshot unchanged"
    );
}

#[test]
fn zero_output_stage_is_replay_safe_and_its_sources_are_excluded_from_scan() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "zero-output", "scope", 10, 100, "zero");
    let profile = extraction_profile();
    let sources = extraction_scan(&reg, profile.clone()).sources;
    let before_graph = graph_snapshot(&reg);

    let first = staged_result(stage_extraction(
        &reg,
        profile.clone(),
        sources.clone(),
        crate::extract::types::ValidatedExtraction {
            items: vec![],
            relations: vec![],
        },
    ));
    let run_id = match first {
        StageExtractionResult::Staged { run_id } => run_id,
        other => panic!("first zero-output stage must create a run: {other:?}"),
    };
    assert_eq!(policy_counts(&dir), (1, 10, 0, 0));

    assert_eq!(
        staged_result(stage_extraction(
            &reg,
            profile.clone(),
            sources,
            crate::extract::types::ValidatedExtraction {
                items: vec![],
                relations: vec![],
            },
        )),
        StageExtractionResult::AlreadyStaged { run_id },
        "exact replay must retain the original run identity"
    );
    assert_eq!(
        policy_counts(&dir),
        (1, 10, 0, 0),
        "exact replay must not create any additional policy rows"
    );
    assert!(
        extraction_scan(&reg, profile).sources.is_empty(),
        "zero-output success must ledger every source and exclude it from later scans"
    );
    assert_eq!(
        graph_snapshot(&reg),
        before_graph,
        "successful staging and replay must leave the canonical graph snapshot unchanged"
    );
}

#[test]
fn stage_extraction_rejects_partial_or_mixed_source_ledger_conflicts() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "mixed-ledger", "scope", 10, 100, "mixed");
    let profile = extraction_profile();
    let sources = extraction_scan(&reg, profile.clone()).sources;
    let first_source = sources[0].clone();
    let before_graph = graph_snapshot(&reg);

    assert!(matches!(
        staged_result(stage_extraction(
            &reg,
            profile.clone(),
            vec![first_source],
            crate::extract::types::ValidatedExtraction {
                items: vec![],
                relations: vec![],
            },
        )),
        StageExtractionResult::Staged { .. }
    ));
    assert_eq!(policy_counts(&dir), (1, 1, 0, 0));

    let response = stage_extraction(
        &reg,
        profile,
        sources,
        crate::extract::types::ValidatedExtraction {
            items: vec![],
            relations: vec![],
        },
    );
    assert!(
        matches!(response, Response::Err { .. }),
        "a source set containing both ledgered and new turns must be rejected: {response:?}"
    );
    assert_eq!(
        policy_counts(&dir),
        (1, 1, 0, 0),
        "conflicting source ledger must not create a partial second run"
    );
    assert_eq!(
        graph_snapshot(&reg),
        before_graph,
        "partial and conflicting stages must leave the canonical graph snapshot unchanged"
    );
}

#[test]
fn stage_extraction_rejects_any_source_snapshot_mismatch_without_mutating_graph_or_policy() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "snapshot", "scope", 10, 100, "snapshot");
    let profile = extraction_profile();
    let sources = extraction_scan(&reg, profile.clone()).sources;
    let before_graph = graph_snapshot(&reg);

    let mut cases = Vec::new();
    cases.push((
        "reused node id",
        vec![sources[0].clone(), sources[0].clone()],
    ));
    let mut missing_node = sources.clone();
    missing_node[0].node_id = 999_999;
    cases.push(("mismatched node id", missing_node));
    let mut changed_hash = sources.clone();
    changed_hash[0].content_hash = "different-hash".into();
    cases.push(("changed content hash", changed_hash));
    let mut changed_session = sources.clone();
    changed_session[0].session_id = "other-session".into();
    cases.push(("changed session", changed_session));
    let mut changed_scope = sources.clone();
    changed_scope[0].scope = "other-scope".into();
    cases.push(("changed scope", changed_scope));
    let mut unknown_turn = sources;
    unknown_turn[0].turn_key = "unknown-turn-key".into();
    cases.push(("unknown turn key", unknown_turn));

    for (label, invalid_sources) in cases {
        let response = stage_extraction(
            &reg,
            profile.clone(),
            invalid_sources,
            crate::extract::types::ValidatedExtraction {
                items: vec![],
                relations: vec![],
            },
        );
        assert!(
            matches!(&response, Response::Err { .. }),
            "{label} must reject staging: {response:?}"
        );
        if label == "reused node id" {
            assert!(
                matches!(
                    &response,
                    Response::Err {
                        kind: ErrKind::InvalidParams,
                        ..
                    }
                ),
                "caller InvalidInput must map to invalid_params: {response:?}"
            );
        }
        assert_eq!(
            policy_counts(&dir),
            (0, 0, 0, 0),
            "{label} must leave every success table empty"
        );
        assert_eq!(
            graph_snapshot(&reg),
            before_graph,
            "{label} must not alter the canonical graph snapshot"
        );
    }
}

#[test]
fn extraction_failures_record_error_contract_without_ledgering_sources() {
    let (reg, dir) = stub_registry();
    capture_turns(&reg, "failures", "scope", 10, 100, "failure");
    let profile = extraction_profile();
    let initial_sources = extraction_scan(&reg, profile.clone()).sources;
    let before_graph = graph_snapshot(&reg);

    for (error_kind, llm_invoked) in [
        (ExtractionErrorKind::Spawn, false),
        (ExtractionErrorKind::Timeout, true),
        (ExtractionErrorKind::InvalidJson, true),
        (ExtractionErrorKind::SchemaReject, true),
    ] {
        let response = dispatch(
            &reg,
            Request::RecordExtractionFailure {
                namespace: None,
                profile: profile.clone(),
                turn_count: initial_sources.len() as u32,
                llm_invoked,
                error_kind,
                duration_ms: 29,
            },
        );
        assert!(
            matches!(response, Response::Ok { .. }),
            "failure record must be accepted: {response:?}"
        );
    }

    let connection =
        rusqlite::Connection::open(dir.path().join("memory.db")).expect("open policy database");
    let rows: Vec<(String, bool, u64, u64, u64)> = {
        let mut statement = connection
            .prepare(
                "SELECT error_kind, llm_invoked, turn_count, candidate_count, relation_count
                 FROM extract_runs ORDER BY id",
            )
            .expect("query failure runs");
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .expect("map failure runs")
            .collect::<Result<_, _>>()
            .expect("read failure runs")
    };
    assert_eq!(
        rows,
        vec![
            ("spawn".into(), false, 10, 0, 0),
            ("timeout".into(), true, 10, 0, 0),
            ("invalid-json".into(), true, 10, 0, 0),
            ("schema-reject".into(), true, 10, 0, 0),
        ],
        "failure runs must preserve the wire error kind, invocation flag, and zero output counts"
    );
    assert_eq!(
        policy_counts(&dir),
        (4, 0, 0, 0),
        "failure recording creates only runs, never source ledger or staged output"
    );
    assert_eq!(
        extraction_scan(&reg, profile).sources,
        initial_sources,
        "failure rows must leave their sources selectable for a later attempt"
    );
    assert_eq!(
        graph_snapshot(&reg),
        before_graph,
        "failure recording must leave the canonical graph snapshot unchanged"
    );
}

mod migration_job {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, Condvar};

    use anamnesis::Error;
    use anamnesis::embedding::EmbeddingProvider;
    use anamnesis::storage::SqliteStorage;

    use crate::dispatch::DaemonRuntimeContext;
    use crate::memory::MigrationLockLease;
    use crate::memory::migration::MigrationRuntime;

    struct OldProvider;

    impl EmbeddingProvider for OldProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            Ok(texts.iter().map(|_| vec![0.5; 4]).collect())
        }

        fn dimensions(&self) -> usize {
            4
        }

        fn model_name(&self) -> &str {
            "source-model"
        }
    }
    struct TargetProvider;

    impl EmbeddingProvider for TargetProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            Ok(texts.iter().map(|_| vec![0.25; 3]).collect())
        }

        fn dimensions(&self) -> usize {
            3
        }

        fn model_name(&self) -> &str {
            "target-model"
        }
    }

    struct BlockingProvider {
        calls: AtomicUsize,
        started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        released: Arc<(Mutex<bool>, Condvar)>,
    }

    impl BlockingProvider {
        fn new(started: std::sync::mpsc::Sender<()>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                started: Mutex::new(Some(started)),
                released: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        fn release(&self) {
            let (released, wake) = self.released.as_ref();
            *released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            wake.notify_all();
        }
    }

    impl EmbeddingProvider for BlockingProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            self.calls.fetch_add(texts.len(), Ordering::SeqCst);
            if let Some(started) = self
                .started
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = started.send(());
            }
            let (released, wake) = self.released.as_ref();
            let mut released = released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while !*released {
                released = wake
                    .wait(released)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            Ok(texts.iter().map(|_| vec![0.25; 3]).collect())
        }
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>, Error> {
            Ok(vec![0.25; 3])
        }

        fn dimensions(&self) -> usize {
            3
        }

        fn model_name(&self) -> &str {
            "target-model"
        }
    }

    fn wait_for_fixture_lock_release(db: &std::path::Path) {
        let mut lock_path = db.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .expect("open fixture lock probe");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match fs4::FileExt::try_lock(&lock) {
                Ok(()) => break,
                Err(fs4::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("fixture lock remained held after drop: {error}"),
            }
        }
        fs4::FileExt::unlock(&lock).expect("release fixture lock probe");
    }

    fn create_old_fixture(db: &std::path::Path, dir: &std::path::Path) {
        let mut registry = MemoryRegistry::file_backed_with(
            Arc::new(OldProvider),
            db.to_path_buf(),
            dir.to_path_buf(),
            "default".to_string(),
            false,
        );
        registry
            .remember("legacy embedding content", None)
            .expect("create old embedding fixture");
        registry.flush_all_open().expect("flush old fixture");
        drop(registry);
        wait_for_fixture_lock_release(db);
    }
    fn create_compatible_fixture(db: &std::path::Path, dir: &std::path::Path) {
        let mut registry = MemoryRegistry::file_backed_with(
            Arc::new(TargetProvider),
            db.to_path_buf(),
            dir.to_path_buf(),
            "compatible".to_string(),
            false,
        );
        registry
            .remember("legacy content in compatible namespace", None)
            .expect("create compatible embedding fixture");
        registry.flush_all_open().expect("flush compatible fixture");
        drop(registry);
        wait_for_fixture_lock_release(db);
    }

    fn daemon_runtime(
        registry: Arc<Mutex<MemoryRegistry>>,
        default_db: &std::path::Path,
    ) -> (DispatchRuntime, Arc<MigrationRuntime>) {
        let mut lock_path = default_db.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .expect("open daemon lock");
        fs4::FileExt::try_lock(&lock).expect("acquire daemon lock");
        let migrations = Arc::new(
            MigrationRuntime::new(
                &registry,
                default_db.to_path_buf(),
                MigrationLockLease::from(Arc::new(lock)),
            )
            .expect("create migration runtime"),
        );
        let runtime = DispatchRuntime::daemon(DaemonRuntimeContext {
            registry,
            migrations: Arc::clone(&migrations),
        });
        (runtime, migrations)
    }

    fn stats_request(namespace: Option<&str>) -> Request {
        Request::Stats {
            namespace: namespace.map(str::to_string),
            recall: None,
        }
    }

    fn recall_request(namespace: Option<&str>) -> Request {
        Request::Recall {
            query: "legacy content".to_string(),
            limit: Some(3),
            namespace: namespace.map(str::to_string),
            reinforce: Some(false),
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
            event_kind: None,
        }
    }

    #[test]
    fn mismatch_schedules_exactly_one_background_job_per_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let compatible_db = dir.path().join("compatible.db");
        create_old_fixture(&db, dir.path());
        create_compatible_fixture(&compatible_db, dir.path());
        let backup = crate::memory::backup_path_for_database(&db)
            .expect("derive deterministic default migration backup path");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let provider = Arc::new(BlockingProvider::new(started_tx));
        let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed_unlocked_with(
            provider.clone(),
            db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        )));
        let (runtime, migrations) = daemon_runtime(registry, &db);
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                dispatch_two_phase(&runtime, stats_request(None))
            }));
        }
        barrier.wait();
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("one migration worker reaches embedding");
        let compatible_recall = ok_text(dispatch_two_phase(
            &runtime,
            recall_request(Some("compatible")),
        ));
        assert!(
            compatible_recall.contains("legacy content in compatible namespace"),
            "compatible namespace recall must complete while default migrates: {compatible_recall}"
        );
        for worker in workers {
            assert!(matches!(worker.join().unwrap(), Response::Err { .. }));
        }
        assert_eq!(migrations.job_count(), 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        provider.release();
        migrations.drain().expect("migration completes");
        assert!(
            backup.is_file(),
            "the single default migration job must preserve its backup at {backup:?}"
        );
        assert!(matches!(
            dispatch_two_phase(&runtime, stats_request(None)),
            Response::Ok { .. }
        ));
    }

    #[test]
    fn recall_while_migrating_returns_internal_and_hook_injects_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        create_old_fixture(&db, dir.path());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let provider = Arc::new(BlockingProvider::new(started_tx));
        let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed_unlocked_with(
            provider.clone(),
            db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        )));
        let (runtime, migrations) = daemon_runtime(registry, &db);
        assert!(matches!(
            dispatch_two_phase(&runtime, stats_request(None)),
            Response::Err { .. }
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("migration reaches embedding");
        let response = dispatch_two_phase(&runtime, recall_request(None));
        let Response::Err { kind, message } = response else {
            panic!("recall must be unavailable during migration")
        };
        assert_eq!(kind, ErrKind::Internal);
        let outcome: Result<Result<String, &str>, tokio::time::error::Elapsed> =
            Ok(Err(message.as_str()));
        assert!(crate::hook::interpret_recall_for_test(outcome).is_none());
        provider.release();
        migrations.drain().expect("migration completes");
    }

    #[test]
    fn backup_failure_never_starts_embedding_and_remains_fail_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        create_old_fixture(&db, dir.path());
        let before = SqliteStorage::inspect_embedding_migration(&db).unwrap();
        let backup = crate::memory::backup_path_for_database(&db).unwrap();
        std::fs::create_dir(&backup).unwrap();
        let (started_tx, _started_rx) = std::sync::mpsc::channel();
        let provider = Arc::new(BlockingProvider::new(started_tx));
        let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed_unlocked_with(
            provider.clone(),
            db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        )));
        let (runtime, migrations) = daemon_runtime(Arc::clone(&registry), &db);
        assert!(matches!(
            dispatch_two_phase(&runtime, stats_request(None)),
            Response::Err { .. }
        ));
        assert!(migrations.drain().is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .namespace_migration_state("default"),
            Some(crate::memory::NamespaceMigrationState::Failed { .. })
        ));
        assert!(matches!(
            dispatch_two_phase(&runtime, stats_request(None)),
            Response::Err { .. }
        ));
        let after = SqliteStorage::inspect_embedding_migration(&db).unwrap();
        assert_eq!(before.embedding_model, after.embedding_model);
        assert_eq!(before.embedding_dimensions, after.embedding_dimensions);
        assert!(after.checkpoint.is_none());
    }

    #[test]
    fn auto_migration_opt_out_does_not_mutate_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        create_old_fixture(&db, dir.path());
        let before = SqliteStorage::inspect_embedding_migration(&db).unwrap();
        let (started_tx, _started_rx) = std::sync::mpsc::channel();
        let provider = Arc::new(BlockingProvider::new(started_tx));
        let mut registry = MemoryRegistry::file_backed_unlocked_with(
            provider.clone(),
            db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        );
        registry.set_auto_migrate_embeddings(false);
        let registry = Arc::new(Mutex::new(registry));
        let (runtime, migrations) = daemon_runtime(registry, &db);
        let response = dispatch_two_phase(&runtime, stats_request(None));
        let Response::Err { message, .. } = response else {
            panic!("opt-out mismatch must fail open")
        };
        assert!(message.contains("ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS"));
        assert_eq!(migrations.job_count(), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let after = SqliteStorage::inspect_embedding_migration(&db).unwrap();
        assert_eq!(before.embedding_model, after.embedding_model);
        assert_eq!(before.embedding_dimensions, after.embedding_dimensions);
        assert!(after.checkpoint.is_none());
    }

    #[test]
    fn named_namespace_job_holds_its_own_lock() {
        let dir = tempfile::tempdir().unwrap();
        let default_db = dir.path().join("memory.db");
        let named_db = dir.path().join("other.db");
        create_old_fixture(&named_db, dir.path());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let provider = Arc::new(BlockingProvider::new(started_tx));
        let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed_unlocked_with(
            provider.clone(),
            default_db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        )));
        let (runtime, migrations) = daemon_runtime(registry, &default_db);
        assert!(matches!(
            dispatch_two_phase(&runtime, stats_request(Some("other"))),
            Response::Err { .. }
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("named migration reaches embedding");
        let mut named_lock_path = named_db.as_os_str().to_os_string();
        named_lock_path.push(".lock");
        let competitor = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(named_lock_path)
            .unwrap();
        assert!(fs4::FileExt::try_lock(&competitor).is_err());
        provider.release();
        migrations.drain().expect("named migration completes");
    }

    #[test]
    fn migration_lock_is_acquired_only_after_registry_guard_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let default_db = dir.path().join("memory.db");
        let named_db = dir.path().join("other.db");
        create_old_fixture(&named_db, dir.path());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let provider = Arc::new(BlockingProvider::new(started_tx));
        let registry = Arc::new(Mutex::new(MemoryRegistry::file_backed_unlocked_with(
            provider.clone(),
            default_db.clone(),
            dir.path().to_path_buf(),
            "default".to_string(),
            false,
        )));
        let (runtime, migrations) = daemon_runtime(Arc::clone(&registry), &default_db);
        let observed = Arc::new(AtomicUsize::new(0));
        let observed_clone = Arc::clone(&observed);
        let registry_clone = Arc::clone(&registry);
        migrations.set_lock_observer(Arc::new(move || {
            assert!(registry_clone.try_lock().is_ok());
            observed_clone.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(matches!(
            dispatch_two_phase(&runtime, stats_request(Some("other"))),
            Response::Err { .. }
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("migration reaches embedding");
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        provider.release();
        migrations.drain().expect("migration completes");
    }
    // ── R2 Task 8: extraction audit (RED) ──────────────────────────────────────

    fn extraction_audit_list(reg: &Arc<Mutex<MemoryRegistry>>) -> serde_json::Value {
        let text = ok_text(dispatch(
            reg,
            Request::ExtractionAuditList {
                namespace: None,
                limit: Some(100),
            },
        ));
        serde_json::from_str(&text).expect("audit list must be typed JSON")
    }
    fn rendered_extraction_audit(reg: &Arc<Mutex<MemoryRegistry>>) -> String {
        let text = ok_text(dispatch(
            reg,
            Request::ExtractionAuditList {
                namespace: None,
                limit: Some(100),
            },
        ));
        let result: crate::extract::audit::ExtractionAuditResult =
            serde_json::from_str(&text).expect("audit list must decode to its typed result");
        crate::extract::audit::render_audit_report(&result)
    }

    fn audit_fixture(
        reg: &Arc<Mutex<MemoryRegistry>>,
    ) -> (
        Vec<crate::extract::types::ExtractionSource>,
        serde_json::Value,
    ) {
        capture_turns(reg, "audit", "scope", 10, 100, "audit");
        let profile = extraction_profile();
        let sources = extraction_scan(reg, profile.clone()).sources;
        staged_result(stage_extraction(
            reg,
            profile.clone(),
            sources.clone(),
            canonical_extraction(
                &profile,
                &sources,
                &[
                    ("one", sources[0].node_id),
                    ("two", sources[1].node_id),
                    ("three", sources[2].node_id),
                ],
                &[("one", "two", crate::extract::types::RelationKind::Supports)],
            ),
        ));
        (sources, extraction_audit_list(reg))
    }

    fn audit_candidate<'a>(
        audit: &'a serde_json::Value,
        item_local_id: &str,
    ) -> &'a serde_json::Value {
        audit["candidates"]
            .as_array()
            .expect("candidate rows")
            .iter()
            .find(|candidate| candidate["item_local_id"] == item_local_id)
            .unwrap_or_else(|| panic!("candidate {item_local_id} must be present"))
    }
    fn audit_source<'a>(candidate: &'a serde_json::Value, turn_key: &str) -> &'a serde_json::Value {
        candidate["sources"]
            .as_array()
            .expect("candidate sources")
            .iter()
            .find(|source| source["turn_key"] == turn_key)
            .unwrap_or_else(|| panic!("source {turn_key} must be present"))
    }
    fn authoritative_source_content<'a>(
        sources: &'a [crate::extract::types::ExtractionSource],
        turn_key: &str,
    ) -> &'a str {
        &sources
            .iter()
            .find(|source| source.turn_key == turn_key)
            .unwrap_or_else(|| panic!("authoritative source {turn_key} must be present"))
            .content
    }

    fn audit_relation<'a>(
        audit: &'a serde_json::Value,
        from_item_local_id: &str,
        to_item_local_id: &str,
    ) -> &'a serde_json::Value {
        audit["relations"]
            .as_array()
            .expect("relation rows")
            .iter()
            .find(|relation| {
                relation["from_item_local_id"] == from_item_local_id
                    && relation["to_item_local_id"] == to_item_local_id
            })
            .unwrap_or_else(|| {
                panic!("relation {from_item_local_id} -> {to_item_local_id} must be present")
            })
    }

    fn assert_audit_rejection(response: &Response) {
        let Response::Err { kind, message } = response else {
            panic!("unavailable or mismatched audit source must reject: {response:?}");
        };
        assert_eq!(*kind, ErrKind::InvalidParams);
        assert_eq!(
            message,
            "extraction audit candidate sources are unavailable or mismatched"
        );
    }
    #[test]
    fn extraction_audit_resolves_a_unique_live_turn_key_after_node_id_changes() {
        let (reg, _dir) = stub_registry();
        let (sources, _) = audit_fixture(&reg);
        let original = &sources[0];
        let replacement_id = {
            let handle = {
                let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
                registry.namespace_handle(None).expect("default namespace")
            };
            let mut memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
            let graph = memory.engine_mut().graph_mut();
            let mut replacement = graph
                .get_node(anamnesis::graph::NodeId(original.node_id))
                .expect("staged source node")
                .clone();
            let replacement_id = graph.next_node_id();
            replacement.id = replacement_id;
            graph
                .add_node(replacement)
                .expect("replacement source node");
            graph
                .remove_node(anamnesis::graph::NodeId(original.node_id))
                .expect("remove stale node-id hint");
            replacement_id.0
        };

        let audit = extraction_audit_list(&reg);
        let source = audit_source(audit_candidate(&audit, "one"), &original.turn_key);
        assert_eq!(source["availability"], "available");
        assert_eq!(source["node_id"], replacement_id);
        assert_eq!(source["content"], original.content);
    }
    #[test]
    fn extraction_audit_uses_a_valid_node_id_hint_when_turn_key_is_ambiguous() {
        let (reg, _dir) = stub_registry();
        let (sources, _) = audit_fixture(&reg);
        let original = &sources[0];
        {
            let handle = {
                let mut registry = reg.lock().unwrap_or_else(|poison| poison.into_inner());
                registry.namespace_handle(None).expect("default namespace")
            };
            let mut memory = handle.lock().unwrap_or_else(|poison| poison.into_inner());
            let graph = memory.engine_mut().graph_mut();
            let mut duplicate = graph
                .get_node(anamnesis::graph::NodeId(original.node_id))
                .expect("staged source node")
                .clone();
            duplicate.id = graph.next_node_id();
            graph
                .add_node(duplicate)
                .expect("duplicate capture source node");
        }

        let audit = extraction_audit_list(&reg);
        let source = audit_source(audit_candidate(&audit, "one"), &original.turn_key);
        assert_eq!(source["availability"], "available");
        assert_eq!(source["node_id"], original.node_id);
        assert_eq!(source["content"], original.content);
    }

    #[test]
    fn extraction_audit_demo_reads_live_sources_and_never_mutates_the_graph() {
        let (reg, dir) = stub_registry();
        let (sources, listed) = audit_fixture(&reg);
        let connection = rusqlite::Connection::open(dir.path().join("memory.db"))
            .expect("open audit policy database");
        let provenance_columns: Vec<String> = connection
            .prepare(
                "SELECT name FROM pragma_table_info('extract_candidates')
                 WHERE name IN ('source_turn_keys', 'source_content_hashes', 'source_node_ids')",
            )
            .expect("inspect candidate provenance schema")
            .query_map([], |row| row.get(0))
            .expect("query candidate provenance schema")
            .collect::<Result<_, _>>()
            .expect("read candidate provenance schema");
        assert_eq!(
            provenance_columns,
            [
                "source_turn_keys",
                "source_content_hashes",
                "source_node_ids"
            ],
            "policy provenance stores only source identity, hashes, and node ids"
        );
        let stored_candidate_contents: Vec<String> = connection
            .prepare("SELECT content FROM extract_candidates")
            .expect("inspect staged candidate content")
            .query_map([], |row| row.get(0))
            .expect("query staged candidate content")
            .collect::<Result<_, _>>()
            .expect("read staged candidate content");
        assert!(
            stored_candidate_contents
                .iter()
                .all(|content| !sources.iter().any(|source| content == &source.content)),
            "policy tables must not persist raw source content as candidate provenance"
        );
        drop(connection);
        let candidates = listed["candidates"].as_array().expect("candidate rows");
        let relations = listed["relations"].as_array().expect("relation rows");

        assert_eq!(candidates.len(), 3);
        assert_eq!(relations.len(), 1);
        let one = audit_candidate(&listed, "one");
        let two = audit_candidate(&listed, "two");
        let three = audit_candidate(&listed, "three");
        let one_turn_key = one["source_turn_keys"][0]
            .as_str()
            .expect("one source turn key");
        let two_turn_key = two["source_turn_keys"][0]
            .as_str()
            .expect("two source turn key");
        let three_turn_key = three["source_turn_keys"][0]
            .as_str()
            .expect("three source turn key");
        let one_source = audit_source(one, one_turn_key);
        let two_source = audit_source(two, two_turn_key);
        let three_source = audit_source(three, three_turn_key);
        assert_eq!(one["content"], "derived one");
        assert_eq!(one["kind"], "lesson");
        assert_eq!(one["confidence"], 0.9);
        assert_eq!(one_source["availability"], "available");
        assert_eq!(
            one_source["content"],
            authoritative_source_content(&sources, one_turn_key)
        );
        assert_eq!(two["content"], "derived two");
        assert_eq!(two_source["availability"], "available");
        assert_eq!(
            two_source["content"],
            authoritative_source_content(&sources, two_turn_key)
        );
        assert_eq!(three["content"], "derived three");
        assert_eq!(three_source["availability"], "available");
        assert_eq!(
            three_source["content"],
            authoritative_source_content(&sources, three_turn_key)
        );
        let first_candidate = one["id"].as_u64().expect("one candidate id");
        let second_candidate = two["id"].as_u64().expect("two candidate id");
        let third_candidate = three["id"].as_u64().expect("three candidate id");
        let relation = audit_relation(&listed, "one", "two");
        let relation_id = relation["id"].as_u64().expect("relation id");
        assert_eq!(relation["run_id"], one["run_id"]);
        assert_eq!(relation["profile_id"], one["profile_id"]);
        assert_eq!(relation["from_item_local_id"], "one");
        assert_eq!(relation["to_item_local_id"], "two");
        assert_eq!(relation["relation_type"], "supports");

        let before_list = graph_snapshot(&reg);
        let relisted = extraction_audit_list(&reg);
        assert_eq!(relisted, listed, "list must be a pure graph read");
        assert_eq!(
            graph_snapshot(&reg),
            before_list,
            "list must not mutate graph"
        );

        let updated_candidate = dispatch(
            &reg,
            Request::UpdateExtractionCandidateAudit {
                namespace: None,
                candidate_id: first_candidate,
                support: crate::extract::types::AuditSupport::Partial,
                contamination: Some(crate::extract::types::ContaminationCategory::UnsupportedClaim),
                reviewer: "  reviewer  ".into(),
            },
        );
        assert!(
            matches!(updated_candidate, Response::Ok { .. }),
            "{updated_candidate:?}"
        );
        let updated_relation = dispatch(
            &reg,
            Request::UpdateExtractionRelationAudit {
                namespace: None,
                relation_id,
                verdict: crate::extract::types::RelationVerdict::WrongDirection,
                reviewer: "reviewer".into(),
            },
        );
        assert!(
            matches!(updated_relation, Response::Ok { .. }),
            "{updated_relation:?}"
        );
        let reviewed = extraction_audit_list(&reg);
        let candidate = audit_candidate(&reviewed, "one");
        let relation = audit_relation(&reviewed, "one", "two");
        assert_eq!(candidate["audit_support"], "partial");
        assert_eq!(candidate["contamination_category"], "unsupported-claim");
        assert_eq!(candidate["reviewed_by"], "reviewer");
        assert!(
            candidate["reviewed_at"].is_u64(),
            "candidate review must be timestamped"
        );
        assert_eq!(relation["audit_status"], "wrong-direction");
        assert_eq!(relation["reviewed_by"], "reviewer");
        assert!(
            relation["reviewed_at"].is_u64(),
            "relation review must be timestamped"
        );
        assert_eq!(
            graph_snapshot(&reg),
            before_list,
            "valid candidate and relation audit writes are policy-only"
        );

        ok_text(dispatch(
            &reg,
            Request::Forget {
                id: one_source["node_id"].as_u64().expect("one source node id"),
                reason: None,
                hard: Some(true),
                namespace: None,
            },
        ));
        let before_rejected_update = graph_snapshot(&reg);
        let unavailable = extraction_audit_list(&reg);
        let unavailable_one = audit_candidate(&unavailable, "one");
        let unavailable_one_source = audit_source(unavailable_one, one_turn_key);
        assert_eq!(unavailable_one_source["availability"], "source-unavailable");
        assert_eq!(unavailable_one_source["content"], serde_json::Value::Null);
        assert!(rendered_extraction_audit(&reg).contains("AUDIT UNAVAILABLE: source-unavailable"));
        let rejected = dispatch(
            &reg,
            Request::UpdateExtractionCandidateAudit {
                namespace: None,
                candidate_id: first_candidate,
                support: crate::extract::types::AuditSupport::Unsupported,
                contamination: None,
                reviewer: "reviewer".into(),
            },
        );
        assert_audit_rejection(&rejected);
        assert_eq!(
            graph_snapshot(&reg),
            before_rejected_update,
            "rejected unavailable-source audit cannot mutate graph"
        );

        ok_text(dispatch(
            &reg,
            Request::Update {
                id: two_source["node_id"].as_u64().expect("two source node id"),
                new_content: "live content no longer matches staged source".into(),
                namespace: None,
            },
        ));
        let before_mismatch_rejection = graph_snapshot(&reg);
        let mismatched = extraction_audit_list(&reg);
        let mismatched_two = audit_candidate(&mismatched, "two");
        let mismatched_two_source = audit_source(mismatched_two, two_turn_key);
        assert_eq!(mismatched_two_source["availability"], "source-mismatch");
        assert_eq!(mismatched_two_source["content"], serde_json::Value::Null);
        assert!(rendered_extraction_audit(&reg).contains("AUDIT UNAVAILABLE: source-mismatch"));
        let rejected = dispatch(
            &reg,
            Request::UpdateExtractionCandidateAudit {
                namespace: None,
                candidate_id: second_candidate,
                support: crate::extract::types::AuditSupport::Unsupported,
                contamination: None,
                reviewer: "reviewer".into(),
            },
        );
        assert_audit_rejection(&rejected);
        assert_eq!(
            graph_snapshot(&reg),
            before_mismatch_rejection,
            "rejected mismatched-source audit cannot mutate graph"
        );

        let connection = rusqlite::Connection::open(dir.path().join("memory.db"))
            .expect("open audit policy database");
        connection
            .execute(
                "UPDATE extract_candidates SET source_content_hashes = '[]' WHERE id = ?1",
                [third_candidate],
            )
            .expect("empty persisted provenance hashes");
        let before_malformed_rejection = graph_snapshot(&reg);
        let malformed = extraction_audit_list(&reg);
        assert!(
            audit_candidate(&malformed, "three")["sources"]
                .as_array()
                .expect("malformed provenance sources")
                .is_empty(),
            "empty provenance vectors must not be treated as available"
        );
        let rejected = dispatch(
            &reg,
            Request::UpdateExtractionCandidateAudit {
                namespace: None,
                candidate_id: third_candidate,
                support: crate::extract::types::AuditSupport::Unsupported,
                contamination: None,
                reviewer: "reviewer".into(),
            },
        );
        assert_audit_rejection(&rejected);
        assert_eq!(
            graph_snapshot(&reg),
            before_malformed_rejection,
            "malformed provenance rejection cannot mutate graph"
        );
    }
}
