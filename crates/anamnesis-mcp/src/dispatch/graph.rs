//! Canonical graph-viz JSON rendering for `dispatch`'s `Request::Graph` arm.
//!
//! [`render_graph`] maps an [`anamnesis::memory::Subgraph`] (bounded k-hop
//! export from [`anamnesis::Memory::subgraph`]) into the wire DTO a graph-viz
//! consumer renders directly: `{schema, seed_ids, truncated, nodes, edges}`.
//! Split out the same way [`super::render`] is: a pure formatting layer with
//! no registry/locking concerns of its own.

use std::collections::HashMap;

use anamnesis::graph::{MemoryTier, NodeId};
use anamnesis::memory::Subgraph;
use serde::Serialize;

use crate::memory;

/// Salience below which a node's display tier is [`MemoryTier::Archival`],
/// mirroring the private threshold `anamnesis::api::salience_tier` uses
/// (that function is `pub(crate)` to the engine crate, unreachable from
/// here, so this crate keeps its own copy of the same band).
const ARCHIVE_SALIENCE_THRESHOLD: f64 = 0.10;
/// Salience above which a node's display tier is [`MemoryTier::Core`].
const CORE_SALIENCE_THRESHOLD: f64 = 0.80;

/// One node in the canonical graph JSON payload.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphNodeDto {
    pub id: u64,
    pub label: String,
    pub r#type: String,
    pub salience: f64,
    pub depth: usize,
    pub tier: String,
    pub created_at: u64,
    pub entity_tags: Vec<String>,
    pub retracted: bool,
}

/// One edge in the canonical graph JSON payload.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphEdgeDto {
    pub id: u64,
    pub source: u64,
    pub target: u64,
    pub r#type: String,
    pub weight: f64,
}

/// The full canonical graph JSON payload `dispatch`'s `Request::Graph` arm
/// returns via [`render_graph`].
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphSubgraphDto {
    pub schema: u32,
    pub seed_ids: Vec<u64>,
    pub truncated: bool,
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<GraphEdgeDto>,
}

/// Salience-derived display tier label, computed the same way
/// [`anamnesis::memory::MemoryView::tier`] is derived internally
/// (`format!("{:?}", tier)` over the same salience bands), so a graph node
/// and its `get`/`list` view report the identical band for the same
/// salience. Never serializes the raw `Node` embedding or access-history
/// reservoirs — only this derived label.
fn tier_label(salience: f64) -> String {
    let tier = if salience < ARCHIVE_SALIENCE_THRESHOLD {
        MemoryTier::Archival
    } else if salience > CORE_SALIENCE_THRESHOLD {
        MemoryTier::Core
    } else {
        MemoryTier::Recall
    };
    format!("{tier:?}")
}

/// Render a bounded-BFS [`Subgraph`] plus its resolved `seed_ids` into the
/// canonical wire JSON. `schema` is fixed at `1`; bump it (and document the
/// change) if the DTO shape ever changes in a consumer-visible way.
///
/// Never serializes `Node::embedding` or `Node::access_history` — only the
/// display-oriented fields a graph-viz consumer needs (label, type, salience,
/// depth, tier, timestamps, entity tags, retracted).
pub(crate) fn render_graph(sub: &Subgraph, seed_ids: &[u64]) -> String {
    let depth_by_id: HashMap<NodeId, usize> = sub.depths.iter().copied().collect();

    let nodes = sub
        .nodes
        .iter()
        .map(|n| GraphNodeDto {
            id: n.id.0,
            label: n.name.clone(),
            r#type: memory::knowledge_type_label(&n.node_type),
            salience: n.salience,
            depth: depth_by_id.get(&n.id).copied().unwrap_or(0),
            tier: tier_label(n.salience),
            created_at: n.created_at.0,
            entity_tags: n.entity_tags.clone(),
            retracted: n.metadata.get("retracted").is_some_and(|v| v == "true"),
        })
        .collect();

    let edges = sub
        .edges
        .iter()
        .map(|e| GraphEdgeDto {
            id: e.id.0,
            source: e.source.0,
            target: e.target.0,
            r#type: memory::edge_type_label(&e.edge_type),
            weight: e.weight,
        })
        .collect();

    let dto = GraphSubgraphDto {
        schema: 1,
        seed_ids: seed_ids.to_vec(),
        truncated: sub.truncated,
        nodes,
        edges,
    };
    serde_json::to_string(&dto).unwrap_or_else(|_| "{}".to_string())
}
