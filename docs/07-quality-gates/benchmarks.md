# Benchmark Design

Benchmarks verify that implementations satisfy the performance budget implied by this SSOT. Measurements must be reproducible and must state graph size and query shape.

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

## Local Answer-Accuracy Diagnostic

`local_answer` is the end-to-end QA diagnostic. It is deliberately separate
from the published retrieval table: answer accuracy depends on the selected
reader and judge models and is not an engine-only score.

The harness can run four paired routes over the same selected questions:

| Route | Context | Reader | What it isolates |
|---|---|---|---|
| `0-full-context` | complete conversation history | baseline local model | no-retrieval reader upper bound |
| `1-oracle-baseline` | dataset gold evidence | baseline local model | reader ceiling on supplied evidence |
| `2-retrieval-baseline` | shipped `Memory` + FastEmbed package | same baseline model | loss attributable to retrieval/context shape |
| `3-retrieval-strong` | exact same retrieved package as route 2 | stronger local model | reader-capability recovery |

Route 2 is a **product-wire** lane by default. Dataset turns enter through the
same `Memory::add` windowing recipe as a consumer session; queries use
`Memory::search_result_at_with`; optional consumer reranking returns scores
through `Memory::repackage_reranked_at`; and the reader receives the exact
`Memory::render_context(&recall)` string used by the MCP recall path. The
harness does not maintain a second fragment renderer or inject benchmark-only
dates into that product context: observation and validity times come from the
actual source nodes.

`--diagnostic-fragment-context` enables the historical adapter-enriched
per-fragment context. It is analysis-only and cannot be combined accidentally
with a headline claim. Fragment compaction, episodic hydration, and the
benchmark-only RRF experiment require this diagnostic lane.

`--derived-memory-artifact <json>` is a separate, explicit extraction
ablation. Schema v1 is dataset-fingerprint bound and records the exact local
Qwen 3.6 model digest and prompt version. It may cite only raw source
session/turn ids; the harness materializes additive derived semantic knowledge
and typed relations through the product APIs while preserving every raw turn. Report
artifact and no-artifact lanes separately.

Generate that artifact with
`scripts/generate_locomo_derived_artifact.py`. The generator reads conversation
turns only (never `qa`), batches at the product extractor's 20-source limit,
and calls `anamnesis extract-preview`, which reuses the exact versioned product
prompt, local provider, and validator without staging graph mutations. The
built-in Qwen 3.6 profile uses Ollama's non-streaming chat API on loopback with
the validator's strict JSON Schema; custom provider commands retain the bounded
no-shell subprocess path. Its sidecar checkpoint is resumable. Stable
sample-qualified session ids are mandatory because LoCoMo reuses raw
`session_1`/`D1:1` identifiers across samples; artifact ingest rejects
cross-sample provenance.

`--answer-report <predict-only-report.json>` consumes the product contexts
already persisted by an exact reader-free run and executes only the frozen
Qwen 3.6 reader/judge lane. It rejects partial contexts, dataset mismatches,
in-place overwrite, alternate context/derived flags, and non-Qwen-3.6 models.
This avoids rerunning an expensive deterministic reranker while keeping the
answer report's original retrieval metrics and context bytes.

`--judge-report <answered-report.json>` preserves every answer and retrieval
field and runs only the separately versioned local semantic judge. Use it to
apply one identical Qwen 3.6 judge lane to historical Anamnesis profiles and
competitor-adapter outputs without paying for retrieval or reader generation
again. The output path must differ from the source.

`--external-memory-artifact <json>` is the cross-system lane for Mem0,
Supermemory, MemKraft, and future adapters. Schema v1 is bound to the exact
dataset fingerprint and selected question set. Each record contains only
`question_id`, the external system's exact returned `context`, and optional
source text/session/turn provenance. `deny_unknown_fields` rejects answer,
reference, gold, and relevance keys. The harness does not fabricate Anamnesis
candidate/reranker metrics for this lane; it reports only the same frozen local
reader and semantic-judge outcomes. System name, version, and configuration
digest are mandatory.

For a local Mem0 OSS comparison, run the upstream
`mem0ai/memory-benchmarks` LoCoMo `--predict-only` phase and pass its output
through `scripts/export_memorybench_contexts.py`. The converter requires this
harness's selection report, dataset fingerprint, upstream version, config
file, and cutoff. It copies only returned memory strings and strips upstream
ground truth, evidence annotations, answers, and judgments before producing
the strict external artifact. Anamnesis and Mem0 can then use the same Qwen
3.6 reader, prompt, official-compatible F1, and separately named semantic
judge. Cloud Supermemory follows the same wire, but no comparison is valid
without credentials plus an exact provider version/config digest.

An optional consumer cascade keeps both rerankers outside core:
`--consumer-prefilter-cross-encoder`, `--consumer-prefilter-k`, and
`--consumer-cross-encoder`. `--consumer-prefilter-query-fusion` applies RRF to
the exact deterministic query variants recorded in `SearchTrace` at the fast
prefilter only; the final quality reranker still sees the complete original
question. Cascade latency and quality are promotion gates, not defaults.

Headline answer evaluation is cold/read-only: it does not call `Memory::used`
between questions, so earlier benchmark questions cannot warm later ones.
`real_memory --warmup N` remains the explicit lifecycle diagnostic for committed
retrieval, and commit-trace correctness is covered separately by engine tests.

Route 0 is enabled with `--full-context`; LongMemEval requires a declared
context window of at least 131072 for that route. Route 3 is opt-in with
`--run-strong-reader`. `--stratify N` uses a stable
hash sample within every question type; record the accompanying
`--sample-seed` instead of taking the first N dataset rows.

LoCoMo's primary answer metric is the official deterministic category-aware
token F1, including NLTK's default Porter stemmer extensions. The separate
local LLM judge is secondary diagnostic data, not the headline score.
LoCoMo turns include `blip_caption` as shared-image text, matching the official
full-context and dialog-RAG prompt construction; the image-search `query` field
is not used. LongMemEval uses its official category-specific
judge criteria, with the local judge model and digest disclosed because the
upstream metric itself is model-based.

The CLI rejects non-loopback Ollama URLs. The report stores model manifest
digests, a dataset fingerprint, every gold/retrieved context item, retrieval
metrics at three named selection surfaces (`candidate@K`, `reranker@K`, and
`delivered@K`) plus exact-text `rendered` gold coverage, the exact
product-rendered context, raw answers, official
scores, raw judge responses, parse failures,
per-stage latencies, generation settings, termination reasons, prompt/output
token counts, and hidden-thinking character counts. An empty final answer is a
run error rather than a zero-quality answer. The report is written after every
answer and judgment, so an interrupted run can resume without silently
substituting empty answers.
Answer generation and judging run in separate phases. This avoids repeatedly
swapping large local reader and judge models while preserving an incremental,
resumable report.

The headline LoCoMo score is always raw official F1. A second
`reader_surface_f1` applies one frozen, reference-blind transform only when the
entire answer is a valid standalone ISO date. It is reported separately and
must not be presented as memory-quality improvement. Compare two reports with:

```bash
python3 ../../scripts/compare_local_answer.py BASELINE.json CANDIDATE.json
```

The comparison refuses mismatched datasets, question sets, reader controls,
prompt versions, or context surfaces. It reports paired question and
conversation-cluster intervals plus Answer-F1 changes conditional on actual
product-rendered recall improving, tying, or regressing.
For a reader-free fixed-ranking selection experiment, pass
`--selection-variant top-10` (or another recorded cutoff). That lane compares
selected, delivered, and rendered recall, rendered Hit, per-type deltas, and
both paired intervals without requiring answer routes.

`--consumer-ranking-report` replays consumer scores from a compatible report
while still rebuilding the graph, validating the nodes against a fresh core
readout, and repackaging through the product API. Dataset/sample controls,
candidate surface, seed limit, extraction fingerprint, reranker identity, and
source relevance policy must match. This is the preferred way to compare
several consumer selection policies without repeatedly sampling a reranker.

For a paired reader experiment, `--paired-answer-report BASELINE.json` may be
combined with `--answer-report`. The harness reuses a stored answer (and,
when compatible, its judge decision) only when the complete rendered reader
prompt, prompt versions, local model digests, and generation settings are
identical. Changed prompts are generated normally. The paired report
fingerprint and per-route reuse bit are persisted, so this optimization cannot
silently turn different contexts into ties.

Example pilot runs:

```bash
# Run from crates/anamnesis (Cargo sets this as the bench process cwd).
ollama pull qwen3.6:35b-a3b

cargo bench --features embed --bench local_answer -- \
  --dataset locomo --stratify 1 --skip-adversarial --full-context \
  --baseline-reader-model qwen3.6:35b-a3b \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-locomo-pilot.json

cargo bench --features embed --bench local_answer -- \
  --dataset longmemeval --stratify 1 \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-longmemeval-pilot.json

# Reader-free retrieval diagnosis: 25 questions from each non-adversarial type.
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --stratify 25 --sample-seed 42 \
  --skip-adversarial --predict-only \
  --consumer-cross-encoder BAAI/bge-reranker-base \
  --consumer-candidate-k 100 --first-stage-seed-limit 10 \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-locomo-retrieval-n100-seed42.json

# Local semantic-answer lane. Raw official F1 remains the headline score;
# this judge result is reported separately and applied identically to systems.
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --stratify 25 --sample-seed 42 \
  --skip-adversarial --retrieval-only \
  --baseline-reader-model qwen3.6:35b-a3b \
  --judge-model qwen3.6:35b-a3b --reader-no-think --judge-no-think \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-locomo-qwen36-n100-seed42.json
```

Use `--resume` with the same output and configuration after interruption.
Pilot or stratified runs must be labeled as such; headline claims require the
declared full split. An unparseable judge response is recorded as unparsed and
excluded from the accuracy denominator, never coerced to incorrect.

The first completed harness-validation run is recorded in
[local-answer-pilot-2026-07-24.md](local-answer-pilot-2026-07-24.md).
The first seeded 100-question 7B-reader diagnostic is recorded in
[local-answer-7b-n100-2026-07-24.md](local-answer-7b-n100-2026-07-24.md).
The paired 7B versus 35B reader comparison is recorded in
[local-answer-reader-comparison-n100-2026-07-24.md](local-answer-reader-comparison-n100-2026-07-24.md).
That historical run used non-thinking temperature-zero generation and a
generic binary local judge. It is retained as harness-development evidence,
not an official LoCoMo quality claim.
Current local answer development and promotion runs use
`qwen3.6:35b-a3b`. Qwen 3.5 records below are immutable historical evidence,
not current reproduction instructions.
The first seeded Qwen3.5 35B-A3B result using LoCoMo's official deterministic
F1, including the top-k sweep and rejected package ablations, is recorded in
[local-answer-official-f1-n100-2026-07-25.md](local-answer-official-f1-n100-2026-07-25.md).
The reproducible greedy Qwen3.5 35B-A3B gate, corrected temporal anchoring,
annotated-evidence ceiling, readout audit, and rejected E5-large comparison are
recorded in
[local-answer-greedy-n100-2026-07-25.md](local-answer-greedy-n100-2026-07-25.md).
The paired n=200 local reranking gate, accepted high-quality profile, product
integration boundary, and rejected fast alternatives are recorded in
[local-answer-reranking-n200-2026-07-25.md](local-answer-reranking-n200-2026-07-25.md).
The schema-v16 product-wire contract, reranking validity repair, four-stage
evidence accounting, and first smoke run are recorded in
[local-answer-product-wire-v16-2026-07-25.md](local-answer-product-wire-v16-2026-07-25.md).
The first Qwen 3.6 product-wire bottleneck decomposition is recorded in
[local-answer-qwen36-diagnosis-n20-2026-07-25.md](local-answer-qwen36-diagnosis-n20-2026-07-25.md).

LoCoMo category names follow the official evaluation code:
`1=multi-hop`, `2=temporal`, `3=open-domain`, `4=single-hop`,
`5=adversarial`. The downloader preserves the dataset's
`session_*_date_time` fields because temporal questions require them.
LongMemEval-S is downloaded from the cleaned release at a pinned dataset
revision; pre-cleaning snapshots must not be mixed with current results.
The LoCoMo `1-oracle-baseline` route means dataset-annotated evidence, not a
guaranteed perfect oracle. Upstream evidence IDs contain known noisy cases, so
the route is diagnostic and the official full-context/retrieval answer F1
remains the outcome metric.

## Regression Judgment

- Latency is judged by p95 for each fixture.
- Output quality is judged by bucket shape and tension presence for golden queries.
- Failures are categorized as performance-budget failures or context-shape changes.
- Large-graph optimization must not change the public API.

## Related Documents

- Cognitive-fidelity results (forgetting, spacing, fan charts) are in [fidelity-results.md](fidelity-results.md).
- Observability reports are defined in [observability.md](observability.md).
- Query flow is defined in [pipeline.md](../05-context-retrieval/pipeline.md).
- Storage cost is defined in [storage.md](../03-persistence/storage.md).
