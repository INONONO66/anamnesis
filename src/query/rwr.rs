//! Additive directed Random-Walk-with-Restart over the conductance graph.
//!
//! This is the authoritative retrieval flow (ADR-0005,
//! [activation-flow.md](../../docs/05-context-retrieval/activation-flow.md)). It is
//! **read-only**: it never mutates retained action, conductance, salience, edge
//! weight, timestamps, or trust.
//!
//! For each site, outgoing edges are normalized into a row-stochastic transition
//! matrix over *projected conductance* scaled by a within-row edge-type factor:
//!
//! ```text
//! g_ij  = project_conductance(C_ij) * edge_type_factor_ij
//! P(i,j) = g_ij / sum_k g_ik
//! I_ij  = a_i * g_ij                      (path current)
//! a_next = alpha * seed(Q) + (1 - alpha) * transpose(P) * a
//! ```
//!
//! `Contradicts` edges are **excluded** from `P` (routed to frustration). Multiple
//! incoming paths are *summed*, never maxed: posterior log-odds add log-LR evidence.
//! Because `seed` is L1-normalized and `P` is row-stochastic, the operator has
//! contraction modulus `(1 - alpha) < 1` and converges geometrically to a unique
//! fixed point.

use std::collections::HashMap;

use crate::graph::{EdgeId, EdgeType, NodeId, Timestamp};
use crate::mechanics::priors::{
    self, RWR_MAX_ITERATIONS, RWR_TOLERANCE, edge_type_factor, project_conductance, restart_alpha,
};
use crate::query::activation::edge_valid_at;
use crate::storage::StorageAdapter;

/// Edge-keyed path-current map `I_ij = a_i * g_ij` captured at the settled response.
pub type PathCurrentMap = HashMap<EdgeId, f64>;

/// The settled transient response of an additive RWR flow.
///
/// All quantities are query-local and transient; none is persisted.
#[derive(Debug, Clone, Default)]
pub struct ActivationResponse {
    /// Site id → transient activation response `a*`.
    pub activation: HashMap<NodeId, f64>,
    /// Edge id → path current `I_ij` at the settled response.
    pub path_current: PathCurrentMap,
    /// Site id → effective impedance `Z_i` (approximate access cost from the field).
    pub impedance: HashMap<NodeId, f64>,
    /// Number of iterations performed.
    pub iterations: usize,
    /// Final residual `||a_next - a||_1`.
    pub residual: f64,
    /// Whether the iteration bound stopped convergence.
    pub truncated: bool,
    /// Edges split off to frustration (excluded `Contradicts`).
    pub excluded_edges: Vec<EdgeId>,
}

/// One outgoing transition from a source row of the conductance matrix.
struct Transition {
    target: NodeId,
    edge_id: EdgeId,
    /// `g_ij = project_conductance(C_ij) * edge_type_factor_ij`.
    conductance: f64,
}

/// Runs the additive directed RWR from a restart distribution, deriving `alpha`
/// from the mean associative-reach prior `L`.
///
/// The restart distribution must be an L1-normalized seed; this function
/// renormalizes defensively. Returns the settled [`ActivationResponse`].
pub fn additive_rwr<S: StorageAdapter>(
    seed: &HashMap<NodeId, f64>,
    storage: &S,
    now: Timestamp,
) -> ActivationResponse {
    additive_rwr_with_alpha(seed, restart_alpha(priors::MEAN_ASSOCIATIVE_REACH_L), storage, now)
}

/// Runs the additive directed RWR with an explicit restart rate `alpha`.
pub fn additive_rwr_with_alpha<S: StorageAdapter>(
    seed: &HashMap<NodeId, f64>,
    alpha: f64,
    storage: &S,
    now: Timestamp,
) -> ActivationResponse {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        restart_alpha(priors::MEAN_ASSOCIATIVE_REACH_L)
    };

    // Stable, deterministic node iteration order.
    let mut node_ids: Vec<NodeId> = storage
        .all_node_ids()
        .into_iter()
        .filter(|id| storage.get_node(*id).is_ok())
        .collect();
    node_ids.sort_by_key(|id| id.0);

    if node_ids.is_empty() {
        return ActivationResponse::default();
    }

    let restart = normalize_seed(seed, &node_ids, storage);
    if restart.is_empty() {
        return ActivationResponse::default();
    }
    // Deterministic, sorted view of the restart distribution. Iterating the
    // `HashMap` directly would expose hash-seed-dependent ordering, and f64 addition
    // is not associative, so the restart/dangling redistribution must accumulate in
    // a stable order to satisfy the determinism MUST (ADR-0004: same graph + query
    // => identical result).
    let restart_sorted: Vec<(NodeId, f64)> = {
        let mut v: Vec<(NodeId, f64)> = restart.iter().map(|(&id, &m)| (id, m)).collect();
        v.sort_by_key(|(id, _)| id.0);
        v
    };

    // Precompute the transition rows once (read-only graph): for each source,
    // its outgoing transitions over projected conductance, the row sum, and the
    // excluded Contradicts edges.
    let mut rows: HashMap<NodeId, Vec<Transition>> = HashMap::with_capacity(node_ids.len());
    let mut excluded_edges: Vec<EdgeId> = Vec::new();

    for &source in &node_ids {
        let mut transitions: Vec<Transition> = Vec::new();
        collect_transitions(
            source,
            storage,
            now,
            &mut transitions,
            &mut excluded_edges,
        );
        if !transitions.is_empty() {
            rows.insert(source, transitions);
        }
    }
    excluded_edges.sort_by_key(|e| e.0);
    excluded_edges.dedup();

    // Iterate: a_next = alpha * seed + (1 - alpha) * transpose(P) * a.
    let mut current = restart.clone();
    let mut iterations = 0usize;
    let mut residual = 0.0;
    let mut truncated = true;

    for _ in 0..RWR_MAX_ITERATIONS {
        iterations += 1;
        let mut next: HashMap<NodeId, f64> = HashMap::with_capacity(node_ids.len());

        // Restart contribution.
        for (id, mass) in &restart_sorted {
            add_mass(&mut next, *id, alpha * *mass);
        }

        // Propagation contribution. Dangling rows (no outgoing transitions) return
        // their walk mass to the restart distribution, preserving total mass.
        for &source in &node_ids {
            let a_i = current.get(&source).copied().unwrap_or(0.0);
            if !a_i.is_finite() || a_i <= 0.0 {
                continue;
            }
            let walk_mass = (1.0 - alpha) * a_i;
            if walk_mass <= 0.0 {
                continue;
            }

            match rows.get(&source) {
                Some(transitions) => {
                    let row_sum: f64 = transitions.iter().map(|t| t.conductance).sum();
                    if !row_sum.is_finite() || row_sum <= 0.0 {
                        for (id, mass) in &restart_sorted {
                            add_mass(&mut next, *id, walk_mass * *mass);
                        }
                        continue;
                    }
                    for t in transitions {
                        // P(i,j) = g_ij / sum_k g_ik
                        add_mass(&mut next, t.target, walk_mass * t.conductance / row_sum);
                    }
                }
                None => {
                    for (id, mass) in &restart_sorted {
                        add_mass(&mut next, *id, walk_mass * *mass);
                    }
                }
            }
        }

        residual = l1_delta(&current, &next, &node_ids);
        current = next;

        if residual < RWR_TOLERANCE {
            truncated = false;
            break;
        }
    }

    // Path currents at the settled response: I_ij = a_i * g_ij. Iterate sources in
    // deterministic sorted order: an edge appears as a transition from both of its
    // endpoints (forward and backward), so the final `path_current[edge]` is the
    // last writer — the source order must be stable to satisfy the determinism MUST
    // (ADR-0004). `node_ids` is already sorted; the higher-id endpoint wins.
    let mut path_current: PathCurrentMap = HashMap::new();
    for &source in &node_ids {
        let Some(transitions) = rows.get(&source) else {
            continue;
        };
        let a_i = current.get(&source).copied().unwrap_or(0.0);
        if !a_i.is_finite() || a_i <= 0.0 {
            continue;
        }
        for t in transitions {
            let current_value = a_i * t.conductance;
            if current_value.is_finite() && current_value > 0.0 {
                path_current.insert(t.edge_id, current_value);
            }
        }
    }

    // Effective impedance Z_i: access cost from the query field is the negative
    // log of the response (a probability-like quantity). A site that receives no
    // current is maximally impeded; one fully reached has Z -> 0. Approximated
    // from the settled response, deterministic and finite.
    let mut impedance: HashMap<NodeId, f64> = HashMap::with_capacity(current.len());
    for (&id, &a) in &current {
        impedance.insert(id, effective_impedance(a));
    }

    current.retain(|id, score| score.is_finite() && *score > 0.0 && storage.get_node(*id).is_ok());

    ActivationResponse {
        activation: current,
        path_current,
        impedance,
        iterations,
        residual,
        truncated,
        excluded_edges,
    }
}

/// Effective impedance `Z_i = -ln(a_i)` clamped to a finite non-negative range.
///
/// Higher activation = lower impedance (cheaper to reach). A zero/absent response
/// maps to the [`LOG_ODDS_CLAMP`](crate::mechanics::priors) ceiling.
fn effective_impedance(activation: f64) -> f64 {
    if !activation.is_finite() || activation <= 0.0 {
        return priors::LOG_ODDS_CLAMP;
    }
    (-activation.ln()).clamp(0.0, priors::LOG_ODDS_CLAMP)
}

/// Collects the conductance-weighted outgoing transitions for one source row.
///
/// Both outgoing (`source → target`, forward) and incoming (`target → source`,
/// backward) edges are traversed, with `Supersedes` direction respected via
/// `edge_type_factor`. `Contradicts` edges are excluded and reported.
fn collect_transitions<S: StorageAdapter>(
    source: NodeId,
    storage: &S,
    now: Timestamp,
    transitions: &mut Vec<Transition>,
    excluded: &mut Vec<EdgeId>,
) {
    let mut edge_ids: Vec<EdgeId> = Vec::new();
    edge_ids.extend(storage.edges_from(source).iter().copied());
    edge_ids.extend(storage.edges_to(source).iter().copied());
    edge_ids.sort_by_key(|e| e.0);
    edge_ids.dedup();

    for edge_id in edge_ids {
        let Ok(edge) = storage.get_edge(edge_id) else {
            continue;
        };
        if !edge_valid_at(edge, now) {
            continue;
        }
        if matches!(edge.edge_type, EdgeType::Contradicts) {
            excluded.push(edge_id);
            continue;
        }

        // Determine direction and the neighbour reached.
        let (neighbour, is_forward) = if edge.source == source {
            (edge.target, true)
        } else if edge.target == source {
            (edge.source, false)
        } else {
            continue;
        };
        if storage.get_node(neighbour).is_err() {
            continue;
        }

        let g = transition_conductance(edge.conductance, &edge.edge_type, is_forward);
        if g.is_finite() && g > 0.0 {
            transitions.push(Transition {
                target: neighbour,
                edge_id,
                conductance: g,
            });
        }
    }
}

/// `g_ij = project_conductance(C_ij) * edge_type_factor_ij`, as a positive
/// within-row conductance (activation-flow.md). [`project_conductance`] maps the
/// unbounded log-LR reservoir to a strictly positive `(0, 1)` conductance, so row
/// normalization `P(i,j) = g_ij / sum_k g_ik` is well-defined and `P` stays
/// row-stochastic for every finite reservoir (including negative log-LR).
fn transition_conductance(conductance: f64, edge_type: &EdgeType, is_forward: bool) -> f64 {
    let factor = edge_type_factor(edge_type, is_forward);
    if !factor.is_finite() || factor <= 0.0 {
        return 0.0;
    }
    let g = project_conductance(conductance) * factor;
    if g.is_finite() && g > 0.0 { g } else { 0.0 }
}

/// L1-normalizes the seed restart distribution over live nodes.
fn normalize_seed<S: StorageAdapter>(
    seed: &HashMap<NodeId, f64>,
    node_ids: &[NodeId],
    storage: &S,
) -> HashMap<NodeId, f64> {
    let _ = node_ids;
    let mut filtered: HashMap<NodeId, f64> = HashMap::new();
    for (&id, &mass) in seed {
        if mass.is_finite() && mass > 0.0 && storage.get_node(id).is_ok() {
            *filtered.entry(id).or_insert(0.0) += mass;
        }
    }
    let sum: f64 = filtered.values().sum();
    if !sum.is_finite() || sum <= f64::EPSILON {
        return HashMap::new();
    }
    for mass in filtered.values_mut() {
        *mass /= sum;
    }
    filtered
}

fn add_mass(distribution: &mut HashMap<NodeId, f64>, node_id: NodeId, mass: f64) {
    if mass.is_finite() && mass > 0.0 {
        *distribution.entry(node_id).or_insert(0.0) += mass;
    }
}

fn l1_delta(current: &HashMap<NodeId, f64>, next: &HashMap<NodeId, f64>, node_ids: &[NodeId]) -> f64 {
    node_ids
        .iter()
        .map(|id| {
            let a = current.get(id).copied().unwrap_or(0.0);
            let b = next.get(id).copied().unwrap_or(0.0);
            (a - b).abs()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::mechanics::priors::project_weight;
    use crate::graph::edge::EdgeSource;
    use crate::graph::node::Origin;
    use crate::graph::{Edge, KnowledgeType, MemoryTier, Node, ScopePath};
    use crate::peer::SourceKind;
    use crate::storage::SqliteStorage;

    fn origin() -> Origin {
        Origin {
            peer_id: crate::graph::types::PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        }
    }

    fn node(id: u64) -> Node {
        Node {
            id: NodeId(id),
            node_type: KnowledgeType::Semantic,
            name: format!("n{id}"),
            summary: None,
            content: format!("content {id}"),
            embedding: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            accessed_at: Timestamp(0),
            valid_from: None,
            valid_until: None,
            salience: project_weight(crate::mechanics::priors::INITIAL_RETAINED_ACTION),
            retained_action: crate::mechanics::priors::INITIAL_RETAINED_ACTION,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: origin(),
            entity_tags: vec![],
            metadata: std::collections::HashMap::new(),
        }
    }

    fn edge(id: u64, src: u64, tgt: u64, et: EdgeType, conductance: f64) -> Edge {
        Edge {
            id: EdgeId(id),
            source: NodeId(src),
            target: NodeId(tgt),
            edge_type: et,
            weight: project_weight(conductance),
            conductance,
            edge_source: EdgeSource::Manual,
            created_at: Timestamp(0),
            accessed_at: Timestamp(0),
            valid_from: None,
            valid_until: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    fn store(nodes: &[u64], edges: Vec<Edge>) -> SqliteStorage {
        let mut storage = SqliteStorage::new().expect("sqlite");
        for &id in nodes {
            storage.set_node(node(id)).expect("set node");
        }
        for e in edges {
            storage.set_edge(e).expect("set edge");
        }
        storage
    }

    #[test]
    fn empty_graph_returns_empty() {
        let storage = SqliteStorage::new().expect("sqlite");
        let r = additive_rwr(&HashMap::from([(NodeId(0), 1.0)]), &storage, Timestamp(0));
        assert!(r.activation.is_empty());
    }

    #[test]
    fn converges_and_conserves_mass() {
        let storage = store(
            &[0, 1, 2],
            vec![
                edge(0, 0, 1, EdgeType::Semantic, 2.0),
                edge(1, 1, 2, EdgeType::Semantic, 2.0),
            ],
        );
        let r = additive_rwr(&HashMap::from([(NodeId(0), 1.0)]), &storage, Timestamp(0));
        assert!(!r.truncated, "should converge within bound");
        // Seed mass is L1-normalized and P is row-stochastic, so total response
        // mass is conserved at 1.0.
        let total: f64 = r.activation.values().sum();
        assert!((total - 1.0).abs() < 1e-7, "mass not conserved: {total}");
        assert!(r.activation.values().all(|v| v.is_finite() && *v > 0.0));
    }

    #[test]
    fn restart_concentrates_on_seed_in_directed_chain() {
        // In a pure directed-out chain the seed is the only node receiving restart
        // mass, so it holds the largest single share at low alpha-reach.
        let storage = store(
            &[0, 1, 2, 3],
            vec![
                edge(0, 0, 1, EdgeType::Semantic, 2.0),
                edge(1, 1, 2, EdgeType::Semantic, 2.0),
                edge(2, 2, 3, EdgeType::Semantic, 2.0),
            ],
        );
        // High alpha (short reach) keeps mass near the restart seed.
        let r = additive_rwr_with_alpha(&HashMap::from([(NodeId(0), 1.0)]), 0.6, &storage, Timestamp(0));
        let a0 = r.activation[&NodeId(0)];
        let a3 = r.activation.get(&NodeId(3)).copied().unwrap_or(0.0);
        assert!(a0 > a3, "seed {a0} should exceed far node {a3}");
    }

    #[test]
    fn rows_are_row_stochastic_in_propagation() {
        // Two outgoing edges from the seed; their P-row must sum to 1.
        let storage = store(
            &[0, 1, 2],
            vec![
                edge(0, 0, 1, EdgeType::Semantic, 1.0),
                edge(1, 0, 2, EdgeType::Reason, 3.0),
            ],
        );
        let mut transitions = Vec::new();
        let mut excluded = Vec::new();
        collect_transitions(NodeId(0), &storage, Timestamp(0), &mut transitions, &mut excluded);
        let row_sum: f64 = transitions.iter().map(|t| t.conductance).sum();
        let p: Vec<f64> = transitions.iter().map(|t| t.conductance / row_sum).collect();
        let total: f64 = p.iter().sum();
        assert!((total - 1.0).abs() < 1e-12, "P row must sum to 1, got {total}");
    }

    #[test]
    fn negative_conductance_reservoir_still_yields_row_stochastic_p() {
        // The row-stochasticity invariant (activation-flow.md): `C_ij` is an unbounded
        // log-LR that may be NEGATIVE, yet `g_ij = project_conductance(C_ij) * factor`
        // stays strictly positive because `project_conductance = logistic(C) > 0`. So
        // even an edge with a negative reservoir keeps the P-row a valid distribution.
        let storage = store(
            &[0, 1, 2],
            vec![
                // Negative log-LR reservoir on the first edge — would be non-positive
                // if the raw reservoir were used as a flow weight.
                edge(0, 0, 1, EdgeType::Semantic, -5.0),
                edge(1, 0, 2, EdgeType::Reason, 3.0),
            ],
        );
        let mut transitions = Vec::new();
        let mut excluded = Vec::new();
        collect_transitions(NodeId(0), &storage, Timestamp(0), &mut transitions, &mut excluded);
        // Both edges propagate: g_ij > 0 even for the negative-reservoir edge.
        assert_eq!(transitions.len(), 2, "negative-C edge must still propagate");
        for t in &transitions {
            assert!(t.conductance > 0.0, "g_ij must be > 0, got {}", t.conductance);
        }
        let row_sum: f64 = transitions.iter().map(|t| t.conductance).sum();
        assert!(row_sum > 0.0, "row sum must be a valid positive normalizer");
        let total: f64 = transitions.iter().map(|t| t.conductance / row_sum).sum();
        assert!((total - 1.0).abs() < 1e-12, "P row must sum to 1, got {total}");
        // End-to-end: the negative-reservoir target is reached by propagation, and the
        // full response still conserves mass (row-stochastic P + L1 seed).
        let r = additive_rwr(&HashMap::from([(NodeId(0), 1.0)]), &storage, Timestamp(0));
        assert!(r.activation.get(&NodeId(1)).copied().unwrap_or(0.0) > 0.0);
        let mass: f64 = r.activation.values().sum();
        assert!((mass - 1.0).abs() < 1e-7, "mass not conserved: {mass}");
    }

    #[test]
    fn contradicts_edges_are_excluded() {
        let storage = store(
            &[0, 1],
            vec![edge(0, 0, 1, EdgeType::Contradicts, 5.0)],
        );
        let r = additive_rwr(&HashMap::from([(NodeId(0), 1.0)]), &storage, Timestamp(0));
        assert_eq!(r.excluded_edges, vec![EdgeId(0)]);
        // Node 1 receives no propagation; only restart mass on the seed.
        let a1 = r.activation.get(&NodeId(1)).copied().unwrap_or(0.0);
        assert_eq!(a1, 0.0, "Contradicts target must not be reached by P");
    }

    #[test]
    fn additive_paths_sum() {
        // Diamond: 0 -> 1 -> 3 and 0 -> 2 -> 3. Node 3 gets both paths.
        let storage = store(
            &[0, 1, 2, 3],
            vec![
                edge(0, 0, 1, EdgeType::Semantic, 2.0),
                edge(1, 0, 2, EdgeType::Semantic, 2.0),
                edge(2, 1, 3, EdgeType::Semantic, 2.0),
                edge(3, 2, 3, EdgeType::Semantic, 2.0),
            ],
        );
        let r = additive_rwr(&HashMap::from([(NodeId(0), 1.0)]), &storage, Timestamp(0));
        assert!(r.activation.get(&NodeId(3)).copied().unwrap_or(0.0) > 0.0);
    }

    #[test]
    fn idempotent_same_graph_same_query() {
        let storage = store(
            &[0, 1, 2, 3],
            vec![
                edge(0, 0, 1, EdgeType::Semantic, 2.0),
                edge(1, 1, 2, EdgeType::Reason, 1.0),
                edge(2, 2, 3, EdgeType::Temporal, 0.5),
            ],
        );
        let seed = HashMap::from([(NodeId(0), 1.0)]);
        let a = additive_rwr(&seed, &storage, Timestamp(0));
        let b = additive_rwr(&seed, &storage, Timestamp(0));
        assert_eq!(a.activation, b.activation);
        assert_eq!(a.iterations, b.iterations);
    }

    #[test]
    fn path_current_captured() {
        let storage = store(&[0, 1], vec![edge(0, 0, 1, EdgeType::Semantic, 2.0)]);
        let r = additive_rwr(&HashMap::from([(NodeId(0), 1.0)]), &storage, Timestamp(0));
        assert!(r.path_current.contains_key(&EdgeId(0)));
    }

    #[test]
    fn impedance_decreases_with_activation() {
        assert!(effective_impedance(0.5) < effective_impedance(0.1));
        assert_eq!(effective_impedance(0.0), priors::LOG_ODDS_CLAMP);
    }
}
