//! SQLite storage adapter with FTS5 text search.

use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::{
    Edge, EdgeId, EdgeType, KnowledgeType, MemoryTier, Node, NodeId, ScopePath, Timestamp,
};
use crate::storage::StorageAdapter;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const EMPTY_EDGE_SLICE: &[EdgeId] = &[];

/// SQLite-backed storage adapter.
///
/// The adapter keeps graph objects and hot SoA fields cached in memory so the
/// `StorageAdapter` reference-returning API remains fast, while every explicit
/// setter persists the same state to SQLite. Full-text search is backed by FTS5.
pub struct SqliteStorage {
    conn: Mutex<Connection>,

    nodes: Vec<Option<Node>>,
    edges: Vec<Option<Edge>>,
    salience: Vec<f64>,
    accessed_at: Vec<Timestamp>,
    decay_checkpoint: Vec<Timestamp>,
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
        create_schema(&conn)?;

        let mut storage = Self {
            conn: Mutex::new(conn),
            nodes: Vec::new(),
            edges: Vec::new(),
            salience: Vec::new(),
            accessed_at: Vec::new(),
            decay_checkpoint: Vec::new(),
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
            self.accessed_at.resize(new_len, Timestamp(0));
            self.decay_checkpoint.resize(new_len, Timestamp(0));
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

        for (node, salience, accessed_at, decay_checkpoint) in nodes {
            let idx = node.id.0 as usize;
            self.ensure_node_capacity(idx);
            self.salience[idx] = salience;
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
        Ok(())
    }

    fn insert_node_row(&self, node: &Node, decay_checkpoint: Timestamp) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        insert_node_row(&conn, node, decay_checkpoint)
    }

    fn insert_edge_row(&self, edge: &Edge) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        insert_edge_row(&conn, edge)
    }

    fn query_node_ids(&self, sql: &str, value: &str) -> Vec<NodeId> {
        self.query_node_ids_inner(sql, value).unwrap_or_default()
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
}

impl Clone for SqliteStorage {
    fn clone(&self) -> Self {
        let cloned = Self::in_memory()
            .unwrap_or_else(|e| panic!("failed to clone sqlite storage into memory: {e}"));
        for id in self.all_node_ids() {
            let node = self
                .get_node(id)
                .unwrap_or_else(|e| panic!("failed to read node during sqlite clone: {e}"))
                .clone();
            cloned
                .insert_node_row(
                    &node,
                    self.get_decay_checkpoint(id).unwrap_or_else(|e| {
                        panic!("failed to read decay checkpoint during sqlite clone: {e}")
                    }),
                )
                .unwrap_or_else(|e| panic!("failed to write node during sqlite clone: {e}"));
        }
        for id in self.all_edge_ids() {
            let edge = self
                .get_edge(id)
                .unwrap_or_else(|e| panic!("failed to read edge during sqlite clone: {e}"))
                .clone();
            cloned
                .insert_edge_row(&edge)
                .unwrap_or_else(|e| panic!("failed to write edge during sqlite clone: {e}"));
        }
        {
            let conn = cloned
                .lock_conn()
                .unwrap_or_else(|e| panic!("failed to lock sqlite clone: {e}"));
            for id in &self.free_node_ids {
                conn.execute(
                    "INSERT INTO free_ids (id_type, id_value) VALUES ('node', ?1)",
                    [id.0],
                )
                .unwrap_or_else(|e| panic!("failed to clone free node id: {e}"));
            }
            for id in &self.free_edge_ids {
                conn.execute(
                    "INSERT INTO free_ids (id_type, id_value) VALUES ('edge', ?1)",
                    [id.0],
                )
                .unwrap_or_else(|e| panic!("failed to clone free edge id: {e}"));
            }
        }
        Self::from_connection(
            cloned
                .conn
                .into_inner()
                .unwrap_or_else(|_| panic!("failed to unwrap cloned sqlite connection")),
        )
        .unwrap_or_else(|e| panic!("failed to load cloned sqlite storage: {e}"))
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
        self.accessed_at[idx] = node.accessed_at;
        self.decay_checkpoint[idx] = node.accessed_at;
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
        self.accessed_at[idx] = Timestamp(0);
        self.decay_checkpoint[idx] = Timestamp(0);
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
        self.lock_conn()?
            .execute(
                "INSERT OR REPLACE INTO salience (node_id, salience) VALUES (?1, ?2)",
                params![id.0, salience],
            )
            .map_err(sqlite_error)?;
        self.salience[idx] = salience;
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
        self.lock_conn()?
            .execute(
                "INSERT OR REPLACE INTO accessed_at (node_id, accessed_at) VALUES (?1, ?2)",
                params![id.0, ts.0],
            )
            .map_err(sqlite_error)?;
        self.accessed_at[idx] = ts;
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
        self.lock_conn()?
            .execute(
                "INSERT OR REPLACE INTO decay_checkpoint (node_id, decay_checkpoint) VALUES (?1, ?2)",
                params![id.0, ts.0],
            )
            .map_err(sqlite_error)?;
        self.decay_checkpoint[idx] = ts;
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

    fn nodes_by_agent(&self, agent_id: &str) -> Vec<NodeId> {
        self.query_node_ids(
            "SELECT id FROM nodes WHERE agent_id = ?1 ORDER BY id",
            agent_id,
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

    fn text_search(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)> {
        if limit == 0 || query.trim().is_empty() {
            return Vec::new();
        }

        self.text_search_inner(query, limit).unwrap_or_default()
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
            agent_id TEXT NOT NULL,
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
            metadata TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS salience (
            node_id INTEGER PRIMARY KEY,
            salience REAL NOT NULL
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

        CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(node_type);
        CREATE INDEX IF NOT EXISTS idx_nodes_agent ON nodes(agent_id);
        CREATE INDEX IF NOT EXISTS idx_nodes_scope ON nodes(scope);
        CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_node);
        CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_node);
        CREATE INDEX IF NOT EXISTS idx_entity_tags_tag ON entity_tags(tag);
        ",
    )
    .map_err(sqlite_error)
}

fn insert_node_row(
    conn: &Connection,
    node: &Node,
    decay_checkpoint: Timestamp,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR REPLACE INTO nodes (
            id, name, summary, content, embedding_json, node_type, agent_id, session_id,
            scope, confidence, valid_from, valid_until, created_at, updated_at,
            access_count, access_history, tier, metadata
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            node.id.0,
            node.name,
            node.summary,
            node.content,
            encode_embedding(node.embedding.as_deref()),
            encode_knowledge_type(&node.node_type),
            node.origin.agent_id,
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
}

fn insert_edge_row(conn: &Connection, edge: &Edge) -> Result<(), Error> {
    conn.execute(
        "INSERT OR REPLACE INTO edges (
            id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn load_nodes(conn: &Connection) -> Result<Vec<(Node, f64, Timestamp, Timestamp)>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT
                n.id, n.name, n.summary, n.content, n.embedding_json, n.node_type,
                n.agent_id, n.session_id, n.scope, n.confidence, n.valid_from,
                n.valid_until, n.created_at, n.updated_at, n.access_count,
                n.access_history, n.tier, n.metadata, s.salience, a.accessed_at,
                d.decay_checkpoint
             FROM nodes n
             JOIN salience s ON s.node_id = n.id
             JOIN accessed_at a ON a.node_id = n.id
             JOIN decay_checkpoint d ON d.node_id = n.id
             ORDER BY n.id",
        )
        .map_err(sqlite_error)?;

    let rows = stmt
        .query_map([], |row| {
            let id = NodeId(row.get::<_, u64>(0)?);
            let scope_raw: String = row.get(8)?;
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
                    agent_id: row.get(6)?,
                    session_id: row.get(7)?,
                    scope: decode_scope(&scope_raw).map_err(to_sql_error)?,
                    confidence: row.get(9)?,
                },
                valid_from: row.get::<_, Option<u64>>(10)?.map(Timestamp),
                valid_until: row.get::<_, Option<u64>>(11)?.map(Timestamp),
                created_at: Timestamp(row.get(12)?),
                updated_at: Timestamp(row.get(13)?),
                access_count: row.get(14)?,
                access_history: decode_timestamp_deque(&row.get::<_, String>(15)?)
                    .map_err(to_sql_error)?,
                tier: decode_memory_tier(&row.get::<_, String>(16)?).map_err(to_sql_error)?,
                metadata: decode_map(&row.get::<_, String>(17)?).map_err(to_sql_error)?,
                salience: row.get(18)?,
                accessed_at: Timestamp(row.get(19)?),
                entity_tags: Vec::new(),
            };
            Ok((
                node,
                row.get::<_, f64>(18)?,
                Timestamp(row.get(19)?),
                Timestamp(row.get(20)?),
            ))
        })
        .map_err(sqlite_error)?;

    let mut nodes = rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?;
    for (node, _, _, _) in &mut nodes {
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
            "SELECT id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata
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
                created_at: Timestamp(row.get(5)?),
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
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
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
        .map(|part| format!("\"{}\"", part.replace('"', "\"\"")))
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
