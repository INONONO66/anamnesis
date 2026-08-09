# Benchmark Design

Benchmarks verify that implementations satisfy the performance budget implied
by this SSOT. Every measurement must state graph size, query shape, source
revision, dataset fingerprint, configuration, and retained evidence. A report
may claim independent reproducibility only when the named binary, model
identity, inputs, and generated artifacts are available to the reproducer.

## Measurement Targets

| Target | Input Size | Metrics |
|---|---|---|
| ingest | node count, candidate count | observations per second, edge proposals |
| conductance | candidate count | coupling proposals per second |
| tick | node count | nodes scanned per second |
| search | seed count, graph degree | p50, p95 latency |
| activation flow | edge count | iterations, residual, current-map time |
| packaging | token budget | selected sites, truncation |
| snapshot | graph size | clone time and memory |
| storage | CRUD batch | operations per second |

## Fixture Graphs

| Size | Nodes | Edges | Purpose |
|---|---:|---:|---|
| small | 1k | 5k | local development |
| medium | 100k | 1M | expected serious project memory |
| large | 1M+ | 10M+ | scalability exploration |

Fixtures should include:

- identity sites,
- semantic and procedural knowledge,
- episodic fragments,
- entity hubs,
- contradiction pairs,
- scoped private and universal knowledge,
- stale and recently accessed sites.

## Search Scenario

Each benchmark query should declare:

- text cue,
- optional embedding,
- scope,
- temporal filter,
- expected bucket mix,
- expected tension behavior,
- token budget.

## Performance Budgets

| Operation | Small | Medium |
|---|---:|---:|
| ingest | interactive | bounded by top-k candidate scan |
| search | sub-second target | p95 budget required |
| tick full scan | seconds or less | batch or incremental path required |
| snapshot | interactive | background recommended |

Exact thresholds are project-calibrated. The benchmark must make regressions visible.

## Quality Counters

Fast retrieval is not enough. Benchmarks also store:

| Counter | Meaning |
|---|---|
| selected_identity | identity bucket count |
| selected_knowledge | knowledge bucket count |
| selected_memory | memory bucket count |
| tension_count | returned tension count |
| budget_used | token budget utilization |
| truncation_count | resolution downgrade count |
| residual | activation-flow convergence residual |

## Local Answer Diagnostic

`local_answer` measures retrieval, rendered context, and answer behavior as
separate surfaces. Reader and judge results are not engine-only scores.

| Route | Context | Purpose |
|---|---|---|
| `0-full-context` | complete sample history | no-retrieval diagnostic |
| `1-oracle-baseline` | dataset-annotated evidence | reader diagnostic |
| `2-retrieval-baseline` | canonical `Memory` package | product-path measurement |
| `3-retrieval-strong` | the same retrieved package | optional reader configuration |
| `diag-oracle-strong` | frozen dataset-annotated evidence | same-reader diagnostic |
| `diag-full-history-strong` | complete raw session history | same-reader diagnostic |

Routes 0 and 1 are diagnostics. In a live document-reranker run, route 2 calls
the canonical `Memory::search_reranked_for_plan_at` path once and measures that
call. The same precomputed `RecallPlan` then flows through plan-aware readout and
`render_context_for_plan_with`. Route 3 changes only the reader configuration
and reuses the exact package frozen by route 2. The harness does not maintain a
second production fragment renderer.

After the product package and its retrieval latency are frozen, the harness
runs a separate deterministic source search solely to record candidate and
feature diagnostics. It cannot add, remove, reorder, or repackage route 2
evidence, and its elapsed time is not included in the product retrieval
latency. Replayed rankings and fixed-cutoff selection screens remain diagnostic
routes and do not qualify the canonical live-reranker path.

`--strong-reader-contexts retrieval,oracle` runs the same
strong local reader against the already-frozen oracle context. The resulting
`diag-oracle-strong` route is label-assisted, is never a product-path score,
and cannot feed evidence back into formation, retrieval, or rendering.

`--strong-reader-contexts full-history` is a label-free no-retrieval
diagnostic. In stored-report mode it reloads only a fingerprint-matched local
dataset, joins questions to the frozen sample identities, and supplies raw
session turns to the reader. It fails rather than truncating when the declared
reader context budget is insufficient.

### Evaluation isolation

The harness enforces its input boundary with distinct types:

- `FormationInput` contains only dataset identity and normalized source
  sessions. Graph construction cannot receive questions, expected answers,
  categories, or relevance annotations.
- `RetrievalInput` contains only question id, question text, and optional
  question time. Search, reranking, selection, and product rendering cannot
  receive expected answers, categories, or relevance annotations.
- The evaluation layer retains gold fields and applies them only after the
  product context or answer has been frozen.

Full-context and oracle-evidence routes remain explicitly labeled diagnostics
and cannot qualify production formation or retrieval behavior.

Retrieval metrics follow the public product surfaces rather than treating the
internal cognitive trace as the reranker input:

- `candidate` uses the exact validated `EvidenceDocument` sidecar supplied by
  canonical production reranking. A grouped document is credited through every
  raw delivery source whose text it contains; private scoring-only contributors
  are not treated as delivered evidence. Runs without a live or frozen document
  ranking retain their actual first-stage cognitive surface.
- `reranker` applies the consumer ordering and final cutoff to those same
  source-bearing documents.
- `delivered` measures packaged fragments after deterministic selection and
  validity filtering.
- `rendered` measures raw evidence that is visible in the exact product context
  string supplied to the reader.

Raw trace ranks remain available only as diagnostics. Report schema 45 records
the complete consumer ranking separately from the final-cutoff metric surface
and records a local deterministic change detector over the complete canonical
document surface. Frozen replay runs the same prepare/complete API, verifies
that surface, and applies production selection to every scored row without a
model call. The change detector guards accidental drift in trusted local
artifacts; it is not a cryptographic authenticity mechanism. Older schemas and
reports whose engine package-policy version differs are not accepted as replay
inputs. Reports that measured `candidate` or `reranker` through a representative
trace node instead of all represented raw sources are not directly comparable
on those two fields.

A `--derived-memory-artifact` is a declared formation input. It must be bound
to the dataset fingerprint, cite live raw sources, and enter through the same
typed admission path as other source-grounded records. It is reported
separately from raw-turn formation.

### Local model boundary

Reader and judge lanes are restricted to Qwen 3.6 and local transports:

- Ollama uses `--ollama-base-url`, accepts only a literal loopback-IP HTTP endpoint, and
  disables proxies and redirects.
- OMLX uses `--omlx-reader` and/or `--omlx-judge` with
  `--omlx-base-url` (or `OMLX_BASE_URL`). The compatible chat transport
  accepts only literal loopback-IP hosts, disables proxies and redirects, and
  never reads or sends credentials.
- `--omlx-model-digest <SHA256>` records a caller-supplied model digest. The
  runner must derive and verify it against the resolved local artifact before
  making an exact model-identity claim.

The optional reflected-reader route uses the same label-free `ReaderInput` as
the direct reader. The core validates the typed shape of an untrusted
evidence-analysis draft and checks every citation against the delivered source
ids; this deterministic check does not prove semantic entailment. The model
verification pass receives the same complete rendered context and remains
responsible for semantic verification. A bounded recovery policy may repair one
structurally invalid draft and reverify one answerable or unresolved abstention.
Reports store the admitted answer and reflection, list deterministic
transformations, and record conditional model calls and latency separately.
Recovery receives no expected answer, category, annotation, or judge output.
Empty required analysis, verification, or final-answer responses fail the run;
empty optional recovery responses follow the recorded bounded fallback policy.

### Report integrity

Reports record dataset and model fingerprints, configuration, product context,
retrieval surfaces, stage latencies, raw answers, judge parse state, and token
counts. Reports are written incrementally and `--resume` requires an identical
configuration. `--answer-report` and `--judge-report` can reuse frozen prior
stages without exposing gold fields to formation or retrieval.

Primary runs are cold and read-only: they do not call `Memory::used` between
questions. Lifecycle learning is measured separately with the explicit warmup
mode and engine contract tests.

LoCoMo uses its deterministic category-aware token F1 as the primary answer
metric. A local semantic judge is a separately named diagnostic. LongMemEval
uses its declared category-specific evaluation contract; reports must include
the judge model identity because that metric is model-dependent.

Annotation-based retrieval recall, deterministic LoCoMo F1, and
model-dependent semantic-judge accuracy are distinct metric regimes. They must
not be compared as one score. External comparisons require an identical split,
category mapping, adversarial policy, reader, and judge configuration.

A minimal local smoke run from `crates/anamnesis` is:

```bash
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --stratify 1 --skip-adversarial --predict-only \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-locomo-smoke.json
```

Pilot and stratified runs must be labeled as such. Qualification requires a
declared full split, source revision, dataset fingerprint, model digest,
configuration, and retained evidence.

## Regression Judgment

- Latency is judged by p95 for each fixture.
- Real-memory reports keep three latency boundaries distinct: `latency_ms` is
  query through packaged evidence, `context_render_latency_ms` is exact product
  context rendering, and `context_ready_latency_ms` is their sum. Consumer
  prompt wrappers, tokenization, and model generation are outside all three and
  belong in end-to-end reader measurements.
- Output quality is judged by bucket shape and tension presence for golden queries.
- Failures are categorized as performance-budget failures or context-shape changes.
- Large-graph optimization must not change the public API.

## Evidence-complete release gate

The architecture in
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
is eligible for release only when all of the following are reported
independently:

| Gate | Required evidence |
|---|---|
| formation parity | For identical source revisions, typed candidates, profile identity, and review records, every declared entry point uses the same admission policy/transaction and produces equivalent admitted records |
| extractor parity | Model digest, prompt/schema version, decoding controls, batching, and validator match; this consumer contract is reported independently from formation admission |
| grounding | Invalid span/hash/range fails closed; delivered provenance coverage is 100% |
| admission isolation | Routing-only facts never appear without authoritative source hydration |
| scope and time | Property tests show no scope widening, disjoint-scope chain, or invalid-time hop |
| retrieval surfaces | Candidate, reranked, selected-chain, hydrated, rendered, and all-required-slot recall are separate |
| chain value | A chain or tension enters final evidence only when it fills a requested slot, resolves a typed relation, or supplies a required contradiction, supersession, or validity qualifier |
| answer transfer | Reader-free changes are accompanied by an end-to-end reader run and a separately labeled oracle-evidence diagnostic |
| category floors | Direct, temporal, collection, relationship, and inference classes pass declared thresholds independently; aggregate score cannot hide a weak class |
| generalization | Development sample, held-out/full split, and conversation-cluster intervals are distinguished |
| regression | Direct and temporal golden queries do not regress when catalog/chain lanes are enabled |
| graceful degradation | Empty catalog, extractor failure, and reflection failure preserve deterministic raw recall and explicit uncovered slots |
| scale | Indexed results match an exhaustive reference on bounded fixtures; hot retrieval performs no full catalog scan |
| latency | Every engine stage and context-ready p95 is recorded; consumer reflection/generation remains separate |
| source lifecycle | Source revision/update/retraction/deletion and dependent eligibility, projection, index generation, and mutation events commit atomically or roll back together |
| state recovery | Snapshot/clone/restore covers graph, source revisions, catalog/admission state, active versions, generation keys, and dependency-ordered events |
| formation isolation | Formation sees only source turns, source metadata, and the formation profile; it cannot access questions, answers, annotations, labels, judge output, or prior results |
| retrieval isolation | Retrieval sees the question plus admitted memory and declared runtime constraints; it cannot access expected answers, evidence annotations, labels, judge output, or evaluation-only route hints |
| judge integrity | Parse failures count as incorrect in the emitted all-question diagnostic; semantic qualification requires 100% parse coverage |

Threshold values belong to a versioned release profile and calibration record,
not to production branching logic. Release qualification uses the full declared
suite; a small balanced sample is a development gate only.

## Related Documents

- Cognitive-fidelity results (forgetting, spacing, fan charts) are in [fidelity-results.md](fidelity-results.md).
- Observability reports are defined in [observability.md](observability.md).
- Query flow is defined in [pipeline.md](../05-context-retrieval/pipeline.md).
- Storage cost is defined in [storage.md](../03-persistence/storage.md).
