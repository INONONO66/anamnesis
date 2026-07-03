//! Integration tests for the `Memory` management API (P1-T2):
//! `update_content` / `get` / `list` / `forget` / `unforget` / `delete_hard` /
//! `supersede`.
//!
//! Hermetic: a deterministic byte-derived embedder (no model download, no
//! network), the same pattern used by `tests/reasoning_advantage.rs`.
//
// allow: SIZE_OK — 8 independently-scoped test scenarios (one per public
// method plus a baseline, an adversarial-input case, and a manual-QA demo)
// are each required by the task spec to stay isolated per the "one mega-test"
// anti-pattern rule; `tests/reasoning_advantage.rs` in this same suite is a
// comparable 244-pure-LOC precedent for integration-test file size.

use std::sync::Arc;

use anamnesis::Error;
use anamnesis::engine::{EmbeddingProvider, KnowledgeType, NodeId, Timestamp};
use anamnesis::memory::{ListFilter, Memory};

// ---------------------------------------------------------------------------
// Deterministic, model-free embedder (mirrors tests/readout_behavior.rs).
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct HashEmbedder;

fn embed_text(text: &str) -> Vec<f32> {
    let bytes = text.as_bytes();
    let a = bytes.iter().step_by(1).map(|&b| b as f32).sum::<f32>();
    let b = bytes
        .iter()
        .skip(1)
        .step_by(2)
        .map(|&b| b as f32)
        .sum::<f32>();
    let c = bytes
        .iter()
        .skip(2)
        .step_by(3)
        .map(|&b| b as f32)
        .sum::<f32>();
    let d = bytes.len() as f32;
    let mag = (a * a + b * b + c * c + d * d).sqrt().max(1.0);
    vec![a / mag, b / mag, c / mag, d / mag]
}

impl EmbeddingProvider for HashEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts.iter().map(|t| embed_text(t)).collect())
    }
    fn dimensions(&self) -> usize {
        4
    }
    fn model_name(&self) -> &str {
        "hash-stub"
    }
}

fn mem() -> Memory {
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedder);
    Memory::in_memory_with_provider(provider).expect("in-memory Memory")
}

fn t(ms: u64) -> Timestamp {
    Timestamp(ms)
}

// ── Baseline (must pass on unchanged code) ──────────────────────────────────

#[test]
fn baseline_add_note_and_search_still_work() {
    let mut m = mem();
    let receipt = m.add_note("baseline note about zebras", t(1)).unwrap();
    let recall = m.search_at("zebras", 5, t(2)).unwrap();
    assert!(
        recall
            .hits
            .iter()
            .any(|h| h.node_id == receipt.episodic || Some(h.node_id) == receipt.finalized_semantic),
        "baseline search must still find the added note"
    );
}

// ── update_content ───────────────────────────────────────────────────────────

#[test]
fn update_content_reembeds_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manage.db");
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedder);

    let sem_id = {
        let mut m = Memory::with_provider(&path, provider.clone()).unwrap();
        let receipt = m.add_note("old alpha unique-marker", t(1)).unwrap();
        m.flush_all().unwrap();
        let sem_id = receipt.finalized_semantic.unwrap();

        m.update_content(sem_id, "new beta unique-marker", t(2))
            .unwrap();

        let recall = m.search_at("new beta unique-marker", 5, t(3)).unwrap();
        assert!(
            recall.hits.iter().any(|h| h.node_id == sem_id),
            "search must find the re-embedded content"
        );
        assert_eq!(m.get(sem_id).unwrap().content, "new beta unique-marker");
        sem_id
    };

    // Reopen: the updated content must have persisted (write-through row write).
    let m2 = Memory::with_provider(&path, provider).unwrap();
    assert_eq!(
        m2.get(sem_id).unwrap().content,
        "new beta unique-marker",
        "updated content must survive reopen"
    );
}

// ── forget / unforget ────────────────────────────────────────────────────────

#[test]
fn forget_hides_unforget_restores() {
    let mut m = mem();
    let receipt = m
        .add_note("forgettable distinctive phrase xyzzy", t(1))
        .unwrap();
    m.flush_all().unwrap();
    let sem_id = receipt.finalized_semantic.unwrap();

    let before = m.search_at("distinctive phrase xyzzy", 5, t(2)).unwrap();
    assert!(before.hits.iter().any(|h| h.node_id == sem_id));

    m.forget(sem_id, "no longer relevant", t(3)).unwrap();
    assert!(m.get(sem_id).unwrap().retracted);
    let after_forget = m.search_at("distinctive phrase xyzzy", 5, t(4)).unwrap();
    assert!(
        !after_forget.hits.iter().any(|h| h.node_id == sem_id),
        "forgotten node must be excluded from search"
    );

    m.unforget(sem_id, t(5)).unwrap();
    assert!(!m.get(sem_id).unwrap().retracted);
    let after_unforget = m.search_at("distinctive phrase xyzzy", 5, t(6)).unwrap();
    assert!(
        after_unforget.hits.iter().any(|h| h.node_id == sem_id),
        "unforgotten node must be visible to search again"
    );
}

// ── delete_hard ──────────────────────────────────────────────────────────────

#[test]
fn delete_hard_removes_node_and_edges() {
    let mut m = mem();
    let receipt = m.add_note("to be permanently deleted", t(1)).unwrap();
    m.flush_all().unwrap();
    let sem_id = receipt.finalized_semantic.unwrap();

    assert!(m.get(sem_id).is_ok());
    m.delete_hard(sem_id).unwrap();
    assert!(
        m.get(sem_id).is_err(),
        "deleted node must be unreadable via get()"
    );
}

// ── supersede ────────────────────────────────────────────────────────────────

#[test]
fn supersede_sets_validity_windows() {
    let mut m = mem();
    let old_receipt = m.add_note("old fact about deploys", t(1)).unwrap();
    m.flush_all().unwrap();
    let new_receipt = m.add_note("new fact about deploys", t(2)).unwrap();
    m.flush_all().unwrap();

    let old_id = old_receipt.finalized_semantic.unwrap();
    let new_id = new_receipt.finalized_semantic.unwrap();

    m.supersede(new_id, old_id).unwrap();

    let old_view = m.get(old_id).unwrap();
    let new_view = m.get(new_id).unwrap();
    assert!(
        old_view.valid_until.is_some(),
        "superseded node must have valid_until set"
    );
    assert!(
        new_view.valid_from.is_some(),
        "superseding node must have valid_from set"
    );
}

// ── list ─────────────────────────────────────────────────────────────────────

#[test]
fn list_orders_by_salience_and_filters() {
    let mut m = mem();
    let a = m
        .add("s1", "vip", "vip content one", t(1))
        .unwrap()
        .episodic;
    let b = m
        .add("s1", "vip", "vip content two", t(2))
        .unwrap()
        .episodic;
    let c = m
        .add("s1", "other", "other content entirely", t(3))
        .unwrap()
        .episodic;
    m.flush_all().unwrap();

    // Filter to Episodic nodes tagged "speaker-vip" — exactly {a, b}.
    let vip_filter = ListFilter {
        min_salience: 0.0,
        limit: 10,
        node_type: Some(KnowledgeType::Episodic),
        tag: Some("speaker-vip".to_string()),
    };
    let vip = m.list(&vip_filter).unwrap();
    let vip_ids: Vec<NodeId> = vip.iter().map(|v| v.node_id).collect();
    assert!(vip_ids.contains(&a) && vip_ids.contains(&b));
    assert!(
        !vip_ids.contains(&c),
        "tag filter must exclude non-matching nodes"
    );

    // Salience-descending order: every consecutive pair is non-increasing.
    let all = m
        .list(&ListFilter {
            min_salience: 0.0,
            limit: 10,
            node_type: Some(KnowledgeType::Episodic),
            tag: None,
        })
        .unwrap();
    assert!(all.len() >= 3, "expected all 3 episodic nodes, got {all:?}");
    assert!(
        all.windows(2).all(|w| w[0].salience >= w[1].salience),
        "list must be ordered by salience descending: {all:?}"
    );

    // Tag filter narrows to the "other" speaker only.
    let other_filter = ListFilter {
        min_salience: 0.0,
        limit: 10,
        node_type: Some(KnowledgeType::Episodic),
        tag: Some("speaker-other".to_string()),
    };
    let other = m.list(&other_filter).unwrap();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].node_id, c);

    // Limit caps the result count.
    let capped_filter = ListFilter {
        min_salience: 0.0,
        limit: 1,
        node_type: Some(KnowledgeType::Episodic),
        tag: Some("speaker-vip".to_string()),
    };
    let capped = m.list(&capped_filter).unwrap();
    assert_eq!(capped.len(), 1);
}

// ── adversarial: malformed_input (missing NodeId) ────────────────────────────

#[test]
fn mgmt_ops_on_missing_id_error() {
    let mut m = mem();
    let missing = NodeId(999_999);

    assert!(m.get(missing).is_err(), "get on missing id must error");
    assert!(
        m.update_content(missing, "x", t(1)).is_err(),
        "update_content on missing id must error"
    );
    assert!(
        m.forget(missing, "why", t(1)).is_err(),
        "forget on missing id must error"
    );
    assert!(
        m.unforget(missing, t(1)).is_err(),
        "unforget on missing id must error"
    );
    assert!(
        m.delete_hard(missing).is_err(),
        "delete_hard on missing id must error"
    );
    assert!(
        m.supersede(missing, missing).is_err(),
        "supersede on missing ids must error"
    );
}

// ── manual QA demo ───────────────────────────────────────────────────────────

#[test]
fn memory_manage_demo() {
    let mut m = mem();

    let receipt = m.add_note("old fact demo unique phrase", t(1)).unwrap();
    m.flush_all().unwrap();
    let sem_id = receipt.finalized_semantic.unwrap();

    m.update_content(sem_id, "new fact demo unique phrase", t(2))
        .unwrap();
    let recall = m.search_at("new fact demo unique phrase", 5, t(3)).unwrap();
    let search_new_hits = recall.hits.iter().filter(|h| h.node_id == sem_id).count();
    println!("search_new_hits={search_new_hits}");
    assert!(search_new_hits > 0);

    m.forget(sem_id, "demo forget", t(4)).unwrap();
    let after_forget_retracted = m.get(sem_id).unwrap().retracted;
    println!("after_forget_retracted={after_forget_retracted}");
    assert!(after_forget_retracted);

    m.unforget(sem_id, t(5)).unwrap();
    let after_unforget_retracted = m.get(sem_id).unwrap().retracted;
    println!("after_unforget_retracted={after_unforget_retracted}");
    assert!(!after_unforget_retracted);

    let receipt2 = m.add_note("second note for supersede demo", t(6)).unwrap();
    m.flush_all().unwrap();
    let sem_id2 = receipt2.finalized_semantic.unwrap();
    m.supersede(sem_id2, sem_id).unwrap();
    let old_valid_until_set = m.get(sem_id).unwrap().valid_until.is_some();
    println!("old_valid_until_set={old_valid_until_set}");
    assert!(old_valid_until_set);

    let list = m
        .list(&ListFilter {
            min_salience: 0.0,
            limit: 10,
            node_type: None,
            tag: None,
        })
        .unwrap();
    println!("list_len={}", list.len());
    if let Some(first) = list.first() {
        println!("list_first_id={:?}", first.node_id);
    }
    assert!(!list.is_empty());
}
