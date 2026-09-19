# Experimental Node ingest runtime

This is an ingest/recovery application, not a production memory service. No
service manager, recall, policy, extraction, embeddings, or commit pipeline is
installed or advertised.

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

## G005 authority boundary

Backup/restore remain fail-closed at the authenticated RPC boundary. `Runtime`
accepts a lifecycle-owned `TrustedAuthorityAdapter` injection, but the current
RPC methods do not carry the required owner/epoch/path/manifest authority
inputs, and the core has no authority snapshot API for member, generation,
coverage, physical-link, invalidation, or source evidence. Therefore the daemon
retains typed `backup_adapter_unavailable`/`restore_adapter_unavailable`
refusals and never fabricates an archive or marker. The adapter implementation
still pins the exact Neo4j image digest and validates container ownership before
offline commands; activation must wait for the missing authority API.

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
