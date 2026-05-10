//! Result assembly stage — package graph activations into a `SearchResult`.

use std::collections::{HashMap, HashSet};

use crate::api::{Engine, SpreadingModel};
use crate::error::Error;
use crate::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use crate::mechanics::attraction::cosine_similarity;
use crate::mechanics::gravity::compute_mass;
use crate::mechanics::repulsion::{apply_damping, compute_repulsion, rigidity};
use crate::query::activation::edge_valid_at;
use crate::query::assembly::{ScoredNode, assemble_context_package, determine_scope};
use crate::query::scoring::{final_score, scope_weight};
use crate::query::types::SearchPlan;
use crate::query::{
    ContextPackage, Fragment, PackagingMode, QueryConfig, SearchInput, SearchResult, SearchTrace,
};
use crate::storage::StorageAdapter;

pub(crate) struct SearchAssemblyRequest<'a> {
    pub(crate) activations: &'a HashMap<NodeId, f64>,
    pub(crate) seed_ids: &'a [NodeId],
    pub(crate) config: &'a QueryConfig,
    pub(crate) input: &'a SearchInput,
    pub(crate) plan: &'a SearchPlan,
    pub(crate) strategies_used: Vec<String>,
    pub(crate) spread_iterations: usize,
    pub(crate) spreading_model: Option<SpreadingModel>,
    pub(crate) edge_count_skipped_invalid: usize,
    pub(crate) convergence_rounds: usize,
    pub(crate) converged: bool,
}

pub(crate) fn assemble_search_result<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    request: SearchAssemblyRequest<'_>,
) -> Result<SearchResult, Error> {
    if request.activations.is_empty() {
        return Ok(SearchResult {
            package: ContextPackage::empty(),
            trace: SearchTrace {
                strategies_used: request.strategies_used,
                seed_count: request.seed_ids.len(),
                spread_iterations: request.spread_iterations,
                spreading_model: request.spreading_model,
                packaging_mode: None,
                edge_count_skipped_invalid: request.edge_count_skipped_invalid,
                convergence_rounds: request.convergence_rounds,
                converged: request.converged,
            },
        });
    }

    let mut package = assemble_graph_recall_package(
        engine,
        request.activations,
        request.seed_ids,
        request.config,
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

    let trace = SearchTrace {
        strategies_used: request.strategies_used,
        seed_count: request.seed_ids.len(),
        spread_iterations: request.spread_iterations,
        spreading_model: request.spreading_model,
        packaging_mode: Some(packaging_mode),
        edge_count_skipped_invalid: request.edge_count_skipped_invalid,
        convergence_rounds: request.convergence_rounds,
        converged: request.converged,
    };

    Ok(SearchResult { package, trace })
}

fn assemble_graph_recall_package<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    activations: &HashMap<NodeId, f64>,
    seed_ids: &[NodeId],
    config: &QueryConfig,
) -> ContextPackage {
    let storage = engine.graph.storage();
    let mut damped_activations = activations.clone();
    let now = config.now.unwrap_or_else(Timestamp::now);

    for &node_id in activations.keys() {
        let contradicts_inputs: Vec<(f64, f64, f64)> = storage
            .edges_to(node_id)
            .iter()
            .filter_map(|&edge_id| {
                let edge = storage.get_edge(edge_id).ok()?;
                if !matches!(edge.edge_type, EdgeType::Contradicts) {
                    return None;
                }
                if !edge_valid_at(edge, now) {
                    return None;
                }
                let source_activation = activations.get(&edge.source).copied().unwrap_or(0.0);
                if source_activation == 0.0 {
                    return None;
                }
                let source_node = storage.get_node(edge.source).ok()?;
                Some((
                    edge.weight,
                    rigidity(&source_node.node_type),
                    source_activation,
                ))
            })
            .collect();

        if contradicts_inputs.is_empty() {
            continue;
        }

        let repulsion = compute_repulsion(&contradicts_inputs);
        if repulsion > 0.0 {
            let current = activations.get(&node_id).copied().unwrap_or(0.0);
            damped_activations.insert(node_id, apply_damping(current, repulsion));
        }
    }

    let seed_entity_tags: Vec<Vec<String>> = seed_ids
        .iter()
        .filter_map(|node_id| storage.get_node(*node_id).ok())
        .map(|node| node.entity_tags.clone())
        .collect();
    let hopfield_context = match engine.config.energy_model {
        super::super::EnergyModel::WeightedSum => None,
        super::super::EnergyModel::Hopfield => super::super::build_hopfield_scoring_context(
            &config.query_embedding,
            &damped_activations,
            storage,
        ),
    };

    let mut scored_nodes = Vec::new();
    for (&node_id, &activation) in &damped_activations {
        if activation < config.min_activation {
            continue;
        }

        let node = match storage.get_node(node_id) {
            Ok(node) => node,
            Err(_) => continue,
        };
        let salience = storage.get_salience(node_id).unwrap_or(0.0);
        let mass = compute_mass(salience, node.access_count, &node.node_type);
        let vector_similarity = match (&config.query_embedding, &node.embedding) {
            (Some(query_embedding), Some(node_embedding)) => {
                cosine_similarity(query_embedding, node_embedding)
            }
            _ => 0.0,
        };
        let scoring_similarity = match engine.config.energy_model {
            super::super::EnergyModel::WeightedSum => vector_similarity,
            super::super::EnergyModel::Hopfield => super::super::hopfield_adjusted_similarity(
                hopfield_context.as_ref(),
                vector_similarity,
                node.embedding.as_ref(),
            ),
        };
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
        let scope_weight = scope_weight(&config.scope, &node.origin.scope, shared_entities);
        let relevance = final_score(activation, scoring_similarity, salience, mass, scope_weight);

        scored_nodes.push(ScoredNode {
            node_id,
            name: node.name.clone(),
            summary: node.summary.clone(),
            content: node.content.clone(),
            node_type: node.node_type.clone(),
            relevance,
            origin: node.origin.clone(),
        });
    }

    let mut contradicts_edges: Vec<(NodeId, NodeId, f64)> = Vec::new();
    for &node_id in damped_activations.keys() {
        for &edge_id in storage.edges_from(node_id) {
            if let Ok(edge) = storage.get_edge(edge_id)
                && matches!(edge.edge_type, EdgeType::Contradicts)
                && edge_valid_at(edge, now)
            {
                contradicts_edges.push((edge.source, edge.target, edge.weight));
            }
        }
    }

    let identity_activations: Vec<(NodeId, KnowledgeType, f64)> = damped_activations
        .iter()
        .filter_map(|(&node_id, &activation)| {
            let node = storage.get_node(node_id).ok()?;
            if !is_identity_type(&node.node_type) {
                return None;
            }
            let Some(agent_id) = &config.agent_id else {
                return None;
            };
            (node.origin.agent_id == *agent_id)
                .then(|| (node_id, node.node_type.clone(), activation))
        })
        .collect();

    assemble_context_package(
        scored_nodes,
        &identity_activations,
        &contradicts_edges,
        &damped_activations,
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
    engine.graph.get_node(node_id).is_ok_and(|node| {
        let from_ok = node.valid_from.is_none_or(|valid_from| valid_from <= as_of);
        let until_ok = node
            .valid_until
            .is_none_or(|valid_until| valid_until > as_of);
        from_ok && until_ok
    })
}

fn is_identity_type(node_type: &KnowledgeType) -> bool {
    matches!(
        node_type,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    )
}
