//! Gated recall primitives: the `recall`/`recall_packaged`/`recall_packaged_gated`
//! registry methods and their namespace-locked bodies, plus recall gates and the
//! scope/tag post-filter applied before rendering.

use std::collections::{HashMap, HashSet};

use crate::capture::META_CAPTURE;
#[cfg(test)]
use anamnesis::embedding::RerankScore;
use anamnesis::embedding::RerankingProvider;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp, valid_at};
use anamnesis::memory::{
    CognitiveRecallScore, ContextRenderOptions, ContextRenderStyle, Direction, Hit, Recall,
    RerankedRecallOptions,
};
use anamnesis::query::assembly::estimate_tokens;
use anamnesis::query::{AccessedSite, Fragment};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use anamnesis::{Error, Memory};

use super::{MemoryRegistry, PackagedRecall, RecallGateTrace, RecallOutcome};

pub(crate) struct RecallFilters<'a> {
    pub(crate) gate: Option<f64>,
    pub(crate) cosine_gate: Option<f64>,
    pub(crate) scope: Option<&'a str>,
    pub(crate) tag: Option<&'a str>,
    pub(crate) knowledge_only: bool,
}

/// Test reranker that preserves the cognitive candidate order while exercising
/// the exact production orchestration path.
#[cfg(test)]
pub(crate) struct CognitiveReranker;

#[cfg(test)]
impl RerankingProvider for CognitiveReranker {
    fn rerank(&self, _query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error> {
        Ok(documents
            .iter()
            .enumerate()
            .map(|(index, _)| RerankScore::new(index, (documents.len() - index) as f64))
            .collect())
    }

    fn model_name(&self) -> &str {
        "test-cognitive-order"
    }
}

#[cfg(test)]
pub(crate) struct InflatedReranker;

#[cfg(test)]
impl RerankingProvider for InflatedReranker {
    fn rerank(&self, _query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error> {
        Ok(documents
            .iter()
            .enumerate()
            .map(|(index, _)| RerankScore::new(index, 1_000_000.0 - index as f64))
            .collect())
    }

    fn model_name(&self) -> &str {
        "test-inflated-reranker"
    }
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
    /// Ticks the engine exactly once. Calling `tick` before and after the same
    /// search is not equivalent to a single update because `tick` is
    /// not a no-op to call twice per recall — idle-edge leakage and node decay
    /// both key off elapsed time since the last tick, so a second tick a few
    /// milliseconds later would double decay/leak pressure on every read. One
    /// tick per recall preserves
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
        // `seed_limit` tracks `limit` inside `search`; oversampling would change
        // the RWR seed distribution. Search the requested width, then collapse
        // Episodic/Semantic representations to canonical hits. A heavily
        // duplicated result may therefore contain fewer than `limit` hits.
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
        let reranker = self.reranker()?;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_recall_packaged_gated(
            &mut mem,
            query,
            limit,
            reinforce,
            gate,
            cosine_gate,
            reranker.as_ref(),
        )
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
    reranker: &dyn RerankingProvider,
) -> Result<RecallOutcome, Error> {
    let output = mem.search_reranked(query, reranker, RerankedRecallOptions::new(limit))?;
    finish_recall(
        mem,
        query,
        output.recall,
        &output.cognitive_scores,
        reinforce,
        gate,
        cosine_gate,
    )
}

/// Like [`mem_recall_packaged_gated`], with a scope/tag filter applied to the
/// [`ContextPackage`](anamnesis::query::ContextPackage) before rendering.
pub(crate) fn mem_recall_packaged_gated_filtered(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    limit: usize,
    reinforce: bool,
    filters: RecallFilters<'_>,
    reranker: &dyn RerankingProvider,
) -> Result<RecallOutcome, Error> {
    if filters.scope.is_none() && filters.tag.is_none() && !filters.knowledge_only {
        return mem_recall_packaged_gated(
            mem,
            query,
            limit,
            reinforce,
            filters.gate,
            filters.cosine_gate,
            reranker,
        );
    }

    let scope_path = filters.scope.map(ScopePath::new).transpose()?;
    let options = match scope_path {
        Some(scope) => RerankedRecallOptions::new(limit).with_scope(scope),
        None => RerankedRecallOptions::new(limit),
    };
    let output = mem.search_reranked(query, reranker, options)?;
    let mut recall = output.recall;

    filter_context_package(mem, &mut recall.package, filters.scope, filters.tag);
    recall
        .hits
        .retain(|h| node_matches_scope_tag(mem, h.node_id, filters.scope, filters.tag));
    if filters.knowledge_only {
        apply_knowledge_only(mem, &mut recall.package, &mut recall.hits, limit)?;
    }
    synchronize_filtered_package(&mut recall.package);

    finish_recall(
        mem,
        query,
        recall,
        &output.cognitive_scores,
        reinforce,
        filters.gate,
        filters.cosine_gate,
    )
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
    query: &str,
    recall: Recall,
    cognitive_scores: &[CognitiveRecallScore],
    reinforce: bool,
    gate: Option<f64>,
    cosine_gate: Option<f64>,
) -> Result<RecallOutcome, Error> {
    let hits = super::dedup_hits(recall.hits.clone());
    let cognitive_top = hits.first().map(|hit| {
        let mut cognitive_hit = hit.clone();
        if let Some(score) = cognitive_scores
            .iter()
            .find(|score| score.node_id == hit.node_id)
        {
            cognitive_hit.score = score.score;
            cognitive_hit.cosine = score.cosine;
        }
        cognitive_hit
    });
    let mut trace = gate_trace(cognitive_top.as_ref(), gate, cosine_gate);

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
    let context =
        mem.render_context_for_with(query, &recall, configured_context_render_options())?;
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

fn configured_context_render_options() -> ContextRenderOptions {
    let style = std::env::var("ANAMNESIS_CONTEXT_STYLE")
        .ok()
        .as_deref()
        .map_or(ContextRenderStyle::Detailed, parse_context_render_style);
    ContextRenderOptions::with_style(style)
}

fn parse_context_render_style(value: &str) -> ContextRenderStyle {
    if value.trim().eq_ignore_ascii_case("evidence") {
        ContextRenderStyle::Evidence
    } else {
        ContextRenderStyle::Detailed
    }
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
    source_limit: usize,
) -> Result<(), Error> {
    // `knowledge_only` suppresses transcript chatter, not the authoritative raw
    // evidence that makes a selected knowledge fragment usable. Keep only two
    // narrowly grounded Episodic classes:
    //
    // 1. direct ExtractedFrom sources of a surviving non-capture knowledge node;
    // 2. final selected raw hits cited by a live reviewed atomic fact.
    //
    // The second case is needed because atomic facts are sidecar records rather
    // than graph nodes: production atomic/chain routing returns their raw source
    // directly, so there may be no selected Semantic endpoint to inspect here.
    let selected_hit_ids: HashSet<NodeId> = hits.iter().map(|hit| hit.node_id).collect();
    let knowledge_ids: Vec<NodeId> = package
        .knowledge
        .iter()
        .map(|fragment| fragment.node_id)
        .filter(|node_id| !is_capture_node(mem, *node_id))
        .collect();
    let mut selected_knowledge_sources = extracted_sources_for(mem, &knowledge_ids)?;
    let mut ordered_knowledge_sources: Vec<_> =
        selected_knowledge_sources.iter().copied().collect();
    ordered_knowledge_sources.sort_unstable();
    ordered_knowledge_sources.truncate(source_limit);
    selected_knowledge_sources.retain(|source_id| ordered_knowledge_sources.contains(source_id));

    // Deep source canonicalization can leave the reviewed Semantic node as the
    // sole packaged representation. `knowledge_only` still owes the consumer
    // the immutable raw evidence behind that knowledge, so materialize the
    // validated direct ExtractedFrom source into the same package and commit
    // trace. This is bounded by the caller's requested result width.
    let knowledge_ids_set: HashSet<_> = knowledge_ids.iter().copied().collect();
    let source_relevance = package
        .knowledge
        .iter()
        .filter(|fragment| knowledge_ids_set.contains(&fragment.node_id))
        .map(|fragment| fragment.relevance)
        .fold(0.0_f64, f64::max);
    let source_work = package
        .commit_trace
        .accessed
        .iter()
        .filter(|site| knowledge_ids_set.contains(&site.node_id))
        .map(|site| site.readout_work)
        .fold(source_relevance.clamp(0.0, 1.0), f64::max);
    let source_rank = hits
        .iter()
        .find(|hit| knowledge_ids_set.contains(&hit.node_id))
        .map(|hit| (hit.score, hit.cosine));
    for source_id in &ordered_knowledge_sources {
        let source = mem.engine().graph().get_node(*source_id)?;
        if !package
            .memories
            .iter()
            .any(|fragment| fragment.node_id == *source_id)
        {
            package.memories.push(Fragment {
                node_id: *source_id,
                name: source.name.clone(),
                summary: source.summary.clone(),
                content: Some(source.content.clone()),
                node_type: source.node_type.clone(),
                relevance: source_relevance,
                origin: source.origin.clone(),
            });
        }
        if !hits.iter().any(|hit| hit.node_id == *source_id) {
            let (score, cosine) = source_rank.unwrap_or((source_relevance, 0.0));
            hits.push(Hit {
                node_id: *source_id,
                text: source.content.clone(),
                score,
                cosine,
                at: source.created_at,
                speaker: source
                    .entity_tags
                    .iter()
                    .find_map(|tag| tag.strip_prefix("speaker-").map(str::to_owned)),
                session: Some(source.origin.session_id.clone()),
            });
        }
        if !package
            .commit_trace
            .accessed
            .iter()
            .any(|site| site.node_id == *source_id)
        {
            package.commit_trace.accessed.push(AccessedSite {
                node_id: *source_id,
                readout_work: source_work,
            });
        }
    }
    let selected_extracted_sources = selected_extracted_sources(mem, &selected_hit_ids)?;
    let atomic_sources = selected_atomic_sources(mem, &selected_hit_ids)?;
    let mut grounded_sources = selected_knowledge_sources.clone();
    grounded_sources.extend(selected_extracted_sources.iter().copied());
    grounded_sources.extend(atomic_sources.iter().copied());

    // Deep readout canonicalizes Semantic/Episodic representations by raw
    // source. Keep every selected, validated direct source addressable whether
    // the Semantic or raw representation originally won final selection.
    let addressable_extracted_sources: HashSet<NodeId> = selected_extracted_sources
        .union(&selected_knowledge_sources)
        .copied()
        .collect();

    package
        .memories
        .retain(|fragment| grounded_sources.contains(&fragment.node_id));
    package.tensions.clear();
    package
        .identity
        .retain(|f| !is_capture_node(mem, f.node_id));
    package
        .knowledge
        .retain(|f| !is_capture_node(mem, f.node_id));
    // Direct provenance is rendered from `package.memories`; retaining the raw
    // hit also lets consumers cite the canonical evidence node directly.
    hits.retain(|hit| {
        !is_capture_node(mem, hit.node_id)
            || atomic_sources.contains(&hit.node_id)
            || addressable_extracted_sources.contains(&hit.node_id)
    });
    Ok(())
}

fn extracted_sources_for(
    mem: &Memory<SqliteStorage>,
    knowledge_ids: &[NodeId],
) -> Result<HashSet<NodeId>, Error> {
    let mut sources = HashSet::new();
    let now = Timestamp::now();
    for &knowledge_id in knowledge_ids {
        for neighbor in mem.neighbors(knowledge_id)? {
            if neighbor.direction != Direction::Outgoing
                || neighbor.edge_type != EdgeType::ExtractedFrom
            {
                continue;
            }
            let source = mem.engine().graph().get_node(neighbor.node)?;
            if source.node_type == KnowledgeType::Episodic
                && source.created_at <= now
                && !metadata_is_retracted(&source.metadata)
                && valid_at(source.valid_from, source.valid_until, now)
            {
                sources.insert(source.id);
            }
        }
    }
    Ok(sources)
}

fn selected_extracted_sources(
    mem: &Memory<SqliteStorage>,
    selected_hit_ids: &HashSet<NodeId>,
) -> Result<HashSet<NodeId>, Error> {
    let now = Timestamp::now();
    let mut sources = HashSet::new();
    for &source_id in selected_hit_ids {
        let source = mem.engine().graph().get_node(source_id)?;
        if source.node_type != KnowledgeType::Episodic
            || source.created_at > now
            || metadata_is_retracted(&source.metadata)
            || !valid_at(source.valid_from, source.valid_until, now)
        {
            continue;
        }
        for neighbor in mem.neighbors(source_id)? {
            if neighbor.direction != Direction::Incoming
                || neighbor.edge_type != EdgeType::ExtractedFrom
            {
                continue;
            }
            let derived = mem.engine().graph().get_node(neighbor.node)?;
            if derived.node_type == KnowledgeType::Semantic
                && derived.created_at <= now
                && !is_capture_node(mem, derived.id)
                && !metadata_is_retracted(&derived.metadata)
                && valid_at(derived.valid_from, derived.valid_until, now)
            {
                sources.insert(source_id);
                break;
            }
        }
    }
    Ok(sources)
}

fn selected_atomic_sources(
    mem: &Memory<SqliteStorage>,
    selected_hit_ids: &HashSet<NodeId>,
) -> Result<HashSet<NodeId>, Error> {
    let storage = mem.engine().graph().storage();
    let now = Timestamp::now();
    let mut sources = HashSet::new();

    // Every admitted fact is stamped with the exact incarnation fingerprint of
    // each source it cites. Resolve only the bounded final hit set through the
    // storage metadata index; this covers multi-source facts without scanning
    // the full sidecar on every hook invocation. Legacy facts without the stamp
    // remain fail-closed, matching `atomic_fact_source_is_current`.
    for &source_id in selected_hit_ids {
        let source = storage.get_node(source_id)?;
        if source.node_type != KnowledgeType::Episodic
            || source.created_at > now
            || metadata_is_retracted(&source.metadata)
            || !valid_at(source.valid_from, source.valid_until, now)
        {
            continue;
        }
        let source_key = format!("anamnesis:source-incarnation:{}", source_id.0);
        let source_incarnation = storage.atomic_source_incarnation(source)?;
        for fact_id in storage.atomic_fact_ids_by_metadata(&source_key, &source_incarnation)? {
            let fact = storage.get_atomic_fact(fact_id)?;
            if !fact.source_node_ids.contains(&source_id)
                || metadata_is_retracted(&fact.metadata)
                || fact.observed_at > now
                || !valid_at(fact.valid_from, fact.valid_until, now)
                || source.origin.session_id != fact.source_session_id
                || source.origin.scope != fact.scope
                || !storage.atomic_fact_source_is_current(fact, source)?
            {
                continue;
            }
            sources.insert(source_id);
            break;
        }
    }
    Ok(sources)
}

fn metadata_is_retracted(metadata: &HashMap<String, String>) -> bool {
    metadata
        .get("retracted")
        .is_some_and(|value| value == "true")
}

/// Reconcile the transient accounting/commit projection after any MCP-level
/// scope, tag, or knowledge-only filtering. Only fragments actually delivered
/// in the rendered package may be reinforced by `used()`.
fn synchronize_filtered_package(package: &mut anamnesis::query::ContextPackage) {
    let visible: HashSet<NodeId> = package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .map(|fragment| fragment.node_id)
        .collect();

    package
        .commit_trace
        .accessed
        .retain(|site| visible.contains(&site.node_id));
    package
        .commit_trace
        .co_readout
        .retain(|pair| visible.contains(&pair.node_a) && visible.contains(&pair.node_b));
    package
        .commit_trace
        .path_used
        .retain(|path| visible.contains(&path.source) && visible.contains(&path.target));
    let presented_tensions: HashSet<(NodeId, NodeId)> = package
        .tensions
        .iter()
        .map(|tension| ordered_pair(tension.node_a, tension.node_b))
        .collect();
    package.commit_trace.tensions_activated.retain(|tension| {
        visible.contains(&tension.node_a)
            && visible.contains(&tension.node_b)
            && presented_tensions.contains(&ordered_pair(tension.node_a, tension.node_b))
    });
    package
        .committed_ids
        .retain(|node_id| visible.contains(node_id));

    let identity_ids: HashSet<NodeId> = package
        .identity
        .iter()
        .map(|fragment| fragment.node_id)
        .collect();
    package.agent_tension = package
        .tensions
        .iter()
        .filter(|tension| {
            identity_ids.contains(&tension.node_a) || identity_ids.contains(&tension.node_b)
        })
        .map(|tension| tension.stress.max(0.0))
        .sum::<f64>()
        .clamp(0.0, 1.0);

    const CHARS_PER_TOKEN: usize = 4;
    let fragment_tokens = |fragment: &anamnesis::query::Fragment| {
        estimate_tokens(&fragment.name, CHARS_PER_TOKEN)
            + fragment
                .summary
                .as_deref()
                .map_or(0, |summary| estimate_tokens(summary, CHARS_PER_TOKEN))
            + fragment
                .content
                .as_deref()
                .map_or(0, |content| estimate_tokens(content, CHARS_PER_TOKEN))
    };
    package.token_usage.identity_used = package.identity.iter().map(fragment_tokens).sum();
    package.token_usage.knowledge_used = package.knowledge.iter().map(fragment_tokens).sum();
    package.token_usage.memories_used = package.memories.iter().map(fragment_tokens).sum();
    package.token_usage.used = package.token_usage.identity_used
        + package.token_usage.knowledge_used
        + package.token_usage.memories_used;
}

fn ordered_pair(left: NodeId, right: NodeId) -> (NodeId, NodeId) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn is_capture_node(mem: &Memory<SqliteStorage>, node_id: NodeId) -> bool {
    mem.engine()
        .graph()
        .get_node(node_id)
        .map(|n| n.metadata.get(META_CAPTURE).is_some_and(|v| v == "true"))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::{ContextRenderStyle, parse_context_render_style, synchronize_filtered_package};
    use anamnesis::graph::{EdgeId, EdgeType, KnowledgeType, NodeId, Origin, PeerId};
    use anamnesis::query::{
        AccessedSite, ActivatedTension, CoReadoutPair, ContextPackage, Fragment, PathUsedEdge,
    };

    #[test]
    fn evidence_context_style_is_explicit_and_case_insensitive() {
        assert_eq!(
            parse_context_render_style(" evidence "),
            ContextRenderStyle::Evidence
        );
        assert_eq!(
            parse_context_render_style("EVIDENCE"),
            ContextRenderStyle::Evidence
        );
        assert_eq!(
            parse_context_render_style("detailed"),
            ContextRenderStyle::Detailed
        );
        assert_eq!(
            parse_context_render_style("unknown"),
            ContextRenderStyle::Detailed
        );
    }

    #[test]
    fn filtered_package_accounting_contains_only_visible_fragments() {
        let visible = NodeId(1);
        let hidden = NodeId(2);
        let mut package = ContextPackage::empty();
        package.token_usage.total = 100;
        package.token_usage.used = 99;
        package.token_usage.knowledge_used = 99;
        package.knowledge.push(Fragment {
            node_id: visible,
            name: "visible".to_owned(),
            summary: Some("summary".to_owned()),
            content: Some("payload".to_owned()),
            node_type: KnowledgeType::Semantic,
            relevance: 1.0,
            origin: Origin::test_default(PeerId(0)),
        });
        package.commit_trace.accessed = vec![
            AccessedSite {
                node_id: visible,
                readout_work: 0.8,
            },
            AccessedSite {
                node_id: hidden,
                readout_work: 0.7,
            },
        ];
        package.commit_trace.co_readout.push(CoReadoutPair {
            node_a: visible,
            node_b: hidden,
            activation_a: 0.8,
            activation_b: 0.7,
        });
        package.commit_trace.path_used.push(PathUsedEdge {
            edge_id: EdgeId(1),
            source: visible,
            target: hidden,
            edge_type: EdgeType::Semantic,
            flux: 0.5,
        });
        package
            .commit_trace
            .tensions_activated
            .push(ActivatedTension {
                node_a: visible,
                node_b: visible,
                stress: 0.4,
            });
        package.committed_ids = vec![visible, hidden];

        synchronize_filtered_package(&mut package);

        assert_eq!(package.commit_trace.accessed.len(), 1);
        assert_eq!(package.commit_trace.accessed[0].node_id, visible);
        assert!(package.commit_trace.co_readout.is_empty());
        assert!(package.commit_trace.path_used.is_empty());
        assert!(package.commit_trace.tensions_activated.is_empty());
        assert_eq!(package.committed_ids, [visible]);
        assert_eq!(package.token_usage.total, 100);
        assert_eq!(package.token_usage.identity_used, 0);
        assert_eq!(package.token_usage.knowledge_used, 6);
        assert_eq!(package.token_usage.memories_used, 0);
        assert_eq!(package.token_usage.used, 6);
        assert_eq!(package.agent_tension, 0.0);
    }
}
