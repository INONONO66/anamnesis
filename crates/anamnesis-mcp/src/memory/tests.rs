use super::*;
use anamnesis::memory::{ListFilter, NoteOptions, Relation};

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
        .hits
        .first()
        .map(|h| h.score)
        .expect("a relevant hit exists");
    let tau = top + 1.0; // strictly above the best score ⇒ gate trips.

    let gated = reg
        .recall_packaged_gated("auth race condition", 5, None, Some(false), Some(tau), None)
        .unwrap();
    assert!(
        gated.context.is_empty(),
        "above-τ gate must yield empty context, got:\n{}",
        gated.context
    );
    assert!(
        gated.hits.is_empty(),
        "above-τ gate must yield no hits, got {} hits",
        gated.hits.len()
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
    assert!(gated.context.is_empty());
    assert!(gated.hits.is_empty());
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
    assert!(!gated.hits.is_empty(), "τ=0.0 must admit the relevant hit");
    assert!(
        gated.context.contains("##"),
        "expected a rendered section header, got:\n{}",
        gated.context
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
        gated.context.is_empty(),
        "above-cosine gate must yield empty context, got:\n{}",
        gated.context
    );
    assert!(
        gated.hits.is_empty(),
        "above-cosine gate must yield no hits, got {} hits",
        gated.hits.len()
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

    assert!(out.context.contains("## KNOWLEDGE"));
    assert!(
        !out.context.contains("## MEMORIES"),
        "episodic section must be dropped:\n{}",
        out.context
    );
    assert!(!out.context.contains("captured conversation window"));
    assert!(
        out.hits
            .iter()
            .all(|h| !h.text.contains("captured conversation window")),
        "capture hits must be dropped: {:?}",
        out.hits
    );
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
        out.context.is_empty(),
        "excluded remote hits must not open the cosine gate:\n{}",
        out.context
    );
    assert!(
        out.hits.is_empty(),
        "filtered below-gate hits must not be returned: {:?}",
        out.hits
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
    assert!(!gated.hits.is_empty());
    assert!(gated.context.contains("##"));
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
            !pkg.hits.is_empty(),
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
        assert!(pkg.hits.is_empty(), "gate must trip at τ=1e9");
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
