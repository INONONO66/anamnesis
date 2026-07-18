//! Reproduction tests for the infallible snapshot path seam.
//!
//! `Engine::snapshot()` returned `Result` but cloned storage through the
//! infallible `Clone` (which panics on any SQLite failure), and
//! `check_invariants()` did the same. The fallible `try_clone` seam (added in
//! the durability tranche) exists but nothing consulted it: a storage whose
//! `try_clone` fails must surface `Err` — never panic, never a silent success
//! through the infallible path.

use std::collections::VecDeque;

use anamnesis::Engine;
use anamnesis::api::EngineConfig;
use anamnesis::error::Error;
use anamnesis::graph::{AccessTrace, Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::mechanics::observability::InvariantCheck;
use anamnesis::storage::{SqliteStorage, StorageAdapter};

/// A storage adapter whose durable clone always fails, as a contended or
/// corrupt SQLite file would make it fail.
#[derive(Clone)]
struct FailingCloneAdapter(SqliteStorage);

impl FailingCloneAdapter {
    fn new() -> Self {
        Self(SqliteStorage::in_memory().expect("in-memory storage"))
    }
}

impl StorageAdapter for FailingCloneAdapter {
    fn try_clone(&self) -> Result<Self, Error> {
        Err(Error::StorageError(
            "injected clone failure (contended store)".to_string(),
        ))
    }

    fn next_node_id(&mut self) -> NodeId {
        self.0.next_node_id()
    }

    fn next_edge_id(&mut self) -> EdgeId {
        self.0.next_edge_id()
    }

    fn set_node(&mut self, node: Node) -> Result<(), Error> {
        self.0.set_node(node)
    }

    fn get_node(&self, id: NodeId) -> Result<&Node, Error> {
        self.0.get_node(id)
    }

    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error> {
        self.0.get_node_mut(id)
    }

    fn delete_node(&mut self, id: NodeId) -> Result<(), Error> {
        self.0.delete_node(id)
    }

    fn set_edge(&mut self, edge: Edge) -> Result<(), Error> {
        self.0.set_edge(edge)
    }

    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error> {
        self.0.get_edge(id)
    }

    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error> {
        self.0.get_edge_mut(id)
    }

    fn delete_edge(&mut self, id: EdgeId) -> Result<(), Error> {
        self.0.delete_edge(id)
    }

    fn edges_from(&self, id: NodeId) -> &[EdgeId] {
        self.0.edges_from(id)
    }

    fn edges_to(&self, id: NodeId) -> &[EdgeId] {
        self.0.edges_to(id)
    }

    fn get_salience(&self, id: NodeId) -> Result<f64, Error> {
        self.0.get_salience(id)
    }

    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error> {
        self.0.set_salience(id, salience)
    }

    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error> {
        self.0.get_accessed_at(id)
    }

    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error> {
        self.0.set_accessed_at(id, ts)
    }

    fn get_decay_checkpoint(&self, id: NodeId) -> Result<Timestamp, Error> {
        self.0.get_decay_checkpoint(id)
    }

    fn set_decay_checkpoint(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error> {
        self.0.set_decay_checkpoint(id, ts)
    }

    fn get_access_history(&self, id: NodeId) -> Result<&VecDeque<AccessTrace>, Error> {
        self.0.get_access_history(id)
    }

    fn append_access_trace(&mut self, id: NodeId, trace: AccessTrace) -> Result<(), Error> {
        self.0.append_access_trace(id, trace)
    }

    fn get_evidence_prior(&self, id: NodeId) -> Result<f64, Error> {
        self.0.get_evidence_prior(id)
    }

    fn set_evidence_prior(&mut self, id: NodeId, prior: f64) -> Result<(), Error> {
        self.0.set_evidence_prior(id, prior)
    }

    fn get_retained_action(&self, id: NodeId) -> Result<f64, Error> {
        self.0.get_retained_action(id)
    }

    fn set_retained_action(&mut self, id: NodeId, value: f64) -> Result<(), Error> {
        self.0.set_retained_action(id, value)
    }

    fn get_conductance(&self, id: EdgeId) -> Result<f64, Error> {
        self.0.get_conductance(id)
    }

    fn set_conductance(&mut self, id: EdgeId, value: f64) -> Result<(), Error> {
        self.0.set_conductance(id, value)
    }

    fn get_edge_accessed_at(&self, id: EdgeId) -> Result<Timestamp, Error> {
        self.0.get_edge_accessed_at(id)
    }

    fn set_edge_accessed_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error> {
        self.0.set_edge_accessed_at(id, ts)
    }

    fn get_edge_leaked_at(&self, id: EdgeId) -> Result<Timestamp, Error> {
        self.0.get_edge_leaked_at(id)
    }

    fn set_edge_leaked_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error> {
        self.0.set_edge_leaked_at(id, ts)
    }

    fn get_node_type(&self, id: NodeId) -> Result<&KnowledgeType, Error> {
        self.0.get_node_type(id)
    }

    fn node_count(&self) -> usize {
        self.0.node_count()
    }

    fn edge_count(&self) -> usize {
        self.0.edge_count()
    }

    fn all_node_ids(&self) -> Vec<NodeId> {
        self.0.all_node_ids()
    }

    fn all_edge_ids(&self) -> Vec<EdgeId> {
        self.0.all_edge_ids()
    }
}

#[test]
fn snapshot_returns_err_instead_of_panicking_when_clone_fails() {
    let mut engine = Engine::with_storage(EngineConfig::default(), FailingCloneAdapter::new());
    let result = engine.snapshot("contended");
    assert!(
        result.is_err(),
        "snapshot must surface the clone failure as Err, not panic or succeed \
         through the infallible Clone path"
    );
}

#[test]
fn check_invariants_reports_clone_failure_instead_of_panicking() {
    let engine = Engine::with_storage(EngineConfig::default(), FailingCloneAdapter::new());
    let report = engine.check_invariants(None);
    assert!(
        report
            .results
            .iter()
            .any(|r| r.check == InvariantCheck::SnapshotRestoreConsistency && !r.passed),
        "check_invariants must report SnapshotRestoreConsistency as failed when the \
         clone fails, not panic or report a pass"
    );
}
