//! Server-side derived enrichment for `/api/graph`: community `cluster` ids
//! (Leiden community detection, union-find-free — see [`cluster_ids`]) and
//! `doi` (degree-of-interest) scores (see [`doi_scores`]), both threaded into
//! `GraphNodeDto` by [`super::graph::render_graph`].
//!
//! Deliberately skips PCA/2D projection: the graph-viz consumer
//! (`3d-force-graph`) runs its own 3D force layout client-side, so
//! server-computed coordinates would be discarded on arrival anyway.

use std::collections::HashMap;

use anamnesis::graph::NodeId;
use anamnesis::memory::Subgraph;
use leiden_rs::{GraphDataBuilder, Leiden, LeidenConfig, Partition};

/// Below this node count, Leiden is skipped and every node gets cluster `0`
/// — community detection is noise on graphs this small.
const CLUSTER_MIN_NODES: usize = 8;

/// Fixed RNG seed so identical subgraphs always partition identically
/// (asserted by `clusters_are_stable`).
const LEIDEN_SEED: u64 = 42;

const DOI_SALIENCE_WEIGHT: f64 = 1.0;
const DOI_RECENCY_WEIGHT: f64 = 0.5;
const DOI_DEPTH_WEIGHT: f64 = 0.3;
const DOI_SEED_BONUS: f64 = 1.0;

/// Exponential-decay time constant (days) for [`recency_score`].
const RECENCY_DECAY_DAYS: f64 = 14.0;
const MS_PER_DAY: f64 = 86_400_000.0;

/// Community id per node, via Leiden over the subgraph's induced edges.
///
/// Below [`CLUSTER_MIN_NODES`], or if the solver errors on this input, every
/// node is assigned cluster `0` — a deliberate hybrid-by-size fallback, not
/// a silently-broken feature.
pub fn cluster_ids(sub: &Subgraph) -> HashMap<NodeId, u32> {
    if sub.nodes.len() < CLUSTER_MIN_NODES {
        return sub.nodes.iter().map(|n| (n.id, 0)).collect();
    }

    // `sub.nodes`'s own iteration order is not guaranteed stable across
    // calls (the BFS that built it may traverse a `HashMap`-backed
    // adjacency index), so dense indices are assigned by sorted `NodeId`
    // rather than by that order — otherwise the same real node could land
    // at a different dense index on each call, making Leiden's output
    // nondeterministic even with edges sorted and a fixed seed.
    let mut sorted_ids: Vec<NodeId> = sub.nodes.iter().map(|n| n.id).collect();
    sorted_ids.sort_by_key(|id| id.0);
    let index_of: HashMap<NodeId, usize> = sorted_ids
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();

    match run_leiden(sub, &index_of) {
        Some(partition) => sub
            .nodes
            .iter()
            .map(|n| (n.id, partition.community_of(index_of[&n.id]) as u32))
            .collect(),
        None => sub.nodes.iter().map(|n| (n.id, 0)).collect(),
    }
}

/// Build the induced [`leiden_rs::GraphData`] and run Leiden; `None` on any
/// builder/solver error so the caller falls back to cluster `0`.
///
/// Edges are sorted into a canonical `(src, dst)` order before insertion:
/// `sub.edges`'s iteration order is not guaranteed stable across calls, and
/// Leiden's local-moving phase is insertion-order-sensitive even with a
/// fixed seed, so an unsorted edge list would make `cluster_ids` output
/// nondeterministic for the identical input graph.
fn run_leiden(sub: &Subgraph, index_of: &HashMap<NodeId, usize>) -> Option<Partition> {
    let mut edges: Vec<(usize, usize, f64)> = sub
        .edges
        .iter()
        .filter_map(|e| {
            let src = *index_of.get(&e.source)?;
            let dst = *index_of.get(&e.target)?;
            if src == dst {
                return None;
            }
            let weight = if e.weight.is_finite() && e.weight >= 0.0 {
                e.weight
            } else {
                1.0
            };
            let (lo, hi) = (src.min(dst), src.max(dst));
            Some((lo, hi, weight))
        })
        .collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.total_cmp(&b.2)));

    let mut builder = GraphDataBuilder::new(sub.nodes.len());
    for (src, dst, weight) in edges {
        builder.add_edge(src, dst, weight).ok()?;
    }
    let graph = builder.build().ok()?;
    let config = LeidenConfig {
        seed: Some(LEIDEN_SEED),
        ..LeidenConfig::default()
    };
    Leiden::new(config).run(&graph).ok().map(|out| out.partition)
}

/// Degree-of-interest per node: `w_s*salience + w_r*recency - w_d*depth`,
/// plus [`DOI_SEED_BONUS`] for depth-0 seeds. Pure — no solver, no I/O.
pub fn doi_scores(sub: &Subgraph, now_ms: u64) -> HashMap<NodeId, f64> {
    let depth_by_id: HashMap<NodeId, usize> = sub.depths.iter().copied().collect();
    sub.nodes
        .iter()
        .map(|n| {
            let depth = depth_by_id.get(&n.id).copied().unwrap_or(0);
            let recency = recency_score(n.created_at.0, now_ms);
            let seed_bonus = if depth == 0 { DOI_SEED_BONUS } else { 0.0 };
            let score = DOI_SALIENCE_WEIGHT * n.salience + DOI_RECENCY_WEIGHT * recency
                - DOI_DEPTH_WEIGHT * (depth as f64)
                + seed_bonus;
            (n.id, score)
        })
        .collect()
}

/// Maps node age to `[0, 1]` (newer is higher) via exponential decay over
/// days. Nodes stamped after `now_ms` (clock skew) are clamped to age `0`.
fn recency_score(created_at_ms: u64, now_ms: u64) -> f64 {
    let age_days = now_ms.saturating_sub(created_at_ms) as f64 / MS_PER_DAY;
    (-age_days / RECENCY_DECAY_DAYS).exp()
}
