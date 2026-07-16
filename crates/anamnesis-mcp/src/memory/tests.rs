use super::*;
use crate::proto::RecallEventKind;
use anamnesis::memory::{ListFilter, NoteOptions, Relation};
use anamnesis::storage::StorageAdapter;

fn registry(reinforce: bool) -> MemoryRegistry {
    MemoryRegistry::in_memory_with(Arc::new(StubProvider), reinforce)
}

#[test]
fn embed_model_from_name_maps_supported_models() {
    assert_eq!(
        format!(
            "{:?}",
            embed_model_from_name("multilingual-e5-small").unwrap()
        ),
        "MultilingualE5Small"
    );
    assert_eq!(
        format!(
            "{:?}",
            embed_model_from_name("multilingual-e5-base").unwrap()
        ),
        "MultilingualE5Base"
    );
    assert_eq!(
        format!(
            "{:?}",
            embed_model_from_name("multilingual-e5-large").unwrap()
        ),
        "MultilingualE5Large"
    );
    assert_eq!(
        format!("{:?}", embed_model_from_name("bge-base-en-v1.5").unwrap()),
        "BGEBaseENV15"
    );

    let err = embed_model_from_name("unknown-model").unwrap_err();
    assert!(
        err.to_string().contains("multilingual-e5-small"),
        "supported model list should be actionable: {err}"
    );
}

struct FixedDimProvider {
    dim: usize,
}

impl EmbeddingProvider for FixedDimProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts.iter().map(|_| vec![0.1; self.dim]).collect())
    }

    fn dimensions(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        "fixed-dim"
    }
}

struct NamedProvider {
    dim: usize,
    name: &'static str,
}

impl EmbeddingProvider for NamedProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts.iter().map(|_| vec![0.1; self.dim]).collect())
    }

    fn dimensions(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        self.name
    }
}

fn file_registry(
    provider: Arc<dyn EmbeddingProvider>,
    db: PathBuf,
    dir: PathBuf,
) -> MemoryRegistry {
    MemoryRegistry::file_backed_with(provider, db, dir, "default".into(), false)
}

fn wait_for_file_registry_lock_release(db: &std::path::Path) {
    let mut lock_path = db.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .expect("open file registry lock probe");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match fs4::FileExt::try_lock(&lock) {
            Ok(()) => break,
            Err(fs4::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => panic!("file registry lock remained held after drop: {error}"),
        }
    }
    fs4::FileExt::unlock(&lock).expect("release file registry lock probe");
}
fn v0_connection() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("open v0 policy database");
    connection
        .execute_batch(
            "
            CREATE TABLE mcp_schema_version (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                version INTEGER NOT NULL
            );
            INSERT INTO mcp_schema_version (id, version) VALUES (1, 0);
            ",
        )
        .expect("create v0 policy schema");
    connection
}

fn registry_with_policy_version(version: i64) -> (MemoryRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create policy registry directory");
    let db = dir.path().join("policy.db");
    let connection = rusqlite::Connection::open(&db).expect("open policy database");
    connection
        .execute_batch(
            "
            CREATE TABLE mcp_schema_version (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                version INTEGER NOT NULL
            );
            ",
        )
        .expect("create policy schema");
    connection
        .execute(
            "INSERT INTO mcp_schema_version (id, version) VALUES (1, ?1)",
            [version],
        )
        .expect("seed policy schema version");
    drop(connection);

    (
        file_registry(Arc::new(StubProvider), db, dir.path().to_path_buf()),
        dir,
    )
}
fn recall_event(
    at_ms: u64,
    event_kind: RecallEventKind,
    top: Option<(f64, f64)>,
    gate_threshold: f64,
    cosine_gate: f64,
    result_node_ids: Vec<u64>,
    auto_extract_node_count: u64,
) -> RecallEvent {
    let (top_score, top_cosine) = top
        .map(|(score, cosine)| (Some(score), Some(cosine)))
        .unwrap_or((None, None));
    let has_hits = top.is_some();
    let readout_pass = top_score.is_some_and(|score| score >= gate_threshold);
    let cosine_pass = top_cosine.is_some_and(|cosine| cosine >= cosine_gate);
    RecallEvent {
        at_ms,
        namespace: "default".into(),
        event_kind,
        query_chars: 8,
        scope: Some("project/anamnesis".into()),
        knowledge_only: true,
        has_hits,
        readout_pass,
        cosine_pass,
        eligible: has_hits && readout_pass && cosine_pass,
        top_score,
        top_cosine,
        gate_threshold: Some(gate_threshold),
        cosine_gate: Some(cosine_gate),
        result_node_ids,
        auto_extract_node_count,
    }
}
fn event_kind_stats(stats: &RecallStats, event_kind: RecallEventKind) -> &EventKindStats {
    stats
        .by_event_kind
        .iter()
        .find(|stats| stats.event_kind == event_kind)
        .expect("event kind must have recall telemetry")
}

#[test]
fn recall_stats_aggregates_gate_buckets() {
    let mut store = PolicyStore::in_memory().expect("open policy store");
    let events = [
        // Empty attempts must not enter a gate-abstention bucket.
        recall_event(
            1,
            RecallEventKind::UserPrompt,
            None,
            0.8,
            0.8,
            Vec::new(),
            0,
        ),
        // Readout-only abstention: score fails while cosine passes.
        recall_event(
            2,
            RecallEventKind::SessionStart,
            Some((0.70, 0.80)),
            0.8,
            0.8,
            Vec::new(),
            0,
        ),
        // Cosine-only abstention: score passes while cosine fails its observed gate.
        recall_event(
            3,
            RecallEventKind::Tool,
            Some((0.90, 0.82)),
            0.8,
            0.9,
            Vec::new(),
            0,
        ),
        // Both gates fail.
        recall_event(
            4,
            RecallEventKind::Unknown,
            Some((0.70, 0.84)),
            0.8,
            0.9,
            Vec::new(),
            0,
        ),
        recall_event(
            5,
            RecallEventKind::UserPrompt,
            Some((0.90, 0.88)),
            0.8,
            0.8,
            vec![1, 2, 3],
            2,
        ),
        recall_event(
            6,
            RecallEventKind::Tool,
            Some((0.95, 0.90)),
            0.8,
            0.8,
            vec![4, 5],
            0,
        ),
    ];
    for event in &events {
        store
            .insert_recall_event(event)
            .expect("seed recall telemetry event");
    }

    let stats: RecallStats = store.recall_stats().expect("aggregate recall telemetry");

    assert_eq!(stats.total_attempts, 6);
    assert_eq!(stats.by_event_kind.len(), 4);
    let abstentions: &AbstentionStats = &stats.abstentions;
    let cosine: &CosineStats = &stats.cosine;
    let auto_exposure: &AutoExposureStats = &stats.auto_exposure;
    let user_prompt = event_kind_stats(&stats, RecallEventKind::UserPrompt);
    assert_eq!(user_prompt.attempts, 2);
    assert_eq!(user_prompt.eligible, 1);
    let session_start = event_kind_stats(&stats, RecallEventKind::SessionStart);
    assert_eq!(session_start.attempts, 1);
    assert_eq!(session_start.eligible, 0);
    let tool = event_kind_stats(&stats, RecallEventKind::Tool);
    assert_eq!(tool.attempts, 2);
    assert_eq!(tool.eligible, 1);
    let unknown = event_kind_stats(&stats, RecallEventKind::Unknown);
    assert_eq!(unknown.attempts, 1);
    assert_eq!(unknown.eligible, 0);

    assert_eq!(abstentions.empty, 1);
    assert_eq!(abstentions.readout_only, 1);
    assert_eq!(abstentions.cosine_only, 1);
    assert_eq!(abstentions.both, 1);

    assert_eq!(cosine.samples, 5);
    assert_eq!(cosine.nulls, 1);
    assert_eq!(cosine.p50, Some(0.84));
    assert_eq!(cosine.p90, Some(0.90));
    assert_eq!(cosine.p95, Some(0.90));

    assert_eq!(auto_exposure.eligible_events, 2);
    assert_eq!(auto_exposure.events_with_auto, 1);
    assert_eq!(auto_exposure.result_slots, 5);
    assert_eq!(auto_exposure.auto_slots, 2);

    assert_eq!(stats.sweep.len(), 11);
    let first_sweep_point: &SweepPoint = &stats.sweep[0];
    assert_eq!(first_sweep_point.threshold, 0.80);
    for (point, (hundredths, eligible)) in stats
        .sweep
        .iter()
        .zip((80_u8..=90).zip([3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1]))
    {
        assert_eq!(point.threshold, f64::from(hundredths) / 100.0);
        assert_eq!(point.eligible, eligible);
        assert_eq!(point.attempts, 6);
    }
}

#[test]
fn recall_gate_trace_follows_production_gate_decisions() {
    let mut empty = registry(false);
    let empty_outcome = empty
        .recall_packaged_gated(
            "nothing here",
            5,
            None,
            Some(false),
            Some(f64::MAX),
            Some(f64::MAX),
        )
        .expect("empty recall outcome");
    assert_eq!(
        (
            empty_outcome.trace.has_hits,
            empty_outcome.trace.readout_pass,
            empty_outcome.trace.cosine_pass,
            empty_outcome.trace.eligible,
        ),
        (false, false, false, false)
    );

    let mut reg = registry(false);
    reg.remember("the auth bug was a race in the middleware", None)
        .expect("seed recallable memory");

    for (name, gate, cosine_gate, expected) in [
        ("readout only", Some(f64::MAX), None, (false, true, false)),
        ("cosine only", None, Some(f64::MAX), (true, false, false)),
        (
            "both",
            Some(f64::MAX),
            Some(f64::MAX),
            (false, false, false),
        ),
        ("eligible", None, None, (true, true, true)),
    ] {
        let outcome = reg
            .recall_packaged_gated(
                "auth race condition",
                5,
                None,
                Some(false),
                gate,
                cosine_gate,
            )
            .expect("recall outcome");
        let trace = outcome.trace;
        assert_eq!(
            (trace.readout_pass, trace.cosine_pass, trace.eligible),
            expected,
            "{name}"
        );
        assert_eq!(
            trace.eligible,
            trace.has_hits && trace.readout_pass && trace.cosine_pass,
            "{name}"
        );
        assert_eq!(outcome.packaged.hits.is_empty(), !trace.eligible, "{name}");
    }
}

#[test]
fn recall_stats_empty_dataset_has_no_nan() {
    let store = PolicyStore::in_memory().expect("open policy store");

    let stats: RecallStats = store
        .recall_stats()
        .expect("aggregate empty recall telemetry");

    assert_eq!(stats.total_attempts, 0);
    assert!(stats.by_event_kind.is_empty());
    assert_eq!(stats.abstentions.empty, 0);
    assert_eq!(stats.abstentions.readout_only, 0);
    assert_eq!(stats.abstentions.cosine_only, 0);
    assert_eq!(stats.abstentions.both, 0);
    assert_eq!(stats.cosine.samples, 0);
    assert_eq!(stats.cosine.nulls, 0);
    assert_eq!(stats.cosine.p50, None);
    assert_eq!(stats.cosine.p90, None);
    assert_eq!(stats.cosine.p95, None);
    assert_eq!(stats.auto_exposure.eligible_events, 0);
    assert_eq!(stats.auto_exposure.events_with_auto, 0);
    assert_eq!(stats.auto_exposure.result_slots, 0);
    assert_eq!(stats.auto_exposure.auto_slots, 0);

    assert_eq!(stats.sweep.len(), 11);
    for (point, hundredths) in stats.sweep.iter().zip(80_u8..=90) {
        assert_eq!(point.threshold, f64::from(hundredths) / 100.0);
        assert!(
            point.threshold.is_finite(),
            "threshold must not be NaN or infinite"
        );
        assert_eq!(point.eligible, 0);
        assert_eq!(point.attempts, 0);
    }
}

#[test]
fn policy_schema_fresh_and_v0_migration_converge() {
    let fresh = PolicyStore::in_memory().expect("fresh policy store");
    let migrated = PolicyStore::from_test_connection(v0_connection()).expect("migrated store");

    assert_eq!(
        fresh.schema_fingerprint().expect("fresh schema"),
        migrated.schema_fingerprint().expect("migrated schema")
    );
    assert_eq!(fresh.schema_version().expect("fresh version"), 1);
    assert_eq!(migrated.schema_version().expect("migrated version"), 1);
}
#[test]
fn ready_policy_store_persists_minimized_event_and_rejects_out_of_range_values() {
    let mut reg = registry(false);
    let handles = reg
        .namespace_handles(None)
        .expect("resolve in-memory namespace handles");
    let _memory = handles
        .memory
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut policy = MemoryRegistry::policy_store(&handles.policy).expect("open policy store");
    let PolicyStoreState::Ready(store) = &mut *policy else {
        panic!("in-memory policy store should be ready");
    };
    let event = RecallEvent {
        at_ms: 1,
        namespace: "default".into(),
        event_kind: RecallEventKind::Tool,
        query_chars: 11,
        scope: Some("project".into()),
        knowledge_only: true,
        has_hits: true,
        readout_pass: true,
        cosine_pass: true,
        eligible: true,
        top_score: Some(0.9),
        top_cosine: Some(0.8),
        gate_threshold: Some(0.7),
        cosine_gate: Some(0.6),
        result_node_ids: vec![1, 2],
        auto_extract_node_count: 1,
    };

    store
        .insert_recall_event(&event)
        .expect("insert recall event");

    let error = store
        .insert_recall_event(&RecallEvent {
            at_ms: u64::MAX,
            ..event
        })
        .expect_err("out-of-range timestamp should be rejected");
    assert!(
        matches!(
            &error,
            Error::InvalidInput(message)
                if message == "invalid policy store value: recall event timestamp"
        ),
        "expected invalid policy value error, got {error:?}"
    );
}
#[test]
fn phase_one_resolution_leaves_policy_uninitialized() {
    let (mut reg, _dir) = registry_with_policy_version(1);

    let handles = reg
        .namespace_handles(None)
        .expect("resolve namespace handles");

    assert!(matches!(
        &*handles
            .policy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        PolicyStoreState::Uninitialized { path: Some(_) }
    ));
}

#[test]
fn repeated_phase_one_resolution_reuses_namespace_handles() {
    let (mut reg, _dir) = registry_with_policy_version(1);

    let first = reg.namespace_handles(None).expect("first resolution");
    let second = reg
        .namespace_handles(Some("default"))
        .expect("second resolution");

    assert_eq!(first.key, second.key);
    assert!(Arc::ptr_eq(&first.memory, &second.memory));
    assert!(Arc::ptr_eq(&first.policy, &second.policy));
}

#[test]
fn future_policy_schema_disables_only_policy_features() {
    let (mut reg, _dir) = registry_with_policy_version(2);

    reg.remember("core recall remains available", None)
        .expect("remember");
    let recalled = reg.recall_packaged("core recall", 5, None).expect("recall");
    assert!(!recalled.hits.is_empty());
    let handles = reg.namespace_handles(None).expect("resolve policy handle");
    let memory = handles
        .memory
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(MemoryRegistry::policy_store(&handles.policy).is_err());
    drop(memory);

    assert!(matches!(
        &*handles
            .policy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        PolicyStoreState::Disabled { .. }
    ));
}

#[test]
fn namespace_open_records_and_accepts_same_embedding_model() {
    // Given: a graph created with one named embedding provider.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model.db");
    let provider = || {
        Arc::new(NamedProvider {
            dim: 3,
            name: "model-a",
        }) as Arc<dyn EmbeddingProvider>
    };
    let mut first = file_registry(provider(), db.clone(), dir.path().to_path_buf());
    first.remember("durable model stamp", None).unwrap();
    drop(first);
    wait_for_file_registry_lock_release(&db);

    // When: the namespace is reopened with the same model.
    let mut reopened = file_registry(provider(), db, dir.path().to_path_buf());

    // Then: model comparison succeeds.
    reopened.stats(None).unwrap();
}

#[test]
fn namespace_open_pins_bge_baseline_and_preserves_graph_on_e5_rejection() {
    // Given: a populated file-backed graph stamped by bge-base-en-v1.5 at 768 dimensions.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model.db");
    let bge_provider = || {
        Arc::new(NamedProvider {
            dim: 768,
            name: "bge-base-en-v1.5",
        }) as Arc<dyn EmbeddingProvider>
    };
    let mut first = file_registry(bge_provider(), db.clone(), dir.path().to_path_buf());
    let first_node = first.remember("baseline source", None).unwrap();
    let second_node = first.remember("baseline target", None).unwrap();
    first
        .relate(first_node, second_node, "related", None)
        .unwrap();
    let created_stats = first.stats(None).unwrap();
    assert!(created_stats.node_count > 0);
    assert!(created_stats.edge_count > 0);
    drop(first);
    wait_for_file_registry_lock_release(&db);

    let mut matching = file_registry(bge_provider(), db.clone(), dir.path().to_path_buf());
    let matching_stats = matching.stats(None).unwrap();
    assert_eq!(matching_stats.node_count, created_stats.node_count);
    assert_eq!(matching_stats.edge_count, created_stats.edge_count);
    drop(matching);
    wait_for_file_registry_lock_release(&db);

    let storage_before = SqliteStorage::open(&db).unwrap();
    let counts_before = (storage_before.node_count(), storage_before.edge_count());
    assert_eq!(
        counts_before,
        (created_stats.node_count, created_stats.edge_count)
    );
    assert_eq!(
        storage_before.embedding_model_name().unwrap().as_deref(),
        Some("bge-base-en-v1.5")
    );
    drop(storage_before);

    // When: the populated namespace is opened with multilingual-e5-small at 384 dimensions.
    let mut mismatched = file_registry(
        Arc::new(NamedProvider {
            dim: 384,
            name: "multilingual-e5-small",
        }),
        db.clone(),
        dir.path().to_path_buf(),
    );
    let error = mismatched.stats(None).unwrap_err();
    drop(mismatched);
    wait_for_file_registry_lock_release(&db);

    // Then: the incompatible provider is rejected without changing the persisted graph.
    let message = error.to_string();
    assert!(message.contains("DB has 768-d embeddings"), "{message}");
    assert!(message.contains("multilingual-e5-small"), "{message}");
    let storage_after = SqliteStorage::open(&db).unwrap();
    assert_eq!(
        (storage_after.node_count(), storage_after.edge_count()),
        counts_before
    );
}

#[test]
fn namespace_open_rejects_different_same_dimension_model_actionably() {
    // Given: a graph stamped by model-a with 3-dimensional embeddings.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model.db");
    let mut first = file_registry(
        Arc::new(NamedProvider {
            dim: 3,
            name: "model-a",
        }),
        db.clone(),
        dir.path().to_path_buf(),
    );
    first
        .remember("same dimension is not same space", None)
        .unwrap();
    drop(first);
    wait_for_file_registry_lock_release(&db);

    // When: the namespace is reopened with model-b at the same dimension.
    let mut reopened = file_registry(
        Arc::new(NamedProvider {
            dim: 3,
            name: "model-b",
        }),
        db,
        dir.path().to_path_buf(),
    );
    let err = reopened.stats(None).unwrap_err();

    // Then: the error names both models and the recovery control.
    let message = err.to_string();
    assert!(message.contains("model-a"), "{message}");
    assert!(message.contains("model-b"), "{message}");
    assert!(message.contains("ANAMNESIS_EMBED_MODEL"), "{message}");
}

#[test]
fn dim_mismatch_error_names_migrate_command_and_both_spaces() {
    let mismatch = NamespaceCompatibility::DimensionMismatch {
        stored_model: Some("bge-base-en-v1.5".to_string()),
        db_dimensions: vec![Some(768)],
        target_model: "multilingual-e5-small".to_string(),
        target_dimensions: 384,
    };

    let message = mismatch.message();

    assert!(message.contains("768-d"), "{message}");
    assert!(message.contains("bge-base-en-v1.5"), "{message}");
    assert!(message.contains("multilingual-e5-small"), "{message}");
    assert!(message.contains("384-d"), "{message}");
    assert!(
        message.contains("anamnesis migrate-embeddings [--namespace NS]"),
        "{message}"
    );
}

#[test]
fn model_mismatch_error_preserves_stored_model_fallback() {
    let mismatch = NamespaceCompatibility::ModelMismatch {
        stored_model: "model-a".to_string(),
        target_model: "model-b".to_string(),
        target_dimensions: 384,
    };

    let message = mismatch.message();

    assert!(
        message.contains("anamnesis migrate-embeddings [--namespace NS]"),
        "{message}"
    );
    assert!(
        message.contains("ANAMNESIS_EMBED_MODEL=model-a"),
        "{message}"
    );
}

#[test]
fn namespace_open_backfills_unstamped_legacy_db_after_dimension_match() {
    // Given: an old graph with 3-dimensional embeddings but no model stamp.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("legacy.db");
    let legacy_provider: Arc<dyn EmbeddingProvider> = Arc::new(NamedProvider {
        dim: 3,
        name: "legacy-writer",
    });
    let mut legacy = Memory::with_provider(&db, legacy_provider).unwrap();
    legacy
        .add("s", "user", "legacy unstamped node", Timestamp(1))
        .unwrap();
    legacy.flush_all().unwrap();
    drop(legacy);

    // When: the namespace first opens with a dimension-compatible model.
    let mut registry = file_registry(
        Arc::new(NamedProvider {
            dim: 3,
            name: "model-a",
        }),
        db.clone(),
        dir.path().to_path_buf(),
    );
    registry.stats(None).unwrap();
    drop(registry);

    // Then: the compatible model is durably backfilled.
    let storage = SqliteStorage::open(&db).unwrap();
    assert_eq!(
        storage.embedding_model_name().unwrap(),
        Some("model-a".to_string())
    );
}

#[test]
fn namespace_open_preserves_dimension_mismatch_and_leaves_model_unstamped() {
    // Given: an unstamped legacy graph containing 3-dimensional embeddings.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("legacy.db");
    let legacy_provider: Arc<dyn EmbeddingProvider> = Arc::new(NamedProvider {
        dim: 3,
        name: "legacy-writer",
    });
    let mut legacy = Memory::with_provider(&db, legacy_provider).unwrap();
    legacy
        .add("s", "user", "legacy mismatched node", Timestamp(1))
        .unwrap();
    legacy.flush_all().unwrap();
    drop(legacy);

    // When: the namespace opens with a provider of a different dimension.
    let mut registry = file_registry(
        Arc::new(NamedProvider {
            dim: 4,
            name: "model-b",
        }),
        db.clone(),
        dir.path().to_path_buf(),
    );
    let err = registry.stats(None).unwrap_err();
    drop(registry);

    // Then: the existing dimension error is preserved and no model is stamped.
    assert!(err.to_string().contains("DB has 3-d embeddings"), "{err}");
    let storage = SqliteStorage::open(&db).unwrap();
    assert_eq!(storage.embedding_model_name().unwrap(), None);
}

#[test]
fn namespace_open_allows_empty_db() {
    // Given: an empty database and a named provider.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("empty.db");
    let mut registry = file_registry(
        Arc::new(NamedProvider {
            dim: 3,
            name: "model-a",
        }),
        db,
        dir.path().to_path_buf(),
    );

    // When: the empty namespace is opened.
    let stats = registry.stats(None).unwrap();

    // Then: it passes validation without requiring an existing embedding.
    assert_eq!(stats.node_count, 0);
}

struct ScopeGateProvider;

impl EmbeddingProvider for ScopeGateProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("scope gate query") || text.contains("scope gate high") {
                    vec![1.0, 0.0]
                } else if text.contains("scope gate local") {
                    vec![0.2, 0.98]
                } else {
                    vec![0.5, 0.5]
                }
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "scope-gate"
    }
}
struct ScopedTraceProvider;

impl EmbeddingProvider for ScopedTraceProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("deployment") && !text.contains("local") {
                    vec![1.0, 0.0]
                } else if text.contains("local deployment") {
                    vec![0.6, 0.8]
                } else {
                    vec![0.5, 0.5]
                }
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "scoped-trace"
    }
}

fn scoped_test_memory() -> Memory<SqliteStorage> {
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(ScopedTraceProvider);
    let mut mem = Memory::in_memory_with_provider(provider).expect("create scoped test memory");

    let remote = NoteOptions {
        scope: Some(anamnesis::graph::ScopePath::new("project/b").expect("remote scope")),
        ..NoteOptions::default()
    };
    mem.add_note_with(
        "remote deployment note excluded by scope",
        anamnesis::graph::Timestamp(1),
        remote,
    )
    .expect("add remote note");

    let local = NoteOptions {
        scope: Some(anamnesis::graph::ScopePath::new("project/a").expect("local scope")),
        ..NoteOptions::default()
    };
    mem.add_note_with(
        "local deployment note retained after filtering",
        anamnesis::graph::Timestamp(2),
        local,
    )
    .expect("add local note");

    mem
}

#[test]
fn verify_embedding_dim_allows_empty_and_matching_but_rejects_mismatch() {
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(FixedDimProvider { dim: 384 });
    let mut mem = Memory::in_memory_with_provider(provider).unwrap();

    verify_embedding_dim(&mem, 768, "bge-base-en-v1.5").unwrap();
    mem.add(
        "s",
        "user",
        "dimensioned memory",
        anamnesis::graph::Timestamp(1),
    )
    .unwrap();

    verify_embedding_dim(&mem, 384, "multilingual-e5-small").unwrap();
    let err = verify_embedding_dim(&mem, 768, "bge-base-en-v1.5").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("DB has 384-d embeddings"), "{msg}");
    assert!(msg.contains("bge-base-en-v1.5"), "{msg}");
    assert!(msg.contains("ANAMNESIS_EMBED_MODEL"), "{msg}");
}

// ── Single-tick-per-recall (flagship bug #2) ────────────────────────────

#[test]
fn recall_ticks_engine_exactly_once() {
    let mut reg = registry(true);
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let before = TICK_CALLS.with(|c| c.get());
    reg.recall("auth race condition", 5, None).unwrap();
    assert_eq!(
        TICK_CALLS.with(|c| c.get()) - before,
        1,
        "recall must tick the engine exactly once, not twice"
    );
}

#[test]
fn recall_packaged_gated_ticks_engine_exactly_once() {
    let mut reg = registry(true);
    reg.remember("the cache key omitted the lockfile hash", None)
        .unwrap();
    let before = TICK_CALLS.with(|c| c.get());
    reg.recall_packaged_gated("cache key lockfile", 5, None, None, None, None)
        .unwrap();
    assert_eq!(
        TICK_CALLS.with(|c| c.get()) - before,
        1,
        "recall_packaged_gated must tick the engine exactly once, not twice"
    );
}

#[test]
fn recall_packaged_gated_gated_out_still_ticks_exactly_once() {
    let mut reg = registry(true);
    reg.remember("unrelated note", None).unwrap();
    let before = TICK_CALLS.with(|c| c.get());
    // An impossibly high gate threshold forces the gated-out early-return
    // branch, which has its own tick call site.
    reg.recall_packaged_gated("unrelated note", 5, None, None, Some(1_000.0), None)
        .unwrap();
    assert_eq!(
        TICK_CALLS.with(|c| c.get()) - before,
        1,
        "the gated-out branch must also tick the engine exactly once"
    );
}

#[test]
fn second_registry_on_same_db_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("m.db");
    let mut a = MemoryRegistry::file_backed_with(
        Arc::new(StubProvider),
        db.clone(),
        dir.path().to_path_buf(),
        "default".into(),
        false,
    );
    a.remember("first writer holds the lock", None).unwrap();

    let mut b = MemoryRegistry::file_backed_with(
        Arc::new(StubProvider),
        db,
        dir.path().to_path_buf(),
        "default".into(),
        false,
    );
    let err = b.remember("second writer must be rejected", None);
    assert!(
        err.is_err(),
        "a second registry on the same DB file must be rejected by the lock"
    );
}

#[test]
fn remember_then_recall_returns_a_hit() {
    let mut reg = registry(true);
    // `remember` returns the episodic node id; `.unwrap()` already proves it
    // stored without error (any u64 is a valid id).
    let _id = reg
        .remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let hits = reg.recall("auth race condition", 5, None).unwrap();
    assert!(!hits.is_empty(), "expected at least one hit after remember");
}

#[test]
fn recall_collapses_duplicate_text_nodes() {
    let mut reg = registry(false);
    // One note = an Episodic + a Semantic node with identical text. recall must
    // return that text once, not twice.
    reg.remember("the cache key omitted the lockfile hash", None)
        .unwrap();
    let hits = reg.recall("cache key lockfile", 5, None).unwrap();
    let mut texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
    texts.sort_unstable();
    let unique = {
        let mut t = texts.clone();
        t.dedup();
        t.len()
    };
    assert_eq!(
        texts.len(),
        unique,
        "recall returned duplicate-text hits: {texts:?}"
    );
}

#[test]
fn ingest_conversation_counts_turns() {
    let mut reg = registry(true);
    let turns = vec![
        Turn {
            speaker: "alice".into(),
            text: "we picked postgres".into(),
            at_ms: None,
        },
        Turn {
            speaker: "bob".into(),
            text: "because of jsonb".into(),
            at_ms: None,
        },
        Turn {
            speaker: "alice".into(),
            text: "and row-level security".into(),
            at_ms: None,
        },
    ];
    let summary = reg
        .ingest_conversation("design-chat", &turns, None, false)
        .unwrap();
    assert_eq!(summary.episodic, 3);
    assert!(summary.semantic >= 1);
}

#[test]
fn namespaces_are_isolated() {
    let mut reg = registry(true);
    reg.remember("alpha-only secret", Some("alpha")).unwrap();
    let beta_hits = reg.recall("alpha-only secret", 5, Some("beta")).unwrap();
    assert!(
        beta_hits.is_empty(),
        "namespace beta must not see alpha's memory"
    );
}

#[test]
fn sanitize_blocks_path_traversal() {
    assert_eq!(
        MemoryRegistry::sanitize("../../etc/passwd"),
        "------etc-passwd"
    );
    assert_eq!(MemoryRegistry::sanitize(""), "default");
    assert_eq!(MemoryRegistry::sanitize("Work Project:1"), "work-project-1");
}

/// A non-default namespace whose sanitized stem equals the default DB file
/// stem must be rejected, not silently aliased onto the default file.
#[test]
fn namespace_colliding_with_default_db_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let default_db = dir.path().join("memory.db");
    let mut reg = MemoryRegistry::file_backed(
        default_db,
        dir.path().to_path_buf(),
        "default".to_string(),
        false,
    );
    reg.provider = Some(Arc::new(StubProvider));
    // ns "memory" sanitizes to "memory" → <dir>/memory.db == default_db.
    let err = reg.remember("leak attempt", Some("memory")).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
}

/// Raw namespaces that sanitize to the same stem must collapse to ONE
/// instance over ONE file, not two instances racing over the same file.
#[test]
fn sanitize_equal_namespaces_share_one_instance() {
    let dir = tempfile::tempdir().unwrap();
    let default_db = dir.path().join("memory.db");
    let mut reg = MemoryRegistry::file_backed(
        default_db,
        dir.path().to_path_buf(),
        "default".to_string(),
        false,
    );
    reg.provider = Some(Arc::new(StubProvider));
    reg.remember("shared via Alpha", Some("Alpha")).unwrap();
    // "alpha" sanitizes to the same stem as "Alpha"; it must see the write.
    let hits = reg.recall("shared via Alpha", 5, Some("alpha")).unwrap();
    assert!(
        !hits.is_empty(),
        "alpha must see Alpha's write (same canonical namespace)"
    );
    // Exactly one open instance for both raw spellings.
    assert_eq!(reg.open.len(), 1, "Alpha and alpha must share one instance");
}

// ── parse_relation ────────────────────────────────────────────────────────

#[test]
fn parse_relation_canonical_and_aliases() {
    use anamnesis::memory::Relation;
    assert_eq!(parse_relation("causes").unwrap(), Relation::Causes);
    assert_eq!(parse_relation("CAUSAL").unwrap(), Relation::Causes);
    assert_eq!(
        parse_relation("contradicts").unwrap(),
        Relation::Contradicts
    );
    assert_eq!(parse_relation("supports").unwrap(), Relation::Supports);
    assert_eq!(parse_relation("refutes").unwrap(), Relation::Refutes);
    assert_eq!(parse_relation("reason").unwrap(), Relation::Reason);
    assert_eq!(
        parse_relation("rejected-alternative").unwrap(),
        Relation::RejectedAlternative
    );
    // space/underscore are normalized to `-`.
    assert_eq!(
        parse_relation("Rejected Alternative").unwrap(),
        Relation::RejectedAlternative
    );
    assert_eq!(parse_relation("belongs_to").unwrap(), Relation::BelongsTo);
    assert_eq!(parse_relation("related").unwrap(), Relation::Related);
    assert_eq!(parse_relation("semantic").unwrap(), Relation::Related);
}

#[test]
fn parse_relation_accepts_supersedes() {
    assert_eq!(parse_relation("supersedes").unwrap(), Relation::Supersedes);
    assert_eq!(parse_relation("supersede").unwrap(), Relation::Supersedes);
    assert_eq!(parse_relation("SUPERSEDES").unwrap(), Relation::Supersedes);
}

#[test]
fn parse_relation_custom_preserves_label() {
    use anamnesis::memory::Relation;
    // Custom label keeps its original case after the `custom:` prefix.
    assert_eq!(
        parse_relation("custom:Blocks").unwrap(),
        Relation::Custom("Blocks".to_string())
    );
    assert_eq!(
        parse_relation("  custom:depends-on ").unwrap(),
        Relation::Custom("depends-on".to_string())
    );
}

#[test]
fn parse_relation_rejects_unknown_and_empty_custom() {
    let err = parse_relation("frobnicate").unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    let err = parse_relation("custom:").unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
}

// ── management API (registry-level convenience wrappers) ───────────────────

#[test]
fn registry_update_edits_content() {
    let mut reg = registry(false);
    let id = reg.remember("the deploy script is bash", None).unwrap();
    reg.update(id, "the deploy script is python", None).unwrap();
    let view = reg.get(id, None).unwrap();
    assert_eq!(view.content, "the deploy script is python");
}

#[test]
fn registry_forget_soft_then_hard() {
    let mut reg = registry(false);
    let id = reg.remember("a stale credential note", None).unwrap();
    reg.forget(id, "rotated", false, None).unwrap();
    let view = reg.get(id, None).unwrap();
    assert!(view.retracted, "soft-forgotten node must show retracted");

    reg.forget(id, "", true, None).unwrap();
    assert!(
        reg.get(id, None).is_err(),
        "hard-forgotten node must no longer be readable"
    );
}

#[test]
fn registry_supersede_sets_validity_window() {
    let mut reg = registry(false);
    let old = reg.remember("we use postgres", None).unwrap();
    let new = reg.remember("we use sqlite", None).unwrap();
    reg.supersede(new, old, None).unwrap();
    let old_view = reg.get(old, None).unwrap();
    assert!(old_view.valid_until.is_some());
}

#[test]
fn registry_list_orders_by_salience_and_filters() {
    let mut reg = registry(false);
    reg.remember("a note about apples", None).unwrap();
    let filter = ListFilter {
        min_salience: 0.0,
        limit: 10,
        node_type: None,
        tag: None,
        scope: None,
        metadata: None,
    };
    let views = reg.list(&filter, None).unwrap();
    assert!(!views.is_empty());
    for w in views.windows(2) {
        assert!(w[0].salience >= w[1].salience);
    }
}

// ── relate ────────────────────────────────────────────────────────────────

#[test]
fn relate_links_two_remembered_nodes() {
    let mut reg = registry(false);
    let a = reg.remember("the deploy failed", None).unwrap();
    let b = reg.remember("the disk was full", None).unwrap();
    // b causes a. Returns a valid (non-panicking) edge id.
    let _edge = reg.relate(b, a, "causes", None).unwrap();
    // A contradiction edge must show up in stats.
    let _edge2 = reg.relate(a, b, "contradicts", None).unwrap();
    let stats = reg.stats(None).unwrap();
    assert!(
        stats.contradiction_count >= 1,
        "expected a contradiction edge, got {}",
        stats.contradiction_count
    );
}

#[test]
fn relate_unknown_relation_errors() {
    let mut reg = registry(false);
    let a = reg.remember("x", None).unwrap();
    let b = reg.remember("y", None).unwrap();
    let err = reg.relate(a, b, "not-a-relation", None).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
}

#[test]
fn relate_missing_endpoint_errors() {
    let mut reg = registry(false);
    let a = reg.remember("only node", None).unwrap();
    // u64::MAX is not a real node id.
    let result = reg.relate(a, u64::MAX, "related", None);
    assert!(
        result.is_err(),
        "linking to a missing node must error: {result:?}"
    );
}

// ── recall_packaged ───────────────────────────────────────────────────────

#[test]
fn recall_packaged_returns_context_and_dedup_hits() {
    let mut reg = registry(true);
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let packaged = reg.recall_packaged("auth race condition", 5, None).unwrap();
    // hits are de-duplicated (the Episodic+Semantic copies collapse).
    let mut texts: Vec<&str> = packaged.hits.iter().map(|h| h.text.as_str()).collect();
    texts.sort_unstable();
    let mut unique = texts.clone();
    unique.dedup();
    assert_eq!(
        texts.len(),
        unique.len(),
        "packaged hits had duplicate text"
    );
    // Context is a string (may be empty if nothing packaged, but with a hit it
    // should carry a section header).
    if !packaged.hits.is_empty() {
        assert!(
            packaged.context.contains("##"),
            "expected a section header in context:\n{}",
            packaged.context
        );
    }
}

// ── recall_packaged_gated (the hook recall path) ───────────────────────────

/// A gate `τ` above the top hit's score ⇒ empty context AND empty hits
/// (the hook injects nothing).
#[test]
fn gated_recall_below_threshold_is_empty() {
    let mut reg = registry(false);
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    // First read the true top score with no gate, then set τ just above it.
    let ungated = reg
        .recall_packaged_gated("auth race condition", 5, None, Some(false), None, None)
        .unwrap();
    let top = ungated
        .packaged
        .hits
        .first()
        .map(|h| h.score)
        .expect("a relevant hit exists");
    let tau = top + 1.0; // strictly above the best score ⇒ gate trips.

    let gated = reg
        .recall_packaged_gated("auth race condition", 5, None, Some(false), Some(tau), None)
        .unwrap();
    assert!(
        gated.packaged.context.is_empty(),
        "above-τ gate must yield empty context, got:\n{}",
        gated.packaged.context
    );
    assert!(
        gated.packaged.hits.is_empty(),
        "above-τ gate must yield no hits, got {} hits",
        gated.packaged.hits.len()
    );
}

/// No hits at all ⇒ gated out (treated as below any threshold).
#[test]
fn gated_recall_with_no_hits_is_empty() {
    let mut reg = registry(false);
    // Empty graph: nothing to retrieve, so any gate (even 0.0) yields empty.
    let gated = reg
        .recall_packaged_gated("nothing here", 5, None, Some(false), Some(0.0), None)
        .unwrap();
    assert!(gated.packaged.context.is_empty());
    assert!(gated.packaged.hits.is_empty());
}

/// A gate `τ` at/below the top score ⇒ the rendered top-k context block.
#[test]
fn gated_recall_at_or_above_threshold_renders_top_k() {
    let mut reg = registry(false);
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    // τ = 0.0 admits every positive-scored hit.
    let gated = reg
        .recall_packaged_gated("auth race condition", 5, None, Some(false), Some(0.0), None)
        .unwrap();
    assert!(
        !gated.packaged.hits.is_empty(),
        "τ=0.0 must admit the relevant hit"
    );
    assert!(
        gated.packaged.context.contains("##"),
        "expected a rendered section header, got:\n{}",
        gated.packaged.context
    );
}

#[test]
fn cosine_gate_trips_when_top_cosine_below_threshold() {
    let mut reg = registry(false);
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();

    let gated = reg
        .recall_packaged_gated(
            "auth race condition",
            5,
            None,
            Some(false),
            None,
            Some(1.01),
        )
        .unwrap();

    assert!(
        gated.packaged.context.is_empty(),
        "above-cosine gate must yield empty context, got:\n{}",
        gated.packaged.context
    );
    assert!(
        gated.packaged.hits.is_empty(),
        "above-cosine gate must yield no hits, got {} hits",
        gated.packaged.hits.len()
    );
}

#[test]
fn knowledge_only_drops_memories_tensions_and_capture_fragments() {
    let mut reg = registry(false);
    let handle = reg.namespace_handle(None).unwrap();
    let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());

    mem_remember(&mut mem, "distilled: recall gate is cosine-based").unwrap();
    let mut opts = NoteOptions::default();
    opts.metadata
        .push(("capture".to_string(), "true".to_string()));
    mem_remember_with(
        &mut mem,
        "captured conversation window about recall gate",
        opts,
    )
    .unwrap();

    let out = mem_recall_packaged_gated_filtered(
        &mut mem,
        "recall gate",
        10,
        false,
        RecallFilters {
            gate: None,
            cosine_gate: None,
            scope: None,
            tag: None,
            knowledge_only: true,
        },
    )
    .unwrap();

    assert!(out.packaged.context.contains("## KNOWLEDGE"));
    assert!(
        !out.packaged.context.contains("## MEMORIES"),
        "episodic section must be dropped:\n{}",
        out.packaged.context
    );
    assert!(
        !out.packaged
            .context
            .contains("captured conversation window")
    );
    assert!(
        out.packaged
            .hits
            .iter()
            .all(|h| !h.text.contains("captured conversation window")),
        "capture hits must be dropped: {:?}",
        out.packaged.hits
    );
}

#[test]
fn trace_uses_filtered_top_and_preserves_both_gate_failures() {
    let mut mem = scoped_test_memory();
    let outcome = mem_recall_packaged_gated_filtered(
        &mut mem,
        "deployment",
        5,
        false,
        RecallFilters {
            gate: Some(100.0),
            cosine_gate: Some(1.0),
            scope: Some("project/a"),
            tag: None,
            knowledge_only: true,
        },
    )
    .expect("recall outcome");

    assert!(outcome.trace.has_hits);
    assert!(!outcome.trace.readout_pass);
    assert!(!outcome.trace.cosine_pass);
    assert!(!outcome.trace.eligible);
    assert!(
        outcome.trace.top_cosine.expect("filtered top cosine") < 1.0,
        "trace must use the scope-filtered local hit rather than the excluded remote hit"
    );
    assert!(outcome.packaged.hits.is_empty());
}
#[test]
fn eligible_trace_snapshots_deduplicated_result_ids_and_auto_extract_metadata() {
    let mut mem = Memory::in_memory_with_provider(Arc::new(StubProvider)).unwrap();
    let mut auto_extract = NoteOptions::default();
    auto_extract
        .metadata
        .push(("origin".to_string(), "auto-extract".to_string()));
    mem_remember_with(&mut mem, "auto-extract recall result", auto_extract).unwrap();

    let outcome =
        mem_recall_packaged_gated(&mut mem, "auto-extract recall result", 5, false, None, None)
            .unwrap();

    let expected_ids: Vec<u64> = outcome
        .packaged
        .hits
        .iter()
        .map(|hit| hit.node_id.0)
        .collect();
    assert_eq!(outcome.trace.result_node_ids, expected_ids);
    assert_eq!(outcome.trace.auto_extract_node_count, 1);
}

#[test]
fn unfiltered_fast_path_has_the_same_trace_as_filtered_path() {
    let mut fast = scoped_test_memory();
    let mut filtered = scoped_test_memory();

    let fast =
        mem_recall_packaged_gated(&mut fast, "deployment", 5, false, Some(0.0), Some(0.0)).unwrap();
    let filtered = mem_recall_packaged_gated_filtered(
        &mut filtered,
        "deployment",
        5,
        false,
        RecallFilters {
            gate: Some(0.0),
            cosine_gate: Some(0.0),
            scope: None,
            tag: None,
            knowledge_only: false,
        },
    )
    .unwrap();

    assert_eq!(fast.trace, filtered.trace);
    assert_eq!(fast.packaged.hits.len(), filtered.packaged.hits.len());
}

#[test]
fn empty_filtered_result_is_not_a_gate_specific_failure() {
    let trace = gate_trace(None, Some(12.0), Some(0.86));

    assert!(!trace.has_hits);
    assert!(!trace.readout_pass);
    assert!(!trace.cosine_pass);
    assert!(!trace.eligible);
    assert_eq!(trace.top_score, None);
    assert_eq!(trace.top_cosine, None);
}
#[test]
fn filtered_recall_gates_on_final_filtered_hits() {
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(ScopeGateProvider);
    let mut mem = Memory::in_memory_with_provider(provider).unwrap();

    let mut remote = NoteOptions {
        scope: Some(anamnesis::graph::ScopePath::new("project/remote").unwrap()),
        ..NoteOptions::default()
    };
    remote.tags.push("scope-gate".to_string());
    mem.add_note_with(
        "scope gate high remote note excluded by project scope",
        anamnesis::graph::Timestamp(1),
        remote,
    )
    .unwrap();

    let local = NoteOptions {
        scope: Some(anamnesis::graph::ScopePath::new("project/local").unwrap()),
        ..NoteOptions::default()
    };
    mem.add_note_with(
        "scope gate local note is visible but below cosine gate",
        anamnesis::graph::Timestamp(2),
        local,
    )
    .unwrap();

    let out = mem_recall_packaged_gated_filtered(
        &mut mem,
        "scope gate query",
        10,
        false,
        RecallFilters {
            gate: None,
            cosine_gate: Some(0.9),
            scope: Some("project/local"),
            tag: None,
            knowledge_only: false,
        },
    )
    .unwrap();

    assert!(
        out.packaged.context.is_empty(),
        "excluded remote hits must not open the cosine gate:\n{}",
        out.packaged.context
    );
    assert!(
        out.packaged.hits.is_empty(),
        "filtered below-gate hits must not be returned: {:?}",
        out.packaged.hits
    );
}

/// `gate = None` means no gating: the rendered block comes back even with a
/// huge would-be threshold, exactly as the classic `recall_packaged`.
#[test]
fn gated_recall_none_gate_never_filters() {
    let mut reg = registry(false);
    reg.remember("postgres was chosen for jsonb", None).unwrap();
    let gated = reg
        .recall_packaged_gated("postgres jsonb", 5, None, Some(false), None, None)
        .unwrap();
    assert!(gated.trace.has_hits);
    assert!(gated.trace.readout_pass);
    assert!(gated.trace.cosine_pass);
    assert!(gated.trace.eligible);
    assert!(!gated.packaged.hits.is_empty());
    assert!(gated.packaged.context.contains("##"));
}

/// `reinforce = false` is a pure read: repeated reads never lift base-level
/// salience (it only decays under the ticks), while `reinforce = true` does
/// lift it via the `used()` commit.
#[test]
fn read_only_recall_does_not_reinforce_but_reinforcing_does() {
    // Read-only: salience must not climb across repeated reads.
    let mut ro = registry(false);
    ro.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let ro_before = ro.stats(None).unwrap().avg_salience;
    for _ in 0..3 {
        let pkg = ro
            .recall_packaged_gated("auth race condition", 5, None, Some(false), None, None)
            .unwrap();
        assert!(
            !pkg.packaged.hits.is_empty(),
            "each read should still return the hit"
        );
    }
    let ro_after = ro.stats(None).unwrap().avg_salience;
    assert!(
        ro_after <= ro_before,
        "read-only recall must not increase salience: {ro_before} -> {ro_after}"
    );

    // Reinforcing: salience should climb under the same reads.
    let mut rw = registry(false);
    rw.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let rw_before = rw.stats(None).unwrap().avg_salience;
    for _ in 0..3 {
        rw.recall_packaged_gated("auth race condition", 5, None, Some(true), None, None)
            .unwrap();
    }
    let rw_after = rw.stats(None).unwrap().avg_salience;
    assert!(
        rw_after > rw_before,
        "reinforcing recall must increase salience: {rw_before} -> {rw_after}"
    );
}

/// A gated read-out that trips `τ` is a pure read regardless of `reinforce`:
/// nothing relevant ⇒ nothing reinforced.
#[test]
fn gated_out_recall_never_reinforces_even_when_asked() {
    let mut reg = registry(false);
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let before = reg.stats(None).unwrap().avg_salience;
    // τ astronomically high ⇒ always gated out, even with reinforce=true.
    for _ in 0..3 {
        let pkg = reg
            .recall_packaged_gated("auth race condition", 5, None, Some(true), Some(1e9), None)
            .unwrap();
        assert!(pkg.packaged.hits.is_empty(), "gate must trip at τ=1e9");
    }
    let after = reg.stats(None).unwrap().avg_salience;
    assert!(
        after <= before,
        "a gated-out recall must not reinforce: {before} -> {after}"
    );
}

/// `recall_packaged` (the classic entry) still behaves exactly as before:
/// it delegates to the gated method with the registry's reinforce default
/// and no gate. With `reinforce_on_recall = true` it lifts salience.
#[test]
fn recall_packaged_preserves_classic_reinforcing_behavior() {
    let mut reg = registry(true); // reinforce_on_recall = true
    reg.remember("the auth bug was a race in the middleware", None)
        .unwrap();
    let before = reg.stats(None).unwrap().avg_salience;
    for _ in 0..3 {
        let pkg = reg.recall_packaged("auth race condition", 5, None).unwrap();
        assert!(!pkg.hits.is_empty());
        assert!(pkg.context.contains("##"));
    }
    let after = reg.stats(None).unwrap().avg_salience;
    assert!(
        after > before,
        "classic recall_packaged with reinforce default on must lift salience: {before} -> {after}"
    );
}

// ── stats ─────────────────────────────────────────────────────────────────

#[test]
fn stats_counts_remembered_nodes() {
    let mut reg = registry(false);
    let empty = reg.stats(None).unwrap();
    assert_eq!(empty.node_count, 0);
    reg.remember("one fact", None).unwrap();
    reg.remember("another fact", None).unwrap();
    let s = reg.stats(None).unwrap();
    // Each `remember` is an Episodic + Semantic node (2 per note).
    assert!(
        s.node_count >= 4,
        "expected >= 4 nodes, got {}",
        s.node_count
    );
}

// ── usage_report (dogfood metrics) ─────────────────────────────────────────

#[test]
fn usage_report_counts_ops_and_backlog() {
    let mut reg = registry(true);
    // 1 remember, 1 relate, 2 recalls (1 reinforcing), 1 captured turn, 1 pull.
    let a = reg.remember("the deploy failed", None).unwrap();
    let b = reg.remember("the disk was full", None).unwrap();
    reg.relate(b, a, "causes", None).unwrap();
    let _ = reg
        .recall_packaged_gated("deploy", 5, None, Some(false), None, None)
        .unwrap();
    let _ = reg
        .recall_packaged_gated("deploy", 5, None, Some(true), None, None)
        .unwrap();
    let turns = vec![Turn {
        speaker: "user".into(),
        text: "capture me".into(),
        at_ms: Some(1),
    }];
    reg.ingest_conversation("s", &turns, None, true).unwrap();
    let _ = reg.pull_pending(Some(10), None).unwrap();

    let report = reg.usage_report(None).unwrap();
    assert!(
        report.contains("recalls: 2 (1 reinforcing)"),
        "got: {report}"
    );
    assert!(report.contains("remembers: 2"), "got: {report}");
    assert!(report.contains("relates: 1"), "got: {report}");
    assert!(report.contains("captured turns: 1"), "got: {report}");
    assert!(report.contains("extraction pulls: 1"), "got: {report}");
    assert!(
        report.contains("extraction backlog: 0"),
        "drained: {report}"
    );
    assert!(report.contains("captured total: 1"), "got: {report}");
    assert!(report.contains("stale ratio"), "got: {report}");
}

mod embedding_migration {
    use super::migration::{
        EMBEDDING_MIGRATION_BATCH_SIZE, migrate_embeddings,
        migrate_embeddings_with_backup_verification_cleanup,
    };
    use super::*;
    use anamnesis::engine::{KnowledgeType, Node, NodeId, Timestamp};
    use anamnesis::graph::edge::{Edge, EdgeSource};
    use anamnesis::graph::node::Origin;
    use anamnesis::graph::types::{PeerId, SourceKind};
    use anamnesis::graph::{EdgeType, MemoryTier, ScopePath};
    use anamnesis::storage::sqlite::{EmbeddingMigrationInspection, EmbeddingSelection};
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingProvider {
        dimensions: usize,
        model: &'static str,
        passage_value: f32,
        raw_value: f32,
        fail_at: Option<usize>,
        passages: Mutex<Vec<String>>,
        raw_calls: AtomicUsize,
    }

    impl RecordingProvider {
        fn healthy(dimensions: usize, model: &'static str) -> Self {
            Self {
                dimensions,
                model,
                passage_value: 0.75,
                raw_value: -0.25,
                fail_at: None,
                passages: Mutex::new(Vec::new()),
                raw_calls: AtomicUsize::new(0),
            }
        }

        fn failing(dimensions: usize, model: &'static str, fail_at: usize) -> Self {
            Self {
                fail_at: Some(fail_at),
                ..Self::healthy(dimensions, model)
            }
        }

        fn passage_inputs(&self) -> Vec<String> {
            self.passages
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl EmbeddingProvider for RecordingProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            self.raw_calls.fetch_add(texts.len(), Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|_| vec![self.raw_value; self.dimensions])
                .collect())
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn model_name(&self) -> &str {
            self.model
        }

        fn embed_passage(&self, text: &str) -> Result<Vec<f32>, Error> {
            let call = {
                let mut passages = self
                    .passages
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let call = passages.len();
                passages.push(text.to_string());
                call
            };
            if self.fail_at.is_some_and(|fail_at| call >= fail_at) {
                return Err(Error::InvalidInput(format!(
                    "injected passage failure for {text}"
                )));
            }
            Ok(vec![self.passage_value; self.dimensions])
        }
    }

    fn fixture_node(id: NodeId, dimensions: usize, prefix: &str) -> Node {
        Node {
            id,
            node_type: KnowledgeType::Semantic,
            name: format!("{prefix}-node-{}", id.0),
            summary: Some(format!("{prefix}-summary-{}", id.0)),
            content: format!("{prefix}-content-{}", id.0),
            embedding: Some(vec![0.125; dimensions]),
            created_at: Timestamp(1_000 + id.0),
            updated_at: Timestamp(2_000 + id.0),
            accessed_at: Timestamp(3_000 + id.0),
            valid_from: Some(Timestamp(900 + id.0)),
            valid_until: Some(Timestamp(9_000 + id.0)),
            salience: 0.7,
            retained_action: 0.4,
            evidence_prior: 0.3,
            access_count: 2,
            access_history: VecDeque::new(),
            tier: MemoryTier::Recall,
            origin: Origin {
                peer_id: PeerId(17),
                source_kind: SourceKind::AgentObservation,
                session_id: format!("{prefix}-session"),
                scope: ScopePath::new("project/migration").expect("valid fixture scope"),
                confidence: 0.93,
            },
            entity_tags: vec!["migration".to_string(), format!("entity-{}", id.0)],
            metadata: HashMap::from([
                ("preserved".to_string(), "true".to_string()),
                ("fixture".to_string(), prefix.to_string()),
            ]),
        }
    }

    fn create_fixture(
        db_path: &std::path::Path,
        count: usize,
        dimensions: usize,
        model: &str,
        prefix: &str,
    ) {
        let mut storage = SqliteStorage::open(db_path).expect("open fixture database");
        for _ in 0..count {
            let id = storage.next_node_id();
            storage
                .set_node(fixture_node(id, dimensions, prefix))
                .expect("persist fixture node");
        }
        if count >= 2 {
            let edge_id = storage.next_edge_id();
            storage
                .set_edge(Edge::seeded(
                    edge_id,
                    NodeId(0),
                    NodeId(1),
                    EdgeType::Reason,
                    0.6,
                    EdgeSource::Manual,
                    Timestamp(4_000),
                    Timestamp(4_500),
                    HashMap::from([("edge-preserved".to_string(), "true".to_string())]),
                ))
                .expect("persist fixture edge");
        }
        storage
            .set_embedding_model_name(model)
            .expect("stamp source model");
    }

    fn request(
        db_path: &std::path::Path,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> EmbeddingMigrationRequest {
        wait_for_file_registry_lock_release(db_path);
        let pending = PendingEmbeddingMigrationRequest {
            namespace: "default".to_string(),
            db_path: db_path.to_path_buf(),
            provider,
        };
        let lease = acquire_namespace_migration_lock(db_path).expect("acquire migration lock");
        (pending, lease).into()
    }

    fn daemon_leased_request(
        db_path: &std::path::Path,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> EmbeddingMigrationRequest {
        let lock_path = namespace_lock_path(db_path);
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .expect("open daemon fixture lock");
        fs4::FileExt::try_lock(&lock_file).expect("acquire daemon fixture lock");
        (
            PendingEmbeddingMigrationRequest {
                namespace: "default".to_string(),
                db_path: db_path.to_path_buf(),
                provider,
            },
            MigrationLockLease::from(Arc::new(lock_file)),
        )
            .into()
    }

    fn inspection(db_path: &std::path::Path) -> EmbeddingMigrationInspection {
        SqliteStorage::inspect_embedding_migration(db_path).expect("inspect migration state")
    }

    fn graph_snapshot(db_path: &std::path::Path) -> (Vec<Node>, Vec<Edge>) {
        let storage = SqliteStorage::open(db_path).expect("open graph snapshot");
        let nodes = storage
            .all_node_ids()
            .into_iter()
            .map(|id| storage.get_node(id).expect("snapshot node").clone())
            .collect();
        let edges = storage
            .all_edge_ids()
            .into_iter()
            .map(|id| storage.get_edge(id).expect("snapshot edge").clone())
            .collect();
        (nodes, edges)
    }

    fn non_embedding_snapshot(db_path: &std::path::Path) -> (Vec<Node>, Vec<Edge>) {
        let (mut nodes, edges) = graph_snapshot(db_path);
        for node in &mut nodes {
            node.embedding = None;
        }
        (nodes, edges)
    }

    fn expected_inputs(prefix: &str, range: std::ops::Range<usize>) -> Vec<String> {
        range
            .map(|index| format!("{prefix}-content-{index}"))
            .collect()
    }

    #[test]
    fn migrates_768_fixture_via_passage_and_preserves_graph() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("graph.db");
        create_fixture(&db_path, 3, 768, "bge-base-en-v1.5", "happy");
        {
            let mut storage = SqliteStorage::open(&db_path).expect("open Scenario A fixture");
            let mut identity = storage
                .get_node(NodeId(1))
                .expect("identity fixture node")
                .clone();
            identity.node_type = KnowledgeType::Identity;
            identity.salience = 0.61;
            identity.origin.peer_id = PeerId(18);
            identity.origin.session_id = "happy-identity-session".to_string();
            identity.valid_until = None;
            identity.access_count = 7;
            identity
                .metadata
                .insert("knowledge-role".to_string(), "identity".to_string());
            storage
                .set_node(identity)
                .expect("persist identity fixture node");

            let mut decision = storage
                .get_node(NodeId(2))
                .expect("decision fixture node")
                .clone();
            decision.node_type = KnowledgeType::Custom("decision".to_string());
            decision.salience = 0.83;
            decision.origin.peer_id = PeerId(19);
            decision.origin.session_id = "happy-decision-session".to_string();
            decision.valid_from = None;
            decision.access_count = 5;
            decision
                .metadata
                .insert("knowledge-role".to_string(), "decision".to_string());
            storage
                .set_node(decision)
                .expect("persist decision fixture node");

            let edge_id = storage.next_edge_id();
            storage
                .set_edge(Edge::seeded(
                    edge_id,
                    NodeId(1),
                    NodeId(2),
                    EdgeType::Supports,
                    0.4,
                    EdgeSource::Auto,
                    Timestamp(5_000),
                    Timestamp(5_500),
                    HashMap::from([("edge-preserved".to_string(), "supports".to_string())]),
                ))
                .expect("persist supports fixture edge");
        }
        let before = non_embedding_snapshot(&db_path);
        assert_eq!(
            before
                .0
                .iter()
                .map(|node| node.node_type.clone())
                .collect::<Vec<_>>(),
            vec![
                KnowledgeType::Semantic,
                KnowledgeType::Identity,
                KnowledgeType::Custom("decision".to_string()),
            ]
        );
        assert_eq!(
            before
                .1
                .iter()
                .map(|edge| edge.edge_type.clone())
                .collect::<Vec<_>>(),
            vec![EdgeType::Reason, EdgeType::Supports]
        );
        let provider = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));
        let mut progress = Vec::new();

        let outcome = migrate_embeddings(
            daemon_leased_request(&db_path, provider.clone()),
            &mut |event| progress.push(event),
        )
        .expect("migrate fixture");

        let EmbeddingMigrationOutcome::Migrated(report) = outcome else {
            panic!("expected migrated outcome");
        };
        assert_eq!(report.scanned, 3);
        assert_eq!(report.migrated, 3);
        assert_eq!(report.resumed, 0);
        assert_eq!(report.batches, 1);
        assert!(report.backup_path.is_file());
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].committed, 3);
        assert_eq!(provider.passage_inputs(), expected_inputs("happy", 0..3));
        assert_eq!(provider.raw_calls.load(Ordering::SeqCst), 0);
        let after = inspection(&db_path);
        assert_eq!(
            after.embedding_model.as_deref(),
            Some("multilingual-e5-small")
        );
        assert_eq!(after.checkpoint, None);
        assert_eq!(after.embedding_dimensions, vec![Some(384); 3]);
        assert_eq!(non_embedding_snapshot(&db_path), before);
        let storage = SqliteStorage::open(&db_path).expect("open migrated graph");
        assert_eq!(storage.node_count(), 3);
        assert_eq!(storage.edge_count(), 2);
        assert_eq!(storage.text_search("happy-content-1", 5)[0].0, NodeId(1));
        assert_eq!(
            storage
                .get_node(NodeId(0))
                .expect("migrated node")
                .embedding,
            Some(vec![0.75; 384])
        );
        drop(storage);
        let mut registry =
            file_registry(provider.clone(), db_path.clone(), dir.path().to_path_buf());
        assert_eq!(registry.stats(None).expect("guard reopen").node_count, 3);
        assert!(
            !registry
                .recall("happy-content-1", 3, None)
                .expect("recall migrated graph")
                .is_empty()
        );
    }

    #[test]
    fn interruption_after_one_batch_resumes_only_remaining_old_dimensions() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("resume-dimension.db");
        create_fixture(&db_path, 70, 768, "bge-base-en-v1.5", "dimension");
        let failing = Arc::new(RecordingProvider::failing(
            384,
            "multilingual-e5-small",
            EMBEDDING_MIGRATION_BATCH_SIZE,
        ));
        let mut first_progress = Vec::new();

        let first = migrate_embeddings(request(&db_path, failing.clone()), &mut |event| {
            first_progress.push(event)
        });

        assert!(first.is_err());
        assert_eq!(first_progress.len(), 1);
        let interrupted = inspection(&db_path);
        assert_eq!(
            interrupted.embedding_model.as_deref(),
            Some("bge-base-en-v1.5")
        );
        assert!(interrupted.checkpoint.is_some());
        assert_eq!(
            interrupted
                .embedding_dimensions
                .iter()
                .filter(|dimension| **dimension == Some(384))
                .count(),
            64
        );
        assert_eq!(
            interrupted
                .embedding_dimensions
                .iter()
                .filter(|dimension| **dimension == Some(768))
                .count(),
            6
        );
        assert_eq!(failing.raw_calls.load(Ordering::SeqCst), 0);
        let checkpoint = interrupted
            .checkpoint
            .as_ref()
            .expect("checkpoint remains after the committed batch failure");
        let backup_path = checkpoint.backup_path.clone();
        let mut validation_path = backup_path.as_os_str().to_os_string();
        validation_path.push(".verify");
        let validation_path = std::path::PathBuf::from(validation_path);
        SqliteStorage::create_verified_backup(&backup_path, &validation_path)
            .expect("checkpoint backup passes core verified-backup quick check");
        assert!(validation_path.is_file());
        std::fs::remove_file(&validation_path).expect("remove checkpoint backup validation copy");
        assert!(!validation_path.exists());

        let healthy = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));
        {
            let mut guarded_registry =
                file_registry(healthy.clone(), db_path.clone(), dir.path().to_path_buf());
            let recall_error = guarded_registry
                .recall("dimension-content-0", 3, None)
                .expect_err("normal registry recall must reject the incomplete migration");
            assert!(
                recall_error
                    .to_string()
                    .contains("anamnesis migrate-embeddings"),
                "{recall_error}"
            );
            drop(guarded_registry);
        }
        let outcome = migrate_embeddings(request(&db_path, healthy.clone()), &mut |_| {})
            .expect("resume dimension migration");

        let EmbeddingMigrationOutcome::Migrated(report) = outcome else {
            panic!("expected resumed migration");
        };
        assert_eq!(report.migrated, 6);
        assert_eq!(report.resumed, 64);
        assert_eq!(
            healthy.passage_inputs(),
            expected_inputs("dimension", 64..70)
        );
        let completed = inspection(&db_path);
        assert_eq!(completed.embedding_dimensions, vec![Some(384); 70]);
        assert_eq!(
            completed.embedding_model.as_deref(),
            Some("multilingual-e5-small")
        );
        assert_eq!(completed.checkpoint, None);
    }

    #[test]
    fn resume_cleanup_failure_reports_preserved_verified_backup() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("resume-cleanup.db");
        create_fixture(
            &db_path,
            1,
            768,
            "bge-base-en-v1.5",
            "resume-cleanup-secret",
        );
        let failing = Arc::new(RecordingProvider::failing(384, "multilingual-e5-small", 0));

        migrate_embeddings(request(&db_path, failing), &mut |_| {})
            .expect_err("initial migration must leave a checkpoint");
        let checkpoint = inspection(&db_path)
            .checkpoint
            .expect("initial migration checkpoint");
        let backup_path = checkpoint.backup_path.clone();
        assert!(backup_path.is_file());

        let mut validation_path = backup_path.clone().into_os_string();
        validation_path.push(".verify");
        let validation_path = std::path::PathBuf::from(validation_path);
        let mut fail_cleanup = |_: &std::path::Path| {
            Err(Error::StorageError(
                "injected backup verification cleanup failure".to_string(),
            ))
        };
        let provider = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));
        let failure = migrate_embeddings_with_backup_verification_cleanup(
            request(&db_path, provider.clone()),
            &mut |_| {},
            &mut fail_cleanup,
        )
        .expect_err("cleanup failure must stop the resume");
        let message = failure.to_string();
        assert_eq!(
            failure.backup_state,
            MigrationBackupState::BackupPreserved {
                backup_path: backup_path.clone(),
            },
        );
        assert_eq!(
            failure.failure_context,
            MigrationFailureContext::BackupVerificationCleanup {
                backup_path: backup_path.clone(),
                validation_path: validation_path.clone(),
            },
        );
        assert_eq!(
            failure.retained_source(),
            &Error::StorageError("injected backup verification cleanup failure".to_string())
        );
        assert!(validation_path.is_file());

        assert!(std::error::Error::source(&failure).is_none());
        assert!(
            message.contains(&backup_path.display().to_string()),
            "{message}"
        );
        assert!(message.contains("after the checkpoint backup"), "{message}");
        assert!(message.contains("was verified"), "{message}");
        assert!(message.contains("is preserved"), "{message}");
        assert!(message.contains("Remove the validation copy"), "{message}");
        assert!(
            message.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{message}"
        );
        assert!(
            !message.contains("resume-cleanup-secret-content-0"),
            "{message}"
        );
        assert!(provider.passage_inputs().is_empty());
        let resumed = inspection(&db_path);
        assert_eq!(
            resumed
                .checkpoint
                .as_ref()
                .map(|checkpoint| &checkpoint.backup_path),
            Some(&backup_path)
        );
        assert_eq!(resumed.embedding_dimensions, vec![Some(768)]);
    }
    #[test]
    fn resume_backup_verification_failure_names_preserved_checkpoint_backup() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("resume-verification.db");
        create_fixture(
            &db_path,
            1,
            768,
            "bge-base-en-v1.5",
            "resume-verification-secret",
        );
        let failing = Arc::new(RecordingProvider::failing(384, "multilingual-e5-small", 0));

        migrate_embeddings(request(&db_path, failing), &mut |_| {})
            .expect_err("initial migration must leave a checkpoint");
        let checkpoint = inspection(&db_path)
            .checkpoint
            .expect("initial migration checkpoint");
        let backup_path = checkpoint.backup_path.clone();
        std::fs::write(&backup_path, b"invalid checkpoint backup")
            .expect("corrupt checkpoint backup");

        let provider = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));
        let failure = migrate_embeddings(request(&db_path, provider.clone()), &mut |_| {})
            .expect_err("backup verification must stop the resume");
        let message = failure.to_string();

        assert!(std::error::Error::source(&failure).is_none());
        assert!(
            message.contains(&backup_path.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains("re-verify the preserved checkpoint backup"),
            "{message}"
        );
        assert!(
            message.contains("Preserve it, resolve the verification issue"),
            "{message}"
        );
        assert!(
            message.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{message}"
        );
        assert!(
            !message.contains("resume-verification-secret-content-0"),
            "{message}"
        );
        assert!(provider.passage_inputs().is_empty());
        let resumed = inspection(&db_path);
        assert_eq!(
            resumed
                .checkpoint
                .as_ref()
                .map(|checkpoint| &checkpoint.backup_path),
            Some(&backup_path)
        );
        assert_eq!(resumed.embedding_dimensions, vec![Some(768)]);
    }

    #[test]
    fn same_dimension_model_change_resumes_from_atomic_cursor() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("resume-cursor.db");
        create_fixture(&db_path, 70, 3, "model-a", "cursor");
        let failing = Arc::new(RecordingProvider::failing(
            3,
            "model-b",
            EMBEDDING_MIGRATION_BATCH_SIZE,
        ));

        assert!(migrate_embeddings(request(&db_path, failing), &mut |_| {}).is_err());
        let checkpoint = inspection(&db_path)
            .checkpoint
            .expect("cursor checkpoint remains");
        assert_eq!(checkpoint.selection, EmbeddingSelection::Cursor);
        assert_eq!(checkpoint.cursor, Some(NodeId(63)));
        assert_eq!(inspection(&db_path).embedding_dimensions, vec![Some(3); 70]);
        let healthy = Arc::new(RecordingProvider::healthy(3, "model-b"));

        let outcome = migrate_embeddings(request(&db_path, healthy.clone()), &mut |_| {})
            .expect("resume cursor migration");

        let EmbeddingMigrationOutcome::Migrated(report) = outcome else {
            panic!("expected resumed cursor migration");
        };
        assert_eq!(report.migrated, 6);
        assert_eq!(report.resumed, 64);
        assert_eq!(healthy.passage_inputs(), expected_inputs("cursor", 64..70));
        assert_eq!(
            inspection(&db_path).embedding_model.as_deref(),
            Some("model-b")
        );
        assert_eq!(inspection(&db_path).checkpoint, None);
    }

    #[test]
    fn migration_errors_distinguish_pre_backup_from_preserved_backup() {
        let dir = tempfile::tempdir().expect("temporary directory");

        let pre_backup_db = dir.path().join("pre-backup.db");
        create_fixture(
            &pre_backup_db,
            1,
            768,
            "bge-base-en-v1.5",
            "pre-backup-secret",
        );
        let pre_backup_path = backup_path_for_database(
            &std::fs::canonicalize(&pre_backup_db).expect("canonicalize pre-backup database"),
        )
        .expect("derive pre-backup path");
        std::fs::create_dir(&pre_backup_path).expect("block backup destination with directory");
        let pre_backup_provider =
            Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));

        let pre_backup_error =
            migrate_embeddings(request(&pre_backup_db, pre_backup_provider), &mut |_| {})
                .expect_err("backup creation must fail")
                .to_string();

        assert!(
            pre_backup_error.contains(&pre_backup_path.display().to_string()),
            "{pre_backup_error}"
        );
        assert!(
            pre_backup_error.contains("no verified backup was created"),
            "{pre_backup_error}"
        );
        assert!(
            pre_backup_error.contains("Preserve or move it before rerunning"),
            "{pre_backup_error}"
        );
        assert!(
            pre_backup_error.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{pre_backup_error}"
        );
        assert!(
            !pre_backup_error.contains("pre-backup-secret-content-0"),
            "{pre_backup_error}"
        );

        let post_backup_db = dir.path().join("post-backup.db");
        create_fixture(
            &post_backup_db,
            1,
            768,
            "bge-base-en-v1.5",
            "post-backup-secret",
        );
        let post_backup_path = backup_path_for_database(
            &std::fs::canonicalize(&post_backup_db).expect("canonicalize post-backup database"),
        )
        .expect("derive post-backup path");
        let post_backup_provider =
            Arc::new(RecordingProvider::failing(384, "multilingual-e5-small", 0));

        let post_backup_failure =
            migrate_embeddings(request(&post_backup_db, post_backup_provider), &mut |_| {})
                .expect_err("embedding must fail after backup creation");
        assert_eq!(
            post_backup_failure.retained_source(),
            &Error::InvalidInput(
                "injected passage failure for post-backup-secret-content-0".to_string()
            )
        );
        assert!(std::error::Error::source(&post_backup_failure).is_none());
        let post_backup_error = post_backup_failure.to_string();
        let post_backup_debug = format!("{post_backup_failure:?}");
        let post_backup_anyhow_debug = format!("{:?}", anyhow::Error::new(post_backup_failure));

        assert!(
            post_backup_error.contains(&post_backup_path.display().to_string()),
            "{post_backup_error}"
        );
        assert!(
            post_backup_error.contains("is preserved"),
            "{post_backup_error}"
        );
        assert!(
            post_backup_error.contains("cause category: invalid input"),
            "{post_backup_error}"
        );
        assert!(
            post_backup_error.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{post_backup_error}"
        );
        assert!(
            !post_backup_error.contains("post-backup-secret-content-0"),
            "{post_backup_error}"
        );
        assert!(
            !post_backup_debug.contains("post-backup-secret-content-0"),
            "{post_backup_debug}"
        );
        assert!(
            post_backup_debug.contains("is preserved"),
            "{post_backup_debug}"
        );
        assert!(
            post_backup_debug.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{post_backup_debug}"
        );
        assert!(
            post_backup_anyhow_debug.contains("is preserved"),
            "{post_backup_anyhow_debug}"
        );
        assert!(
            post_backup_anyhow_debug.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{post_backup_anyhow_debug}"
        );
        assert!(
            !post_backup_anyhow_debug.contains("post-backup-secret-content-0"),
            "{post_backup_anyhow_debug}"
        );
    }

    #[test]
    fn backup_failure_writes_no_checkpoint_or_embedding() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("backup-failure.db");
        create_fixture(&db_path, 2, 768, "bge-base-en-v1.5", "backup-failure");
        let before = graph_snapshot(&db_path);
        let backup_path = backup_path_for_database(&db_path).expect("derive backup path");
        std::fs::create_dir(&backup_path).expect("block backup destination with directory");
        let provider = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));

        let result = migrate_embeddings(request(&db_path, provider.clone()), &mut |_| {});

        assert!(result.is_err());
        assert!(provider.passage_inputs().is_empty());
        assert_eq!(graph_snapshot(&db_path), before);
        let after = inspection(&db_path);
        assert_eq!(after.checkpoint, None);
        assert_eq!(after.embedding_model.as_deref(), Some("bge-base-en-v1.5"));
    }

    #[test]
    fn existing_unrelated_backup_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("unrelated.db");
        create_fixture(&db_path, 2, 768, "bge-base-en-v1.5", "unrelated");
        let backup_path = backup_path_for_database(&db_path).expect("derive backup path");
        let unrelated = b"operator-owned-unrelated-backup";
        std::fs::write(&backup_path, unrelated).expect("write unrelated backup bytes");
        let provider = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));
        let result = migrate_embeddings(request(&db_path, provider.clone()), &mut |_| {});

        let error = result
            .expect_err("existing unrelated backup must fail closed")
            .to_string();
        assert!(
            error.contains(&backup_path.display().to_string()),
            "{error}"
        );
        assert!(
            error.contains("Preserve or move it before rerunning"),
            "{error}"
        );
        assert!(
            error.contains("anamnesis migrate-embeddings [--namespace NS]"),
            "{error}"
        );
        assert!(!error.contains("unrelated-content-0"), "{error}");
        assert_eq!(std::fs::read(&backup_path).expect("read backup"), unrelated);
        assert!(provider.passage_inputs().is_empty());
        assert_eq!(inspection(&db_path).checkpoint, None);
        assert_eq!(
            inspection(&db_path).embedding_dimensions,
            vec![Some(768); 2]
        );
    }

    #[test]
    fn existing_backup_without_checkpoint_fails_closed_even_when_coarse_inventory_matches() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("live.db");
        let other_path = dir.path().join("other.db");
        create_fixture(&db_path, 2, 768, "bge-base-en-v1.5", "live");
        create_fixture(&other_path, 2, 768, "bge-base-en-v1.5", "other");
        let backup_path = backup_path_for_database(&db_path).expect("derive backup path");
        SqliteStorage::create_verified_backup(&other_path, &backup_path)
            .expect("create valid but unrelated backup");
        let backup_bytes = std::fs::read(&backup_path).expect("snapshot unrelated backup");
        let provider = Arc::new(RecordingProvider::healthy(384, "multilingual-e5-small"));

        let result = migrate_embeddings(request(&db_path, provider.clone()), &mut |_| {});

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&backup_path).expect("reread unrelated backup"),
            backup_bytes
        );
        assert!(provider.passage_inputs().is_empty());
        assert_eq!(inspection(&db_path).checkpoint, None);
        assert_eq!(
            inspection(&db_path).embedding_dimensions,
            vec![Some(768); 2]
        );
    }

    #[test]
    fn manual_migration_refuses_daemon_owned_lock_before_backup() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("daemon-owned.db");
        create_fixture(&db_path, 2, 768, "bge-base-en-v1.5", "locked");
        let before = graph_snapshot(&db_path);
        let backup_path = backup_path_for_database(&db_path).expect("derive backup path");
        let _daemon_lease = acquire_namespace_migration_lock(&db_path)
            .expect("simulate daemon-owned namespace lock");
        let registry = file_registry(
            Arc::new(RecordingProvider::healthy(
                384,
                "intfloat/multilingual-e5-small",
            )),
            db_path.clone(),
            dir.path().to_path_buf(),
        );

        let error = crate::cli::prepare_manual_migration(&registry, None)
            .err()
            .expect("manual migration must refuse the daemon lock");

        let message = error.to_string();
        assert!(message.contains("stop the anamnesis daemon"), "{message}");
        assert!(message.contains("retry"), "{message}");
        assert!(!backup_path.exists());
        assert_eq!(graph_snapshot(&db_path), before);
    }

    #[test]
    fn progress_is_reported_only_for_committed_batches() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let db_path = dir.path().join("committed-progress.db");
        create_fixture(
            &db_path,
            EMBEDDING_MIGRATION_BATCH_SIZE + 1,
            768,
            "bge-base-en-v1.5",
            "progress",
        );
        let provider = Arc::new(RecordingProvider::failing(
            384,
            "multilingual-e5-small",
            EMBEDDING_MIGRATION_BATCH_SIZE,
        ));
        let mut progress = Vec::new();

        let result = migrate_embeddings(request(&db_path, provider), &mut |event| {
            progress.push(event)
        });

        assert!(result.is_err());
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].batch, 1);
        assert_eq!(progress[0].committed, EMBEDDING_MIGRATION_BATCH_SIZE);
        assert_eq!(progress[0].total, EMBEDDING_MIGRATION_BATCH_SIZE + 1);
    }
}
