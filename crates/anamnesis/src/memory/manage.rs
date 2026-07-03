//! Memory-management API — update, read, list, forget/unforget, hard delete,
//! and supersede for individual nodes.
//!
//! See the [module docs](super) for the front-door contract. These methods
//! wrap engine primitives that already exist and are independently tested:
//! [`Engine::retract`](crate::api::Engine::retract) /
//! [`Engine::unretract`](crate::api::Engine::unretract) and the
//! `Supersedes` edge type's validity-window side effect in
//! [`Engine::link`](crate::api::Engine::link).

use crate::error::Error;
use crate::graph::{EdgeId, NodeId, Timestamp};
use crate::storage::StorageAdapter;

use super::view::{ListFilter, MemoryView, node_to_view};
use super::{Memory, Relation, embed_one};

impl<S: StorageAdapter + Clone> Memory<S> {
    /// Replace a node's content and re-embed it via the same provider
    /// `Memory` holds — mirrors how [`Memory::add_note`] produces its
    /// embedding.
    ///
    /// Write-through: the full node row is persisted via `set_node`
    /// (`flush()` does not carry `content`/`embedding`, so a bare hot-field
    /// write would be lost on reopen).
    ///
    /// # Errors
    ///
    /// Returns an error if `id` does not exist or the provider fails to
    /// embed `new_content`.
    pub fn update_content(
        &mut self,
        id: NodeId,
        new_content: &str,
        at: Timestamp,
    ) -> Result<(), Error> {
        let embedding = embed_one(&*self.provider, new_content)?;
        let mut node = self.engine.graph().get_node(id)?.clone();
        node.content = new_content.to_string();
        node.embedding = Some(embedding);
        node.updated_at = at;
        self.engine.graph_mut().storage_mut().set_node(node)
    }

    /// Read a single node as a [`MemoryView`].
    ///
    /// # Errors
    ///
    /// Returns an error if `id` does not exist.
    pub fn get(&self, id: NodeId) -> Result<MemoryView, Error> {
        let node = self.engine.graph().get_node(id)?;
        Ok(node_to_view(node))
    }

    /// List nodes matching `filter`, ordered by salience (highest first).
    ///
    /// `Ok` is always returned unless a storage-level inconsistency makes a
    /// listed node id unreadable; such nodes are silently skipped rather than
    /// failing the whole listing.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<MemoryView>, Error> {
        let mut views: Vec<MemoryView> = self
            .engine
            .graph()
            .all_node_ids()
            .into_iter()
            .filter_map(|id| self.engine.graph().get_node(id).ok())
            .filter(|node| node.salience >= filter.min_salience)
            .filter(|node| {
                filter
                    .node_type
                    .as_ref()
                    .is_none_or(|nt| &node.node_type == nt)
            })
            .filter(|node| {
                filter
                    .tag
                    .as_ref()
                    .is_none_or(|tag| node.entity_tags.iter().any(|t| t == tag))
            })
            .map(node_to_view)
            .collect();
        views.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        views.truncate(filter.limit);
        Ok(views)
    }

    /// Soft-delete a node — see [`Engine::retract`](crate::api::Engine::retract).
    ///
    /// Retracted nodes are excluded from `search()`/`query()` but remain
    /// readable via [`get`](Memory::get) for audit, and are reversible via
    /// [`unforget`](Memory::unforget).
    ///
    /// # Errors
    ///
    /// Returns an error if `id` does not exist.
    pub fn forget(&mut self, id: NodeId, reason: &str, at: Timestamp) -> Result<(), Error> {
        self.engine.retract(id, reason, at)
    }

    /// Reverse a previous [`forget`](Memory::forget) — see
    /// [`Engine::unretract`](crate::api::Engine::unretract).
    ///
    /// # Errors
    ///
    /// Returns an error if `id` does not exist.
    pub fn unforget(&mut self, id: NodeId, at: Timestamp) -> Result<(), Error> {
        self.engine.unretract(id, at)
    }

    /// Permanently remove a node and its incident edges.
    ///
    /// Unlike [`forget`](Memory::forget), this is irreversible — the node is
    /// gone, not merely hidden. Prefer `forget` for the reversible path.
    ///
    /// # Errors
    ///
    /// Returns an error if `id` does not exist.
    pub fn delete_hard(&mut self, id: NodeId) -> Result<(), Error> {
        self.engine.graph_mut().remove_node(id)
    }

    /// Mark `new_id` as superseding `old_id` — sets `old_id.valid_until` and
    /// `new_id.valid_from` via [`Relation::Supersedes`].
    ///
    /// # Errors
    ///
    /// Returns an error if either endpoint does not exist.
    pub fn supersede(&mut self, new_id: NodeId, old_id: NodeId) -> Result<EdgeId, Error> {
        self.relate(new_id, old_id, Relation::Supersedes)
    }
}
