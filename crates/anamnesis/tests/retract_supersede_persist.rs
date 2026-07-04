//! #7 — `retract()` and `Supersedes` validity-window mutations must survive a
//! drop + reopen cycle.
//!
//! `SqliteStorage::flush()` only write-behinds the hot fields (salience /
//! accessed_at / evidence_prior / retained_action). The retraction markers
//! (node `metadata`) and supersede windows (`valid_from` / `valid_until`) live
//! on the `nodes` row, which `flush()` never touches — so before the fix they
//! were lost on reopen (the retraction/supersede "resurrected"). These tests use
//! a temp FILE-backed DB (NOT in-memory) so the reopen actually exercises disk.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};

fn obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1_000),
        valid_from: None,
        valid_until: None,
    }
}

fn config() -> EngineConfig {
    EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false)
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "anamnesis-{tag}-{}-{}.db",
        std::process::id(),
        Timestamp::now().0
    ))
}

#[test]
fn retract_persists_across_reopen() {
    let path = temp_db_path("retract-persist");

    // Given: an ingested node that is retracted, flushed, then the engine dropped.
    let node_id: NodeId = {
        let storage = SqliteStorage::open(&path).expect("open file db");
        let mut engine = Engine::with_storage(config(), storage);
        let IngestResult::Created(ids) = engine.ingest(obs("doomed")).unwrap() else {
            panic!("expected Created");
        };
        let id = ids[0];
        engine
            .retract(id, "superseded by newer info", Timestamp(2_000))
            .expect("retract ok");
        engine.graph_mut().storage_mut().flush().expect("flush ok");
        id
    };

    // When: the same path is reopened in a fresh storage.
    let storage = SqliteStorage::open(&path).expect("reopen file db");

    // Then: the retraction markers persist (they did not before the fix).
    let node = storage.get_node(node_id).expect("node persisted");
    assert_eq!(
        node.metadata.get("retracted").map(String::as_str),
        Some("true"),
        "retraction marker must survive reopen (flush() skips node metadata)"
    );
    assert_eq!(
        node.metadata.get("retraction_reason").map(String::as_str),
        Some("superseded by newer info"),
        "retraction reason must survive reopen"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn supersede_validity_windows_persist_across_reopen() {
    let path = temp_db_path("supersede-persist");

    // Given: new-fact Supersedes old-fact, flushed, then the engine dropped.
    let (old_id, new_id): (NodeId, NodeId) = {
        let storage = SqliteStorage::open(&path).expect("open file db");
        let mut engine = Engine::with_storage(config(), storage);
        let IngestResult::Created(old_ids) = engine.ingest(obs("old-fact")).unwrap() else {
            panic!("expected Created");
        };
        let IngestResult::Created(new_ids) = engine.ingest(obs("new-fact")).unwrap() else {
            panic!("expected Created");
        };
        let old_id = old_ids[0];
        let new_id = new_ids[0];
        engine
            .link(new_id, old_id, EdgeType::Supersedes)
            .expect("link supersedes ok");
        engine.graph_mut().storage_mut().flush().expect("flush ok");
        (old_id, new_id)
    };

    // When: the same path is reopened in a fresh storage.
    let storage = SqliteStorage::open(&path).expect("reopen file db");

    // Then: both validity windows persist (they did not before the fix).
    let old_node = storage.get_node(old_id).expect("old node persisted");
    let new_node = storage.get_node(new_id).expect("new node persisted");
    assert!(
        old_node.valid_until.is_some(),
        "superseded node's valid_until must survive reopen (flush() skips valid_until)"
    );
    assert!(
        new_node.valid_from.is_some(),
        "superseding node's valid_from must survive reopen (flush() skips valid_from)"
    );

    let _ = std::fs::remove_file(&path);
}

/// P1-T1 RED: `unretract()` must clear the retraction metadata and persist that
/// removal through a drop + reopen cycle, mirroring `retract`'s write-through path.
#[test]
fn unretract_clears_metadata_and_persists() {
    let path = temp_db_path("unretract-persist");

    // Given: an ingested node that is retracted, then un-retracted, then flushed.
    let node_id: NodeId = {
        let storage = SqliteStorage::open(&path).expect("open file db");
        let mut engine = Engine::with_storage(config(), storage);
        let IngestResult::Created(ids) = engine.ingest(obs("reinstated")).unwrap() else {
            panic!("expected Created");
        };
        let id = ids[0];

        engine
            .retract(id, "superseded by newer info", Timestamp(2_000))
            .expect("retract ok");
        assert!(
            engine.is_retracted(id).expect("is_retracted ok"),
            "node must be retracted before unretract"
        );

        engine
            .unretract(id, Timestamp(3_000))
            .expect("unretract ok");
        assert!(
            !engine.is_retracted(id).expect("is_retracted ok"),
            "node must no longer be retracted after unretract"
        );

        engine.graph_mut().storage_mut().flush().expect("flush ok");
        id
    };

    // When: the same path is reopened in a fresh storage.
    let storage = SqliteStorage::open(&path).expect("reopen file db");

    // Then: the un-retraction survives reopen — all three metadata keys are gone.
    let node = storage.get_node(node_id).expect("node persisted");
    assert!(
        node.metadata.get("retracted").is_none_or(|v| v != "true"),
        "retracted marker must be cleared after reopen"
    );
    assert!(
        !node.metadata.contains_key("retraction_reason"),
        "retraction_reason must be removed after reopen"
    );
    assert!(
        !node.metadata.contains_key("retracted_at"),
        "retracted_at must be removed after reopen"
    );

    let _ = std::fs::remove_file(&path);
}

/// P1-T1 MANUAL-QA demo: prints the `is_retracted` state at each stage of the
/// retract → unretract → reopen cycle for a hands-on DB-state-diff artifact.
/// Run with `cargo test -p anamnesis-engine unretract_roundtrip_demo -- --nocapture`.
#[test]
fn unretract_roundtrip_demo() {
    let path = temp_db_path("unretract-demo");

    let node_id: NodeId = {
        let storage = SqliteStorage::open(&path).expect("open file db");
        let mut engine = Engine::with_storage(config(), storage);
        let IngestResult::Created(ids) = engine.ingest(obs("demo-node")).unwrap() else {
            panic!("expected Created");
        };
        let id = ids[0];

        engine
            .retract(id, "demo retraction", Timestamp(2_000))
            .expect("retract ok");
        println!(
            "after_retract is_retracted={}",
            engine.is_retracted(id).expect("is_retracted ok")
        );

        engine
            .unretract(id, Timestamp(3_000))
            .expect("unretract ok");
        println!(
            "after_unretract is_retracted={}",
            engine.is_retracted(id).expect("is_retracted ok")
        );

        engine.graph_mut().storage_mut().flush().expect("flush ok");
        id
    };

    let storage = SqliteStorage::open(&path).expect("reopen file db");
    let engine = Engine::with_storage(config(), storage);
    println!(
        "after_reopen is_retracted={}",
        engine.is_retracted(node_id).expect("is_retracted ok")
    );

    let _ = std::fs::remove_file(&path);
}
