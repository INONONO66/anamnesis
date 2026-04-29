//! ContextPackage assembly for the Anamnesis query pipeline.
//!
//! Partitions scored nodes by type (identity/knowledge/memories), applies
//! L0/L1/L2 resolution based on token budget, and computes agent tension.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equation
//! (14) T_agent = sum_{k in Identity(a)} rho_k * sum_{Contradicts(k)} w_neg * X_j

use std::collections::HashMap;

use crate::graph::node::Origin;
use crate::graph::{KnowledgeType, NodeId};
use crate::mechanics::repulsion::rigidity;
use crate::query::types::{ContextPackage, Fragment, Scope, Tension, TokenBudget};

/// Determines the scope of a node relative to the query context.
pub fn determine_scope(query_project: &Option<String>, node_project: &Option<String>) -> Scope {
    match (query_project, node_project) {
        (Some(q), Some(n)) if q == n => Scope::SameProject,
        (_, None) | (None, _) => Scope::Universal,
        (_, Some(n)) => Scope::OtherProject(n.clone()),
    }
}

/// Determines whether a node type belongs to the identity partition.
pub fn is_identity_type(kt: &KnowledgeType) -> bool {
    matches!(
        kt,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    )
}

/// Determines whether a node type belongs to the memories partition.
pub fn is_memory_type(kt: &KnowledgeType) -> bool {
    matches!(kt, KnowledgeType::Episodic | KnowledgeType::Event)
}

/// Estimates the token count for a string.
///
/// Uses a simple character-based heuristic: `len / chars_per_token` (ceiling).
pub fn estimate_tokens(text: &str, chars_per_token: usize) -> usize {
    let cpt = chars_per_token.max(1);
    text.chars().count().div_ceil(cpt)
}

/// Resolution level for fragment content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// L0: name only.
    L0,
    /// L1: name + summary.
    L1,
    /// L2: name + summary + content.
    L2,
}

/// Builds a [`Fragment`] from a scored node at the specified resolution.
pub fn build_fragment(node: &ScoredNode, scope: Scope, resolution: Resolution) -> Fragment {
    Fragment {
        node_id: node.node_id,
        name: node.name.clone(),
        summary: match resolution {
            Resolution::L0 => None,
            Resolution::L1 | Resolution::L2 => node.summary.clone(),
        },
        content: match resolution {
            Resolution::L0 | Resolution::L1 => None,
            Resolution::L2 => Some(node.content.clone()),
        },
        node_type: node.node_type.clone(),
        relevance: node.relevance,
        origin: node.origin.clone(),
        scope,
    }
}

/// Input for assembling a [`ContextPackage`].
pub struct ScoredNode {
    pub node_id: NodeId,
    pub name: String,
    pub summary: Option<String>,
    pub content: String,
    pub node_type: KnowledgeType,
    pub relevance: f64,
    pub origin: Origin,
}

/// Computes the agent tension score.
///
/// Equation (14): T_agent = sum_{k in Identity(a)} rho_k * sum_{Contradicts(k)} w_neg * X_j
///
/// Only counts contradictions where the identity node is actually an endpoint
/// (source or target). The "other" endpoint's activation is used as X_j.
pub fn compute_agent_tension(
    identity_activations: &[(NodeId, KnowledgeType, f64)],
    contradicts_edges: &[(NodeId, NodeId, f64)],
    activations: &HashMap<NodeId, f64>,
) -> f64 {
    let mut tension = 0.0;

    for (identity_id, kt, _identity_activation) in identity_activations {
        let rho = rigidity(kt);

        for (source, target, w_neg) in contradicts_edges {
            let other = if source == identity_id {
                Some(target)
            } else if target == identity_id {
                Some(source)
            } else {
                None
            };

            if let Some(other_id) = other {
                let other_activation = activations.get(other_id).copied().unwrap_or(0.0);
                if other_activation > 0.0 {
                    tension += rho * w_neg * other_activation;
                }
            }
        }
    }

    tension.clamp(0.0, 1.0)
}

/// Assembles a [`ContextPackage`] from scored nodes.
///
/// 1. Sorts nodes by relevance (descending).
/// 2. Assigns resolution (L0/L1/L2) by relevance and remaining token budget.
/// 3. Partitions into identity / knowledge / memories by [`KnowledgeType`].
/// 4. Collects [`Tension`] entries for activated contradiction pairs.
/// 5. Computes `agent_tension` via equation (14).
pub fn assemble_context_package(
    mut scored_nodes: Vec<ScoredNode>,
    identity_activations: &[(NodeId, KnowledgeType, f64)],
    contradicts_edges: &[(NodeId, NodeId, f64)],
    activations: &HashMap<NodeId, f64>,
    token_budget: usize,
    chars_per_token: usize,
    query_project: &Option<String>,
) -> ContextPackage {
    scored_nodes.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut identity_frags: Vec<Fragment> = Vec::new();
    let mut knowledge_frags: Vec<Fragment> = Vec::new();
    let mut memory_frags: Vec<Fragment> = Vec::new();
    let mut tensions: Vec<Tension> = Vec::new();

    let mut budget = TokenBudget::new(token_budget);

    for node in &scored_nodes {
        let name_tokens = estimate_tokens(&node.name, chars_per_token);
        let summary_tokens = node
            .summary
            .as_ref()
            .map(|s| estimate_tokens(s, chars_per_token))
            .unwrap_or(0);
        let content_tokens = estimate_tokens(&node.content, chars_per_token);
        let l1_tokens = name_tokens.saturating_add(summary_tokens);
        let l2_tokens = l1_tokens.saturating_add(content_tokens);
        let remaining = budget.remaining();

        if remaining < name_tokens {
            break; // No budget left even for L0
        }

        let scope = determine_scope(query_project, &node.origin.project_id);
        let (resolution, tokens_used) = if remaining >= l2_tokens {
            (Resolution::L2, l2_tokens)
        } else if remaining >= l1_tokens {
            (Resolution::L1, l1_tokens)
        } else {
            (Resolution::L0, name_tokens)
        };
        let frag = build_fragment(node, scope, resolution);

        if is_identity_type(&node.node_type) {
            budget.used += tokens_used;
            budget.identity_used += tokens_used;
            identity_frags.push(frag);
        } else if is_memory_type(&node.node_type) {
            budget.used += tokens_used;
            budget.memories_used += tokens_used;
            memory_frags.push(frag);
        } else {
            budget.used += tokens_used;
            budget.knowledge_used += tokens_used;
            knowledge_frags.push(frag);
        }
    }

    // Build tensions from Contradicts edges between activated nodes
    for (source, target, w) in contradicts_edges {
        let source_act = activations.get(source).copied().unwrap_or(0.0);
        let target_act = activations.get(target).copied().unwrap_or(0.0);
        if source_act > 0.0 && target_act > 0.0 {
            tensions.push(Tension {
                node_a: *source,
                node_b: *target,
                edge_weight: *w,
                description: None,
            });
        }
    }

    let agent_tension = compute_agent_tension(identity_activations, contradicts_edges, activations);

    ContextPackage {
        identity: identity_frags,
        knowledge: knowledge_frags,
        memories: memory_frags,
        tensions,
        token_usage: budget,
        agent_tension,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_origin() -> Origin {
        Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: Some("proj-a".to_string()),
            confidence: 0.9,
        }
    }

    fn make_node(id: u64, kt: KnowledgeType, name: &str, relevance: f64) -> ScoredNode {
        ScoredNode {
            node_id: NodeId(id),
            name: name.to_string(),
            summary: Some(format!("Summary of {name}")),
            content: format!("Full content of {name}"),
            node_type: kt,
            relevance,
            origin: make_origin(),
        }
    }

    #[test]
    fn identity_nodes_go_to_identity_partition() {
        let nodes = vec![
            make_node(0, KnowledgeType::IdentityCore, "I am an architect", 0.9),
            make_node(1, KnowledgeType::Semantic, "auth uses factory", 0.8),
        ];
        let pkg = assemble_context_package(
            nodes,
            &[],
            &[],
            &HashMap::new(),
            10000,
            4,
            &Some("proj-a".to_string()),
        );
        assert_eq!(pkg.identity.len(), 1);
        assert_eq!(pkg.knowledge.len(), 1);
        assert_eq!(pkg.memories.len(), 0);
    }

    #[test]
    fn episodic_nodes_go_to_memories_partition() {
        let nodes = vec![
            make_node(0, KnowledgeType::Episodic, "session note", 0.7),
            make_node(1, KnowledgeType::Event, "deployment event", 0.6),
        ];
        let pkg = assemble_context_package(nodes, &[], &[], &HashMap::new(), 10000, 4, &None);
        assert_eq!(pkg.memories.len(), 2);
        assert_eq!(pkg.identity.len(), 0);
        assert_eq!(pkg.knowledge.len(), 0);
    }

    #[test]
    fn token_budget_respected() {
        let nodes = vec![
            make_node(0, KnowledgeType::Semantic, &"a".repeat(100), 0.9),
            make_node(1, KnowledgeType::Semantic, &"b".repeat(100), 0.8),
            make_node(2, KnowledgeType::Semantic, &"c".repeat(100), 0.7),
        ];
        // Budget of 10 tokens (40 chars) — only first node's name fits
        let pkg = assemble_context_package(nodes, &[], &[], &HashMap::new(), 10, 4, &None);
        assert!(
            pkg.token_usage.used <= 10 + 1, // allow 1 token rounding
            "used {} tokens, budget was 10",
            pkg.token_usage.used
        );
    }

    #[test]
    fn tensions_populated_for_activated_contradictions() {
        let a = NodeId(0);
        let b = NodeId(1);
        let mut activations = HashMap::new();
        activations.insert(a, 0.8);
        activations.insert(b, 0.6);

        let contradicts = vec![(a, b, 0.9)];
        let pkg =
            assemble_context_package(vec![], &[], &contradicts, &activations, 10000, 4, &None);
        assert_eq!(pkg.tensions.len(), 1);
        assert_eq!(pkg.tensions[0].node_a, a);
        assert_eq!(pkg.tensions[0].node_b, b);
    }

    #[test]
    fn agent_tension_zero_without_contradictions() {
        let pkg = assemble_context_package(vec![], &[], &[], &HashMap::new(), 10000, 4, &None);
        assert_eq!(pkg.agent_tension, 0.0);
    }

    #[test]
    fn agent_tension_nonzero_with_identity_contradiction() {
        let identity_id = NodeId(0);
        let contradicted = NodeId(1);
        let mut activations = HashMap::new();
        activations.insert(identity_id, 0.9);
        activations.insert(contradicted, 0.8);

        let identity_acts = vec![(identity_id, KnowledgeType::IdentityCore, 0.9)];
        let contradicts = vec![(identity_id, contradicted, 0.8)];

        let tension = compute_agent_tension(&identity_acts, &contradicts, &activations);
        assert!(
            tension > 0.0,
            "tension should be > 0 with active contradiction"
        );
    }

    #[test]
    fn agent_tension_ignores_unrelated_contradictions() {
        let identity_id = NodeId(0);
        let unrelated_a = NodeId(5);
        let unrelated_b = NodeId(6);
        let mut activations = HashMap::new();
        activations.insert(identity_id, 0.9);
        activations.insert(unrelated_a, 0.8);
        activations.insert(unrelated_b, 0.7);

        let identity_acts = vec![(identity_id, KnowledgeType::IdentityCore, 0.9)];
        let contradicts = vec![(unrelated_a, unrelated_b, 0.9)];

        let tension = compute_agent_tension(&identity_acts, &contradicts, &activations);
        assert_eq!(
            tension, 0.0,
            "unrelated contradictions should not affect identity tension"
        );
    }

    #[test]
    fn sorted_by_relevance_descending() {
        let nodes = vec![
            make_node(0, KnowledgeType::Semantic, "low relevance", 0.3),
            make_node(1, KnowledgeType::Semantic, "high relevance", 0.9),
            make_node(2, KnowledgeType::Semantic, "medium relevance", 0.6),
        ];
        let pkg = assemble_context_package(nodes, &[], &[], &HashMap::new(), 10000, 4, &None);
        assert_eq!(pkg.knowledge.len(), 3);
        assert!(pkg.knowledge[0].relevance >= pkg.knowledge[1].relevance);
        assert!(pkg.knowledge[1].relevance >= pkg.knowledge[2].relevance);
    }

    #[test]
    fn determine_scope_same_project() {
        let scope = determine_scope(&Some("proj-a".to_string()), &Some("proj-a".to_string()));
        assert_eq!(scope, Scope::SameProject);
    }

    #[test]
    fn determine_scope_universal() {
        let scope = determine_scope(&Some("proj-a".to_string()), &None);
        assert_eq!(scope, Scope::Universal);
    }

    #[test]
    fn determine_scope_other_project() {
        let scope = determine_scope(&Some("proj-a".to_string()), &Some("proj-b".to_string()));
        assert_eq!(scope, Scope::OtherProject("proj-b".to_string()));
    }
}
