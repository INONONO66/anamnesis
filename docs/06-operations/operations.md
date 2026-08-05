# Operations

The operational contract for running Anamnesis as memory for a coding agent: when
the agent should reach for each tool, how automatic capture flows from a raw turn
to a distilled lesson, what happens when something fails, how the on-demand daemon
lives and dies across plugin upgrades, and every environment knob that tunes it.

This is the runtime SSOT. The values below are current code truth — env names and
defaults live in [`config.rs`](../../crates/anamnesis-mcp/src/config.rs),
[`capture.rs`](../../crates/anamnesis-mcp/src/capture.rs), and
[`daemon.rs`](../../crates/anamnesis-mcp/src/daemon.rs).

## When to use which tool

The plugin exposes twelve MCP tools. Hooks drive proactive capture and recall;
the tools below are deliberate client operations.

| Tool | When | What it does |
|:--|:--|:--|
| `recall` | Before answering, whenever prior context could matter | Runs canonical reranked recall. With server reinforcement enabled, a successful explicit call commits the exact returned package as deliberate use; disabling it keeps the call read-only. Proactive hook recall always requests the non-reinforcing path. |
| `remember` | Right after a decision, convention, or lesson worth keeping | Writes a single durable memory. This is the on-demand path; passive capture handles the raw transcript separately. |
| `relate` | To record *why* — the edge between two recalled nodes | Adds a typed reasoning edge (`causes` / `contradicts` / `supports` / …) between node ids surfaced by a prior `recall`. This is what makes why-chains traceable instead of a flat list. |
| `ingest_conversation` | Bulk import of an external transcript | One-shot import of turns you already have. **Not the capture path** — the hooks capture live sessions; use this only to seed history. |
| `ingest_attachment_transcript` | Admit a textual attachment transcript produced by a consumer | Stores one immutable raw source with attachment hash and processor provenance. The engine does not open files, fetch URLs, or run OCR, vision, document, or embedding models. |
| `extract_pending` | When the SessionStart nudge appears | Returns accumulated raw turns for the connected agent to distill with `relate` / `remember`. The nudge fires after the backlog crosses the threshold; complete the batch in the same session when possible (see [Failure & recovery](#failure--recovery-semantics)). |
| `stats` | To check health and dogfood usage | Reports graph health plus the per-daemon **usage** section (recalls / remembers / relates, `extraction backlog`, `captured total`, `stale ratio (14d)`). The presence of that usage section is also how you tell a current daemon from an old one (see [Daemon lifecycle](#daemon-lifecycle--version-skew)). |
| `update` | When the text of an existing memory is wrong or incomplete | Replaces the selected memory's content while retaining its identity and provenance contract. |
| `forget` | When a memory must no longer participate in recall | Soft-retracts by default or permanently deletes when explicitly requested. |
| `supersede` | When a newer memory replaces an older one | Records the replacement and closes the older memory's validity interval. |
| `list` | To inspect memory inventory without a query cue | Lists memories by salience with optional filters. |
| `get` | To inspect one known node id | Returns that memory's full detail and provenance. |

Explicit MCP recall and proactive hook recall share the same query-aware
`Memory::render_context_for_with` path. `ANAMNESIS_CONTEXT_STYLE` changes only
the layout of the validated package; it does not select a different set of
evidence. The surfaces differ in their reinforcement contract, not their
ranking, selection, or rendering implementation.

## Automatic capture lifecycle

Capture runs without the agent asking, in two best-effort stages:

```text
Stop (≤8-turn recent window)             ┐
PreCompact (tail before compaction)      ├─► content-hash dedup ─► un-extracted queue
SessionEnd (Claude Code only)            ┘         (idempotent)          │
                                                                         │  len ≥ ANAMNESIS_EXTRACT_THRESHOLD_N (20)
                                                                         ▼
                                                          SessionStart injects a one-line nudge
                                                                         │
                                                                         ▼
                                                          agent calls extract_pending → relate / remember
```

- **Stage 1 (passive).** A supported `Stop` event submits a small recent window
  (≤8 turns); `PreCompact` submits the tail before the context window is
  compacted; `SessionEnd` is an additional Claude Code event. Accepted turns
  are written as raw `Episodic` memories and **content-hash-deduplicated** in
  the daemon, so overlapping windows collapse to one row. Host event delivery,
  transcript availability, and daemon reachability are outside this guarantee.
- **Queue + threshold.** Un-extracted turns accumulate in a queue. Once its length
  reaches `ANAMNESIS_EXTRACT_THRESHOLD_N` (default **20**), the next `SessionStart`
  injects the extraction nudge.
- **Stage 2 (agent-driven).** The agent calls `extract_pending`, which hands back
  the raw turns to distill into reasoning (`relate`) and lessons (`remember`).

## Shadow extraction (opt-in)

Shadow extraction is a separate path for auditing prospective distilled memories.
It is off by default. Extraction runs only when `ANAMNESIS_EXTRACT_MODE=shadow` is set exactly;
`off`, `auto`, boolean-like values, and every other unrecognized value disable it.
`ANAMNESIS_EXTRACT_CMD` configures exactly one provider profile. The default
selects local `qwen3.6:35b-a3b`, thinking disabled, and the versioned strict
output schema. That built-in profile calls Ollama's non-streaming chat API over
HTTP loopback, so its output is not mixed with interactive terminal control
sequences. A custom command is shell-word parsed and executed directly with
bounded stdin/stdout: no shell and no fallback command. A custom command may
send content elsewhere; the default cannot.

OMLX can be used without a key or external model service through the bundled
loopback-only adapter:

```bash
export ANAMNESIS_EXTRACT_CMD="python3 /path/to/anamnesis/scripts/run_local_openai_extractor.py --base-url http://127.0.0.1:8000 --model Qwen3.6-35B-A3B-4bit --timeout-secs 600"
```

The adapter rejects non-loopback URLs and emits only the local Qwen response on
stdout. Its OpenAI-compatible wire is an OMLX transport detail; no OpenAI
credential or remote LLM is involved.

Run one pass manually with `anamnesis extract [--namespace NS]`. A pass selects
one temporal session-and-scope group with **1–10** eligible turns and sends at
most that one batch to the configured extractor. The background worker drains
complete ten-source batches and then the final shorter tail. The provider
timeout defaults to **240 s** and can be
boundedly overridden with `ANAMNESIS_EXTRACT_TIMEOUT_SECS=1..3600`; stdout and stderr each have
their own **1 MiB** cap. Invalid JSON or exact-grounding failure receives one
fail-closed retry. If a batch still fails validation after its allowed retry,
or has a non-repairable schema rejection, the worker recursively isolates
deterministic halves: valid partitions are staged through the same product
path, while an irreducible invalid source remains raw and eligible for a later
pass. All branches share the finite partition-recovery invocation budget
derived from the maximum batch size (including grounding retries); exhaustion stops new
calls and preserves the last durably recorded validation error. Provider
failure, timeout, or an over-limit stream does not partition and
leaves the complete affected batch eligible. A valid empty (`items=[]`) result
is different: it records the selected sources in the zero-output ledger, so
they are not sent again.

Stage-1 raw capture remains in the graph as `Episodic` memories. Provider stdin/the raw source
batch, raw stdout/stderr, and the raw command are transient and are not persisted or logged by
the extraction policy or error records. The policy side schema persists only extractor profile
hash/components, run and failure scalars, validated candidates and relations, the source
identity/hash ledger, an engine-owned binding to each exact source allocation, canonical
subject/relation/object fields, exact evidence-object value, live-source byte range and span
hash, and audit labels. It does not copy the raw evidence span:
audit and promotion reconstruct that span from the still-matching authoritative source. Shadow extraction
performs no automatic pruning or cleanup: those rows persist until an operator takes a database
lifecycle action.

Relation audit rows reference their two candidate rows by stable policy primary keys. Review and
promotion therefore revalidate the durable source-allocation bindings of both endpoints rather
than maintaining a second, independently mutable provenance copy.

A successful extraction pass stages candidates, relations, run metadata, and source ledger
records in the policy side schema only. It never changes graph nodes, graph edges, or
`anamnesis:extracted` metadata. Staged candidates remain invisible to recall until an operator
reviews and explicitly promotes one.

### Shadow audit

List staged candidates and relations with their current source evidence:

```bash
anamnesis extract --audit [--namespace NS] [--limit N]
```

Record a candidate review or a relation review without writing graph content:

```bash
anamnesis extract --audit --candidate ID --support partial \
  [--contamination unsupported-claim] [--reviewer NAME] [--namespace NS]
anamnesis extract --audit --relation ID --relation-verdict correct \
  [--reviewer NAME] [--namespace NS]
```

Promote only a reviewed `supported` candidate with no contamination:

```bash
anamnesis extract --promote-candidate ID [--namespace NS]
anamnesis extract --promote-relation ID [--namespace NS]
```

Promotion is additive and idempotent. It creates one record in the isolated atomic-fact sidecar,
stamps the extractor profile and candidate idempotency key, and keeps every cited raw Episodic
source ID as authoritative provenance. It creates no graph node or `ExtractedFrom` edge, does not
enter node FTS, and cannot perturb attraction, forgetting, or the ordinary graph candidate pool.
The sidecar stores the canonical claim plus the evidence source/range/hash metadata, not a copied
raw evidence span. A richer canonical-plus-live-evidence surface may be used once to compute its
embedding and is not persisted.
The sidecar record inherits the latest cited source observation time; the review/promotion wall
clock never manufactures recency. Query-time routing may use the compact fact, but only its live,
scope-valid raw sources can enter the production reranker/context path. The promotion response
returns `atomic_fact_id`; `node_id` remains a compatibility alias for older clients.
Unsupported, partial, contaminated, unreviewed, unavailable, or source-mismatched candidates are
rejected. A relation can be
promoted only after a `correct` review and after both endpoint candidates have been promoted;
promotion writes a dedicated typed `reason`, `causal`, `contradicts`, or `supports` sidecar
record with reviewer, review profile, review time, scope, validity, and an idempotency key.
Free-form fact metadata never grants relation authority. It returns `atomic_relation_id`;
`edge_id` remains a compatibility alias and does not identify a graph edge.

A source marked `source-unavailable` no longer has its recorded node. A
`source-mismatch` source resolves to a live node but no longer matches its recorded allocation,
turn key, session, scope, or content hash. Candidate review, relation review, and promotion are
rejected while any cited source is unavailable or mismatched. Deleting and recreating a node does
not restore the prior allocation, even when its numeric id and public fields are identical.
Policy rows created before allocation bindings were introduced remain listable for audit but fail
closed for review and promotion.

## Failure & recovery semantics

The capture and recall paths are **fail-open**: a missing binary, an unreachable
daemon, or a slow model never blocks or erases a prompt — the hook degrades to a
silent no-op and the agent proceeds. Concretely:

- **Hooks never block.** Recall injection is skipped rather than delayed past its
  timeout; capture that cannot reach the daemon is dropped for that turn, not
  retried inline. A prompt is always delivered unmodified.
- **Persisted sources survive downstream failure.** Once accepted by the
  daemon, a raw turn remains an `Episodic` source even when extraction,
  validation, review, or promotion fails. A turn that never reaches the daemon
  is not reconstructed by this guarantee.
- **Pulled-but-abandoned extractions are redelivered once.** When `extract_pending`
  hands out a batch, those turns are marked `pending:<epoch-ms>:<attempt>` *before*
  they leave the in-memory queue. If the agent never emits its distillation (session
  died, nudge ignored), the mark ages. At the **next daemon start**, marks older
  than `ANAMNESIS_EXTRACT_REDELIVERY_MS` (default **21_600_000 ms = 6h**) with
  attempts remaining are re-queued and delivered **one** more time. The attempt cap
  is **2** total deliveries (`EXTRACT_MAX_PULL_ATTEMPTS`); on the final attempt the
  turn is marked done regardless, so a permanently-abandoned batch cannot loop
  forever.

### Evidence-catalog recovery contract

The catalog proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
uses raw sources as the recovery root:

- a failed validation or catalog write leaves the source unchanged and
  retrievable;
- a source revision update, retraction, deletion, scope change, or validity
  change atomically makes dependent records ineligible before they can be
  served;
- rebuild replays a named formation profile against immutable source revisions and
  writes through the normal admission transaction;
- routing-only records remain isolated during rebuild and never substitute for
  missing raw evidence;
- index rebuild is generation-stamped and becomes visible atomically; and
- rollback restores the prior eligible catalog generation without changing
  graph ids or source bytes.

Operators inspect formation audit counts, stale-derived ratio, provenance
coverage, and catalog generation before enabling a rebuilt index.

## Recall telemetry rollout gate

Recall telemetry is a privacy-minimized side schema, not a record of prompt content. It never
stores a raw query, transcript, or rendered context. Each row contains only recall metadata:
event kind and provenance, namespace/scope, `query_chars`, knowledge-only state, the filtered top
score/cosine, gate settings, result node ids/counts, and the four gate booleans `has_hits`,
`readout_pass`, `cosine_pass`, and `eligible`. Retention keeps the newest **10,000** rows.

Run `anamnesis stats --recall` against the same database and namespace as the daemon. Its counts,
abstentions, threshold sweep, cosine percentiles, and auto-exposure ratios measure **injection
eligibility, not delivery or quality**: they cannot establish that a client rendered context, that
an agent used it, or that an answer improved. The ordinary `stats` command omits this section.

The telemetry side schema is optional and version-gated. An unsupported schema
version, or a policy-store open, write, or query failure, disables or degrades telemetry only. It must never block core recall; the
hook retains its fail-open contract and still delivers the user's prompt (with no injected context
when recall itself cannot complete). The dispatch regression tests own the open/write/query
fail-open assertions; the procedure below records a reproducible write-failure observation only.

### Hook activation evidence procedure

Run this fail-closed procedure against a disposable database before enabling
automatic hook injection in a new environment. It creates one note through production CLI paths, gives its Episodic copy a valid
authoritative retained-action advantage, proves the unfiltered top, restores the same pre-observation
snapshot, and then proves the hook's knowledge-only filter persists the Semantic copy.

Run the following as one script. Setup, unfiltered recall, and final stats use `--embedded`, so those
processes release the database lock synchronously. Hook calls still exercise the real daemon path;
`wait_for_db_lock` prevents direct SQLite access or snapshot replacement until daemon shutdown has
released `<db>.lock`.

```bash
set -euo pipefail

RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/anamnesis-recall-rollout.XXXXXX")"
export ANAMNESIS_DB="$RUN_DIR/memory.db"
export ANAMNESIS_NAMESPACE="recall-verification"
export ANAMNESIS_DAEMON_GRACE_SECS=0
export ANAMNESIS_REINFORCE=false
export ANAMNESIS_HOOK_THRESHOLD=13.0
export ANAMNESIS_HOOK_COSINE_GATE=0.86
NS_DB="$ANAMNESIS_DB"
NOTE="Recall verification marker must select this exact note"

cleanup_failed_gate() {
  sqlite3 "$NS_DB" 'DROP TRIGGER IF EXISTS recall_events_force_insert_failure;' \
    >/dev/null 2>&1 || true
  rm -rf "$RUN_DIR"
}
trap cleanup_failed_gate EXIT

wait_for_db_lock() {
  python3 - "$NS_DB.lock" <<'PY'
import fcntl
import pathlib
import sys
import time

lock_path = pathlib.Path(sys.argv[1])
deadline = time.monotonic() + 15.0
with lock_path.open("a+b") as lock_file:
    while True:
        try:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
            break
        except BlockingIOError:
            if time.monotonic() >= deadline:
                raise SystemExit(f"timed out waiting for {lock_path}")
            time.sleep(0.05)
PY
}

EPISODIC_ID="$(
  anamnesis remember "$NOTE" --namespace "$ANAMNESIS_NAMESPACE" --embedded |
    python3 -c 'import re,sys
text=sys.stdin.read()
match=re.fullmatch(r"stored node ([0-9]+)\n?", text)
assert match, text
print(match.group(1))'
)"
SEMANTIC_ID="$(
  sqlite3 "$NS_DB" \
    "SELECT id FROM nodes WHERE node_type='semantic' AND content='$NOTE' ORDER BY id LIMIT 1"
)"
test -n "$EPISODIC_ID"
test -n "$SEMANTIC_ID"
test "$EPISODIC_ID" != "$SEMANTIC_ID"

# retained_action is authoritative; salience is its bounded logistic projection.
# The capture marker is the production knowledge-only exclusion predicate.
sqlite3 "$NS_DB" \
  "UPDATE retained_action SET value=20.0 WHERE node_id=$EPISODIC_ID;
   UPDATE salience SET salience=0.9999999979388463 WHERE node_id=$EPISODIC_ID;
   UPDATE nodes SET metadata='capture' || char(9) || 'true' WHERE id=$EPISODIC_ID;"
anamnesis stats --recall --namespace "$ANAMNESIS_NAMESPACE" --embedded >/dev/null
cp "$NS_DB" "$RUN_DIR/rank-baseline.db"

anamnesis recall "Recall verification marker" --limit 2 \
  --namespace "$ANAMNESIS_NAMESPACE" --embedded | tee "$RUN_DIR/unfiltered-recall.txt"
RAW_TOP_ID="$(
  python3 - "$RUN_DIR/unfiltered-recall.txt" <<'PY'
import json
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text()
nodes = json.loads(text.split("## NODES (for `relate`)\n", 1)[1])
assert nodes, text
print(nodes[0]["node_id"])
PY
)"
test "$RAW_TOP_ID" = "$EPISODIC_ID"
test "$(sqlite3 "$NS_DB" "SELECT node_type FROM nodes WHERE id=$RAW_TOP_ID")" = "episodic"

# Give successful and forced-failure hook calls the identical pre-observation graph state.
cp "$RUN_DIR/rank-baseline.db" "$NS_DB"
CONTROL_ROWS="$(sqlite3 "$NS_DB" 'SELECT COUNT(*) FROM recall_events')"

printf '{"hook_event_name":"UserPromptSubmit","prompt":"Recall verification marker","cwd":"%s"}\n' \
  "$RUN_DIR" | anamnesis hook user-prompt | tee "$RUN_DIR/hook-success.json"
wait_for_db_lock

SUCCESS_ROWS="$(sqlite3 "$NS_DB" 'SELECT COUNT(*) FROM recall_events')"
test "$SUCCESS_ROWS" -eq "$((CONTROL_ROWS + 1))"
FILTERED_TOP_ID="$(
  sqlite3 "$NS_DB" \
    "SELECT json_extract(result_node_ids, '\$[0]')
     FROM recall_events WHERE event_kind='user-prompt' ORDER BY id DESC LIMIT 1"
)"
test "$FILTERED_TOP_ID" = "$SEMANTIC_ID"
test "$(sqlite3 "$NS_DB" "SELECT node_type FROM nodes WHERE id=$FILTERED_TOP_ID")" = "semantic"

sqlite3 "$NS_DB" '.schema recall_events' | tee "$RUN_DIR/recall-events-schema.txt"
sqlite3 -header -column "$NS_DB" \
  'SELECT id, at_ms, namespace, event_kind, query_chars, scope, knowledge_only,
          has_hits, readout_pass, cosine_pass, eligible, top_score, top_cosine,
          gate_threshold, cosine_gate, result_node_ids, auto_extract_node_count
   FROM recall_events ORDER BY id DESC LIMIT 5;' |
  tee "$RUN_DIR/success-rows.txt"
cp "$NS_DB" "$RUN_DIR/success.db"

cp "$RUN_DIR/rank-baseline.db" "$NS_DB"
sqlite3 "$NS_DB" <<'SQL'
CREATE TRIGGER recall_events_force_insert_failure
BEFORE INSERT ON recall_events
BEGIN
  SELECT RAISE(FAIL, 'forced telemetry insert failure');
END;
SQL

printf '{"hook_event_name":"UserPromptSubmit","prompt":"Recall verification marker","cwd":"%s"}\n' \
  "$RUN_DIR" | anamnesis hook user-prompt | tee "$RUN_DIR/hook-failure.json"
wait_for_db_lock

FAILURE_ROWS="$(sqlite3 "$NS_DB" 'SELECT COUNT(*) FROM recall_events')"
test "$FAILURE_ROWS" -eq "$CONTROL_ROWS"
diff -u "$RUN_DIR/hook-success.json" "$RUN_DIR/hook-failure.json"
sqlite3 "$NS_DB" 'DROP TRIGGER IF EXISTS recall_events_force_insert_failure;'

cp "$RUN_DIR/success.db" "$NS_DB"
anamnesis stats --recall --namespace "$ANAMNESIS_NAMESPACE" --embedded |
  tee "$RUN_DIR/recall-stats.txt"

# Success: retain the evidence directory for review instead of running the failure cleanup trap.
trap - EXIT
printf 'Recall telemetry evidence retained at %s\n' "$RUN_DIR"
```

The hook command is a local entrypoint simulation, not evidence that Claude Code delivered a real
external `UserPromptSubmit`. Retain the unfiltered and filtered ids/types, schema, successful row,
forced-failure zero-row delta, byte-identical hook output, 11-point sweep, cosine p50/p90/p95 with
sample/NULL counts, and both auto-exposure ratios. The schema must contain `query_chars` but no
raw-query, transcript, or rendered-context column. Label all metrics as eligibility/exposure rather
than delivery or quality.

The trigger proves only telemetry-write fail-open; the dispatch regression suite owns the causal
migration/open and stats-query failure evidence. After reviewing or copying the retained evidence,
remove it with:

```bash
rm -rf "$RUN_DIR"
```

This deterministic procedure does not establish delivery by an external hook
host. Record external activation as a separate integration test and do not mix
its result with entrypoint-simulation evidence.

## Daemon lifecycle & version skew

Anamnesis runs an **on-demand daemon per database**. A client (a plugin hook, an
MCP `serve` adapter, the CLI) spawns it on first use and connects over a local
socket; the client processes are thin proxies, the daemon owns the DB. When the
last client disconnects, the daemon waits out an idle grace period —
`ANAMNESIS_DAEMON_GRACE_SECS` (default **30s**; `0` ⇒ exit as soon as the last
client leaves) — and then exits. The next client respawns a fresh one.

**Version skew is the sharp edge.** A long-lived `serve` adapter (an MCP client that
stays connected for the whole session) keeps its daemon alive, and that daemon keeps
running the binary version it was spawned from. If you upgrade the plugin mid-session,
the **old daemon stays up** and an old daemon **silently ignores request fields it
does not understand** — so a newer client's capture request can degrade to a plain
ingest without any error. This was observed in the field:
[#86](https://github.com/INONONO66/anamnesis/issues/86).

- **Detection.** An old daemon's `stats` output **lacks the usage section**
  (`extraction backlog` / `captured total` / `stale ratio` absent). Cross-check
  `anamnesis --version` against the running `anamnesis daemon` process.
- **Recovery.** Stop the stale `anamnesis daemon` process after a binary upgrade;
  the next client respawns the current version. The daemon is disposable and the
  durable database remains on disk.
- **Codex-specific.** Freshly installed plugin hooks are **silently skipped until the
  plugin is interactively trusted** in Codex — capture and recall look inert until
  you trust it once ([#87](https://github.com/INONONO66/anamnesis/issues/87)).

Transport selection is separate from the knobs below: `ANAMNESIS_NO_DAEMON=1` (or
`--embedded`) bypasses the daemon and opens the DB in-process, and `ANAMNESIS_SOCKET`
overrides the socket path when the default is too long for the platform.

## Embedding-space migration

An embedding dimension or model mismatch is a database compatibility problem, not a
recall-quality warning. The preferred recovery path is:
> **Upgrade warning:** Non-plugin installs (`npm install -g` or `cargo install`) of binaries **<=0.17.0** lack the model guard and can mix 768-dimensional embeddings into a migrated database; remove those installs before upgrading.

```text
anamnesis migrate-embeddings [--namespace NS]
```

Run the command while the daemon is stopped: it must own the selected namespace
database lock for the entire operation. Select the namespace explicitly with
`--namespace NS`, or omit it to use the configured default namespace and its normal
database-path rules. Confirm free disk space for a complete SQLite backup before
starting. The migration derives its required backup name from the live database path
and the local date: `<db>.bak-YYYYMMDD` (for example,
`memory.db.bak-20260715`).

### Automatic migration, availability, and resume

When the daemon finds a mismatch, it creates and verifies the required backup, runs
embedding replacements in background batches, then reopens the namespace through
the normal compatibility guard. If initial backup creation or verification fails,
migration does not start and no migration writes occur. On resume, the daemon
re-verifies the durable checkpoint backup before starting a new batch. A resume
verification failure stops new writes for that attempt, but leaves the durable
checkpoint, prior committed batches, and live partial state intact.

While a namespace is migrating, MCP operations for that namespace return an error.
Hook recall follows its existing fail-open behavior and injects no context for that
request. Other namespaces remain available.

After an interruption, rerun the manual command or restart the daemon to resume.
For a target with a different dimension, candidates are selected from the stored
per-node dimensions (including missing embeddings). For a same-dimension model
replacement, the migration resumes from its committed checkpoint cursor rather than
treating matching dimensions as complete.

### Recovery and configuration

Keep the verified `<db>.bak-YYYYMMDD` backup. When a migration fails, stop the
daemon, preserve the failed live database at a separate path for diagnosis, and only
then restore the backup to the live database path.

To disable only automatic daemon migration, set
`ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS` to `0`, `false`, or `no` before starting the
process. The mismatch remains an actionable error; this opt-out performs no database
mutation:

```bash
export ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS=0
```

When the stored model is known and migration is not wanted, use it as the non-migrating fallback:

```bash
export ANAMNESIS_EMBED_MODEL=<stored-model>
```

## Env knobs

Every value below is verified against source. Defaults apply when the variable is
unset or unparseable (parsing is fail-soft — a garbage value falls back to the
default, never an error).

| Variable | Default | Effect |
|:--|:--|:--|
| `ANAMNESIS_DB` | `<data_dir>/anamnesis/memory.db` (project `.anamnesis/` if found, else `~/.anamnesis/memory.db`) | SQLite file for the default namespace. |
| `ANAMNESIS_NAMESPACE` | `default` | Namespace used when a call omits one. |
| `ANAMNESIS_REINFORCE` | `true` | Commit the package returned by an explicit MCP/CLI `recall` as deliberate use; `0` / `false` / `no` keeps that call read-only. Proactive hook recall is non-reinforcing regardless of this default. |
| `ANAMNESIS_CONTEXT_STYLE` | `detailed` | Recall context wire. Exact `evidence` keeps the same validated package and commit trace but renders compact evidence grouped by source session and ordered by observation time; any other value uses the full diagnostic layout. |
| `ANAMNESIS_HOOK_THRESHOLD` | `13.0` | `τ` — the recall injection gate. A floor on the **top recall score**, which is raw ACT-R activation (~8–16 on a typical graph), **not** a 0..1 similarity; a sub-1 value silently disables the gate. **Recalibrate per graph** — activation magnitude scales with density/recency. |
| `ANAMNESIS_HOOK_TOPK` | `20` | Cap on injected per-turn memories. |
| `ANAMNESIS_HOOK_SEED_K` | `5` | SessionStart seed-recall size. |
| `ANAMNESIS_HOOK_TIMEOUT_MS` | `4000` | Per-hook fail-open timeout (ms) for latency-sensitive product recall. |
| `ANAMNESIS_CAPTURE_ENABLED` | `true` | Global capture kill-switch; `0` / `false` / `no` disables passive capture. |
| `ANAMNESIS_EXTRACT_THRESHOLD_N` | `20` | Un-extracted queue length that triggers the SessionStart extraction nudge. |
| `ANAMNESIS_EXTRACT_REDELIVERY_MS` | `21600000` (6h) | TTL after which a pulled-but-abandoned extraction is re-queued once (attempt cap 2). |
| `ANAMNESIS_EXTRACT_MODE` | `off` | Only exact `shadow` permits configured extraction of raw captured content; `auto`, boolean-like, and unrecognized values degrade to off. |
| `ANAMNESIS_EXTRACT_CMD` | built-in local Qwen 3.6 structured profile | The built-in profile uses non-streaming Ollama HTTP on loopback. An explicitly configured non-Ollama argv is shell-word parsed and executed without a shell. |
| `ANAMNESIS_DAEMON_GRACE_SECS` | `30` | Idle grace before a zero-client daemon exits; `0` ⇒ exit immediately. |
| `ANAMNESIS_EMBED_MODEL` | `multilingual-e5-small` | Embedding model. Set it to the known stored model to continue without migrating. |
| `ANAMNESIS_RERANK_MODEL` | `BAAI/bge-reranker-base` | Local cross-encoder used by canonical reranked recall. |
| `ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS` | `true` | Enables daemon migration after a model/dimension mismatch; `0` / `false` / `no` disables it without mutating the DB. |

## Troubleshooting

- **Recall/capture went silent after killing the daemon.** A `serve` adapter does
  **not** reconnect on its own after its daemon is killed — the session's MCP
  connection dies with it. **Restart the session**; the next client respawns a
  current daemon.
- **First run is slow / recall is empty at first.** With `feature = "embed"`, the
  embedding model (~100–500 MB) downloads in the background starting at
  `SessionStart`. Recall quality is degraded until the download completes; it is a
  one-time cost cached under `~/.anamnesis/models` (`FASTEMBED_CACHE_DIR`).
- **A plugin update did not take effect.** Updating requires a marketplace pull
  **and** a session restart — and, per [version skew](#daemon-lifecycle--version-skew),
  killing any stale daemon that a long-lived `serve` adapter kept alive.
