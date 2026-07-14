use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anamnesis::embedding::{EmbeddingProvider, widen};
use anamnesis::storage::sqlite::{
    EmbeddingBatch, EmbeddingMigrationCheckpoint, EmbeddingMigrationInspection,
    EmbeddingReplacement, EmbeddingSelection,
};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use anamnesis::{Error, Memory};

use super::{
    EmbeddingMigrationOutcome, EmbeddingMigrationReport, EmbeddingMigrationRequest,
    EmbeddingProgress, backup_path_for_database, verify_embedding_compatibility,
};

pub(crate) const EMBEDDING_MIGRATION_BATCH_SIZE: usize = 64;

pub(crate) fn migrate_embeddings(
    request: EmbeddingMigrationRequest,
    progress: &mut dyn FnMut(EmbeddingProgress),
) -> Result<EmbeddingMigrationOutcome, Error> {
    let EmbeddingMigrationRequest {
        pending,
        lock_lease,
    } = request;
    let _held_file = lock_lease.file();
    let db_path = std::fs::canonicalize(&pending.db_path).map_err(|error| {
        Error::StorageError(format!(
            "resolve migration database {:?}: {error}",
            pending.db_path
        ))
    })?;
    let inspection = SqliteStorage::inspect_embedding_migration(&db_path)?;
    let target_model = pending.provider.model_name().to_string();
    let target_dim = pending.provider.dimensions();

    if inspection.checkpoint.is_none()
        && inventory_is_compatible(&inspection, target_dim)
        && inspection
            .embedding_model
            .as_deref()
            .is_none_or(|model| model == target_model)
    {
        reopen_with_normal_guard(&db_path, Arc::clone(&pending.provider))?;
        return Ok(EmbeddingMigrationOutcome::NoOp {
            model: target_model,
            dimensions: target_dim,
        });
    }

    let source_model = inspection.embedding_model.clone();
    let source_dim = source_dimension(&inspection)?;
    let expected_backup_path = backup_path_for_database(&db_path)?;
    let selection = if source_dim.is_some_and(|dim| dim == target_dim)
        && !inspection
            .embedding_dimensions
            .iter()
            .any(|dimension| dimension.is_none())
    {
        EmbeddingSelection::Cursor
    } else {
        EmbeddingSelection::Dimension
    };
    let mut checkpoint = match inspection.checkpoint {
        Some(checkpoint) => {
            let checkpoint_backup =
                std::fs::canonicalize(&checkpoint.backup_path).map_err(|error| {
                    Error::StorageError(format!(
                        "resolve checkpoint backup {:?}: {error}",
                        checkpoint.backup_path
                    ))
                })?;
            let expected_backup =
                std::fs::canonicalize(&expected_backup_path).map_err(|error| {
                    Error::StorageError(format!(
                        "resolve expected migration backup {expected_backup_path:?}: {error}"
                    ))
                })?;
            if checkpoint.source_model.as_deref() != source_model.as_deref()
                || checkpoint.target_model != target_model
                || checkpoint.target_dim != target_dim
                || checkpoint_backup != expected_backup
            {
                return Err(Error::InvalidInput(
                    "embedding migration checkpoint does not match the live source/target tuple \
                     and canonical backup"
                        .to_string(),
                ));
            }
            verify_existing_backup(&checkpoint.backup_path)?;
            checkpoint
        }
        None => {
            SqliteStorage::create_verified_backup(&db_path, &expected_backup_path).map_err(
                |error| {
                    Error::StorageError(format!(
                        "create verified embedding migration backup {expected_backup_path:?}: \
                         {error}"
                    ))
                },
            )?;
            EmbeddingMigrationCheckpoint {
                source_model: source_model.clone(),
                source_dim,
                target_model: target_model.clone(),
                target_dim,
                selection,
                cursor: None,
                backup_path: expected_backup_path,
            }
        }
    };

    let total = inspection.embedding_dimensions.len();
    let mut storage = SqliteStorage::open(&db_path)?;
    if storage.embedding_migration_checkpoint()?.is_none() {
        storage.begin_embedding_migration(&checkpoint)?;
    }
    let resumed = resumed_count(&storage, &checkpoint)?;
    let mut migrated = 0usize;
    let mut batches = 0usize;

    loop {
        let candidates =
            storage.embedding_candidates(&checkpoint, EMBEDDING_MIGRATION_BATCH_SIZE)?;
        if candidates.is_empty() {
            break;
        }
        let mut replacements = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            let embedded = pending.provider.embed_passage(&candidate.content)?;
            if embedded.len() != target_dim {
                return Err(Error::InvalidInput(format!(
                    "node {} passage embedding has dimension {}, expected {}",
                    candidate.node_id.0,
                    embedded.len(),
                    target_dim
                )));
            }
            if !embedded.iter().all(|value| value.is_finite()) {
                return Err(Error::InvalidInput(format!(
                    "node {} passage embedding contains a non-finite value",
                    candidate.node_id.0
                )));
            }
            replacements.push(EmbeddingReplacement {
                node_id: candidate.node_id,
                embedding: widen(&embedded),
            });
        }
        let next_cursor = match checkpoint.selection {
            EmbeddingSelection::Dimension => None,
            EmbeddingSelection::Cursor => candidates.last().map(|candidate| candidate.node_id),
        };
        let committed = replacements.len();
        storage.commit_embedding_batch(EmbeddingBatch {
            replacements,
            next_cursor,
        })?;
        checkpoint.cursor = next_cursor;
        migrated += committed;
        batches += 1;
        progress(EmbeddingProgress {
            namespace: pending.namespace.clone(),
            committed: resumed + migrated,
            total,
            batch: batches,
            source_model: checkpoint.source_model.clone(),
            source_dimensions: checkpoint.source_dim,
            target_model: target_model.clone(),
            target_dimensions: target_dim,
        });
    }

    storage.finish_embedding_migration()?;
    drop(storage);
    reopen_with_normal_guard(&db_path, pending.provider)?;
    Ok(EmbeddingMigrationOutcome::Migrated(
        EmbeddingMigrationReport {
            scanned: total,
            migrated,
            resumed,
            batches,
            backup_path: checkpoint.backup_path,
        },
    ))
}

fn inventory_is_compatible(inspection: &EmbeddingMigrationInspection, target_dim: usize) -> bool {
    inspection.embedding_dimensions.is_empty()
        || inspection
            .embedding_dimensions
            .iter()
            .all(|dimension| *dimension == Some(target_dim))
}

fn source_dimension(inspection: &EmbeddingMigrationInspection) -> Result<Option<usize>, Error> {
    if let Some(checkpoint) = inspection.checkpoint.as_ref() {
        return Ok(checkpoint.source_dim);
    }
    let mut dimensions = inspection.embedding_dimensions.iter().flatten().copied();
    let first = dimensions.next();
    if dimensions.any(|dimension| Some(dimension) != first) {
        return Err(Error::InvalidInput(
            "embedding inventory has mixed dimensions without a durable migration checkpoint"
                .to_string(),
        ));
    }
    Ok(first)
}

fn verify_existing_backup(path: &Path) -> Result<(), Error> {
    let mut validation_path = OsString::from(path.as_os_str());
    validation_path.push(".verify");
    let validation_path = PathBuf::from(validation_path);
    SqliteStorage::create_verified_backup(path, &validation_path).map_err(|error| {
        Error::StorageError(format!(
            "verify existing embedding migration backup {path:?}: {error}"
        ))
    })?;
    std::fs::remove_file(&validation_path).map_err(|error| {
        Error::StorageError(format!(
            "remove backup verification copy {validation_path:?}: {error}"
        ))
    })
}

fn resumed_count(
    storage: &SqliteStorage,
    checkpoint: &EmbeddingMigrationCheckpoint,
) -> Result<usize, Error> {
    match checkpoint.selection {
        EmbeddingSelection::Dimension => Ok(storage
            .all_node_ids()
            .into_iter()
            .filter_map(|node_id| storage.get_node(node_id).ok())
            .filter(|node| {
                node.embedding
                    .as_ref()
                    .is_some_and(|embedding| embedding.len() == checkpoint.target_dim)
            })
            .count()),
        EmbeddingSelection::Cursor => Ok(checkpoint.cursor.map_or(0, |cursor| {
            storage
                .all_node_ids()
                .into_iter()
                .filter(|node_id| node_id.0 <= cursor.0)
                .count()
        })),
    }
}

fn reopen_with_normal_guard(
    db_path: &Path,
    provider: Arc<dyn EmbeddingProvider>,
) -> Result<(), Error> {
    let dimensions = provider.dimensions();
    let model = provider.model_name().to_string();
    let mut memory = Memory::with_provider(db_path, provider)?;
    verify_embedding_compatibility(&mut memory, dimensions, &model)
}
