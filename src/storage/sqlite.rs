//! SQLite storage adapter with FTS5 text search.

use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::types::PeerId;
use crate::graph::{
    Edge, EdgeId, EdgeType, KnowledgeType, MemoryTier, Node, NodeId, ScopePath, Timestamp,
};
use crate::peer::SourceKind;
use crate::storage::StorageAdapter;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const EMPTY_EDGE_SLICE: &[EdgeId] = &[];

/// SQLite-backed storage adapter.
///
/// The adapter keeps graph objects and hot SoA fields cached in memory so the
/// `StorageAdapter` reference-returning API remains fast. Node and edge writes
/// remain write-through for FTS5/index maintenance; hot-field setters use
/// dirty flags for write-behind persistence. Full-text search is backed by FTS5.
pub struct SqliteStorage {
    conn: Mutex<Connection>,

    nodes: Vec<Option<Node>>,
    edges: Vec<Option<Edge>>,
    salience: Vec<f64>,
    retained_action: Vec<f64>,
    accessed_at: Vec<Timestamp>,
    decay_checkpoint: Vec<Timestamp>,
    edge_conductance: Vec<f64>,
    edge_accessed_at: Vec<Timestamp>,
    dirty_salience: Vec<bool>,
    dirty_retained_action: Vec<bool>,
    dirty_accessed_at: Vec<bool>,
    dirty_decay_checkpoint: Vec<bool>,
    dirty_edge_conductance: Vec<bool>,
    dirty_edge_accessed_at: Vec<bool>,
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

impl SqliteStorage {
    /// Create an in-memory SQLite storage backend.
    pub fn new() -> Result<Self, Error> {
        Self::in_memory()
    }

    /// Create an in-memory SQLite storage backend.
    pub fn in_memory() -> Result<Self, Error> {
        Self::from_connection(Connection::open_in_memory().map_err(sqlite_error)?)
    }

    /// Open or create a SQLite-backed storage file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_connection(Connection::open(path).map_err(sqlite_error)?)
    }

    fn from_connection(conn: Connection) -> Result<Self, Error> {
        migrate_schema(&conn)?;

        let capacity = 0;
        let mut storage = Self {
            conn: Mutex::new(conn),
            nodes: Vec::new(),
            edges: Vec::new(),
            salience: Vec::new(),
            retained_action: Vec::new(),
            accessed_at: Vec::new(),
            decay_checkpoint: Vec::new(),
            edge_conductance: Vec::new(),
            edge_accessed_at: Vec::new(),
            dirty_salience: vec![false; capacity],
            dirty_retained_action: vec![false; capacity],
            dirty_accessed_at: vec![false; capacity],
            dirty_decay_checkpoint: vec![false; capacity],
            dirty_edge_conductance: vec![false; capacity],
            dirty_edge_accessed_at: vec![false; capacity],
            node_types: Vec::new(),
            adjacency_out: Vec::new(),
            adjacency_in: Vec::new(),
            next_node_counter: 0,
            next_edge_counter: 0,
            free_node_ids: Vec::new(),
            free_edge_ids: Vec::new(),
            live_node_count: 0,
            live_edge_count: 0,
        };
        storage.load_from_db()?;
        Ok(storage)
    }

    fn lock_conn(&self) -> Result<MutexGuard<'_, Connection>, Error> {
        self.conn
            .lock()
            .map_err(|_| Error::StorageError("sqlite connection lock poisoned".to_string()))
    }

    fn ensure_node_capacity(&mut self, idx: usize) {
        if idx >= self.nodes.len() {
            let new_len = idx + 1;
            self.nodes.resize_with(new_len, || None);
            self.salience.resize(new_len, 0.0);
            self.retained_action.resize(new_len, 0.0);
            self.accessed_at.resize(new_len, Timestamp(0));
            self.decay_checkpoint.resize(new_len, Timestamp(0));
            self.dirty_salience.resize(new_len, false);
            self.dirty_retained_action.resize(new_len, false);
            self.dirty_accessed_at.resize(new_len, false);
            self.dirty_decay_checkpoint.resize(new_len, false);
            self.node_types.resize_with(new_len, || None);
            self.adjacency_out.resize_with(new_len, Vec::new);
            self.adjacency_in.resize_with(new_len, Vec::new);
        }
    }

    fn ensure_edge_capacity(&mut self, idx: usize) {
        if idx >= self.edges.len() {
            let new_len = idx + 1;
            self.edges.resize_with(new_len, || None);
            self.edge_conductance.resize(new_len, 0.0);
            self.edge_accessed_at.resize(new_len, Timestamp(0));
            self.dirty_edge_conductance.resize(new_len, false);
            self.dirty_edge_accessed_at.resize(new_len, false);
        }
    }

    fn load_from_db(&mut self) -> Result<(), Error> {
        let (nodes, edges, free_nodes, free_edges) = {
            let conn = self.lock_conn()?;
            let nodes = load_nodes(&conn)?;
            let edges = load_edges(&conn)?;
            let free_nodes = load_free_ids(&conn, "node")?
                .into_iter()
                .map(NodeId)
                .collect::<Vec<_>>();
            let free_edges = load_free_ids(&conn, "edge")?
                .into_iter()
                .map(EdgeId)
                .collect::<Vec<_>>();
            (nodes, edges, free_nodes, free_edges)
        };

        for (node, salience, retained_action, accessed_at, decay_checkpoint) in nodes {
            let idx = node.id.0 as usize;
            self.ensure_node_capacity(idx);
            self.salience[idx] = salience;
            self.retained_action[idx] = retained_action;
            self.accessed_at[idx] = accessed_at;
            self.decay_checkpoint[idx] = decay_checkpoint;
            self.node_types[idx] = Some(node.node_type.clone());
            self.nodes[idx] = Some(node);
            self.live_node_count += 1;
        }

        for edge in edges {
            let idx = edge.id.0 as usize;
            self.ensure_edge_capacity(idx);
            self.ensure_node_capacity(edge.source.0 as usize);
            self.ensure_node_capacity(edge.target.0 as usize);
            self.edge_conductance[idx] = edge.conductance;
            self.edge_accessed_at[idx] = edge.accessed_at;
            self.adjacency_out[edge.source.0 as usize].push(edge.id);
            self.adjacency_in[edge.target.0 as usize].push(edge.id);
            self.edges[idx] = Some(edge);
            self.live_edge_count += 1;
        }

        self.free_node_ids = free_nodes;
        self.free_edge_ids = free_edges;
        self.next_node_counter = self
            .nodes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, slot)| slot.as_ref().map(|_| idx as u64 + 1))
            .unwrap_or(0);
        self.next_edge_counter = self
            .edges
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, slot)| slot.as_ref().map(|_| idx as u64 + 1))
            .unwrap_or(0);
        self.dirty_salience = vec![false; self.nodes.len()];
        self.dirty_retained_action = vec![false; self.nodes.len()];
        self.dirty_accessed_at = vec![false; self.nodes.len()];
        self.dirty_decay_checkpoint = vec![false; self.nodes.len()];
        // Size edge SoA + reset edge dirty arrays to the final edge capacity.
        self.edge_conductance.resize(self.edges.len(), 0.0);
        self.edge_accessed_at.resize(self.edges.len(), Timestamp(0));
        self.dirty_edge_conductance = vec![false; self.edges.len()];
        self.dirty_edge_accessed_at = vec![false; self.edges.len()];
        Ok(())
    }

    fn query_node_ids(&self, sql: &str, value: &str) -> Vec<NodeId> {
        self.query_node_ids_inner(sql, value).unwrap_or_default()
    }

    fn query_node_ids_u64(&self, sql: &str, value: u64) -> Vec<NodeId> {
        self.query_node_ids_u64_inner(sql, value)
            .unwrap_or_default()
    }

    fn query_node_ids_inner(&self, sql: &str, value: &str) -> Result<Vec<NodeId>, Error> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(sql).map_err(sqlite_error)?;
        let rows = stmt
            .query_map([value], |row| row.get::<_, u64>(0))
            .map_err(sqlite_error)?;
        let ids = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?
            .into_iter()
            .map(NodeId)
            .collect();
        Ok(ids)
    }

    fn query_node_ids_u64_inner(&self, sql: &str, value: u64) -> Result<Vec<NodeId>, Error> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(sql).map_err(sqlite_error)?;
        let rows = stmt
            .query_map([value], |row| row.get::<_, u64>(0))
            .map_err(sqlite_error)?;
        let ids = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?
            .into_iter()
            .map(NodeId)
            .collect();
        Ok(ids)
    }
}

impl Clone for SqliteStorage {
    fn clone(&self) -> Self {
        let cloned = Self::in_memory()
            .unwrap_or_else(|e| panic!("failed to clone sqlite storage into memory: {e}"));

        {
            let conn = cloned
                .lock_conn()
                .unwrap_or_else(|e| panic!("failed to lock sqlite clone: {e}"));

            conn.execute_batch("BEGIN IMMEDIATE;")
                .unwrap_or_else(|e| panic!("failed to begin transaction during sqlite clone: {e}"));

            let write_result = (|| -> Result<(), Box<dyn std::error::Error>> {
                for id in self.all_node_ids() {
                    let node = self
                        .get_node(id)
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?
                        .clone();
                    let decay_checkpoint = self
                        .get_decay_checkpoint(id)
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute(
                        "INSERT OR REPLACE INTO nodes (
                            id, name, summary, content, embedding_json, node_type, peer_id, source_kind, session_id,
                            scope, confidence, valid_from, valid_until, created_at, updated_at,
                            access_count, access_history, tier, metadata
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                        params![
                            node.id.0,
                            node.name,
                            node.summary,
                            node.content,
                            encode_embedding(node.embedding.as_deref()),
                            encode_knowledge_type(&node.node_type),
                            node.origin.peer_id.0,
                            encode_source_kind(&node.origin.source_kind),
                            node.origin.session_id,
                            node.origin.scope.as_str(),
                            node.origin.confidence,
                            node.valid_from.map(|ts| ts.0),
                            node.valid_until.map(|ts| ts.0),
                            node.created_at.0,
                            node.updated_at.0,
                            node.access_count,
                            encode_timestamp_deque(&node.access_history),
                            encode_memory_tier(&node.tier),
                            encode_map(&node.metadata),
                        ],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute(
                        "INSERT OR REPLACE INTO salience (node_id, salience) VALUES (?1, ?2)",
                        params![node.id.0, node.salience],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute(
                        "INSERT OR REPLACE INTO accessed_at (node_id, accessed_at) VALUES (?1, ?2)",
                        params![node.id.0, node.accessed_at.0],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute(
                        "INSERT OR REPLACE INTO decay_checkpoint (node_id, decay_checkpoint) VALUES (?1, ?2)",
                        params![node.id.0, decay_checkpoint.0],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute(
                        "INSERT OR REPLACE INTO retained_action (node_id, value) VALUES (?1, ?2)",
                        params![node.id.0, node.retained_action],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute("DELETE FROM node_fts WHERE id = ?1", [node.id.0])
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute(
                        "INSERT INTO node_fts (id, name, content) VALUES (?1, ?2, ?3)",
                        params![node.id.0, node.name, node.content],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    conn.execute("DELETE FROM entity_tags WHERE node_id = ?1", [node.id.0])
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

                    for tag in unique_strings(&node.entity_tags) {
                        conn.execute(
                            "INSERT OR IGNORE INTO entity_tags (node_id, tag) VALUES (?1, ?2)",
                            params![node.id.0, tag],
                        )
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                    }
                }

                for id in self.all_edge_ids() {
                    let edge = self
                        .get_edge(id)
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?
                        .clone();

                    conn.execute(
                        "INSERT OR REPLACE INTO edges (
                            id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata, edge_source, conductance, accessed_at
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            edge.id.0,
                            edge.source.0,
                            edge.target.0,
                            encode_edge_type(&edge.edge_type),
                            edge.weight,
                            edge.created_at.0,
                            edge.valid_from.map(|ts| ts.0),
                            edge.valid_until.map(|ts| ts.0),
                            encode_map(&edge.metadata),
                            encode_edge_source(&edge.edge_source),
                            edge.conductance,
                            edge.accessed_at.0,
                        ],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                }

                for id in &self.free_node_ids {
                    conn.execute(
                        "INSERT INTO free_ids (id_type, id_value) VALUES ('node', ?1)",
                        [id.0],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                }

                for id in &self.free_edge_ids {
                    conn.execute(
                        "INSERT INTO free_ids (id_type, id_value) VALUES ('edge', ?1)",
                        [id.0],
                    )
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                }

                Ok(())
            })();

            if let Err(error) = write_result {
                let _ = conn.execute_batch("ROLLBACK;");
                panic!("failed during sqlite clone transaction: {error}");
            }

            if let Err(e) = conn.execute_batch("COMMIT;") {
                let _ = conn.execute_batch("ROLLBACK;");
                panic!("failed to commit sqlite clone transaction: {e}");
            }
        }

        let mut result = Self::from_connection(
            cloned
                .conn
                .into_inner()
                .unwrap_or_else(|_| panic!("failed to unwrap cloned sqlite connection")),
        )
        .unwrap_or_else(|e| panic!("failed to load cloned sqlite storage: {e}"));
        result.next_node_counter = result.next_node_counter.max(self.next_node_counter);
        result.next_edge_counter = result.next_edge_counter.max(self.next_edge_counter);
        result
    }
}

impl StorageAdapter for SqliteStorage {
    fn next_node_id(&mut self) -> NodeId {
        if let Some(id) = self.free_node_ids.pop() {
            if let Ok(conn) = self.lock_conn() {
                let _ = conn.execute(
                    "DELETE FROM free_ids WHERE id_type = 'node' AND id_value = ?1",
                    [id.0],
                );
            }
            return id;
        }
        let id = NodeId(self.next_node_counter);
        self.next_node_counter += 1;
        self.ensure_node_capacity(id.0 as usize);
        id
    }

    fn next_edge_id(&mut self) -> EdgeId {
        if let Some(id) = self.free_edge_ids.pop() {
            if let Ok(conn) = self.lock_conn() {
                let _ = conn.execute(
                    "DELETE FROM free_ids WHERE id_type = 'edge' AND id_value = ?1",
                    [id.0],
                );
            }
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
        let was_empty = self.nodes[idx].is_none();
        {
            let conn = self.lock_conn()?;
            insert_node_row(&conn, &node, node.accessed_at)?;
        }

        self.salience[idx] = node.salience;
        self.retained_action[idx] = node.retained_action;
        self.accessed_at[idx] = node.accessed_at;
        self.decay_checkpoint[idx] = node.accessed_at;
        self.dirty_salience[idx] = false;
        self.dirty_retained_action[idx] = false;
        self.dirty_accessed_at[idx] = false;
        self.dirty_decay_checkpoint[idx] = false;
        self.node_types[idx] = Some(node.node_type.clone());
        self.nodes[idx] = Some(node);
        if was_empty {
            self.live_node_count += 1;
        }
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

        {
            let conn = self.lock_conn()?;
            conn.execute("DELETE FROM entity_tags WHERE node_id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute("DELETE FROM node_fts WHERE id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute("DELETE FROM salience WHERE node_id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute("DELETE FROM accessed_at WHERE node_id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute("DELETE FROM decay_checkpoint WHERE node_id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute("DELETE FROM retained_action WHERE node_id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute("DELETE FROM nodes WHERE id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute(
                "INSERT INTO free_ids (id_type, id_value) VALUES ('node', ?1)",
                [id.0],
            )
            .map_err(sqlite_error)?;
        }

        self.nodes[idx] = None;
        self.salience[idx] = 0.0;
        self.retained_action[idx] = 0.0;
        self.accessed_at[idx] = Timestamp(0);
        self.decay_checkpoint[idx] = Timestamp(0);
        self.dirty_salience[idx] = false;
        self.dirty_retained_action[idx] = false;
        self.dirty_accessed_at[idx] = false;
        self.dirty_decay_checkpoint[idx] = false;
        self.node_types[idx] = None;
        self.adjacency_out[idx].clear();
        self.adjacency_in[idx].clear();
        self.live_node_count -= 1;
        self.free_node_ids.push(id);
        Ok(())
    }

    fn set_edge(&mut self, edge: Edge) -> Result<(), Error> {
        let idx = edge.id.0 as usize;
        self.ensure_edge_capacity(idx);
        self.ensure_node_capacity(edge.source.0 as usize);
        self.ensure_node_capacity(edge.target.0 as usize);

        if let Some(old_edge) = self.edges[idx].as_ref() {
            self.adjacency_out[old_edge.source.0 as usize].retain(|eid| *eid != edge.id);
            self.adjacency_in[old_edge.target.0 as usize].retain(|eid| *eid != edge.id);
        } else {
            self.live_edge_count += 1;
        }

        {
            let conn = self.lock_conn()?;
            insert_edge_row(&conn, &edge)?;
        }
        self.edge_conductance[idx] = edge.conductance;
        self.edge_accessed_at[idx] = edge.accessed_at;
        self.dirty_edge_conductance[idx] = false;
        self.dirty_edge_accessed_at[idx] = false;
        self.adjacency_out[edge.source.0 as usize].push(edge.id);
        self.adjacency_in[edge.target.0 as usize].push(edge.id);
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

        {
            let conn = self.lock_conn()?;
            conn.execute("DELETE FROM edges WHERE id = ?1", [id.0])
                .map_err(sqlite_error)?;
            conn.execute(
                "INSERT INTO free_ids (id_type, id_value) VALUES ('edge', ?1)",
                [id.0],
            )
            .map_err(sqlite_error)?;
        }

        self.adjacency_out[source_idx].retain(|eid| *eid != id);
        self.adjacency_in[target_idx].retain(|eid| *eid != id);
        self.edges[idx] = None;
        self.edge_conductance[idx] = 0.0;
        self.edge_accessed_at[idx] = Timestamp(0);
        self.dirty_edge_conductance[idx] = false;
        self.dirty_edge_accessed_at[idx] = false;
        self.live_edge_count -= 1;
        self.free_edge_ids.push(id);
        Ok(())
    }

    fn edges_from(&self, id: NodeId) -> &[EdgeId] {
        self.adjacency_out
            .get(id.0 as usize)
            .map(Vec::as_slice)
            .unwrap_or(EMPTY_EDGE_SLICE)
    }

    fn edges_to(&self, id: NodeId) -> &[EdgeId] {
        self.adjacency_in
            .get(id.0 as usize)
            .map(Vec::as_slice)
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
        self.dirty_salience[idx] = true;
        if let Some(node) = self.nodes[idx].as_mut() {
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
        self.dirty_accessed_at[idx] = true;
        if let Some(node) = self.nodes[idx].as_mut() {
            node.accessed_at = ts;
        }
        Ok(())
    }

    fn get_decay_checkpoint(&self, id: NodeId) -> Result<Timestamp, Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        Ok(self.decay_checkpoint[idx])
    }

    fn set_decay_checkpoint(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        self.decay_checkpoint[idx] = ts;
        self.dirty_decay_checkpoint[idx] = true;
        Ok(())
    }

    fn get_retained_action(&self, id: NodeId) -> Result<f64, Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        Ok(self.retained_action[idx])
    }

    fn set_retained_action(&mut self, id: NodeId, value: f64) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        // Reservoir is authoritative; recompute the salience projection
        // (ADR-0002 "commit recomputes projections" — intended for commit/tick).
        let salience = crate::mechanics::priors::project_salience(value);
        self.retained_action[idx] = value;
        self.dirty_retained_action[idx] = true;
        self.salience[idx] = salience;
        self.dirty_salience[idx] = true;
        if let Some(node) = self.nodes[idx].as_mut() {
            node.retained_action = value;
            node.salience = salience;
        }
        Ok(())
    }

    fn get_conductance(&self, id: EdgeId) -> Result<f64, Error> {
        let idx = id.0 as usize;
        if idx >= self.edges.len() || self.edges[idx].is_none() {
            return Err(Error::EdgeNotFound(id));
        }
        Ok(self.edge_conductance[idx])
    }

    fn set_conductance(&mut self, id: EdgeId, value: f64) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.edges.len() || self.edges[idx].is_none() {
            return Err(Error::EdgeNotFound(id));
        }
        // Reservoir is authoritative; recompute the weight projection
        // (ADR-0002 "commit recomputes projections" — intended for commit/tick).
        let weight = crate::mechanics::priors::project_weight(value);
        self.edge_conductance[idx] = value;
        self.dirty_edge_conductance[idx] = true;
        if let Some(edge) = self.edges[idx].as_mut() {
            edge.conductance = value;
            edge.weight = weight;
        }
        Ok(())
    }

    fn get_edge_accessed_at(&self, id: EdgeId) -> Result<Timestamp, Error> {
        let idx = id.0 as usize;
        if idx >= self.edges.len() || self.edges[idx].is_none() {
            return Err(Error::EdgeNotFound(id));
        }
        Ok(self.edge_accessed_at[idx])
    }

    fn set_edge_accessed_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.edges.len() || self.edges[idx].is_none() {
            return Err(Error::EdgeNotFound(id));
        }
        self.edge_accessed_at[idx] = ts;
        self.dirty_edge_accessed_at[idx] = true;
        if let Some(edge) = self.edges[idx].as_mut() {
            edge.accessed_at = ts;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        {
            let conn = self.lock_conn()?;
            conn.execute_batch("BEGIN IMMEDIATE;")
                .map_err(sqlite_error)?;

            let write_result = (|| -> Result<(), Error> {
                for (idx, dirty) in self.dirty_salience.iter().enumerate() {
                    if *dirty {
                        conn.execute(
                            "INSERT OR REPLACE INTO salience (node_id, salience) VALUES (?1, ?2)",
                            params![idx as u64, self.salience[idx]],
                        )
                        .map_err(sqlite_error)?;
                    }
                }

                for (idx, dirty) in self.dirty_accessed_at.iter().enumerate() {
                    if *dirty {
                        conn.execute(
                            "INSERT OR REPLACE INTO accessed_at (node_id, accessed_at) VALUES (?1, ?2)",
                            params![idx as u64, self.accessed_at[idx].0],
                        )
                        .map_err(sqlite_error)?;
                    }
                }

                for (idx, dirty) in self.dirty_decay_checkpoint.iter().enumerate() {
                    if *dirty {
                        conn.execute(
                            "INSERT OR REPLACE INTO decay_checkpoint (node_id, decay_checkpoint) VALUES (?1, ?2)",
                            params![idx as u64, self.decay_checkpoint[idx].0],
                        )
                        .map_err(sqlite_error)?;
                    }
                }

                for (idx, dirty) in self.dirty_retained_action.iter().enumerate() {
                    if *dirty {
                        conn.execute(
                            "INSERT OR REPLACE INTO retained_action (node_id, value) VALUES (?1, ?2)",
                            params![idx as u64, self.retained_action[idx]],
                        )
                        .map_err(sqlite_error)?;
                    }
                }

                for (idx, dirty) in self.dirty_edge_conductance.iter().enumerate() {
                    if *dirty {
                        // Persist the conductance reservoir AND its weight
                        // projection together (ADR-0002: weight tracks C_ij).
                        let weight =
                            self.edges[idx]
                                .as_ref()
                                .map(|e| e.weight)
                                .unwrap_or_else(|| {
                                    crate::mechanics::priors::project_weight(
                                        self.edge_conductance[idx],
                                    )
                                });
                        conn.execute(
                            "UPDATE edges SET conductance = ?2, weight = ?3 WHERE id = ?1",
                            params![idx as u64, self.edge_conductance[idx], weight],
                        )
                        .map_err(sqlite_error)?;
                    }
                }

                for (idx, dirty) in self.dirty_edge_accessed_at.iter().enumerate() {
                    if *dirty {
                        conn.execute(
                            "UPDATE edges SET accessed_at = ?2 WHERE id = ?1",
                            params![idx as u64, self.edge_accessed_at[idx].0],
                        )
                        .map_err(sqlite_error)?;
                    }
                }

                Ok(())
            })();

            if let Err(error) = write_result {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(error);
            }

            if let Err(error) = conn.execute_batch("COMMIT;").map_err(sqlite_error) {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(error);
            }
        }

        self.dirty_salience.fill(false);
        self.dirty_retained_action.fill(false);
        self.dirty_accessed_at.fill(false);
        self.dirty_decay_checkpoint.fill(false);
        self.dirty_edge_conductance.fill(false);
        self.dirty_edge_accessed_at.fill(false);
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
        self.query_node_ids(
            "SELECT node_id FROM entity_tags WHERE tag = ?1 ORDER BY node_id",
            tag,
        )
    }

    fn nodes_by_type(&self, kt: &KnowledgeType) -> Vec<NodeId> {
        self.query_node_ids(
            "SELECT id FROM nodes WHERE node_type = ?1 ORDER BY id",
            &encode_knowledge_type(kt),
        )
    }

    fn nodes_by_peer(&self, peer_id: PeerId) -> Vec<NodeId> {
        self.query_node_ids_u64(
            "SELECT id FROM nodes WHERE peer_id = ?1 ORDER BY id",
            peer_id.0,
        )
    }

    fn nodes_by_scope(&self, scope: &ScopePath) -> Vec<NodeId> {
        self.query_node_ids(
            "SELECT id FROM nodes WHERE scope = ?1 ORDER BY id",
            scope.as_str(),
        )
    }

    fn node_ids_descending(&self) -> Vec<NodeId> {
        let mut ids = self.all_node_ids();
        ids.sort_by_key(|id| std::cmp::Reverse(id.0));
        ids
    }

    fn node_ids_descending_limit(&self, limit: usize) -> Vec<NodeId> {
        if limit == 0 {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(limit);
        for (i, slot) in self.nodes.iter().enumerate().rev() {
            if slot.is_some() {
                result.push(NodeId(i as u64));
                if result.len() >= limit {
                    break;
                }
            }
        }
        result
    }

    fn text_search(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)> {
        if limit == 0 || query.trim().is_empty() {
            return Vec::new();
        }

        self.text_search_inner(query, limit).unwrap_or_default()
    }

    fn store_peer(&mut self, profile: &crate::peer::PeerProfile) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO peers \
             (id, name, trust_level, trust_reservoir, trust_evidence_count) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                profile.id.0,
                profile.name,
                encode_trust_level(&profile.trust_level),
                profile.trust_reservoir,
                profile.trust_evidence_count,
            ],
        )
        .map_err(sqlite_error)?;
        conn.execute(
            "DELETE FROM peer_aliases WHERE peer_id = ?1",
            [profile.id.0],
        )
        .map_err(sqlite_error)?;
        conn.execute(
            "INSERT OR IGNORE INTO peer_aliases (peer_id, alias, alias_type) VALUES (?1, ?2, 'name')",
            params![profile.id.0, profile.name],
        )
        .map_err(sqlite_error)?;
        for alias in &profile.aliases {
            conn.execute(
                "INSERT OR IGNORE INTO peer_aliases (peer_id, alias, alias_type) VALUES (?1, ?2, 'alias')",
                params![profile.id.0, alias],
            )
            .map_err(sqlite_error)?;
        }
        for (platform, username) in &profile.platforms {
            let alias_type = format!("platform:{platform}");
            conn.execute(
                "INSERT OR IGNORE INTO peer_aliases (peer_id, alias, alias_type) VALUES (?1, ?2, ?3)",
                params![profile.id.0, username, alias_type],
            )
            .map_err(sqlite_error)?;
        }
        Ok(())
    }

    fn store_peer_alias(
        &mut self,
        peer_id: PeerId,
        alias: &str,
        alias_type: &str,
    ) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO peer_aliases (peer_id, alias, alias_type) VALUES (?1, ?2, ?3)",
            params![peer_id.0, alias, alias_type],
        )
        .map_err(sqlite_error)?;
        Ok(())
    }

    fn load_peers(&self) -> Result<crate::peer::PeerRegistry, Error> {
        let conn = self.lock_conn()?;
        let mut registry = crate::peer::PeerRegistry::new();

        let mut stmt = conn
            .prepare(
                "SELECT id, name, trust_level, trust_reservoir, trust_evidence_count \
                 FROM peers ORDER BY id",
            )
            .map_err(sqlite_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, u64>(4)?,
                ))
            })
            .map_err(sqlite_error)?;

        for row in rows {
            let (id, name, trust_str, trust_reservoir, trust_evidence_count) =
                row.map_err(sqlite_error)?;
            let trust_level = decode_trust_level(&trust_str);
            let peer_id = PeerId(id);
            registry
                .register_peer_with_id(peer_id, &name, trust_level)
                .map_err(|e| Error::StorageError(format!("loading peer {id}: {e}")))?;
            // Restore the persisted evidence trust state (corroboration/feedback
            // moved the reservoir; do not re-seed from the coarse level).
            registry
                .set_trust_state(peer_id, trust_reservoir, trust_evidence_count)
                .map_err(|e| Error::StorageError(format!("loading peer trust {id}: {e}")))?;
        }

        let mut alias_stmt = conn
            .prepare("SELECT peer_id, alias, alias_type FROM peer_aliases ORDER BY peer_id")
            .map_err(sqlite_error)?;
        let alias_rows = alias_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(sqlite_error)?;

        for row in alias_rows {
            let (peer_id_raw, alias, alias_type) = row.map_err(sqlite_error)?;
            let peer_id = PeerId(peer_id_raw);
            if alias_type == "name" {
                continue;
            } else if alias_type == "alias" {
                let _ = registry.add_alias(peer_id, &alias);
            } else if let Some(platform) = alias_type.strip_prefix("platform:") {
                let _ = registry.add_platform(peer_id, platform, &alias);
            }
        }

        Ok(registry)
    }

    fn delete_peer(&mut self, peer_id: PeerId) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        conn.execute("DELETE FROM peer_aliases WHERE peer_id = ?1", [peer_id.0])
            .map_err(sqlite_error)?;
        conn.execute("DELETE FROM peers WHERE id = ?1", [peer_id.0])
            .map_err(sqlite_error)?;
        Ok(())
    }
}

impl SqliteStorage {
    fn text_search_inner(&self, query: &str, limit: usize) -> Result<Vec<(NodeId, f64)>, Error> {
        let exact = self.exact_text_matches(query, limit);
        if exact.len() >= limit {
            return Ok(exact);
        }

        let mut results = exact;
        let mut seen: std::collections::HashSet<NodeId> =
            results.iter().map(|(id, _)| *id).collect();
        let fts_query = make_fts_query(query);
        let remaining = limit.saturating_sub(results.len());
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, bm25(node_fts) AS rank \
                 FROM node_fts \
                 WHERE node_fts MATCH ?1 \
                 ORDER BY rank \
                 LIMIT ?2",
            )
            .map_err(sqlite_error)?;
        let rows = stmt
            .query_map(params![fts_query, remaining as u64], |row| {
                Ok((row.get::<_, u64>(0)?, row.get::<_, f64>(1)?))
            })
            .map_err(sqlite_error)?;

        for row in rows {
            let (raw_id, rank) = row.map_err(sqlite_error)?;
            let id = NodeId(raw_id);
            if seen.insert(id) && self.get_node(id).is_ok() {
                results.push((id, rank_to_score(rank)));
            }
        }

        results.truncate(limit);
        Ok(results)
    }

    fn exact_text_matches(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)> {
        let query_lower = query.to_lowercase();
        self.all_node_ids()
            .into_iter()
            .filter_map(|id| {
                self.get_node(id).ok().and_then(|node| {
                    if node.name.to_lowercase() == query_lower
                        || node.content.to_lowercase() == query_lower
                    {
                        Some((id, 1.0))
                    } else {
                        None
                    }
                })
            })
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::graph::MemoryTier;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;

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
            retained_action: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::peer::SourceKind::AgentObservation,
                session_id: "test-session".to_string(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    fn temp_db_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "anamnesis-{name}-{}-{}.sqlite",
            std::process::id(),
            Timestamp::now().0
        ));
        path
    }

    #[test]
    fn hot_field_setters_update_memory_and_mark_dirty() {
        let mut storage = SqliteStorage::new().expect("sqlite storage initializes");
        let id = storage.next_node_id();
        storage.set_node(make_node(id, 0.5)).expect("node stored");
        let idx = id.0 as usize;

        storage.set_salience(id, 0.42).expect("salience updated");
        assert_eq!(storage.get_salience(id).expect("salience exists"), 0.42);
        assert!(storage.dirty_salience[idx]);

        storage
            .set_accessed_at(id, Timestamp(2000))
            .expect("accessed_at updated");
        assert_eq!(
            storage.get_accessed_at(id).expect("accessed_at exists"),
            Timestamp(2000)
        );
        assert!(storage.dirty_accessed_at[idx]);

        storage
            .set_decay_checkpoint(id, Timestamp(3000))
            .expect("decay checkpoint updated");
        assert_eq!(
            storage
                .get_decay_checkpoint(id)
                .expect("decay checkpoint exists"),
            Timestamp(3000)
        );
        assert!(storage.dirty_decay_checkpoint[idx]);
    }

    #[test]
    fn reservoir_setters_update_projection_and_mark_dirty() {
        let mut storage = SqliteStorage::new().expect("sqlite storage initializes");
        let id = storage.next_node_id();
        storage.set_node(make_node(id, 0.5)).expect("node stored");
        let idx = id.0 as usize;

        let action = 1.25_f64;
        storage
            .set_retained_action(id, action)
            .expect("retained action updated");
        assert_eq!(storage.get_retained_action(id).unwrap(), action);
        // Reservoir is authoritative; salience is its projection.
        assert_eq!(
            storage.get_salience(id).unwrap(),
            crate::mechanics::priors::project_salience(action)
        );
        assert!(storage.dirty_retained_action[idx]);
        assert!(storage.dirty_salience[idx]);
        // Dense Node record must agree with the SoA arrays.
        let node = storage.get_node(id).unwrap();
        assert_eq!(node.retained_action, action);
        assert_eq!(
            node.salience,
            crate::mechanics::priors::project_salience(action)
        );
    }

    #[test]
    fn reservoirs_round_trip_through_flush_and_reopen() {
        let path = temp_db_path("flush-reservoirs");
        let (node_id, edge_id) = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let n0 = storage.next_node_id();
            let n1 = storage.next_node_id();
            storage.set_node(make_node(n0, 0.5)).expect("node 0 stored");
            storage.set_node(make_node(n1, 0.5)).expect("node 1 stored");

            let e0 = storage.next_edge_id();
            storage
                .set_edge(crate::graph::Edge {
                    id: e0,
                    source: n0,
                    target: n1,
                    edge_type: EdgeType::Semantic,
                    weight: 0.5,
                    conductance: 0.0,
                    edge_source: crate::graph::edge::EdgeSource::Auto,
                    created_at: Timestamp(1000),
                    accessed_at: Timestamp(1000),
                    valid_from: None,
                    valid_until: None,
                    metadata: HashMap::new(),
                })
                .expect("edge 0 stored");

            storage
                .set_retained_action(n0, 2.5)
                .expect("retained action set");
            storage.set_conductance(e0, -1.5).expect("conductance set");
            storage
                .set_edge_accessed_at(e0, Timestamp(5555))
                .expect("edge accessed_at set");

            let nidx = n0.0 as usize;
            let eidx = e0.0 as usize;
            assert!(storage.dirty_retained_action[nidx]);
            assert!(storage.dirty_edge_conductance[eidx]);
            assert!(storage.dirty_edge_accessed_at[eidx]);

            storage.flush().expect("reservoirs flush");

            assert!(!storage.dirty_retained_action[nidx]);
            assert!(!storage.dirty_edge_conductance[eidx]);
            assert!(!storage.dirty_edge_accessed_at[eidx]);
            (n0, e0)
        };

        let reopened = SqliteStorage::open(&path).expect("sqlite storage reopens");
        assert_eq!(reopened.get_retained_action(node_id).unwrap(), 2.5);
        assert_eq!(
            reopened.get_salience(node_id).unwrap(),
            crate::mechanics::priors::project_salience(2.5)
        );
        assert_eq!(reopened.get_conductance(edge_id).unwrap(), -1.5);
        assert_eq!(
            reopened.get_edge(edge_id).unwrap().weight,
            crate::mechanics::priors::project_weight(-1.5)
        );
        assert_eq!(
            reopened.get_edge_accessed_at(edge_id).unwrap(),
            Timestamp(5555)
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn flush_persists_dirty_hot_fields_and_clears_flags() {
        let path = temp_db_path("flush-hot-fields");
        let id = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let id = storage.next_node_id();
            storage.set_node(make_node(id, 0.5)).expect("node stored");

            storage.set_salience(id, 0.42).expect("salience updated");
            storage
                .set_accessed_at(id, Timestamp(2000))
                .expect("accessed_at updated");
            storage
                .set_decay_checkpoint(id, Timestamp(3000))
                .expect("decay checkpoint updated");

            let idx = id.0 as usize;
            assert!(storage.dirty_salience[idx]);
            assert!(storage.dirty_accessed_at[idx]);
            assert!(storage.dirty_decay_checkpoint[idx]);

            storage.flush().expect("dirty hot fields flush");

            assert!(!storage.dirty_salience[idx]);
            assert!(!storage.dirty_accessed_at[idx]);
            assert!(!storage.dirty_decay_checkpoint[idx]);
            id
        };

        let reopened = SqliteStorage::open(&path).expect("sqlite storage reopens");
        assert_eq!(reopened.get_salience(id).expect("salience exists"), 0.42);
        assert_eq!(
            reopened.get_accessed_at(id).expect("accessed_at exists"),
            Timestamp(2000)
        );
        assert_eq!(
            reopened
                .get_decay_checkpoint(id)
                .expect("decay checkpoint exists"),
            Timestamp(3000)
        );

        let _ = std::fs::remove_file(path);
    }
}

/// Current on-disk schema version. The fresh-DB `create_schema` path and the
/// incremental migration chain must converge on an IDENTICAL schema at this
/// version (same columns, same indexes).
const SCHEMA_VERSION: u32 = 4;

/// Run schema migrations to bring the database up to the current version.
///
/// Version history:
/// - v1 (implicit): original schema with `agent_id TEXT` column on nodes
/// - v2: `peer_id INTEGER` + `source_kind TEXT` replace `agent_id`; peers/peer_aliases tables added
/// - v3: `retained_action` reservoir table + edge `conductance`/`accessed_at`
///   reservoir columns (ADR-0002); valid-interval and salience-projection indexes
/// - v4 (current): peer evidence-trust columns `trust_reservoir REAL` +
///   `trust_evidence_count INTEGER` (social.md "Peer Trust"); seeded from the coarse
///   `trust_level` prior for existing peers
fn migrate_schema(conn: &Connection) -> Result<(), Error> {
    // Ensure schema_version table exists
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
        .map_err(sqlite_error)?;

    let version: Option<u32> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sqlite_error)?;

    match version {
        None => {
            // No schema_version row — check if nodes table exists (v1 legacy)
            let nodes_exist: bool = conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='nodes' LIMIT 1",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .map_err(sqlite_error)?
                .is_some();

            if nodes_exist {
                // Existing v1 database — migrate forward through the full chain.
                migrate_v1_to_v2(conn)?;
                migrate_v2_to_v3(conn)?;
                migrate_v3_to_v4(conn)?;
            } else {
                // Brand new database — create the current (v4) schema directly.
                create_schema(conn)?;
            }
            conn.execute_batch(&format!(
                "INSERT INTO schema_version (version) VALUES ({SCHEMA_VERSION});"
            ))
            .map_err(sqlite_error)?;
        }
        Some(1) => {
            migrate_v1_to_v2(conn)?;
            migrate_v2_to_v3(conn)?;
            migrate_v3_to_v4(conn)?;
            conn.execute_batch(&format!(
                "UPDATE schema_version SET version = {SCHEMA_VERSION};"
            ))
            .map_err(sqlite_error)?;
        }
        Some(2) => {
            migrate_v2_to_v3(conn)?;
            migrate_v3_to_v4(conn)?;
            conn.execute_batch(&format!(
                "UPDATE schema_version SET version = {SCHEMA_VERSION};"
            ))
            .map_err(sqlite_error)?;
        }
        Some(3) => {
            migrate_v3_to_v4(conn)?;
            conn.execute_batch(&format!(
                "UPDATE schema_version SET version = {SCHEMA_VERSION};"
            ))
            .map_err(sqlite_error)?;
        }
        Some(4) => {
            // Already at current version — ensure schema is complete (idempotent
            // CREATE IF NOT EXISTS only; no bare ALTER that would fail twice).
            create_schema(conn)?;
        }
        Some(v) => {
            return Err(Error::StorageError(format!(
                "unknown schema version {v}; this build supports up to v{SCHEMA_VERSION}"
            )));
        }
    }

    Ok(())
}

/// Migrate a v2 database to v3: add the `retained_action` reservoir table and
/// the edge `conductance`/`accessed_at` reservoir columns (ADR-0002), create the
/// valid-interval and salience-projection indexes, then DETERMINISTICALLY
/// backfill the reservoirs from the existing bounded projections.
///
/// The whole migration runs inside one transaction. The backfill is computed in
/// Rust via [`crate::mechanics::priors`] (SQLite is built without
/// `SQLITE_ENABLE_MATH_FUNCTIONS`, so the clamped-logit cannot run in SQL).
fn migrate_v2_to_v3(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        // Schema changes: reservoir table + edge reservoir columns + indexes.
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS retained_action (
                node_id INTEGER PRIMARY KEY,
                value REAL NOT NULL
            );

            ALTER TABLE edges ADD COLUMN conductance REAL NOT NULL DEFAULT 0;
            ALTER TABLE edges ADD COLUMN accessed_at INTEGER NOT NULL DEFAULT 0;

            -- New v3 indexes (valid-interval + salience projection).
            CREATE INDEX IF NOT EXISTS idx_nodes_valid ON nodes(valid_from, valid_until);
            CREATE INDEX IF NOT EXISTS idx_edges_valid ON edges(valid_from, valid_until);
            CREATE INDEX IF NOT EXISTS idx_salience ON salience(salience);

            -- Converge on the same index set as the fresh create_schema path,
            -- regardless of which earlier-version indexes were already present
            -- (all IF NOT EXISTS, so this is idempotent).
            CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_nodes_peer ON nodes(peer_id);
            CREATE INDEX IF NOT EXISTS idx_nodes_scope ON nodes(scope);
            CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_node);
            CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_node);
            CREATE INDEX IF NOT EXISTS idx_entity_tags_tag ON entity_tags(tag);
            ",
        )
        .map_err(sqlite_error)?;

        // Deterministic node reservoir backfill:
        //   retained_action.value = salience_to_action(salience)
        // Read every salience row, compute in Rust, write back.
        let salience_rows: Vec<(u64, f64)> = {
            let mut stmt = conn
                .prepare("SELECT node_id, salience FROM salience")
                .map_err(sqlite_error)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, u64>(0)?, row.get::<_, f64>(1)?)))
                .map_err(sqlite_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?
        };
        for (node_id, salience) in salience_rows {
            let action = crate::mechanics::priors::salience_to_action(salience);
            conn.execute(
                "INSERT OR REPLACE INTO retained_action (node_id, value) VALUES (?1, ?2)",
                params![node_id, action],
            )
            .map_err(sqlite_error)?;
        }

        // Deterministic edge reservoir backfill:
        //   conductance = weight_to_conductance(weight); accessed_at = created_at.
        let edge_rows: Vec<(u64, f64, u64)> = {
            let mut stmt = conn
                .prepare("SELECT id, weight, created_at FROM edges")
                .map_err(sqlite_error)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                })
                .map_err(sqlite_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?
        };
        for (edge_id, weight, created_at) in edge_rows {
            let conductance = crate::mechanics::priors::weight_to_conductance(weight);
            conn.execute(
                "UPDATE edges SET conductance = ?2, accessed_at = ?3 WHERE id = ?1",
                params![edge_id, conductance, created_at],
            )
            .map_err(sqlite_error)?;
        }

        Ok(())
    })();

    if let Err(error) = result {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    if let Err(error) = conn.execute_batch("COMMIT;").map_err(sqlite_error) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    Ok(())
}

/// Migrate a v3 database to v4: add the peer evidence-trust columns
/// `trust_reservoir REAL` and `trust_evidence_count INTEGER` (social.md "Peer
/// Trust"), then seed each existing peer's reservoir from its coarse `trust_level`
/// prior so cold-start behavior is unchanged. The whole migration is one transaction.
fn migrate_v3_to_v4(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        conn.execute_batch(
            "
            ALTER TABLE peers ADD COLUMN trust_reservoir REAL NOT NULL DEFAULT 0;
            ALTER TABLE peers ADD COLUMN trust_evidence_count INTEGER NOT NULL DEFAULT 0;
            ",
        )
        .map_err(sqlite_error)?;

        // Seed the reservoir from the coarse level prior (computed in Rust — the
        // mapping is a small lookup, SQLite has no helper for it).
        let peer_rows: Vec<(u64, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, trust_level FROM peers")
                .map_err(sqlite_error)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sqlite_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?
        };
        for (id, trust_str) in peer_rows {
            let prior = decode_trust_level(&trust_str).prior_trust_reservoir();
            conn.execute(
                "UPDATE peers SET trust_reservoir = ?2 WHERE id = ?1",
                params![id, prior],
            )
            .map_err(sqlite_error)?;
        }

        Ok(())
    })();

    if let Err(error) = result {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    if let Err(error) = conn.execute_batch("COMMIT;").map_err(sqlite_error) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    Ok(())
}

/// Migrate a v1 database (agent_id TEXT) to v2 (peer_id INTEGER + source_kind TEXT).
fn migrate_v1_to_v2(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "
        -- Add new columns to nodes (with defaults for existing rows)
        ALTER TABLE nodes ADD COLUMN peer_id INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE nodes ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'agent_observation';

        -- Add edge_source column to edges (with default for existing rows)
        ALTER TABLE edges ADD COLUMN edge_source TEXT NOT NULL DEFAULT 'auto';

        -- Create peers and peer_aliases tables
        CREATE TABLE IF NOT EXISTS peers (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            trust_level TEXT NOT NULL DEFAULT 'agent'
        );

        CREATE TABLE IF NOT EXISTS peer_aliases (
            peer_id INTEGER NOT NULL,
            alias TEXT NOT NULL,
            alias_type TEXT NOT NULL DEFAULT 'alias',
            PRIMARY KEY (peer_id, alias)
        );

        -- Create index on peer_id
        CREATE INDEX IF NOT EXISTS idx_nodes_peer ON nodes(peer_id);
        ",
    )
    .map_err(sqlite_error)
}

fn create_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = OFF;

        CREATE TABLE IF NOT EXISTS nodes (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            summary TEXT,
            content TEXT NOT NULL,
            embedding_json TEXT,
            node_type TEXT NOT NULL,
            peer_id INTEGER NOT NULL DEFAULT 0,
            source_kind TEXT NOT NULL DEFAULT 'agent_observation',
            session_id TEXT NOT NULL,
            scope TEXT NOT NULL,
            confidence REAL NOT NULL,
            valid_from INTEGER,
            valid_until INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            access_count INTEGER NOT NULL,
            access_history TEXT NOT NULL,
            tier TEXT NOT NULL,
            metadata TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS edges (
            id INTEGER PRIMARY KEY,
            from_node INTEGER NOT NULL,
            to_node INTEGER NOT NULL,
            edge_type TEXT NOT NULL,
            weight REAL NOT NULL,
            created_at INTEGER NOT NULL,
            valid_from INTEGER,
            valid_until INTEGER,
            metadata TEXT NOT NULL,
            edge_source TEXT NOT NULL DEFAULT 'auto',
            conductance REAL NOT NULL DEFAULT 0,
            accessed_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS salience (
            node_id INTEGER PRIMARY KEY,
            salience REAL NOT NULL
        );

        CREATE TABLE IF NOT EXISTS retained_action (
            node_id INTEGER PRIMARY KEY,
            value REAL NOT NULL
        );

        CREATE TABLE IF NOT EXISTS accessed_at (
            node_id INTEGER PRIMARY KEY,
            accessed_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS decay_checkpoint (
            node_id INTEGER PRIMARY KEY,
            decay_checkpoint INTEGER NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS node_fts USING fts5(
            id UNINDEXED,
            name,
            content
        );

        CREATE TABLE IF NOT EXISTS entity_tags (
            node_id INTEGER NOT NULL,
            tag TEXT NOT NULL,
            PRIMARY KEY (node_id, tag)
        );

        CREATE TABLE IF NOT EXISTS free_ids (
            id_type TEXT NOT NULL,
            id_value INTEGER NOT NULL,
            PRIMARY KEY (id_type, id_value)
        );

        CREATE TABLE IF NOT EXISTS peers (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            trust_level TEXT NOT NULL DEFAULT 'agent',
            trust_reservoir REAL NOT NULL DEFAULT 0,
            trust_evidence_count INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS peer_aliases (
            peer_id INTEGER NOT NULL,
            alias TEXT NOT NULL,
            alias_type TEXT NOT NULL DEFAULT 'alias',
            PRIMARY KEY (peer_id, alias)
        );

        CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(node_type);
        CREATE INDEX IF NOT EXISTS idx_nodes_peer ON nodes(peer_id);
        CREATE INDEX IF NOT EXISTS idx_nodes_scope ON nodes(scope);
        CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_node);
        CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_node);
        CREATE INDEX IF NOT EXISTS idx_entity_tags_tag ON entity_tags(tag);
        CREATE INDEX IF NOT EXISTS idx_nodes_valid ON nodes(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_edges_valid ON edges(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_salience ON salience(salience);
        ",
    )
    .map_err(sqlite_error)
}

fn insert_node_row(
    conn: &Connection,
    node: &Node,
    decay_checkpoint: Timestamp,
) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let write_result = (|| -> Result<(), Error> {
        conn.execute(
            "INSERT OR REPLACE INTO nodes (
                id, name, summary, content, embedding_json, node_type, peer_id, source_kind, session_id,
                scope, confidence, valid_from, valid_until, created_at, updated_at,
                access_count, access_history, tier, metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                node.id.0,
                node.name,
                node.summary,
                node.content,
                encode_embedding(node.embedding.as_deref()),
                encode_knowledge_type(&node.node_type),
                node.origin.peer_id.0,
                encode_source_kind(&node.origin.source_kind),
                node.origin.session_id,
                node.origin.scope.as_str(),
                node.origin.confidence,
                node.valid_from.map(|ts| ts.0),
                node.valid_until.map(|ts| ts.0),
                node.created_at.0,
                node.updated_at.0,
                node.access_count,
                encode_timestamp_deque(&node.access_history),
                encode_memory_tier(&node.tier),
                encode_map(&node.metadata),
            ],
        )
        .map_err(sqlite_error)?;
        conn.execute(
            "INSERT OR REPLACE INTO salience (node_id, salience) VALUES (?1, ?2)",
            params![node.id.0, node.salience],
        )
        .map_err(sqlite_error)?;
        conn.execute(
            "INSERT OR REPLACE INTO accessed_at (node_id, accessed_at) VALUES (?1, ?2)",
            params![node.id.0, node.accessed_at.0],
        )
        .map_err(sqlite_error)?;
        conn.execute(
            "INSERT OR REPLACE INTO decay_checkpoint (node_id, decay_checkpoint) VALUES (?1, ?2)",
            params![node.id.0, decay_checkpoint.0],
        )
        .map_err(sqlite_error)?;
        conn.execute(
            "INSERT OR REPLACE INTO retained_action (node_id, value) VALUES (?1, ?2)",
            params![node.id.0, node.retained_action],
        )
        .map_err(sqlite_error)?;
        conn.execute("DELETE FROM node_fts WHERE id = ?1", [node.id.0])
            .map_err(sqlite_error)?;
        conn.execute(
            "INSERT INTO node_fts (id, name, content) VALUES (?1, ?2, ?3)",
            params![node.id.0, node.name, node.content],
        )
        .map_err(sqlite_error)?;
        conn.execute("DELETE FROM entity_tags WHERE node_id = ?1", [node.id.0])
            .map_err(sqlite_error)?;
        for tag in unique_strings(&node.entity_tags) {
            conn.execute(
                "INSERT OR IGNORE INTO entity_tags (node_id, tag) VALUES (?1, ?2)",
                params![node.id.0, tag],
            )
            .map_err(sqlite_error)?;
        }
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    if let Err(error) = conn.execute_batch("COMMIT;").map_err(sqlite_error) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    Ok(())
}

fn insert_edge_row(conn: &Connection, edge: &Edge) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let write_result = (|| -> Result<(), Error> {
        conn.execute(
            "INSERT OR REPLACE INTO edges (
                id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata, edge_source, conductance, accessed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                edge.id.0,
                edge.source.0,
                edge.target.0,
                encode_edge_type(&edge.edge_type),
                edge.weight,
                edge.created_at.0,
                edge.valid_from.map(|ts| ts.0),
                edge.valid_until.map(|ts| ts.0),
                encode_map(&edge.metadata),
                encode_edge_source(&edge.edge_source),
                edge.conductance,
                edge.accessed_at.0,
            ],
        )
        .map_err(sqlite_error)?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    if let Err(error) = conn.execute_batch("COMMIT;").map_err(sqlite_error) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    Ok(())
}

/// A loaded node plus its SoA hot fields: `(node, salience, retained_action,
/// accessed_at, decay_checkpoint)`.
type LoadedNode = (Node, f64, f64, Timestamp, Timestamp);

fn load_nodes(conn: &Connection) -> Result<Vec<LoadedNode>, Error> {
    // All hot-field tables are read via LEFT JOIN so a node missing a hot-field
    // row (e.g. inserted before a flush, an incomplete transaction, or
    // corruption) degrades gracefully without node loss: a node present in the
    // `nodes` table always loads. Missing values fall back to defaults in Rust
    // (salience -> 0.0, accessed_at/decay_checkpoint -> Timestamp(0)).
    // The COALESCE onto the clamped-logit backfill of salience is also done in
    // Rust (SQLite is built without SQLITE_ENABLE_MATH_FUNCTIONS, so `LN` is
    // unavailable): NULL `r.value` falls back to `salience_to_action(salience)`.
    let mut stmt = conn
        .prepare(
            "SELECT
                n.id, n.name, n.summary, n.content, n.embedding_json, n.node_type,
                n.peer_id, n.source_kind, n.session_id, n.scope, n.confidence, n.valid_from,
                n.valid_until, n.created_at, n.updated_at, n.access_count,
                n.access_history, n.tier, n.metadata, s.salience, a.accessed_at,
                d.decay_checkpoint, r.value
             FROM nodes n
             LEFT JOIN salience s ON s.node_id = n.id
             LEFT JOIN accessed_at a ON a.node_id = n.id
             LEFT JOIN decay_checkpoint d ON d.node_id = n.id
             LEFT JOIN retained_action r ON r.node_id = n.id
             ORDER BY n.id",
        )
        .map_err(sqlite_error)?;

    let rows = stmt
        .query_map([], |row| {
            let id = NodeId(row.get::<_, u64>(0)?);
            let scope_raw: String = row.get(9)?;
            // Missing salience row (LEFT JOIN NULL) defaults to 0.0.
            let salience: f64 = row.get::<_, Option<f64>>(19)?.unwrap_or(0.0);
            // COALESCE(r.value, salience_to_action(salience)) — done in Rust.
            let retained_action: f64 = row
                .get::<_, Option<f64>>(22)?
                .unwrap_or_else(|| crate::mechanics::priors::salience_to_action(salience));
            // Missing accessed_at / decay_checkpoint rows default to Timestamp(0).
            let accessed_at = Timestamp(row.get::<_, Option<u64>>(20)?.unwrap_or(0));
            let decay_checkpoint = Timestamp(row.get::<_, Option<u64>>(21)?.unwrap_or(0));
            let node = Node {
                id,
                name: row.get(1)?,
                summary: row.get(2)?,
                content: row.get(3)?,
                embedding: decode_embedding(row.get::<_, Option<String>>(4)?)
                    .map_err(to_sql_error)?,
                node_type: decode_knowledge_type(&row.get::<_, String>(5)?)
                    .map_err(to_sql_error)?,
                origin: Origin {
                    peer_id: PeerId(row.get::<_, u64>(6)?),
                    source_kind: decode_source_kind(&row.get::<_, String>(7)?)
                        .map_err(to_sql_error)?,
                    session_id: row.get(8)?,
                    scope: decode_scope(&scope_raw).map_err(to_sql_error)?,
                    confidence: row.get(10)?,
                },
                valid_from: row.get::<_, Option<u64>>(11)?.map(Timestamp),
                valid_until: row.get::<_, Option<u64>>(12)?.map(Timestamp),
                created_at: Timestamp(row.get(13)?),
                updated_at: Timestamp(row.get(14)?),
                access_count: row.get(15)?,
                access_history: decode_timestamp_deque(&row.get::<_, String>(16)?)
                    .map_err(to_sql_error)?,
                tier: decode_memory_tier(&row.get::<_, String>(17)?).map_err(to_sql_error)?,
                metadata: decode_map(&row.get::<_, String>(18)?).map_err(to_sql_error)?,
                salience,
                retained_action,
                accessed_at,
                entity_tags: Vec::new(),
            };
            Ok((
                node,
                salience,
                retained_action,
                accessed_at,
                decay_checkpoint,
            ))
        })
        .map_err(sqlite_error)?;

    let mut nodes = rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?;
    for (node, _, _, _, _) in &mut nodes {
        node.entity_tags = load_entity_tags(conn, node.id)?;
    }
    Ok(nodes)
}

fn load_entity_tags(conn: &Connection, node_id: NodeId) -> Result<Vec<String>, Error> {
    let mut stmt = conn
        .prepare("SELECT tag FROM entity_tags WHERE node_id = ?1 ORDER BY tag")
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([node_id.0], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)
}

fn load_edges(conn: &Connection) -> Result<Vec<Edge>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata, edge_source, conductance, accessed_at
             FROM edges ORDER BY id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Edge {
                id: EdgeId(row.get(0)?),
                source: NodeId(row.get(1)?),
                target: NodeId(row.get(2)?),
                edge_type: decode_edge_type(&row.get::<_, String>(3)?).map_err(to_sql_error)?,
                weight: row.get(4)?,
                conductance: row.get(10)?,
                edge_source: decode_edge_source(&row.get::<_, String>(9)?).map_err(to_sql_error)?,
                created_at: Timestamp(row.get(5)?),
                accessed_at: Timestamp(row.get(11)?),
                valid_from: row.get::<_, Option<u64>>(6)?.map(Timestamp),
                valid_until: row.get::<_, Option<u64>>(7)?.map(Timestamp),
                metadata: decode_map(&row.get::<_, String>(8)?).map_err(to_sql_error)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)
}

fn load_free_ids(conn: &Connection, id_type: &str) -> Result<Vec<u64>, Error> {
    let mut stmt = conn
        .prepare("SELECT id_value FROM free_ids WHERE id_type = ?1 ORDER BY id_value")
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([id_type], |row| row.get::<_, u64>(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)
}

fn unique_strings(values: &[String]) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .filter_map(|value| {
            if seen.insert(value.as_str()) {
                Some(value.as_str())
            } else {
                None
            }
        })
        .collect()
}

fn encode_knowledge_type(value: &KnowledgeType) -> String {
    match value {
        KnowledgeType::IdentityCore => "identity_core".to_string(),
        KnowledgeType::IdentityLearned => "identity_learned".to_string(),
        KnowledgeType::IdentityState => "identity_state".to_string(),
        KnowledgeType::Semantic => "semantic".to_string(),
        KnowledgeType::Procedural => "procedural".to_string(),
        KnowledgeType::Entity => "entity".to_string(),
        KnowledgeType::Convention => "convention".to_string(),
        KnowledgeType::Decision => "decision".to_string(),
        KnowledgeType::Gotcha => "gotcha".to_string(),
        KnowledgeType::Hypothesis => "hypothesis".to_string(),
        KnowledgeType::Evidence => "evidence".to_string(),
        KnowledgeType::DebugSession => "debug_session".to_string(),
        KnowledgeType::Episodic => "episodic".to_string(),
        KnowledgeType::Event => "event".to_string(),
        KnowledgeType::Custom(name) => format!("custom:{}", escape_text(name)),
    }
}

fn decode_knowledge_type(value: &str) -> Result<KnowledgeType, Error> {
    Ok(match value {
        "identity_core" => KnowledgeType::IdentityCore,
        "identity_learned" => KnowledgeType::IdentityLearned,
        "identity_state" => KnowledgeType::IdentityState,
        "semantic" => KnowledgeType::Semantic,
        "procedural" => KnowledgeType::Procedural,
        "entity" => KnowledgeType::Entity,
        "convention" => KnowledgeType::Convention,
        "decision" => KnowledgeType::Decision,
        "gotcha" => KnowledgeType::Gotcha,
        "hypothesis" => KnowledgeType::Hypothesis,
        "evidence" => KnowledgeType::Evidence,
        "debug_session" => KnowledgeType::DebugSession,
        "episodic" => KnowledgeType::Episodic,
        "event" => KnowledgeType::Event,
        custom if custom.starts_with("custom:") => {
            KnowledgeType::Custom(unescape_text(&custom[7..])?)
        }
        other => {
            return Err(Error::StorageError(format!(
                "unknown knowledge type: {other}"
            )));
        }
    })
}

fn encode_edge_type(value: &EdgeType) -> String {
    match value {
        EdgeType::Semantic => "semantic".to_string(),
        EdgeType::Causal => "causal".to_string(),
        EdgeType::Temporal => "temporal".to_string(),
        EdgeType::Reason => "reason".to_string(),
        EdgeType::ReinforcedBy => "reinforced_by".to_string(),
        EdgeType::ConsolidatedFrom => "consolidated_from".to_string(),
        EdgeType::ExtractedFrom => "extracted_from".to_string(),
        EdgeType::Entity => "entity".to_string(),
        EdgeType::Supersedes => "supersedes".to_string(),
        EdgeType::RejectedAlternative => "rejected_alternative".to_string(),
        EdgeType::Supports => "supports".to_string(),
        EdgeType::Refutes => "refutes".to_string(),
        EdgeType::BelongsTo => "belongs_to".to_string(),
        EdgeType::Contradicts => "contradicts".to_string(),
        EdgeType::Custom(name) => format!("custom:{}", escape_text(name)),
    }
}

fn decode_edge_type(value: &str) -> Result<EdgeType, Error> {
    Ok(match value {
        "semantic" => EdgeType::Semantic,
        "causal" => EdgeType::Causal,
        "temporal" => EdgeType::Temporal,
        "reason" => EdgeType::Reason,
        "reinforced_by" => EdgeType::ReinforcedBy,
        "consolidated_from" => EdgeType::ConsolidatedFrom,
        "extracted_from" => EdgeType::ExtractedFrom,
        "entity" => EdgeType::Entity,
        "supersedes" => EdgeType::Supersedes,
        "rejected_alternative" => EdgeType::RejectedAlternative,
        "supports" => EdgeType::Supports,
        "refutes" => EdgeType::Refutes,
        "belongs_to" => EdgeType::BelongsTo,
        "contradicts" => EdgeType::Contradicts,
        custom if custom.starts_with("custom:") => EdgeType::Custom(unescape_text(&custom[7..])?),
        other => return Err(Error::StorageError(format!("unknown edge type: {other}"))),
    })
}

fn encode_memory_tier(value: &MemoryTier) -> &'static str {
    match value {
        MemoryTier::Auto => "auto",
        MemoryTier::Core => "core",
        MemoryTier::Recall => "recall",
        MemoryTier::Archival => "archival",
    }
}

fn decode_memory_tier(value: &str) -> Result<MemoryTier, Error> {
    Ok(match value {
        "auto" => MemoryTier::Auto,
        "core" => MemoryTier::Core,
        "recall" => MemoryTier::Recall,
        "archival" => MemoryTier::Archival,
        other => return Err(Error::StorageError(format!("unknown memory tier: {other}"))),
    })
}

fn encode_embedding(value: Option<&[f64]>) -> Option<String> {
    value.map(|items| {
        items
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
            .join(",")
    })
}

fn decode_embedding(value: Option<String>) -> Result<Option<Vec<f64>>, Error> {
    value
        .map(|encoded| {
            if encoded.is_empty() {
                return Ok(Vec::new());
            }
            encoded
                .split(',')
                .map(|part| {
                    part.parse::<f64>().map_err(|e| {
                        Error::StorageError(format!("invalid embedding value '{part}': {e}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
}

fn encode_timestamp_deque(value: &VecDeque<Timestamp>) -> String {
    value
        .iter()
        .map(|ts| ts.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn decode_timestamp_deque(value: &str) -> Result<VecDeque<Timestamp>, Error> {
    if value.is_empty() {
        return Ok(VecDeque::new());
    }
    value
        .split(',')
        .map(|part| {
            part.parse::<u64>()
                .map(Timestamp)
                .map_err(|e| Error::StorageError(format!("invalid timestamp '{part}': {e}")))
        })
        .collect()
}

fn encode_map(map: &HashMap<String, String>) -> String {
    let mut entries = map.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(left, _)| *left);
    entries
        .into_iter()
        .map(|(key, value)| format!("{}\t{}", escape_text(key), escape_text(value)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_map(value: &str) -> Result<HashMap<String, String>, Error> {
    let mut map = HashMap::new();
    if value.is_empty() {
        return Ok(map);
    }
    for line in value.split('\n') {
        let (key, raw_value) = line
            .split_once('\t')
            .ok_or_else(|| Error::StorageError("invalid metadata entry".to_string()))?;
        map.insert(unescape_text(key)?, unescape_text(raw_value)?);
    }
    Ok(map)
}

fn escape_text(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\t', "%09")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
}

fn unescape_text(value: &str) -> Result<String, Error> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(Error::StorageError("invalid percent escape".to_string()));
            }
            let hex = &value[index + 1..index + 3];
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|e| Error::StorageError(format!("invalid percent escape: {e}")))?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|e| Error::StorageError(format!("invalid utf-8 escape: {e}")))
}

fn encode_edge_source(source: &crate::graph::edge::EdgeSource) -> &'static str {
    match source {
        crate::graph::edge::EdgeSource::Auto => "auto",
        crate::graph::edge::EdgeSource::Manual => "manual",
        crate::graph::edge::EdgeSource::Inferred => "inferred",
    }
}

fn decode_edge_source(value: &str) -> Result<crate::graph::edge::EdgeSource, Error> {
    match value {
        "auto" => Ok(crate::graph::edge::EdgeSource::Auto),
        "manual" => Ok(crate::graph::edge::EdgeSource::Manual),
        "inferred" => Ok(crate::graph::edge::EdgeSource::Inferred),
        other => Err(Error::StorageError(format!("unknown edge_source: {other}"))),
    }
}

fn encode_source_kind(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::AgentObservation => "agent_observation",
        SourceKind::HumanInput => "human_input",
        SourceKind::DocumentExtract => "document_extract",
        SourceKind::SystemEvent => "system_event",
        SourceKind::Inferred => "inferred",
        SourceKind::External => "external",
    }
}

fn decode_source_kind(value: &str) -> Result<SourceKind, Error> {
    match value {
        "agent_observation" => Ok(SourceKind::AgentObservation),
        "human_input" => Ok(SourceKind::HumanInput),
        "document_extract" => Ok(SourceKind::DocumentExtract),
        "system_event" => Ok(SourceKind::SystemEvent),
        "inferred" => Ok(SourceKind::Inferred),
        "external" => Ok(SourceKind::External),
        other => Err(Error::StorageError(format!("unknown source_kind: {other}"))),
    }
}

fn encode_trust_level(level: &crate::peer::TrustLevel) -> &'static str {
    match level {
        crate::peer::TrustLevel::Owner => "owner",
        crate::peer::TrustLevel::Admin => "admin",
        crate::peer::TrustLevel::Member => "member",
        crate::peer::TrustLevel::Agent => "agent",
        crate::peer::TrustLevel::Observer => "observer",
        crate::peer::TrustLevel::Untrusted => "untrusted",
    }
}

fn decode_trust_level(value: &str) -> crate::peer::TrustLevel {
    match value {
        "owner" => crate::peer::TrustLevel::Owner,
        "admin" => crate::peer::TrustLevel::Admin,
        "member" => crate::peer::TrustLevel::Member,
        "agent" => crate::peer::TrustLevel::Agent,
        "observer" => crate::peer::TrustLevel::Observer,
        "untrusted" => crate::peer::TrustLevel::Untrusted,
        _ => crate::peer::TrustLevel::Agent,
    }
}

fn decode_scope(value: &str) -> Result<ScopePath, Error> {
    if value.is_empty() {
        Ok(ScopePath::universal())
    } else {
        ScopePath::new(value)
    }
}

fn make_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|part| format!("\"{}\"*", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn rank_to_score(rank: f64) -> f64 {
    (1.0 / (1.0 + rank.abs())).clamp(0.0, 1.0)
}

fn sqlite_error(error: rusqlite::Error) -> Error {
    Error::StorageError(error.to_string())
}

fn to_sql_error(error: Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

#[allow(dead_code)]
fn table_exists(conn: &Connection, table_name: &str) -> Result<bool, Error> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE name = ?1 LIMIT 1",
        [table_name],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(sqlite_error)
}
