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
metrics, raw answers, official scores, raw judge responses, parse failures,
per-stage latencies, generation settings, termination reasons, prompt/output
token counts, and hidden-thinking character counts. An empty final answer is a
run error rather than a zero-quality answer. The report is written after every
answer and judgment, so an interrupted run can resume without silently
substituting empty answers.
Answer generation and judging run in separate phases. This avoids repeatedly
swapping large local reader and judge models while preserving an incremental,
resumable report.

Example pilot runs:

```bash
# Run from crates/anamnesis (Cargo sets this as the bench process cwd).
ollama pull qwen2.5:latest
ollama pull qwen3.5:35b-a3b
ollama pull gemma3:12b

cargo bench --features embed --bench local_answer -- \
  --dataset locomo --stratify 1 --skip-adversarial --full-context \
  --baseline-reader-model qwen3.5:35b-a3b \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-locomo-pilot.json

cargo bench --features embed --bench local_answer -- \
  --dataset longmemeval --stratify 1 \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-longmemeval-pilot.json

# Reproducible 7B-reader diagnostic: 25 questions from each non-adversarial type.
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --stratify 25 --sample-seed 42 \
  --skip-adversarial --baseline-only \
  --baseline-reader-model qwen2.5:latest --judge-model gemma3:12b \
  --allow-download --embed-cache .fastembed_cache/local-answer.sqlite \
  --output benches/eval/results/local-answer-locomo-7b-n100-seed42.json
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
