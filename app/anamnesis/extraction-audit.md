# Claim/judge audit pipeline

This is a non-serving G004 subset. `capabilities.extraction` remains false.
The installation-authenticated methods below execute and retain model audit
work; they do not create Facts, Entities, Communities, semantic links or Hits.
A `correct` or `suppress` decision is an observation, not authorization to
correct or suppress memory. No semantic quality is certified.

Configure `ANAMNESIS_EXTRACTION_CONFIG` with the existing HTTP adapter fields
`endpoint`, `model`, `model_incarnation` (SHA-256), and `timeout_ms`.
A missing provider rejects creation/execution. A provider supporting only the
old standalone judge ABI fails closed for the new `judge_claims` task.

An existing writable extraction generation is required; generation admission
and lifecycle operations remain internal Engine/Store APIs. No generation
cutover or recall readiness follows from this audit pipeline.

- `extraction.audit.create {id,generation_id,source_id}` creates an idempotent
  claim task using the configured model, exact stored Episode revision and
  content-byte SHA-256. Callers cannot supply model output, source text,
  policy, role or lineage replacements.
- `extraction.audit.run {task_id,expected_version,worker_id,lease_ms}` runs the
  queued claim and then its unique child judge. HTTP calls are outside
  retryable transactions. Already terminal tasks do not call the provider
  again. Failed or abandoned leases require explicit internal
  `retryModelTask`/`settleModelTask`; this subset has no automatic retry loop
  or recovery RPC.
- `extraction.audit.status {pipeline_id}` returns `unknown` when no pipeline
  exists, otherwise the current tasks, their terminal attempts, and up to 64
  decisions. Disconnect is not failure or adoption: query status with the
  same identity on a new authenticated connection.

The HTTP judge receives `{text,task:'judge_claims',claim_context}` plus the
configured model identity. `claim_context` carries the exact parent task ID,
immutable attempt ID, output digest and ordered claims. The response uses
`{task:'judge_claims',claim_body_digest,decisions,language,modality}`;
each decision has `{claim_index,disposition,evidence}`. All indexes 0..N-1
must occur exactly once in order, with byte-identical parent evidence.
Evidence validates exact UTF-8 slices, not entailment. The old audit
`modality` means text/code/mixed/unknown, not Fact speech-act modality.

Each judge lease appends its own immutable input premise: current source
head, policy revision, and parent attempt binding. Completion revalidates
these premises and current source permission under the writer fence.
Changed premises retain a content-free failure; denied output retains a
content-free cancelled attempt. Valid per-claim disposition rows and the
terminal judge attempt commit atomically. Decisions are read through 65
composite point seeks, including an overflow sentinel, with no adjacency or
candidate fallback. Required missing decision rows reject status/coverage.

Audit coverage cannot seal a successful claim until its judge succeeds with
complete decision authority. It never marks the original Outbox processed
or certifies materialization, embeddings or serving readiness. Original
bytes, source records, episode lineage and receipt/Hit authority are unchanged.

The separate [Episode lineage admission](episode-lineage.md) now retains
prospective authenticated ancestry and provides a checked semantic-source
adapter. It does not change this legacy audit ABI or authorize its dispositions.

Still absent from this audit pipeline: semantic claim lifecycle integration,
bounded generation-scoped Entity/Fact candidate reads and indexes, shadow
adjudication review/consumption, Fact/Entity/Community materialization,
correction/evidence mapping, remention/CONTRASTS, derived recall and all six
G004 acceptance fixtures. Existing graph and offline GDS qualification are
separate and are not upgraded by this subset.
