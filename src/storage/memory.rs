//! Arena-based in-memory storage with SoA hot fields.

use crate::error::Error;
use crate::graph::{Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};
use crate::storage::StorageAdapter;

const EMPTY_EDGE_SLICE: &[EdgeId] = &[];

pub struct InMemoryStorage {
    nodes: Vec<Option<Node>>,
    edges: Vec<Option<Edge>>,

    salience: Vec<f64>,
    accessed_at: Vec<Timestamp>,
    node_types: Vec<Option<KnowledgeType>>,

    adjacency_out: Vec<Vec<EdgeId>>,
    adjacency_in: Vec<Vec<EdgeId>>,

    next_node_counter: u64,
    next_edge_counter: u64,
    free_node_ids: Vec<NodeId>,
    free_edge_ids: Vec<EdgeId>,

    live_node_count: usize,
    live_edge_count: usize,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            salience: Vec::new(),
            accessed_at: Vec::new(),
            node_types: Vec::new(),
            adjacency_out: Vec::new(),
            adjacency_in: Vec::new(),
            next_node_counter: 0,
            next_edge_counter: 0,
            free_node_ids: Vec::new(),
            free_edge_ids: Vec::new(),
            live_node_count: 0,
            live_edge_count: 0,
        }
    }

    fn ensure_node_capacity(&mut self, idx: usize) {
        if idx >= self.nodes.len() {
            let new_len = idx + 1;
            self.nodes.resize_with(new_len, || None);
            self.salience.resize(new_len, 0.0);
            self.accessed_at.resize(new_len, Timestamp(0));
            self.node_types.resize_with(new_len, || None);
            self.adjacency_out.resize_with(new_len, Vec::new);
            self.adjacency_in.resize_with(new_len, Vec::new);
        }
    }

    fn ensure_edge_capacity(&mut self, idx: usize) {
        if idx >= self.edges.len() {
            self.edges.resize_with(idx + 1, || None);
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageAdapter for InMemoryStorage {
    fn next_node_id(&mut self) -> NodeId {
        if let Some(id) = self.free_node_ids.pop() {
            return id;
        }
        let id = NodeId(self.next_node_counter);
        self.next_node_counter += 1;
        self.ensure_node_capacity(id.0 as usize);
        id
    }

    fn next_edge_id(&mut self) -> EdgeId {
        if let Some(id) = self.free_edge_ids.pop() {
            return id;
        }
        let id = EdgeId(self.next_edge_counter);
        self.next_edge_counter += 1;
        self.ensure_edge_capacity(id.0 as usize);
        id
    }

    fn set_node(&mut self, node: Node) -> Result<(), Error> {
        let idx = node.id.0 as usize;
        self.ensure_node_capacity(idx);

        self.salience[idx] = node.salience;
        self.accessed_at[idx] = node.accessed_at;
        self.node_types[idx] = Some(node.node_type.clone());

        if self.nodes[idx].is_none() {
            self.live_node_count += 1;
        }
        self.nodes[idx] = Some(node);
        Ok(())
    }

    fn get_node(&self, id: NodeId) -> Result<&Node, Error> {
        let idx = id.0 as usize;
        self.nodes
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .ok_or(Error::NodeNotFound(id))
    }

    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error> {
        let idx = id.0 as usize;
        self.nodes
            .get_mut(idx)
            .and_then(|slot| slot.as_mut())
            .ok_or(Error::NodeNotFound(id))
    }

    fn delete_node(&mut self, id: NodeId) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        self.nodes[idx] = None;
        self.salience[idx] = 0.0;
        self.accessed_at[idx] = Timestamp(0);
        self.node_types[idx] = None;
        if idx < self.adjacency_out.len() {
            self.adjacency_out[idx].clear();
        }
        if idx < self.adjacency_in.len() {
            self.adjacency_in[idx].clear();
        }
        self.live_node_count -= 1;
        self.free_node_ids.push(id);
        Ok(())
    }

    fn set_edge(&mut self, edge: Edge) -> Result<(), Error> {
        let idx = edge.id.0 as usize;
        self.ensure_edge_capacity(idx);

        let source_idx = edge.source.0 as usize;
        let target_idx = edge.target.0 as usize;
        self.ensure_node_capacity(source_idx);
        self.ensure_node_capacity(target_idx);

        if let Some(ref old_edge) = self.edges[idx] {
            let old_source = old_edge.source.0 as usize;
            let old_target = old_edge.target.0 as usize;
            if old_source < self.adjacency_out.len() {
                self.adjacency_out[old_source].retain(|e| *e != edge.id);
            }
            if old_target < self.adjacency_in.len() {
                self.adjacency_in[old_target].retain(|e| *e != edge.id);
            }
        } else {
            self.live_edge_count += 1;
        }

        self.adjacency_out[source_idx].push(edge.id);
        self.adjacency_in[target_idx].push(edge.id);
        self.edges[idx] = Some(edge);
        Ok(())
    }

    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error> {
        let idx = id.0 as usize;
        self.edges
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .ok_or(Error::EdgeNotFound(id))
    }

    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error> {
        let idx = id.0 as usize;
        self.edges
            .get_mut(idx)
            .and_then(|slot| slot.as_mut())
            .ok_or(Error::EdgeNotFound(id))
    }

    fn delete_edge(&mut self, id: EdgeId) -> Result<(), Error> {
        let idx = id.0 as usize;
        let edge = self
            .edges
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .ok_or(Error::EdgeNotFound(id))?;

        let source_idx = edge.source.0 as usize;
        let target_idx = edge.target.0 as usize;

        if source_idx < self.adjacency_out.len() {
            self.adjacency_out[source_idx].retain(|e| *e != id);
        }
        if target_idx < self.adjacency_in.len() {
            self.adjacency_in[target_idx].retain(|e| *e != id);
        }

        self.edges[idx] = None;
        self.live_edge_count -= 1;
        self.free_edge_ids.push(id);
        Ok(())
    }

    fn edges_from(&self, id: NodeId) -> &[EdgeId] {
        let idx = id.0 as usize;
        self.adjacency_out
            .get(idx)
            .map(|v| v.as_slice())
            .unwrap_or(EMPTY_EDGE_SLICE)
    }

    fn edges_to(&self, id: NodeId) -> &[EdgeId] {
        let idx = id.0 as usize;
        self.adjacency_in
            .get(idx)
            .map(|v| v.as_slice())
            .unwrap_or(EMPTY_EDGE_SLICE)
    }

    fn get_salience(&self, id: NodeId) -> Result<f64, Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        Ok(self.salience[idx])
    }

    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        self.salience[idx] = salience;
        if let Some(ref mut node) = self.nodes[idx] {
            node.salience = salience;
        }
        Ok(())
    }

    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        Ok(self.accessed_at[idx])
    }

    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        self.accessed_at[idx] = ts;
        if let Some(ref mut node) = self.nodes[idx] {
            node.accessed_at = ts;
        }
        Ok(())
    }

    fn get_node_type(&self, id: NodeId) -> Result<&KnowledgeType, Error> {
        let idx = id.0 as usize;
        if idx >= self.node_types.len() {
            return Err(Error::NodeNotFound(id));
        }
        self.node_types[idx].as_ref().ok_or(Error::NodeNotFound(id))
    }

    fn node_count(&self) -> usize {
        self.live_node_count
    }

    fn edge_count(&self) -> usize {
        self.live_edge_count
    }

    fn all_node_ids(&self) -> Vec<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| NodeId(i as u64)))
            .collect()
    }

    fn all_edge_ids(&self) -> Vec<EdgeId> {
        self.edges
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| EdgeId(i as u64)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Origin;
    use std::collections::HashMap;

    fn make_node(id: NodeId, salience: f64) -> Node {
        Node {
            id,
            node_type: KnowledgeType::Semantic,
            name: format!("node-{}", id.0),
            summary: None,
            content: format!("content for node {}", id.0),
            embedding: None,
            created_at: Timestamp(1000),
            updated_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            salience,
            access_count: 0,
            origin: Origin {
                agent_id: "test-agent".to_string(),
                session_id: "test-session".to_string(),
                project_id: None,
                confidence: 0.9,
            },
            entity_tags: vec![],
            metadata: HashMap::new(),
        }
    }

    fn make_edge(id: EdgeId, source: NodeId, target: NodeId) -> Edge {
        Edge {
            id,
            source,
            target,
            edge_type: crate::graph::EdgeType::Semantic,
            weight: 0.8,
            created_at: Timestamp(1000),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn new_storage_is_empty() {
        let s = InMemoryStorage::new();
        assert_eq!(s.node_count(), 0);
        assert_eq!(s.edge_count(), 0);
        assert!(s.all_node_ids().is_empty());
        assert!(s.all_edge_ids().is_empty());
    }

    #[test]
    fn allocate_and_store_node() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        assert_eq!(id, NodeId(0));

        let node = make_node(id, 0.7);
        s.set_node(node).unwrap();

        let retrieved = s.get_node(id).unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.salience, 0.7);
        assert_eq!(s.node_count(), 1);
    }

    #[test]
    fn delete_node_frees_id() {
        let mut s = InMemoryStorage::new();
        let id0 = s.next_node_id();
        s.set_node(make_node(id0, 0.5)).unwrap();
        s.delete_node(id0).unwrap();

        assert_eq!(s.node_count(), 0);

        let reused = s.next_node_id();
        assert_eq!(reused, id0);
    }

    #[test]
    fn allocate_and_store_edge() {
        let mut s = InMemoryStorage::new();
        let n0 = s.next_node_id();
        let n1 = s.next_node_id();
        s.set_node(make_node(n0, 0.5)).unwrap();
        s.set_node(make_node(n1, 0.5)).unwrap();

        let eid = s.next_edge_id();
        assert_eq!(eid, EdgeId(0));

        s.set_edge(make_edge(eid, n0, n1)).unwrap();
        let retrieved = s.get_edge(eid).unwrap();
        assert_eq!(retrieved.source, n0);
        assert_eq!(retrieved.target, n1);
        assert_eq!(s.edge_count(), 1);
    }

    #[test]
    fn delete_edge_frees_id() {
        let mut s = InMemoryStorage::new();
        let n0 = s.next_node_id();
        let n1 = s.next_node_id();
        s.set_node(make_node(n0, 0.5)).unwrap();
        s.set_node(make_node(n1, 0.5)).unwrap();

        let eid = s.next_edge_id();
        s.set_edge(make_edge(eid, n0, n1)).unwrap();
        s.delete_edge(eid).unwrap();

        assert_eq!(s.edge_count(), 0);
        let reused = s.next_edge_id();
        assert_eq!(reused, eid);
    }

    #[test]
    fn adjacency_out_correct() {
        let mut s = InMemoryStorage::new();
        let a = s.next_node_id();
        let b = s.next_node_id();
        let c = s.next_node_id();
        s.set_node(make_node(a, 0.5)).unwrap();
        s.set_node(make_node(b, 0.5)).unwrap();
        s.set_node(make_node(c, 0.5)).unwrap();

        let e0 = s.next_edge_id();
        let e1 = s.next_edge_id();
        s.set_edge(make_edge(e0, a, b)).unwrap();
        s.set_edge(make_edge(e1, a, c)).unwrap();

        let out = s.edges_from(a);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&e0));
        assert!(out.contains(&e1));
    }

    #[test]
    fn adjacency_in_correct() {
        let mut s = InMemoryStorage::new();
        let a = s.next_node_id();
        let b = s.next_node_id();
        let c = s.next_node_id();
        s.set_node(make_node(a, 0.5)).unwrap();
        s.set_node(make_node(b, 0.5)).unwrap();
        s.set_node(make_node(c, 0.5)).unwrap();

        let e0 = s.next_edge_id();
        let e1 = s.next_edge_id();
        s.set_edge(make_edge(e0, a, c)).unwrap();
        s.set_edge(make_edge(e1, b, c)).unwrap();

        let inc = s.edges_to(c);
        assert_eq!(inc.len(), 2);
        assert!(inc.contains(&e0));
        assert!(inc.contains(&e1));
    }

    #[test]
    fn adjacency_updated_on_delete() {
        let mut s = InMemoryStorage::new();
        let a = s.next_node_id();
        let b = s.next_node_id();
        s.set_node(make_node(a, 0.5)).unwrap();
        s.set_node(make_node(b, 0.5)).unwrap();

        let eid = s.next_edge_id();
        s.set_edge(make_edge(eid, a, b)).unwrap();
        assert_eq!(s.edges_from(a).len(), 1);

        s.delete_edge(eid).unwrap();
        assert!(s.edges_from(a).is_empty());
        assert!(s.edges_to(b).is_empty());
    }

    #[test]
    fn hot_fields_synced() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        s.set_node(make_node(id, 0.5)).unwrap();

        s.set_salience(id, 0.9).unwrap();
        assert_eq!(s.get_salience(id).unwrap(), 0.9);
        assert_eq!(s.get_node(id).unwrap().salience, 0.9);

        s.set_accessed_at(id, Timestamp(5000)).unwrap();
        assert_eq!(s.get_accessed_at(id).unwrap(), Timestamp(5000));
        assert_eq!(s.get_node(id).unwrap().accessed_at, Timestamp(5000));
    }

    #[test]
    fn all_node_ids_excludes_deleted() {
        let mut s = InMemoryStorage::new();
        let n0 = s.next_node_id();
        let n1 = s.next_node_id();
        let n2 = s.next_node_id();
        s.set_node(make_node(n0, 0.5)).unwrap();
        s.set_node(make_node(n1, 0.5)).unwrap();
        s.set_node(make_node(n2, 0.5)).unwrap();

        s.delete_node(n1).unwrap();

        let ids = s.all_node_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&n0));
        assert!(ids.contains(&n2));
        assert!(!ids.contains(&n1));
    }

    #[test]
    fn get_node_type_from_soa() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        s.set_node(make_node(id, 0.5)).unwrap();

        assert_eq!(s.get_node_type(id).unwrap(), &KnowledgeType::Semantic);
    }

    #[test]
    fn get_nonexistent_node_returns_error() {
        let s = InMemoryStorage::new();
        assert_eq!(s.get_node(NodeId(99)), Err(Error::NodeNotFound(NodeId(99))));
    }

    #[test]
    fn get_nonexistent_edge_returns_error() {
        let s = InMemoryStorage::new();
        assert_eq!(s.get_edge(EdgeId(99)), Err(Error::EdgeNotFound(EdgeId(99))));
    }

    #[test]
    fn edges_from_nonexistent_node_returns_empty() {
        let s = InMemoryStorage::new();
        assert!(s.edges_from(NodeId(99)).is_empty());
    }

    #[test]
    fn set_edge_twice_no_duplicate_adjacency() {
        let mut s = InMemoryStorage::new();
        let n0 = s.next_node_id();
        let n1 = s.next_node_id();
        s.set_node(make_node(n0, 0.5)).unwrap();
        s.set_node(make_node(n1, 0.5)).unwrap();
        let eid = s.next_edge_id();
        s.set_edge(make_edge(eid, n0, n1)).unwrap();
        s.set_edge(make_edge(eid, n0, n1)).unwrap();
        assert_eq!(s.edges_from(n0).len(), 1);
        assert_eq!(s.edges_to(n1).len(), 1);
        assert_eq!(s.edge_count(), 1);
    }

    #[test]
    fn delete_node_clears_adjacency() {
        let mut s = InMemoryStorage::new();
        let n0 = s.next_node_id();
        let n1 = s.next_node_id();
        s.set_node(make_node(n0, 0.5)).unwrap();
        s.set_node(make_node(n1, 0.5)).unwrap();
        let eid = s.next_edge_id();
        s.set_edge(make_edge(eid, n0, n1)).unwrap();
        s.delete_edge(eid).unwrap();
        s.delete_node(n0).unwrap();
        let reused = s.next_node_id();
        assert_eq!(reused, n0);
        s.set_node(make_node(reused, 0.8)).unwrap();
        assert!(s.edges_from(reused).is_empty());
        assert!(s.edges_to(reused).is_empty());
    }

    #[test]
    fn get_node_mut_modifies_in_place() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        s.set_node(make_node(id, 0.5)).unwrap();

        let node = s.get_node_mut(id).unwrap();
        node.name = "modified".to_string();

        assert_eq!(s.get_node(id).unwrap().name, "modified");
    }
}
