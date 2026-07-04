//! Reasoning-capture pipeline for the default-namespace turn stream.
//!
//! Owns the dedup/extraction bookkeeping that turns captured conversational turns
//! into a durable, at-least-once-with-a-cap delivery queue for downstream reasoning
//! extraction: the turn-dedup hash, the `anamnesis:extracted` state machine, the
//! redelivery TTL, and the `pull_pending` / `extraction_status` /
//! `load_extraction_state` operations on [`MemoryRegistry`].
//!
//! This is a move-only split from `memory.rs`; the registry keeps its
//! `seen_turn_keys` / `unextracted` fields and the capture-branch of
//! `ingest_conversation` (which reuses [`turn_key`] and the `META_*` keys here).

use anamnesis::Memory;
use anamnesis::graph::{NodeId, Timestamp};
use anamnesis::storage::SqliteStorage;

use crate::memory::MemoryRegistry;

/// Metadata key: the stable dedup hash stored on each captured episodic node.
pub(crate) const META_TURN_KEY: &str = "anamnesis:turn_key";
/// Metadata key: extraction state of a captured episodic node.
///
/// Values: `"false"` (queued, never pulled) → `"pending:<epoch-ms>:<attempt>"`
/// (pulled; awaiting the agent's relate/remember) → `"true"` (done/exhausted).
/// A pull marks `pending` durably BEFORE the turn leaves the in-memory queue;
/// an abandoned pull (agent crashed before emitting) is re-queued at daemon
/// start once [`extract_redelivery_ms`] elapses, up to
/// [`EXTRACT_MAX_PULL_ATTEMPTS`] total deliveries — after that it is `"true"`.
pub(crate) const META_EXTRACTED: &str = "anamnesis:extracted";

/// Total deliveries a captured turn gets before it is considered extracted.
/// 2 = one normal pull + one redelivery after an abandoned pull.
const EXTRACT_MAX_PULL_ATTEMPTS: u32 = 2;

/// Default redelivery TTL: an abandoned `pending` pull re-queues after 6h.
/// Long enough that a live agent mid-extraction is never redelivered into a
/// concurrent session (duplicate `relate` calls would create duplicate edges).
const DEFAULT_EXTRACT_REDELIVERY_MS: u64 = 21_600_000;

/// Default batch cap when `extract_pending` is called without a limit.
/// `pub(crate)` so `crate::dispatch`'s `PullPending` phase-1 claim uses the
/// SAME default this module's own `pull_pending` uses.
pub(crate) const DEFAULT_PULL_LIMIT: usize = 50;

/// `ANAMNESIS_EXTRACT_REDELIVERY_MS` env override for the redelivery TTL.
pub(crate) fn extract_redelivery_ms() -> u64 {
    std::env::var("ANAMNESIS_EXTRACT_REDELIVERY_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_EXTRACT_REDELIVERY_MS)
}

/// Parsed [`META_EXTRACTED`] state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtractedState {
    /// `"false"` — queued, never pulled.
    New,
    /// `"pending:<epoch-ms>:<attempt>"` — pulled, awaiting the agent's output.
    Pending { at_ms: u64, attempt: u32 },
    /// `"true"` (or unrecognized — conservative: never re-queue garbage).
    Done,
}

fn parse_extracted_state(v: Option<&str>) -> ExtractedState {
    match v {
        Some("false") => ExtractedState::New,
        Some(s) => {
            if let Some(rest) = s.strip_prefix("pending:")
                && let Some((ts, attempt)) = rest.split_once(':')
                && let (Ok(at_ms), Ok(attempt)) = (ts.parse::<u64>(), attempt.parse::<u32>())
            {
                return ExtractedState::Pending { at_ms, attempt };
            }
            ExtractedState::Done
        }
        None => ExtractedState::Done,
    }
}

/// Stable dedup key for a captured turn: lowercase hex Sha256 of
/// `session \0 speaker \0 text \0 at_ms`. Sha256 (not DefaultHasher) because the
/// key is persisted in `node.metadata` and re-derived after daemon restarts /
/// rebuilds — it must be identical across runs and toolchain versions.
pub(crate) fn turn_key(session: &str, speaker: &str, text: &str, at_ms: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(session.as_bytes());
    h.update([0u8]);
    h.update(speaker.as_bytes());
    h.update([0u8]);
    h.update(text.as_bytes());
    h.update([0u8]);
    h.update(at_ms.to_le_bytes());
    format!("{:x}", h.finalize())
}

impl MemoryRegistry {
    /// Deliver up to `limit` (default [`DEFAULT_PULL_LIMIT`]) un-extracted turns
    /// as a JSON array `[{"node_id":N,"content":"..."}]` for the agent.
    ///
    /// **At-least-once with a cap**: each delivered turn is durably marked
    /// `pending:<now>:<attempt>` BEFORE it leaves the in-memory queue — a failed
    /// mark leaves the turn queued (delivered ⟺ marked, never lost to a write
    /// error), and an abandoned pull re-queues at daemon start after the TTL.
    /// On the final allowed attempt the turn is marked `"true"` and delivered
    /// one last time. Raw episodic nodes always survive regardless (fail-open).
    ///
    /// **The un-extracted queue is per-namespace**, keyed by
    /// [`MemoryRegistry::canonical_ns_key`]: `ns` selects WHICH namespace's
    /// queue is drained and marked, consistent with `extraction_status` and
    /// the capture-enqueue side (`ingest_conversation`).
    ///
    /// `crate::dispatch`'s `PullPending` arm claims from `self.unextracted`
    /// (the requested namespace's bucket) and calls [`pull_claimed`] directly
    /// (its own phase split), so in a non-test build this convenience wrapper
    /// has only test consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pull_pending(
        &mut self,
        limit: Option<usize>,
        ns: Option<&str>,
    ) -> Result<String, anamnesis::Error> {
        self.ops.extraction_pulls += 1;
        self.pull_pending_at(limit, ns, Timestamp::now().0)
    }

    /// [`pull_pending`](Self::pull_pending) with an injected clock (testable).
    ///
    /// Two-phase discipline: resolve `ns`'s canonical key, claim (drain) up to
    /// `limit` ids from the front of `self.unextracted[key]` FIRST — a brief,
    /// registry-state-only mutation — then resolve THAT SAME namespace's
    /// handle and do the actual metadata get/set work while holding ONLY the
    /// namespace lock. Any claimed ids [`pull_claimed`] didn't durably mark (a
    /// write failure, or the handle failing to resolve at all) are spliced
    /// back onto the FRONT of that namespace's queue, preserving both the
    /// original FIFO order and the "everything unmarked stays queued" contract
    /// — and, unlike the single-lock predecessor, claiming ELIMINATES the
    /// possibility of two concurrent pulls delivering the same node twice.
    fn pull_pending_at(
        &mut self,
        limit: Option<usize>,
        ns: Option<&str>,
        now_ms: u64,
    ) -> Result<String, anamnesis::Error> {
        let ns_key = self.canonical_ns_key(ns);
        // Resolve the handle FIRST: on a namespace's first open this rebuilds
        // its `unextracted` bucket from durable node metadata (see
        // `MemoryRegistry::namespace_handle`), so the claim below sees the
        // rebuilt queue rather than the empty post-restart default.
        let handle = self.namespace_handle(ns)?;
        let take = limit
            .unwrap_or(DEFAULT_PULL_LIMIT)
            .min(self.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0));
        let claimed: Vec<NodeId> = self
            .unextracted
            .get_mut(&ns_key)
            .map(|q| q.drain(..take).collect())
            .unwrap_or_default();
        let (items, unprocessed) = {
            let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
            pull_claimed(&mut mem, &claimed, now_ms)
        };
        if !unprocessed.is_empty() {
            self.unextracted
                .entry(ns_key)
                .or_default()
                .splice(0..0, unprocessed);
        }
        Ok(serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string()))
    }

    /// Pending-queue size for the hook signal, scoped to `ns`'s own queue
    /// (see [`MemoryRegistry::canonical_ns_key`]) — never a count summed
    /// across every namespace.
    pub fn extraction_status(&mut self, ns: Option<&str>) -> Result<String, anamnesis::Error> {
        // Resolve first: on a namespace's first open this rebuilds its
        // `unextracted` bucket (see `MemoryRegistry::namespace_handle`).
        self.namespace_handle(ns)?;
        let ns_key = self.canonical_ns_key(ns);
        let pending = self.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0);
        Ok(serde_json::json!({ "pending": pending }).to_string())
    }

    /// Number of captured episodic nodes awaiting reasoning extraction in the
    /// DEFAULT namespace. Test accessor — production code uses the field
    /// directly.
    #[cfg(test)]
    pub(crate) fn unextracted_len(&self) -> usize {
        let ns_key = self.canonical_ns_key(None);
        self.unextracted.get(&ns_key).map(Vec::len).unwrap_or(0)
    }

    /// Rebuild the capture indexes (`seen_turn_keys`, `unextracted`) from node
    /// metadata. Called once at daemon startup so the queue + dedup survive
    /// restarts. Idempotent: clears then repopulates.
    ///
    /// Re-queues never-pulled turns (`"false"`) AND abandoned pulls — `pending`
    /// marks older than the redelivery TTL with attempts remaining (an agent
    /// that pulled and crashed before emitting relate/remember).
    pub fn load_extraction_state(&mut self, ns: Option<&str>) -> Result<(), anamnesis::Error> {
        self.load_extraction_state_at(ns, Timestamp::now().0)
    }

    /// [`load_extraction_state`](Self::load_extraction_state) with an injected
    /// clock (testable).
    ///
    /// Resolves the namespace handle (brief global-lock-equivalent access via
    /// `&mut self`), then does the full-graph scan under ONLY the namespace
    /// lock; `self.seen_turn_keys`/`self.unextracted` are written last, once
    /// the namespace lock has already been released. Only `ns`'s own bucket
    /// of `self.unextracted` is replaced — every other namespace's queue is
    /// untouched, so this is safe to call independently per namespace (and,
    /// on upgrade from the old single-global-queue binary, replays exactly
    /// into the DEFAULT namespace's bucket — no data loss).
    fn load_extraction_state_at(
        &mut self,
        ns: Option<&str>,
        now_ms: u64,
    ) -> Result<(), anamnesis::Error> {
        let ttl = extract_redelivery_ms();
        let ns_key = self.canonical_ns_key(ns);
        let handle = self.namespace_handle(ns)?;
        let (keys, pending) = {
            let mem = handle.lock().unwrap_or_else(|p| p.into_inner());
            scan_extraction_state(&mem, now_ms, ttl)
        };
        self.seen_turn_keys.clear();
        self.seen_turn_keys.extend(keys);
        self.unextracted.insert(ns_key, pending);
        Ok(())
    }
}

/// Namespace-locked batch of already-claimed node ids: read each node's
/// content, compute + durably write its next `META_EXTRACTED` mark, and
/// collect the delivered `{node_id, content}` payload. On the first write
/// failure, stop and return the remaining (from that index onward, inclusive)
/// claimed ids as `unprocessed` so the caller can restore them to the queue —
/// "everything delivered is marked, everything unmarked stays queued".
pub(crate) fn pull_claimed(
    mem: &mut Memory<SqliteStorage>,
    claimed: &[NodeId],
    now_ms: u64,
) -> (Vec<serde_json::Value>, Vec<NodeId>) {
    let mut items = Vec::with_capacity(claimed.len());
    for (i, &id) in claimed.iter().enumerate() {
        let (content, state) = match mem.engine().graph().get_node(id) {
            Ok(n) => (
                n.content.clone(),
                parse_extracted_state(n.metadata.get(META_EXTRACTED).map(String::as_str)),
            ),
            Err(_) => (String::new(), ExtractedState::New),
        };
        let attempt = match state {
            ExtractedState::Pending { attempt, .. } => attempt + 1,
            _ => 1,
        };
        let mark = if attempt >= EXTRACT_MAX_PULL_ATTEMPTS {
            "true".to_string()
        } else {
            format!("pending:{now_ms}:{attempt}")
        };
        if mem.set_metadata(id, META_EXTRACTED, &mark).is_err() {
            return (items, claimed[i..].to_vec());
        }
        items.push(serde_json::json!({ "node_id": id.0, "content": content }));
    }
    (items, Vec::new())
}

/// Namespace-locked, read-only full-graph scan for `load_extraction_state`:
/// collects every `turn_key` metadata value (for dedup) and every node whose
/// `META_EXTRACTED` state means it belongs back in the un-extracted queue.
pub(crate) fn scan_extraction_state(
    mem: &Memory<SqliteStorage>,
    now_ms: u64,
    ttl: u64,
) -> (Vec<String>, Vec<NodeId>) {
    let graph = mem.engine().graph();
    let mut keys = Vec::new();
    let mut pending = Vec::new();
    for id in graph.all_node_ids() {
        if let Ok(node) = graph.get_node(id) {
            if let Some(k) = node.metadata.get(META_TURN_KEY) {
                keys.push(k.clone());
            }
            match parse_extracted_state(node.metadata.get(META_EXTRACTED).map(String::as_str)) {
                ExtractedState::New => pending.push(id),
                ExtractedState::Pending { at_ms, attempt }
                    if attempt < EXTRACT_MAX_PULL_ATTEMPTS
                        && now_ms.saturating_sub(at_ms) > ttl =>
                {
                    pending.push(id)
                }
                _ => {}
            }
        }
    }
    (keys, pending)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::memory::{MemoryRegistry, StubProvider, Turn};

    fn registry(reinforce: bool) -> MemoryRegistry {
        MemoryRegistry::in_memory_with(Arc::new(StubProvider), reinforce)
    }

    /// Retry `f` while it fails with the exclusive-lock error — the previous
    /// registry in a drop-then-reopen test releases its `<db>.lock` flock
    /// asynchronously under parallel test load. Any other error panics.
    fn retry_while_locked<T>(mut f: impl FnMut() -> Result<T, anamnesis::Error>) -> T {
        for _ in 0..200 {
            match f() {
                Ok(v) => return v,
                Err(e)
                    if e.to_string()
                        .contains("already in use by another anamnesis process") =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("unexpected error while reopening: {e}"),
            }
        }
        panic!("<db>.lock never freed after 200 retries (~2s)");
    }

    #[test]
    fn turn_key_is_deterministic_and_field_sensitive() {
        let a = super::turn_key("s1", "user", "hello", 1000);
        let b = super::turn_key("s1", "user", "hello", 1000);
        assert_eq!(a, b, "same inputs ⇒ same key");
        assert_eq!(a.len(), 64, "sha256 hex is 64 chars");
        assert_ne!(
            a,
            super::turn_key("s1", "user", "hello", 1001),
            "at_ms matters"
        );
        assert_ne!(
            a,
            super::turn_key("s1", "assistant", "hello", 1000),
            "speaker matters"
        );
        assert_ne!(
            a,
            super::turn_key("s2", "user", "hello", 1000),
            "session matters"
        );
        assert_ne!(
            a,
            super::turn_key("s1", "user", "HELLO-DIFFERENT", 1000),
            "text matters"
        );
    }

    #[test]
    fn pull_pending_returns_once_and_marks_extracted() {
        let mut reg = registry(true);
        let turns = vec![
            Turn {
                speaker: "user".into(),
                text: "why x".into(),
                at_ms: Some(10),
            },
            Turn {
                speaker: "assistant".into(),
                text: "because y".into(),
                at_ms: Some(20),
            },
        ];
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        assert_eq!(reg.unextracted_len(), 2);
        let json = reg.pull_pending(None, None).unwrap();
        assert!(json.contains("\"node_id\""), "got: {json}");
        assert!(json.contains("because y"), "content included: {json}");
        assert_eq!(reg.unextracted_len(), 0, "drained");
        // Second pull is empty (pending mark held).
        let json2 = reg.pull_pending(None, None).unwrap();
        assert_eq!(json2.trim(), "[]");
    }

    /// Redelivery: an abandoned pull (pending mark, agent never emitted) is
    /// re-queued after the TTL, delivered ONE more time (attempt cap 2), then
    /// treated as done — never an infinite redelivery loop.
    ///
    /// Extended (P1-T4) to be namespace-scoped: captures into "projA" (not the
    /// default namespace) and asserts a sibling namespace ("projB") stays at
    /// zero backlog throughout — the redelivery machinery must operate on the
    /// REQUESTED namespace's queue, not a single global one.
    #[test]
    fn abandoned_pull_redelivers_once_then_done() {
        let mut reg = registry(true);
        let ns = Some("projA");
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "abandoned insight".into(),
            at_ms: Some(10),
        }];
        reg.ingest_conversation("s", &turns, ns, true).unwrap();
        // A sibling namespace was never captured into — must start empty.
        let sibling_before = reg.extraction_status(Some("projB")).unwrap();
        assert!(
            sibling_before.contains("\"pending\":0"),
            "projB must start empty: {sibling_before}"
        );

        let t0 = 1_000u64;
        let ttl = super::DEFAULT_EXTRACT_REDELIVERY_MS;

        // First pull → pending:t0:1, queue drained.
        let j1 = reg.pull_pending_at(None, ns, t0).unwrap();
        assert!(j1.contains("abandoned insight"), "delivered: {j1}");
        let status = reg.extraction_status(ns).unwrap();
        assert!(status.contains("\"pending\":0"), "drained: {status}");

        // Restart BEFORE the TTL: still pending, NOT re-queued.
        reg.load_extraction_state_at(ns, t0 + 1).unwrap();
        assert!(
            reg.extraction_status(ns).unwrap().contains("\"pending\":0"),
            "fresh pending is not redelivered"
        );

        // Restart AFTER the TTL: abandoned → re-queued once.
        reg.load_extraction_state_at(ns, t0 + ttl + 1).unwrap();
        assert!(
            reg.extraction_status(ns).unwrap().contains("\"pending\":1"),
            "abandoned pull re-queued"
        );

        // Final delivery (attempt 2 = cap) → marked done.
        let j2 = reg.pull_pending_at(None, ns, t0 + ttl + 2).unwrap();
        assert!(j2.contains("abandoned insight"), "redelivered: {j2}");

        // Even long after another TTL, it never comes back.
        reg.load_extraction_state_at(ns, t0 + ttl * 10).unwrap();
        assert!(
            reg.extraction_status(ns).unwrap().contains("\"pending\":0"),
            "attempt cap reached ⇒ done"
        );

        // The sibling namespace was never touched by any of the above.
        let sibling_after = reg.extraction_status(Some("projB")).unwrap();
        assert!(
            sibling_after.contains("\"pending\":0"),
            "projB must remain empty throughout: {sibling_after}"
        );
    }

    /// The pending mark is durable BEFORE the turn leaves the queue: after a
    /// pull + daemon restart, the turn is not double-queued (mark survived).
    #[test]
    fn pull_mark_is_durable_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "durable pending".into(),
            at_ms: Some(7),
        }];
        {
            let mut reg = MemoryRegistry::file_backed_with(
                Arc::new(StubProvider),
                db.clone(),
                dir.path().to_path_buf(),
                "default".into(),
                false,
            );
            reg.ingest_conversation("s", &turns, None, true).unwrap();
            let _ = reg.pull_pending_at(None, None, 5_000).unwrap();
        } // drop → release lock (simulated daemon exit right after the pull)
        let mut reg2 = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            db,
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        // Fresh-pending (within TTL) must NOT re-queue — the mark was durable.
        retry_while_locked(|| reg2.load_extraction_state_at(None, 5_001));
        assert_eq!(reg2.unextracted_len(), 0, "pending mark survived restart");
    }

    /// The `limit` param caps a batch; the remainder stays queued.
    #[test]
    fn pull_pending_respects_limit() {
        let mut reg = registry(true);
        let turns: Vec<Turn> = (0..3)
            .map(|i| Turn {
                speaker: "user".into(),
                text: format!("turn {i}"),
                at_ms: Some(100 + i),
            })
            .collect();
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        assert_eq!(reg.unextracted_len(), 3);
        let j = reg.pull_pending(Some(2), None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2, "batch capped at 2");
        assert_eq!(reg.unextracted_len(), 1, "remainder stays queued");
    }

    /// P1-T4 bug fix: capturing into "projA" and pulling "projB" must return
    /// EMPTY — each namespace's un-extracted queue is isolated from every
    /// other's — and vice-versa. Replaces the old
    /// `pull_pending_ignores_namespace_param` test, which asserted the BUG
    /// this fixes (that `pull_pending` ignores its `ns` argument entirely).
    #[test]
    fn capture_into_ns_then_pull_same_ns_returns_it() {
        let mut reg = registry(true);
        let turn_a = vec![Turn {
            speaker: "user".into(),
            text: "projA-only turn".into(),
            at_ms: Some(1),
        }];
        let turn_b = vec![Turn {
            speaker: "user".into(),
            text: "projB-only turn".into(),
            at_ms: Some(2),
        }];
        reg.ingest_conversation("s", &turn_a, Some("projA"), true)
            .unwrap();
        reg.ingest_conversation("s", &turn_b, Some("projB"), true)
            .unwrap();

        // Pulling projB must return B's turn and must NOT leak A's.
        let json_b = reg.pull_pending(None, Some("projB")).unwrap();
        assert!(json_b.contains("projB-only turn"), "got: {json_b}");
        assert!(
            !json_b.contains("projA-only turn"),
            "leaked A into B's pull: {json_b}"
        );

        // Vice-versa: pulling projA now must return A's turn, not B's.
        let json_a = reg.pull_pending(None, Some("projA")).unwrap();
        assert!(json_a.contains("projA-only turn"), "got: {json_a}");
        assert!(
            !json_a.contains("projB-only turn"),
            "leaked B into A's pull: {json_a}"
        );
    }

    /// Durability gap fix: only the DEFAULT namespace's queue was rebuilt at
    /// daemon startup (`daemon.rs` calls `load_extraction_state(None)`); a
    /// non-default namespace's queue silently stayed empty after a restart
    /// until something explicitly called `load_extraction_state(Some(ns))`.
    /// Captures into two non-default namespaces ("projA", "projB"), drops the
    /// registry (simulating a daemon restart), reopens over the SAME db dir,
    /// and — WITHOUT calling `load_extraction_state` at all — pulls "projB"
    /// and confirms its turn is returned, while "projA" independently reports
    /// its own backlog (isolation survives the restart too).
    #[test]
    fn nondefault_ns_queue_rebuilds_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let turn_a = vec![Turn {
            speaker: "user".into(),
            text: "projA restart turn".into(),
            at_ms: Some(1),
        }];
        let turn_b = vec![Turn {
            speaker: "user".into(),
            text: "projB restart turn".into(),
            at_ms: Some(2),
        }];
        {
            let mut reg = MemoryRegistry::file_backed_with(
                Arc::new(StubProvider),
                db.clone(),
                dir.path().to_path_buf(),
                "default".into(),
                false,
            );
            reg.ingest_conversation("s", &turn_a, Some("projA"), true)
                .unwrap();
            reg.ingest_conversation("s", &turn_b, Some("projB"), true)
                .unwrap();
        } // drop → release lock, simulating a daemon restart

        let mut reg2 = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            db,
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        // No `load_extraction_state` call anywhere — the fix must rebuild
        // "projB"'s queue purely from `namespace_handle`'s first-open scan.
        let json_b = retry_while_locked(|| reg2.pull_pending(None, Some("projB")));
        assert!(
            json_b.contains("projB restart turn"),
            "projB's queue must survive the restart: {json_b}"
        );
        assert!(
            !json_b.contains("projA restart turn"),
            "projA must not leak into projB's pull: {json_b}"
        );

        // projA's own backlog independently rebuilt too.
        let status_a = reg2.extraction_status(Some("projA")).unwrap();
        assert!(
            status_a.contains("\"pending\":1"),
            "projA's queue must survive the restart: {status_a}"
        );
    }

    /// `extraction_status` backlog must reflect only the REQUESTED
    /// namespace's queue, not a global count summed across every namespace.
    #[test]
    fn extraction_status_is_per_namespace() {
        let mut reg = registry(true);
        let turns_a = vec![Turn {
            speaker: "user".into(),
            text: "a1".into(),
            at_ms: Some(1),
        }];
        let turns_b: Vec<Turn> = (0..3)
            .map(|i| Turn {
                speaker: "user".into(),
                text: format!("b{i}"),
                at_ms: Some(10 + i),
            })
            .collect();
        reg.ingest_conversation("s", &turns_a, Some("projA"), true)
            .unwrap();
        reg.ingest_conversation("s", &turns_b, Some("projB"), true)
            .unwrap();

        let status_a = reg.extraction_status(Some("projA")).unwrap();
        assert!(status_a.contains("\"pending\":1"), "got: {status_a}");
        let status_b = reg.extraction_status(Some("projB")).unwrap();
        assert!(status_b.contains("\"pending\":3"), "got: {status_b}");
    }

    /// malformed_input: a namespace name that NEEDS sanitizing (spaces,
    /// punctuation, mixed case) must canonicalize to the SAME key for both
    /// the capture-enqueue and the pull — a raw spelling requiring
    /// sanitization must still see (and drain) its own queue.
    #[test]
    fn odd_namespace_name_canonicalizes_consistently_for_enqueue_and_pull() {
        let dir = tempfile::tempdir().unwrap();
        let default_db = dir.path().join("memory.db");
        let mut reg = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            default_db,
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "needs sanitizing".into(),
            at_ms: Some(1),
        }];
        // Capture under a raw namespace that needs sanitizing (space + punctuation).
        reg.ingest_conversation("s", &turns, Some("Proj A!"), true)
            .unwrap();
        // A DIFFERENT raw spelling sanitizing to the SAME stem must pull it.
        let pulled = reg.pull_pending(None, Some("PROJ A!")).unwrap();
        assert!(pulled.contains("needs sanitizing"), "got: {pulled}");
    }

    /// BACK-COMPAT (stale_state): a queue rebuilt from persisted node metadata
    /// in the DEFAULT namespace's DB — exactly what an upgrade from the old
    /// single-global-queue binary leaves on disk, since the automatic capture
    /// hook always captured with `namespace: None` — must load into the
    /// DEFAULT namespace's bucket (addressable by both `None` and the literal
    /// default name) and remain pullable there. No data loss on upgrade.
    #[test]
    fn back_compat_persisted_queue_loads_into_default_namespace_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "pre-fix persisted turn".into(),
            at_ms: Some(42),
        }];
        {
            let mut reg = MemoryRegistry::file_backed_with(
                Arc::new(StubProvider),
                db.clone(),
                dir.path().to_path_buf(),
                "default".into(),
                false,
            );
            reg.ingest_conversation("s", &turns, None, true).unwrap();
        } // drop → release lock (simulated upgrade: same DB, new binary)
        let mut reg2 = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            db,
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        retry_while_locked(|| reg2.load_extraction_state(None));
        let status = reg2.extraction_status(None).unwrap();
        assert!(
            status.contains("\"pending\":1"),
            "old default-namespace state must rebuild into the default bucket: {status}"
        );
        // Addressable by the default namespace's literal name too.
        let status_named = reg2.extraction_status(Some("default")).unwrap();
        assert!(
            status_named.contains("\"pending\":1"),
            "got: {status_named}"
        );
        // And pullable there — no data loss on upgrade.
        let pulled = reg2.pull_pending(None, Some("default")).unwrap();
        assert!(pulled.contains("pre-fix persisted turn"), "got: {pulled}");
    }

    #[test]
    fn extraction_status_reports_pending() {
        let mut reg = registry(true);
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "z".into(),
            at_ms: Some(99),
        }];
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        let s = reg.extraction_status(None).unwrap();
        assert!(s.contains("\"pending\":1"), "got: {s}");
        // threshold field is intentionally absent — the hook reads only `pending`.
        assert!(
            !s.contains("\"threshold\""),
            "threshold must be absent: {s}"
        );
    }

    #[test]
    fn capture_dedups_identical_turns() {
        let mut reg = registry(true);
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "ship it".into(),
            at_ms: Some(1000),
        }];
        let s1 = reg.ingest_conversation("sess", &turns, None, true).unwrap();
        assert_eq!(s1.episodic, 1, "first capture creates one episodic");
        // Same turn again (multi-hook) ⇒ deduped, no new node.
        let s2 = reg.ingest_conversation("sess", &turns, None, true).unwrap();
        assert_eq!(s2.episodic, 0, "identical turn is skipped");
    }

    #[test]
    fn capture_enqueues_unextracted_but_plain_ingest_does_not() {
        let mut reg = registry(true);
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "a decision".into(),
            at_ms: Some(2000),
        }];
        reg.ingest_conversation("s", &turns, None, false).unwrap();
        assert_eq!(
            reg.unextracted_len(),
            0,
            "non-capture ingest does not enqueue"
        );
        let turns2 = vec![Turn {
            speaker: "user".into(),
            text: "another".into(),
            at_ms: Some(3000),
        }];
        reg.ingest_conversation("s", &turns2, None, true).unwrap();
        assert_eq!(reg.unextracted_len(), 1, "capture ingest enqueues");
    }

    #[test]
    fn extraction_state_rebuilds_from_metadata_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let turns = vec![Turn {
            speaker: "user".into(),
            text: "persist me".into(),
            at_ms: Some(5000),
        }];
        {
            let mut reg = MemoryRegistry::file_backed_with(
                Arc::new(StubProvider),
                db.clone(),
                dir.path().to_path_buf(),
                "default".into(),
                false,
            );
            reg.ingest_conversation("s", &turns, None, true).unwrap();
            assert_eq!(reg.unextracted_len(), 1);
        } // drop → releases lock
        // Fresh registry on the same DB: index empty until loaded.
        let mut reg2 = MemoryRegistry::file_backed_with(
            Arc::new(StubProvider),
            db.clone(),
            dir.path().to_path_buf(),
            "default".into(),
            false,
        );
        assert_eq!(reg2.unextracted_len(), 0, "index empty before load");
        // First fallible op after reopen — retry while the dropped registry's
        // flock release settles (the `unextracted_len` read above never touches
        // the DB, so this `load` is what first takes the `<db>.lock`).
        retry_while_locked(|| reg2.load_extraction_state(None));
        assert_eq!(
            reg2.unextracted_len(),
            1,
            "unextracted rebuilt from metadata"
        );
        // And dedup still holds after reload (same turn is skipped).
        let s = reg2.ingest_conversation("s", &turns, None, true).unwrap();
        assert_eq!(
            s.episodic, 0,
            "turn_key reloaded ⇒ dedup holds across restart"
        );
    }

    #[test]
    fn capture_to_extract_full_loop() {
        let mut reg = registry(true);
        // Stage 1: capture two reasoning-bearing turns.
        let turns = vec![
            Turn {
                speaker: "user".into(),
                text: "deploy failed".into(),
                at_ms: Some(1),
            },
            Turn {
                speaker: "assistant".into(),
                text: "because the disk was full".into(),
                at_ms: Some(2),
            },
        ];
        reg.ingest_conversation("s", &turns, None, true).unwrap();
        // Status reports pending.
        let st = reg.extraction_status(None).unwrap();
        assert!(st.contains("\"pending\":2"));
        // Stage 2: pull drains and marks extracted.
        let pulled = reg.pull_pending(None, None).unwrap();
        assert!(pulled.contains("disk was full"));
        assert_eq!(reg.unextracted_len(), 0);
        // Re-ingesting the same turns (multi-hook) adds nothing (dedup holds).
        let again = reg.ingest_conversation("s", &turns, None, true).unwrap();
        assert_eq!(again.episodic, 0);
        // Queue stays drained (already extracted, deduped).
        assert_eq!(reg.unextracted_len(), 0);
    }
}
