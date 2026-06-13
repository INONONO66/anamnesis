//! Clone-based snapshot storage for reversible engine state.
//!
//! Snapshots are in-process only. They include the full storage state via
//! `Storage: Clone`, which captures all SoA hot fields (including the internal
//! `decay_checkpoint`). Cross-version snapshot serialization is not supported:
//! consumers requiring durable persistence must implement an external
//! serialization layer over a `StorageAdapter` that supports it.

use crate::error::Error;
use crate::graph::Timestamp;

/// Unique identifier for an engine snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotId(pub u64);

/// A stored snapshot entry.
#[derive(Clone)]
pub struct SnapshotEntry<S: Clone> {
    /// Stable snapshot identifier.
    pub id: SnapshotId,
    /// Consumer-provided label for display and audit trails.
    pub label: String,
    /// Timestamp associated with snapshot creation.
    pub timestamp: Timestamp,
    /// Full cloned storage state.
    pub storage: S,
}

/// Backend interface for clone-based snapshot storage.
pub trait SnapshotBackend<S: Clone> {
    /// Store a clone of the current storage state.
    fn take(&mut self, label: &str, storage: &S, timestamp: Timestamp) -> SnapshotId;

    /// Return a cloned storage state for the requested snapshot.
    fn restore(&self, id: &SnapshotId) -> Result<S, Error>;

    /// List stored snapshot metadata in insertion order.
    fn list(&self) -> Vec<(SnapshotId, String, Timestamp)>;

    /// Remove a stored snapshot.
    fn drop_snapshot(&mut self, id: SnapshotId) -> Option<SnapshotEntry<S>>;
}

/// In-memory clone-based snapshot store.
pub struct SnapshotStore<S: Clone> {
    entries: Vec<SnapshotEntry<S>>,
    next_id: u64,
}

impl<S: Clone> SnapshotStore<S> {
    /// Create an empty snapshot store.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 0,
        }
    }

    /// Store a clone of the current storage state.
    pub fn take(&mut self, label: &str, storage: &S, timestamp: Timestamp) -> SnapshotId {
        let id = SnapshotId(self.next_id);
        self.next_id += 1;

        self.entries.push(SnapshotEntry {
            id,
            label: label.to_string(),
            timestamp,
            storage: storage.clone(),
        });

        id
    }

    /// Return a cloned storage state for the requested snapshot.
    pub fn restore(&self, id: &SnapshotId) -> Result<S, Error> {
        self.entries
            .iter()
            .find(|entry| entry.id == *id)
            .map(|entry| entry.storage.clone())
            .ok_or_else(|| Error::InvalidInput(format!("snapshot not found: {}", id.0)))
    }

    /// List stored snapshot metadata in insertion order.
    pub fn list(&self) -> Vec<(SnapshotId, String, Timestamp)> {
        self.entries
            .iter()
            .map(|entry| (entry.id, entry.label.clone(), entry.timestamp))
            .collect()
    }

    /// Remove a stored snapshot.
    pub fn drop_snapshot(&mut self, id: SnapshotId) -> Option<SnapshotEntry<S>> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        Some(self.entries.remove(index))
    }
}

impl<S: Clone> Default for SnapshotStore<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone> SnapshotBackend<S> for SnapshotStore<S> {
    fn take(&mut self, label: &str, storage: &S, timestamp: Timestamp) -> SnapshotId {
        SnapshotStore::take(self, label, storage, timestamp)
    }

    fn restore(&self, id: &SnapshotId) -> Result<S, Error> {
        SnapshotStore::restore(self, id)
    }

    fn list(&self) -> Vec<(SnapshotId, String, Timestamp)> {
        SnapshotStore::list(self)
    }

    fn drop_snapshot(&mut self, id: SnapshotId) -> Option<SnapshotEntry<S>> {
        SnapshotStore::drop_snapshot(self, id)
    }
}
