//! SQLite storage adapter with FTS5 text search.

use crate::error::Error;
use crate::graph::node::Origin;
use crate::graph::types::PeerId;
use crate::graph::types::SourceKind;
use crate::graph::{
    AccessTrace, Edge, EdgeId, EdgeType, KnowledgeType, MemoryTier, Node, NodeId, ScopePath,
    Timestamp,
};
use crate::storage::StorageAdapter;
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const EMPTY_EDGE_SLICE: &[EdgeId] = &[];

const EMBEDDING_MIGRATION_PHASE_KEY: &str = "embedding.migration.phase";
const EMBEDDING_MIGRATION_SOURCE_MODEL_KEY: &str = "embedding.migration.source_model";
const EMBEDDING_MIGRATION_SOURCE_DIM_KEY: &str = "embedding.migration.source_dim";
const EMBEDDING_MIGRATION_TARGET_MODEL_KEY: &str = "embedding.migration.target_model";
const EMBEDDING_MIGRATION_TARGET_DIM_KEY: &str = "embedding.migration.target_dim";
const EMBEDDING_MIGRATION_SELECTION_KEY: &str = "embedding.migration.selection";
const EMBEDDING_MIGRATION_CURSOR_KEY: &str = "embedding.migration.cursor";
const EMBEDDING_MIGRATION_BACKUP_PATH_KEY: &str = "embedding.migration.backup_path";
const EMBEDDING_MIGRATION_PHASE: &str = "in-progress";
const EMBEDDING_MIGRATION_KEYS: [&str; 8] = [
    EMBEDDING_MIGRATION_PHASE_KEY,
    EMBEDDING_MIGRATION_SOURCE_MODEL_KEY,
    EMBEDDING_MIGRATION_SOURCE_DIM_KEY,
    EMBEDDING_MIGRATION_TARGET_MODEL_KEY,
    EMBEDDING_MIGRATION_TARGET_DIM_KEY,
    EMBEDDING_MIGRATION_SELECTION_KEY,
    EMBEDDING_MIGRATION_CURSOR_KEY,
    EMBEDDING_MIGRATION_BACKUP_PATH_KEY,
];

/// A failure while creating or verifying a durable SQLite backup.
#[derive(Debug)]
pub enum BackupError {
    /// The destination could not be created with create-new semantics.
    Io(io::Error),
    /// SQLite could not copy or inspect the database.
    Storage(Error),
    /// The copied database failed `PRAGMA quick_check`.
    IntegrityCheckFailed(String),
    /// A failed backup could not be removed.
    Cleanup {
        /// The original backup failure.
        backup: String,
        /// The cleanup failure.
        cleanup: io::Error,
    },
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "backup I/O error: {error}"),
            Self::Storage(error) => write!(formatter, "backup {error}"),
            Self::IntegrityCheckFailed(result) => {
                write!(formatter, "backup quick_check returned '{result}'")
            }
            Self::Cleanup { backup, cleanup } => {
                write!(
                    formatter,
                    "{backup}; failed to remove incomplete backup: {cleanup}"
                )
            }
        }
    }
}

impl std::error::Error for BackupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::IntegrityCheckFailed(_) => None,
            Self::Cleanup { cleanup, .. } => Some(cleanup),
        }
    }
}

impl From<io::Error> for BackupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// How migration candidates are selected during resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddingSelection {
    /// Select missing vectors and vectors whose dimension differs from the target.
    Dimension,
    /// Select every node after the last atomically committed node ID.
    Cursor,
}

/// Durable state needed to resume an embedding migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingMigrationCheckpoint {
    /// Model recorded before migration, when known.
    pub source_model: Option<String>,
    /// Embedding dimension observed before migration, when known.
    pub source_dim: Option<usize>,
    /// Model to stamp only after full persisted-vector validation.
    pub target_model: String,
    /// Required dimension of every completed vector.
    pub target_dim: usize,
    /// Resume selection strategy.
    pub selection: EmbeddingSelection,
    /// Last committed node in cursor mode.
    pub cursor: Option<NodeId>,
    /// Verified durable backup protecting the migration.
    pub backup_path: PathBuf,
}

/// A node whose embedding must be regenerated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingCandidate {
    /// Stable graph node ID.
    pub node_id: NodeId,
    /// Source-of-truth text to pass to the embedding provider.
    pub content: String,
    /// Current persisted vector dimension, or `None` when missing.
    pub embedding_dim: Option<usize>,
}

/// A validated replacement vector for one node.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingReplacement {
    /// Node receiving the replacement.
    pub node_id: NodeId,
    /// Complete replacement vector.
    pub embedding: Vec<f64>,
}

/// One atomically committed set of replacement vectors.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingBatch {
    /// Vectors to persist and then apply to the in-memory cache.
    pub replacements: Vec<EmbeddingReplacement>,
    /// Cursor persisted in the same transaction as the vectors.
    pub next_cursor: Option<NodeId>,
}

/// Read-only migration state inspected without running normal schema opening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingMigrationInspection {
    /// Persisted embedding-model stamp, when present.
    pub embedding_model: Option<String>,
    /// Per-node embedding dimensions in ascending node-ID order.
    pub embedding_dimensions: Vec<Option<usize>>,
    /// Durable migration checkpoint, when present.
    pub checkpoint: Option<EmbeddingMigrationCheckpoint>,
}

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
    evidence_prior: Vec<f64>,
    accessed_at: Vec<Timestamp>,
    decay_checkpoint: Vec<Timestamp>,
    edge_conductance: Vec<f64>,
    edge_accessed_at: Vec<Timestamp>,
    edge_leaked_at: Vec<Timestamp>,
    dirty_salience: Vec<bool>,
    dirty_retained_action: Vec<bool>,
    dirty_evidence_prior: Vec<bool>,
    dirty_accessed_at: Vec<bool>,
    dirty_decay_checkpoint: Vec<bool>,
    dirty_edge_conductance: Vec<bool>,
    dirty_edge_accessed_at: Vec<bool>,
    dirty_edge_leaked_at: Vec<bool>,
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
    const EMBEDDING_MODEL_KEY: &str = "embedding_model";

    /// Create an in-memory SQLite storage backend.
    pub fn new() -> Result<Self, Error> {
        Self::in_memory()
    }

    /// Create an in-memory SQLite storage backend.
    pub fn in_memory() -> Result<Self, Error> {
        Self::from_connection(Connection::open_in_memory().map_err(sqlite_error)?)
    }

    /// Read all `embedding.migration.*` metadata pairs verbatim for the clone
    /// path (raw copy: no validation, so a mid-migration checkpoint is carried
    /// byte-for-byte even when partially written).
    fn migration_metadata_pairs(&self) -> Result<Vec<(String, String)>, Error> {
        let conn = self.lock_conn()?;
        let mut statement = conn
            .prepare("SELECT key, value FROM graph_metadata WHERE key LIKE 'embedding.migration.%'")
            .map_err(sqlite_error)?;
        let pairs = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        Ok(pairs)
    }

    /// Open or create a SQLite-backed storage file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_connection(Connection::open(path).map_err(sqlite_error)?)
    }

    /// Inspect embedding state through a read-only raw connection without migrating schemas.
    pub fn inspect_embedding_migration(
        path: impl AsRef<Path>,
    ) -> Result<EmbeddingMigrationInspection, Error> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite_error)?;
        if !table_exists(&conn, "nodes")? {
            return Err(Error::StorageError(
                "embedding inspection requires a nodes table".to_string(),
            ));
        }

        let embedding_model = if table_exists(&conn, "graph_metadata")? {
            metadata_value(&conn, Self::EMBEDDING_MODEL_KEY)?
        } else {
            None
        };
        let checkpoint = if table_exists(&conn, "graph_metadata")? {
            read_embedding_migration_checkpoint(&conn)?
        } else {
            None
        };
        let encoded_embeddings = {
            let mut statement = conn
                .prepare("SELECT embedding_json FROM nodes ORDER BY id")
                .map_err(sqlite_error)?;
            statement
                .query_map([], |row| row.get::<_, Option<String>>(0))
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?
        };
        let embedding_dimensions = encoded_embeddings
            .into_iter()
            .map(decode_embedding)
            .map(|embedding| embedding.map(|value| value.map(|vector| vector.len())))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(EmbeddingMigrationInspection {
            embedding_model,
            embedding_dimensions,
            checkpoint,
        })
    }

    /// Create a no-overwrite online backup and verify it with `PRAGMA quick_check`.
    pub fn create_verified_backup(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<(), BackupError> {
        let source = source.as_ref();
        let destination = destination.as_ref();
        let reserved = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        drop(reserved);

        let backup_result = (|| -> Result<(), BackupError> {
            let source_conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| BackupError::Storage(sqlite_error(error)))?;
            let mut destination_conn =
                Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_WRITE)
                    .map_err(|error| BackupError::Storage(sqlite_error(error)))?;
            {
                let backup = Backup::new(&source_conn, &mut destination_conn)
                    .map_err(|error| BackupError::Storage(sqlite_error(error)))?;
                backup
                    .run_to_completion(128, Duration::from_millis(5), None)
                    .map_err(|error| BackupError::Storage(sqlite_error(error)))?;
            }
            drop(destination_conn);

            let verification =
                Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|error| BackupError::Storage(sqlite_error(error)))?;
            let quick_check = verification
                .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .map_err(|error| BackupError::Storage(sqlite_error(error)))?;
            if quick_check != "ok" {
                return Err(BackupError::IntegrityCheckFailed(quick_check));
            }
            Ok(())
        })();

        if let Err(error) = backup_result {
            if let Err(cleanup) = std::fs::remove_file(destination) {
                return Err(BackupError::Cleanup {
                    backup: error.to_string(),
                    cleanup,
                });
            }
            return Err(error);
        }
        Ok(())
    }

    /// Return the embedding model recorded for this graph, if any.
    pub fn embedding_model_name(&self) -> Result<Option<String>, Error> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT value FROM graph_metadata WHERE key = ?1",
            [Self::EMBEDDING_MODEL_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)
    }

    /// Record the embedding model used to create this graph's vector space.
    pub fn set_embedding_model_name(&mut self, model: &str) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO graph_metadata (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![Self::EMBEDDING_MODEL_KEY, model],
        )
        .map_err(sqlite_error)?;
        Ok(())
    }

    /// Return the durable embedding-migration checkpoint, if one is active.
    pub fn embedding_migration_checkpoint(
        &self,
    ) -> Result<Option<EmbeddingMigrationCheckpoint>, Error> {
        let conn = self.lock_conn()?;
        read_embedding_migration_checkpoint(&conn)
    }

    /// Persist a complete migration checkpoint in one immediate transaction.
    pub fn begin_embedding_migration(
        &mut self,
        checkpoint: &EmbeddingMigrationCheckpoint,
    ) -> Result<(), Error> {
        validate_embedding_migration_checkpoint(checkpoint)?;
        let backup_path = checkpoint.backup_path.to_str().ok_or_else(|| {
            Error::InvalidInput(
                "embedding.migration.backup_path must contain valid UTF-8".to_string(),
            )
        })?;
        let mut conn = self.lock_conn()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        clear_embedding_migration_metadata(&transaction)?;
        set_metadata(
            &transaction,
            EMBEDDING_MIGRATION_PHASE_KEY,
            EMBEDDING_MIGRATION_PHASE,
        )?;
        if let Some(source_model) = checkpoint.source_model.as_deref() {
            set_metadata(
                &transaction,
                EMBEDDING_MIGRATION_SOURCE_MODEL_KEY,
                source_model,
            )?;
        }
        if let Some(source_dim) = checkpoint.source_dim {
            set_metadata(
                &transaction,
                EMBEDDING_MIGRATION_SOURCE_DIM_KEY,
                &source_dim.to_string(),
            )?;
        }
        set_metadata(
            &transaction,
            EMBEDDING_MIGRATION_TARGET_MODEL_KEY,
            &checkpoint.target_model,
        )?;
        set_metadata(
            &transaction,
            EMBEDDING_MIGRATION_TARGET_DIM_KEY,
            &checkpoint.target_dim.to_string(),
        )?;
        set_metadata(
            &transaction,
            EMBEDDING_MIGRATION_SELECTION_KEY,
            encode_embedding_selection(checkpoint.selection),
        )?;
        if let Some(cursor) = checkpoint.cursor {
            set_metadata(
                &transaction,
                EMBEDDING_MIGRATION_CURSOR_KEY,
                &cursor.0.to_string(),
            )?;
        }
        set_metadata(
            &transaction,
            EMBEDDING_MIGRATION_BACKUP_PATH_KEY,
            backup_path,
        )?;
        transaction.commit().map_err(sqlite_error)
    }

    /// Return at most `limit` candidates in deterministic ascending node-ID order.
    pub fn embedding_candidates(
        &self,
        checkpoint: &EmbeddingMigrationCheckpoint,
        limit: usize,
    ) -> Result<Vec<EmbeddingCandidate>, Error> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        validate_embedding_migration_checkpoint(checkpoint)?;
        let rows = {
            let conn = self.lock_conn()?;
            let mut statement = conn
                .prepare("SELECT id, content, embedding_json FROM nodes ORDER BY id")
                .map_err(sqlite_error)?;
            statement
                .query_map([], |row| {
                    Ok((
                        NodeId(row.get::<_, u64>(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?
        };
        let mut candidates = Vec::with_capacity(limit.min(rows.len()));
        for (node_id, content, encoded_embedding) in rows {
            let embedding = decode_embedding(encoded_embedding)?;
            let selected = match checkpoint.selection {
                EmbeddingSelection::Dimension => embedding
                    .as_ref()
                    .is_none_or(|vector| vector.len() != checkpoint.target_dim),
                EmbeddingSelection::Cursor => {
                    checkpoint.cursor.is_none_or(|cursor| node_id.0 > cursor.0)
                }
            };
            if selected {
                candidates.push(EmbeddingCandidate {
                    node_id,
                    content,
                    embedding_dim: embedding.as_ref().map(Vec::len),
                });
                if candidates.len() == limit {
                    break;
                }
            }
        }
        Ok(candidates)
    }

    /// Atomically commit replacement vectors and the migration cursor.
    pub fn commit_embedding_batch(&mut self, batch: EmbeddingBatch) -> Result<(), Error> {
        let checkpoint = self.embedding_migration_checkpoint()?.ok_or_else(|| {
            Error::InvalidInput("embedding migration checkpoint is not active".to_string())
        })?;
        for replacement in &batch.replacements {
            if replacement.embedding.len() != checkpoint.target_dim {
                return Err(Error::InvalidInput(format!(
                    "node {} replacement has dimension {}, expected {}",
                    replacement.node_id.0,
                    replacement.embedding.len(),
                    checkpoint.target_dim
                )));
            }
            if !replacement.embedding.iter().all(|value| value.is_finite()) {
                return Err(Error::InvalidInput(format!(
                    "node {} replacement contains a non-finite embedding value",
                    replacement.node_id.0
                )));
            }
            let index = usize::try_from(replacement.node_id.0)
                .map_err(|_| Error::NodeNotFound(replacement.node_id))?;
            if self.nodes.get(index).and_then(Option::as_ref).is_none() {
                return Err(Error::NodeNotFound(replacement.node_id));
            }
        }

        let mut conn = self.lock_conn()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        for replacement in &batch.replacements {
            let encoded = encode_embedding(Some(&replacement.embedding)).ok_or_else(|| {
                Error::StorageError("replacement embedding failed to encode".to_string())
            })?;
            let updated = transaction
                .execute(
                    "UPDATE nodes SET embedding_json = ?1 WHERE id = ?2",
                    params![encoded, replacement.node_id.0],
                )
                .map_err(sqlite_error)?;
            if updated != 1 {
                return Err(Error::NodeNotFound(replacement.node_id));
            }
        }
        if let Some(cursor) = batch.next_cursor {
            set_metadata(
                &transaction,
                EMBEDDING_MIGRATION_CURSOR_KEY,
                &cursor.0.to_string(),
            )?;
        }
        transaction.commit().map_err(sqlite_error)?;
        drop(conn);

        for replacement in batch.replacements {
            let index = usize::try_from(replacement.node_id.0)
                .map_err(|_| Error::NodeNotFound(replacement.node_id))?;
            let node = self
                .nodes
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or(Error::NodeNotFound(replacement.node_id))?;
            node.embedding = Some(replacement.embedding);
        }
        Ok(())
    }

    /// Validate all persisted vectors, stamp the target model, and clear checkpoint keys.
    pub fn finish_embedding_migration(&mut self) -> Result<(), Error> {
        let mut conn = self.lock_conn()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Exclusive)
            .map_err(sqlite_error)?;
        let checkpoint = read_embedding_migration_checkpoint(&transaction)?.ok_or_else(|| {
            Error::InvalidInput("embedding migration checkpoint is not active".to_string())
        })?;
        let persisted = {
            let mut statement = transaction
                .prepare("SELECT id, embedding_json FROM nodes ORDER BY id")
                .map_err(sqlite_error)?;
            statement
                .query_map([], |row| {
                    Ok((
                        NodeId(row.get::<_, u64>(0)?),
                        row.get::<_, Option<String>>(1)?,
                    ))
                })
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?
        };
        for (node_id, encoded_embedding) in persisted {
            let embedding = decode_embedding(encoded_embedding)?.ok_or_else(|| {
                Error::InvalidInput(format!(
                    "node {} is missing an embedding during migration completion",
                    node_id.0
                ))
            })?;
            if embedding.len() != checkpoint.target_dim {
                return Err(Error::InvalidInput(format!(
                    "node {} has dimension {}, expected {} during migration completion",
                    node_id.0,
                    embedding.len(),
                    checkpoint.target_dim
                )));
            }
            if !embedding.iter().all(|value| value.is_finite()) {
                return Err(Error::InvalidInput(format!(
                    "node {} contains a non-finite embedding during migration completion",
                    node_id.0
                )));
            }
        }
        set_metadata(
            &transaction,
            Self::EMBEDDING_MODEL_KEY,
            &checkpoint.target_model,
        )?;
        clear_embedding_migration_metadata(&transaction)?;
        transaction.commit().map_err(sqlite_error)
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
            evidence_prior: Vec::new(),
            accessed_at: Vec::new(),
            decay_checkpoint: Vec::new(),
            edge_conductance: Vec::new(),
            edge_accessed_at: Vec::new(),
            edge_leaked_at: Vec::new(),
            dirty_salience: vec![false; capacity],
            dirty_retained_action: vec![false; capacity],
            dirty_evidence_prior: vec![false; capacity],
            dirty_accessed_at: vec![false; capacity],
            dirty_decay_checkpoint: vec![false; capacity],
            dirty_edge_conductance: vec![false; capacity],
            dirty_edge_accessed_at: vec![false; capacity],
            dirty_edge_leaked_at: vec![false; capacity],
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
            self.evidence_prior.resize(new_len, 0.0);
            self.accessed_at.resize(new_len, Timestamp(0));
            self.decay_checkpoint.resize(new_len, Timestamp(0));
            self.dirty_salience.resize(new_len, false);
            self.dirty_retained_action.resize(new_len, false);
            self.dirty_evidence_prior.resize(new_len, false);
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
            self.edge_leaked_at.resize(new_len, Timestamp(0));
            self.dirty_edge_conductance.resize(new_len, false);
            self.dirty_edge_accessed_at.resize(new_len, false);
            self.dirty_edge_leaked_at.resize(new_len, false);
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
            self.evidence_prior[idx] = node.evidence_prior;
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
            self.edge_leaked_at[idx] = edge.leaked_at;
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
        self.dirty_evidence_prior = vec![false; self.nodes.len()];
        self.dirty_accessed_at = vec![false; self.nodes.len()];
        self.dirty_decay_checkpoint = vec![false; self.nodes.len()];
        self.evidence_prior.resize(self.nodes.len(), 0.0);
        // Size edge SoA + reset edge dirty arrays to the final edge capacity.
        self.edge_conductance.resize(self.edges.len(), 0.0);
        self.edge_accessed_at.resize(self.edges.len(), Timestamp(0));
        self.edge_leaked_at.resize(self.edges.len(), Timestamp(0));
        self.dirty_edge_conductance = vec![false; self.edges.len()];
        self.dirty_edge_accessed_at = vec![false; self.edges.len()];
        self.dirty_edge_leaked_at = vec![false; self.edges.len()];
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
        self.try_clone()
            .unwrap_or_else(|e| panic!("failed to clone sqlite storage: {e}"))
    }
}

impl StorageAdapter for SqliteStorage {
    fn try_clone(&self) -> Result<Self, Error> {
        let cloned = Self::in_memory()?;

        {
            let conn = cloned.lock_conn()?;

            conn.execute_batch("BEGIN IMMEDIATE;")
                .map_err(sqlite_error)?;

            let write_result = (|| -> Result<(), Error> {
                if let Some(model) = self.embedding_model_name()? {
                    conn.execute(
                        "INSERT INTO graph_metadata (key, value) VALUES (?1, ?2)",
                        params![Self::EMBEDDING_MODEL_KEY, model],
                    )
                    .map_err(sqlite_error)?;
                }

                for (key, value) in self.migration_metadata_pairs()? {
                    conn.execute(
                        "INSERT INTO graph_metadata (key, value) VALUES (?1, ?2)",
                        params![key, value],
                    )
                    .map_err(sqlite_error)?;
                }

                for id in self.all_node_ids() {
                    let node = self.get_node(id)?.clone();
                    let decay_checkpoint = self.get_decay_checkpoint(id)?;

                    conn.execute(
                        "INSERT OR REPLACE INTO nodes (
                            id, name, summary, content, embedding_json, node_type, peer_id, source_kind, session_id,
                            scope, confidence, valid_from, valid_until, created_at, updated_at,
                            access_count, access_history, tier, metadata, evidence_prior
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
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
                            encode_access_history(&node.access_history),
                            encode_memory_tier(&node.tier),
                            encode_map(&node.metadata),
                            node.evidence_prior,
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
                }

                for id in self.all_edge_ids() {
                    let edge = self.get_edge(id)?.clone();

                    conn.execute(
                        "INSERT OR REPLACE INTO edges (
                            id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata, edge_source, conductance, accessed_at, leaked_at
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                            edge.leaked_at.0,
                        ],
                    )
                    .map_err(sqlite_error)?;
                }

                for id in &self.free_node_ids {
                    conn.execute(
                        "INSERT INTO free_ids (id_type, id_value) VALUES ('node', ?1)",
                        [id.0],
                    )
                    .map_err(sqlite_error)?;
                }

                for id in &self.free_edge_ids {
                    conn.execute(
                        "INSERT INTO free_ids (id_type, id_value) VALUES ('edge', ?1)",
                        [id.0],
                    )
                    .map_err(sqlite_error)?;
                }

                Ok(())
            })();

            if let Err(error) = write_result {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(error);
            }

            if let Err(e) = conn.execute_batch("COMMIT;") {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(sqlite_error(e));
            }
        }

        let mut result = Self::from_connection(cloned.conn.into_inner().map_err(|_| {
            Error::StorageError("failed to unwrap cloned sqlite connection".to_string())
        })?)?;
        result.next_node_counter = result.next_node_counter.max(self.next_node_counter);
        result.next_edge_counter = result.next_edge_counter.max(self.next_edge_counter);
        Ok(result)
    }

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
        self.evidence_prior[idx] = node.evidence_prior;
        self.accessed_at[idx] = node.accessed_at;
        self.decay_checkpoint[idx] = node.accessed_at;
        self.dirty_salience[idx] = false;
        self.dirty_retained_action[idx] = false;
        self.dirty_evidence_prior[idx] = false;
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
        self.evidence_prior[idx] = 0.0;
        self.accessed_at[idx] = Timestamp(0);
        self.decay_checkpoint[idx] = Timestamp(0);
        self.dirty_salience[idx] = false;
        self.dirty_retained_action[idx] = false;
        self.dirty_evidence_prior[idx] = false;
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
        self.edge_leaked_at[idx] = edge.leaked_at;
        self.dirty_edge_conductance[idx] = false;
        self.dirty_edge_accessed_at[idx] = false;
        self.dirty_edge_leaked_at[idx] = false;
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
        self.edge_leaked_at[idx] = Timestamp(0);
        self.dirty_edge_conductance[idx] = false;
        self.dirty_edge_accessed_at[idx] = false;
        self.dirty_edge_leaked_at[idx] = false;
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

    fn get_access_history(&self, id: NodeId) -> Result<&VecDeque<AccessTrace>, Error> {
        let idx = id.0 as usize;
        self.nodes
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .map(|node| &node.access_history)
            .ok_or(Error::NodeNotFound(id))
    }

    fn append_access_trace(&mut self, id: NodeId, trace: AccessTrace) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        // Append the trace (with its pre-computed per-trace decay) to the bounded
        // 32-trace window in memory.
        let encoded = if let Some(node) = self.nodes[idx].as_mut() {
            node.record_access(trace);
            encode_access_history(&node.access_history)
        } else {
            return Err(Error::NodeNotFound(id));
        };
        // B_i is recomputed from access_history at projection time, so the trace
        // must be durably persisted now (write-through on the nodes row column).
        {
            let conn = self.lock_conn()?;
            conn.execute(
                "UPDATE nodes SET access_history = ?2 WHERE id = ?1",
                params![id.0, encoded],
            )
            .map_err(sqlite_error)?;
        }
        Ok(())
    }

    fn get_evidence_prior(&self, id: NodeId) -> Result<f64, Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        Ok(self.evidence_prior[idx])
    }

    fn set_evidence_prior(&mut self, id: NodeId, prior: f64) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].is_none() {
            return Err(Error::NodeNotFound(id));
        }
        self.evidence_prior[idx] = prior;
        self.dirty_evidence_prior[idx] = true;
        if let Some(node) = self.nodes[idx].as_mut() {
            node.evidence_prior = prior;
        }
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
        // A committed use is, by definition, not idle: clear any outstanding
        // idle-leak debt by resetting the leak checkpoint to the same instant
        // (trait-level contract; keeps the two fields distinct but synced on
        // every "use" event).
        self.edge_leaked_at[idx] = ts;
        self.dirty_edge_leaked_at[idx] = true;
        if let Some(edge) = self.edges[idx].as_mut() {
            edge.accessed_at = ts;
            edge.leaked_at = ts;
        }
        Ok(())
    }

    fn get_edge_leaked_at(&self, id: EdgeId) -> Result<Timestamp, Error> {
        let idx = id.0 as usize;
        if idx >= self.edges.len() || self.edges[idx].is_none() {
            return Err(Error::EdgeNotFound(id));
        }
        Ok(self.edge_leaked_at[idx])
    }

    fn set_edge_leaked_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error> {
        let idx = id.0 as usize;
        if idx >= self.edges.len() || self.edges[idx].is_none() {
            return Err(Error::EdgeNotFound(id));
        }
        self.edge_leaked_at[idx] = ts;
        self.dirty_edge_leaked_at[idx] = true;
        if let Some(edge) = self.edges[idx].as_mut() {
            edge.leaked_at = ts;
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

                for (idx, dirty) in self.dirty_evidence_prior.iter().enumerate() {
                    if *dirty {
                        // `evidence_prior` is a column on the `nodes` table (P_i is
                        // a decay-exempt persistent reservoir, ADR-0008).
                        conn.execute(
                            "UPDATE nodes SET evidence_prior = ?2 WHERE id = ?1",
                            params![idx as u64, self.evidence_prior[idx]],
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

                for (idx, dirty) in self.dirty_edge_leaked_at.iter().enumerate() {
                    if *dirty {
                        conn.execute(
                            "UPDATE edges SET leaked_at = ?2 WHERE id = ?1",
                            params![idx as u64, self.edge_leaked_at[idx].0],
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
        self.dirty_evidence_prior.fill(false);
        self.dirty_accessed_at.fill(false);
        self.dirty_decay_checkpoint.fill(false);
        self.dirty_edge_conductance.fill(false);
        self.dirty_edge_accessed_at.fill(false);
        self.dirty_edge_leaked_at.fill(false);
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
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::graph::types::SourceKind::AgentObservation,
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
                    leaked_at: Timestamp(1000),
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

    #[test]
    fn access_history_encode_decode_round_trip_is_lossless() {
        // The per-trace decay `d_j` is an f64; `to_bits`/`from_bits` must round-trip
        // it EXACTLY (bit-for-bit), including awkward values, so a re-opened DB
        // reproduces the activation-dependent decays without drift.
        let mut history: VecDeque<AccessTrace> = VecDeque::new();
        history.push_back(AccessTrace {
            at: Timestamp(1_000),
            decay: 0.16, // m_type·α for Semantic
        });
        history.push_back(AccessTrace {
            at: Timestamp(86_401_000),
            decay: 0.234_567_890_123_456_7,
        });
        history.push_back(AccessTrace {
            at: Timestamp(u64::MAX),
            decay: 0.0, // Core: permanent
        });

        let encoded = encode_access_history(&history);
        // Format sanity: ";"-separated "ts,decaybits" entries.
        assert_eq!(encoded.split(';').count(), 3);
        let decoded = decode_access_history(&encoded).expect("decode round-trips");
        assert_eq!(decoded.len(), history.len());
        for (orig, got) in history.iter().zip(decoded.iter()) {
            assert_eq!(orig.at, got.at, "timestamp must round-trip");
            assert_eq!(
                orig.decay.to_bits(),
                got.decay.to_bits(),
                "per-trace decay must round-trip bit-for-bit"
            );
        }

        // Empty deque ↔ empty string.
        let empty: VecDeque<AccessTrace> = VecDeque::new();
        assert_eq!(encode_access_history(&empty), "");
        assert!(decode_access_history("").expect("empty decodes").is_empty());
    }

    #[test]
    fn append_access_trace_persists_per_trace_decay() {
        // A committed access appends a trace whose decay is durably persisted and
        // recovered on re-open.
        let path = temp_db_path("append-trace");
        let id = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let id = storage.next_node_id();
            storage.set_node(make_node(id, 0.5)).expect("node stored");
            storage
                .append_access_trace(
                    id,
                    AccessTrace {
                        at: Timestamp(5_000),
                        decay: 0.16,
                    },
                )
                .expect("trace appended");
            storage.flush().expect("flush");
            id
        };

        let reopened = SqliteStorage::open(&path).expect("sqlite storage reopens");
        let hist = reopened.get_access_history(id).expect("history exists");
        assert_eq!(hist.len(), 1);
        let trace = hist.front().expect("one trace");
        assert_eq!(trace.at, Timestamp(5_000));
        assert_eq!(trace.decay.to_bits(), 0.16_f64.to_bits());

        let _ = std::fs::remove_file(path);
    }

    /// Legacy-DB compat guard: a database written by another build may carry
    /// `node_type` strings that are not known variants in THIS build — either
    /// consumer strings from an older/newer schema, or (after the 0.10.0
    /// KnowledgeType shrink) the wire strings of variants this build has since
    /// deleted. Reopening such a DB must NOT fail: every unrecognized bare string
    /// decodes to `KnowledgeType::Custom(<original>)` so the node loads and the
    /// graph opens.
    ///
    /// The planted strings here are deliberately ones NOT in the current match
    /// arms (this task deletes no variants, so a currently-known string like
    /// `hypothesis` would still decode to its own variant — see
    /// `known_node_types_are_untouched_by_fallback`). They stand in for exactly
    /// the class of strings a later deletion will move from a known arm into the
    /// fallback; the guard's behavior on them is identical before and after that
    /// deletion.
    #[test]
    fn unknown_node_types_decode_as_custom_on_reopen() {
        let path = temp_db_path("legacy-node-types");
        // Bare strings unknown to this build. Two classes, both must land as
        // `Custom(<original>)`, none carry the `custom:` prefix:
        //   1. now-REAL legacy wire strings of variants deleted in the 0.10.0
        //      KnowledgeType 15→4 collapse — these are exactly the strings the
        //      v6→v7 migration rewrites to `custom:<string>` on reopen, and which
        //      the decode fallback would also catch on any un-migrated row;
        //   2. never-were-variant consumer strings from another schema, caught
        //      purely by the decode fallback (no migration arm touches them).
        let legacy = [
            // (1) deleted-variant wire strings, now legacy:
            "gotcha",
            "decision",
            "procedural",
            "entity",
            "convention",
            "event",
            "hypothesis",
            "evidence",
            "debug_session",
            // (2) never-were-variant consumer strings:
            "coding_standard",
            "footgun",
            "identity_snapshot",
        ];

        // Seed real, well-formed nodes through the normal path, then flush so the
        // rows exist in the `nodes` table.
        let ids: Vec<NodeId> = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let ids: Vec<NodeId> = legacy
                .iter()
                .map(|_| {
                    let id = storage.next_node_id();
                    storage.set_node(make_node(id, 0.5)).expect("node stored");
                    id
                })
                .collect();
            storage.flush().expect("flush");
            ids
        };

        // Plant the legacy type strings directly via raw SQL, bypassing encode.
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            for (id, ty) in ids.iter().zip(legacy.iter()) {
                conn.execute(
                    "UPDATE nodes SET node_type = ?2 WHERE id = ?1",
                    params![id.0, *ty],
                )
                .expect("planted legacy node_type");
            }
        }

        // Reopen through the normal path: this must succeed despite unknown types.
        let reopened = SqliteStorage::open(&path).expect("reopen tolerates legacy node types");

        // Every planted node loads as Custom(<original string>).
        for (id, ty) in ids.iter().zip(legacy.iter()) {
            let node_type = reopened.get_node_type(*id).expect("node type present");
            assert_eq!(
                node_type,
                &KnowledgeType::Custom((*ty).to_string()),
                "legacy type {ty:?} should decode to Custom",
            );
        }

        // Full node sweep must not error on any node.
        for id in reopened.all_node_ids() {
            reopened
                .get_node(id)
                .expect("get_node clean over full sweep");
        }

        let _ = std::fs::remove_file(path);
    }

    /// Decode contract for the collapsed 4-variant `KnowledgeType`
    /// (Episodic/Semantic/Identity/Custom):
    /// - the three surviving fixed wire strings decode to their exact variant;
    /// - the three legacy identity tiers keep identity semantics — explicit arms
    ///   fold `identity_core`/`identity_learned`/`identity_state` into `Identity`
    ///   rather than letting them ride the generic `Custom` fallback;
    /// - every removed knowledge/memory wire string now falls through the B0
    ///   fallback to `Custom(<original>)` (no explicit arm), which the v6→v7
    ///   migration mirrors by rewriting the stored string to `custom:<string>`;
    /// - the explicit `custom:` prefix path is unaffected.
    #[test]
    fn known_node_types_are_untouched_by_fallback() {
        // Surviving fixed variants + the two legacy-identity classes that decode to
        // their exact (merged) variant, never `Custom`.
        let exact = [
            ("identity", KnowledgeType::Identity),
            ("identity_core", KnowledgeType::Identity),
            ("identity_learned", KnowledgeType::Identity),
            ("identity_state", KnowledgeType::Identity),
            ("semantic", KnowledgeType::Semantic),
            ("episodic", KnowledgeType::Episodic),
        ];
        for (wire, expected) in exact {
            assert_eq!(
                decode_knowledge_type(wire).expect("known type decodes"),
                expected,
                "wire string {wire:?} must decode to its exact variant",
            );
        }

        // Removed-variant wire strings now decode to Custom via the B0 fallback.
        for wire in [
            "procedural",
            "entity",
            "convention",
            "decision",
            "gotcha",
            "hypothesis",
            "evidence",
            "debug_session",
            "event",
        ] {
            assert_eq!(
                decode_knowledge_type(wire).expect("removed type decodes"),
                KnowledgeType::Custom(wire.to_string()),
                "removed wire string {wire:?} must fall back to Custom",
            );
        }

        // The explicit custom encoding still round-trips (and is NOT confused with
        // the bare-string fallback).
        assert_eq!(
            decode_knowledge_type(&encode_knowledge_type(&KnowledgeType::Custom(
                "my type".to_string()
            )))
            .expect("custom decodes"),
            KnowledgeType::Custom("my type".to_string()),
        );
    }

    /// A fallback-decoded legacy node must survive a further open/save cycle
    /// without further corruption. The first reopen normalizes a bare unknown
    /// string (`retired_type`) into the canonical `Custom("retired_type")`, which
    /// re-encodes as `custom:retired_type`; a second flush + reopen is then a
    /// fixed point (bare -> Custom is a one-way normalization; every later cycle
    /// is stable). This is the property that makes the guard safe: re-saving a
    /// DB opened via the fallback never mangles the type further.
    #[test]
    fn fallback_decoded_node_type_round_trips_stably() {
        let path = temp_db_path("legacy-node-types-roundtrip");
        let id = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let id = storage.next_node_id();
            storage.set_node(make_node(id, 0.5)).expect("node stored");
            storage.flush().expect("flush");
            id
        };
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            conn.execute(
                "UPDATE nodes SET node_type = 'retired_type' WHERE id = ?1",
                params![id.0],
            )
            .expect("planted legacy node_type");
        }

        // First reopen: bare "retired_type" -> Custom("retired_type"). Re-persist
        // it (re-encode now writes `custom:retired_type`).
        {
            let mut storage = SqliteStorage::open(&path).expect("first reopen tolerates legacy");
            assert_eq!(
                storage.get_node_type(id).expect("type present"),
                &KnowledgeType::Custom("retired_type".to_string()),
            );
            // Force a rewrite of the node row via the normal persistence path.
            let mut node = storage.get_node(id).expect("node present").clone();
            node.content = "touched".to_string();
            storage.set_node(node).expect("node re-stored");
            storage.flush().expect("flush re-encoded node");
        }

        // Confirm the on-disk encoding is now the canonical custom form.
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            let encoded: String = conn
                .query_row(
                    "SELECT node_type FROM nodes WHERE id = ?1",
                    params![id.0],
                    |row| row.get(0),
                )
                .expect("read encoded node_type");
            assert_eq!(encoded, "custom:retired_type");
        }

        // Second reopen is a fixed point: still Custom("retired_type").
        let reopened = SqliteStorage::open(&path).expect("second reopen");
        assert_eq!(
            reopened.get_node_type(id).expect("type present"),
            &KnowledgeType::Custom("retired_type".to_string()),
        );

        let _ = std::fs::remove_file(path);
    }

    /// `decode_memory_tier` must tolerate unknown/legacy tier strings by falling
    /// back to `MemoryTier::Auto`, mirroring the node-type compat guard, so a
    /// legacy DB with a dropped tier string still opens.
    #[test]
    fn decode_memory_tier_falls_back_to_auto_on_unknown() {
        // Known strings keep their exact meaning.
        assert_eq!(decode_memory_tier("auto").unwrap(), MemoryTier::Auto);
        assert_eq!(decode_memory_tier("core").unwrap(), MemoryTier::Core);
        assert_eq!(decode_memory_tier("recall").unwrap(), MemoryTier::Recall);
        assert_eq!(
            decode_memory_tier("archival").unwrap(),
            MemoryTier::Archival
        );
        // Unknown strings fall back to Auto instead of erroring.
        assert_eq!(decode_memory_tier("legacy_tier").unwrap(), MemoryTier::Auto);
        assert_eq!(decode_memory_tier("").unwrap(), MemoryTier::Auto);
    }

    /// Carried B0 finding + option (a) resolution: the v6→v7 normalization migration
    /// must rewrite legacy `node_type` strings on reopen so `nodes_by_type` — which
    /// filters SQL by the *encoded* query string — finds them immediately, with no
    /// eventual-consistency gap.
    ///
    /// Plants a v6-era DB carrying a bare `gotcha` (a removed variant's wire string)
    /// and the three legacy identity tiers, reopens through the normal migration
    /// path, and asserts both the on-disk normalization and the lookup:
    ///   - `gotcha`  → stored as `custom:gotcha`, found by `nodes_by_type(Custom("gotcha"))`;
    ///   - `identity_core|learned|state` → stored as `identity`, found by `nodes_by_type(Identity)`.
    #[test]
    fn migration_v7_normalizes_legacy_node_types_for_nodes_by_type() {
        let path = temp_db_path("v7-normalize-nodes-by-type");

        // Seed well-formed rows through the normal path, then flush so the rows
        // exist in the `nodes` table.
        let (gotcha_id, identity_ids) = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let gotcha_id = storage.next_node_id();
            storage
                .set_node(make_node(gotcha_id, 0.5))
                .expect("gotcha node stored");
            let identity_ids: Vec<NodeId> = (0..3)
                .map(|_| {
                    let id = storage.next_node_id();
                    storage
                        .set_node(make_node(id, 0.5))
                        .expect("id node stored");
                    id
                })
                .collect();
            storage.flush().expect("flush");
            (gotcha_id, identity_ids)
        };

        // Rewind to a v6-era DB: plant the legacy bare wire strings directly and set
        // the recorded schema version back to 6, so reopening runs the v6→v7 step.
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            conn.execute(
                "UPDATE nodes SET node_type = 'gotcha' WHERE id = ?1",
                params![gotcha_id.0],
            )
            .expect("planted legacy gotcha");
            let legacy_identity = ["identity_core", "identity_learned", "identity_state"];
            for (id, ty) in identity_ids.iter().zip(legacy_identity.iter()) {
                conn.execute(
                    "UPDATE nodes SET node_type = ?2 WHERE id = ?1",
                    params![id.0, *ty],
                )
                .expect("planted legacy identity tier");
            }
            conn.execute_batch("UPDATE schema_version SET version = 6;")
                .expect("rewind schema_version to 6");
        }

        // Reopen: the v6→v7 migration normalizes the column in place.
        let reopened = SqliteStorage::open(&path).expect("reopen runs v6->v7 migration");

        // On-disk column is normalized to the canonical encodings.
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            let gotcha_enc: String = conn
                .query_row(
                    "SELECT node_type FROM nodes WHERE id = ?1",
                    params![gotcha_id.0],
                    |row| row.get(0),
                )
                .expect("read gotcha node_type");
            assert_eq!(gotcha_enc, "custom:gotcha", "gotcha row normalized");
            for id in &identity_ids {
                let enc: String = conn
                    .query_row(
                        "SELECT node_type FROM nodes WHERE id = ?1",
                        params![id.0],
                        |row| row.get(0),
                    )
                    .expect("read identity node_type");
                assert_eq!(
                    enc, "identity",
                    "identity tier row normalized to bare identity"
                );
            }
        }

        // The carried finding: nodes_by_type now FINDS the normalized rows.
        assert_eq!(
            reopened.nodes_by_type(&KnowledgeType::Custom("gotcha".to_string())),
            vec![gotcha_id],
            "nodes_by_type(Custom(\"gotcha\")) must find the normalized legacy row",
        );
        let mut found_identity = reopened.nodes_by_type(&KnowledgeType::Identity);
        found_identity.sort();
        let mut expected_identity = identity_ids.clone();
        expected_identity.sort();
        assert_eq!(
            found_identity, expected_identity,
            "nodes_by_type(Identity) must find all normalized legacy identity rows",
        );

        // In-memory decode agrees with storage.
        assert_eq!(
            reopened
                .get_node_type(gotcha_id)
                .expect("gotcha type present"),
            &KnowledgeType::Custom("gotcha".to_string()),
        );

        let _ = std::fs::remove_file(path);
    }

    /// v7→v8: the v6→v7 step only normalized the *fixed* legacy list, so an
    /// ARBITRARY bare non-canonical `node_type` — a type string written by a
    /// foreign/future writer, e.g. `foo_type` — still loaded as `Custom("foo_type")`
    /// but stayed invisible to `nodes_by_type(Custom("foo_type"))` (which filters SQL
    /// by the *encoded* `custom:foo_type`) until re-saved. The v8 migration rewrites
    /// every such bare string to its canonical `custom:*` encoding on reopen.
    ///
    /// Both a plain (`foo_type`) and an adversarial (`weird%type`, carrying an
    /// `escape_text` metacharacter) bare string are planted: the adversarial case
    /// proves the rewrite goes through Rust `encode_knowledge_type` (which escapes
    /// `%` → `%25`) rather than a literal SQL `'custom:' || node_type` concat, which
    /// would produce `custom:weird%type` and never match the escaped query string.
    #[test]
    fn migration_v8_normalizes_arbitrary_bare_node_types_for_nodes_by_type() {
        let path = temp_db_path("v8-normalize-arbitrary");

        // Seed two well-formed rows through the normal path, then flush.
        let (plain_id, adversarial_id) = {
            let mut storage = SqliteStorage::open(&path).expect("sqlite storage opens");
            let plain_id = storage.next_node_id();
            storage
                .set_node(make_node(plain_id, 0.5))
                .expect("plain node stored");
            let adversarial_id = storage.next_node_id();
            storage
                .set_node(make_node(adversarial_id, 0.5))
                .expect("adversarial node stored");
            storage.flush().expect("flush");
            (plain_id, adversarial_id)
        };

        // Rewind to a v7-era DB: plant the arbitrary bare wire strings directly (the
        // exact form a foreign writer would leave) and set schema_version back to 7,
        // so reopening runs the v7→v8 step. The v6→v7 list does NOT cover these, so
        // running the full chain to v7 leaves them untouched — only v8 rewrites them.
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            conn.execute(
                "UPDATE nodes SET node_type = 'foo_type' WHERE id = ?1",
                params![plain_id.0],
            )
            .expect("planted plain bare type");
            conn.execute(
                "UPDATE nodes SET node_type = 'weird%type' WHERE id = ?1",
                params![adversarial_id.0],
            )
            .expect("planted adversarial bare type");
            conn.execute_batch("UPDATE schema_version SET version = 7;")
                .expect("rewind schema_version to 7");
        }

        // Reopen: the v7→v8 migration normalizes the column in place.
        let reopened = SqliteStorage::open(&path).expect("reopen runs v7->v8 migration");

        // On-disk column is normalized to the canonical `custom:*` encoding — with
        // the metacharacter escaped for the adversarial row.
        {
            let conn = Connection::open(&path).expect("raw conn opens");
            let plain_enc: String = conn
                .query_row(
                    "SELECT node_type FROM nodes WHERE id = ?1",
                    params![plain_id.0],
                    |row| row.get(0),
                )
                .expect("read plain node_type");
            assert_eq!(plain_enc, "custom:foo_type", "plain bare row normalized");
            let adversarial_enc: String = conn
                .query_row(
                    "SELECT node_type FROM nodes WHERE id = ?1",
                    params![adversarial_id.0],
                    |row| row.get(0),
                )
                .expect("read adversarial node_type");
            assert_eq!(
                adversarial_enc, "custom:weird%25type",
                "adversarial bare row must be escaped via encode_knowledge_type, not raw concat"
            );
        }

        // The finding: nodes_by_type now FINDS the normalized rows.
        assert_eq!(
            reopened.nodes_by_type(&KnowledgeType::Custom("foo_type".to_string())),
            vec![plain_id],
            "nodes_by_type(Custom(\"foo_type\")) must find the normalized row",
        );
        assert_eq!(
            reopened.nodes_by_type(&KnowledgeType::Custom("weird%type".to_string())),
            vec![adversarial_id],
            "nodes_by_type(Custom(\"weird%type\")) must find the escaped normalized row",
        );

        // Decode is a fixed point: the canonical form still yields the original value.
        assert_eq!(
            reopened
                .get_node_type(plain_id)
                .expect("plain type present"),
            &KnowledgeType::Custom("foo_type".to_string()),
        );
        assert_eq!(
            reopened
                .get_node_type(adversarial_id)
                .expect("adversarial type present"),
            &KnowledgeType::Custom("weird%type".to_string()),
        );

        let _ = std::fs::remove_file(path);
    }

    /// Bug #5: SQLite FTS5 `bm25()` returns MORE-NEGATIVE values for BETTER
    /// matches. `rank_to_score` must therefore be monotone-INCREASING in
    /// `(-rank)` so a strong match (the query term dense in a short document)
    /// outranks a weak match (the term appearing once amid a lot of filler),
    /// bounded to `[0, 1]`. The old `1.0 / (1.0 + rank.abs())` formula treats
    /// magnitude alone, so it inverts the ranking: the strong match's more
    /// negative rank has the LARGER absolute value, which the old formula
    /// scores LOWER.
    #[test]
    fn bm25_score_ranks_strong_match_above_weak_match() {
        let mut storage = SqliteStorage::new().expect("sqlite storage initializes");

        let strong_id = storage.next_node_id();
        let mut strong = make_node(strong_id, 0.5);
        strong.name = "Distributed consensus notes".to_string();
        strong.content =
            "raft raft raft raft raft raft leader election and log replication".to_string();
        storage.set_node(strong).expect("strong node stored");

        let weak_id = storage.next_node_id();
        let mut weak = make_node(weak_id, 0.5);
        weak.name = "Unrelated gardening notes".to_string();
        weak.content = "a very long passage about gardening and cooking recipes that only \
            mentions raft one single time somewhere deep inside a lot of filler padding \
            text meant to dilute relevance across many many unrelated words of content \
            that goes on and on and on for quite a while before finally trailing off"
            .to_string();
        storage.set_node(weak).expect("weak node stored");

        let results = storage.text_search("raft", 10);
        let strong_score = results
            .iter()
            .find(|(id, _)| *id == strong_id)
            .map(|(_, score)| *score)
            .expect("strong match must be found");
        let weak_score = results
            .iter()
            .find(|(id, _)| *id == weak_id)
            .map(|(_, score)| *score)
            .expect("weak match must be found");

        assert!(
            (0.0..=1.0).contains(&strong_score),
            "strong score must be in [0, 1], got {strong_score}"
        );
        assert!(
            (0.0..=1.0).contains(&weak_score),
            "weak score must be in [0, 1], got {weak_score}"
        );
        assert!(
            strong_score > weak_score,
            "a dense strong match ({strong_score}) must outrank a single-mention weak \
             match ({weak_score}); bm25() is more-negative-is-better, so the score must be \
             monotone-increasing in (-rank)"
        );
    }
}

/// Current on-disk schema version. The fresh-DB `create_schema` path and the
/// incremental migration chain must converge on an IDENTICAL schema at this
/// version (same columns, same indexes).
const SCHEMA_VERSION: u32 = 11;

/// Run schema migrations to bring the database up to the current version.
///
/// Version history:
/// - v1 (implicit): original schema with `agent_id TEXT` column on nodes
/// - v2: `peer_id INTEGER` + `source_kind TEXT` replace `agent_id`; peers/peer_aliases tables added
/// - v3: `retained_action` reservoir table + edge `conductance`/`accessed_at`
///   reservoir columns (ADR-0002); valid-interval and salience-projection indexes
/// - v4: peer evidence-trust columns `trust_reservoir REAL` +
///   `trust_evidence_count INTEGER` (social.md "Peer Trust"); seeded from the coarse
///   `trust_level` prior for existing peers
/// - v6: DROP the `peers` / `peer_aliases` tables — the peer/trust
///   subsystem was removed (production is single-peer, `PeerId(0)`). Nodes' own
///   `peer_id` / `source_kind` columns (inside the Origin encoding) STAY; the
///   `idx_nodes_peer` index STAYS. No node data is touched.
/// - v7: normalize `nodes.node_type` for the KnowledgeType 15→4 collapse
///   (Episodic/Semantic/Identity/Custom). The three legacy identity wire strings
///   (`identity_core`/`identity_learned`/`identity_state`) are rewritten to bare
///   `identity`, and every deleted knowledge/memory wire string (`procedural`,
///   `entity`, `convention`, `decision`, `gotcha`, `hypothesis`, `evidence`,
///   `debug_session`, `event`) is rewritten to its canonical `custom:<string>`
///   form. This makes on-disk storage match the new decode immediately, closing the
///   eventual-consistency gap where a bare legacy string decoded in-memory to
///   `Custom(...)`/`Identity` but `nodes_by_type` (which filters by the raw encoded
///   string) would miss the un-normalized row until it was re-saved. Idempotent: the
///   `IN (...)` filters only match the legacy bare strings, never the post-migration
///   `identity` / `custom:*` values. No schema/column change — data only.
/// - v8: normalize EVERY remaining bare non-canonical `node_type` to its
///   canonical `custom:<escaped>` encoding, not just the fixed v7 legacy list. An
///   arbitrary bare string from a foreign/future writer loaded as `Custom(<raw>)`
///   but stayed invisible to `nodes_by_type` (which filters by the encoded
///   `custom:<escaped>` form) until re-saved. The rewrite runs in Rust per row so
///   `%`/tab/CR/LF are escaped via `encode_knowledge_type` (a raw `'custom:' ||
///   node_type` SQL concat could not). Idempotent: only bare non-canonical rows are
///   selected. No schema/column change — data only.
/// - v9: backfill a creation `AccessTrace` for every node whose `access_history`
///   is empty (`''`) — the shape a legacy pre-ACT-R row left on disk, which
///   otherwise makes `compute_base_level` return `NEG_INFINITY`. Data only,
///   idempotent (`WHERE access_history = ''`). See [`migrate_v8_to_v9`].
/// - v10: `edges.leaked_at INTEGER` — the per-edge leak checkpoint
///   `Engine::tick`'s idle-edge leakage measures idle time from (flagship bug
///   #2: without a checkpoint, `accessed_at` — a committed-USE marker, never
///   advanced by leakage — made every repeated `tick` at a fixed idle window
///   re-subtract the same idle-window leak again, collapsing an idle edge's
///   conductance the more the graph was ticked). Backfilled to `accessed_at`
///   for existing rows. See [`migrate_v9_to_v10`].
/// - v11 (current): `graph_metadata` key/value table for graph-wide persistent
///   metadata, initially used to guard the embedding model identity.
/// - v5: `nodes.evidence_prior REAL NOT NULL DEFAULT 0` — the persistent,
///   decay-exempt evidence prior `P_i` of the `A_i = B_i + P_i` decomposition
///   (ADR-0008). Backfilled to `0.0`: the base-level `B_i` is recomputed from
///   `access_history`, so persistent strength comes purely from `B_i` at first
///   recompute; backfilling `P_i` from the old `retained_action` scalar would
///   double-count access history (that scalar already absorbed it). The obsolete
///   `decay_checkpoint` / `retained_action` tables are retained for snapshot
///   back-compat but are no longer load-bearing for memory strength.
///
///   The `access_history` TEXT column now stores semicolon-separated `ts,decaybits`
///   pairs (the per-trace activation-dependent decay `d_j`, Pavlik & Anderson 2005;
///   see [`encode_access_history`]) rather than the comma-separated timestamps that
///   the unreleased intermediate revision of this same column used. No version bump
///   is taken for this format change: the timestamp-only ACT-R `access_history`
///   format never reached a released schema (`main` is at v2 with the legacy
///   exponential-salience model; v3–v5 and the base-level substrate live only on this
///   unmerged branch), and the only `access_history` rows a forward migration
///   encounters (the v1→v5 chain) are the empty legacy strings, which decode
///   identically under both formats. A round-trip encode/decode test guards the
///   `to_bits`/`from_bits` losslessness.
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
                // Each hop stamps its own `schema_version` on commit (see
                // `migrate_v4_to_v5`), so no outer stamp is needed here; the last
                // hop in the chain leaves the recorded version at
                // `SCHEMA_VERSION`.
                migrate_v1_to_v2(conn)?;
                migrate_v2_to_v3(conn)?;
                migrate_v3_to_v4(conn)?;
                migrate_v4_to_v5(conn)?;
                migrate_v5_to_v6(conn)?;
                migrate_v6_to_v7(conn)?;
                migrate_v7_to_v8(conn)?;
                migrate_v8_to_v9(conn)?;
                migrate_v9_to_v10(conn)?;
            } else {
                // Brand new database — create the current schema directly.
                // No hop runs to stamp the version, so this arm stamps it itself.
                create_schema(conn)?;
                conn.execute_batch(&format!(
                    "INSERT INTO schema_version (version) VALUES ({SCHEMA_VERSION});"
                ))
                .map_err(sqlite_error)?;
            }
        }
        // Every arm below resumes the chain from its recorded version. Each hop
        // is transactional, idempotent, and stamps its own `schema_version` on
        // commit (see `migrate_v4_to_v5`), so a crash between hops leaves the
        // recorded version exactly at the last hop that actually committed —
        // resuming here never replays an already-applied hop.
        Some(1) => {
            migrate_v1_to_v2(conn)?;
            migrate_v2_to_v3(conn)?;
            migrate_v3_to_v4(conn)?;
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(2) => {
            migrate_v2_to_v3(conn)?;
            migrate_v3_to_v4(conn)?;
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(3) => {
            migrate_v3_to_v4(conn)?;
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(4) => {
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(5) => {
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(6) => {
            migrate_v6_to_v7(conn)?;
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(7) => {
            migrate_v7_to_v8(conn)?;
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(8) => {
            migrate_v8_to_v9(conn)?;
            migrate_v9_to_v10(conn)?;
        }
        Some(9) => {
            migrate_v9_to_v10(conn)?;
        }
        Some(10) => {
            migrate_v10_to_v11(conn)?;
        }
        Some(11) => {
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

    let migrated_version: u32 = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .map_err(sqlite_error)?;
    if migrated_version == 10 {
        migrate_v10_to_v11(conn)?;
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
        // The two ALTERs are guarded individually (SQLite has no `ADD COLUMN IF
        // NOT EXISTS`); everything else here is already idempotent.
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS retained_action (
                node_id INTEGER PRIMARY KEY,
                value REAL NOT NULL
            );
            ",
        )
        .map_err(sqlite_error)?;
        if !column_exists(conn, "edges", "conductance")? {
            conn.execute_batch("ALTER TABLE edges ADD COLUMN conductance REAL NOT NULL DEFAULT 0;")
                .map_err(sqlite_error)?;
        }
        if !column_exists(conn, "edges", "accessed_at")? {
            conn.execute_batch(
                "ALTER TABLE edges ADD COLUMN accessed_at INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(sqlite_error)?;
        }
        conn.execute_batch(
            "
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

        stamp_version(conn, 3)
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
        // Guarded individually: SQLite has no `ADD COLUMN IF NOT EXISTS`.
        if !column_exists(conn, "peers", "trust_reservoir")? {
            conn.execute_batch(
                "ALTER TABLE peers ADD COLUMN trust_reservoir REAL NOT NULL DEFAULT 0;",
            )
            .map_err(sqlite_error)?;
        }
        if !column_exists(conn, "peers", "trust_evidence_count")? {
            conn.execute_batch(
                "ALTER TABLE peers ADD COLUMN trust_evidence_count INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(sqlite_error)?;
        }

        // Seed the reservoir from the coarse level prior (computed in Rust — the
        // mapping is a small lookup, SQLite has no helper for it). The peer/trust
        // subsystem was removed and this whole table is dropped at v6; the mapping
        // is inlined here so the historical chain no longer depends on the deleted
        // `TrustLevel` type while still producing a faithful intermediate v4 state.
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
            // Historical coarse-level → prior-reservoir mapping (log trust-odds).
            let prior: f64 = match trust_str.as_str() {
                "owner" => 4.0,
                "admin" => 2.0,
                "member" => 1.0,
                "untrusted" => -2.0,
                // "agent" / "observer" / unknown → neutral (no-evidence) prior.
                _ => 0.0,
            };
            conn.execute(
                "UPDATE peers SET trust_reservoir = ?2 WHERE id = ?1",
                params![id, prior],
            )
            .map_err(sqlite_error)?;
        }

        stamp_version(conn, 4)
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

/// Migrate a v4 database to v5: add the `nodes.evidence_prior` column — the
/// persistent, decay-exempt evidence prior `P_i` of the `A_i = B_i + P_i`
/// decomposition (ADR-0008).
///
/// Backfill is `0.0` (the column DEFAULT): the base level `B_i` is recomputed from
/// each node's `access_history`, so persistent strength comes purely from `B_i` at
/// first recompute. Backfilling `P_i` from the obsolete `retained_action` scalar
/// would double-count access history (that scalar already absorbed accesses), so a
/// zero prior is the doc-faithful choice and needs no Rust loop. The obsolete
/// `retained_action` / `decay_checkpoint` tables are left in place for snapshot
/// back-compat; they are no longer load-bearing for memory strength.
///
/// Runs in its own transaction and stamps `schema_version = 5` itself on commit,
/// so a crash before this hop finishes can never leave the recorded version
/// behind an already-applied `ALTER TABLE` (replay would otherwise collide with
/// a duplicate column).
fn migrate_v4_to_v5(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        // Guarded: SQLite has no `ADD COLUMN IF NOT EXISTS`.
        if !column_exists(conn, "nodes", "evidence_prior")? {
            conn.execute_batch(
                "ALTER TABLE nodes ADD COLUMN evidence_prior REAL NOT NULL DEFAULT 0;",
            )
            .map_err(sqlite_error)?;
        }

        stamp_version(conn, 5)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Migrate a v5 database to v6: DROP the `peers` and `peer_aliases` tables.
///
/// The peer/trust subsystem was removed (production is single-peer, `PeerId(0)`;
/// non-zero peers existed only in tests). This drops the two peer-only tables and
/// their trust columns. Nodes are untouched: their `peer_id` / `source_kind`
/// columns live inside the Origin encoding on the `nodes` table and STAY, and the
/// `idx_nodes_peer` index (used by `nodes_by_peer` for the identity-prior search
/// bias) STAYS. `DROP TABLE IF EXISTS` is idempotent, so re-running is safe.
///
/// Runs in its own transaction and stamps `schema_version = 6` itself on commit
/// (see `migrate_v4_to_v5` for the crash-safety rationale shared by every hop).
fn migrate_v5_to_v6(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        conn.execute_batch(
            "
            DROP TABLE IF EXISTS peer_aliases;
            DROP TABLE IF EXISTS peers;
            ",
        )
        .map_err(sqlite_error)?;

        stamp_version(conn, 6)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Migrate a v6 database to v7: normalize `nodes.node_type` for the KnowledgeType
/// 15→4 collapse (Episodic/Semantic/Identity/Custom).
///
/// Rewrites the on-disk wire strings of the removed variants so storage matches the
/// new decode immediately:
/// - the three legacy identity tiers (`identity_core`/`identity_learned`/
///   `identity_state`) become the bare merged `identity`;
/// - every removed knowledge/memory string (`procedural`, `entity`, `convention`,
///   `decision`, `gotcha`, `hypothesis`, `evidence`, `debug_session`, `event`)
///   becomes its canonical `custom:<string>` form.
///
/// This closes the carried eventual-consistency gap (B0 review): without it, a
/// legacy row with a bare `gotcha` decodes in-memory to `Custom("gotcha")`, but
/// [`nodes_by_type`](SqliteStorage::nodes_by_type) filters SQL by the *encoded*
/// query string `custom:gotcha` and would miss the un-normalized row until it was
/// re-saved. After this migration the row's stored string is `custom:gotcha`, so the
/// lookup matches deterministically.
///
/// Runs in one transaction. Idempotent: the `IN (...)` filters only ever match the
/// legacy bare strings, never the post-migration `identity` / `custom:*` values, so
/// re-running is a no-op. The `custom:` rewrite assumes the removed wire strings
/// contain no `escape_text` metacharacters (they are all fixed ASCII identifiers),
/// so a literal `'custom:' || node_type` yields the same bytes `encode_knowledge_type`
/// would produce for `Custom(<string>)`.
fn migrate_v6_to_v7(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        // Legacy identity tiers -> merged bare `identity`.
        conn.execute_batch(
            "
            UPDATE nodes SET node_type = 'identity'
            WHERE node_type IN ('identity_core', 'identity_learned', 'identity_state');
            ",
        )
        .map_err(sqlite_error)?;

        // Removed knowledge/memory variants -> canonical `custom:<string>`.
        conn.execute_batch(
            "
            UPDATE nodes SET node_type = 'custom:' || node_type
            WHERE node_type IN (
                'procedural', 'entity', 'convention', 'decision', 'gotcha',
                'hypothesis', 'evidence', 'debug_session', 'event'
            );
            ",
        )
        .map_err(sqlite_error)?;

        stamp_version(conn, 7)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Migrate a v7 database to v8: normalize EVERY remaining bare non-canonical
/// `node_type` to its canonical `custom:<escaped>` encoding.
///
/// The v6→v7 step only normalized a *fixed* list of removed legacy strings. An
/// arbitrary bare string — a `node_type` written by a foreign or future writer, or
/// any consumer type this build does not recognize — still loaded as
/// `Custom(<raw>)` via the decode fallback but stayed INVISIBLE to
/// [`nodes_by_type`](SqliteStorage::nodes_by_type), which filters SQL by the
/// *encoded* query string `encode_knowledge_type(&Custom(raw))` = `custom:<escaped>`.
/// So a type-filtered query missed the un-normalized row until it happened to be
/// re-saved. This migration closes that gap for all bare strings, not just the
/// fixed legacy list.
///
/// The rewrite is done in Rust, per row, rather than as a single
/// `UPDATE ... SET node_type = 'custom:' || node_type` in SQL: the canonical
/// `Custom` encoding escapes `%`, tab, CR, and LF via [`escape_text`], which SQL
/// cannot reproduce. A raw SQL concat would leave a bare `weird%type` as
/// `custom:weird%type`, but `nodes_by_type(&Custom("weird%type"))` queries for the
/// ESCAPED `custom:weird%25type` and would still miss it. Re-encoding each raw
/// value through [`encode_knowledge_type`] guarantees the stored bytes match what
/// the lookup produces for every input. Row counts here are tiny (only the
/// un-normalized rows are touched).
///
/// Runs in one transaction (BEGIN IMMEDIATE). Idempotent: the SELECT filter
/// excludes the three canonical bare variants (`episodic`/`semantic`/`identity`)
/// and anything already carrying the `custom:` prefix, so once normalized a row is
/// never rewritten again, and re-running is a no-op.
fn migrate_v7_to_v8(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        // Collect the raw values of every bare non-canonical row. The three
        // canonical bare variants and any already-`custom:`-prefixed row are
        // excluded, so this only ever selects strings that decode via the
        // `Custom(<raw>)` fallback and are therefore currently invisible to
        // `nodes_by_type`.
        let raw_types: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT node_type FROM nodes
                     WHERE node_type NOT IN ('episodic', 'semantic', 'identity')
                       AND node_type NOT LIKE 'custom:%'",
                )
                .map_err(sqlite_error)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sqlite_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?
        };

        // Re-encode each raw value through the canonical Custom encoding (escaping
        // metacharacters) and rewrite the matching rows. Matching on the exact raw
        // string is safe: a raw value already equal to its canonical encoding cannot
        // appear here (it would carry the `custom:` prefix and be filtered out).
        for raw in raw_types {
            let canonical = encode_knowledge_type(&KnowledgeType::Custom(raw.clone()));
            conn.execute(
                "UPDATE nodes SET node_type = ?1 WHERE node_type = ?2",
                params![canonical, raw],
            )
            .map_err(sqlite_error)?;
        }

        stamp_version(conn, 8)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Migrate a v8 database to v9: backfill a creation [`AccessTrace`] for every
/// node row whose `access_history` is empty (`''`).
///
/// A legacy row written before the ACT-R access-trace substrate existed decodes
/// `access_history = ''` to an empty `VecDeque`
/// ([`decode_access_history`]), which makes
/// [`crate::mechanics::forgetting::compute_base_level`] return `NEG_INFINITY`.
/// This backfills the exact creation trace [`Engine::ingest`] seeds for a brand
/// new node (see `api/mod.rs`): one trace stamped at the row's `created_at`
/// (falling back to `accessed_at`, matched via `COALESCE`), at the ingest floor
/// decay `d_j = m_type * DECAY_INTERCEPT` for the row's own decoded `node_type`.
///
/// Runs in one transaction (`BEGIN IMMEDIATE`) and stamps its own
/// `schema_version = 9` on commit (see `migrate_v8_to_v9`'s sibling hops for the
/// per-hop crash-safety contract). Idempotent: the `WHERE access_history = ''`
/// selector, repeated on both the `SELECT` and the `UPDATE`, only ever matches
/// un-backfilled rows, so re-running is a no-op and a row that already carries a
/// real trace is never touched.
fn migrate_v8_to_v9(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        let empty_history_rows: Vec<(u64, String, u64)> = {
            // `accessed_at` is a separate hot-field table (SoA), not a `nodes`
            // column — LEFT JOIN it for the created_at fallback.
            let mut stmt = conn
                .prepare(
                    "SELECT n.id, n.node_type, COALESCE(n.created_at, a.accessed_at)
                     FROM nodes n
                     LEFT JOIN accessed_at a ON a.node_id = n.id
                     WHERE n.access_history = ''",
                )
                .map_err(sqlite_error)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                })
                .map_err(sqlite_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?
        };

        for (id, node_type_raw, at) in empty_history_rows {
            let node_type = decode_knowledge_type(&node_type_raw)?;
            let creation_decay = crate::mechanics::priors::decay_multiplier_for_type(&node_type)
                * crate::mechanics::priors::DECAY_INTERCEPT;
            let mut creation_history = VecDeque::new();
            creation_history.push_back(AccessTrace {
                at: Timestamp(at),
                decay: creation_decay,
            });
            conn.execute(
                "UPDATE nodes SET access_history = ?2 WHERE id = ?1 AND access_history = ''",
                params![id, encode_access_history(&creation_history)],
            )
            .map_err(sqlite_error)?;
        }

        stamp_version(conn, 9)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Migrate a v9 database to v10: add the `edges.leaked_at INTEGER` column — the
/// per-edge leak checkpoint [`Engine::tick`](crate::api::Engine::tick)'s
/// idle-edge leakage now measures idle time from (flagship bug #2: without a
/// checkpoint, `accessed_at` — which marks committed USE, not leak history, and
/// is never advanced by a leak — made every repeated `tick` at a fixed idle
/// window re-subtract the same idle-window leak again, so an idle edge's
/// conductance collapsed toward zero the more the graph was ticked).
///
/// Backfill: `leaked_at = accessed_at` for every existing edge row.
/// `accessed_at` is the closest available approximation of "last known
/// non-idle instant" for a pre-migration edge — no earlier leak ever happened
/// under the old scheme, so there is no genuine leak history to recover, and
/// this matches the invariant production code maintains going forward
/// (`set_edge_accessed_at` keeps `leaked_at` in sync with `accessed_at` on
/// every committed use).
///
/// Runs in one transaction (`BEGIN IMMEDIATE`) and stamps its own
/// `schema_version = 10` on commit (see `migrate_v8_to_v9`'s sibling hops for
/// the per-hop crash-safety contract). Idempotent: the `ADD COLUMN` is guarded
/// by [`column_exists`] (SQLite has no `ADD COLUMN IF NOT EXISTS`), and the
/// backfill `UPDATE` deterministically sets every row's `leaked_at` from its
/// own current `accessed_at` regardless of how many times it runs.
fn migrate_v9_to_v10(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        if !column_exists(conn, "edges", "leaked_at")? {
            conn.execute_batch(
                "ALTER TABLE edges ADD COLUMN leaked_at INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(sqlite_error)?;
        }

        conn.execute_batch("UPDATE edges SET leaked_at = accessed_at;")
            .map_err(sqlite_error)?;

        stamp_version(conn, 10)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Migrate v10 to v11 by adding graph-wide key/value metadata.
fn migrate_v10_to_v11(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS graph_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .map_err(sqlite_error)?;
        stamp_version(conn, 11)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Stamp `schema_version` to `version`, to be called inside the caller's own
/// migration-hop transaction just before `COMMIT`.
///
/// The table holds exactly one row; `DELETE` then `INSERT` stamps the version
/// whether or not a row already exists yet, so every hop can call this
/// unconditionally instead of branching on `INSERT` vs `UPDATE`.
fn stamp_version(conn: &Connection, version: u32) -> Result<(), Error> {
    conn.execute_batch(&format!(
        "DELETE FROM schema_version; INSERT INTO schema_version (version) VALUES ({version});"
    ))
    .map_err(sqlite_error)
}

/// Whether `table` currently has a column named `column`, via `PRAGMA table_info`.
///
/// SQLite has no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so every
/// column-adding hop guards its `ALTER TABLE` with this check first. That makes
/// resuming the chain from a stale recorded version safe even when the target
/// column was already added by an earlier attempt at the same hop (per-hop
/// stamping prevents new staleness going forward; this guard keeps a hop
/// idempotent against a database left stale by a crash before the fix existed).
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, Error> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlite_error)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?;
    for name in names {
        if name.map_err(sqlite_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Migrate a v1 database (agent_id TEXT) to v2 (peer_id INTEGER + source_kind TEXT).
///
/// Runs in its own transaction and stamps `schema_version = 2` itself on commit
/// (see `migrate_v4_to_v5` for the crash-safety rationale shared by every hop).
fn migrate_v1_to_v2(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(sqlite_error)?;

    let result = (|| -> Result<(), Error> {
        // Add new columns to nodes/edges (with defaults for existing rows).
        // Guarded individually: SQLite has no `ADD COLUMN IF NOT EXISTS`.
        if !column_exists(conn, "nodes", "peer_id")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN peer_id INTEGER NOT NULL DEFAULT 0;")
                .map_err(sqlite_error)?;
        }
        if !column_exists(conn, "nodes", "source_kind")? {
            conn.execute_batch(
                "ALTER TABLE nodes ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'agent_observation';",
            )
            .map_err(sqlite_error)?;
        }
        if !column_exists(conn, "edges", "edge_source")? {
            conn.execute_batch(
                "ALTER TABLE edges ADD COLUMN edge_source TEXT NOT NULL DEFAULT 'auto';",
            )
            .map_err(sqlite_error)?;
        }

        conn.execute_batch(
            "
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
        .map_err(sqlite_error)?;

        stamp_version(conn, 2)
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;").map_err(sqlite_error)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
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
            metadata TEXT NOT NULL,
            evidence_prior REAL NOT NULL DEFAULT 0
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
            accessed_at INTEGER NOT NULL DEFAULT 0,
            leaked_at INTEGER NOT NULL DEFAULT 0
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
            content,
            tokenize = 'porter unicode61'
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

        CREATE TABLE IF NOT EXISTS graph_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
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
                access_count, access_history, tier, metadata, evidence_prior
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
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
                encode_access_history(&node.access_history),
                encode_memory_tier(&node.tier),
                encode_map(&node.metadata),
                node.evidence_prior,
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
                id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata, edge_source, conductance, accessed_at, leaked_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                edge.leaked_at.0,
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
                d.decay_checkpoint, r.value, n.evidence_prior
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
            // Evidence prior P_i (NOT NULL DEFAULT 0 column on `nodes`).
            let evidence_prior: f64 = row.get::<_, Option<f64>>(23)?.unwrap_or(0.0);
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
                access_history: decode_access_history(&row.get::<_, String>(16)?)
                    .map_err(to_sql_error)?,
                tier: decode_memory_tier(&row.get::<_, String>(17)?).map_err(to_sql_error)?,
                metadata: decode_map(&row.get::<_, String>(18)?).map_err(to_sql_error)?,
                salience,
                retained_action,
                evidence_prior,
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
            "SELECT id, from_node, to_node, edge_type, weight, created_at, valid_from, valid_until, metadata, edge_source, conductance, accessed_at, leaked_at
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
                leaked_at: Timestamp(row.get(12)?),
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
        KnowledgeType::Identity => "identity".to_string(),
        KnowledgeType::Semantic => "semantic".to_string(),
        KnowledgeType::Episodic => "episodic".to_string(),
        KnowledgeType::Custom(name) => format!("custom:{}", escape_text(name)),
    }
}

fn decode_knowledge_type(value: &str) -> Result<KnowledgeType, Error> {
    Ok(match value {
        "identity" => KnowledgeType::Identity,
        // Legacy identity tiers (pre-0.10.0 three-tier ladder) keep their slow-decay
        // identity semantics: explicit arms fold them into the merged `Identity`
        // rather than letting them ride the generic fallback into `Custom`. The
        // v6→v7 normalization migration rewrites these rows to bare `"identity"`
        // on open, but these arms still guard any row that predates it.
        "identity_core" | "identity_learned" | "identity_state" => KnowledgeType::Identity,
        "semantic" => KnowledgeType::Semantic,
        "episodic" => KnowledgeType::Episodic,
        custom if custom.starts_with("custom:") => {
            KnowledgeType::Custom(unescape_text(&custom[7..])?)
        }
        // Legacy-DB compat guard (ADR / task B0): any bare string this build does
        // not recognize — a consumer type from another schema, or the wire string
        // of a variant deleted in the 0.10.0 shrink (`procedural`, `entity`,
        // `convention`, `decision`, `gotcha`, `hypothesis`, `evidence`,
        // `debug_session`, `event`) — decodes to `Custom(<original>)` instead of
        // erroring, so the node loads and the DB opens. This is a one-way
        // normalization: re-encoding the result writes the canonical
        // `custom:<original>` form (see `encode_knowledge_type`), which decodes
        // right back to the same `Custom`, so open/save cycles are a fixed point
        // and never corrupt the value further. The v6→v7 migration rewrites these
        // rows to the canonical `custom:<original>` on open so `nodes_by_type`
        // matches them immediately; these arms still guard any un-migrated row.
        // Known variants and the explicit `custom:` prefix are matched by the arms
        // above and are unaffected.
        other => KnowledgeType::Custom(other.to_string()),
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
        // Legacy-DB compat guard (task B0): an unrecognized tier string falls back
        // to `Auto` (the `MemoryTier::default()`) rather than erroring, mirroring
        // the node-type guard so a DB carrying a dropped/foreign tier string still
        // opens. `Auto` means "no override — tier follows salience", the safe
        // neutral choice.
        _ => MemoryTier::Auto,
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

/// Encode the bounded access-history window as semicolon-separated `ts,decaybits`
/// pairs. The per-trace decay `d_j` (`f64`) is serialized losslessly via
/// [`f64::to_bits`] so the activation-dependent decay round-trips exactly; an empty
/// deque encodes to the empty string.
fn encode_access_history(value: &VecDeque<AccessTrace>) -> String {
    value
        .iter()
        .map(|trace| format!("{},{}", trace.at.0, trace.decay.to_bits()))
        .collect::<Vec<_>>()
        .join(";")
}

/// Decode the access-history column written by [`encode_access_history`]: each
/// `;`-separated entry is `ts,decaybits`, with the decay recovered exactly via
/// [`f64::from_bits`]. An empty string decodes to an empty deque.
fn decode_access_history(value: &str) -> Result<VecDeque<AccessTrace>, Error> {
    if value.is_empty() {
        return Ok(VecDeque::new());
    }
    value
        .split(';')
        .map(|entry| {
            let (ts_part, decay_part) = entry.split_once(',').ok_or_else(|| {
                Error::StorageError(format!(
                    "invalid access trace '{entry}': missing ',' separator"
                ))
            })?;
            let at = ts_part.parse::<u64>().map(Timestamp).map_err(|e| {
                Error::StorageError(format!("invalid trace timestamp '{ts_part}': {e}"))
            })?;
            let bits = decay_part.parse::<u64>().map_err(|e| {
                Error::StorageError(format!("invalid trace decay bits '{decay_part}': {e}"))
            })?;
            Ok(AccessTrace {
                at,
                decay: f64::from_bits(bits),
            })
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

fn decode_scope(value: &str) -> Result<ScopePath, Error> {
    if value.is_empty() {
        Ok(ScopePath::universal())
    } else {
        ScopePath::new(value)
    }
}

/// OR-joined prefix terms: natural-language queries should match nodes that
/// contain only some of the query tokens, ranked by BM25 (which downweights
/// common tokens), instead of requiring every token to be present.
fn make_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|part| format!("\"{}\"*", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn rank_to_score(rank: f64) -> f64 {
    // SQLite FTS5 `bm25()` is more-negative-is-better, so the score must be
    // monotone-increasing in `(-rank)`: logistic(-rank) = 1 / (1 + e^rank).
    (1.0 / (1.0 + rank.exp())).clamp(0.0, 1.0)
}

fn sqlite_error(error: rusqlite::Error) -> Error {
    Error::StorageError(error.to_string())
}

fn metadata_value(conn: &Connection, key: &str) -> Result<Option<String>, Error> {
    conn.query_row(
        "SELECT value FROM graph_metadata WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
    .map_err(sqlite_error)
}

fn required_metadata_value(conn: &Connection, key: &str) -> Result<String, Error> {
    metadata_value(conn, key)?.ok_or_else(|| {
        Error::StorageError(format!("missing embedding migration metadata key '{key}'"))
    })
}

fn invalid_metadata_value(key: &str) -> Error {
    Error::StorageError(format!(
        "invalid embedding migration metadata value for key '{key}'"
    ))
}

fn parse_metadata_usize(key: &str, value: &str) -> Result<usize, Error> {
    value
        .parse::<usize>()
        .map_err(|_| invalid_metadata_value(key))
}

fn parse_metadata_node_id(key: &str, value: &str) -> Result<NodeId, Error> {
    value
        .parse::<u64>()
        .map(NodeId)
        .map_err(|_| invalid_metadata_value(key))
}

fn read_embedding_migration_checkpoint(
    conn: &Connection,
) -> Result<Option<EmbeddingMigrationCheckpoint>, Error> {
    let Some(phase) = metadata_value(conn, EMBEDDING_MIGRATION_PHASE_KEY)? else {
        return Ok(None);
    };
    if phase != EMBEDDING_MIGRATION_PHASE {
        return Err(invalid_metadata_value(EMBEDDING_MIGRATION_PHASE_KEY));
    }

    let source_model = metadata_value(conn, EMBEDDING_MIGRATION_SOURCE_MODEL_KEY)?;
    let source_dim = metadata_value(conn, EMBEDDING_MIGRATION_SOURCE_DIM_KEY)?
        .map(|value| parse_metadata_usize(EMBEDDING_MIGRATION_SOURCE_DIM_KEY, &value))
        .transpose()?;
    let target_model = required_metadata_value(conn, EMBEDDING_MIGRATION_TARGET_MODEL_KEY)?;
    let target_dim_raw = required_metadata_value(conn, EMBEDDING_MIGRATION_TARGET_DIM_KEY)?;
    let target_dim = parse_metadata_usize(EMBEDDING_MIGRATION_TARGET_DIM_KEY, &target_dim_raw)?;
    let selection_raw = required_metadata_value(conn, EMBEDDING_MIGRATION_SELECTION_KEY)?;
    let selection = match selection_raw.as_str() {
        "dimension" => EmbeddingSelection::Dimension,
        "cursor" => EmbeddingSelection::Cursor,
        _ => return Err(invalid_metadata_value(EMBEDDING_MIGRATION_SELECTION_KEY)),
    };
    let cursor = metadata_value(conn, EMBEDDING_MIGRATION_CURSOR_KEY)?
        .map(|value| parse_metadata_node_id(EMBEDDING_MIGRATION_CURSOR_KEY, &value))
        .transpose()?;
    let backup_path = PathBuf::from(required_metadata_value(
        conn,
        EMBEDDING_MIGRATION_BACKUP_PATH_KEY,
    )?);
    let checkpoint = EmbeddingMigrationCheckpoint {
        source_model,
        source_dim,
        target_model,
        target_dim,
        selection,
        cursor,
        backup_path,
    };
    validate_embedding_migration_checkpoint(&checkpoint)?;
    Ok(Some(checkpoint))
}

fn validate_embedding_migration_checkpoint(
    checkpoint: &EmbeddingMigrationCheckpoint,
) -> Result<(), Error> {
    if checkpoint.target_model.is_empty() {
        return Err(invalid_metadata_value(EMBEDDING_MIGRATION_TARGET_MODEL_KEY));
    }
    if checkpoint.target_dim == 0 {
        return Err(invalid_metadata_value(EMBEDDING_MIGRATION_TARGET_DIM_KEY));
    }
    if checkpoint.source_dim == Some(0) {
        return Err(invalid_metadata_value(EMBEDDING_MIGRATION_SOURCE_DIM_KEY));
    }
    if checkpoint.backup_path.as_os_str().is_empty() {
        return Err(invalid_metadata_value(EMBEDDING_MIGRATION_BACKUP_PATH_KEY));
    }
    Ok(())
}

const fn encode_embedding_selection(selection: EmbeddingSelection) -> &'static str {
    match selection {
        EmbeddingSelection::Dimension => "dimension",
        EmbeddingSelection::Cursor => "cursor",
    }
}

fn set_metadata(conn: &Connection, key: &str, value: &str) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO graph_metadata (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn clear_embedding_migration_metadata(conn: &Connection) -> Result<(), Error> {
    for key in EMBEDDING_MIGRATION_KEYS {
        conn.execute("DELETE FROM graph_metadata WHERE key = ?1", [key])
            .map_err(sqlite_error)?;
    }
    Ok(())
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
