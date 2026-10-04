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
- `extraction.audit.status {pipeline_id}` returns `unknown` when the journal
  holds no entry for that pipeline, otherwise the current tasks, their
  terminal attempts, and up to 64 decisions. Disconnect is not failure or
  adoption: query status with the same identity on a new authenticated
  connection.

## Journal and audit log

Pipeline state is not in Neo4j. Tasks, leases, attempts, judge inputs and
decisions live in `<installation root>/extraction-state.json`, one entry per
pipeline keyed by `pipeline_id` and reachable by work key
(`<generation_id>:<source_id>`) or attempt ID. Every mutation rewrites the
whole file through a temporary name and an atomic rename, writes are
serialized, and the file is schema-validated on load and on every write.
The RPC value shapes (`ModelTask`, `ExtractionAttempt`, `ExtractionJudgeInput`,
`ExtractionDisposition`, `ExtractionPipeline`) are unchanged; they are now
built from the journal entry instead of graph nodes. Neo4j keeps
`ExtractionGeneration`, `ExtractionCoverage` and the memory the pipeline
materializes: Facts, Entities, evidence links and conducting arcs.

The file is bounded by a retention rule. An entry leaves the journal once
coverage has sealed its source for its generation and materialization is
terminal for it: custody exists (`semantic_writes` decided and the relation
judge disabled, complete or omitted), or the pipeline is a sealed omission
(failed or cancelled claim or judge). An entry whose relation judge is still
pending stays until the verdict lands. A standalone task (`pipeline: false`)
follows the same rule with a terminal attempt in place of custody. After an
entry is pruned, `extraction.audit.status` answers `unknown` for it and a
store transition on one of its tasks (retry, cancel, settle, lease) throws
`unknown_ModelTask` instead of reaching the `coverage_frozen` fence; the
audit log, not the file, is the durable record.

Each transition writes one structured line to the daemon log (journald on the
LXC), carrying `pipeline_id`, `generation_id`, `source_id`, `task_id` and
`kind` (`claim` or `judge`) plus the fields named: `extraction.task.created`
{work_key}, `extraction.task.leased` {attempt_id, worker_id, lease_epoch,
lease_until} before the provider call, `extraction.attempt.recorded`
{attempt_id, state, reason, detail, decisions, body_digest},
`extraction.task.settled` {attempt_id, reason} for `expired` or
`worker_lost`, `extraction.task.retried` {attempts},
`extraction.task.cancelled`, `extraction.pipeline.materialized`
{semantic_writes, relation_judge, facts, refused, duplicates} and
`extraction.pipeline.pruned` {sealed_ingest_seq}.

A daemon killed between the lease and the recorded attempt leaves a `leased`
task in the file. On restart the scheduler finds the Episode again (its
`ingest_seq` is above `ExtractionCoverage.covered_ingest_seq`), resolves the
leased task by work key, settles it as `worker_lost` or `expired`, retries
within the lost-lease budget and re-runs it. Materialization is idempotent
(one Fact per digest, one custody row per occurrence key), so a replayed
pipeline writes no second Fact. Coverage resumes from the covered sequence
in the graph, which the journal never holds. Losing the file loses in-flight
leases and attempt history, not memory: unsealed sources are re-driven and
sealed ones are already in coverage.

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
content-free cancelled attempt. A `provider_mismatch` attempt names the
check it failed in `detail` (`model`, `incarnation`, `envelope`, `json`,
`normalize`, `span`, `judge_shape` or `digest`). A lease stamps the task
with the identity of the provider that runs it, so work queued or lost
under a previous daemon configuration is finished by the current one
rather than refused until its attempt budget is spent (#218). Valid per-claim decisions and the
terminal judge attempt are written to the journal entry in one step. Decisions
are read from that entry (at most 64, index-ordered) with no adjacency or
candidate fallback. Required missing decisions reject status/coverage.

Audit coverage seals a successful pipeline only with complete judge authority.
A terminal failed or cancelled claim/judge instead seals a content-free omission
binding its stage, immutable attempt ID, state and reason into the coverage digest.
It contributes no Fact, and retry is forbidden after sealing. Missing judges,
queued/leased work and expired/worker-lost leases remain incomplete, not omissions.
Cutover accepts these sealed omissions alongside independently verified successful
materializations. Its bounded evidence/witness checks account for up to 64 claims
per source rather than mistaking the 256-source bound for a 256-Fact bound.
The audit never writes to the Episode itself; coverage is the only cursor.
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
