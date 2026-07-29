# Competitor Benchmark Protocol Audit

Date: 2026-07-25

Status: protocol audit and target-setting note; not a public benchmark claim

## Why the Headline Numbers Are Not One Scale

Anamnesis currently reports local Qwen 3.6 raw LoCoMo token F1 and exact
gold-unit retrieval coverage. The commonly cited competitor numbers use
different readers, context sizes, relevance definitions, and judges.

| System | Headline | What the published harness actually measures |
|---|---:|---|
| Mem0 | 92.5 LoCoMo | Cloud answerer plus binary LLM judge, up to 200 memories, explicit dates, extracted memories, and a benchmark-specific reasoning prompt |
| Supermemory | 83.5 Recall@10 | LLM relevance over the returned top 10; the open harness sets recall to 1 when any relevant result is returned, so this is Hit@10 rather than gold-unit recall |
| MemKraft | 98 LongMemEval | 50-question oracle subset, Claude Sonnet 4.6, three-run semantic majority; not LoCoMo and not retrieval evaluation |

Primary sources:

- <https://mem0.ai/research>
- <https://github.com/mem0ai/memory-benchmarks>
- <https://supermemory.ai/blog/latency-budgets-memory-retrieval/>
- <https://github.com/supermemoryai/memorybench>
- <https://github.com/seojoonkim/memkraft>

The numbers remain useful as product direction, but they cannot be compared
directly to Anamnesis raw token F1.

## Comparable Targets

Maintain two explicitly separate lanes.

### Product-local integrity lane

- reader: `qwen3.6:35b-a3b`;
- exact product context wire;
- raw official-compatible LoCoMo token F1;
- true gold-unit candidate, selected, delivered, and rendered recall;
- Hit@K, P@1, MRR, and NDCG reported alongside recall;
- paired question and conversation-cluster intervals.

### Industry-compatible diagnostic lane

- binary semantic judge applied identically to every compared system;
- the same answerer, judge, context cutoff, prompt, and dates;
- competitor-style Hit@10 named as Hit@10, never as true recall;
- no promotion decision based on this lane alone.

On the current Qwen 3.6 n=100 product-wire gate, exact rendered Hit@10 is 84%
and true rendered gold-unit recall is 72.83%. Supermemory's published 83.5
"Recall@10" is therefore closest to the first number, not the second. The
relevance definitions still differ, so this is an orientation result rather
than a superiority claim.

## Competitor Mechanisms Worth Adopting

These mechanisms are compatible with the Anamnesis product boundary:

1. Render observation/event dates with every evidence fragment. Relative
   expressions are otherwise underdetermined.
2. Preserve a broad candidate pool, then perform coverage-aware consumer
   selection before core validation and packaging.
3. Reassemble the final selected set against the full token budget instead of
   budgeting a large pool and discarding most of it afterward.
4. Extend deterministic temporal query signals to graph-reached candidates.
5. Add consumer-owned extraction of atomic facts, entities, events, and valid
   times with `ExtractedFrom` provenance while retaining every raw fragment.
6. Use a reader prompt that scans all supplied evidence, enumerates distinct
   events for count questions, and grounds relative dates.

These would violate the project boundary and are excluded:

- LLM inference or extraction inside the core crate;
- replacing raw episodic fragments with summaries or extracted facts;
- gold-aware selection, normalization, or prompting;
- changing the official token metric to inflate the headline;
- making a cross-encoder or network dependency part of the default core path.

## Measured Anamnesis Bottlenecks

Frozen Qwen 3.6 n=100, BGE-base embedding and reranker:

- candidate@100 true recall: 84.07%;
- selected@10 true recall: 62.15%;
- rendered true recall: 72.83%;
- rendered Hit@10: 84%;
- raw Answer F1: 43.14%.

Type diagnosis:

- single-hop: rendered recall 92.00%, F1 66.05%;
- multi-hop: rendered recall 52.64%, F1 34.45%;
- open-domain: rendered recall 58.00%, F1 31.93%;
- temporal: rendered recall 88.67%, F1 40.11%.

The temporal result proves that retrieval alone cannot close the gap. Evidence
time is absent from the old product renderer, so expressions such as "next
month" and "last weekend" cannot be grounded.

An offline cutoff sweep over the same reranker order showed:

| Cutoff | Selected true recall | Hit |
|---|---:|---:|
| 10 | 53.58% | 70% |
| 20 | 67.08% | 85% |
| 32 | 68.33% | 85% |
| 50 | 73.75% | 85% |

The actual top-20 product run increased selected recall by 13.5 points but
rendered recall by only 2.5 points and changed raw F1 by -0.53 points. The
package is budgeted before the final result limit, so discarded candidates do
not return their budget and many surviving Episodic fragments remain synthetic
labels. A larger K without final-set reassembly is therefore not a promotion
candidate.

## Newly Found Fidelity Defect

The benchmark dataset adapter stores epoch seconds, while the engine
`Timestamp` contract is epoch milliseconds. The graph builder previously passed
seconds directly into the engine, compressing a real day to 86.4 engine
seconds. This invalidated temporal-proximity and forgetting time scales.

The correct boundary is:

- dataset structures remain epoch seconds for source fidelity;
- graph ingest and question query times convert seconds to milliseconds;
- synthetic fallback gaps are expressed directly in milliseconds.

This repair is a fidelity correction, not a claimed quality improvement.

## Ordered Experiments

1. Correct timestamp units and render source-node observation/validity time on
   the actual MCP/product wire. Run paired Qwen 3.6 top-10.
2. Reassemble the already selected final set using the full token budget.
   Preserve validity, packaging mode, provenance sources, tension endpoints,
   and commit-trace semantics. Run paired top-10.
3. Freeze one candidate/reranker order and compare coverage-aware top-10,
   top-20, and token budgets without rerunning retrieval.
4. Extend non-seed temporal scoring and deterministic temporal query
   decomposition. Gate on retrieval and Answer F1 together.
5. Add generic consumer-side fact/entity/event extraction in shadow mode, then
   materialize only provenance-linked derived nodes after held-out success.

Near-term promotion targets are rendered Hit@10 at or above 85%, true rendered
recall at or above 75%, and local raw Answer F1 in the 45-50% range. The current
top-20 n=100 candidate reaches 88%, 78.64%, and 47.12%, respectively, but its
conversation-cluster interval still crosses zero. It is a target-reaching
candidate, not a promoted claim. Exceeding 50% will also require reader/context
improvement and cannot honestly be attributed to memory retrieval alone.

## 2026-07-26 Execution Update

The timestamp, final-set reassembly, label-only, validity-safe reranking, and
product-renderer fidelity defects above are fixed. Source coverage passed its
n=100 retrieval gate and failed Answer F1 transfer; the compact Evidence wire
also failed its fixed-ranking reader screen. Neither is a default.

The external comparison lane now accepts a fingerprint-bound context artifact
with no answer, gold, relevance, or judge fields. The local Mem0 execution is
frozen to:

- `mem0ai/memory-benchmarks@4b61c5d31b9c668a12b4f5e78064248a02c82d2b`;
- `mem0ai/mem0@b357a5a1b03c299ec8229c268e63cfac0f7c6566`;
- local `qwen3.6:35b-a3b` extraction;
- local `nomic-embed-text` embeddings;
- Qdrant;
- the same downstream Qwen 3.6 answer and semantic-judge controls as
  Anamnesis.

The upstream benchmark commit does not clean-build as published: its Docker
requirements reference the removed `feat/v3-pipeline` branch and omit Mem0's
optional `ollama` package. It also omits `fastembed` while the server describes
its route as semantic + BM25 + entity search; the unmodified image explicitly
logs that BM25 is disabled. The local reproduction pins the Mem0 commit above
and installs `ollama>=0.6.0` plus `fastembed>=0.3.1`.

Three provider/harness compatibility repairs are also required:

- the pinned Ollama adapter is made to send `think=false`;
- the pinned Mem0 search API receives identity constraints through its
  supported `filters` parameter rather than rejected top-level arguments;
- the synchronous OSS client timeout is raised from 300 to 1,800 seconds so a
  slow completed add is not retried and duplicated.

All deviations are recorded in `configs/mem0-oss-qwen36.yaml`; no memory
prompt, extraction instruction, query, result cutoff, or ranking algorithm is
patched. A ten-way ingestion attempt was rejected after proving that requests
can complete server-side after the 300-second client timeout. The evidence run
uses serial, checkpointed ingestion under a fresh isolated run id.

The official client sends a per-session epoch `timestamp`, but the OSS server
request model drops it. This cannot be repaired as a pass-through: the pinned
`Memory.add` implementation explicitly rejects non-null timestamps as a
platform-only feature. The local lane therefore reports this as a Mem0 OSS
temporal limitation. It does not inject benchmark dates into memory text or
change Mem0's extraction prompt to simulate the cloud feature.

The repaired checkout is reproducible rather than hand-maintained:

```bash
python3 scripts/prepare_mem0_local_benchmark.py /tmp/memory-benchmarks --build
```

The preparer verifies both upstream SHAs, applies exact-shape fail-closed
patches, copies the frozen Qwen configuration, and writes SHA-256 values for
every patched file to `anamnesis-local-reproduction.json`.

The two LoCoMo files are content-identical after removing upstream-only speaker
name fields and flattening the nested `conversation` object. Their canonical
JSON SHA-256 is
`224ada26f8e0454ca2d4fc178c76ec4238dbf704c36521f90c73ee09a1cdb784`.
Mem0 therefore ingests the upstream nested wire it expects, while the external
artifact is bound to Anamnesis's frozen flat-file fingerprint and exact
selected question ids.

The serial Qwen 3.6 ingestion is checkpointed and still in progress at this
record's cutoff. No Mem0 result is reported or inferred from a partial
conversation; a competitor score is eligible only after the declared
conversation set exports a complete strict external-memory artifact.
