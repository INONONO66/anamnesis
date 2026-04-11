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
    if chars_per_token == 0 {
        return 0;
    }
    text.len().div_ceil(chars_per_token)
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
/// # Parameters
/// - `identity_activations`: `(node_type, activation)` for activated identity nodes
/// - `contradicts_edges`: `(source_id, target_id, edge_weight)` for all Contradicts edges
/// - `activations`: activation map for all nodes
pub fn compute_agent_tension(
    identity_activations: &[(KnowledgeType, f64)],
    contradicts_edges: &[(NodeId, NodeId, f64)],
    activations: &HashMap<NodeId, f64>,
) -> f64 {
    let mut tension = 0.0;

    for (kt, _identity_activation) in identity_activations {
        let rho = rigidity(kt);

        for (_source, target, w_neg) in contradicts_edges {
            let target_activation = activations.get(target).copied().unwrap_or(0.0);
            if target_activation > 0.0 {
                tension += rho * w_neg * target_activation;
            }
        }
    }

    tension.clamp(0.0, 1.0)
}

/// Assembles a [`ContextPackage`] from scored nodes.
///
/// 1. Sorts nodes by relevance (descending).
/// 2. Partitions into identity / knowledge / memories by [`KnowledgeType`].
/// 3. Assigns resolution (L0/L1/L2) respecting the token budget.
/// 4. Upgrades the top-3 knowledge fragments to L2 if budget remains.
/// 5. Collects [`Tension`] entries for activated contradiction pairs.
/// 6. Computes `agent_tension` via equation (14).
pub fn assemble_context_package(
    mut scored_nodes: Vec<ScoredNode>,
    identity_activations: &[(KnowledgeType, f64)],
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

        if budget.remaining() < name_tokens {
            break; // No budget left even for L0
        }

        let scope = determine_scope(query_project, &node.origin.project_id);

        if is_identity_type(&node.node_type) {
            // Identity: always L0 (names only)
            let frag = build_fragment(node, scope, Resolution::L0);
            budget.used += name_tokens;
            budget.identity_used += name_tokens;
            identity_frags.push(frag);
        } else if is_memory_type(&node.node_type) {
            // Memories: L0 by default
            let frag = build_fragment(node, scope, Resolution::L0);
            budget.used += name_tokens;
            budget.memories_used += name_tokens;
            memory_frags.push(frag);
        } else {
            // Knowledge: L1 if budget allows, L0 otherwise
            let summary_tokens = node
                .summary
                .as_ref()
                .map(|s| estimate_tokens(s, chars_per_token))
                .unwrap_or(0);

            let resolution = if budget.remaining() >= name_tokens + summary_tokens {
                Resolution::L1
            } else {
                Resolution::L0
            };

            let tokens_used = match resolution {
                Resolution::L1 => name_tokens + summary_tokens,
                _ => name_tokens,
            };

            let frag = build_fragment(node, scope, resolution);
            budget.used += tokens_used;
            budget.knowledge_used += tokens_used;
            knowledge_frags.push(frag);
        }
    }

    // Upgrade top-3 knowledge fragments to L2 if budget allows
    for frag in knowledge_frags.iter_mut().take(3) {
        if let Some(node) = scored_nodes.iter().find(|n| n.node_id == frag.node_id) {
            let content_tokens = estimate_tokens(&node.content, chars_per_token);
            if budget.remaining() >= content_tokens {
                frag.content = Some(node.content.clone());
                budget.used += content_tokens;
                budget.knowledge_used += content_tokens;
            }
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
        let contradicted = NodeId(1);
        let mut activations = HashMap::new();
        activations.insert(contradicted, 0.8);

        let identity_acts = vec![(KnowledgeType::IdentityCore, 0.9)];
        let contradicts = vec![(NodeId(0), contradicted, 0.8)];

        let tension = compute_agent_tension(&identity_acts, &contradicts, &activations);
        assert!(
            tension > 0.0,
            "tension should be > 0 with active contradiction"
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
