//! ContextPackage assembly for the Anamnesis query pipeline.
//!
//! Partitions scored nodes by type (identity/knowledge/memories), applies
//! L0/L1/L2 resolution based on token budget, and computes agent tension.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equation
//! (14) T_agent = sum_{k in Identity(a)} rho_k * sum_{Contradicts(k)} w_neg * X_j

use std::collections::{HashMap, HashSet};

use crate::graph::node::Origin;
use crate::graph::scope::{ScopePath, ScopeRelation};
use crate::graph::{KnowledgeType, NodeId};
use crate::mechanics::repulsion::rigidity;
use crate::query::types::{ContextPackage, Fragment, Query, Tension, TokenBudget};

/// Determines the scope of a node relative to the query context.
pub fn determine_scope(query_scope: &ScopePath, node_scope: &ScopePath) -> ScopeRelation {
    query_scope.relation_to(node_scope)
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
pub fn build_fragment(node: &ScoredNode, scope: ScopeRelation, resolution: Resolution) -> Fragment {
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

fn upgrade_fragment_to_l2(
    fragment: &mut Fragment,
    node: &ScoredNode,
    budget: &mut TokenBudget,
    chars_per_token: usize,
) -> usize {
    if fragment.content.is_some() {
        return 0;
    }

    let missing_summary_tokens = match (&node.summary, &fragment.summary) {
        (Some(summary), None) => estimate_tokens(summary, chars_per_token),
        _ => 0,
    };
    let content_tokens = estimate_tokens(&node.content, chars_per_token);
    let tokens_needed = missing_summary_tokens.saturating_add(content_tokens);

    if budget.remaining() < tokens_needed {
        return 0;
    }

    if fragment.summary.is_none() {
        fragment.summary = node.summary.clone();
    }
    fragment.content = Some(node.content.clone());
    budget.used += tokens_needed;
    tokens_needed
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
    query_scope: &ScopePath,
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

        let scope = determine_scope(query_scope, &node.origin.scope);
        let (resolution, tokens_used) = if remaining >= l2_tokens {
            (Resolution::L2, l2_tokens)
        } else if remaining >= l1_tokens {
            (Resolution::L1, l1_tokens)
        } else {
            (Resolution::L0, name_tokens)
        };

        if is_identity_type(&node.node_type) {
            let frag = build_fragment(node, scope, resolution);
            budget.used += tokens_used;
            budget.identity_used += tokens_used;
            identity_frags.push(frag);
        } else if is_memory_type(&node.node_type) {
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
            budget.memories_used += tokens_used;
            memory_frags.push(frag);
        } else {
            let frag = build_fragment(node, scope, resolution);
            budget.used += tokens_used;
            budget.knowledge_used += tokens_used;
            knowledge_frags.push(frag);
        }
    }

    // Upgrade knowledge fragments to L2 while budget allows.
    for frag in &mut knowledge_frags {
        if let Some(node) = scored_nodes.iter().find(|n| n.node_id == frag.node_id) {
            budget.knowledge_used +=
                upgrade_fragment_to_l2(frag, node, &mut budget, chars_per_token);
        }
    }

    // Upgrade memory fragments to L2 while budget allows.
    for frag in &mut memory_frags {
        if let Some(node) = scored_nodes.iter().find(|n| n.node_id == frag.node_id) {
            budget.memories_used +=
                upgrade_fragment_to_l2(frag, node, &mut budget, chars_per_token);
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

/// Hints for mode-specific assembly that cannot be derived from scored nodes alone.
#[derive(Debug, Clone, Default)]
pub struct ModeContext {
    /// Node IDs at depth 1 for Neighborhood queries.
    pub adjacent_ids: HashSet<NodeId>,
}

const BUDGET_IDENTITY_PCT: f64 = 0.10;
const BUDGET_KNOWLEDGE_PCT: f64 = 0.65;
const BUDGET_MEMORY_PCT: f64 = 0.20;

const ELEVATION_SALIENCE_THRESHOLD: f64 = 0.7;
const ELEVATION_ACTIVATION_THRESHOLD: f64 = 0.5;

/// Assembles a [`ContextPackage`] with query-mode-aware resolution policy.
///
/// Delegates to [`assemble_context_package`] for base assembly, then applies
/// mode-specific resolution adjustments and budget partitioning.
///
/// # Resolution Policy by Mode
///
/// | Query Mode | Knowledge Default | Memory Default | Top-k L2 |
/// |------------|------------------|----------------|----------|
/// | Associative | L1 | L0 | top-3 knowledge |
/// | Neighborhood | L2 (adjacent), L1 (others) | L1 (adjacent), L0 (others) | all adjacent |
/// | Temporal | L0 | L1 (all visible) | top-3 memories |
/// | TypeFiltered | L2 (target type), L0 (others) | L0 | all target type |
/// | List | L0 (all) | L0 (all) | none (index mode) |
///
/// # Elevation Rules
///
/// - **Salience-conditional memory elevation**: memory fragments with salience > 0.7
///   and activation > 0.5 are promoted to L1.
/// - **Tension-triggered provenance elevation**: fragments involved in Contradicts edges
///   are promoted to at least L1 to surface provenance context.
///
/// # Budget Allocation
///
/// 10% identity, 65% knowledge, 20% memory, 5% overhead.
#[allow(clippy::too_many_arguments)]
pub fn assemble_context_package_for_mode(
    scored_nodes: Vec<ScoredNode>,
    query: &Query,
    identity_activations: &[(NodeId, KnowledgeType, f64)],
    contradicts_edges: &[(NodeId, NodeId, f64)],
    activations: &HashMap<NodeId, f64>,
    token_budget: usize,
    chars_per_token: usize,
    query_scope: &ScopePath,
    mode_context: &ModeContext,
) -> ContextPackage {
    let upgrade_data: HashMap<NodeId, NodeUpgradeData> = scored_nodes
        .iter()
        .map(|n| {
            let target_res = target_resolution_for_node(
                n,
                query,
                &mode_context.adjacent_ids,
                activations,
                contradicts_edges,
            );
            (
                n.node_id,
                NodeUpgradeData {
                    summary: n.summary.clone(),
                    content: n.content.clone(),
                    target_resolution: target_res,
                },
            )
        })
        .collect();

    let mut package = assemble_context_package(
        scored_nodes,
        identity_activations,
        contradicts_edges,
        activations,
        token_budget,
        chars_per_token,
        query_scope,
    );

    if matches!(query, Query::Associative { .. } | Query::List { .. }) {
        return package;
    }

    let identity_budget = (token_budget as f64 * BUDGET_IDENTITY_PCT) as usize;
    let knowledge_budget = (token_budget as f64 * BUDGET_KNOWLEDGE_PCT) as usize;
    let memory_budget = (token_budget as f64 * BUDGET_MEMORY_PCT) as usize;

    apply_mode_resolution(
        &mut package.identity,
        &upgrade_data,
        identity_budget,
        chars_per_token,
    );
    apply_mode_resolution(
        &mut package.knowledge,
        &upgrade_data,
        knowledge_budget,
        chars_per_token,
    );
    apply_mode_resolution(
        &mut package.memories,
        &upgrade_data,
        memory_budget,
        chars_per_token,
    );

    recalculate_token_usage(&mut package, chars_per_token);

    package
}

struct NodeUpgradeData {
    summary: Option<String>,
    content: String,
    target_resolution: Resolution,
}

fn target_resolution_for_node(
    node: &ScoredNode,
    query: &Query,
    adjacent_ids: &HashSet<NodeId>,
    activations: &HashMap<NodeId, f64>,
    contradicts_edges: &[(NodeId, NodeId, f64)],
) -> Resolution {
    let is_adjacent = adjacent_ids.contains(&node.node_id);
    let is_identity = is_identity_type(&node.node_type);
    let is_memory = is_memory_type(&node.node_type);

    let in_tension = contradicts_edges
        .iter()
        .any(|(a, b, _)| *a == node.node_id || *b == node.node_id);

    let activation = activations.get(&node.node_id).copied().unwrap_or(0.0);
    let elevated_memory = is_memory
        && node.relevance > ELEVATION_SALIENCE_THRESHOLD
        && activation > ELEVATION_ACTIVATION_THRESHOLD;

    if is_identity {
        return if in_tension {
            Resolution::L2
        } else {
            Resolution::L1
        };
    }

    match query {
        Query::Associative { .. } => {
            if is_memory {
                Resolution::L0
            } else {
                Resolution::L1
            }
        }
        Query::Neighborhood { entity, .. } => {
            let is_entity_root = node.node_id == *entity;
            if is_memory {
                if is_adjacent || is_entity_root || elevated_memory {
                    Resolution::L1
                } else {
                    Resolution::L0
                }
            } else if is_adjacent || is_entity_root {
                Resolution::L2
            } else {
                Resolution::L1
            }
        }
        Query::Temporal { .. } => {
            if is_memory {
                if in_tension || elevated_memory {
                    Resolution::L2
                } else {
                    Resolution::L1
                }
            } else if in_tension {
                Resolution::L1
            } else {
                Resolution::L0
            }
        }
        Query::TypeFiltered { node_type, .. } => {
            let is_target_type = node.node_type == *node_type;
            if is_memory {
                if elevated_memory {
                    Resolution::L1
                } else {
                    Resolution::L0
                }
            } else if is_target_type {
                Resolution::L2
            } else if in_tension {
                Resolution::L1
            } else {
                Resolution::L0
            }
        }
        Query::List { .. } => Resolution::L0,
    }
}

fn apply_mode_resolution(
    fragments: &mut [Fragment],
    upgrade_data: &HashMap<NodeId, NodeUpgradeData>,
    partition_budget: usize,
    chars_per_token: usize,
) {
    let mut used = 0usize;

    for frag in fragments.iter_mut() {
        let Some(data) = upgrade_data.get(&frag.node_id) else {
            continue;
        };

        let target_res = data.target_resolution;
        let current_res = current_resolution(frag);

        match target_res.cmp(&current_res) {
            std::cmp::Ordering::Less => {
                downgrade_fragment(frag, target_res);
            }
            std::cmp::Ordering::Greater => {
                let cost = upgrade_cost_from_data(frag, data, target_res, chars_per_token);
                if used.saturating_add(cost) <= partition_budget {
                    upgrade_fragment_from_data(frag, data, target_res);
                    used += cost;
                } else {
                    used += fragment_tokens(frag, chars_per_token);
                }
            }
            std::cmp::Ordering::Equal => {
                used += fragment_tokens(frag, chars_per_token);
            }
        }
    }
}

fn current_resolution(frag: &Fragment) -> Resolution {
    if frag.content.is_some() {
        Resolution::L2
    } else if frag.summary.is_some() {
        Resolution::L1
    } else {
        Resolution::L0
    }
}

fn downgrade_fragment(frag: &mut Fragment, target: Resolution) {
    match target {
        Resolution::L0 => {
            frag.summary = None;
            frag.content = None;
        }
        Resolution::L1 => {
            frag.content = None;
        }
        Resolution::L2 => {}
    }
}

fn upgrade_fragment_from_data(frag: &mut Fragment, data: &NodeUpgradeData, target: Resolution) {
    match target {
        Resolution::L1 => {
            if frag.summary.is_none() {
                frag.summary = data.summary.clone();
            }
        }
        Resolution::L2 => {
            if frag.summary.is_none() {
                frag.summary = data.summary.clone();
            }
            if frag.content.is_none() {
                frag.content = Some(data.content.clone());
            }
        }
        Resolution::L0 => {}
    }
}

fn upgrade_cost_from_data(
    frag: &Fragment,
    data: &NodeUpgradeData,
    target: Resolution,
    chars_per_token: usize,
) -> usize {
    let current = current_resolution(frag);
    if target <= current {
        return fragment_tokens(frag, chars_per_token);
    }

    let mut cost = estimate_tokens(&frag.name, chars_per_token);
    if matches!(target, Resolution::L1 | Resolution::L2) {
        if let Some(ref summary) = data.summary {
            cost += estimate_tokens(summary, chars_per_token);
        }
    }
    if matches!(target, Resolution::L2) {
        cost += estimate_tokens(&data.content, chars_per_token);
    }
    cost
}

fn fragment_tokens(frag: &Fragment, chars_per_token: usize) -> usize {
    let mut tokens = estimate_tokens(&frag.name, chars_per_token);
    if let Some(ref summary) = frag.summary {
        tokens += estimate_tokens(summary, chars_per_token);
    }
    if let Some(ref content) = frag.content {
        tokens += estimate_tokens(content, chars_per_token);
    }
    tokens
}

fn recalculate_token_usage(package: &mut ContextPackage, chars_per_token: usize) {
    let identity_used: usize = package
        .identity
        .iter()
        .map(|f| fragment_tokens(f, chars_per_token))
        .sum();
    let knowledge_used: usize = package
        .knowledge
        .iter()
        .map(|f| fragment_tokens(f, chars_per_token))
        .sum();
    let memories_used: usize = package
        .memories
        .iter()
        .map(|f| fragment_tokens(f, chars_per_token))
        .sum();

    package.token_usage.identity_used = identity_used;
    package.token_usage.knowledge_used = knowledge_used;
    package.token_usage.memories_used = memories_used;
    package.token_usage.used = identity_used + knowledge_used + memories_used;
}

impl PartialOrd for Resolution {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Resolution {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn rank(r: &Resolution) -> u8 {
            match r {
                Resolution::L0 => 0,
                Resolution::L1 => 1,
                Resolution::L2 => 2,
            }
        }
        rank(self).cmp(&rank(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_origin() -> Origin {
        Origin {
            peer_id: crate::graph::types::PeerId(0),
            source_kind: crate::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::new("proj-a").expect("valid scope"),
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
            &ScopePath::new("proj-a").expect("valid scope"),
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
        let pkg = assemble_context_package(
            nodes,
            &[],
            &[],
            &HashMap::new(),
            10000,
            4,
            &ScopePath::universal(),
        );
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
        let pkg = assemble_context_package(
            nodes,
            &[],
            &[],
            &HashMap::new(),
            10,
            4,
            &ScopePath::universal(),
        );
        assert!(
            pkg.token_usage.used <= 10 + 1,
            "used {} tokens, budget was 10",
            pkg.token_usage.used
        );
    }

    #[test]
    fn memory_l2_upgrade_requires_summary_budget_when_summary_exists() {
        let mut node = make_node(0, KnowledgeType::Episodic, "n", 0.9);
        node.summary = Some("summary".to_string());
        node.content = "c".to_string();

        let pkg = assemble_context_package(
            vec![node],
            &[],
            &[],
            &HashMap::new(),
            2,
            1,
            &ScopePath::universal(),
        );

        assert_eq!(pkg.memories.len(), 1);
        assert_eq!(pkg.memories[0].summary, None);
        assert_eq!(pkg.memories[0].content, None);
        assert_eq!(pkg.token_usage.used, 1);
        assert_eq!(pkg.token_usage.memories_used, 1);
    }

    #[test]
    fn memory_l2_upgrade_accounts_content_when_no_summary_exists() {
        let mut node = make_node(0, KnowledgeType::Episodic, "n", 0.9);
        node.summary = None;
        node.content = "cc".to_string();

        let pkg = assemble_context_package(
            vec![node],
            &[],
            &[],
            &HashMap::new(),
            3,
            1,
            &ScopePath::universal(),
        );

        assert_eq!(pkg.memories.len(), 1);
        assert_eq!(pkg.memories[0].summary, None);
        assert_eq!(pkg.memories[0].content, Some("cc".to_string()));
        assert_eq!(pkg.token_usage.used, 3);
        assert_eq!(pkg.token_usage.memories_used, 3);
    }

    #[test]
    fn tensions_populated_for_activated_contradictions() {
        let a = NodeId(0);
        let b = NodeId(1);
        let mut activations = HashMap::new();
        activations.insert(a, 0.8);
        activations.insert(b, 0.6);

        let contradicts = vec![(a, b, 0.9)];
        let pkg = assemble_context_package(
            vec![],
            &[],
            &contradicts,
            &activations,
            10000,
            4,
            &ScopePath::universal(),
        );
        assert_eq!(pkg.tensions.len(), 1);
        assert_eq!(pkg.tensions[0].node_a, a);
        assert_eq!(pkg.tensions[0].node_b, b);
    }

    #[test]
    fn agent_tension_zero_without_contradictions() {
        let pkg = assemble_context_package(
            vec![],
            &[],
            &[],
            &HashMap::new(),
            10000,
            4,
            &ScopePath::universal(),
        );
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
        let pkg = assemble_context_package(
            nodes,
            &[],
            &[],
            &HashMap::new(),
            10000,
            4,
            &ScopePath::universal(),
        );
        assert_eq!(pkg.knowledge.len(), 3);
        assert!(pkg.knowledge[0].relevance >= pkg.knowledge[1].relevance);
        assert!(pkg.knowledge[1].relevance >= pkg.knowledge[2].relevance);
    }

    #[test]
    fn determine_scope_same_project() {
        let query_scope = ScopePath::new("proj-a").expect("valid scope");
        let node_scope = ScopePath::new("proj-a").expect("valid scope");
        let scope = determine_scope(&query_scope, &node_scope);
        assert_eq!(scope, ScopeRelation::Exact);
    }

    #[test]
    fn determine_scope_universal() {
        let query_scope = ScopePath::new("proj-a").expect("valid scope");
        let scope = determine_scope(&query_scope, &ScopePath::universal());
        assert_eq!(scope, ScopeRelation::Universal);
    }

    #[test]
    fn determine_scope_other_project() {
        let query_scope = ScopePath::new("proj-a").expect("valid scope");
        let node_scope = ScopePath::new("proj-b").expect("valid scope");
        let scope = determine_scope(&query_scope, &node_scope);
        assert_eq!(scope, ScopeRelation::Unrelated);
    }
}
