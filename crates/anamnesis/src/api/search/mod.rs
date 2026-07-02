//! Search module — unified text + vector + graph retrieval.
//!
//! This module implements the `Engine::search()` method, which combines
//! text search, vector similarity, and spreading activation to retrieve
//! relevant knowledge fragments.

use std::collections::HashMap;

use crate::error::Error;
use crate::graph::{KnowledgeType, NodeId};
use crate::query::{QueryConfig, SearchCandidate, SearchInput, SearchResult};

pub(crate) mod assemble;
pub(crate) mod candidates;
pub(crate) mod fusion;
pub(crate) mod plan;
pub(crate) mod recall;
pub(crate) mod seeds;

use crate::api::Engine;
use crate::storage::StorageAdapter;

const DEFAULT_SOURCE_CANDIDATE_LIMIT: usize = 64;

/// Execute unified search — combines text search, vector similarity, and graph traversal.
///
/// Automatically derives a `SearchPlan` from the input and executes the appropriate
/// retrieval strategies. Returns a `SearchResult` with a `ContextPackage` and trace.
///
/// Returns `Err(Error::InvalidInput)` if both `text` is empty and `query_embedding` is `None`.
pub(crate) fn search<S: StorageAdapter + Clone>(
    engine: &Engine<S>,
    input: SearchInput,
) -> Result<SearchResult, Error> {
    let plan = plan::derive_search_plan(&input, &engine.config)?;

    let mut per_source: Vec<Vec<SearchCandidate>> = Vec::new();
    let mut strategies_used: Vec<String> = Vec::new();
    let mut flow_invocations = 0usize;
    let storage = engine.graph.storage();

    let sub_queries = if plan.use_text {
        plan::decompose_query(&plan.text)
    } else {
        Vec::new()
    };
    let source_candidate_limit = input
        .limit
        .max(plan.seed_limit)
        .max(DEFAULT_SOURCE_CANDIDATE_LIMIT);

    if plan.use_text {
        for sub_query in &sub_queries {
            let text_candidates =
                candidates::collect_text_candidates(storage, sub_query, source_candidate_limit);
            if !text_candidates.is_empty() {
                if !strategies_used
                    .iter()
                    .any(|strategy| strategy == "text_search")
                {
                    strategies_used.push("text_search".to_string());
                }
                per_source.push(text_candidates);
            }
        }
    }

    if plan.use_vector
        && let Some(ref query_embedding) = input.query_embedding
    {
        let vector_candidates =
            candidates::collect_vector_candidates(storage, query_embedding, source_candidate_limit);
        if !vector_candidates.is_empty() {
            per_source.push(vector_candidates);
        }
        strategies_used.push("vector_similarity".to_string());
    }

    if plan.use_entity {
        let entity_candidates = candidates::collect_entity_candidates(
            storage,
            &input.entity_tags,
            source_candidate_limit,
        );
        if !entity_candidates.is_empty() {
            per_source.push(entity_candidates);
            strategies_used.push("entity_tags".to_string());
        }
    }

    let fused = fusion::fuse_candidates(per_source);
    let selected_seeds = seeds::select_recall_seeds(fused, input.seed_limit);
    let all_seed_ids: Vec<NodeId> = selected_seeds.iter().map(|c| c.node_id).collect();

    let config = QueryConfig {
        budget: input.limit.saturating_mul(5),
        min_activation: 0.0,
        agent_id: input.agent_id.clone(),
        scope: input.scope.clone(),
        query_embedding: input.query_embedding.clone(),
        context: input.context.clone(),
        now: (input.now.0 > 0).then_some(input.now),
        ..QueryConfig::default()
    };

    let mut response = crate::query::ActivationResponse::default();
    let mut field = crate::query::field::QueryField::new();

    if plan.use_graph {
        let identity_prior = identity_prior_for_search(storage, &config);
        let identity_prior_ref = (!identity_prior.is_empty()).then_some(&identity_prior);
        let (graph_response, recall_trace, recall_field) = recall::run_graph_recalls(
            storage,
            &selected_seeds,
            &config,
            identity_prior_ref,
            &plan.time_cues,
        );

        flow_invocations = recall_trace.invocation_count as usize;
        response = graph_response;
        field = recall_field;
    }

    if flow_invocations > 0 {
        strategies_used.push("activation_flow".to_string());
    }

    assemble::assemble_search_result(
        engine,
        assemble::SearchAssemblyRequest {
            response: &response,
            seed_ids: &all_seed_ids,
            config: &config,
            input: &input,
            plan: &plan,
            strategies_used,
            field: &field,
        },
    )
}

fn identity_prior_for_search<S: StorageAdapter>(
    storage: &S,
    config: &QueryConfig,
) -> HashMap<NodeId, f64> {
    let Some(agent_id) = &config.agent_id else {
        return HashMap::new();
    };

    let peer_id = crate::graph::types::PeerId(agent_id.parse::<u64>().unwrap_or(0));
    storage
        .nodes_by_peer(peer_id)
        .into_iter()
        .filter_map(|node_id| {
            let node = storage.get_node(node_id).ok()?;
            is_identity_type(&node.node_type).then(|| {
                let salience = storage.get_salience(node_id).unwrap_or(0.0).clamp(0.0, 1.0);
                (node_id, salience)
            })
        })
        .collect()
}

fn is_identity_type(node_type: &KnowledgeType) -> bool {
    matches!(node_type, KnowledgeType::Identity)
}
