//! Contract proof (Gate 0.2): a downstream-style `StorageAdapter` implementor
//! written before `try_clone` existed must compile source-unchanged, and the
//! trait's default `try_clone` must keep Engine snapshot/restore working.
//!
//! `DelegatingAdapter` deliberately does NOT mention `try_clone`: if adding
//! the method were a breaking change, this file would fail to compile.

use std::collections::VecDeque;

use anamnesis::Engine;
use anamnesis::api::EngineConfig;
use anamnesis::error::Error;
use anamnesis::graph::{AccessTrace, Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};

#[derive(Clone)]
struct DelegatingAdapter(SqliteStorage);

impl StorageAdapter for DelegatingAdapter {
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
fn external_adapter_compiles_without_try_clone_and_snapshots() {
    let adapter = DelegatingAdapter(SqliteStorage::in_memory().expect("in-memory storage"));
    let mut engine = Engine::with_storage(EngineConfig::default(), adapter);

    let snapshot = engine
        .snapshot("contract")
        .expect("snapshot must succeed through the default try_clone");
    engine.restore(&snapshot).expect("restore must succeed");
    assert_eq!(engine.list_snapshots().len(), 1);
}
