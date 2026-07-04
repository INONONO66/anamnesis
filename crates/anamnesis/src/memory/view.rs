//! View types for the [`Memory`](super::Memory) management API.
//!
//! [`MemoryView`] is a read-only projection of a graph [`Node`], returned by
//! [`Memory::get`](super::Memory::get) and [`Memory::list`](super::Memory::list).
//! [`ListFilter`] configures `list`'s salience/type/tag narrowing and ordering.

use std::collections::HashMap;

use crate::graph::node::Node;
use crate::graph::{KnowledgeType, MemoryTier, NodeId, Timestamp};

/// A read-only snapshot of a single memory node.
///
/// Returned by [`Memory::get`](super::Memory::get) and
/// [`Memory::list`](super::Memory::list). Exposes the fields a management
/// consumer needs without handing out the full internal [`Node`]
/// representation (access-history reservoirs, etc. are omitted); provenance
/// (`peer_id`/`session_id`/`scope`/`confidence`, projected from
/// [`Origin`](crate::graph::Origin)) is surfaced so a consumer can attribute
/// each memory to the agent/session/scope that produced it — the attribution
/// foundation for multi-agent/team memory. Multi-writer merge and
/// trust-weighting remain roadmap.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive] // may gain fields; keep additive
pub struct MemoryView {
    /// Id of the node this view was read from.
    pub node_id: NodeId,
    /// Full content (L2 — source of truth).
    pub content: String,
    /// Consumer-defined metadata key-value pairs.
    pub metadata: HashMap<String, String>,
    /// Entity tags for cross-node linking.
    pub entity_tags: Vec<String>,
    /// Current salience projection `[0, 1]`.
    pub salience: f64,
    /// Salience-derived display tier (Archival/Recall/Core) — the same band
    /// the engine's own `TierTransition` reporting uses, computed fresh from
    /// [`salience`](Self::salience) rather than the stored `node.tier` (which
    /// is always `Auto` in production; see [`crate::graph::MemoryTier`]).
    pub tier: MemoryTier,
    /// Knowledge type classification.
    pub node_type: KnowledgeType,
    /// Creation timestamp (immutable after creation).
    pub created_at: Timestamp,
    /// Last-modified timestamp.
    pub updated_at: Timestamp,
    /// When the fact represented by this node became valid, if bounded.
    pub valid_from: Option<Timestamp>,
    /// When the fact became invalid, if bounded (set by a `Supersedes` edge).
    pub valid_until: Option<Timestamp>,
    /// Whether the node is currently retracted (see
    /// [`Memory::forget`](super::Memory::forget)).
    pub retracted: bool,
    /// Stringified `origin.peer_id` — the writer that produced this node.
    pub peer_id: String,
    /// `origin.session_id` — the session that produced this node.
    pub session_id: String,
    /// Canonical `origin.scope` string (`ScopePath::as_str()`).
    pub scope: String,
    /// `origin.confidence` `[0, 1]` at write time.
    pub confidence: f64,
}

/// Filter and ordering knobs for [`Memory::list`](super::Memory::list).
///
/// Results are ordered by salience, highest first. All set filters are
/// additive (AND-combined).
#[derive(Debug, Clone)]
pub struct ListFilter {
    /// Minimum salience `[0, 1]` a node must have to be included.
    pub min_salience: f64,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Restrict to a single [`KnowledgeType`], if set.
    pub node_type: Option<KnowledgeType>,
    /// Restrict to nodes carrying this entity tag, if set.
    pub tag: Option<String>,
    /// Restrict to nodes whose origin scope matches this string (compared
    /// against the canonical `ScopePath::as_str()`), if set.
    pub scope: Option<String>,
    /// Restrict to nodes carrying this metadata `(key, value)` pair
    /// (exact-match on both), if set.
    pub metadata: Option<(String, String)>,
}

impl Default for ListFilter {
    /// No salience floor, no type/tag/scope/metadata narrowing, capped at
    /// 100 results.
    fn default() -> Self {
        Self {
            min_salience: 0.0,
            limit: 100,
            node_type: None,
            tag: None,
            scope: None,
            metadata: None,
        }
    }
}

/// Project a [`Node`] into its read-only [`MemoryView`].
pub(super) fn node_to_view(node: &Node) -> MemoryView {
    MemoryView {
        node_id: node.id,
        content: node.content.clone(),
        metadata: node.metadata.clone(),
        entity_tags: node.entity_tags.clone(),
        salience: node.salience,
        tier: crate::api::salience_tier(node.salience),
        node_type: node.node_type.clone(),
        created_at: node.created_at,
        updated_at: node.updated_at,
        valid_from: node.valid_from,
        valid_until: node.valid_until,
        retracted: node.metadata.get("retracted").is_some_and(|v| v == "true"),
        peer_id: node.origin.peer_id.0.to_string(),
        session_id: node.origin.session_id.clone(),
        scope: node.origin.scope.as_str().to_string(),
        confidence: node.origin.confidence,
    }
}
