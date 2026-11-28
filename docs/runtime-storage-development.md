# Runtime storage development

This page is a developer note on storage tests and the storage contract in
`packages/core`. It isn't a production qualification. The contracts are in
docs/00 to 10, the daemon is described in the
[app README](../app/anamnesis/README.md), and GitHub issue #186 tracks
remaining work.

## Test isolation

Use Bun 1.4.1 to install the version-2 lockfile:

```sh
bun install --frozen-lockfile
bun run typecheck
bun run test
```

`bun run test` executes the owning foundation harness. It creates a uniquely
named, labeled Neo4j container with fresh credentials and an ephemeral
loopback port, waits for its exact startup event, checks authenticated
connectivity, and injects that endpoint into the test subprocess.
It removes only its owned container and volumes in cleanup.

Evidence uses a fresh directory beneath the supplied evidence root and records
commands, the secret-redacted endpoint, raw output, process results, and cleanup.
The default test selection is `packages`, `scripts/qa`,
`app/anamnesis/runtime.test.ts` and `app/anamnesis/runtime-authority.test.ts`.
Individual integration files must also be run through that environment:

```sh
bun scripts/qa/runtime-scenarios.ts --case foundation \
  --evidence-root .omo/evidence/storage \
  --test-path packages/core/src/storage-contract.test.ts
```

The old standalone `scripts/ensure-test-db.ts` refuses to start a database.
It cannot safely transfer process ownership or environment to another command.
Do not reuse the historical shared port 7688 or an existing user container.
Harness failure and cleanup failure are not successful test runs.

## Revisions and digests

`Engine.remember` accepts an optional `expected_previous_revision_key`.
Explicit null pins the first revision; a hash pins its expected predecessor.
A conflicting head rejects with `stale_revision`. An already stored revision
is checked for identical content before a current-head comparison, so retries
of older revisions remain idempotent. Omitting the field retains the legacy
append interface in `Engine`; the daemon's `remember` RPC passes the field
through from the request.

New writes carry `digest_format: rfc8785-v1`. Unmarked stored digests retain
legacy insertion-order serialization. Retrying a legacy record is verified
using that record's format; unknown formats fail closed. No existing digest
or original identity is rewritten merely by initialization or verification.
The current verifier is not a general importer for malformed historical data;
the full raw legacy inventory/decoder remains a separate roadmap gate.

## Session topology

Chronological insertion uses `(time_utc, ingest_seq)` and repairs the cached
predecessor/successor links, including backdated and equal-time records.
Explicit `previous` record links remain a legacy compatibility behavior,
separate from revision predecessor CAS. Newly stored topology metadata permits
verification and reconstruction; rebuilding unmarked historical topology
refuses rather than guessing missing parent information.

Only NEXT_EPISODE cache links are rebuilt. Original Episodes and revision
history are preserved.

## Numeric diagnostics

Pure PPR now evaluates its fixed-point residual rather than returning a
constant. It reports actual update count, total probability, and raw retained
role-weight row totals (`rowSums`, zero for dangling rows). These row totals
are denominators, not normalized stochastic-row sums.

Role-only weights, uniform dangling redistribution, sorted accumulation,
finite options, duplicate-node rejection, and independently calculated small
graphs are covered by tests. This is not a GDS execution result or evidence
that graph retrieval improves real tasks. Online recall runs this solver
over at most 1,024 seed-adjacent arcs; the 2,000-node / 20,000-link envelope
and the same-envelope GDS gate are roadmap targets
([09-roadmap](09-roadmap.md#where-the-code-is-today)).
