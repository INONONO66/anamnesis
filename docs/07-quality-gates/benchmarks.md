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
| `0-full-context` | complete selected history | no-retrieval diagnostic |
| `1-oracle-baseline` | dataset-annotated evidence | reader diagnostic |
| `2-retrieval-baseline` | canonical `Memory` package | product-path measurement |
| `3-retrieval-strong` | the same retrieved package | optional reader configuration |

Routes 0 and 1 are diagnostics. Only route 2 exercises formation, search,
reranking, source-aware selection, and query-aware context rendering through
the public `Memory` surface used by direct crate consumers and MCP clients.
The harness does not maintain a second production fragment renderer.

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

A `--derived-memory-artifact` is a declared formation input. It must be bound
to the dataset fingerprint, cite live raw sources, and enter through the same
typed admission path as other source-grounded records. It is reported
separately from raw-turn formation.

### Local model boundary

Reader and judge lanes are restricted to Qwen 3.6 and local transports:

- Ollama uses `--ollama-base-url` and rejects non-loopback hosts.
- OMLX uses `--omlx-reader` and/or `--omlx-judge` with
  `--omlx-base-url` (or `OMLX_BASE_URL`). The compatible chat transport
  rejects non-loopback hosts, disables proxies and redirects, and never reads
  or sends credentials.
- `--omlx-model-digest <SHA256>` records the exact local model identity.

The optional reflected-reader route uses the same label-free `ReaderInput` as
the direct reader. Its first pass is untrusted evidence analysis; the second
pass verifies that draft against the same rendered context. Empty output is a
run error, and the harness does not rewrite model answers.

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
| answer transfer | Reader-free changes are accompanied by an end-to-end reader run and an oracle-evidence ceiling |
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
