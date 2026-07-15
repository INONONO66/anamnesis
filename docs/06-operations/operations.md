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
