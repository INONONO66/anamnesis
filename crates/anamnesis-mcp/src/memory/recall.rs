//! Gated recall primitives: the `recall`/`recall_packaged`/`recall_packaged_gated`
//! registry methods and their namespace-locked bodies, plus recall gates and the
//! scope/tag post-filter applied before rendering.

use std::collections::HashSet;

use anamnesis::graph::{NodeId, Timestamp};
use anamnesis::memory::Hit;
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};

use super::{MemoryRegistry, PackagedRecall};

pub(crate) struct RecallFilters<'a> {
    pub(crate) gate: Option<f64>,
    pub(crate) cosine_gate: Option<f64>,
    pub(crate) scope: Option<&'a str>,
    pub(crate) tag: Option<&'a str>,
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
    pub fn recall_packaged(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<PackagedRecall, Error> {
        // The classic path: reinforce per the registry default, no gate.
        self.recall_packaged_gated(query, limit, ns, None, None, None)
    }

    /// Gated, optionally read-only variant of [`recall_packaged`](Self::recall_packaged)
    /// for the Claude Code hook path.
    ///
    /// - `reinforce`: `None` ⇒ use the registry's configured default; `Some(false)`
    ///   ⇒ a pure read (skip the reinforcing `used()` commit); `Some(true)` ⇒ force
    ///   reinforcement.
    /// - `gate`: the need-odds threshold `τ`. After ranking, if there are no hits OR
    ///   the top hit's score is `< τ`, return an **empty** [`PackagedRecall`] (empty
    ///   `context`, empty `hits`) so the caller injects nothing. `None` ⇒ no gate.
    ///
    /// Tick semantics match [`recall`](Self::recall): exactly ONE tick per call
    /// (see its doc for why not two), after the search, on every branch
    /// (gated-out or not) — durability of any reinforcement (or of the gated-out
    /// read) never depends on how the call resolved. When the gate trips, the
    /// read is pure (never reinforces) regardless of `reinforce`, since there is
    /// nothing relevant to mark as used.
    pub fn recall_packaged_gated(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
        reinforce: Option<bool>,
        gate: Option<f64>,
        cosine_gate: Option<f64>,
    ) -> Result<PackagedRecall, Error> {
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
) -> Result<PackagedRecall, Error> {
    let recall = mem.search(query, limit)?;

    if gated_out(gate, cosine_gate, &recall.hits) {
        // Pure read, no reinforcement, nothing to inject. Still tick once for
        // durability / decay-clock advancement (single tick per call).
        mem.tick(Timestamp::now())?;
        #[cfg(test)]
        super::record_tick();
        return Ok(PackagedRecall {
            context: String::new(),
            hits: Vec::new(),
        });
    }

    // Render the context block from the package BEFORE `used` consumes it.
    let context = recall.as_context();
    let raw = recall.hits.clone();
    if reinforce {
        mem.used(recall)?;
    }
    // Single tick per call (see `MemoryRegistry::recall` for why not two).
    mem.tick(Timestamp::now())?;
    #[cfg(test)]
    super::record_tick();
    let hits = super::dedup_hits(raw);
    Ok(PackagedRecall { context, hits })
}

/// Like [`mem_recall_packaged_gated`], with a scope/tag filter applied to the
/// [`ContextPackage`](anamnesis::query::ContextPackage) BEFORE rendering — so an excluded node's content never
/// reaches the rendered `context` block (identity/knowledge/memories/tensions),
/// not just the compact `NODES` list. Re-implements the gate/render/reinforce
/// sequence rather than delegating to `mem_recall_packaged_gated`, since that
/// function renders before returning and the filter must run earlier.
pub(crate) fn mem_recall_packaged_gated_filtered(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    limit: usize,
    reinforce: bool,
    filters: RecallFilters<'_>,
) -> Result<PackagedRecall, Error> {
    if filters.scope.is_none() && filters.tag.is_none() {
        return mem_recall_packaged_gated(
            mem,
            query,
            limit,
            reinforce,
            filters.gate,
            filters.cosine_gate,
        );
    }

    let mut recall = mem.search(query, limit)?;

    if gated_out(filters.gate, filters.cosine_gate, &recall.hits) {
        mem.tick(Timestamp::now())?;
        #[cfg(test)]
        super::record_tick();
        return Ok(PackagedRecall {
            context: String::new(),
            hits: Vec::new(),
        });
    }

    filter_context_package(mem, &mut recall.package, filters.scope, filters.tag);
    recall
        .hits
        .retain(|h| node_matches_scope_tag(mem, h.node_id, filters.scope, filters.tag));

    let context = recall.as_context();
    let raw = recall.hits.clone();
    if reinforce {
        mem.used(recall)?;
    }
    mem.tick(Timestamp::now())?;
    #[cfg(test)]
    super::record_tick();
    let hits = super::dedup_hits(raw);
    Ok(PackagedRecall { context, hits })
}

fn gated_out(gate: Option<f64>, cosine_gate: Option<f64>, hits: &[Hit]) -> bool {
    let top = hits.first();
    let readout_trip = match (gate, top.map(|h| h.score)) {
        (Some(tau), Some(score)) => score < tau,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let cosine_trip = match (cosine_gate, top.map(|h| h.cosine)) {
        (Some(tau), Some(cosine)) => cosine < tau,
        (Some(_), None) => true,
        (None, _) => false,
    };
    readout_trip || cosine_trip
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
    let scope_ok = scope.is_none_or(|s| node.origin.scope.as_str() == s);
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
