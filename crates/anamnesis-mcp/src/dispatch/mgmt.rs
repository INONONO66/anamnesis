//! The single-phase management tools: `update`/`forget`/`supersede`/`list`/`get`.
//!
//! Unlike `recall`/`remember`/`relate`/`ingest`/`stats` (which run the full
//! three-phase discipline documented on `dispatch`'s module doc), these tools
//! need only ONE brief global-lock resolve ([`resolve_handle`]) followed by the
//! namespace-locked op, so they share a simpler two-step shape: resolve, then
//! lock-and-call, classifying any error via [`bump_and_classify`]. Split out of
//! `dispatch.rs` verbatim (behavior-preserving move only) — the arm bodies
//! below are byte-identical to their prior inline form.

use std::sync::{Arc, Mutex};

use anamnesis::graph::NodeId;
use anamnesis::memory::ListFilter;

use crate::memory::{self, MemoryRegistry};
use crate::proto::Response;

use super::render;

/// Phase 1 for the management tools (`update`/`forget`/`supersede`/`list`/
/// `get`): resolve the namespace handle under a brief global lock, bumping
/// `dispatch_errors` and returning a ready [`Response`] on failure.
fn resolve_handle(
    registry: &Arc<Mutex<MemoryRegistry>>,
    namespace: Option<&str>,
) -> Result<Arc<Mutex<anamnesis::Memory>>, Response> {
    let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
    reg.namespace_handle(namespace).map_err(|e| {
        reg.ops.dispatch_errors += 1;
        Response::internal(e)
    })
}

/// Phase 3 for the management tools: brief global lock to bump
/// `dispatch_errors`, then classify a `Memory` error as caller-vs-internal
/// (missing/bad id ⇒ `invalid_params`; anything else ⇒ `internal`).
fn bump_and_classify(registry: &Arc<Mutex<MemoryRegistry>>, err: anamnesis::Error) -> Response {
    let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
    reg.ops.dispatch_errors += 1;
    if memory::is_caller_error(&err) {
        Response::invalid_params(err)
    } else {
        Response::internal(err)
    }
}

/// Body of `dispatch`'s `Request::Update` arm.
pub(crate) fn dispatch_update(
    registry: &Arc<Mutex<MemoryRegistry>>,
    id: u64,
    new_content: &str,
    namespace: Option<&str>,
) -> Response {
    let handle = match resolve_handle(registry, namespace) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let result = {
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        memory::mem_update(&mut mem, NodeId(id), new_content)
    };
    match result {
        Ok(()) => Response::ok(format!("updated node {id}")),
        Err(e) => bump_and_classify(registry, e),
    }
}

/// Body of `dispatch`'s `Request::Forget` arm.
pub(crate) fn dispatch_forget(
    registry: &Arc<Mutex<MemoryRegistry>>,
    id: u64,
    reason: Option<String>,
    hard: Option<bool>,
    namespace: Option<&str>,
) -> Response {
    let handle = match resolve_handle(registry, namespace) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let hard = hard.unwrap_or(false);
    let result = {
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        if hard {
            memory::mem_forget_hard(&mut mem, NodeId(id))
        } else {
            let reason = reason.unwrap_or_else(|| "forgotten via MCP".to_string());
            memory::mem_forget(&mut mem, NodeId(id), &reason)
        }
    };
    match result {
        Ok(()) if hard => Response::ok(format!("forgot node {id} (hard delete)")),
        Ok(()) => Response::ok(format!("forgot node {id} (soft delete)")),
        Err(e) => bump_and_classify(registry, e),
    }
}

/// Body of `dispatch`'s `Request::Supersede` arm.
pub(crate) fn dispatch_supersede(
    registry: &Arc<Mutex<MemoryRegistry>>,
    new_id: u64,
    old_id: u64,
    namespace: Option<&str>,
) -> Response {
    let handle = match resolve_handle(registry, namespace) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let result = {
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        memory::mem_supersede(&mut mem, NodeId(new_id), NodeId(old_id))
    };
    match result {
        Ok(edge) => Response::ok(format!("superseded {old_id} by {new_id} (edge {edge})")),
        Err(e) => bump_and_classify(registry, e),
    }
}

/// Body of `dispatch`'s `Request::List` arm.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_list(
    registry: &Arc<Mutex<MemoryRegistry>>,
    min_salience: Option<f64>,
    limit: Option<u32>,
    node_type: Option<String>,
    tag: Option<String>,
    namespace: Option<&str>,
    scope: Option<String>,
    metadata: Option<String>,
) -> Response {
    let handle = match resolve_handle(registry, namespace) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let metadata = match metadata.as_deref().map(memory::parse_metadata_filter) {
        Some(Ok(kv)) => Some(kv),
        Some(Err(e)) => return bump_and_classify(registry, e),
        None => None,
    };
    let filter = ListFilter {
        min_salience: min_salience.unwrap_or(0.0),
        limit: limit.map(|l| l as usize).unwrap_or(100),
        node_type: node_type.as_deref().map(memory::parse_knowledge_type),
        tag,
        scope,
        metadata,
    };
    let result = {
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        memory::mem_list(&mut mem, &filter)
    };
    match result {
        Ok(views) => Response::ok(render::render_list(&views)),
        Err(e) => bump_and_classify(registry, e),
    }
}

/// Body of `dispatch`'s `Request::Get` arm.
pub(crate) fn dispatch_get(
    registry: &Arc<Mutex<MemoryRegistry>>,
    id: u64,
    namespace: Option<&str>,
) -> Response {
    let handle = match resolve_handle(registry, namespace) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let result = {
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        memory::mem_get(&mut mem, NodeId(id))
    };
    match result {
        Ok(view) => Response::ok(render::render_view(&view)),
        Err(e) => bump_and_classify(registry, e),
    }
}
