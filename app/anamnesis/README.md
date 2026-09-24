# Experimental Node ingest runtime

This is an experimental ingest/recovery application, not a production memory
service. Recall, policy and commit RPCs are advertised; extraction and embedding
capabilities reflect configured providers. No OS service manager is installed.

## Optional providers

Set `ANAMNESIS_LLM_BASE_URL` and `ANAMNESIS_LLM_API_KEY_FILE` (private JSON
containing a `bearer` string) to enable extraction. The default model is
`claude-haiku-4-5`. `ANAMNESIS_LLM_DIALECT` is `anthropic_messages` for models
starting with `claude`, otherwise `openai_chat`; either can be explicitly selected.
`ANAMNESIS_EXTRACTION_PROMPT_FILE` overrides the bundled extraction prompt.
See [extraction audit](extraction-audit.md) for the audit-only semantics.
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
  byte reservations. Commit uses the existing `ObjectStore` API (which requires
  one bounded whole-object buffer), verifies the hash, and fsyncs data/metadata.
- A single bounded serial queue governs admission. Raw frame lengths, connection
  count, queued requests, uploads, reserved upload bytes, and response buffering
  are bounded. Unsupported future RPC methods return explicit errors.
- Committed receipts are read back from Neo4j with their real ID and ingest
  sequence. A versioned canonical RPC-body digest additionally binds fields such
  as mass that the existing core digest does not cover. Both digests are checked;
  matching only a revision key never makes an UNKNOWN delivery successful.
- Durable binding files preserve delivery identity, data incarnation, predecessor,
  and original filesystem epoch. During database unavailability, ACK follows
  `DurableSpool.append`, directory fsync, and verification of the published entry.
- Startup, authenticated `status`, `remember`, and `ingest.status` probe the DB and
  drain dependency-ready entries. Recovery is **demand-driven**, not a background
  polling service. Use `status` after restoring the DB. Missing predecessors,
  revision conflicts, cycles, and quarantine remain visible rather than being
  marked complete.
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
## Source snapshots and managed foreground mode

`node dist/anamnesis-ops.mjs ingest snapshot.jsonl checkpoint.json` consumes an
immutable UTF-8 JSONL snapshot of `RpcRememberParams` records, limited to 16 MiB
and the protocol's per-record bounds. Use explicit source/revision identities.
Before sending, it durably saves the normalized line and delivery identity in
`<checkpoint>.pending.json`. Only a matching committed receipt advances the
checkpoint's snapshot hash, installation incarnation, next line and last receipt;
pending is retired afterward. A spooled receipt is not source completion.
Re-running reconciles an existing pending identity through `ingest.status`
without resending it. It rejects changed snapshots, foreign incarnations,
unresolved pending work, and receipts that do not match the checkpointed source
line. It does not tail changing logs or
replace the existing backfill format adapters. Keep snapshot/checkpoint paths
private and distinct. `ANAMNESIS_RUNTIME_SOCKET` optionally overrides the socket
used by ops (including a transport relay).

A lost response is `outcome_unknown` with `retryable: false`; no checkpoint
advancement or automatic request retry occurs. Resolve a known delivery identity
with `ingest.status`. A still-unknown pending identity remains available for
operator resolution; restarting the source command does not automatically
retransmit it.
The source checkpoint has its own exclusive process lease; confirmed-dead source
processes are reclaimed using the same conservative PID ownership policy.

`node dist/anamnesis-ops.mjs managed` runs a foreground supervisor with at most
three automatic restarts after unexpected child termination. It never signals an
unrelated PID. SIGINT/SIGTERM stop and await its owned daemon; a clean RPC shutdown
ends supervision. This is not an installed OS service or an unbounded restart loop.

The five recovery scenarios use owned, isolated Neo4j and real built Node UDS:

```sh
bun run build:runtime
bun scripts/qa/runtime-scenarios.ts --case uds-ingest --evidence-root /tmp/g002-evidence
# Other cases: object-spool-crashes, outage-drain-50, source-resume,
# managed-ingest-restart. Run database cases serially on small Docker VMs.
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
Embeddings are deliberately disabled. It copies source transcripts read-only,
tries Codex raw admission, and deterministically converts real Claude text turns
when fewer than 200 Episodes are admitted. `ANAMNESIS_E2E_FALLBACK_PROJECT` can
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

Extraction uses the Engine pipeline after daemon shutdown (`ops extract` only
reports capability readiness). `Engine.digest` acknowledges the original-message
Outbox only after generation coverage/cutover has accepted each extraction outcome.
Recall records full-text/top-20 query results, one-based minimum Fact ranks and
Fact-hit coverage; contrast companions are included under the same policy and
output budget as their primary Facts. Since `verify` reports admission health rather
than graph counts, read-only database snapshots supplement its RPC output.
`scripts/qa/dream-leiden-real.test.ts` requires a Docker VM with at least 3 GiB;
its isolated GDS fixture uses an explicit hostname mapping and JVM heap caps.

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

Evidence is in `.omo/evidence/runtime-recovery/runtime-app-v3`. The initial process
RED caught malformed-JSON and fabricated revision-identity behavior; its initial
authentication assertions already passed. Later process tests additionally cover
version mismatch, split/oversized frames, invalid UTF-8, uploads, and shutdown.

Spool completion advances only through the contiguous committed prefix; later
completed records remain replay-visible behind a gap. Runtime lookups and drain
use finite pages. Marker publication uses synced temporary files, rename and
directory sync, rather than overwriting published markers in place. These
guarantees do not establish a bound on total scan work, request latency, or
runtime status-map memory. The evidence proves process restart and database
outage/recovery, **not hardware power-loss qualification or retention-based
journal reclamation**.

Delivery bindings are retained for exact outcome lookup. They and committed
objects are not garbage-collected by this experimental runtime. Database row
reads are daemon-only and coupled explicitly to the current originals schema;
no new core export or package was added.
