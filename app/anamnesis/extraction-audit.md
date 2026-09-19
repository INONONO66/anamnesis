# Claim/judge audit pipeline

`capabilities.extraction` reports whether an extraction provider is configured;
it does not certify semantic serving readiness. The installation-authenticated
pipeline retains claim/judge audit work. Completed retained claims also materialize
generation-scoped Facts, Entities, evidence links and conducting arcs. Generation
coverage and cutover remain explicit Engine operations; policy-authorized derived
recall serves only the selected generation. No semantic quality is certified.
A `correct` or `suppress` decision does not modify the original Episode.

Configure `ANAMNESIS_LLM_BASE_URL` and `ANAMNESIS_LLM_API_KEY_FILE` for the
chat provider. The private JSON key file contains a `bearer` string. The model
defaults to `claude-haiku-4-5`; `ANAMNESIS_LLM_DIALECT` accepts
`anthropic_messages` or `openai_chat` (inferred from a `claude` model prefix).
The Anthropic transport uses `/v1/messages`; OpenAI uses `/v1/chat/completions`.
`ANAMNESIS_EXTRACTION_PROMPT_FILE` overrides the bundled prompt.
Chat models return exact evidence quote strings, not offsets. The adapter locates
quotes in the source and computes UTF-8 byte spans locally, then validates the
strict audit ABI. New claims use the first occurrence of a repeated quote, or
the exact byte occurrence nearest a supplied legacy start offset. Model offsets
never authorize bytes. Claim judges retain the parent's occurrence. Unsupported
quotes and their claims/decisions are dropped; an empty claim result suppresses
extraction. A judge missing required decisions still fails the unchanged Store
completion fence rather than inventing authority.
Leading JSON fences (including a trailing explanation) are unwrapped, but extra
JSON fields and malformed shapes remain rejected.
The legacy `ANAMNESIS_EXTRACTION_CONFIG` HTTP adapter fields remain supported:
`endpoint`, `model`, `model_incarnation` (SHA-256), and `timeout_ms`.
The LLM base URL takes precedence over that legacy adapter.
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

Audit coverage seals a successful pipeline only with complete judge authority.
A terminal failed or cancelled claim/judge instead seals a content-free omission
binding its stage, immutable attempt ID, state and reason into the coverage digest.
It contributes no Fact, and retry is forbidden after sealing. Missing judges,
queued/leased work and expired/worker-lost leases remain incomplete, not omissions.
Cutover accepts these sealed omissions alongside independently verified successful
materializations. Its bounded evidence/witness checks account for up to 64 claims
per source rather than mistaking the 256-source bound for a 256-Fact bound.
The audit never marks the original Outbox processed.
Materialization and cutover additionally check persisted generation custody,
conducting arcs, policy, indexes and optional embedding coverage. Original bytes,
source records, episode lineage and receipt/Hit authority are unchanged.

The separate [Episode lineage admission](episode-lineage.md) now retains
prospective authenticated ancestry and provides a checked semantic-source
adapter. It does not change this legacy audit ABI or authorize its dispositions.

This pipeline does not certify semantic equivalence, contradiction, attribution
quality, Community synthesis or full G004 acceptance. Existing graph and offline
GDS qualification remain separate. The live opt-in QA run is documented in the
[runtime README](README.md#live-extraction-acceptance-explicit-opt-in).
