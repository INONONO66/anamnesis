//! Arena-based in-memory storage with SoA hot fields.

use crate::error::Error;
use crate::graph::{Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};
use crate::storage::StorageAdapter;
use std::collections::HashMap;

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

    pub(crate) entity_tag_index: HashMap<String, Vec<NodeId>>,
    pub(crate) type_index: HashMap<KnowledgeType, Vec<NodeId>>,
    pub(crate) agent_index: HashMap<String, Vec<NodeId>>,
    pub(crate) project_index: HashMap<String, Vec<NodeId>>,
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
            entity_tag_index: HashMap::new(),
            type_index: HashMap::new(),
            agent_index: HashMap::new(),
            project_index: HashMap::new(),
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

        if let Some(old_node) = self.nodes[idx].as_ref() {
            let old_tags = old_node.entity_tags.clone();
            let old_type = old_node.node_type.clone();
            let old_agent = old_node.origin.agent_id.clone();
            let old_project = old_node.origin.project_id.clone();
            let old_id = old_node.id;

            for tag in &old_tags {
                if let Some(v) = self.entity_tag_index.get_mut(tag) {
                    v.retain(|&id| id != old_id);
                }
            }
            if let Some(v) = self.type_index.get_mut(&old_type) {
                v.retain(|&id| id != old_id);
            }
            if let Some(v) = self.agent_index.get_mut(&old_agent) {
                v.retain(|&id| id != old_id);
            }
            if let Some(proj) = old_project {
                if let Some(v) = self.project_index.get_mut(&proj) {
                    v.retain(|&id| id != old_id);
                }
            }
        } else {
            self.live_node_count += 1;
        }

        self.salience[idx] = node.salience;
        self.accessed_at[idx] = node.accessed_at;
        self.node_types[idx] = Some(node.node_type.clone());

        let new_id = node.id;
        let mut seen_tags: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for tag in &node.entity_tags {
            if seen_tags.insert(tag.as_str()) {
                self.entity_tag_index
                    .entry(tag.clone())
                    .or_default()
                    .push(new_id);
            }
        }
        self.type_index
            .entry(node.node_type.clone())
            .or_default()
            .push(new_id);
        self.agent_index
            .entry(node.origin.agent_id.clone())
            .or_default()
            .push(new_id);
        if let Some(ref proj) = node.origin.project_id {
            self.project_index
                .entry(proj.clone())
                .or_default()
                .push(new_id);
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

        let tags: Vec<String> = self.nodes[idx]
            .as_ref()
            .map(|n| n.entity_tags.clone())
            .unwrap_or_default();
        let node_type: Option<KnowledgeType> =
            self.nodes[idx].as_ref().map(|n| n.node_type.clone());
        let agent_id: Option<String> = self.nodes[idx].as_ref().map(|n| n.origin.agent_id.clone());
        let project_id: Option<String> = self.nodes[idx]
            .as_ref()
            .and_then(|n| n.origin.project_id.clone());

        for tag in &tags {
            if let Some(v) = self.entity_tag_index.get_mut(tag) {
                v.retain(|&nid| nid != id);
            }
        }
        if let Some(kt) = node_type {
            if let Some(v) = self.type_index.get_mut(&kt) {
                v.retain(|&nid| nid != id);
            }
        }
        if let Some(agent) = agent_id {
            if let Some(v) = self.agent_index.get_mut(&agent) {
                v.retain(|&nid| nid != id);
            }
        }
        if let Some(proj) = project_id {
            if let Some(v) = self.project_index.get_mut(&proj) {
                v.retain(|&nid| nid != id);
            }
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

    fn nodes_by_entity_tag(&self, tag: &str) -> Vec<NodeId> {
        self.entity_tag_index.get(tag).cloned().unwrap_or_default()
    }

    fn nodes_by_type(&self, kt: &KnowledgeType) -> Vec<NodeId> {
        self.type_index.get(kt).cloned().unwrap_or_default()
    }

    fn nodes_by_agent(&self, agent_id: &str) -> Vec<NodeId> {
        self.agent_index.get(agent_id).cloned().unwrap_or_default()
    }

    fn nodes_by_project(&self, project_id: &str) -> Vec<NodeId> {
        self.project_index
            .get(project_id)
            .cloned()
            .unwrap_or_default()
    }

    fn node_ids_descending(&self) -> Vec<NodeId> {
        let mut ids = self.all_node_ids();
        ids.sort_by_key(|a| std::cmp::Reverse(a.0));
        ids
    }

    fn text_search(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)> {
        if limit == 0 {
            return Vec::new();
        }

        let query_lower = query.to_lowercase();
        let all_ids = self.all_node_ids();
        let mut results: Vec<(NodeId, f64)> = Vec::new();

        for &id in &all_ids {
            if let Ok(node) = self.get_node(id) {
                if node.name.to_lowercase() == query_lower
                    || node.content.to_lowercase() == query_lower
                {
                    results.push((id, 1.0));
                }
            }
        }

        if results.len() >= limit {
            results.truncate(limit);
            return results;
        }

        let mut found_ids: std::collections::HashSet<NodeId> =
            results.iter().map(|(id, _)| *id).collect();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        if !query_words.is_empty() && !all_ids.is_empty() {
            let mut idf_by_word = HashMap::new();
            let node_count = all_ids.len() as f64;

            for word in &query_words {
                idf_by_word.entry(*word).or_insert_with(|| {
                    let df = all_ids
                        .iter()
                        .filter(|&&id| {
                            self.get_node(id).ok().is_some_and(|node| {
                                let text = format!(
                                    "{} {}",
                                    node.name.to_lowercase(),
                                    node.content.to_lowercase()
                                );
                                text.contains(*word)
                            })
                        })
                        .count() as f64;

                    if df > 0.0 {
                        (node_count / df).ln()
                    } else {
                        0.0
                    }
                });
            }

            for &id in &all_ids {
                if found_ids.contains(&id) {
                    continue;
                }

                if let Ok(node) = self.get_node(id) {
                    let text = format!(
                        "{} {}",
                        node.name.to_lowercase(),
                        node.content.to_lowercase()
                    );
                    let mut total_idf = 0.0;
                    let mut matched = 0;

                    for word in &query_words {
                        if text.contains(*word) {
                            total_idf += idf_by_word.get(word).copied().unwrap_or(0.0);
                            matched += 1;
                        }
                    }

                    if matched > 0 {
                        let score = (total_idf / matched as f64).clamp(0.5, 1.0);
                        results.push((id, score));
                    }
                }
            }

            if results.len() >= limit {
                results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                results.truncate(limit);
                return results;
            }
        }

        found_ids = results.iter().map(|(id, _)| *id).collect();
        for &id in &all_ids {
            if found_ids.contains(&id) {
                continue;
            }

            if let Ok(node) = self.get_node(id) {
                let text = format!(
                    "{} {}",
                    node.name.to_lowercase(),
                    node.content.to_lowercase()
                );
                if text.contains(&query_lower) {
                    results.push((id, 0.5));
                }
            }
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Origin;
    use std::collections::{HashMap, VecDeque};

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
            access_history: VecDeque::new(),
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
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        }
    }

    fn make_node_indexed(
        id: NodeId,
        entity_tags: Vec<&str>,
        node_type: KnowledgeType,
        agent_id: &str,
        project_id: Option<&str>,
    ) -> Node {
        Node {
            id,
            node_type,
            name: format!("node-{}", id.0),
            summary: None,
            content: "test".to_string(),
            embedding: None,
            created_at: Timestamp(1000),
            updated_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            access_count: 0,
            access_history: VecDeque::new(),
            origin: Origin {
                agent_id: agent_id.to_string(),
                session_id: "session".to_string(),
                project_id: project_id.map(|s| s.to_string()),
                confidence: 0.9,
            },
            entity_tags: entity_tags.iter().map(|s| s.to_string()).collect(),
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
    fn new_storage_has_empty_indexes() {
        let s = InMemoryStorage::new();
        assert_eq!(s.entity_tag_index.len(), 0);
        assert_eq!(s.type_index.len(), 0);
        assert_eq!(s.agent_index.len(), 0);
        assert_eq!(s.project_index.len(), 0);
    }

    #[test]
    fn set_node_populates_indexes() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        let node = make_node_indexed(
            id,
            vec!["auth"],
            KnowledgeType::Convention,
            "agent-A",
            Some("proj-P"),
        );

        s.set_node(node).unwrap();

        assert!(
            s.entity_tag_index
                .get("auth")
                .is_some_and(|v| v.contains(&id))
        );
        assert!(
            s.type_index
                .get(&KnowledgeType::Convention)
                .is_some_and(|v| v.contains(&id))
        );
        assert!(
            s.agent_index
                .get("agent-A")
                .is_some_and(|v| v.contains(&id))
        );
        assert!(
            s.project_index
                .get("proj-P")
                .is_some_and(|v| v.contains(&id))
        );
    }

    #[test]
    fn delete_node_removes_from_indexes() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        s.set_node(make_node_indexed(
            id,
            vec!["auth"],
            KnowledgeType::Convention,
            "A",
            Some("P"),
        ))
        .unwrap();

        s.delete_node(id).unwrap();

        assert!(
            !s.entity_tag_index
                .get("auth")
                .is_some_and(|v| v.contains(&id))
        );
        assert!(
            !s.type_index
                .get(&KnowledgeType::Convention)
                .is_some_and(|v| v.contains(&id))
        );
    }

    #[test]
    fn set_node_update_refreshes_indexes() {
        let mut s = InMemoryStorage::new();
        let id = s.next_node_id();
        s.set_node(make_node_indexed(
            id,
            vec!["old-tag"],
            KnowledgeType::Semantic,
            "agent",
            None,
        ))
        .unwrap();
        assert!(
            s.entity_tag_index
                .get("old-tag")
                .is_some_and(|v| v.contains(&id))
        );

        s.set_node(make_node_indexed(
            id,
            vec!["new-tag"],
            KnowledgeType::Semantic,
            "agent",
            None,
        ))
        .unwrap();

        assert!(
            !s.entity_tag_index
                .get("old-tag")
                .is_some_and(|v| v.contains(&id))
        );
        assert!(
            s.entity_tag_index
                .get("new-tag")
                .is_some_and(|v| v.contains(&id))
        );
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
    fn nodes_by_entity_tag_returns_correct_set() {
        let mut s = InMemoryStorage::new();
        let id1 = s.next_node_id();
        let id2 = s.next_node_id();
        let id3 = s.next_node_id();
        s.set_node(make_node_indexed(
            id1,
            vec!["auth"],
            KnowledgeType::Semantic,
            "A",
            None,
        ))
        .unwrap();
        s.set_node(make_node_indexed(
            id2,
            vec!["auth", "db"],
            KnowledgeType::Semantic,
            "A",
            None,
        ))
        .unwrap();
        s.set_node(make_node_indexed(
            id3,
            vec!["db"],
            KnowledgeType::Convention,
            "B",
            None,
        ))
        .unwrap();
        let auth_set: std::collections::HashSet<_> =
            s.nodes_by_entity_tag("auth").into_iter().collect();
        assert_eq!(auth_set, [id1, id2].iter().copied().collect());
    }

    #[test]
    fn nodes_by_type_returns_correct_set() {
        let mut s = InMemoryStorage::new();
        let id1 = s.next_node_id();
        let id2 = s.next_node_id();
        s.set_node(make_node_indexed(
            id1,
            vec![],
            KnowledgeType::Semantic,
            "A",
            None,
        ))
        .unwrap();
        s.set_node(make_node_indexed(
            id2,
            vec![],
            KnowledgeType::Convention,
            "A",
            None,
        ))
        .unwrap();
        let semantic = s.nodes_by_type(&KnowledgeType::Semantic);
        assert_eq!(semantic, vec![id1]);
    }

    #[test]
    fn nodes_by_agent_returns_correct_set() {
        let mut s = InMemoryStorage::new();
        let id1 = s.next_node_id();
        let id2 = s.next_node_id();
        s.set_node(make_node_indexed(
            id1,
            vec![],
            KnowledgeType::Semantic,
            "agent-A",
            None,
        ))
        .unwrap();
        s.set_node(make_node_indexed(
            id2,
            vec![],
            KnowledgeType::Semantic,
            "agent-B",
            None,
        ))
        .unwrap();
        let a_nodes = s.nodes_by_agent("agent-A");
        assert_eq!(a_nodes, vec![id1]);
    }

    #[test]
    fn nodes_by_project_returns_correct_set() {
        let mut s = InMemoryStorage::new();
        let id1 = s.next_node_id();
        let id2 = s.next_node_id();
        s.set_node(make_node_indexed(
            id1,
            vec![],
            KnowledgeType::Semantic,
            "A",
            Some("proj-X"),
        ))
        .unwrap();
        s.set_node(make_node_indexed(
            id2,
            vec![],
            KnowledgeType::Semantic,
            "A",
            None,
        ))
        .unwrap();
        let proj_nodes = s.nodes_by_project("proj-X");
        assert_eq!(proj_nodes, vec![id1]);
    }

    #[test]
    fn node_ids_descending_returns_sorted() {
        let mut s = InMemoryStorage::new();
        let id0 = s.next_node_id();
        let id1 = s.next_node_id();
        let id2 = s.next_node_id();
        s.set_node(make_node(id0, 0.5)).unwrap();
        s.set_node(make_node(id1, 0.5)).unwrap();
        s.set_node(make_node(id2, 0.5)).unwrap();
        let desc = s.node_ids_descending();
        assert_eq!(desc, vec![id2, id1, id0]);
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
