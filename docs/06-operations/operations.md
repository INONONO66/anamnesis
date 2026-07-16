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

The plugin exposes six MCP tools. The hooks drive capture and recall on their own;
these are the moves the agent makes deliberately.

| Tool | When | What it does |
|:--|:--|:--|
| `recall` | Before answering, whenever prior context could matter | Read-only spreading-activation recall. Reading reinforces the memories returned when `reinforce_on_recall` is left at its server default (`ANAMNESIS_REINFORCE` unset ⇒ on); a later `recall` / `relate` over the same nodes is the "it helped" signal. |
| `remember` | Right after a decision, convention, or lesson worth keeping | Writes a single durable memory. This is the on-demand path; passive capture handles the raw transcript separately. |
| `relate` | To record *why* — the edge between two recalled nodes | Adds a typed reasoning edge (`causes` / `contradicts` / `supports` / …) between node ids surfaced by a prior `recall`. This is what makes why-chains traceable instead of a flat list. |
| `ingest_conversation` | Bulk import of an external transcript | One-shot import of turns you already have. **Not the capture path** — the hooks capture live sessions; use this only to seed history. |
| `extract_pending` | When the SessionStart nudge appears | Pulls the accumulated raw turns, distills them into reasoning and lessons, and emits `relate` / `remember` promptly. The nudge only fires once the backlog crosses the threshold; act on it in the same session so the pulled batch is not abandoned (see [Failure & recovery](#failure--recovery-semantics)). |
| `stats` | To check health and dogfood usage | Reports graph health plus the per-daemon **usage** section (recalls / remembers / relates, `extraction backlog`, `captured total`, `stale ratio (14d)`). The presence of that usage section is also how you tell a current daemon from an old one (see [Daemon lifecycle](#daemon-lifecycle--version-skew)). |

## Automatic capture lifecycle

Capture runs without the agent asking, in two stages, and is designed so a raw
turn is never lost:

```text
Stop (≤8-turn recent window, each turn)  ┐
PreCompact (tail before compaction)      ├─► content-hash dedup ─► un-extracted queue
SessionEnd (Claude Code only)            ┘         (idempotent)          │
                                                                         │  len ≥ ANAMNESIS_EXTRACT_THRESHOLD_N (20)
                                                                         ▼
                                                          SessionStart injects a one-line nudge
                                                                         │
                                                                         ▼
                                                          agent calls extract_pending → relate / remember
```

- **Stage 1 (passive).** The `Stop` hook streams a small recent window (≤8 turns)
  every turn; `PreCompact` flushes the tail before the context window is compacted;
  `SessionEnd` is the Claude-Code-only backstop. Each turn is written as a raw
  `Episodic` memory, **content-hash-deduped** in the daemon so the overlap between
  successive `Stop` windows collapses to one row.
- **Queue + threshold.** Un-extracted turns accumulate in a queue. Once its length
  reaches `ANAMNESIS_EXTRACT_THRESHOLD_N` (default **20**), the next `SessionStart`
  injects the extraction nudge.
- **Stage 2 (agent-driven).** The agent calls `extract_pending`, which hands back
  the raw turns to distill into reasoning (`relate`) and lessons (`remember`).

## R2 shadow extraction (opt-in)

R2 extraction is a separate, **shadow-only** path for auditing prospective distilled memories.
It is off by default. Raw captured content leaves the machine only when
`ANAMNESIS_EXTRACT_MODE=shadow` is set exactly; `off`, `auto`, boolean-like values, and every
other unrecognized value disable extraction. `ANAMNESIS_EXTRACT_CMD` configures exactly one
provider command (default argv `claude -p`), shell-word parsed and executed directly: no shell
and no fallback command.

Run one pass manually with `anamnesis extract [--namespace NS]`. A pass selects one temporal
session-and-scope group with **10–20** eligible turns and sends at most that one batch to the
configured extractor. The provider has a **120 s** timeout; stdout and stderr each have their
own **1 MiB** cap. Failure, timeout, malformed output, or an over-limit stream leaves its
sources eligible for a later pass. A valid empty (`items=[]`) result is different: it records
the selected sources in the zero-output ledger, so they are not sent again.

Groups with fewer than 10 turns remain permanently unprocessed in R2. This intentionally biases
shadow audit samples toward longer sessions; account for that bias in an R3 promotion decision.
Age-based flushing is deferred to R3 and must be reconsidered only if measurements demonstrate
that the bias warrants it.

Stage-1 raw capture remains in the graph as `Episodic` memories. Provider stdin/the raw source
batch, raw stdout/stderr, and the raw command are transient and are not persisted or logged by
R2 policy or error records. The policy side schema persists only extractor profile
hash/components, run and failure scalars, validated candidates and relations, the source
identity/hash ledger, and audit labels. R2 performs no automatic pruning or cleanup: those rows
persist until an operator takes a database lifecycle action.

A successful R2 pass stages candidates, relations, run metadata, and source ledger records in
the policy side schema only. It never changes graph nodes, graph edges, or
`anamnesis:extracted` metadata. Staged candidates are invisible to recall until R3 commits an
approved result.

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

A source marked `source-unavailable` no longer has its recorded node. A
`source-mismatch` source still resolves by node id but no longer matches its recorded turn key,
session, scope, or content hash. Candidate review updates are rejected while any cited source is
unavailable or mismatched; restore the authoritative source before reviewing it.

## Failure & recovery semantics

The capture and recall paths are **fail-open**: a missing binary, an unreachable
daemon, or a slow model never blocks or erases a prompt — the hook degrades to a
silent no-op and the agent proceeds. Concretely:

- **Hooks never block.** Recall injection is skipped rather than delayed past its
  timeout; capture that cannot reach the daemon is dropped for that turn, not
  retried inline. A prompt is always delivered unmodified.
- **Raw Episodic always survives.** Whatever else fails, the passively-captured raw
  turns persist as `Episodic` memories; distillation is best-effort on top of a
  durable transcript, never a precondition for keeping it.
- **Pulled-but-abandoned extractions are redelivered once.** When `extract_pending`
  hands out a batch, those turns are marked `pending:<epoch-ms>:<attempt>` *before*
  they leave the in-memory queue. If the agent never emits its distillation (session
  died, nudge ignored), the mark ages. At the **next daemon start**, marks older
  than `ANAMNESIS_EXTRACT_REDELIVERY_MS` (default **21_600_000 ms = 6h**) with
  attempts remaining are re-queued and delivered **one** more time. The attempt cap
  is **2** total deliveries (`EXTRACT_MAX_PULL_ATTEMPTS`); on the final attempt the
  turn is marked done regardless, so a permanently-abandoned batch cannot loop
  forever.

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

The telemetry side schema is optional. A future side-schema version, or a policy-store open,
write, or query failure, disables or degrades telemetry only. It must never block core recall; the
hook retains its fail-open contract and still delivers the user's prompt (with no injected context
when recall itself cannot complete). The dispatch regression tests own the open/write/query
fail-open assertions; the procedure below records a reproducible write-failure observation only.

### Pre-plugin-reactivation evidence gate

Plugin reactivation remains blocked until this fail-closed procedure succeeds against a disposable
database. It creates one note through production CLI paths, gives its Episodic copy a valid
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
export ANAMNESIS_NAMESPACE="r1-rollout"
export ANAMNESIS_DAEMON_GRACE_SECS=0
export ANAMNESIS_REINFORCE=false
export ANAMNESIS_HOOK_THRESHOLD=13.0
export ANAMNESIS_HOOK_COSINE_GATE=0.86
NS_DB="$ANAMNESIS_DB"
NOTE="R1 rollout marker semantic recall must select this exact note"

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

anamnesis recall "R1 rollout marker semantic recall" --limit 2 \
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

printf '{"hook_event_name":"UserPromptSubmit","prompt":"R1 rollout marker semantic recall","cwd":"%s"}\n' \
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

printf '{"hook_event_name":"UserPromptSubmit","prompt":"R1 rollout marker semantic recall","cwd":"%s"}\n' \
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

A real external `UserPromptSubmit` activation with the installed plugin remains pending. Do not
reactivate the plugin from this deterministic procedure or regression output alone; collect and
review that external activation evidence separately.

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
- **Workaround (until #86 lands).** Kill the stale `anamnesis daemon` process; the
  next client respawns its own, current version. The daemon is disposable — killing
  it loses no data (the DB is on disk).
- **Codex-specific.** Freshly installed plugin hooks are **silently skipped until the
  plugin is interactively trusted** in Codex — capture and recall look inert until
  you trust it once ([#87](https://github.com/INONONO66/anamnesis/issues/87)).

Transport selection is separate from the knobs below: `ANAMNESIS_NO_DAEMON=1` (or
`--embedded`) bypasses the daemon and opens the DB in-process, and `ANAMNESIS_SOCKET`
overrides the socket path when the default is too long for the platform.

## Embedding-space migration

An embedding dimension or model mismatch is a database compatibility problem, not a
recall-quality warning. The preferred recovery path is:

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
| `ANAMNESIS_REINFORCE` | `true` | Auto-reinforce the package returned by `recall`; `0` / `false` / `no` disables. |
| `ANAMNESIS_HOOK_THRESHOLD` | `13.0` | `τ` — the recall injection gate. A floor on the **top recall score**, which is raw ACT-R activation (~8–16 on a typical graph), **not** a 0..1 similarity; a sub-1 value silently disables the gate. **Recalibrate per graph** — activation magnitude scales with density/recency. |
| `ANAMNESIS_HOOK_TOPK` | `5` | Cap on injected per-turn memories. |
| `ANAMNESIS_HOOK_SEED_K` | `5` | SessionStart seed-recall size. |
| `ANAMNESIS_HOOK_TIMEOUT_MS` | `1500` | Per-hook fail-open timeout (ms). |
| `ANAMNESIS_CAPTURE_ENABLED` | `true` | Global capture kill-switch; `0` / `false` / `no` disables passive capture. |
| `ANAMNESIS_EXTRACT_THRESHOLD_N` | `20` | Un-extracted queue length that triggers the SessionStart extraction nudge. |
| `ANAMNESIS_EXTRACT_REDELIVERY_MS` | `21600000` (6h) | TTL after which a pulled-but-abandoned extraction is re-queued once (attempt cap 2). |
| `ANAMNESIS_EXTRACT_MODE` | `off` | R2 mode: only exact `shadow` permits external extraction of raw captured content; `auto`, boolean-like, and unrecognized values degrade to off. |
| `ANAMNESIS_EXTRACT_CMD` | `claude -p` | External extractor argv, shell-word parsed and executed without a shell. |
| `ANAMNESIS_DAEMON_GRACE_SECS` | `30` | Idle grace before a zero-client daemon exits; `0` ⇒ exit immediately. |
| `ANAMNESIS_EMBED_MODEL` | `multilingual-e5-small` | Embedding model. Set it to the known stored model to continue without migrating. |
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
