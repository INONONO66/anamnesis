# 08 — Repo Layout, Build & Release

## Language: TypeScript only

The code is TypeScript only.

- Storage, indexes and pattern matching are Neo4j's job. The numeric work the
  engine does itself (local PPR, forgetting, RRF) is size-bounded and runs on
  `Float64Array`. Everything else is orchestration and I/O.
- One toolchain, no contract codegen across languages, no native build matrix.
- If a real numeric bottleneck appears, only `packages/core/src/dynamics/` is
  swapped for a native implementation behind its interface. That seam is the
  one the code protects.

## Monorepo (bun workspace)

```text
anamnesis/
├── package.json                # bun workspace root, gate and test scripts
├── packages/
│   ├── protocol/               # zod contract (source of truth) + JSON Schema export
│   │   ├── src/                # element, link, rpc, extraction, lineage, materialization ...
│   │   ├── schemas/*.schema.json   # exported artifacts (committed)
│   │   └── scripts/export-schemas.ts
│   └── core/                   # Neo4j store, write path, recall, extraction, embedding, identity, objects
│       └── src/dynamics/       # pure functions: retention, adoption, PPR, RRF, budget. No Neo4j dependency
├── app/anamnesis/              # daemon, ops CLI, client, wire framing, runtime, backup/restore
├── scripts/                    # migrate, backup, qa runners, eval harness, gates, research
└── deploy/vm/                  # systemd units, env example, backup and health scripts
```

Reusable domain code lives in `packages/`; the running and operating layer
lives in `app/anamnesis`. Harnesses that inject or retrieve text live in
separate repositories and attach over the RPC contract (D39). `remember`,
`recall`, `report_outcome` and the policy RPCs are API-only. The ops CLI
(`anamnesis-ops`) covers operation: `up`, `down`, `foreground`, `managed`,
`status`, `verify`, `backup`, `restore`, `load`, `extract`, `embed`,
`embed-requeue`, `recall` for inspection, `projection write` and the
identity, people, spaces and replies commands. A harness supplies
`origin_role` and, for an assistant turn that received anamnesis context,
`lineage_mode: "receipts"` with its parent Usage IDs. That literal names
parent-Usage lineage; the daemon verifies it against the authenticated caller
and never infers lineage from text (D49).

`dynamics` having no Neo4j dependency is what lets the forgetting, PPR and
ordering fixtures run without a container.

## Contract: zod is the source of truth

The zod schemas in `@anamnesis/protocol` are the only definition of
Element, Link, every RPC method, the error-code enum and the extraction and
lineage records. They give runtime validation at the system boundary and TS
type inference at once. `bun run schemas` exports JSON Schema into
`packages/protocol/schemas/` for a language-neutral contract. Regenerate it
in the same change as any schema edit; stale exported schemas fail the
schema-equality tests.

The server owns `episode_digest_version`. The verifier dispatches on the
stored version, so verify, retry, backup/restore and rebuild run write-free
over version-1 rows. No Episode is reserialized, relabeled or rewritten, and
there is no data migration of originals (docs/01 §1). Schema-level changes
are versioned by `Meta.schema_version` (`packages/core/src/schema-migrations.ts`).

## Toolchain and runtime

- **Development**: bun for the workspace, tests and scripts.
- **Runtime**: Node. The daemon, ops CLI and client are `bun build
  --target=node` bundles in `dist/` (`bun run build:runtime`), with the
  prompt files copied beside them.
- **No native dependencies**: the store is a Neo4j server and neo4j-driver is
  pure JS.
- **Floating point**: PPR uses only `+ × ÷` and is bit-reproducible. Mass
  and retention use `Math.pow` and carry a 1e-12 cross-runtime tolerance
  (docs/06 §7).
- **Tokenizers are optional and pinned.** A `tokens` budget is accepted only
  when the named `tokenizer_id` is installed and pinned by digest; an unknown
  ID is rejected. There is no estimate of any kind (D44).
- **GDS is pinned** to 2.13.12 in validation containers (docs/07 §1).

## Distribution

There is no published package. The runtime is the three `dist/` bundles
built from this repository. Installation, the systemd units in `deploy/vm/`,
environment variables, TCP access, backup timers and upgrade steps are in
[deploy](deploy.md); the daemon's local run, admission and recovery rules are
in [app/anamnesis](../app/anamnesis/README.md). The data layout is docs/01
§6.

## CI (GitHub Actions)

No workflow files exist. The gating commands are listed in docs/07 §6.

## Versioning

`@anamnesis/protocol` and `@anamnesis/core` move in lockstep (0.1.0);
the daemon and clients ship together, so independent versioning would add
nothing. `hello` carries the RPC version (`version: 2`). Other versions are
recorded on data, not on packages: `Meta.schema_version`, the stored
`episode_digest_version`, the extraction generation's configuration
(including `fact_language_policy`), the embedding `profileId` on vectors and
indexes, the `tokenizer_id` digest in each token-budget response, and the
policy revision on each Usage (D48, D49, D62). Changing a generation's
configuration means a new generation, never an in-place rewrite.

## Repository operations

- `anamnesis2` is the main branch. Work branches off it and lands by PR.
- Minimum before merge: `bun run gate` and `bun run test`.
- Any write outside the SET/DELETE list in docs/01 §8 is rejected in review.
- No test pins prose. Tests assert machine-consumed values: parsed fields,
  JSON examples, numeric fixtures, link targets.
