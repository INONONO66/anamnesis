use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use anamnesis::embedding::{EmbeddingProvider, widen};
use anamnesis::storage::sqlite::{
    EmbeddingBatch, EmbeddingMigrationCheckpoint, EmbeddingMigrationInspection,
    EmbeddingReplacement, EmbeddingSelection,
};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use anamnesis::{Error, Memory};

use super::{
    EmbeddingMigrationFailure, EmbeddingMigrationOutcome, EmbeddingMigrationReport,
    EmbeddingMigrationRequest, EmbeddingProgress, MemoryRegistry, MigrationBackupState,
    MigrationFailureContext, MigrationLockLease, PendingEmbeddingMigrationRequest,
    acquire_namespace_migration_lock, backup_path_for_database, verify_embedding_compatibility,
};

pub(crate) const EMBEDDING_MIGRATION_BATCH_SIZE: usize = 64;

pub(crate) fn migrate_embeddings(
    request: EmbeddingMigrationRequest,
    progress: &mut dyn FnMut(EmbeddingProgress),
) -> Result<EmbeddingMigrationOutcome, EmbeddingMigrationFailure> {
    let mut cleanup_verification_copy = remove_backup_verification_copy;
    migrate_embeddings_with_backup_verification_cleanup(
        request,
        progress,
        &mut cleanup_verification_copy,
    )
}

pub(super) fn migrate_embeddings_with_backup_verification_cleanup(
    request: EmbeddingMigrationRequest,
    progress: &mut dyn FnMut(EmbeddingProgress),
    cleanup_verification_copy: &mut dyn FnMut(&Path) -> Result<(), Error>,
) -> Result<EmbeddingMigrationOutcome, EmbeddingMigrationFailure> {
    let mut backup_state = MigrationBackupState::NoBackupCreated;
    let mut failure_context = MigrationFailureContext::FixedCategory;
    migrate_embeddings_inner(
        request,
        progress,
        &mut backup_state,
        &mut failure_context,
        cleanup_verification_copy,
    )
    .map_err(|source| EmbeddingMigrationFailure::new(backup_state, failure_context, source))
}

fn migrate_embeddings_inner(
    request: EmbeddingMigrationRequest,
    progress: &mut dyn FnMut(EmbeddingProgress),
    backup_state: &mut MigrationBackupState,
    failure_context: &mut MigrationFailureContext,
    cleanup_verification_copy: &mut dyn FnMut(&Path) -> Result<(), Error>,
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
            let checkpoint_backup = match std::fs::canonicalize(&checkpoint.backup_path) {
                Ok(path) => path,
                Err(error) => {
                    *failure_context = MigrationFailureContext::CheckpointBackupValidation {
                        backup_path: checkpoint.backup_path.clone(),
                    };
                    return Err(Error::StorageError(format!(
                        "resolve checkpoint backup {:?}: {error}",
                        checkpoint.backup_path
                    )));
                }
            };
            let expected_backup = match std::fs::canonicalize(&expected_backup_path) {
                Ok(path) => path,
                Err(error) => {
                    *failure_context = MigrationFailureContext::CheckpointBackupValidation {
                        backup_path: checkpoint.backup_path.clone(),
                    };
                    return Err(Error::StorageError(format!(
                        "resolve expected migration backup {expected_backup_path:?}: {error}"
                    )));
                }
            };
            if checkpoint.source_model.as_deref() != source_model.as_deref()
                || checkpoint.target_model != target_model
                || checkpoint.target_dim != target_dim
                || checkpoint_backup != expected_backup
            {
                *failure_context = MigrationFailureContext::CheckpointBackupValidation {
                    backup_path: checkpoint.backup_path.clone(),
                };
                return Err(Error::InvalidInput(
                    "embedding migration checkpoint does not match the live source/target tuple \
                     and canonical backup"
                        .to_string(),
                ));
            }
            let validation_path = match verify_existing_backup(&checkpoint.backup_path) {
                Ok(path) => path,
                Err(error) => {
                    *failure_context = MigrationFailureContext::CheckpointBackupVerification {
                        backup_path: checkpoint.backup_path.clone(),
                    };
                    return Err(error);
                }
            };
            *backup_state = MigrationBackupState::BackupPreserved {
                backup_path: checkpoint.backup_path.clone(),
            };
            if let Err(error) = cleanup_verification_copy(&validation_path) {
                *failure_context = MigrationFailureContext::BackupVerificationCleanup {
                    backup_path: checkpoint.backup_path.clone(),
                    validation_path,
                };
                return Err(error);
            }
            checkpoint
        }
        None => {
            let backup_result =
                SqliteStorage::create_verified_backup(&db_path, &expected_backup_path).map_err(
                    |error| {
                        Error::StorageError(format!(
                            "create verified embedding migration backup {expected_backup_path:?}: \
                             {error}"
                        ))
                    },
                );
            if let Err(error) = backup_result {
                *failure_context = MigrationFailureContext::BackupCreation {
                    backup_path: expected_backup_path.clone(),
                    destination_exists: expected_backup_path.exists(),
                };
                return Err(error);
            }
            *backup_state = MigrationBackupState::BackupPreserved {
                backup_path: expected_backup_path.clone(),
            };
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

fn verify_existing_backup(path: &Path) -> Result<PathBuf, Error> {
    let mut validation_path = OsString::from(path.as_os_str());
    validation_path.push(".verify");
    let validation_path = PathBuf::from(validation_path);
    SqliteStorage::create_verified_backup(path, &validation_path).map_err(|error| {
        Error::StorageError(format!(
            "verify existing embedding migration backup {path:?}: {error}"
        ))
    })?;
    Ok(validation_path)
}

fn remove_backup_verification_copy(validation_path: &Path) -> Result<(), Error> {
    std::fs::remove_file(validation_path).map_err(|error| {
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

pub(crate) struct MigrationSupervisor {
    jobs: HashMap<String, std::thread::JoinHandle<()>>,
    registry: Weak<Mutex<MemoryRegistry>>,
    failures: Arc<Mutex<Vec<String>>>,
}

impl MigrationSupervisor {
    fn new(registry: &Arc<Mutex<MemoryRegistry>>) -> Self {
        Self {
            jobs: HashMap::new(),
            registry: Arc::downgrade(registry),
            failures: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn spawn_leased(
        &mut self,
        key: String,
        request: EmbeddingMigrationRequest,
    ) -> Result<(), Error> {
        if self.jobs.contains_key(&key) {
            return Ok(());
        }
        let registry = self.registry.clone();
        let failures = Arc::clone(&self.failures);
        let worker_key = key.clone();
        let handle = std::thread::Builder::new()
            .name(format!("embedding-migration-{key}"))
            .spawn(move || {
                let mut progress = |event: EmbeddingProgress| {
                    tracing::info!(
                        namespace = %event.namespace,
                        committed = event.committed,
                        total = event.total,
                        batch = event.batch,
                        source_model = ?event.source_model,
                        source_dimensions = ?event.source_dimensions,
                        target_model = %event.target_model,
                        target_dimensions = event.target_dimensions,
                        "embedding migration batch committed"
                    );
                };
                let state = migrate_embeddings(request, &mut progress)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                if let Some(registry) = registry.upgrade() {
                    registry
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .finish_namespace_migration(&worker_key, state.clone());
                }
                match state {
                    Ok(()) => tracing::info!(
                        namespace = %worker_key,
                        "embedding migration completed"
                    ),
                    Err(message) => {
                        tracing::error!(
                            namespace = %worker_key,
                            error = %message,
                            "embedding migration failed"
                        );
                        failures
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(format!("{worker_key}: {message}"));
                    }
                }
            })
            .map_err(|error| {
                Error::StorageError(format!("spawn embedding migration for {key}: {error}"))
            })?;
        self.jobs.insert(key, handle);
        Ok(())
    }

    pub(crate) fn drain(&mut self) -> Result<(), Error> {
        let jobs = std::mem::take(&mut self.jobs);
        let mut errors = Vec::new();
        for (key, handle) in jobs {
            if handle.join().is_err() {
                errors.push(format!("{key}: migration worker panicked"));
            }
        }
        errors.extend(std::mem::take(
            &mut *self
                .failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        ));
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::StorageError(format!(
                "embedding migration workers failed: {}",
                errors.join("; ")
            )))
        }
    }

    #[cfg(test)]
    fn job_count(&self) -> usize {
        self.jobs.len()
    }
}

pub(crate) struct MigrationRuntime {
    supervisor: Mutex<MigrationSupervisor>,
    default_db_lock: Arc<std::fs::File>,
    default_db_path: PathBuf,
    #[cfg(test)]
    lock_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl MigrationRuntime {
    pub(crate) fn new(
        registry: &Arc<Mutex<MemoryRegistry>>,
        default_db_path: PathBuf,
        default_lease: MigrationLockLease,
    ) -> Result<Self, Error> {
        Ok(Self {
            supervisor: Mutex::new(MigrationSupervisor::new(registry)),
            default_db_lock: default_lease.into_daemon_default()?,
            default_db_path,
            #[cfg(test)]
            lock_observer: Mutex::new(None),
        })
    }

    pub(crate) fn spawn_once(
        &self,
        key: String,
        pending: PendingEmbeddingMigrationRequest,
    ) -> Result<(), Error> {
        #[cfg(test)]
        if let Some(observer) = self
            .lock_observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            observer();
        }
        let lease = if pending.db_path == self.default_db_path {
            MigrationLockLease::from(Arc::clone(&self.default_db_lock))
        } else {
            acquire_namespace_migration_lock(&pending.db_path)?
        };
        self.supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .spawn_leased(key, EmbeddingMigrationRequest::from((pending, lease)))
    }

    pub(crate) fn drain(&self) -> Result<(), Error> {
        self.supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
    }

    #[cfg(test)]
    pub(crate) fn job_count(&self) -> usize {
        self.supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .job_count()
    }

    #[cfg(test)]
    pub(crate) fn set_lock_observer(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        *self
            .lock_observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(observer);
    }
}
