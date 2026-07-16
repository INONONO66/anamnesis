use std::fmt;
use std::fs::OpenOptions;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::client::DaemonClient;
use crate::config::Config;
use crate::daemon::socket_path_for_db;
use crate::extract::config::{ExtractCommand, ExtractConfig, ExtractMode};
use crate::extract::error_log::{ErrorLogKind, append_connect_failure};
use crate::extract::process::{OutputStream, ProcessError, ProcessOutput, run_provider};
use crate::extract::profile::ExtractorProfile;
use crate::extract::prompt::build_extraction_prompt;
use crate::extract::types::{
    ExtractionScanResult, ExtractionSource, ExtractorProfileComponents, ValidatedExtraction,
};
use crate::extract::validate::{ValidationError, validate_output};
use crate::proto::{ExtractionErrorKind, StageExtractionResult};

const MIN_TURNS: u32 = 10;
const MAX_TURNS: u32 = 20;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(120);
const PROVIDER_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerConfig {
    pub(crate) mode: ExtractMode,
    pub(crate) profile: ExtractorProfileComponents,
    pub(crate) command: ExtractCommand,
    pub(crate) provider_timeout: Duration,
    pub(crate) provider_output_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerNoop {
    ModeOff,
    WorkerBusy,
    BelowThreshold,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerOutcome {
    Noop(WorkerNoop),
    Staged {
        run_id: u64,
        candidate_count: usize,
        relation_count: usize,
    },
    AlreadyStaged {
        run_id: u64,
        candidate_count: usize,
        relation_count: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerError {
    Daemon(String),
    Process(ProcessError),
    Validation(ValidationError),
    Profile(String),
    Runtime(String),
    Lock(String),
    Audit,
}

impl fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Daemon(_) => formatter.write_str("extraction daemon request failed"),
            Self::Process(error) => write!(formatter, "{error}"),
            Self::Validation(error) => {
                write!(formatter, "extraction output validation failed: {error}")
            }
            Self::Profile(_) => formatter.write_str("could not construct extraction profile"),
            Self::Runtime(_) => formatter.write_str("could not start extraction worker runtime"),
            Self::Lock(_) => formatter.write_str("could not acquire extraction worker lock"),
            Self::Audit => formatter.write_str("could not record extraction failure audit"),
        }
    }
}

impl std::error::Error for WorkerError {}

pub(crate) trait WorkerDependencies {
    fn connect(&mut self, socket_path: &Path) -> Result<(), WorkerError>;
    fn scan(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        min_turns: u32,
        max_turns: u32,
    ) -> Result<ExtractionScanResult, WorkerError>;
    fn invoke_provider(
        &mut self,
        command: &ExtractCommand,
        prompt: &[u8],
        timeout: Duration,
        output_limit: usize,
    ) -> Result<ProcessOutput, ProcessError>;
    fn record_failure(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        turn_count: u32,
        llm_invoked: bool,
        error_kind: ExtractionErrorKind,
        duration_ms: u64,
    ) -> Result<(), WorkerError>;
    fn stage(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        duration_ms: u64,
        sources: Vec<ExtractionSource>,
        extraction: ValidatedExtraction,
    ) -> Result<StageExtractionResult, WorkerError>;
}

/// Run one opt-in extraction pass using the daemon as the sole persistence API.
pub(crate) fn run_worker(
    cfg: &Config,
    namespace: Option<&str>,
) -> Result<WorkerOutcome, WorkerError> {
    let extract_config =
        ExtractConfig::from_env().map_err(|error| WorkerError::Profile(error.to_string()))?;
    if let Some(warning) = extract_config.mode_warning.as_deref() {
        tracing::warn!(warning = %warning, "extraction is disabled");
    }
    if extract_config.mode != ExtractMode::Shadow {
        return Ok(WorkerOutcome::Noop(WorkerNoop::ModeOff));
    }
    let profile = ExtractorProfile::from_command(&extract_config.command)
        .map_err(|error| WorkerError::Profile(error.to_string()))?;
    let socket_path = socket_path_for_db(&cfg.default_db)
        .map_err(|error| WorkerError::Daemon(error.to_string()))?;
    let worker_config = WorkerConfig {
        mode: extract_config.mode,
        profile: profile.components,
        command: extract_config.command,
        provider_timeout: PROVIDER_TIMEOUT,
        provider_output_limit: PROVIDER_OUTPUT_LIMIT,
    };
    let config = cfg.clone();
    let namespace = namespace.map(str::to_owned);
    std::thread::Builder::new()
        .name("anamnesis-extract".into())
        .spawn(move || {
            let mut dependencies = ProductionDependencies::new(&config)?;
            run_worker_with(
                &worker_config,
                &socket_path,
                namespace.as_deref(),
                &mut dependencies,
            )
        })
        .map_err(|error| WorkerError::Runtime(error.to_string()))?
        .join()
        .map_err(|_| WorkerError::Runtime("extraction worker thread terminated".into()))?
}

/// Deterministic worker core. The lock guard remains held through the complete pass.
pub(crate) fn run_worker_with(
    config: &WorkerConfig,
    socket_path: &Path,
    namespace: Option<&str>,
    dependencies: &mut impl WorkerDependencies,
) -> Result<WorkerOutcome, WorkerError> {
    if config.mode != ExtractMode::Shadow {
        return Ok(WorkerOutcome::Noop(WorkerNoop::ModeOff));
    }

    let mut lock_path = socket_path.as_os_str().to_os_string();
    lock_path.push(".extract.lock");
    let lock_path = std::path::PathBuf::from(lock_path);
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| WorkerError::Lock(error.to_string()))?;
    match fs4::FileExt::try_lock(&lock) {
        Ok(()) => {}
        Err(fs4::TryLockError::WouldBlock) => {
            return Ok(WorkerOutcome::Noop(WorkerNoop::WorkerBusy));
        }
        Err(error) => return Err(WorkerError::Lock(error.to_string())),
    }

    dependencies.connect(socket_path)?;
    let scan = dependencies.scan(namespace, &config.profile, MIN_TURNS, MAX_TURNS)?;
    if scan.sources.len() < MIN_TURNS as usize {
        return Ok(WorkerOutcome::Noop(WorkerNoop::BelowThreshold));
    }

    let prompt = build_extraction_prompt(&scan.sources);
    let (extraction, duration_ms) = match invoke_and_validate(config, &scan, dependencies, &prompt)
    {
        Ok(success) => success,
        Err(first_failure) if first_failure.is_invalid_json() => {
            if record_invocation_failure(
                dependencies,
                namespace,
                &config.profile,
                &scan,
                &first_failure,
            )
            .is_err()
            {
                return Err(WorkerError::Audit);
            }
            match invoke_and_validate(config, &scan, dependencies, &prompt) {
                Ok(success) => success,
                Err(second_failure) => {
                    if record_invocation_failure(
                        dependencies,
                        namespace,
                        &config.profile,
                        &scan,
                        &second_failure,
                    )
                    .is_err()
                    {
                        return Err(WorkerError::Audit);
                    }
                    return Err(second_failure.error);
                }
            }
        }
        Err(failure) => {
            if record_invocation_failure(dependencies, namespace, &config.profile, &scan, &failure)
                .is_err()
            {
                return Err(WorkerError::Audit);
            }
            return Err(failure.error);
        }
    };

    let candidate_count = extraction.items.len();
    let relation_count = extraction.relations.len();
    match dependencies.stage(
        namespace,
        &config.profile,
        duration_ms,
        scan.sources.clone(),
        extraction,
    ) {
        Ok(StageExtractionResult::Staged { run_id }) => Ok(WorkerOutcome::Staged {
            run_id,
            candidate_count,
            relation_count,
        }),
        Ok(StageExtractionResult::AlreadyStaged { run_id }) => Ok(WorkerOutcome::AlreadyStaged {
            run_id,
            candidate_count,
            relation_count,
        }),
        Err(error) => {
            if record_failure(
                dependencies,
                namespace,
                &config.profile,
                &scan,
                ExtractionErrorKind::StageReject,
                true,
                duration_ms,
            )
            .is_err()
            {
                return Err(WorkerError::Audit);
            }
            Err(error)
        }
    }
}

struct InvocationFailure {
    error: WorkerError,
    duration_ms: u64,
}

impl InvocationFailure {
    fn is_invalid_json(&self) -> bool {
        matches!(
            &self.error,
            WorkerError::Validation(ValidationError::InvalidJson)
        )
    }
}

fn invoke_and_validate(
    config: &WorkerConfig,
    scan: &ExtractionScanResult,
    dependencies: &mut impl WorkerDependencies,
    prompt: &str,
) -> Result<(ValidatedExtraction, u64), InvocationFailure> {
    let started = Instant::now();
    let output = dependencies
        .invoke_provider(
            &config.command,
            prompt.as_bytes(),
            config.provider_timeout,
            config.provider_output_limit,
        )
        .map_err(|error| InvocationFailure {
            error: WorkerError::Process(error),
            duration_ms: duration_ms(started),
        })?;
    let duration_ms = duration_ms_from_duration(output.duration);
    let extraction =
        validate_output(&output.stdout, &scan.sources, &scan.profile_id).map_err(|error| {
            InvocationFailure {
                error: WorkerError::Validation(error),
                duration_ms,
            }
        })?;
    Ok((extraction, duration_ms))
}

fn record_failure(
    dependencies: &mut impl WorkerDependencies,
    namespace: Option<&str>,
    profile: &ExtractorProfileComponents,
    scan: &ExtractionScanResult,
    error_kind: ExtractionErrorKind,
    llm_invoked: bool,
    duration_ms: u64,
) -> Result<(), WorkerError> {
    let turn_count = u32::try_from(scan.sources.len())
        .map_err(|_| WorkerError::Daemon("extraction turn count exceeds protocol range".into()))?;
    dependencies.record_failure(
        namespace,
        profile,
        turn_count,
        llm_invoked,
        error_kind,
        duration_ms,
    )
}

fn record_invocation_failure(
    dependencies: &mut impl WorkerDependencies,
    namespace: Option<&str>,
    profile: &ExtractorProfileComponents,
    scan: &ExtractionScanResult,
    failure: &InvocationFailure,
) -> Result<(), WorkerError> {
    record_failure(
        dependencies,
        namespace,
        profile,
        scan,
        failure_kind(&failure.error),
        !matches!(&failure.error, WorkerError::Process(ProcessError::Spawn)),
        failure.duration_ms,
    )
}

fn failure_kind(error: &WorkerError) -> ExtractionErrorKind {
    match error {
        WorkerError::Process(ProcessError::Spawn) => ExtractionErrorKind::Spawn,
        WorkerError::Process(ProcessError::Stdin) => ExtractionErrorKind::Stdin,
        WorkerError::Process(ProcessError::Timeout) => ExtractionErrorKind::Timeout,
        WorkerError::Process(ProcessError::OutputTooLarge {
            stream: OutputStream::Stdout,
        }) => ExtractionErrorKind::StdoutTooLarge,
        WorkerError::Process(ProcessError::OutputTooLarge {
            stream: OutputStream::Stderr,
        }) => ExtractionErrorKind::StderrTooLarge,
        WorkerError::Process(ProcessError::NonZero { .. }) => ExtractionErrorKind::NonZero,
        WorkerError::Process(ProcessError::Wait) => ExtractionErrorKind::SchemaReject,
        WorkerError::Validation(ValidationError::InvalidUtf8) => ExtractionErrorKind::InvalidUtf8,
        WorkerError::Validation(ValidationError::InvalidJson) => ExtractionErrorKind::InvalidJson,
        WorkerError::Validation(
            ValidationError::SchemaReject
            | ValidationError::TooManyItems
            | ValidationError::InvalidItemId
            | ValidationError::DuplicateItemId
            | ValidationError::InvalidContent
            | ValidationError::InvalidConfidence
            | ValidationError::InvalidSourceReference
            | ValidationError::InvalidRelationReference
            | ValidationError::SelfRelation
            | ValidationError::DuplicateCandidateKey
            | ValidationError::DuplicateRelation,
        )
        | WorkerError::Daemon(_)
        | WorkerError::Profile(_)
        | WorkerError::Runtime(_)
        | WorkerError::Lock(_)
        | WorkerError::Audit => ExtractionErrorKind::SchemaReject,
    }
}

fn duration_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn duration_ms_from_duration(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

struct ProductionDependencies {
    runtime: tokio::runtime::Runtime,
    daemon: Option<DaemonClient>,
    config: Config,
}

impl ProductionDependencies {
    fn new(config: &Config) -> Result<Self, WorkerError> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|error| WorkerError::Runtime(error.to_string()))?;
        Ok(Self {
            runtime,
            daemon: None,
            config: config.clone(),
        })
    }
}

impl WorkerDependencies for ProductionDependencies {
    fn connect(&mut self, socket_path: &Path) -> Result<(), WorkerError> {
        match self.runtime.block_on(DaemonClient::connect(&self.config)) {
            Ok(client) => {
                self.daemon = Some(client);
                Ok(())
            }
            Err(error) => {
                let _ = append_connect_failure(
                    &self.config.db_dir(),
                    socket_path,
                    ErrorLogKind::Connect,
                );
                Err(WorkerError::Daemon(error.to_string()))
            }
        }
    }

    fn scan(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        min_turns: u32,
        max_turns: u32,
    ) -> Result<ExtractionScanResult, WorkerError> {
        let namespace = namespace.map(str::to_owned);
        let ProductionDependencies {
            runtime, daemon, ..
        } = self;
        let daemon = daemon
            .as_mut()
            .ok_or_else(|| WorkerError::Daemon("daemon client is not connected".into()))?;
        runtime
            .block_on(daemon.extraction_scan(namespace.as_deref(), profile, min_turns, max_turns))
            .map_err(|error| WorkerError::Daemon(error.to_string()))
    }

    fn invoke_provider(
        &mut self,
        command: &ExtractCommand,
        prompt: &[u8],
        timeout: Duration,
        output_limit: usize,
    ) -> Result<ProcessOutput, ProcessError> {
        self.runtime
            .block_on(run_provider(command, prompt, timeout, output_limit))
    }

    fn record_failure(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        turn_count: u32,
        llm_invoked: bool,
        error_kind: ExtractionErrorKind,
        duration_ms: u64,
    ) -> Result<(), WorkerError> {
        let namespace = namespace.map(str::to_owned);
        let ProductionDependencies {
            runtime, daemon, ..
        } = self;
        let daemon = daemon
            .as_mut()
            .ok_or_else(|| WorkerError::Daemon("daemon client is not connected".into()))?;
        runtime
            .block_on(daemon.record_extraction_failure(
                namespace.as_deref(),
                profile,
                turn_count,
                llm_invoked,
                error_kind,
                duration_ms,
            ))
            .map_err(|error| WorkerError::Daemon(error.to_string()))
    }

    fn stage(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        duration_ms: u64,
        sources: Vec<ExtractionSource>,
        extraction: ValidatedExtraction,
    ) -> Result<StageExtractionResult, WorkerError> {
        let namespace = namespace.map(str::to_owned);
        let ProductionDependencies {
            runtime, daemon, ..
        } = self;
        let daemon = daemon
            .as_mut()
            .ok_or_else(|| WorkerError::Daemon("daemon client is not connected".into()))?;
        runtime
            .block_on(daemon.stage_extraction(
                namespace.as_deref(),
                profile,
                duration_ms,
                sources,
                extraction,
            ))
            .map_err(|error| WorkerError::Daemon(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{
        MIN_TURNS, PROVIDER_TIMEOUT, WorkerConfig, WorkerDependencies, WorkerError, WorkerNoop,
        WorkerOutcome, failure_kind, run_worker, run_worker_with,
    };
    use crate::config::Config;
    use crate::extract::{
        config::{ExtractCommand, ExtractMode},
        process::{OutputStream, ProcessError, ProcessOutput},
        types::{
            ExtractionScanResult, ExtractionSource, ExtractorProfileComponents, ValidatedExtraction,
        },
        validate::ValidationError,
    };
    use crate::proto::{ExtractionErrorKind, StageExtractionResult};

    fn test_socket() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = directory.path().join("daemon.sock");
        (directory, socket)
    }

    #[derive(Default)]
    struct FakeWorker {
        daemon_connects: usize,
        events: Vec<&'static str>,
        provider_calls: usize,
        scans: VecDeque<Result<ExtractionScanResult, WorkerError>>,
        provider_outputs: VecDeque<Result<ProcessOutput, ProcessError>>,
        recorded_failures: Vec<ExtractionErrorKind>,
        recorded_failure_details: Vec<(ExtractionErrorKind, bool, u64)>,
        failure_record_result: VecDeque<Result<(), WorkerError>>,
        staged: Vec<ValidatedExtraction>,
        staged_sources: Vec<Vec<ExtractionSource>>,
        stage_result: VecDeque<Result<StageExtractionResult, WorkerError>>,
    }

    impl WorkerDependencies for FakeWorker {
        fn connect(&mut self, _socket_path: &Path) -> Result<(), WorkerError> {
            self.daemon_connects += 1;
            Ok(())
        }

        fn scan(
            &mut self,
            _namespace: Option<&str>,
            _profile: &ExtractorProfileComponents,
            min_turns: u32,
            max_turns: u32,
        ) -> Result<ExtractionScanResult, WorkerError> {
            assert_eq!((min_turns, max_turns), (MIN_TURNS, 20));
            self.scans.pop_front().expect("test supplied scan response")
        }

        fn invoke_provider(
            &mut self,
            _command: &ExtractCommand,
            _prompt: &[u8],
            _timeout: Duration,
            _output_limit: usize,
        ) -> Result<ProcessOutput, ProcessError> {
            self.provider_calls += 1;
            self.events.push("provider");
            self.provider_outputs
                .pop_front()
                .expect("test supplied provider response")
        }

        fn record_failure(
            &mut self,
            _namespace: Option<&str>,
            _profile: &ExtractorProfileComponents,
            _turn_count: u32,
            _llm_invoked: bool,
            error_kind: ExtractionErrorKind,
            _duration_ms: u64,
        ) -> Result<(), WorkerError> {
            self.recorded_failures.push(error_kind);
            self.recorded_failure_details
                .push((error_kind, _llm_invoked, _duration_ms));
            self.events.push("failure-record");
            self.failure_record_result.pop_front().unwrap_or(Ok(()))
        }

        fn stage(
            &mut self,
            _namespace: Option<&str>,
            _profile: &ExtractorProfileComponents,
            _duration_ms: u64,
            sources: Vec<ExtractionSource>,
            extraction: ValidatedExtraction,
        ) -> Result<StageExtractionResult, WorkerError> {
            self.staged.push(extraction);
            self.events.push("stage");
            self.staged_sources.push(sources);
            self.stage_result
                .pop_front()
                .expect("test supplied stage response")
        }
    }

    fn config(mode: ExtractMode) -> WorkerConfig {
        WorkerConfig {
            mode,
            profile: ExtractorProfileComponents {
                provider_id: "fixture".into(),
                model_id: "fixture-model".into(),
                prompt_version: 1,
                schema_version: 1,
                normalization_version: 1,
                relation_policy_version: 1,
                command_hash: "fixture-command-hash".into(),
            },
            command: ExtractCommand {
                program: "fixture-provider".into(),
                args: vec!["--json".into()],
            },
            provider_timeout: Duration::from_secs(1),
            provider_output_limit: 1024 * 1024,
        }
    }

    fn source(index: u64) -> ExtractionSource {
        ExtractionSource {
            node_id: index,
            turn_key: format!("turn-{index}"),
            session_id: "session".into(),
            scope: "scope".into(),
            content: format!("source-{index}"),
            content_hash: format!("hash-{index}"),
            at_ms: index,
        }
    }

    fn scan(turns: usize) -> ExtractionScanResult {
        ExtractionScanResult {
            profile_id: "profile".into(),
            sources: (0..turns as u64).map(source).collect(),
        }
    }

    fn output(stdout: Vec<u8>) -> ProcessOutput {
        ProcessOutput {
            stdout,
            duration: Duration::from_millis(17),
        }
    }

    fn valid_output() -> ProcessOutput {
        output(br#"{"items":[],"relations":[]}"#.to_vec())
    }

    #[test]
    fn r2_worker_exposes_the_one_shot_entrypoint() {
        let _: fn(&Config, Option<&str>) -> Result<WorkerOutcome, WorkerError> = run_worker;
    }
    #[test]
    fn r2_worker_provider_timeout_matches_approved_contract() {
        assert_eq!(PROVIDER_TIMEOUT, Duration::from_secs(120));
    }
    #[test]
    fn r2_worker_mode_off_does_not_connect_or_invoke_provider() {
        let mut fake = FakeWorker::default();
        let (_socket_directory, socket) = test_socket();

        let outcome = run_worker_with(&config(ExtractMode::Off), &socket, None, &mut fake)
            .expect("off mode is a no-op");

        assert_eq!(outcome, WorkerOutcome::Noop(WorkerNoop::ModeOff));
        assert_eq!(fake.daemon_connects, 0);
        assert_eq!(fake.provider_calls, 0);
    }

    #[test]
    fn r2_worker_preheld_socket_lock_returns_busy_before_daemon_or_provider() {
        let tempdir = tempfile::tempdir().expect("temporary lock directory");
        let socket = tempdir.path().join("daemon.sock");
        let lock_path = PathBuf::from(format!("{}.extract.lock", socket.display()));
        let lock_file = std::fs::File::create(&lock_path).expect("create lock file");
        fs4::FileExt::try_lock(&lock_file).expect("prehold extraction lock");
        let mut fake = FakeWorker::default();

        let outcome = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
            .expect("held lock is a no-op");

        assert_eq!(outcome, WorkerOutcome::Noop(WorkerNoop::WorkerBusy));
        assert_eq!(fake.daemon_connects, 0);
        assert_eq!(
            fake.provider_calls, 0,
            "fixture invocation counter must remain zero"
        );
    }

    #[test]
    fn r2_worker_nine_turn_scan_is_below_threshold_without_a_run() {
        let mut fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize - 1))]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let outcome = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
            .expect("short scan is a no-op");

        assert_eq!(outcome, WorkerOutcome::Noop(WorkerNoop::BelowThreshold));
        assert_eq!(fake.daemon_connects, 1);
        assert_eq!(fake.provider_calls, 0);
        assert!(
            fake.recorded_failures.is_empty(),
            "no failure row is recorded"
        );
        assert!(
            fake.staged.is_empty(),
            "no success run or source ledger is staged"
        );
    }

    #[test]
    fn r2_worker_invalid_json_records_failure_before_one_retry_then_stages_once() {
        let mut fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
            provider_outputs: VecDeque::from([
                Ok(output(b"not json".to_vec())),
                Ok(valid_output()),
            ]),
            stage_result: VecDeque::from([Ok(StageExtractionResult::Staged { run_id: 41 })]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let outcome = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
            .expect("second valid response stages");

        assert_eq!(
            outcome,
            WorkerOutcome::Staged {
                run_id: 41,
                candidate_count: 0,
                relation_count: 0,
            }
        );
        assert_eq!(fake.provider_calls, 2, "invalid JSON has exactly one retry");
        assert_eq!(fake.recorded_failures, [ExtractionErrorKind::InvalidJson]);
        assert_eq!(
            fake.recorded_failure_details,
            [(ExtractionErrorKind::InvalidJson, true, 17)]
        );
        assert_eq!(
            fake.events,
            ["provider", "failure-record", "provider", "stage"],
            "the first failure row is durable before the retry"
        );
        assert_eq!(
            fake.staged_sources.len(),
            1,
            "one source ledger set is staged"
        );
        assert_eq!(fake.staged_sources[0].len(), MIN_TURNS as usize);
    }

    #[test]
    fn r2_worker_failure_row_write_failure_prevents_invalid_json_retry() {
        let mut fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
            provider_outputs: VecDeque::from([
                Ok(output(b"not json".to_vec())),
                Ok(valid_output()),
            ]),
            failure_record_result: VecDeque::from([Err(WorkerError::Daemon(
                "record failed".into(),
            ))]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let error = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
            .expect_err("retry requires the first failure row to be durable");

        assert_eq!(error, WorkerError::Audit);
        assert_eq!(fake.provider_calls, 1, "must not invoke the retry");
        assert_eq!(fake.recorded_failures, [ExtractionErrorKind::InvalidJson]);
        assert!(fake.staged.is_empty());
    }

    #[test]
    fn r2_worker_schema_reject_and_each_process_error_run_once_without_fallback_command() {
        let cases = [
            (
                Ok(output(
                    br#"{"items":[{"invalid":true}],"relations":[]}"#.to_vec(),
                )),
                ExtractionErrorKind::SchemaReject,
            ),
            (Ok(output(vec![0xff])), ExtractionErrorKind::InvalidUtf8),
            (Err(ProcessError::Spawn), ExtractionErrorKind::Spawn),
            (Err(ProcessError::Stdin), ExtractionErrorKind::Stdin),
            (Err(ProcessError::Timeout), ExtractionErrorKind::Timeout),
            (
                Err(ProcessError::OutputTooLarge {
                    stream: OutputStream::Stdout,
                }),
                ExtractionErrorKind::StdoutTooLarge,
            ),
            (
                Err(ProcessError::OutputTooLarge {
                    stream: OutputStream::Stderr,
                }),
                ExtractionErrorKind::StderrTooLarge,
            ),
            (
                Err(ProcessError::NonZero {
                    code: Some(7),
                    stderr_bytes: 0,
                }),
                ExtractionErrorKind::NonZero,
            ),
            (Err(ProcessError::Wait), ExtractionErrorKind::SchemaReject),
        ];

        for (provider_output, expected_failure) in cases {
            let mut fake = FakeWorker {
                scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
                provider_outputs: VecDeque::from([provider_output]),
                ..Default::default()
            };
            let (_socket_directory, socket) = test_socket();

            let _ = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
                .expect_err("schema rejection and every non-JSON process error must fail");

            assert_eq!(fake.provider_calls, 1, "no retry or alternate command");
            assert_eq!(
                fake.recorded_failures,
                [expected_failure],
                "one correctly typed failure row per invocation"
            );
            assert!(fake.staged.is_empty());
        }
    }

    #[test]
    fn r2_worker_failure_mapping_is_exhaustive() {
        let cases = [
            (
                WorkerError::Process(ProcessError::Spawn),
                ExtractionErrorKind::Spawn,
            ),
            (
                WorkerError::Process(ProcessError::Stdin),
                ExtractionErrorKind::Stdin,
            ),
            (
                WorkerError::Process(ProcessError::Timeout),
                ExtractionErrorKind::Timeout,
            ),
            (
                WorkerError::Process(ProcessError::OutputTooLarge {
                    stream: OutputStream::Stdout,
                }),
                ExtractionErrorKind::StdoutTooLarge,
            ),
            (
                WorkerError::Process(ProcessError::OutputTooLarge {
                    stream: OutputStream::Stderr,
                }),
                ExtractionErrorKind::StderrTooLarge,
            ),
            (
                WorkerError::Process(ProcessError::NonZero {
                    code: Some(7),
                    stderr_bytes: 0,
                }),
                ExtractionErrorKind::NonZero,
            ),
            (
                WorkerError::Validation(ValidationError::InvalidUtf8),
                ExtractionErrorKind::InvalidUtf8,
            ),
            (
                WorkerError::Validation(ValidationError::InvalidJson),
                ExtractionErrorKind::InvalidJson,
            ),
        ];
        for (error, kind) in cases {
            assert_eq!(failure_kind(&error), kind);
        }
        for validation_error in [
            ValidationError::SchemaReject,
            ValidationError::TooManyItems,
            ValidationError::InvalidItemId,
            ValidationError::DuplicateItemId,
            ValidationError::InvalidContent,
            ValidationError::InvalidConfidence,
            ValidationError::InvalidSourceReference,
            ValidationError::InvalidRelationReference,
            ValidationError::SelfRelation,
            ValidationError::DuplicateCandidateKey,
            ValidationError::DuplicateRelation,
        ] {
            assert_eq!(
                failure_kind(&WorkerError::Validation(validation_error)),
                ExtractionErrorKind::SchemaReject
            );
        }
    }

    #[test]
    fn r2_worker_terminal_and_stage_audit_failures_are_distinct() {
        let mut terminal_fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
            provider_outputs: VecDeque::from([Err(ProcessError::Stdin)]),
            failure_record_result: VecDeque::from([Err(WorkerError::Daemon(
                "record failed".into(),
            ))]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let terminal_error = run_worker_with(
            &config(ExtractMode::Shadow),
            &socket,
            None,
            &mut terminal_fake,
        )
        .expect_err("terminal failure audit must be durable");
        assert_eq!(terminal_error, WorkerError::Audit);
        assert_eq!(
            terminal_fake.recorded_failures,
            [ExtractionErrorKind::Stdin]
        );

        let mut stage_fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
            provider_outputs: VecDeque::from([Ok(valid_output())]),
            stage_result: VecDeque::from([Err(WorkerError::Daemon("stage failed".into()))]),
            failure_record_result: VecDeque::from([Err(WorkerError::Daemon(
                "record failed".into(),
            ))]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let stage_error =
            run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut stage_fake)
                .expect_err("stage failure audit must be durable");
        assert_eq!(stage_error, WorkerError::Audit);
        assert_eq!(
            stage_fake.recorded_failures,
            [ExtractionErrorKind::StageReject]
        );
    }

    #[test]
    fn r2_worker_second_attempt_audit_failure_is_distinct() {
        let mut fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
            provider_outputs: VecDeque::from([
                Ok(output(b"not json".to_vec())),
                Err(ProcessError::Stdin),
            ]),
            failure_record_result: VecDeque::from([
                Ok(()),
                Err(WorkerError::Daemon("record failed".into())),
            ]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let error = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
            .expect_err("second-attempt audit must be durable");
        assert_eq!(error, WorkerError::Audit);
        assert_eq!(
            fake.recorded_failures,
            [ExtractionErrorKind::InvalidJson, ExtractionErrorKind::Stdin]
        );
    }
    #[test]
    fn r2_worker_returns_replay_outcome_without_duplicate_source_ledger() {
        let mut fake = FakeWorker {
            scans: VecDeque::from([Ok(scan(MIN_TURNS as usize))]),
            provider_outputs: VecDeque::from([Ok(valid_output())]),
            stage_result: VecDeque::from([Ok(StageExtractionResult::AlreadyStaged { run_id: 41 })]),
            ..Default::default()
        };
        let (_socket_directory, socket) = test_socket();

        let outcome = run_worker_with(&config(ExtractMode::Shadow), &socket, None, &mut fake)
            .expect("replay is successful");

        assert_eq!(
            outcome,
            WorkerOutcome::AlreadyStaged {
                run_id: 41,
                candidate_count: 0,
                relation_count: 0,
            }
        );
        assert_eq!(fake.provider_calls, 1);
        assert_eq!(
            fake.staged_sources.len(),
            1,
            "worker makes one stage request; daemon replays its source ledger"
        );
    }
}
