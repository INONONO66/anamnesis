# Reproducible Local LoCoMo Answer Gate

Date: 2026-07-25

Status: frozen development gate, not a full-split or leaderboard claim.

This record supersedes temperature-sampled runs as the local **regression
gate**. Sampling remains useful for reader exploration, but it is not stable
enough for release comparisons: with temperature 0.7, unchanged non-temporal
prompts produced different answers on repeat runs. Greedy generation produced
identical answers, scores, and retrieval contexts on two complete runs.

## Frozen Protocol

| Item | Value |
|---|---|
| Dataset | pinned LoCoMo snapshot, FNV-1a64 `fdf74317b9a55716` |
| Loader | `locomo-caption-v2+longmemeval-cleaned-v1` |
| Selection | seed 42, 25 questions per non-adversarial type, 100 total |
| Memory path | shipped `Memory` search and `ContextPackage`, top 10 |
| Embeddings | `Xenova/bge-base-en-v1.5`, 768 dimensions |
| Reader | local `qwen3.5:35b-a3b`, digest `3460ffeede54…` |
| Runtime | Ollama 0.31.1, loopback only |
| Generation | non-thinking, temperature 0, presence penalty 1.5, seed 42 |
| Context/output | 32,768 / 512 tokens |
| Prompt | `official-format-v6-temporal-anchor` |
| Metric | official deterministic LoCoMo category-aware token F1 |
| Uncertainty | deterministic 10,000-resample question bootstrap |
| Judge | disabled; no same-model judge enters the primary score |

The temporal prompt resolves a relative phrase in an evidence item against that
item's date, not the later question date. The prior wording let the reader use
the question date even when annotated evidence carried the correct session
date. On the same 25 temporal questions, the corrected prompt moved retrieval
F1 from 10.9% to 27.0% (+16.1 points; paired 95% CI +3.7 to +30.0) and
annotated-evidence F1 from 12.7% to 35.2% (+22.6 points; paired 95% CI +4.4 to
+40.5). Two later wording variants scored worse and were rejected.

## Reproducible Answer Result

| Route | Official F1 | Bootstrap 95% CI | Package recall | Package hit |
|---|---:|---:|---:|---:|
| Memory top 10 | **34.88%** | **27.23–42.85%** | 52.93% | 66.00% |
| Dataset-annotated evidence | **48.14%** | **40.28–55.86%** | n/a | n/a |

Two independent greedy Memory runs matched on:

- 100/100 final answer strings;
- 100/100 per-question official F1 values;
- 100/100 ordered retrieval node/text lists.

Wall-clock latency fields differ and are deliberately excluded from this
semantic reproducibility check.

| Type | Memory top 10 | Annotated evidence | Gap | Package recall |
|---|---:|---:|---:|---:|
| Multi-hop | 26.76% | 38.14% | 11.38 pp | 32.70% |
| Open-domain | 20.19% | 42.98% | 22.79 pp | 22.33% |
| Single-hop | 60.02% | 75.81% | 15.79 pp | 80.00% |
| Temporal | 32.55% | 35.62% | 3.07 pp | 76.67% |

The overall annotated-evidence gap is 13.26 points, with a paired bootstrap
95% interval of +5.41 to +21.28 points. The dominant next target is therefore
open-domain and multi-hop evidence selection. Temporal is no longer evidence
of a graph defect on this sample; its remaining ceiling is primarily reader
and metric behavior.

For orientation, this 34.88% frozen development result is still below the
official paper's differently configured GPT-3.5 dialog-RAG top-10 result
(39.7%). The paper uses a different reader and full split including
adversarial questions, so the numbers are not directly comparable.

## Retrieval Signal and Embedding Audits

The full 1,540-question non-adversarial LoCoMo retrieval run with BGE-base
measured Recall@10 69.55%, MRR 46.20%, and NDCG@10 49.71%.

An offline fit split conversations by `sample_index` parity. The shipped
readout point (`w_a=.25`, `w_phi=16`, other fitted channels zero) beat the new
base-only fitted point on held-out conversations. Adding raw lexical score with
weight 2 produced only +0.01 NDCG points and -0.24 recall points on held-out
questions. Neither candidate was promoted to engine code.

The embedding audit also uncovered a protocol bug in the FastEmbed adapter.
FastEmbed reports E5-large as `Qdrant/multilingual-e5-large-onnx`, while the
adapter's old query/passage detector only recognized
`intfloat/multilingual-e5-*`. The first E5-large runs therefore embedded raw
text and are invalid as an asymmetric E5 claim. The adapter now detects both
identities, and the durable identity for a formerly raw Qdrant E5 space is
versioned with `+query-passage-v1` so an existing database is backed up and
re-embedded before use.

E5-small is different: FastEmbed reports it as
`intfloat/multilingual-e5-small`, so the old adapter already applied the correct
query/passage protocol. Its model identity remains unchanged, avoiding a
needless migration of the MCP default embedding space.

| Model | Full Recall@10 | Full MRR | Full NDCG@10 | n=100 answer F1 |
|---|---:|---:|---:|---:|
| BGE-base-en-v1.5 | 69.55% | 46.20% | 49.71% | **34.88%** |
| Multilingual E5-small, query/passage | 69.26% | 47.61% | 50.34% | 32.70% |
| Multilingual E5-large, query/passage | **71.99%** | **53.55%** | **55.43%** | 34.44% |

Correctly prefixed E5-large improved full-set Recall by 2.44 points (paired 95%
CI +1.27 to +3.62), MRR by 7.35 points (+5.83 to +8.88), and NDCG by 5.71
points (+4.51 to +6.90). All four question types improved on full-set
retrieval.

The frozen answer result nevertheless moved only 34.88% → 34.44%, a -0.44
point paired difference with a wide 95% interval of -6.52 to +5.43 points.
Multi-hop (+0.83 points), open-domain (+2.13), and single-hop (+0.26) improved,
while temporal fell by 4.96 points. Some temporal losses are official token-F1
format sensitivity (for example, an ISO date versus the equivalent
natural-language date), but the predeclared metric is not changed after seeing
the result. E5-large is therefore not promoted as the default: its retrieval
gain is real, while reader-facing quality remains statistically unresolved.

E5-small is statistically tied with BGE on full retrieval: Recall -0.29 points
(95% CI -1.59 to +1.00), MRR +1.41 (-0.05 to +2.89), and NDCG +0.62 (-0.54 to
+1.80). Its n=100 answer F1 was 32.70%, -2.18 points versus BGE with a paired
95% interval of -7.67 to +3.05 points. Single-hop improved by 2.97 points, but
multi-hop, open-domain, and temporal fell. This records the current MCP default
rather than motivating a model change.

A final deterministic packaging ablation removed repeated views of the same raw
turn from the BGE top-10 prompt without changing retrieval or replacing them
with lower-ranked evidence. It reduced the mean prompt item count from 10.00 to
8.32 on 86 questions, but F1 was unchanged at 34.94% versus 34.88% (+0.06
points; paired 95% CI -1.36 to +1.68). The candidate is rejected: fewer prompt
items alone do not close the evidence-selection gap.

## Commands

```bash
# Reproducible Memory answer gate. Run twice and compare semantic fields.
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --skip-adversarial --stratify 25 --sample-seed 42 \
  --top-k 10 --skip-local-judge --retrieval-only \
  --embedding-model bge-base-en-v1.5 \
  --baseline-reader-model qwen3.5:35b-a3b --reader-no-think \
  --reader-temperature 0 --reader-top-p 0.8 --reader-top-k 20 \
  --reader-presence-penalty 1.5 --generation-seed 42 \
  --reader-num-ctx 32768 --reader-num-predict 512 \
  --embed-cache .fastembed_cache/local-answer.sqlite --allow-download \
  --output benches/eval/results/local-answer-locomo-greedy-n100.json --force

# Same reader with only dataset-annotated evidence.
# --oracle-only still computes retrieval diagnostics, but skips retrieval answers.
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --skip-adversarial --stratify 25 --sample-seed 42 \
  --top-k 10 --skip-local-judge --oracle-only \
  --baseline-reader-model qwen3.5:35b-a3b --reader-no-think \
  --reader-temperature 0 --reader-top-p 0.8 --reader-top-k 20 \
  --reader-presence-penalty 1.5 --generation-seed 42 \
  --reader-num-ctx 32768 --reader-num-predict 512 \
  --embed-cache .fastembed_cache/local-answer.sqlite --allow-download \
  --output benches/eval/results/local-answer-locomo-greedy-oracle-n100.json --force
```

## Decision

- Keep the cognitive graph mechanics, public API, readout coefficients, and
  BGE default unchanged.
- Keep the query/passage correctness fix. It changes no graph mechanics and
  version-migrates only Qdrant E5 identities that previously stored raw
  vectors; the already-correct E5-small identity remains backward-compatible.
- Use 34.88% as the reproducible local development gate, not a release or
  leaderboard claim.
- Require future candidates to improve actual paired answer F1; retrieval
  metrics remain screening diagnostics.
- Focus the next product-safe work on selective open-domain/multi-hop evidence
  coverage without flooding the reader with same-speaker or same-session noise.
- Run the official full split, including adversarial questions, only after a
  candidate clears the frozen n=100 paired gate.

## Immutable Local Evidence

| Evidence | SHA-256 |
|---|---|
| Greedy Memory run 1 | `4374f904e6dc5eb0cfd31a812bb8f9aa228831f4b5fa5fed4c4b8c2d99eaa082` |
| Greedy Memory run 2 | `63141e067217330bd39d7206f7cfa8f2febb0cf0a648c6882e4a55212ba5beb5` |
| Greedy annotated-evidence run | `9aaf8938388f9dc7a07af67ce72cd59e185839d9d178308ab2a8c4957d4531b7` |
| Temporal prompt v6 paired run | `8f1069e56df3107ed6fd1eecf65c93a3633387931d64850d7f9320e9fba5c561` |
| Full BGE retrieval | `fa293b94447cd279f97dab0fa81759dcaabefa6d382cd54bf7f19d9e82a31e71` |
| Full E5-small query/passage retrieval | `a6016b578b64436df69d259284a8d876c7899761aa22ec4efbf7c383e02a36ca` |
| E5-small query/passage n=100 answer run | `f187bbe40aff7c49455e7e9c512a2ff67b89b6ab6926873432605b954222b55b` |
| Full E5-large query/passage retrieval | `8588827cd552a19d05733e57c7b656fd91b457430503ca12a2eb17a6c6c08bd3` |
| E5-large query/passage n=100 answer run | `b700c06c3c1bd0523da8821457df12fdc9d60f5a8c9d7fcae02668953ed288ef` |
| Rejected BGE top-10 prompt compaction | `3dcf3d7151656603eaabc06380736ffc7a0b493bb371ccb08b2861c7b01a05d5` |
| Invalid raw E5-large retrieval diagnostic | `e1c084e789fc0a996d629daa0191af176f785abb78d2ece1a01eac0bbf9099cb` |
| Invalid raw E5-large n=100 diagnostic | `c96f9a6026071f5b140993474cb73bb8eadd11776458b91b31f10dc499209a9d` |
| Readout signal feature dump | `d356720c6fac941ed329ce8c82056f3476fddd05633aeb4df05d54112876a617` |
