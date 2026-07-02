//! Result assembly stage — package the settled activation response into a `SearchResult`.
//!
//! Read-only. Scores each activated site with the authoritative seven-term
//! additive log-odds readout score ([readout-scoring.md]), applies the
//! deterministic tie-breaker chain, and packages the result. It never mutates
//! reservoirs or projections.
//!
//! [readout-scoring.md]: ../../docs/04-cognitive-dynamics/readout-scoring.md

use std::collections::HashSet;

use crate::api::Engine;
use crate::error::Error;
use crate::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use crate::mechanics::attraction::cosine_similarity;
use crate::query::assembly::{ScoredNode, assemble_context_package, estimate_tokens};
use crate::query::rwr::ActivationResponse;
use crate::query::scoring::{ReadoutInputs, TieBreakKey, rank, readout_score, scope_weight};
use crate::query::types::SearchPlan;
use crate::query::{
    ContextPackage, Fragment, PackagingMode, QueryConfig, SearchInput, SearchResult, SearchTrace,
};
use crate::storage::StorageAdapter;

/// Trace-size memory bound for the pre-packaging readout candidate list. Not a
/// behavioral prior (ADR-0010) — it only caps the diagnostic
/// `SearchTrace::readout` vector and never affects packaging or result limits.
const READOUT_TRACE_CAP: usize = 200;

pub(crate) struct SearchAssemblyRequest<'a> {
    pub(crate) response: &'a ActivationResponse,
    pub(crate) seed_ids: &'a [NodeId],
    pub(crate) config: &'a QueryConfig,
    pub(crate) input: &'a SearchInput,
    pub(crate) plan: &'a SearchPlan,
    pub(crate) strategies_used: Vec<String>,
    pub(crate) field: &'a crate::query::field::QueryField,
}

pub(crate) fn assemble_search_result<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    request: SearchAssemblyRequest<'_>,
) -> Result<SearchResult, Error> {
    let trace = SearchTrace {
        strategies_used: request.strategies_used.clone(),
        seed_count: request.seed_ids.len(),
        iterations: request.response.iterations,
        residual: request.response.residual,
        truncated: request.response.truncated,
        excluded_edge_count: request.response.excluded_edges.len(),
        path_current_count: request.response.path_current.len(),
        packaging_mode: None,
        energy: crate::mechanics::energy::EnergyTerms::default(),
        readout: Vec::new(),
    };

    if request.response.activation.is_empty() {
        return Ok(SearchResult {
            package: ContextPackage::empty(),
            trace,
        });
    }

    let (mut package, readout_candidates) = assemble_graph_recall_package(
        engine,
        request.response,
        request.config,
        request.input,
        request.field,
    );
    let packaging_mode =
        crate::query::decide_packaging(&package.tensions, request.plan, &request.input.text);

    apply_packaging_mode(engine, packaging_mode.clone(), &mut package);

    if request.input.now.0 > 0 {
        apply_validity_filter(engine, &mut package, request.input.now);
    }
    apply_result_limit(
        &mut package,
        request.input.limit,
        request.config.chars_per_token,
    );

    // Capture the read-only commit trace from the FINAL package (after packaging mode
    // and validity filtering), so a later `commit` only integrates work for sites that
    // actually survived into the result (ADR-0004 / interactions.md). Read-only.
    package.commit_trace =
        crate::api::build_commit_trace(engine.graph.storage(), request.response, &package);

    // Compute the query-local readout energy `E(S | Q)` over the FINAL packaged active
    // subsystem (energy.md / ADR-0007). Interpretive only — explains why the bundle was
    // selected; the RWR stationary vector is the true fixed point. Never stored.
    let energy = crate::api::build_readout_energy(
        engine.graph.storage(),
        request.response,
        &package,
        request.config.query_embedding.as_ref(),
    );

    let mut trace = trace;
    trace.packaging_mode = Some(packaging_mode);
    trace.energy = energy;
    trace.readout = readout_candidates;

    Ok(SearchResult { package, trace })
}

fn assemble_graph_recall_package<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    response: &ActivationResponse,
    config: &QueryConfig,
    input: &crate::query::SearchInput,
    field: &crate::query::field::QueryField,
) -> (ContextPackage, Vec<crate::query::ReadoutCandidate>) {
    let storage = engine.graph.storage();
    let now = config.now.unwrap_or_else(Timestamp::now);
    let activations = &response.activation;

    // Surface Contradicts edges between *active* sites as frustration tensions
    // (frustration.md / ADR-0006). Stress is the multiplicative gate product
    // `sigma_ij = contradiction_weight * min(a_i, a_j) * scope_overlap *
    // temporal_overlap`; if any gate is zero the pair is not surfaced. The conflict
    // is surfaced, never suppressed — neither endpoint's activation is reduced.
    let (contradiction_pairs, node_stress) = crate::query::assembly::collect_contradiction_pairs(
        storage,
        activations,
        config.min_activation,
        now,
    );

    let mut scored: Vec<(f64, TieBreakKey, crate::query::ReadoutCandidate, ScoredNode)> =
        Vec::new();
    for (&node_id, &activation) in activations {
        if activation < config.min_activation {
            continue;
        }

        let node = match storage.get_node(node_id) {
            Ok(node) => node,
            Err(_) => continue,
        };

        if let Some(ref peer_filter) = input.peer_filter
            && !peer_filter.contains(&node.origin.peer_id)
        {
            continue;
        }
        if node.metadata.get("retracted").is_some_and(|v| v == "true") {
            continue;
        }

        let salience = storage.get_salience(node_id).unwrap_or(0.0);
        let retained_action = storage.get_retained_action(node_id).unwrap_or(0.0);
        let impedance = response
            .impedance
            .get(&node_id)
            .copied()
            .unwrap_or_default();

        let base_scope_weight = scope_weight(&config.scope, &node.origin.scope);
        // Trust reservoir removed with the peer subsystem; term is neutral pending
        // a real trust source.
        let trust_weight = 1.0;

        // phi_i: query-ALIGNMENT potential bias (potential-landscape.md).
        // Seeded sites keep their collected text/entity signals; the embedding
        // term is refreshed with the query cosine so graph-reached sites
        // (absent from the field) still get semantic-alignment credit.
        //
        // The prior `A_i` is deliberately EXCLUDED here. readout-scoring.md
        // lists `A_i` as "read input and tie-breaker", not a scored term: the
        // reservoir already reaches the score through `logit(s_i)`
        // (`s_i = logistic(A_i)`), so folding it into phi double-counts the
        // prior and lets encoding-time magnitudes (≈3–12 log-odds) drown the
        // bounded alignment signals. `beta_prior · A_i` remains in the SEED
        // field where potential-landscape.md mandates it (restart prior odds).
        // Scope also stays out of phi: it has its own readout term.
        let cosine = match (&config.query_embedding, &node.embedding) {
            (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
            _ => 0.0,
        };
        let mut signals = field.get(node_id).copied().unwrap_or_default();
        signals.retained_action = 0.0;
        signals.embedding_score = signals.embedding_score.max(cosine);
        let phi = crate::query::field::potential_bias(&signals);

        // Frustration stress attached to this site (sum of sigma over its active
        // contradiction partners) feeds the readout `-w_stress` term: contradicting
        // bundles are pushed to separate, never deleted (ADR-0006).
        let stress = node_stress.get(&node_id).copied().unwrap_or(0.0);

        let inputs = ReadoutInputs {
            activation,
            phi,
            salience,
            impedance,
            scope_weight: base_scope_weight,
            trust_weight,
            stress,
        };
        let score = readout_score(&inputs);

        let key = TieBreakKey {
            node_id,
            retained_action,
            impedance,
            accessed_at: node.accessed_at,
        };

        let readout_candidate = crate::query::ReadoutCandidate {
            node_id,
            score,
            activation: inputs.activation,
            phi: inputs.phi,
            salience: inputs.salience,
            impedance: inputs.impedance,
            scope_weight: inputs.scope_weight,
            trust_weight: inputs.trust_weight,
            stress: inputs.stress,
        };

        scored.push((
            score,
            key,
            readout_candidate,
            ScoredNode {
                node_id,
                name: node.name.clone(),
                summary: node.summary.clone(),
                content: node.content.clone(),
                node_type: node.node_type.clone(),
                relevance: score,
                origin: node.origin.clone(),
            },
        ));
    }

    // Rank by readout score with the deterministic tie-breaker chain.
    scored.sort_by(|(sa, ka, _, _), (sb, kb, _, _)| rank(*sa, ka, *sb, kb));

    // Capture the ranked pre-packaging readout candidate list with per-term score
    // components (readout-scoring.md "Trace"). Capped at READOUT_TRACE_CAP as a
    // numerical guard (ADR-0010); all packaged sites are always within this cap.
    let readout_candidates: Vec<crate::query::ReadoutCandidate> = scored
        .iter()
        .take(READOUT_TRACE_CAP)
        .map(|(_, _, candidate, _)| candidate.clone())
        .collect();

    let scored_nodes: Vec<ScoredNode> = scored.into_iter().map(|(_, _, _, n)| n).collect();

    let identity_activations: Vec<(NodeId, KnowledgeType, f64)> = activations
        .iter()
        .filter_map(|(&node_id, &activation)| {
            let node = storage.get_node(node_id).ok()?;
            if !is_identity_type(&node.node_type) {
                return None;
            }
            let agent_id = config.agent_id.as_ref()?;
            (node.origin.peer_id.0.to_string() == *agent_id)
                .then(|| (node_id, node.node_type.clone(), activation))
        })
        .collect();

    let package = assemble_context_package(
        scored_nodes,
        &identity_activations,
        &contradiction_pairs,
        config.token_budget,
        config.chars_per_token,
    );
    (package, readout_candidates)
}

fn apply_packaging_mode<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    packaging_mode: PackagingMode,
    package: &mut ContextPackage,
) {
    match packaging_mode {
        PackagingMode::Balanced => {}
        PackagingMode::KnowledgeOnly => {
            package.token_usage.used = package
                .token_usage
                .used
                .saturating_sub(package.token_usage.memories_used);
            package.token_usage.memories_used = 0;
            package.memories.clear();
        }
        PackagingMode::KnowledgeWithProvenance => {
            include_source_memories(engine, &package.knowledge, &mut package.memories);
        }
        PackagingMode::PersonaWeighted => {
            package.identity.sort_by(|a, b| {
                b.relevance
                    .partial_cmp(&a.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.node_id.cmp(&b.node_id))
            });
        }
        PackagingMode::Timeline => {
            sort_fragments_by_created_at(engine, &mut package.identity);
            sort_fragments_by_created_at(engine, &mut package.knowledge);
            sort_fragments_by_created_at(engine, &mut package.memories);
        }
    }
}

fn include_source_memories<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    knowledge: &[Fragment],
    memories: &mut Vec<Fragment>,
) {
    let mut existing: HashSet<NodeId> = memories.iter().map(|fragment| fragment.node_id).collect();

    for fragment in knowledge {
        for source_fragment in source_memory_fragments(engine, fragment) {
            if existing.insert(source_fragment.node_id) {
                memories.push(source_fragment);
            }
        }
    }
}

fn source_memory_fragments<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    fragment: &Fragment,
) -> Vec<Fragment> {
    let storage = engine.graph.storage();
    let mut seen = HashSet::new();
    let mut fragments = Vec::new();

    for &edge_id in storage.edges_to(fragment.node_id) {
        let Some((source_id, weight)) = storage.get_edge(edge_id).ok().and_then(|edge| {
            (edge.edge_type == EdgeType::ExtractedFrom).then_some((edge.source, edge.weight))
        }) else {
            continue;
        };
        push_source_memory_fragment(engine, fragment, source_id, weight, &mut seen, &mut fragments);
    }

    for &edge_id in storage.edges_from(fragment.node_id) {
        let Some((source_id, weight)) = storage.get_edge(edge_id).ok().and_then(|edge| {
            (edge.edge_type == EdgeType::ExtractedFrom).then_some((edge.target, edge.weight))
        }) else {
            continue;
        };
        push_source_memory_fragment(engine, fragment, source_id, weight, &mut seen, &mut fragments);
    }

    fragments
}

fn push_source_memory_fragment<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    parent: &Fragment,
    source_id: NodeId,
    edge_weight: f64,
    seen: &mut HashSet<NodeId>,
    fragments: &mut Vec<Fragment>,
) {
    if !seen.insert(source_id) {
        return;
    }

    let storage = engine.graph.storage();
    let Ok(node) = storage.get_node(source_id) else {
        return;
    };
    if !matches!(node.node_type, KnowledgeType::Episodic) {
        return;
    }

    fragments.push(Fragment {
        node_id: source_id,
        name: node.name.clone(),
        summary: node.summary.clone(),
        content: Some(node.content.clone()),
        node_type: node.node_type.clone(),
        relevance: (parent.relevance * edge_weight).clamp(0.0, 1.0),
        origin: node.origin.clone(),
    });
}

fn sort_fragments_by_created_at<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    fragments: &mut [Fragment],
) {
    fragments.sort_by(|a, b| {
        let a_created_at = engine
            .graph
            .get_node(a.node_id)
            .map(|node| node.created_at)
            .unwrap_or(Timestamp(u64::MAX));
        let b_created_at = engine
            .graph
            .get_node(b.node_id)
            .map(|node| node.created_at)
            .unwrap_or(Timestamp(u64::MAX));

        a_created_at
            .cmp(&b_created_at)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
}

fn apply_validity_filter<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    package: &mut ContextPackage,
    now: Timestamp,
) {
    package
        .knowledge
        .retain(|fragment| node_is_valid_at(engine, fragment.node_id, now));
    package
        .memories
        .retain(|fragment| node_is_valid_at(engine, fragment.node_id, now));
    package
        .identity
        .retain(|fragment| node_is_valid_at(engine, fragment.node_id, now));
    package.tensions.retain(|tension| {
        node_is_valid_at(engine, tension.node_a, now)
            && node_is_valid_at(engine, tension.node_b, now)
    });
}

fn apply_result_limit(package: &mut ContextPackage, limit: usize, chars_per_token: usize) {
    if package.total_fragments() <= limit {
        return;
    }

    let mut ranked: Vec<(NodeId, f64)> = package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .map(|fragment| (fragment.node_id, fragment.relevance))
        .collect();
    ranked.sort_by(|(left_id, left_score), (right_id, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_id.cmp(right_id))
    });
    let allowed: HashSet<NodeId> = ranked
        .into_iter()
        .take(limit)
        .map(|(node_id, _)| node_id)
        .collect();

    package
        .identity
        .retain(|fragment| allowed.contains(&fragment.node_id));
    package
        .knowledge
        .retain(|fragment| allowed.contains(&fragment.node_id));
    package
        .memories
        .retain(|fragment| allowed.contains(&fragment.node_id));
    package
        .tensions
        .retain(|tension| allowed.contains(&tension.node_a) && allowed.contains(&tension.node_b));
    recalculate_token_usage(package, chars_per_token);
}

fn recalculate_token_usage(package: &mut ContextPackage, chars_per_token: usize) {
    let total = package.token_usage.total;
    package.token_usage = crate::query::TokenBudget::new(total);
    package.token_usage.identity_used = package
        .identity
        .iter()
        .map(|fragment| fragment_tokens(fragment, chars_per_token))
        .sum();
    package.token_usage.knowledge_used = package
        .knowledge
        .iter()
        .map(|fragment| fragment_tokens(fragment, chars_per_token))
        .sum();
    package.token_usage.memories_used = package
        .memories
        .iter()
        .map(|fragment| fragment_tokens(fragment, chars_per_token))
        .sum();
    package.token_usage.used = package.token_usage.identity_used
        + package.token_usage.knowledge_used
        + package.token_usage.memories_used;
}

fn fragment_tokens(fragment: &Fragment, chars_per_token: usize) -> usize {
    let mut tokens = estimate_tokens(&fragment.name, chars_per_token);
    if let Some(summary) = &fragment.summary {
        tokens += estimate_tokens(summary, chars_per_token);
    }
    if let Some(content) = &fragment.content {
        tokens += estimate_tokens(content, chars_per_token);
    }
    tokens
}

fn node_is_valid_at<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    node_id: NodeId,
    as_of: Timestamp,
) -> bool {
    engine
        .graph
        .get_node(node_id)
        .is_ok_and(|node| crate::graph::valid_at(node.valid_from, node.valid_until, as_of))
}

fn is_identity_type(node_type: &KnowledgeType) -> bool {
    matches!(node_type, KnowledgeType::Identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Engine, EngineConfig};
    use crate::graph::{KnowledgeType, ScopePath};
    use crate::query::types::{CommitTrace, ContextPackage};
    use crate::query::{Fragment, PackagingMode, TokenBudget};

    fn make_memory_fragment() -> Fragment {
        use crate::graph::node::Origin;
        Fragment {
            node_id: crate::graph::NodeId(1),
            name: "episode".into(),
            summary: Some("summary".into()),
            content: Some("content".into()),
            node_type: KnowledgeType::Episodic,
            relevance: 0.8,
            origin: Origin {
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::graph::types::SourceKind::AgentObservation,
                session_id: "s1".into(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
        }
    }

    /// `KnowledgeOnly` must clear the memories bucket and adjust token accounting.
    #[test]
    fn knowledge_only_clears_memories_and_adjusts_tokens() {
        let engine: Engine<_> = Engine::with_config(EngineConfig::default());
        let fragment = make_memory_fragment();
        // Estimate tokens contributed by this fragment.
        let fragment_tokens = {
            let chars_per_token = 4usize;
            let mut t = crate::query::assembly::estimate_tokens(&fragment.name, chars_per_token);
            if let Some(ref s) = fragment.summary {
                t += crate::query::assembly::estimate_tokens(s, chars_per_token);
            }
            if let Some(ref c) = fragment.content {
                t += crate::query::assembly::estimate_tokens(c, chars_per_token);
            }
            t
        };

        let mut package = ContextPackage {
            identity: vec![],
            knowledge: vec![],
            memories: vec![fragment],
            tensions: vec![],
            token_usage: TokenBudget {
                total: 4000,
                used: fragment_tokens + 100, // 100 from knowledge
                identity_used: 0,
                knowledge_used: 100,
                memories_used: fragment_tokens,
            },
            agent_tension: 0.0,
            commit_trace: CommitTrace::default(),
            committed_ids: vec![],
        };

        apply_packaging_mode(&engine, PackagingMode::KnowledgeOnly, &mut package);

        assert!(
            package.memories.is_empty(),
            "KnowledgeOnly must clear the memories bucket"
        );
        assert_eq!(
            package.token_usage.memories_used, 0,
            "KnowledgeOnly must zero memories_used"
        );
        assert_eq!(
            package.token_usage.used, 100,
            "KnowledgeOnly must subtract memories tokens from used"
        );
    }
}
