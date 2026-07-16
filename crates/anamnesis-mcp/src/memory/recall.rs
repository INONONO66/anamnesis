//! Gated recall primitives: the `recall`/`recall_packaged`/`recall_packaged_gated`
//! registry methods and their namespace-locked bodies, plus recall gates and the
//! scope/tag post-filter applied before rendering.

use std::collections::HashSet;

use crate::capture::META_CAPTURE;
use anamnesis::graph::{NodeId, ScopePath, Timestamp};
use anamnesis::memory::{Hit, Recall};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};

use super::{MemoryRegistry, PackagedRecall, RecallGateTrace, RecallOutcome};

pub(crate) struct RecallFilters<'a> {
    pub(crate) gate: Option<f64>,
    pub(crate) cosine_gate: Option<f64>,
    pub(crate) scope: Option<&'a str>,
    pub(crate) tag: Option<&'a str>,
    pub(crate) knowledge_only: bool,
}

impl MemoryRegistry {
    /// Search; on success optionally auto-commit (reinforce) the returned package.
    /// A single lazy `tick(now)` after the search keeps forgetting current
    /// without a background thread and persists the reinforcement.
    ///
    /// Returns the raw de-duplicated [`Hit`] list. The CLI/server paths use
    /// [`recall_packaged`](Self::recall_packaged) (which also renders the context
    /// block), so in a non-test build this primitive has only test consumers.
    ///
    /// Ticks the engine exactly ONCE (flagship bug #2): an earlier revision also
    /// ticked before the search for same-call ranking freshness, but `tick` is
    /// not a no-op to call twice per recall — idle-edge leakage and node decay
    /// both key off elapsed time since the last tick, so a second tick a few
    /// milliseconds later doubled decay/leak pressure on every single read (and
    /// this method already runs on every recall, so the doubling compounded
    /// per-call, not just per session). One tick per recall restores
    /// call-frequency independence; the trade-off is that ranking for THIS
    /// call's own `search` uses decay as of the previous tick, not this instant.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn recall(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<Vec<Hit>, Error> {
        let reinforce = self.reinforce_on_recall;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        // `seed_limit` tracks `limit` inside `search`, and the RWR is noisier with
        // more seeds, so do NOT oversample to refill the top-k — that measurably
        // hurts ranking. Instead search `limit`, then collapse the Episodic+Semantic
        // copies `add_note` creates. This collapse alone lifts insight Recall@5 from
        // 0.375 to 0.94 (see src/eval.rs); the trade-off is that a heavily-duplicated
        // result can return fewer than `limit` distinct hits.
        let recall = mem.search(query, limit)?;
        let raw = recall.hits.clone();
        if reinforce {
            mem.used(recall)?;
        }
        // `Engine::commit` does not flush storage, so this `tick` persists any
        // reinforcement to SQLite (without it a CLI one-shot `recall`, or
        // `serve`'s last recall before shutdown, would lose it) and advances the
        // decay clock the NEXT recall's `search` will rank against.
        mem.tick(Timestamp::now())?;
        #[cfg(test)]
        super::record_tick();
        Ok(super::dedup_hits(raw))
    }

    /// Like [`recall`](Self::recall), but also returns the readable context block
    /// rendered from the assembled package (`Recall::as_context`).
    ///
    /// The `context` string is the primary, human-readable `recall` payload; the
    /// `hits` carry the same de-duplicated ranked list so the agent can pass
    /// `node_id`s on to `relate`. Reinforcement / tick semantics are identical to
    /// [`recall`](Self::recall).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn recall_packaged(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<PackagedRecall, Error> {
        // The classic path retains its packaged-only API.
        self.recall_packaged_gated(query, limit, ns, None, None, None)
            .map(|outcome| outcome.packaged)
    }

    /// Gated, optionally read-only variant of [`recall_packaged`](Self::recall_packaged)
    /// for the Claude Code hook path.
    ///
    /// - `reinforce`: `None` ⇒ use the registry's configured default; `Some(false)`
    ///   ⇒ a pure read (skip the reinforcing `used()` commit); `Some(true)` ⇒ force
    ///   reinforcement.
    /// - `gate`: the need-odds threshold `τ`. The final filtered, de-duplicated top
    ///   hit must pass it for the returned package to be eligible.
    ///
    /// Tick semantics match [`recall`](Self::recall): exactly ONE tick per call
    /// after the search, including gated-out calls. Gated-out calls never reinforce.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn recall_packaged_gated(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
        reinforce: Option<bool>,
        gate: Option<f64>,
        cosine_gate: Option<f64>,
    ) -> Result<RecallOutcome, Error> {
        // Count every recall; a recall is "reinforcing" per the SAME resolution
        // the method uses below (`reinforce.unwrap_or(self.reinforce_on_recall)`).
        // Counted on intent, before the gate can turn a would-be reinforce into a
        // pure read — the metric tracks how the caller asked to recall.
        self.ops.recalls += 1;
        if reinforce == Some(true) || (reinforce.is_none() && self.reinforce_on_recall) {
            self.ops.reinforcing_recalls += 1;
        }
        let reinforce = reinforce.unwrap_or(self.reinforce_on_recall);
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_recall_packaged_gated(&mut mem, query, limit, reinforce, gate, cosine_gate)
    }
}

// ── Namespace-locked primitives (phase-2 work) ───────────────────────────────
//
// Each function below operates on an already-resolved `&mut Memory` — no
// registry access, no global lock. `crate::dispatch` calls these directly
// between acquiring and releasing a namespace's `Mutex`, and the
// `MemoryRegistry` convenience methods above call the SAME functions after
// locking their own resolved handle, so the two call paths can never diverge.

/// Namespace-locked body of [`MemoryRegistry::recall_packaged_gated`].
pub(crate) fn mem_recall_packaged_gated(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    limit: usize,
    reinforce: bool,
    gate: Option<f64>,
    cosine_gate: Option<f64>,
) -> Result<RecallOutcome, Error> {
    let recall = mem.search(query, limit)?;
    finish_recall(mem, recall, reinforce, gate, cosine_gate)
}

/// Like [`mem_recall_packaged_gated`], with a scope/tag filter applied to the
/// [`ContextPackage`](anamnesis::query::ContextPackage) before rendering.
pub(crate) fn mem_recall_packaged_gated_filtered(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    limit: usize,
    reinforce: bool,
    filters: RecallFilters<'_>,
) -> Result<RecallOutcome, Error> {
    if filters.scope.is_none() && filters.tag.is_none() && !filters.knowledge_only {
        return mem_recall_packaged_gated(
            mem,
            query,
            limit,
            reinforce,
            filters.gate,
            filters.cosine_gate,
        );
    }

    let scope_path = filters.scope.map(ScopePath::new).transpose()?;
    let mut recall = match scope_path {
        Some(scope) => mem.search_scoped(query, limit, Some(scope))?,
        None => mem.search(query, limit)?,
    };

    filter_context_package(mem, &mut recall.package, filters.scope, filters.tag);
    recall
        .hits
        .retain(|h| node_matches_scope_tag(mem, h.node_id, filters.scope, filters.tag));
    if filters.knowledge_only {
        apply_knowledge_only(mem, &mut recall.package, &mut recall.hits);
    }

    finish_recall(mem, recall, reinforce, filters.gate, filters.cosine_gate)
}

/// Compute the gate decision for the final, de-duplicated top hit.
pub(crate) fn gate_trace(
    top: Option<&Hit>,
    gate: Option<f64>,
    cosine_gate: Option<f64>,
) -> RecallGateTrace {
    let Some(top) = top else {
        return RecallGateTrace {
            has_hits: false,
            readout_pass: false,
            cosine_pass: false,
            eligible: false,
            top_score: None,
            top_cosine: None,
            gate_threshold: gate,
            cosine_gate,
            result_node_ids: Vec::new(),
            auto_extract_node_count: 0,
        };
    };
    let readout_pass = gate.is_none_or(|threshold| top.score >= threshold);
    let cosine_pass = cosine_gate.is_none_or(|threshold| top.cosine >= threshold);
    RecallGateTrace {
        has_hits: true,
        readout_pass,
        cosine_pass,
        eligible: readout_pass && cosine_pass,
        top_score: Some(top.score),
        top_cosine: Some(top.cosine),
        gate_threshold: gate,
        cosine_gate,
        result_node_ids: Vec::new(),
        auto_extract_node_count: 0,
    }
}

fn finish_recall(
    mem: &mut Memory<SqliteStorage>,
    recall: Recall,
    reinforce: bool,
    gate: Option<f64>,
    cosine_gate: Option<f64>,
) -> Result<RecallOutcome, Error> {
    let hits = super::dedup_hits(recall.hits.clone());
    let mut trace = gate_trace(hits.first(), gate, cosine_gate);

    if !trace.eligible {
        mem.tick(Timestamp::now())?;
        #[cfg(test)]
        super::record_tick();
        return Ok(RecallOutcome {
            packaged: PackagedRecall {
                context: String::new(),
                hits: Vec::new(),
            },
            trace,
        });
    }

    trace.result_node_ids = hits.iter().map(|hit| hit.node_id.0).collect();
    trace.auto_extract_node_count = hits
        .iter()
        .filter(|hit| match mem.engine().graph().get_node(hit.node_id) {
            Ok(node) => node
                .metadata
                .get("origin")
                .is_some_and(|origin| origin == "auto-extract"),
            Err(error) => {
                tracing::warn!(
                    node_id = hit.node_id.0,
                    "recall result metadata lookup failed: {error}"
                );
                false
            }
        })
        .count();

    // Render before `used` consumes the package; preserve the existing
    // reinforce-then-tick order.
    let context = recall.as_context();
    if reinforce {
        mem.used(recall)?;
    }
    mem.tick(Timestamp::now())?;
    #[cfg(test)]
    super::record_tick();
    Ok(RecallOutcome {
        packaged: PackagedRecall { context, hits },
        trace,
    })
}

/// Whether `node_id`'s origin scope and entity tags satisfy the requested
/// `scope`/`tag` filters (`None` ⇒ that filter is not applied). A node lookup
/// failure is treated as non-matching (excluded), never a panic.
fn node_matches_scope_tag(
    mem: &Memory<SqliteStorage>,
    node_id: NodeId,
    scope: Option<&str>,
    tag: Option<&str>,
) -> bool {
    let Ok(node) = mem.engine().graph().get_node(node_id) else {
        return false;
    };
    let scope_ok =
        scope.is_none_or(|s| node.origin.scope.is_universal() || node.origin.scope.as_str() == s);
    let tag_ok = tag.is_none_or(|t| node.entity_tags.iter().any(|et| et == t));
    scope_ok && tag_ok
}

/// Drop every fragment (identity/knowledge/memories) and tension in `package`
/// whose referenced node doesn't satisfy the scope/tag filter. A tension is
/// dropped if either endpoint was dropped, so a filtered-out node's existence
/// never leaks through a surviving tension line either.
fn filter_context_package(
    mem: &Memory<SqliteStorage>,
    package: &mut anamnesis::query::ContextPackage,
    scope: Option<&str>,
    tag: Option<&str>,
) {
    let retain_matching = |frags: &mut Vec<anamnesis::query::Fragment>| {
        frags.retain(|f| node_matches_scope_tag(mem, f.node_id, scope, tag));
    };
    retain_matching(&mut package.identity);
    retain_matching(&mut package.knowledge);
    retain_matching(&mut package.memories);

    let surviving: HashSet<NodeId> = package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .map(|f| f.node_id)
        .collect();
    package
        .tensions
        .retain(|t| surviving.contains(&t.node_a) && surviving.contains(&t.node_b));
}

fn apply_knowledge_only(
    mem: &Memory<SqliteStorage>,
    package: &mut anamnesis::query::ContextPackage,
    hits: &mut Vec<Hit>,
) {
    package.memories.clear();
    package.tensions.clear();
    package
        .identity
        .retain(|f| !is_capture_node(mem, f.node_id));
    package
        .knowledge
        .retain(|f| !is_capture_node(mem, f.node_id));
    hits.retain(|h| !is_capture_node(mem, h.node_id));
}

fn is_capture_node(mem: &Memory<SqliteStorage>, node_id: NodeId) -> bool {
    mem.engine()
        .graph()
        .get_node(node_id)
        .map(|n| n.metadata.get(META_CAPTURE).is_some_and(|v| v == "true"))
        .unwrap_or(true)
}
