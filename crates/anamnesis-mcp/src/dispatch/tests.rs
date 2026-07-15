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
            node.metadata.get("capture").map(String::as_str),
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

    let stats = ok_text(dispatch(&reg, Request::Stats { namespace: None }));
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
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match fs4::FileExt::try_lock(&lock) {
                Ok(()) => break,
                Err(fs4::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("acquire daemon lock before deadline: {error}"),
            }
        }
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
}
