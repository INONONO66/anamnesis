# Frontier Memory Program — Measured Status

Date: 2026-07-26

Status: active development program; no full-split or cross-system leaderboard claim

## 2026-07-29 product-path decision

The earlier “benchmark consumer only” disposition for the quality reranker is
superseded. There is now one canonical production pipeline:

`Memory::search_reranked` → cognitive search@20 / candidate@50 →
Memory-compiled evidence documents → `RerankingProvider` → automatic deep
evidence selection → final@20 commit-safe packaging.

`anamnesis-mcp` uses that pipeline with
`BAAI/bge-reranker-base` by default, and the LoCoMo `local_answer` harness
calls the corresponding existing-search overload
`Memory::rerank_search_result_at` for retrieval diagnostics. The harness no
longer privately assembles the headline reranker path. Its defaults reference
the same engine constants for search width, candidate width, and final width.
Benchmark-only rank fusion, replay, cascade, and screen modes remain explicit
diagnostics.

The final n=100 product-profile run measured 1.69 s mean, 2.68 s p95, and
3.09 s maximum retrieval latency with BGE base over 50 candidates. The Rust
hook therefore fails open after 4 s and the plugin process retains its 5 s
outer backstop. Candidate@100 was rejected: it raised candidate recall but
reached 2.77 s mean / 4.49 s p95 while adding only about 2 points of rendered
multi-hop/open-domain recall and regressing the easy types. Operators can
choose another `ANAMNESIS_RERANK_MODEL`, but a reranker error is surfaced
rather than silently falling back to a different retrieval policy.

The `rozgo/bge-reranker-v2-m3` candidate@200 profile remains an explicit
offline quality experiment. Its roughly 25 s mean / 34 s p95 measurement is
not a product default and is not the default LoCoMo headline configuration.

## 2026-07-30 structural retrieval gate

The production pipeline now performs coverage-aware trace@200 → document@50
preselection for Collection, Relationship, and Inference plans. It preserves
the first 30 cognitive rows byte-for-byte, then admits a bounded deeper tail
using query facets, canonical raw sources, source-session bridges, and typed
temporal neighbors. Direct, temporal, causal-why, and gift recommendation
queries keep their prior conservative path.

Reviewed extracted facts no longer share the graph candidate pool. They are
persisted in an isolated atomic-fact sidecar with embeddings, validity,
scope, selective entity tags, and raw Episodic source IDs. A selected fact can
route only those live raw sources; the compact extracted text is never
rendered as evidence, added to node FTS, or allowed to affect graph dynamics.
The benchmark artifact and MCP review/promotion flow both use this same
`Memory::add_atomic_fact` product API.

Reader-free LoCoMo seed-42 balanced n=100:

| Type | candidate@50 | delivered@20 | rendered recall | Baseline delta |
|---|---:|---:|---:|---:|
| multi-hop | 67.90% | 62.90% | **64.90%** | **+5.50 pp** |
| open-domain | 58.03% | 59.67% | **61.67%** | **+5.64 pp** |
| single-hop | 96.00% | 96.00% | **96.00%** | 0.00 pp |
| temporal | 92.00% | 86.67% | **96.00%** | 0.00 pp |
| **overall** | **78.48%** | **76.31%** | **79.64%** | **+2.78 pp** |

Rendered Hit is 89%. Search latency is 1.71 s mean, 2.52 s p95, and
2.97 s maximum, so the 4 s product fail-open gate passes. The two target weak
types each improve by more than five points while single-hop and temporal
remain byte-for-byte equal at the aggregate gate.

The reader contract now requires each reflected Collection item to cite source
IDs copied from the supplied product evidence and deterministically restores a
validated item if final generation omits it. Inference permits a short stable
ordinary-world bridge from an explicit personal premise while prohibiting new
personal facts. A loopback-only OMLX
`Qwen3.6-35B-A3B-4bit` n=8 integration smoke, with API keys removed and
thinking disabled, produced 61.28% official F1; all four reflection outputs
parsed as complete JSON. This is a contract/integration gate, not a replacement
for the existing n=100 answer-quality headline.

Evidence:
`local-answer-locomo-v175-production-full-n100.json`,
`/tmp/anamnesis-v176-omlx-qwen-gate-source-n8.json`, and
`/tmp/anamnesis-v176-omlx-qwen-gate-answer-n8.json`.

## 2026-07-29 final local product-profile gate

The accepted defaults are cognitive search@20, BGE-base candidate@50,
automatic intent-aware evidence selection, and final@20. Internal search and
reranker widths are independent of the delivered context limit. Direct
queries preserve reranker order so the MCP hook's post-package
`knowledge_only` filter retains Semantic alternatives; inference and temporal
queries use canonical-source coverage; enumeration, relationship, and
frequency queries use bounded source-session coverage.

Reader-free LoCoMo seed-42 balanced n=100:

| Type | candidate@50 | delivered@20 | rendered recall | rendered Hit |
|---|---:|---:|---:|---:|
| multi-hop | 62.07% | 57.40% | 59.40% | 88% |
| open-domain | 56.03% | 53.67% | 56.03% | 68% |
| single-hop | 96.00% | 96.00% | 96.00% | 96% |
| temporal | 92.00% | 86.67% | 96.00% | 96% |
| **overall** | **76.53%** | **73.43%** | **76.86%** | **87%** |

The final-width n=20 screen held search@20 and candidate@50 fixed. Qwen 3.6
no-thinking semantic accuracy / official F1 were 60% / 41.71% at final@8,
60% / 42.31% at final@12, and 65% / 43.35% at final@20. Mean prompt sizes
were approximately 794, 1,166, and 1,841 tokens respectively. On this screen,
shrinking the context lost evidence; final@20 did not show net context
pollution.

The unified local-reader gate used loopback OMLX
`Qwen3.6-35B-A3B-4bit`, `enable_thinking=false`, and query-plan-gated
reflection for 43 complex questions:

| Type | Semantic accuracy | Official F1 |
|---|---:|---:|
| multi-hop | 48% | 36.77% |
| open-domain | 52% | 36.33% |
| single-hop | 84% | 72.91% |
| temporal | 80% | 50.44% |
| **overall** | **66%** | **49.11%** |

All 100 local judge responses parsed. Of 34 judged-wrong answers, nine had no
rendered evidence hit, ten had partial rendered coverage, and fifteen had
complete rendered coverage.

After explicit approval, the same frozen product contexts were answered by
GPT-4o with the same complex-only reflection policy and judged by GPT-4o:

| Type | Qwen official F1 | GPT-4o official F1 | GPT-4o delta |
|---|---:|---:|---:|
| multi-hop | 36.77% | 38.24% | +1.48 pp |
| open-domain | 36.33% | 26.83% | -9.51 pp |
| single-hop | 72.91% | 56.67% | -16.24 pp |
| temporal | 50.44% | 60.03% | +9.59 pp |
| **overall** | **49.11%** | **45.44%** | **-3.67 pp** |

The GPT-4o judge marked 55% correct with zero parse failures. That semantic
score is not directly comparable to the 66% Qwen-judged score because the
judge changed; official F1 is deterministic and comparable. GPT-4o improved
multi-hop and temporal F1 but regressed open-domain and single-hop. Of its 45
judged-wrong answers, 11 had no rendered hit, 15 had partial coverage, and 19
had complete rendered coverage. This rejects the assumption that a stronger
provider alone closes the current synthesis residue.

The frontier run used 492,561 input and 11,410 output tokens. The harness's
declared GPT-4o pricing estimate was $1.345503, below the authorized $5 cap.

Evidence:
`local-answer-locomo-v155-final-production-default-c50-search20-final20-n100-source.json`
and
`local-answer-locomo-v156-final-omlx-qwen36-nothink-production-c50-final20-reflect-complex-n100-answer-judge.json`
and
`local-answer-locomo-v157-final-gpt4o-production-c50-final20-reflect-complex-n100-answer-judge.json`.

## Product boundary

The program keeps recall orchestration model-agnostic and synchronous. Raw
episodic fragments remain authoritative graph nodes. The engine accepts a
`RerankingProvider`; its optional FastEmbed adapter mirrors the existing
embedding adapter, while generation and extraction stay in `anamnesis-mcp`.
Existing public APIs remain compatible.

The headline lane renders the exact product context returned by canonical
reranked recall. Retrieval, selection, delivered package, rendered
evidence, raw official-compatible Answer F1, reference-blind reader-surface F1,
and local semantic judgment are reported separately.

Deterministic evidence compilation belongs to the higher-level `Memory`
facade, not to each consumer. `Engine` remains the graph/search/storage kernel
and does not call a language model. `anamnesis-mcp` remains a thin transport and
optional model adapter.

## Reproduced baseline

Frozen reader and dataset controls:

- LoCoMo non-adversarial, seed 42, 25 questions per type, n=100;
- local `qwen3.6:35b-a3b`, no thinking, temperature 0;
- `Xenova/bge-base-en-v1.5`;
- BGE base reranker over candidate@100;
- first-stage seed limit 10.

| Metric | top 10 | top 20 |
|---|---:|---:|
| Candidate true recall | 84.07% | 84.07% |
| Selected/delivered true recall | 62.15% | 73.02% |
| True rendered recall | 72.83% | 78.64% |
| Exact rendered Hit | 84.00% | 88.00% |
| Raw Answer F1 | 43.14% | 47.12% |

Top 20 is not promoted: its question bootstrap interval is positive, but its
conversation-cluster interval crosses zero.

## Bottleneck isolation

Reader-free sweeps established:

| Experiment | Result | Decision |
|---|---|---|
| first-stage seed 10 → 32 → 64 | candidate recall did not improve; final rendered recall regressed | keep 10 |
| cognitive trace top 100 / 200 / 512 | 84.07% / 89.39% / 95.65% macro gold recall | preselection/reranking is the memory-side bottleneck |
| candidate 100 → 200 with BGE base | more candidate recall, worse final top 10/20 | do not widen a weak reranker blindly |
| v2-m3 candidate 200 → 512, n=20 | candidate recall 87.08%→91.25%; reranker@20 and rendered recall unchanged at 73.33% / 77.08% | reject full-width scoring; recover missing hops through a bounded auxiliary lane |
| deterministic evidence-sentence bridge + weighted multi-query RRF, multi-hop n=5 | candidate@228 recall 90.00%; every question's rendered recall unchanged, aggregate 53.33%→53.33% | reject and remove; query-time pseudo-feedback does not solve candidate-to-final transfer |
| Memory-owned automatic rerank documents, v2-m3 n=100 | delivered recall 73.11%→76.35%; rendered 81.42%→82.60%; Hit 91%→92%; multi-hop rendered +4.74pp | retain as Answer candidate, not default: 6 wins / 93 ties / 1 loss and both retrieval CIs cross zero |
| representative-window-only / source+window documents | n=20 improvement 0 / n=100 rendered +0.60pp, each weaker than raw-source documents | reject both; keep score/render alignment and the stronger raw-source lane |
| source-turn dedup + backfill | +1.67pp rendered recall on n=20 top 20; +0.26pp F1, interval crosses zero | retain as opt-in diagnostic, not default |
| deterministic count/location/type decomposition | +1.8pp multi-hop selected/rendered recall when replacing the wrapper; no candidate gain | preserve original query and use decomposition only as auxiliary recall |

The trace-512 result means the graph activation surface already contains most
gold evidence. Removing graph mechanics or replacing the product with a
vector-only store is not supported by the measurements.

## Frontier-quality reranker screen

`rozgo/bge-reranker-v2-m3`, candidate@200, top 20, n=20:

| Metric | BGE base candidate@100 | BGE v2-m3 candidate@200 | Delta |
|---|---:|---:|---:|
| Candidate recall | 80.42% | 87.08% | +6.67pp |
| Delivered recall | 67.08% | 73.33% | +6.25pp |
| Rendered recall | 69.58% | 77.08% | +7.50pp |
| Exact rendered Hit | 85.00% | 90.00% | +5.00pp |
| Qwen 3.6 raw Answer F1 | 46.75% | 48.51% | +1.76pp |

The paired answer result is one win, nineteen ties, zero losses; both bootstrap
lower bounds equal zero. The reranker is also much slower than the shipped BGE
base profile. It is therefore a named frontier-quality candidate, not the
latency default. An n=100 reader-free gate is required before an n=100 reader
run.

The reader-free n=100 gate now passes its directional screen:

| Type | candidate@200 | delivered@20 | rendered recall | rendered Hit |
|---|---:|---:|---:|---:|
| multi-hop | 83.12% | 51.07% | 65.64% | 92% |
| open-domain | 82.45% | 53.36% | 68.03% | 80% |
| single-hop | 96.00% | 96.00% | 96.00% | 96% |
| temporal | 96.00% | 92.00% | 96.00% | 96% |
| **overall** | **89.4%** | **73.1%** | **81.4%** | **91%** |

The same fixed ranking reaches 84.9% rendered recall and 93% Hit at top 32
(mean 3,705 tokens). This justifies a Qwen 3.6 answer run; it does not itself
promote BGE v2-m3 because Answer F1 and cluster-paired intervals remain gates.
The uncascaded profile is offline-only on the measured machine: mean search
latency is 24.9s, p95 33.9s. A consumer-owned BGE-base prefilter → v2-m3
quality-reranker cascade is therefore screened separately; neither model enters
the core or the latency default.

The completed n=100 Qwen 3.6 answer lane scores 48.38% raw official-compatible
F1 and 52.0% with the separately versioned local semantic judge (100/100
parsed). Against the same-reader BGE-base top-20 baseline, raw F1 is +1.26pp
(14 wins, 77 ties, 9 losses), but both paired intervals cross zero:
question-bootstrap `[-2.04, +4.76]pp` and conversation-cluster
`[-1.87, +3.94]pp`. Candidate recall rises +5.32pp while selected recall rises
only +0.09pp, locating the current memory-side bottleneck at candidate-to-final
selection. The uncascaded profile is therefore still not promoted.

The first latency cascade screen (BGE base 200→64, then v2-m3) cuts n=20 mean
search latency from 25.7s to 14.8s, but top-20 rendered recall falls from
77.08% to 73.75%. It is rejected as-is. A second screen fuses the exact
deterministic query variants recorded by core at the fast prefilter only; this
keeps decomposition aligned with the product search plan without moving either
model into core. That screen is also rejected: retrieval is byte-for-byte
unchanged while mean latency rises to 16.9s (p95 19.1s).

Consumer-owned source coverage is more promising. It preserves v2-m3 order,
skips a Semantic candidate only when all of its raw `ExtractedFrom` sources
have already been covered, and backfills from the next candidate. No session
hard quota is used. On n=20 top 10, selected recall rises 63.17%→69.17% and
rendered recall 71.67%→74.17%. Frozen Qwen 3.6 raw F1 rises
45.42%→47.17% (1 win, 19 ties, 0 losses), while semantic accuracy remains
55%. Both bootstrap lower bounds equal zero, so this is an n=100 retrieval
candidate, not a promotion.

The n=100 gate separates the two outcomes. On one frozen v2-m3 ranking,
source coverage improves top-10 rendered recall 74.88%→76.78% and Hit
88%→89% (6 wins, 94 ties, no retrieval losses). Both reader-free paired
intervals are positive: question `[+0.50,+3.70]pp`, conversation cluster
`[+0.74,+3.63]pp`. It nevertheless changes the actual rendered context on 90
questions, not only the six whose gold recall changes. Frozen Qwen 3.6 raw F1
falls 47.99%→45.93%; question and cluster intervals both cross zero
(`[-5.69,+1.09]pp` and `[-6.18,+1.09]pp`). The six recall-improved questions
gain only +1.39pp mean F1, while the other context substitutions dominate.
Source coverage therefore passes the retrieval ablation and fails product
promotion.

`--consumer-ranking-report` makes this result reproducible without rerunning
the consumer model. A replay is accepted only when dataset, sample, candidate
surface, seed limit, derived artifact, reranker identity, and every replayed
node agree with a fresh core readout. The n=20 and n=100 replays reproduced the
live product packages exactly. `--paired-answer-report` additionally reuses an
answer or judge only when the complete reader prompt, prompt version, model
digest, and generation settings agree. In this experiment the ten
byte-identical prompts produced byte-identical Qwen answers; the other 90 were
correctly regenerated.

On those same 20 questions, the Qwen 3.6 oracle-evidence route scores 52.31%
raw F1 versus 48.51% for the frontier retrieval route (3 retrieval wins, 10
ties, 7 losses). This bounds the observed short-run retrieval-to-reader gap at
3.80pp; it does not make 52.31% a model-wide ceiling.

Increasing that same v2-m3 product package from top 20 to top 32 is rejected.
Rendered recall rises 77.08%→80.00% at a mean 3,678 tokens, but Qwen 3.6 raw
F1 falls 48.51%→46.18% (one win, 17 ties, two losses). The two questions whose
rendered recall improves have exactly zero mean F1 change. The paired question
interval is `[-6.83,+0.83]pp` and the conversation-cluster interval is
`[-4.91,0.00]pp`. Wider context is not the remaining quality lever; evidence
selection/compilation inside the available top-20 budget is.

A gold-blind Qwen 3.6 evidence-selector shadow was also rejected. It received
only the question, question time, exact top-32 product context, and candidate
node/text map; it returned evidence ids, then the live engine rebuilt and
validated/repackaged the top 20. Retrieval was exactly unchanged at 77.08%
rendered recall and 90% Hit, but raw F1 fell 48.51%→46.76% (zero wins, 19 ties,
one loss; both interval upper bounds equal zero). Reordering/scoring the same
evidence surface adds reader variance without measured benefit, so the selector
prototype is not retained in product code.

Two broader query rewrites were also removed after reader-free n=20 screens.
Fusing the core's deterministic variants at the quality reranker changed four
rankings and all 20 score-bearing product contexts without changing candidate,
delivered, rendered recall, or Hit. A generic stopword-stripped exact-term
channel changed 15 rankings/contexts but likewise changed none of those
retrieval metrics. Neither proceeds to an Answer run; the narrow,
already-measured deterministic decompositions remain auxiliary and the two
failed generalizations leave no code or default knob behind.

## Memory-owned deep readout

The additive `Memory::search_deep[_at]` and
`Memory::repackage_reranked_deep[_at]` APIs now compile model-free evidence in
the product facade. They:

- classify only bounded retrieval intent;
- group Semantic windows and derived representations by canonical raw
  `ExtractedFrom` sources;
- preserve pure relevance order for direct and temporal lookups;
- use source coverage only for explicit enumeration/relational questions;
- delegate final validity, tension, packaging mode, token budget, commit trace,
  and source-provenance validation to the existing product repackage path.

`RecallPlan` is now the single deterministic planning contract shared by deep
selection and query-aware rendering. It separates retrieval intent from
`AnswerShape`: a question constrained by an explicit date can request a factual
answer, while a question asking for a date requests a temporal answer. The
default parser uses token-sequence locale rule packs rather than sentence-prefix
matching, so polite wrappers, inverted English questions, and spaced/unspaced
Korean forms follow the same path. Consumers still pass only the original query;
structured consumers may supply an optional typed answer-shape hint through
`RecallPlan::infer_with_answer_shape`. No model call, benchmark reference, or
expected answer enters planning.

`Memory::rerank_documents` is the matching minimal-consumer contract. Direct
and temporal queries preserve the ordinary graph-node documents. Enumeration
and relational queries compile overlapping Semantic windows into
speaker-qualified raw Episodic evidence documents, with each raw source emitted
once and a live readout node retained as the score handle. The consumer only
scores those strings. `Memory::repackage_reranked_deep_at` still owns ranking
validation, validity windows, tensions, packaging mode, source preservation,
and commit-trace reconstruction.

The first canonical-document screen exposed and fixed a score/render mismatch:
scoring a raw source while packaging only its Semantic representative improved
selected recall but regressed rendered recall. The retained path uses the raw
source node itself whenever it is present on the candidate surface, so the text
scored by the reranker is the text delivered by the product package.

The n=100 automatic-document candidate remains a frontier profile rather than
the default:

| Metric | same-prompt v2-m3 deep baseline | automatic documents | Delta |
|---|---:|---:|---:|
| Delivered recall@20 | 73.11% | 76.35% | +3.24pp |
| Rendered recall | 81.42% | 82.60% | +1.18pp |
| Rendered Hit | 91.00% | 92.00% | +1.00pp |
| Multi-hop rendered recall | 65.64% | 70.38% | +4.74pp |
| Qwen 3.6 raw Answer F1 | 50.69% | 52.07% | +1.38pp |

The six questions whose rendered recall improved gain +10.14pp mean Answer F1,
so the retrieval change does transmit to the reader. The overall Answer
comparison is 6 wins / 91 ties / 3 losses. Its question bootstrap interval
`[-0.11,+3.31]pp` and conversation-cluster interval
`[-0.018,+2.89]pp` narrowly cross zero. The corresponding retrieval comparison
is 6 wins / 93 ties / 1 loss and also crosses zero. An n=200 paired gate is
therefore required before promotion.

The benchmark's `memory-deep` policy invokes these exact public APIs. The
consumer supplies only an optional externally produced ranking; it does not
reimplement source grouping or selection. On the frozen n=100 v2-m3 ranking,
the first conservative deep-readout version changed five explicit enumeration
contexts and moved raw Qwen 3.6 F1 from 48.38% to 48.47% (one win, 99 ties, no
losses). This is safe but not independently promotable.

Two broader renderers were screened and removed. A replacement ledger layout
fell to roughly 41% on n=20. A prepended query-focused excerpt index moved
48.51% to 48.48% on n=20 (two wins, fourteen ties, four losses). Neither API,
flag, or dead implementation remains.

The benchmark reader prompt no longer contains a literal example date. A
replay showed Qwen copying the old `15 July 2023` example into an unrelated
answer even though the memory context contained the correct resolved date.
Schema 32 / `official-format-v7-reference-free-temporal` keeps the temporal
reasoning instruction but uses no calendar anchor. This is a benchmark
fidelity fix and must be reported separately from memory retrieval gains.

### Reference-blind temporal evidence compilation

`Memory::render_context_for[_with]` adds query-aware context rendering without
changing ranking or package membership. Only questions that ask for a date,
day, week, month, or year activate it. It examines at most the four
highest-ranked packaged fragments, requires deterministic subject/action
overlap with the question, and resolves auditable evidence expressions such as
`yesterday`, `last Friday`, `this week`, `next month`, `two days ago`, and a
bare ordinal such as `the 17th` against the fragment's immutable observation
time. The output retains both the natural temporal relation and the absolute
date/range. Raw text, observation/validity times, provenance, tensions, and
commit semantics are unchanged.

Frozen LoCoMo non-adversarial n=100, seed 42, top 20, identical v2-m3 ranking
and `qwen3.6:35b-a3b` reader:

| Metric | deep readout | + temporal evidence compilation | Delta |
|---|---:|---:|---:|
| Candidate recall@200 | 89.39% | 89.39% | 0 |
| Delivered recall@20 | 73.11% | 73.11% | 0 |
| True rendered recall | 81.42% | 81.42% | 0 |
| Exact rendered Hit | 91.00% | 91.00% | 0 |
| Raw official-compatible Answer F1 | 48.47% | 51.01% | +2.54pp |

The answer comparison is 9 wins, 86 ties, and 5 token-F1 losses. Manual
inspection found that all five losses were semantically correct absolute
dates/ranges replacing an equivalent relative phrase, not new answer errors.
Nevertheless both paired intervals still cross zero: question bootstrap
`[-1.51,+6.90]pp`, conversation-cluster
`[-1.48,+6.94]pp`. This is therefore an n=100 frontier candidate, not a
default promotion or a leaderboard claim. It must pass the existing n=200
question and conversation-cluster gates before promotion.

A fixed 4:1 consumer/cognitive reciprocal-rank fusion is rejected for the same
reason. It raises selected exact recall 73.33%→74.58%, but rendered recall and
Hit remain exactly 77.08% and 90%. The graph rank only substitutes another
representation of evidence already present through Semantic windows; it
changes reader context without delivering a new gold unit. No Answer run is
performed and the fusion policy is not retained.

## Evidence context wire

`Memory::render_context_with` now offers an additive `Evidence` style over the
exact same validated package. It groups evidence by source session, restores
chronological order across sessions and within each session, retains
observation/validity times, omits diagnostic scores and origin boilerplate, and
coalesces exact raw turns that are duplicated inside overlapping Semantic
windows. It does not alter selection, package membership, validity filtering,
tensions, commit traces, or graph state. MCP recall exposes the same product renderer with
`ANAMNESIS_CONTEXT_STYLE=evidence`; the benchmark uses it with
`--evidence-context`.

A fixed-ranking n=20 screen confirms that this style preserves candidate,
selected, delivered, and rendered recall exactly. It reduces mean rendered
characters from 12,103 to 7,425, but Qwen 3.6 raw F1 falls
46.75%→43.83% (1 win, 17 ties, 2 losses; both intervals cross zero).
The compact style remains an explicit product option and is not the benchmark
or MCP default.

## Product-safe extraction

The opt-in extractor now defaults to local `qwen3.6:35b-a3b` with thinking
disabled and a strict structured-output schema. The built-in profile calls the
non-streaming Ollama chat API on an HTTP loopback address, avoiding the
interactive CLI's cursor-rewrite stream; explicitly configured non-Ollama
providers retain the bounded subprocess path. The staged schema
supports generic facts, entities, events, preferences, decisions, causal
knowledge, lessons, conventions, and gotchas, plus selective entity tags and
optional absolute validity windows.

Extraction remains shadow-first. Automatic graph mutation is still forbidden.
An operator may explicitly promote only a reviewed `supported`,
uncontaminated candidate whose source snapshots still match. Promotion:

- creates one isolated atomic-fact sidecar record, not a graph node;
- preserves every raw episodic source;
- restores source scope;
- inherits the latest cited source observation time rather than promotion wall
  time, so review does not manufacture recency;
- stamps profile, candidate, and idempotency metadata;
- applies entity tags and validity windows;
- stores every validated raw Episodic source ID as provenance;
- remains outside node FTS, attraction, forgetting, and graph budgets;
- records the committed atomic-fact id;
- is safe to retry.

A reviewed-correct relation can be promoted only after both endpoints. The
typed `reason`, `causal`, `contradicts`, or `supports` relation is recorded
idempotently in sidecar metadata rather than as a graph edge. Compatibility
response fields named `node_id` and `edge_id` alias the new
`atomic_fact_id` and `atomic_relation_id`; they do not identify graph objects.
The policy schema migrates existing v2 audit rows to v3.

## Remaining release and leaderboard gates

1. Run one unified n=100 local-Qwen answer/judge pass over the new v11
   retrieval contexts; the n=8 gate above validates integration only.
2. Keep the compact evidence renderer opt-in; its fixed-ranking screen
   preserved retrieval but failed Answer F1 transfer.
3. Run the semantic-answer judge as a separately versioned lane, identically
   for Anamnesis and competitor adapters.
4. Require paired question and conversation-cluster lower bounds above zero at
   n=200 before making a cross-system quality claim.
5. Run the full declared LoCoMo split and competitor adapters only for the
   single n=200 winner.

The competitor execution seam is implemented as a strict external-memory
artifact lane. It accepts exact returned contexts but no answer/gold/relevance
fields, requires the selected question set and dataset fingerprint to match,
and runs the same Qwen 3.6 reader and versioned semantic judge. It deliberately
leaves Anamnesis-only retrieval stages absent instead of filling them with
misleading zero or synthetic metrics.

## Frontier mechanism map

The current Mem0 v3 description attributes its 2026 gains to ADD-only atomic
extraction, first-class agent facts, entity linking, semantic+BM25+entity
fusion, and temporal ranking. These are compatible with Anamnesis when mapped
to consumer-owned extraction and model-free core mechanics:

| Mechanism | Anamnesis status | Promotion rule |
|---|---|---|
| ADD-only atomic memories | isolated sidecar populated by reviewed Qwen 3.6 extraction; raw fragments retained | only raw cited sources may enter the evidence lane |
| source provenance | sidecar source IDs plus source snapshot checks | already a hard promotion and routing invariant |
| entity linking | selective tags stored on atomic facts; explicit graph entity seed API remains | do not restore broad speaker cues; test exact selective query entities only |
| semantic + lexical fusion | dense, FTS/BM25, and graph activation already fused | query decomposition remains auxiliary unless paired gates pass |
| temporal state | observation and validity times reach the product context | non-seed temporal refresh stays deferred because temporal candidate recall is already 96% |
| evidence compilation | compact product `Evidence` renderer and source-coverage selector | report retrieval and answer transfer separately |

This map is not a claim of score parity. It identifies which competitor
mechanisms can be adopted without moving models, HTTP, benchmark gold, or
consumer policy into the synchronous engine.

The product-correctness pass also narrows automatic `Timeline` packaging.
Explicit parsed time ranges still select it, while English cue words now
require token boundaries (`when`, not `whenever`; `after`, not `afterparty`).
Korean `최근`/`언제` cues remain supported. This changes neither retrieval
scores nor the public API; it prevents unrelated queries from silently
receiving a chronological package.

## Frozen extraction benchmark contract

`local_answer --derived-memory-artifact <json>` accepts schema version 1. The
artifact is fingerprint-bound to the source dataset and records the exact local
Qwen 3.6 model digest and extraction prompt version. Its wire contains only
derived content, entity tags, optional validity bounds, source session/turn
identifiers, and typed relations; there is no answer, question, gold-unit, or
relevance field.

At ingest, every cited turn must resolve to a raw Episodic node in the declared
session. Each record becomes one isolated atomic fact carrying those raw source
IDs, source scope/session, and validity bounds; it does not become a graph
node. Cross-sample relations, invalid windows, missing sources, duplicate ids,
duplicate relations, and non-Qwen-3.6 artifacts fail closed. The no-artifact
path remains byte-for-byte the normal product ingest recipe, so extraction is
an explicit ablation rather than a hidden baseline change.

Artifact generation now uses the mutation-free `anamnesis extract-preview`
command, which runs the same versioned product prompt/provider/validator over
an explicit 1–20 source batch. The LoCoMo generator reads no QA fields and
checkpoints each local Qwen 3.6 batch. Ingest keys provenance by stable
sample-qualified session id, not the dataset's reused raw session/turn ids; a
two-sample collision regression test prevents derived-memory contamination.
Prompt v7 aligns the provider JSON Schema with every validator bound and
requires standalone atomic claims, so terminal control bytes, source-node
references in memory text, cross-person claim merging, and overfull batches
fail before staging.

The completed local artifact contains 2,278 unique reviewed-shape records and
six typed relations from 399 source batches. Its SHA-256 is
`93f485927cf40271975ac2932cfa0ef9cc3aa53fe4e68b1c32b9e4615da0513b`.
One singleton source failed the strict schema and was omitted fail-closed with
its sample/session/turn/hash retained in the checkpoint. No benchmark question,
answer, or gold evidence enters extraction.

The first materialization experiment exposed one modeling error: even a single
derived Semantic graph node changes node FTS statistics and competes with raw
evidence. `Memory::add_derived_knowledge_with` remains available as an
additive compatibility API, but the accepted extraction path now uses
`Memory::add_atomic_fact`. Its records live in the isolated sidecar and route
back to raw Episodic sources without becoming graph candidates. Existing note
and derived-knowledge APIs and signatures are unchanged.

The held-out retrieval result rejects broad materialization:

| Extraction policy | candidate surface | candidate recall | rendered recall | Hit |
|---|---:|---:|---:|---:|
| no extraction baseline | 100 | 84.07% | 78.64% | 88% |
| all 2,278 derived records | 100 | 80.7% | 71.3% | 82% |

A temporary automatic-source packaging prototype showed that candidate
widening can recover the evidence surface while final selection worsens,
isolating derived-node crowd-out at the consumer boundary. That prototype was
removed: normal `Balanced` packaging keeps its existing meaning, while
`KnowledgeWithProvenance` remains the explicit mode that accompanies knowledge
with raw sources.

A product-compatible consumer guardrail then limited derived nodes to at most
one of each four prefix positions and backfilled with raw candidates. Because
`repackage_reranked_at` treats scores, not caller order, as authoritative, the
guardrail also assigns finite strictly descending positional scores after
reordering. It recovers all-derived rendered recall from 71.3% to 76.47%, but
still loses 2.17pp against no extraction. Both intervals are strictly negative:
question `[-4.33,-0.50]pp`, conversation cluster `[-4.37,-0.45]pp` (zero wins,
95 ties, five losses). It remains an explicit diagnostic/safety policy, not a
default.

Restricting materialization to the 264 records supported by at least two raw
turns also fails: 77.16% rendered recall, −1.48pp paired, zero wins, 96 ties,
four losses. Its question interval is `[-3.17,-0.14]pp`; the conversation
interval is `[-2.78,0.00]pp`. This closes the broad-materialization track.
Extraction remains shadow/review-first, and derived graph mutation is not a
default recall path.

## End-to-end 85% feasibility gate

The target is now defined as at least 85% non-adversarial LoCoMo semantic
answer accuracy on a complete end-to-end product-context lane. Raw
official-compatible token F1 and every retrieval stage remain mandatory
separate metrics; semantic judgment never replaces or rewrites them.

The original n=100 answer prompt selected instructions from the dataset's gold
`question_type`. Its 57% semantic accuracy and 52.07% raw F1 remain useful only
as a historical diagnostic; they are not a deployable product baseline. The
oracle-evidence result (66% / 55.5%) used the same leaked prompt and is likewise
diagnostic rather than a clean model ceiling.

Prompt v9 removes that dependency. It derives a public, deterministic
`RecallPlan` from the question text and maps its `AnswerShape` and
`RecallIntent` to reader instructions. The parser now recognizes duration,
reason, inference, and yes/no forms without benchmark annotations. The answer,
reflection, and final-verification prompts receive no gold type, answer, or
judge feedback.

The frozen v2-m3 n=100 replay first established the honest Q4 local boundary:

| Reader lane | Semantic accuracy | Raw F1 | Mean answer latency | Paired change |
|---|---:|---:|---:|---|
| query-only one-pass v9 | 56% | 49.48% | 6.10 s | baseline |
| Reflect v3 on every question | 62% | 51.01% | 24.96 s | 12 recoveries / 6 regressions |
| query-plan-gated Reflect v3 | 61% | 52.55% | 13.39 s | 7 recoveries / 2 regressions |

The gated route reflects only `Count`, `Collection`, and `Inference` plans.
It leaves `Fact`, `Temporal`, and `Relationship` plans on the one-pass reader.
This decision is reference-blind and uses the same public parser as retrieval.
On n=100 it reflected 37 questions, had zero semantic regressions for
single-hop and temporal questions, and improved F1 by 7.72pp for multi-hop and
4.56pp for open-domain questions. Its p50 latency was 5.32 s; full Reflect's
mean was 24.96 s.

The local precision screen then held every product context, query-derived plan,
prompt, generation option, and retrieval metric fixed while changing only the
reader quantization. `qwen3.6:35b-a3b-q8_0` passed the frozen n=20 gate:
one-pass reached 55% semantic accuracy and 43.1% raw F1, while gated Reflect
reached 65% and 45.4%. On n=100, the Q8 gated route reached 66% semantic
accuracy (95% CI 56.3%..74.5%), 51.91% raw F1, and 14.29 s mean answer latency.
The separately rejudged Q4 gated route reached 64%, 52.55%, and 13.39 s under
the same semantic-judge prompt. Q8 therefore adds two semantic points but loses
0.64pp raw F1; it is a modest quality-first promotion rather than a broad
win.

The Q8 type breakdown is 88% single-hop, 80% temporal, 44% multi-hop, and 52%
open-domain. Against the Q4 gated answers, it has four semantic recoveries and
two regressions. Their reference-only correctness union is 68%, so even a
perfect query-only selector between these two installed quantizations could add
at most two more points on this sample. This makes 66% the practical local-Qwen
headline and 66%..68% the observed local precision plateau, not an 85% path.

The promotion path was gated before n=100. On the same frozen n=20 contexts,
the honest Q4 one-pass lane scored 45% semantic accuracy and 40.7% raw F1.
Q4 Reflect v3 scored 60% and 46.4%, with three semantic recoveries and no
losses. Q8 one-pass then scored 55% and 43.1%, and Q8 gated Reflect scored 65%
and 45.4%. The wider Q8 run confirmed the semantic direction, though not the
small-screen F1 direction.

These measurements also keep the evidence boundary visible. Candidate recall
is 89.39%, delivered recall 76.35%, rendered recall 82.60%, and rendered Hit
92%. Reader work alone cannot recover questions whose required evidence never
reaches the prompt, while the legacy oracle lane shows that the current local
Qwen reader is also far below the 85% target even with annotated evidence.

The MCP recall path now consumes the same
`Memory::search_reranked` orchestration and query-aware product renderer used
by the benchmark. The engine remains synchronous and model-agnostic behind
`RerankingProvider`; the FastEmbed adapter lives at the embedding/model
boundary. Scoped recalls apply scope during cognitive search before reranking;
tag and knowledge filters apply to the canonical package before gating and
rendering. Empty searches return a normal empty `Recall` instead of failing
reranker validation.

The remaining route to 85% is:

1. keep raising multi-hop/open-domain rendered evidence completeness without
   broad context substitutions;
2. expose the optional Reflect stage through `anamnesis-mcp`, with the model
   adapter outside `anamnesis-engine`;
3. run the same frozen contexts through a frontier answer/reflect model and a
   separately versioned judge once credentials are supplied;
4. promote only after n=200 paired question and conversation-cluster gates,
   then run the declared full LoCoMo split.

The answer harness now has an OpenAI-compatible route-3 adapter for that gate.
It reads the bearer token only from `LLM_API_KEY`, accepts the endpoint through
`LLM_BASE_URL` or `--frontier-base-url`, and records the route as non-local.
`--answer-report ... --retrieval-only --frontier-reader` preserves the frozen
route-2 answer and product context, then generates only route 3. This avoids
rerunning ingestion, embeddings, retrieval, or the local baseline reader for
each frontier-model screen. A one-question replay against Ollama's compatible
endpoint verifies that the stored context is preserved and both routes are
present in the output report. Local replays use `--run-reflect-reader` for the
cost-insensitive lane or `--reflect-complex-only` for the balanced lane.

## Multi-hop and open-domain structural gate

The low Q8 type scores were not accepted as a reason to substitute a stronger
reader immediately. A type-stratified, question-paired pass first separated
candidate generation, reranking, context rendering, and answer synthesis on
the same frozen 25 multi-hop and 25 open-domain questions used by the n=100
report.

The model-free changes are:

- query variants add bounded proper-noun anchors for factual bridge and
  enumeration questions, but suppress them for inference forms where an
  entity-only channel displaced useful relational seeds;
- inference is a first-class deterministic answer shape and relational recall
  intent, including yes/no and modal forms;
- enumeration and explicit relationship readout preserve up to four useful
  candidates from one source session before covering another session, then
  backfill by rank;
- manner questions (`How did ...`) and completed-action questions
  (`What has ... done`) map to relationship and collection readout
  respectively, while recurrence questions use a separate frequency shape;
- inference rerank documents retain the Semantic representative used for graph
  expansion while exposing canonical raw source text and removing only exact
  source-set duplicates;
- when an inference Semantic window ends on a question, its rerank document
  includes the immediate same-session raw answer and uses that raw answer as
  the commit-safe representative. The cross-encoder therefore scores the
  complete exchange without injecting generated text into the engine.

The rejected one-per-session prototype is important: it raised candidate recall
but reduced multi-hop reranker recall from 64.05% to 54.57%. It was removed.
The accepted bounded-burst policy produced no question-level multi-hop
retrieval regression:

| Multi-hop stage (n=25) | Frozen baseline | Structural pass |
|---|---:|---:|
| candidate recall | 83.12% | 84.69% |
| reranker/delivered recall | 64.05% | 66.62% |
| rendered recall | 70.38% | 73.95% |
| rendered Hit | 96% | 96% |

For open-domain, the final inference path preserved candidate recall, repaired
the question/answer truncation case, and raised both reranker and rendered
recall:

| Open-domain stage (n=25) | Frozen baseline | Structural pass |
|---|---:|---:|
| candidate recall | 82.45% | 82.45% |
| reranker/delivered recall | 53.36% | 65.67% |
| rendered recall | 68.03% | 68.76% |
| rendered Hit | 80% | 84% |

Against the immediately preceding structural report, only
`locomo-4-qa-5` changes: reranker, delivered, and rendered recall all move from
zero to one, with no question-level retrieval regression. The local Q8 reader
then answers `MinaLima store`, which the Q8 semantic judge accepts against
`House of MinaLima`. A broad 25-question answer replay has not been rerun, so
the last fully measured semantic headlines remain 44% for multi-hop and 52%
for open-domain; the newly recovered open-domain item is recorded separately
rather than silently relabeling the aggregate as 56%.

Two broader experiments remain rejected. One-candidate-per-session reduced
multi-hop reranker recall to 54.57%, while final-stage query-variant RRF reduced
multi-hop rendered recall from 73.95% to 71.29%. Neither path remains in the
accepted implementation.

The local Reflect reader now consumes its validated structured
`candidate_answer` directly for inference questions instead of asking the same
model to rewrite it in a second pass. Reflection is bounded to six relevant
reasoning steps and 600 tokens. This repairs concrete final-stage destruction
such as turning a supported “No” into an abstention or replacing “fitness
watch” with an unsupported device. Collection questions retain final
verification, with a non-empty reflected candidate used only when the verifier
returns the exact abstention sentinel. `--answer-report --resume` also resumes
only an identical route/model/prompt configuration and preserves completed
routes.

Despite the structural retrieval gains and the newly verified targeted answer
recovery, the last full paired type answer scores are 44% for multi-hop and 52%
for open-domain under both the Q8 and separately run Q4 judge-v3 checks. The
remaining failures are predominantly evidence-present synthesis, unstable
enumeration, ordinary world-knowledge bridges, and several dataset or
image-only cases. A provider handoff is justified only for these residual
reader failures; it is not a substitute for the retrieval defects repaired
above or an attempt to chase an absolute score.

### Local reader ceiling after structural repair

The preceding 44% / 52% values are retained as the pre-repair paired baseline.
Two complete, type-stratified n=25 answer screens now replace them for the
local ceiling analysis; neither changes the frozen retrieval contexts.

Full Q8 Reflect v8 raises multi-hop semantic accuracy from 44% to 56% and raw
F1 from 36.41% to 39.67%. The semantic comparison has three recoveries and no
regressions: the reader resolves `home country` to `Sweden`, combines Gina's
promotion methods, and completes Tim's writing types. Retrieval remains
84.69% candidate recall, 66.62% delivered recall, 73.95% rendered recall, and
96% rendered Hit.

On open-domain, the same current Reflect v8 path reaches 56% semantic accuracy.
A narrower v9 inference contract then reaches 60% and 39.94% raw F1. Against
v8 it recovers the road-trip implication, preserves both California and
Florida as Universal Studios alternatives, and normalizes Boston to the United
States; it regresses John Williams and Stamford-to-Connecticut in final
verification, for three recoveries and two regressions. The corresponding
retrieval surface remains 82.45% candidate recall, 65.67% delivered recall,
68.76% rendered recall, and 84% rendered Hit.

The inference contract is gold-blind. It says that `likely` / `might` / `could`
questions request the best supported plausible implication rather than
explicit confirmation, prefers evidence explicitly linked as a reason, goal,
preference, or consequence, preserves indistinguishable ordinary-world
alternatives, and requires the answer to match the semantic type named by the
question. It contains no LoCoMo entity, expected answer, or judge feedback.

A broader span-keyed findings ledger is rejected. It raised multi-hop raw F1
from 39.67% to 44.32% but reduced semantic accuracy from 56% to 48%, with no
semantic recovery and two regressions. The implementation and prompt version
were removed. A subsequent Fact/Relationship abstention-backfill prototype was
also removed without a score claim: the Q8 model could no longer load within
the frozen 600-second timeout after memory pressure increased, so the gate did
not complete.

The last complete single-hop and temporal semantic gates remain 88% and 80%,
and their current fixed retrieval replays remain byte-for-byte unchanged at
96% rendered Hit for both types. Combining the four separately completed
type screens would yield a 71% macro semantic diagnostic
(`88/56/60/80`), but this is not promoted as a new n=100 headline until one
unified Q8 run reproduces all four categories under the accepted v9 policy.

The residual split is now explicit. Of eleven incorrect multi-hop answers, all
eleven have at least one rendered gold hit; five have complete rendered gold
coverage and six are partial-coverage synthesis cases. Of ten incorrect
open-domain answers, two have no rendered gold hit, four have complete rendered
coverage, and four have partial coverage. A frontier provider request is
therefore scoped to evidence-present synthesis and open-world bridging; it
does not excuse the two open-domain retrieval misses or the partial-coverage
cases.

The answer-report runner now completes all answers before starting the local
judge phase. This preserves answers, prompts, scoring, and resume semantics
while avoiding a Q8/Q4 model unload and reload for every question. Interrupted
reports resume missing answers and missing judges independently.

### GPT-4o provider handoff

The requested GPT-4o handoff completed on the frozen seed-42 balanced n=100
diagnostic (25 questions per non-adversarial type). The source is a mechanical
merge of the four accepted type-specific retrieval reports; question ids,
retrieval contexts, and retrieval measurements are preserved rather than
recomputed. GPT-4o served as both Reflect reader and semantic judge.

| Metric | GPT-4o n=100 |
|---|---:|
| semantic judge accuracy | 64.0% |
| raw official LoCoMo F1 | 51.48% |
| judge parse failures | 0 |
| input / output tokens | 761,774 / 20,512 |
| standard-price cost estimate | $2.109555 |

| Type | Semantic accuracy | Raw F1 | Rendered recall | Rendered Hit |
|---|---:|---:|---:|---:|
| single-hop | 68% | 62.60% | 96.00% | 96% |
| multi-hop | 36% | 33.69% | 73.95% | 96% |
| open-domain | 68% | 42.76% | 68.76% | 84% |
| temporal | 84% | 66.88% | 96.00% | 96% |

The provider handoff therefore does not establish a higher aggregate ceiling:
the preceding unified Qwen run was 66% semantic / 51.91% raw F1, while the
latest type-specific Qwen screens were 56% multi-hop and 60% open-domain.
GPT-4o improves open-domain to 68% but regresses multi-hop to 36%.

On the identical multi-hop question set, seven cases move from correct under
the latest Qwen screen to incorrect under GPT-4o, while two move in the other
direction. The failures are dominated by incomplete collection answers. Two
bounded follow-ups reject obvious reader-routing fixes: direct GPT-4o recovers
2/7 of those regressions, while forcing a second synthesis call after Reflect
recovers 1/7. These diagnostics cost $0.065495 and $0.150743 respectively.
Together with the two-question smoke ($0.046805), total GPT-4o experimentation
was $2.372598, below the declared $2.50 stop.

The next multi-hop work should target evidence coverage and explicit
list-completeness against the retrieved context, not a broader provider swap.
This n=100 result remains a development diagnostic, not a full 1,540-question
non-adversarial LoCoMo headline.
