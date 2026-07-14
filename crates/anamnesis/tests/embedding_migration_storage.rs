use anamnesis::engine::{KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{MemoryTier, ScopePath};
use anamnesis::storage::sqlite::{
    BackupError, EmbeddingBatch, EmbeddingMigrationCheckpoint, EmbeddingReplacement,
    EmbeddingSelection,
};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use rusqlite::Connection;
use rusqlite::types::Value;
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::path::Path;

fn make_node(id: NodeId, embedding: Option<Vec<f64>>) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id.0),
        summary: Some(format!("summary-{}", id.0)),
        content: format!("content-{}", id.0),
        embedding,
        created_at: Timestamp(1_000 + id.0),
        updated_at: Timestamp(2_000 + id.0),
        accessed_at: Timestamp(3_000 + id.0),
        valid_from: Some(Timestamp(900 + id.0)),
        valid_until: None,
        salience: 0.7,
        retained_action: 0.5,
        evidence_prior: 0.2,
        access_count: 3,
        access_history: VecDeque::new(),
        tier: MemoryTier::Recall,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(7),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "migration-test".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.95,
        },
        entity_tags: vec!["migration".to_string()],
        metadata: HashMap::from([("kept".to_string(), "yes".to_string())]),
    }
}

fn checkpoint(path: &Path, selection: EmbeddingSelection) -> EmbeddingMigrationCheckpoint {
    EmbeddingMigrationCheckpoint {
        source_model: Some("source-model".to_string()),
        source_dim: Some(3),
        target_model: "target-model".to_string(),
        target_dim: 2,
        selection,
        cursor: None,
        backup_path: path.to_path_buf(),
    }
}

fn table_rows(path: &Path, sql: &str) -> Vec<Vec<Value>> {
    let conn = Connection::open(path).expect("open raw database");
    let mut statement = conn.prepare(sql).expect("prepare snapshot query");
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get::<_, Value>(column))
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("query snapshot rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect snapshot rows")
}

fn insert_nodes(storage: &mut SqliteStorage, embeddings: [Option<Vec<f64>>; 2]) {
    for embedding in embeddings {
        let id = storage.next_node_id();
        storage
            .set_node(make_node(id, embedding))
            .expect("store migration fixture node");
    }
}

#[test]
fn online_backup_is_valid_and_never_overwrites() {
    // Given: a durable graph and a destination path that does not exist.
    let dir = tempfile::tempdir().expect("temporary directory");
    let source = dir.path().join("source.db");
    let destination = dir.path().join("source.db.bak");
    let mut storage = SqliteStorage::open(&source).expect("open source graph");
    insert_nodes(&mut storage, [Some(vec![1.0, 2.0, 3.0]), None]);
    drop(storage);

    // When: an online backup is created and a second creation is attempted.
    SqliteStorage::create_verified_backup(&source, &destination).expect("create backup");
    let original_bytes = std::fs::read(&destination).expect("read verified backup");
    let second = SqliteStorage::create_verified_backup(&source, &destination)
        .expect_err("existing backup must not be overwritten");

    // Then: the backup passes SQLite verification and the second call is create-new.
    let backup = Connection::open(&destination).expect("open backup");
    let quick_check: String = backup
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .expect("quick-check backup");
    assert_eq!(quick_check, "ok");
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get::<_, u64>(0))
            .expect("count backed-up nodes"),
        2
    );
    assert!(matches!(
        second,
        BackupError::Io(ref error) if error.kind() == ErrorKind::AlreadyExists
    ));
    assert_eq!(
        std::fs::read(&destination).expect("reread backup"),
        original_bytes
    );
}

#[test]
fn embedding_batch_updates_only_vectors_and_checkpoint_atomically() {
    // Given: two nodes and a durable dimension-based migration checkpoint.
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("batch.db");
    let backup = dir.path().join("batch.db.bak");
    let mut storage = SqliteStorage::open(&db).expect("open graph");
    insert_nodes(&mut storage, [Some(vec![1.0, 2.0, 3.0]), None]);
    let dimension_checkpoint = checkpoint(&backup, EmbeddingSelection::Dimension);
    storage
        .begin_embedding_migration(&dimension_checkpoint)
        .expect("begin migration");
    let candidates = storage
        .embedding_candidates(&dimension_checkpoint, 64)
        .expect("load candidates");
    assert_eq!(
        candidates
            .iter()
            .map(|item| item.node_id)
            .collect::<Vec<_>>(),
        vec![NodeId(0), NodeId(1)]
    );
    assert_eq!(candidates[0].embedding_dim, Some(3));
    assert_eq!(candidates[1].embedding_dim, None);
    let non_vectors_before = table_rows(
        &db,
        "SELECT id, name, summary, content, node_type, peer_id, source_kind, session_id, \
         scope, confidence, valid_from, valid_until, created_at, updated_at, access_count, \
         access_history, tier, metadata, evidence_prior FROM nodes ORDER BY id",
    );
    let auxiliary_before = table_rows(
        &db,
        "SELECT s.node_id, s.salience, a.accessed_at, d.decay_checkpoint, r.value \
         FROM salience s JOIN accessed_at a USING(node_id) \
         JOIN decay_checkpoint d USING(node_id) JOIN retained_action r USING(node_id) \
         ORDER BY s.node_id",
    );

    // When: both vectors and the cursor are committed as one batch.
    storage
        .commit_embedding_batch(EmbeddingBatch {
            replacements: vec![
                EmbeddingReplacement {
                    node_id: NodeId(0),
                    embedding: vec![10.0, 11.0],
                },
                EmbeddingReplacement {
                    node_id: NodeId(1),
                    embedding: vec![20.0, 21.0],
                },
            ],
            next_cursor: Some(NodeId(1)),
        })
        .expect("commit migration batch");

    // Then: durable rows/cache/cursor agree and every non-vector field is unchanged.
    assert_eq!(
        storage.get_node(NodeId(0)).expect("cached node").embedding,
        Some(vec![10.0, 11.0])
    );
    assert_eq!(
        storage.get_node(NodeId(1)).expect("cached node").embedding,
        Some(vec![20.0, 21.0])
    );
    assert_eq!(
        table_rows(&db, "SELECT id, embedding_json FROM nodes ORDER BY id"),
        vec![
            vec![Value::Integer(0), Value::Text("10,11".to_string())],
            vec![Value::Integer(1), Value::Text("20,21".to_string())],
        ]
    );
    assert_eq!(
        storage
            .embedding_migration_checkpoint()
            .expect("read checkpoint")
            .expect("checkpoint remains")
            .cursor,
        Some(NodeId(1))
    );
    assert_eq!(
        table_rows(
            &db,
            "SELECT id, name, summary, content, node_type, peer_id, source_kind, session_id, \
             scope, confidence, valid_from, valid_until, created_at, updated_at, access_count, \
             access_history, tier, metadata, evidence_prior FROM nodes ORDER BY id",
        ),
        non_vectors_before
    );
    assert_eq!(
        table_rows(
            &db,
            "SELECT s.node_id, s.salience, a.accessed_at, d.decay_checkpoint, r.value \
             FROM salience s JOIN accessed_at a USING(node_id) \
             JOIN decay_checkpoint d USING(node_id) JOIN retained_action r USING(node_id) \
             ORDER BY s.node_id",
        ),
        auxiliary_before
    );
    let cursor_checkpoint = EmbeddingMigrationCheckpoint {
        selection: EmbeddingSelection::Cursor,
        cursor: Some(NodeId(0)),
        ..dimension_checkpoint
    };
    assert_eq!(
        storage
            .embedding_candidates(&cursor_checkpoint, 64)
            .expect("load cursor candidates")
            .into_iter()
            .map(|item| item.node_id)
            .collect::<Vec<_>>(),
        vec![NodeId(1)]
    );
}

#[test]
fn failed_batch_preserves_rows_cache_and_checkpoint() {
    // Given: a trigger that aborts the second valid row update in a migration batch.
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("rollback.db");
    let mut storage = SqliteStorage::open(&db).expect("open graph");
    insert_nodes(
        &mut storage,
        [Some(vec![1.0, 2.0, 3.0]), Some(vec![4.0, 5.0, 6.0])],
    );
    let migration = checkpoint(
        &dir.path().join("rollback.db.bak"),
        EmbeddingSelection::Cursor,
    );
    storage
        .begin_embedding_migration(&migration)
        .expect("begin migration");
    Connection::open(&db)
        .expect("open trigger connection")
        .execute_batch(
            "CREATE TRIGGER abort_second_embedding \
             BEFORE UPDATE OF embedding_json ON nodes WHEN OLD.id = 1 \
             BEGIN SELECT RAISE(ABORT, 'injected batch failure'); END;",
        )
        .expect("install failure trigger");
    let rows_before = table_rows(&db, "SELECT * FROM nodes ORDER BY id");
    let checkpoint_before = storage
        .embedding_migration_checkpoint()
        .expect("read checkpoint");
    let cache_before = [
        storage.get_node(NodeId(0)).expect("cached node").clone(),
        storage.get_node(NodeId(1)).expect("cached node").clone(),
    ];

    // When: the first update succeeds but SQLite aborts the second update.
    let result = storage.commit_embedding_batch(EmbeddingBatch {
        replacements: vec![
            EmbeddingReplacement {
                node_id: NodeId(0),
                embedding: vec![10.0, 11.0],
            },
            EmbeddingReplacement {
                node_id: NodeId(1),
                embedding: vec![20.0, 21.0],
            },
        ],
        next_cursor: Some(NodeId(1)),
    });

    // Then: rollback preserves durable rows, cache entries, and checkpoint cursor.
    assert!(result.is_err());
    assert_eq!(
        table_rows(&db, "SELECT * FROM nodes ORDER BY id"),
        rows_before
    );
    assert_eq!(
        storage
            .embedding_migration_checkpoint()
            .expect("reread checkpoint"),
        checkpoint_before
    );
    assert_eq!(
        storage.get_node(NodeId(0)).expect("cached node"),
        &cache_before[0]
    );
    assert_eq!(
        storage.get_node(NodeId(1)).expect("cached node"),
        &cache_before[1]
    );
}

#[test]
fn completion_stamps_model_only_after_every_vector_matches() {
    // Given: a migration whose persisted inventory contains a wrong dimension.
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("finish.db");
    let mut storage = SqliteStorage::open(&db).expect("open graph");
    insert_nodes(
        &mut storage,
        [Some(vec![1.0, 2.0]), Some(vec![3.0, 4.0, 5.0])],
    );
    storage
        .set_embedding_model_name("source-model")
        .expect("stamp source model");
    let migration = checkpoint(
        &dir.path().join("finish.db.bak"),
        EmbeddingSelection::Dimension,
    );
    storage
        .begin_embedding_migration(&migration)
        .expect("begin migration");

    // When: completion runs before and after the final vector is replaced.
    let rejected = storage.finish_embedding_migration();

    // Then: invalid inventory keeps the source stamp/checkpoint; valid inventory atomically finishes.
    assert!(rejected.is_err());
    assert_eq!(
        storage
            .embedding_model_name()
            .expect("read source stamp")
            .as_deref(),
        Some("source-model")
    );
    assert!(
        storage
            .embedding_migration_checkpoint()
            .expect("read checkpoint")
            .is_some()
    );
    storage
        .commit_embedding_batch(EmbeddingBatch {
            replacements: vec![EmbeddingReplacement {
                node_id: NodeId(1),
                embedding: vec![30.0, 40.0],
            }],
            next_cursor: None,
        })
        .expect("replace final vector");
    storage
        .finish_embedding_migration()
        .expect("finish valid migration");
    assert_eq!(
        storage
            .embedding_model_name()
            .expect("read target stamp")
            .as_deref(),
        Some("target-model")
    );
    assert_eq!(
        storage
            .embedding_migration_checkpoint()
            .expect("read cleared checkpoint"),
        None
    );
}
