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

use crate::proto::RecallEventKind;
use anamnesis::Error;
use rusqlite::Connection;

mod recall;
mod schema;

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

const SCHEMA_VERSION: i64 = 1;
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
    UnsupportedVersion { version: i64 },
    Operation { operation: &'static str },
    InvalidValue { field: &'static str },
}

impl PolicyStoreError {
    fn operation(operation: &'static str) -> Self {
        Self::Operation { operation }
    }

    fn invalid_value(field: &'static str) -> Self {
        Self::InvalidValue { field }
    }

    fn into_engine_error(self) -> Error {
        match self {
            Self::UnsupportedVersion { version } => {
                Error::StorageError(format!("{UNSUPPORTED_VERSION_PREFIX}{version}"))
            }
            Self::Operation { operation } => {
                Error::StorageError(format!("policy store operation failed: {operation}"))
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
        let connection = Connection::open(path)
            .map_err(|_| PolicyStoreError::operation("open policy store").into_engine_error())?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, Error> {
        connection.busy_timeout(BUSY_TIMEOUT).map_err(|_| {
            PolicyStoreError::operation("configure policy store busy timeout").into_engine_error()
        })?;
        schema::initialize(&mut connection).map_err(PolicyStoreError::into_engine_error)?;
        #[cfg(test)]
        observe_operation();
        Ok(Self { connection })
    }

    /// Inserts one data-minimized recall event and prunes older events in the
    /// same transaction, retaining only the newest [`RECALL_EVENT_RETENTION`].
    pub(crate) fn insert_recall_event(&mut self, event: &RecallEvent) -> Result<(), Error> {
        #[cfg(test)]
        observe_operation();

        let transaction = self.connection.transaction().map_err(|_| {
            PolicyStoreError::operation("start recall event transaction").into_engine_error()
        })?;
        recall::insert(&transaction, event).map_err(PolicyStoreError::into_engine_error)?;
        transaction.commit().map_err(|_| {
            PolicyStoreError::operation("commit recall event transaction").into_engine_error()
        })
    }
    /// Aggregates data-minimized recall telemetry without exposing raw queries.
    pub(crate) fn recall_stats(&self) -> Result<RecallStats, Error> {
        recall::stats(&self.connection).map_err(PolicyStoreError::into_engine_error)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self, Error> {
        let connection = Connection::open_in_memory().map_err(|_| {
            PolicyStoreError::operation("open in-memory policy store").into_engine_error()
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
