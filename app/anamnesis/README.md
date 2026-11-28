# anamnesis daemon and ops CLI

This directory holds the anamnesis daemon and its operator CLI
(`anamnesis-ops`). The daemon serves the remember, recall, policy and commit
RPCs; extraction and embedding capabilities reflect configured providers.
Production runs it under the systemd units in `deploy/vm/`, which start
`ops foreground` ([deploy](../../docs/deploy.md)). `ops up` starts the local
`managed` supervisor instead. Don't supervise one root both ways.

## Optional providers

Set `ANAMNESIS_LLM_BASE_URL` and `ANAMNESIS_LLM_API_KEY_FILE` (private JSON
containing a `bearer` string) to enable extraction. The default model is
`claude-haiku-4-5`. `ANAMNESIS_LLM_DIALECT` is `anthropic_messages` for models
starting with `claude`, otherwise `openai_chat`; either can be explicitly selected.
`ANAMNESIS_EXTRACTION_PROMPT_FILE` overrides the bundled extraction prompt.
`ANAMNESIS_IDENTITY_PROMPT_FILE` overrides the bundled identity-judge prompt.
`ANAMNESIS_IDENTITY_OWNER_ACCOUNTS` lists owner accounts as comma-separated `source:actor` entries.
See [extraction audit](extraction-audit.md) for the provider and ABI semantics.

`ANAMNESIS_EXTRACTION_LANE=off` keeps the LLM provider configured for other
runtime lanes but does not create or wake the extraction scheduler. In this
mode `capabilities.extraction` is false and `workers.extraction` reports
`{"state":"unconfigured"}`. The default is `on`.

`ANAMNESIS_EXTRACTION_GATE=jev` enables the pre-extraction gate (D56): the
scheduler scores each claim window with `typesafe-ai/jev` through the Vercel AI
Gateway and skips windows whose best score is below
`ANAMNESIS_EXTRACTION_GATE_THRESHOLD` (default `0.2`). The gate reads its own
private JSON key file from `ANAMNESIS_JEV_GATE_KEY_FILE` (default
`~/.config/anamnesis/jev-vercel-gateway.key.json`); the model id is fixed and the
bearer is never logged. The default is `off`.

`ANAMNESIS_EXTRACTION_SCOPE_FILE` limits which Episodes are extracted. The file
is JSON, `{"sources": ["omo", ...], "rooms": [{"source": "kakao", "room": "<id>"}]}`:
every Episode of a listed source is extracted, plus the listed rooms of other
sources (a room also covers its thread sessions `room/thread`). Any other Episode
is covered without a provider call as an `out_of_scope` cancellation, which is
final for the generation, so list a room before extraction reaches its Episodes.
The file is read at startup; without it every Episode is extracted.
Task identity is a stable SHA-256 of dialect/endpoint/model/prompt configuration,
not an attestation of upstream weights. The reported model identity is tracked
separately; a change within a provider instance fails with `provider_mismatch`.

Embeddings are optional and disabled without configuration. Set
`ANAMNESIS_EMBEDDING_BASE_URL` to enable the OpenAI embedding adapter, optionally
with `ANAMNESIS_EMBEDDING_MODEL`, `ANAMNESIS_EMBEDDING_DIMENSIONS`, and
`ANAMNESIS_EMBEDDING_API_KEY`. Defaults are `Qwen3-Embedding-0.6B` and 1024
dimensions. Its profile identity hashes endpoint/model/dimensions, not server
weights; change configuration when changing weights. Token-hub has no embedding
route, so leave this endpoint unset there. Legacy `ANAMNESIS_EMBEDDING_CONFIG`
and `ANAMNESIS_EXTRACTION_CONFIG` remain supported; explicit base URLs take
precedence. Both `hello` and `status` report the selected provider capabilities.

## Run

Build with the repository's pinned Bun 1.4.1, then execute with Node 24+:

```sh
bun run build:runtime
export ANAMNESIS_RUNTIME_ROOT=/tmp/my-anamnesis
export ANAMNESIS_NEO4J_URI=bolt://127.0.0.1:7687
export ANAMNESIS_NEO4J_PASSWORD='your-database-password'
node dist/anamnesis-ops.mjs foreground
# In another terminal, with the same root:
node dist/anamnesis-ops.mjs status
node dist/anamnesis-ops.mjs verify
```

`ANAMNESIS_NEO4J_USER` and `ANAMNESIS_NEO4J_DATABASE` default to `neo4j`.
`ANAMNESIS_RUNTIME_TOKEN` optionally seeds the first installation token. Without
it, a random token is generated. A later conflicting token is rejected. Root is
0700; token, lease, object files, and socket are private. Keep the root path short
enough for a portable Unix-domain socket (103 UTF-8 bytes including socket name).

`verify` checks authenticated admission health and filesystem permissions. It is
explicitly **not** a full database integrity audit. An unavailable database makes
`verify` exit nonzero; `status` remains available and reports degraded storage.

The built `anamnesis-client.mjs` exports `RpcClient`. Connect with the installation
socket path and token, then use protocol-validated `request(method, params)`.
Requests/responses are JSON with a four-byte unsigned big-endian byte length.
The client has no Bolt connection or Bun runtime dependency.

## Admission and recovery

- A unique process owner protects each root. Confirmed live PIDs are never
  displaced. A dead owner can be reclaimed; incomplete owner/recovery metadata
  requires operator inspection rather than guessed ownership.
- The daemon initializes `Engine`, claims its database writer epoch, and uses
  `Engine.remember` for every database write. It never reclaims an epoch after
  another writer has displaced it.
- Object chunks stream to connection-owned temporary files with sequence and
  byte reservations. `object.commit` uses the existing `ObjectStore` API (which requires
  one bounded whole-object buffer), verifies the hash, and fsyncs data/metadata.
- A single bounded serial queue governs admission. Raw frame lengths, connection
  count, queued requests, uploads, reserved upload bytes, and response buffering
  are bounded. Unsupported future RPC methods return explicit errors.
- A `remember` reply for a revision that is already stored is built from the
  row read back from Neo4j, with its real ID and ingest sequence. The retry
  must match the RPC-body digest, which binds fields such as mass that the
  core digest does not cover, and the stored core digest must verify. Matching
  only a revision key never makes a retry successful. These replies are not
  recall receipts.
- `remember` and `remember.batch` write directly to Neo4j during the request.
  If storage is unavailable, the failed write is not accepted; the client
  receives retryable `storage_unavailable` and retries the same input. In a
  batch, an already committed prefix is retained; later items are not tried.
  An exact retry of a committed revision returns `created: false`, while a
  different body under the same key returns `revision_conflict`.
- Startup, authenticated `status`, and `remember` probe the DB. A bounded
  background probe resumes the identity, embedding, and extraction workers
  after storage recovery.
- Startup ignores an absent or empty legacy `spool/` and refuses a nonempty
  one, naming its path and preserving it. The daemon has no spool, no delivery
  reservations and no `ingest.status` RPC. An absent or empty `deliveries/` is
  fine; leftover `<sha256>.json` files there were pre-commit reservations,
  never acknowledgements, and startup removes them with a
  `legacy_deliveries_cleaned` log event.
- SIGTERM/SIGINT and authenticated `shutdown` stop admission, finish accepted
  work, close clients/drivers, remove uploads/socket, and release ownership.

## Backup and restore

Offline backup and restore are exposed through the authenticated `backup` and
`restore` RPCs and the ops commands `backup <destination-dir>` and
`restore <archive-dir>`. The lifecycle must inject an `OwnedNeo4jAdapter` for
these operations; without one the runtime refuses rather than fabricating an
archive. The adapter accepts only the pinned Neo4j image and an explicitly
labelled owner container. Archive completion markers and member SHA-256 values
are verified before restore admission. A live daemon refuses offline backup
with `daemon_live`; stop it with `down` before invoking an offline lifecycle.
`backup.status` and `restore.status` retain the operation state.

`up` requires an extraction provider (`ANAMNESIS_LLM_BASE_URL` plus its private
key file) and exits 2 with `{"error":"extraction_provider_required"}` when
absent. Embeddings remain optional; `embed` reports a disabled no-op when no
embedding endpoint is configured. `extract`, `embed`, and `recall <query>` are
explicit foreground operations and print one JSON result.

## Loading a cleaned corpus

`node dist/anamnesis-ops.mjs load <clean-dir> <source> <state-dir>` sends one source of a cleaned corpus
(`_rooms.jsonl` plus one JSONL file per room, one record per message) as `remember.batch` calls of up to 32
records, rooms in parallel and each room in file order, with a line cursor per room file under `<state-dir>/<source>/` so a re-run sends
only what was not sent. Options, the record format and the output lines are in
[deploy](../../docs/deploy.md#loading-a-cleaned-corpus). `ANAMNESIS_RUNTIME_SOCKET` optionally overrides the
socket used by ops (including a transport relay).

## Managed foreground mode

`node dist/anamnesis-ops.mjs managed` runs a foreground supervisor with at most
three automatic restarts after unexpected child termination. It never signals an
unrelated PID. SIGINT/SIGTERM stop and await its owned daemon; a clean RPC shutdown
ends supervision. It is a bounded local supervisor, not an unbounded restart loop.
Under systemd use `foreground`: `deploy/vm/anamnesis.service` runs
`ops foreground` with `Restart=always`.

The four recovery scenarios use owned, isolated Neo4j and real built Node UDS:

```sh
bun run build:runtime
bun scripts/qa/runtime-scenarios.ts --case uds-ingest --evidence-root /tmp/g002-evidence
# Other cases: object-upload-crash, storage-retry, managed-ingest-restart. Run database cases serially on small Docker VMs.
```

## Live extraction acceptance (explicit opt-in)

```sh
bun run build:runtime
bun scripts/qa/e2e-real.ts
# Equivalent registry entry; never included in the default test suite:
bun scripts/qa/runtime-scenarios.ts --case e2e-real --evidence-root .omo/evidence/runtime-complete/g4
```

This local QA script requires Docker and SSH access to `inonono`. It fetches the
Haiku token-hub credential into a private temporary file, opens its own tunnel,
and uses only a newly owner-labelled Neo4j container and temporary runtime root.
Embeddings stay off unless `ANAMNESIS_QA_EMBEDDINGS=1`. It copies a Claude
project's transcripts read-only and deterministically converts their text turns
into 200 Episodes with direct lineage. `ANAMNESIS_E2E_FALLBACK_PROJECT` can
select the Claude project directory. Source rules/counts, provider errors, recall,
crash recovery, and cleanup receipts are saved under the evidence directory.
No credential or evidence file belongs in a commit.
`ANAMNESIS_QA_LLM_MIN_INTERVAL_MS` controls the minimum provider-call interval
(default 3000 ms, range 0-60000). Pacing occurs before task leases and applies to
claims, judges and retries; attempts remain bounded at four. Run live QA serially.
That variable only affects `scripts/qa/e2e-real.ts`. The daemon paces its own
extraction lane through three knobs read at startup with the LLM configuration:
`ANAMNESIS_EXTRACTION_MAX_IN_FLIGHT` bounds concurrent pipelines (1-16, default 4);
`ANAMNESIS_LLM_MIN_INTERVAL_MS` is the minimum spacing between consecutive provider
calls (0-600000, default 0 = unpaced), enforced by one FIFO gate that every claim,
judge and retry passes through; `ANAMNESIS_LLM_JITTER_FRACTION` (0-1, default 0.5)
stretches each gap by up to that fraction so calls never line up into bursts on a
shared token hub. `status` reports the effective values and per-process
`calls_total` / `waited_total_ms` under `workers.extraction.pacing`.
`ANAMNESIS_QA_KEEP_ON_FAILURE=1` retains the owned database and temporary runtime
on failure for post-mortem inspection, recording custody in `kept-resources.json`.
It still deletes the token-hub credential and stops the tunnel. Manually remove
only that recorded owner-labelled container (`docker rm -f -v`) and its temporary
root afterward, and append the cleanup receipt. Success always cleans up.

In this script extraction uses the Engine pipeline after daemon shutdown
(`ops extract` only reports capability readiness).
Recall records full-text/top-20 query results, one-based minimum Fact ranks and
Fact-hit coverage; contrast companions are included under the same policy and
output budget as their primary Facts. Since `verify` reports admission health rather
than graph counts, read-only database snapshots supplement its RPC output.

## Verification and limits

```sh
bun run build:runtime
bun run typecheck
bun run test:runtime
bun run test:runtime:surface
```

The surface harness uses real built Node processes and a uniquely labeled Neo4j
container. A test-only TCP relay keeps the endpoint stable when Docker reallocates
its ephemeral published port across restart. Readiness uses process/socket events
with bounded deadlines, not sleeps. The harness removes its container/volumes,
processes, sockets, relay, and temporary roots.

The process tests (`process.test.mjs`) cover malformed JSON, unknown methods,
authentication, refusal while storage is unavailable, legacy `spool/` and
`deliveries/` handling at startup, version mismatch, split and oversized
frames, invalid UTF-8, uploads, and shutdown.

Committed objects are not garbage-collected by this runtime.
