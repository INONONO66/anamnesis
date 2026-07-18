//! Reproduction tests for the snapshot/migration-checkpoint durability seam.
//!
//! An embedding migration is resumable only while its durable checkpoint
//! (the `embedding.migration.*` keys in `graph_metadata`) survives. The
//! snapshot path clones storage but copied only `embedding_model`, silently
//! dropping the checkpoint: a snapshot taken mid-migration restores a graph
//! that no longer reports `IncompleteMigration`, and downstream consumers
//! (the MCP namespace gate) misroute recovery to `DimensionMismatch`.
//!
//! These tests pin the state machine: migration-in-flight -> snapshot ->
//! restore must preserve the checkpoint byte-for-byte and must not resurrect
//! a mixed-dimension graph.

use std::path::PathBuf;

use anamnesis::api::EngineConfig;
use anamnesis::storage::SqliteStorage;
use anamnesis::storage::sqlite::{EmbeddingMigrationCheckpoint, EmbeddingSelection};
use anamnesis::Engine;

fn mid_migration_checkpoint() -> EmbeddingMigrationCheckpoint {
    EmbeddingMigrationCheckpoint {
        source_model: Some("hash-embed-v1".to_string()),
        source_dim: Some(384),
        target_model: "bge-base-en-v1.5".to_string(),
        target_dim: 768,
        selection: EmbeddingSelection::Dimension,
        cursor: None,
        backup_path: PathBuf::from("/tmp/anamnesis-snapshot-checkpoint-backup.db"),
    }
}

/// Engine-level state machine: begin migration -> snapshot -> restore.
/// The restored storage must still report the same checkpoint, so the
/// namespace compat gate keeps routing to migration resume (not a
/// dimension-mismatch error).
#[test]
fn snapshot_restore_preserves_incomplete_migration_checkpoint() {
    let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
    let checkpoint = mid_migration_checkpoint();
    storage
        .begin_embedding_migration(&checkpoint)
        .expect("migration checkpoint persisted");

    let mut engine = Engine::with_storage(EngineConfig::default(), storage);
    let snapshot = engine.snapshot("mid-migration").expect("snapshot");
    engine.restore(&snapshot).expect("restore");

    let restored = engine
        .graph()
        .storage()
        .embedding_migration_checkpoint()
        .expect("checkpoint readable after restore");
    assert_eq!(
        restored,
        Some(checkpoint),
        "migration checkpoint must survive snapshot/restore; losing it misroutes \
         IncompleteMigration recovery to DimensionMismatch and can resurrect a \
         mixed-dimension graph"
    );
}

/// Storage-level pinpoint of the same seam: cloning must carry the durable
/// migration checkpoint, not just the embedding model name.
#[test]
fn sqlite_clone_copies_migration_checkpoint_keys() {
    let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
    let checkpoint = mid_migration_checkpoint();
    storage
        .begin_embedding_migration(&checkpoint)
        .expect("migration checkpoint persisted");

    let cloned = storage.clone();
    let restored = cloned
        .embedding_migration_checkpoint()
        .expect("checkpoint readable on clone");
    assert_eq!(
        restored,
        Some(checkpoint),
        "SqliteStorage::clone dropped the embedding.migration.* metadata keys"
    );
}
