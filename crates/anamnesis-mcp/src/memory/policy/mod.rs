//! MCP-owned, additive policy telemetry stored beside the engine graph.
//!
//! The store intentionally owns only its side schema. Callers retain ownership
//! of graph locking and may disable policy features when this schema is newer
//! than this binary understands.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(test)]
use std::{cell::RefCell, marker::PhantomData, rc::Rc};

use crate::extract::audit::ExtractionAuditResult;
use crate::extract::types::{AuditSupport, ContaminationCategory, RelationVerdict};
use crate::proto::RecallEventKind;
use crate::proto::{ExtractionErrorKind, StageExtractionResult};
use anamnesis::Error;
use rusqlite::{Connection, Error as SqliteError};

mod extraction;
mod recall;
mod schema;

pub(crate) use extraction::ExtractionProfileStatus;
pub(crate) use recall::RecallEvent;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecallStats {
    pub total_attempts: u64,
    pub by_event_kind: Vec<EventKindStats>,
    pub abstentions: AbstentionStats,
    pub cosine: CosineStats,
    pub auto_exposure: AutoExposureStats,
    pub sweep: Vec<SweepPoint>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EventKindStats {
    pub event_kind: RecallEventKind,
    pub attempts: u64,
    pub eligible: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AbstentionStats {
    pub empty: u64,
    pub readout_only: u64,
    pub cosine_only: u64,
    pub both: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CosineStats {
    pub samples: u64,
    pub nulls: u64,
    pub p50: Option<f64>,
    pub p90: Option<f64>,
    pub p95: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AutoExposureStats {
    pub eligible_events: u64,
    pub events_with_auto: u64,
    pub result_slots: u64,
    pub auto_slots: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SweepPoint {
    pub threshold: f64,
    pub eligible: u64,
    pub attempts: u64,
}

const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
thread_local! {
    static OPERATION_OBSERVER: RefCell<Option<Arc<dyn Fn() + Send + Sync>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct OperationObserverGuard {
    previous: Option<Arc<dyn Fn() + Send + Sync>>,
    _thread_bound: PhantomData<Rc<()>>,
}

#[cfg(test)]
impl Drop for OperationObserverGuard {
    fn drop(&mut self) {
        OPERATION_OBSERVER.with(|observer| {
            observer.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
fn observe_operation() {
    OPERATION_OBSERVER.with(|observer| {
        let observer = observer.borrow().clone();
        if let Some(observer) = observer {
            observer();
        }
    });
}
const UNSUPPORTED_VERSION_PREFIX: &str = "unsupported policy schema version ";

/// The registry-owned policy state handle for one namespace.
pub(crate) type PolicyStoreHandle = Arc<Mutex<PolicyStoreState>>;

/// A namespace's policy capability state. A newer side schema disables only
/// policy features; callers can continue using the engine graph normally.
pub(crate) enum PolicyStoreState {
    Uninitialized { path: Option<PathBuf> },
    Ready(PolicyStore),
    Disabled { reason: String },
}

/// Typed internal policy-store failure. Public facade methods translate this
/// into the engine's established error contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicyStoreError {
    UnsupportedVersion {
        version: i64,
    },
    Operation {
        operation: &'static str,
        sqlite_code: Option<i32>,
        sqlite_category: Option<String>,
        sqlite_source: Option<String>,
        path: Option<PathBuf>,
    },
    InvalidValue {
        field: &'static str,
    },
}

impl PolicyStoreError {
    fn operation(operation: &'static str) -> Self {
        Self::Operation {
            operation,
            sqlite_code: None,
            sqlite_category: None,
            sqlite_source: None,
            path: None,
        }
    }

    fn sqlite(operation: &'static str, error: SqliteError) -> Self {
        match error {
            SqliteError::SqliteFailure(error, source) => Self::Operation {
                operation,
                sqlite_code: Some(error.extended_code),
                sqlite_category: Some(format!("{:?}", error.code)),
                sqlite_source: source,
                path: None,
            },
            _ => Self::operation(operation),
        }
    }

    fn with_path(mut self, path: &Path) -> Self {
        if let Self::Operation {
            path: error_path, ..
        } = &mut self
        {
            *error_path = Some(path.to_path_buf());
        }
        self
    }

    fn invalid_value(field: &'static str) -> Self {
        Self::InvalidValue { field }
    }

    fn into_engine_error(self) -> Error {
        match self {
            Self::UnsupportedVersion { version } => {
                Error::StorageError(format!("{UNSUPPORTED_VERSION_PREFIX}{version}"))
            }
            Self::Operation {
                operation,
                sqlite_code,
                sqlite_category,
                sqlite_source,
                path,
            } => {
                let mut message = format!("policy store operation failed: {operation}");
                if let Some(path) = path {
                    message.push_str(&format!("; path: {}", path.display()));
                }
                if let Some(code) = sqlite_code {
                    message.push_str(&format!("; sqlite code: {code}"));
                }
                if let Some(category) = sqlite_category {
                    message.push_str(&format!("; sqlite category: {category}"));
                }
                if let Some(source) = sqlite_source {
                    message.push_str(&format!("; sqlite source: {source}"));
                }
                Error::StorageError(message)
            }
            Self::InvalidValue { field } => {
                Error::InvalidInput(format!("invalid policy store value: {field}"))
            }
        }
    }
}

/// Facade around the MCP-owned SQLite side schema.
pub(crate) struct PolicyStore {
    connection: Connection,
}

impl PolicyStore {
    /// Opens and transactionally converges the MCP side schema without changing
    /// SQLite journal mode or acquiring any graph/registry lock.
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let connection = Connection::open(path).map_err(|error| {
            PolicyStoreError::sqlite("open policy store", error)
                .with_path(path)
                .into_engine_error()
        })?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, Error> {
        connection.busy_timeout(BUSY_TIMEOUT).map_err(|error| {
            PolicyStoreError::sqlite("configure policy store busy timeout", error)
                .into_engine_error()
        })?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|error| {
                PolicyStoreError::sqlite("enable policy store foreign keys", error)
                    .into_engine_error()
            })?;
        schema::initialize(&mut connection).map_err(PolicyStoreError::into_engine_error)?;
        #[cfg(test)]
        observe_operation();
        Ok(Self { connection })
    }

    /// Inserts one data-minimized recall event and prunes older events in the
    /// same transaction, retaining only the newest `RECALL_EVENT_RETENTION` rows.
    pub(crate) fn insert_recall_event(&mut self, event: &RecallEvent) -> Result<(), Error> {
        #[cfg(test)]
        observe_operation();

        let transaction = self.connection.transaction().map_err(|error| {
            PolicyStoreError::sqlite("start recall event transaction", error).into_engine_error()
        })?;
        recall::insert(&transaction, event).map_err(PolicyStoreError::into_engine_error)?;
        transaction.commit().map_err(|error| {
            PolicyStoreError::sqlite("commit recall event transaction", error).into_engine_error()
        })
    }
    /// Aggregates data-minimized recall telemetry without exposing raw queries.
    pub(crate) fn recall_stats(&self) -> Result<RecallStats, Error> {
        recall::stats(&self.connection).map_err(PolicyStoreError::into_engine_error)
    }
    pub(crate) fn processed_extraction_turn_keys(
        &self,
        profile_id: &str,
    ) -> Result<std::collections::HashSet<String>, Error> {
        extraction::processed_turn_keys(&self.connection, profile_id)
            .map_err(PolicyStoreError::into_engine_error)
    }

    pub(crate) fn ensure_extraction_shadow_profile(
        &self,
        profile_id: &str,
        components: &crate::extract::types::ExtractorProfileComponents,
        created_at: u64,
    ) -> Result<ExtractionProfileStatus, Error> {
        extraction::ensure_shadow_profile(&self.connection, profile_id, components, created_at)
            .map_err(PolicyStoreError::into_engine_error)
    }
    pub(crate) fn stage_extraction(
        &mut self,
        profile_id: &str,
        profile_components: &crate::extract::types::ExtractorProfileComponents,
        llm_duration_ms: u64,
        sources: &[crate::extract::types::ExtractionSource],
        validated_extraction: &crate::extract::types::ValidatedExtraction,
    ) -> Result<StageExtractionResult, Error> {
        extraction::stage(
            &mut self.connection,
            profile_id,
            profile_components,
            llm_duration_ms,
            sources,
            validated_extraction,
        )
        .map_err(PolicyStoreError::into_engine_error)
    }

    pub(crate) fn record_extraction_failure(
        &mut self,
        profile_id: &str,
        turn_count: u32,
        llm_invoked: bool,
        error_kind: ExtractionErrorKind,
        duration_ms: u64,
    ) -> Result<(), Error> {
        extraction::record_failure(
            &mut self.connection,
            profile_id,
            turn_count,
            llm_invoked,
            error_kind,
            duration_ms,
        )
        .map_err(PolicyStoreError::into_engine_error)
    }
    pub(crate) fn list_extraction_audit(&self, limit: u32) -> Result<ExtractionAuditResult, Error> {
        extraction::list_audit(&self.connection, limit).map_err(PolicyStoreError::into_engine_error)
    }
    pub(crate) fn update_extraction_candidate_audit(
        &mut self,
        id: u64,
        support: AuditSupport,
        contamination: Option<ContaminationCategory>,
        reviewer: &str,
        reviewed_at: u64,
    ) -> Result<(), Error> {
        extraction::update_candidate_audit(
            &mut self.connection,
            id,
            support,
            contamination,
            reviewer,
            reviewed_at,
        )
        .map_err(PolicyStoreError::into_engine_error)
    }
    pub(crate) fn update_extraction_relation_audit(
        &mut self,
        id: u64,
        verdict: RelationVerdict,
        reviewer: &str,
        reviewed_at: u64,
    ) -> Result<(), Error> {
        extraction::update_relation_audit(&mut self.connection, id, verdict, reviewer, reviewed_at)
            .map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self, Error> {
        let connection = Connection::open_in_memory().map_err(|error| {
            PolicyStoreError::sqlite("open in-memory policy store", error).into_engine_error()
        })?;
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub(crate) fn from_test_connection(connection: Connection) -> Result<Self, Error> {
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub(crate) fn schema_version(&self) -> Result<i64, Error> {
        schema::schema_version(&self.connection).map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn schema_fingerprint(&self) -> Result<String, Error> {
        schema::schema_fingerprint(&self.connection).map_err(PolicyStoreError::into_engine_error)
    }
    #[cfg(test)]
    pub(crate) fn has_table(&self, table_name: &str) -> Result<bool, Error> {
        self.connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = ?1
                )",
                [table_name],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(|error| {
                PolicyStoreError::sqlite("check policy schema table", error).into_engine_error()
            })
    }
    #[cfg(test)]
    pub(crate) fn recall_event_count_for_test(&self) -> Result<u64, Error> {
        recall::count(&self.connection).map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn install_recall_event_insert_failure_trigger_for_test(&self) -> Result<(), Error> {
        recall::install_insert_failure_trigger(&self.connection)
            .map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn read_recall_events_for_test(&self) -> Result<Vec<RecallEvent>, Error> {
        recall::read_all(&self.connection).map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn recall_events_contain_raw_value_for_test(
        &self,
        value: &str,
    ) -> Result<bool, Error> {
        recall::contains_value(&self.connection, value).map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn install_operation_observer_for_test(
        observer: Arc<dyn Fn() + Send + Sync>,
    ) -> OperationObserverGuard {
        let previous = OPERATION_OBSERVER.with(|installed| installed.replace(Some(observer)));
        OperationObserverGuard {
            previous,
            _thread_bound: PhantomData,
        }
    }
}
#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::Connection;

    use super::PolicyStoreError;

    #[test]
    fn sqlite_failures_retain_actionable_open_evidence_without_sql() {
        let connection = Connection::open_in_memory().expect("open policy database");
        connection
            .execute_batch("CREATE TABLE policy_error_probe (value INTEGER CHECK(value = 0));")
            .expect("create policy error probe");
        let sqlite_error = connection
            .execute("INSERT INTO policy_error_probe (value) VALUES (1)", [])
            .expect_err("invalid probe value must fail");

        let message = PolicyStoreError::sqlite("open policy store", sqlite_error)
            .with_path(Path::new("/safe/policy.db"))
            .into_engine_error()
            .to_string();

        assert!(message.contains("open policy store"));
        assert!(message.contains("path: /safe/policy.db"));
        assert!(message.contains("sqlite code:"));
        assert!(message.contains("sqlite category:"));
        assert!(message.contains("sqlite source:"));
        assert!(!message.contains("INSERT"));
    }
}
