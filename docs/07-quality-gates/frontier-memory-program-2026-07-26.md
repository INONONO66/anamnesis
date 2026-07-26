# Frontier Memory Program — Measured Status

Date: 2026-07-26

Status: active development program; no full-split or cross-system leaderboard claim

## Product boundary

The program keeps the engine model-agnostic and synchronous. Raw episodic
fragments remain authoritative graph nodes. Local generation, reranking, and
extraction stay in the benchmark consumer or `anamnesis-mcp`; the engine only
accepts embeddings, rankings, observations, and typed relations. Existing
public APIs remain compatible.

The headline lane renders the exact product context through
`Memory::render_context`. Retrieval, selection, delivered package, rendered
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

- creates additive derived semantic knowledge;
- preserves every raw episodic source;
- restores source scope;
- inherits the latest cited source observation time rather than promotion wall
  time, so review does not manufacture recency;
- stamps profile, candidate, and idempotency metadata;
- applies entity tags and validity windows;
- links every source with `ExtractedFrom` through the narrow,
  type-validating `Memory::link_extracted_source` API;
- records the committed node id;
- is safe to retry.

A reviewed-correct relation can be promoted only after both endpoints. The
typed `reason`, `causal`, `contradicts`, and `supports` edge is recorded
idempotently. The policy schema migrates existing v2 audit rows to v3.

## Remaining promotion gates

1. Recover candidate-to-final selection without a negative conversation-cluster
   interval; wider candidates alone are insufficient.
2. Design a selection objective that improves evidence coverage without the
   broad context substitutions that caused source coverage to fail Answer F1.
3. Generate a frozen benchmark artifact through the same local generic extraction
   flow; never derive from benchmark gold.
4. Keep the compact evidence renderer opt-in; its fixed-ranking screen
   preserved retrieval but failed Answer F1 transfer.
5. Run the semantic-answer judge as a separately versioned lane, identically
   for Anamnesis and competitor adapters.
6. Require paired question and conversation-cluster lower bounds above zero at
   n=200 before any default change.
7. Run the full declared LoCoMo split and competitor adapters only for the
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
| ADD-only atomic memories | generic Qwen 3.6 shadow extraction; raw fragments retained | frozen artifact must improve held-out retrieval and paired F1 |
| source provenance | typed `ExtractedFrom` links and source snapshot checks | already a hard materialization invariant |
| entity linking | selective tags stored on derived nodes; explicit entity seed API exists | do not restore broad speaker cues; test exact selective query entities only |
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
session. A derived note is additive, uses the source session as its origin, and
is linked back to every raw turn. Cross-sample relations, invalid windows,
missing sources, duplicate ids, duplicate relations, and non-Qwen-3.6
artifacts fail closed. The no-artifact path remains byte-for-byte the normal
product ingest recipe, so extraction is an explicit ablation rather than a
hidden baseline change.

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

Materialization exposed and fixed one modeling error: the original note API
creates an Episodic + Semantic pair, so using it for an already-derived fact
duplicated every extracted statement. The additive
`Memory::add_derived_knowledge_with` API now creates exactly one Semantic node;
raw Episodic sources remain separate and are connected by
`ExtractedFrom`. Existing note APIs and signatures are unchanged.

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
