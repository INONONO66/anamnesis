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
use crate::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use crate::mechanics::attraction::cosine_similarity;
use crate::query::assembly::{ScoredNode, assemble_context_package, determine_scope};
use crate::query::rwr::ActivationResponse;
use crate::query::scoring::{ReadoutInputs, TieBreakKey, rank, readout_score, scope_weight};
use crate::query::types::SearchPlan;
use crate::query::{
    ContextPackage, Fragment, PackagingMode, QueryConfig, SearchInput, SearchResult, SearchTrace,
};
use crate::storage::StorageAdapter;

pub(crate) struct SearchAssemblyRequest<'a> {
    pub(crate) response: &'a ActivationResponse,
    pub(crate) seed_ids: &'a [NodeId],
    pub(crate) config: &'a QueryConfig,
    pub(crate) input: &'a SearchInput,
    pub(crate) plan: &'a SearchPlan,
    pub(crate) strategies_used: Vec<String>,
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
    };

    if request.response.activation.is_empty() {
        return Ok(SearchResult {
            package: ContextPackage::empty(),
            trace,
        });
    }

    let mut package = assemble_graph_recall_package(
        engine,
        request.response,
        request.seed_ids,
        request.config,
        request.input,
    );
    let packaging_mode =
        crate::query::decide_packaging(&package.tensions, request.plan, &request.input.text);

    apply_packaging_mode(
        engine,
        packaging_mode.clone(),
        &request.config.scope,
        &mut package,
    );

    if request.input.now.0 > 0 {
        apply_validity_filter(engine, &mut package, request.input.now);
    }

    // Capture the read-only commit trace from the FINAL package (after packaging mode
    // and validity filtering), so a later `commit` only integrates work for sites that
    // actually survived into the result (ADR-0004 / interactions.md). Read-only.
    package.commit_trace =
        crate::api::build_commit_trace(engine.graph.storage(), request.response, &package);

    let mut trace = trace;
    trace.packaging_mode = Some(packaging_mode);

    Ok(SearchResult { package, trace })
}

fn assemble_graph_recall_package<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    response: &ActivationResponse,
    seed_ids: &[NodeId],
    config: &QueryConfig,
    input: &crate::query::SearchInput,
) -> ContextPackage {
    let storage = engine.graph.storage();
    let now = config.now.unwrap_or_else(Timestamp::now);
    let activations = &response.activation;

    let seed_entity_tags: Vec<Vec<String>> = seed_ids
        .iter()
        .filter_map(|node_id| storage.get_node(*node_id).ok())
        .map(|node| node.entity_tags.clone())
        .collect();

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

    let mut scored: Vec<(f64, TieBreakKey, ScoredNode)> = Vec::new();
    for (&node_id, &activation) in activations {
        if activation < config.min_activation {
            continue;
        }

        let node = match storage.get_node(node_id) {
            Ok(node) => node,
            Err(_) => continue,
        };

        if let Some(ref peer_filter) = input.peer_filter {
            if !peer_filter.contains(&node.origin.peer_id) {
                continue;
            }
        }
        if node.metadata.get("retracted").is_some_and(|v| v == "true") {
            continue;
        }

        let salience = storage.get_salience(node_id).unwrap_or(0.0);
        let retained_action = storage.get_retained_action(node_id).unwrap_or(0.0);
        let impedance = response.impedance.get(&node_id).copied().unwrap_or_default();

        let shared_entities = seed_entity_tags
            .iter()
            .map(|tags| {
                node.entity_tags
                    .iter()
                    .filter(|tag| tags.contains(tag))
                    .count()
            })
            .max()
            .unwrap_or(0);
        let base_scope_weight = scope_weight(&config.scope, &node.origin.scope, shared_entities);
        let trust_weight = engine
            .get_peer(node.origin.peer_id)
            .map(|p| p.trust_level.scope_weight_bonus())
            .unwrap_or(0.0);

        // phi_i: query-field potential bias. The embedding alignment is folded in
        // as the phi term so the readout can credit semantic match additively.
        let phi = match (&config.query_embedding, &node.embedding) {
            (Some(qe), Some(ne)) => cosine_similarity(qe, ne),
            _ => 0.0,
        };

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

        scored.push((
            score,
            key,
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
    scored.sort_by(|(sa, ka, _), (sb, kb, _)| rank(*sa, ka, *sb, kb));
    let scored_nodes: Vec<ScoredNode> = scored.into_iter().map(|(_, _, n)| n).collect();

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

    assemble_context_package(
        scored_nodes,
        &identity_activations,
        &contradiction_pairs,
        config.token_budget,
        config.chars_per_token,
        &config.scope,
    )
}

fn apply_packaging_mode<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    packaging_mode: PackagingMode,
    query_scope: &ScopePath,
    package: &mut ContextPackage,
) {
    match packaging_mode {
        PackagingMode::KnowledgeOnly => {
            package.token_usage.used = package
                .token_usage
                .used
                .saturating_sub(package.token_usage.memories_used);
            package.token_usage.memories_used = 0;
            package.memories.clear();
        }
        PackagingMode::KnowledgeWithProvenance => {
            include_source_memories(
                engine,
                query_scope,
                &package.knowledge,
                &mut package.memories,
            );
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
    query_scope: &ScopePath,
    knowledge: &[Fragment],
    memories: &mut Vec<Fragment>,
) {
    let mut existing: HashSet<NodeId> = memories.iter().map(|fragment| fragment.node_id).collect();

    for fragment in knowledge {
        for source_fragment in source_memory_fragments(engine, query_scope, fragment) {
            if existing.insert(source_fragment.node_id) {
                memories.push(source_fragment);
            }
        }
    }
}

fn source_memory_fragments<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    query_scope: &ScopePath,
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
        push_source_memory_fragment(
            engine,
            query_scope,
            fragment,
            source_id,
            weight,
            &mut seen,
            &mut fragments,
        );
    }

    for &edge_id in storage.edges_from(fragment.node_id) {
        let Some((source_id, weight)) = storage.get_edge(edge_id).ok().and_then(|edge| {
            (edge.edge_type == EdgeType::ExtractedFrom).then_some((edge.target, edge.weight))
        }) else {
            continue;
        };
        push_source_memory_fragment(
            engine,
            query_scope,
            fragment,
            source_id,
            weight,
            &mut seen,
            &mut fragments,
        );
    }

    fragments
}

fn push_source_memory_fragment<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    query_scope: &ScopePath,
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
    if !matches!(
        node.node_type,
        KnowledgeType::Episodic | KnowledgeType::Event
    ) {
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
        scope: determine_scope(query_scope, &node.origin.scope),
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
    matches!(
        node_type,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    )
}
