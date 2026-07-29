# Local LoCoMo Reranking Gate

Date: 2026-07-25

Status: historical accepted local high-quality profile under the schema-v15
adapter renderer; product-wire schema-v16 reconfirmation required. It is not a
full-split or leaderboard claim, and not the latency-sensitive hook default.

This gate tests whether a second-stage local reranker can improve the actual
answer delivered by Anamnesis without replacing its cognitive graph, changing
the shipped readout coefficients, or moving model inference into the engine
core. Retrieval metrics screen candidates; paired answer F1 decides promotion.

## Frozen Protocol

| Item | Value |
|---|---|
| Dataset | pinned LoCoMo snapshot, FNV-1a64 `fdf74317b9a55716` |
| Selection | seed 42, 50 questions per non-adversarial type, 200 total |
| First stage | shipped `Memory` cognitive readout, BGE-base embeddings |
| Candidate surface | first 100 nodes from `SearchResult::trace.readout` |
| Second stage | local `BAAI/bge-reranker-base`; all 100 candidates scored, batch size 32 |
| Final package | shared engine packaging rules, top 10 |
| Reader | local `qwen3.5:35b-a3b`, digest `3460ffeede54…` |
| Generation | non-thinking, temperature 0, presence penalty 1.5, seed 42 |
| Prompt / metric | `official-format-v6-temporal-anchor` / official LoCoMo token F1 |
| Judge | disabled |
| Uncertainty | 10,000-resample paired question and conversation-cluster bootstraps |

The comparison is paired by question. The cluster bootstrap samples the ten
LoCoMo conversations with replacement and retains every selected question in a
sampled conversation. This guards against treating questions from the same
conversation as independent evidence.

## Accepted Result

| Route | Official F1 | Recall@10 | Hit@10 |
|---|---:|---:|---:|
| Shipped cognitive top 10 | 35.29% | 52.83% | 64.50% |
| Cognitive top 100 → BGE reranker → top 10 | **40.61%** | **61.65%** | **74.00%** |
| Paired change | **+5.32 pp** | **+8.82 pp** | **+9.50 pp** |

The answer-F1 change is positive under both uncertainty units:

- question bootstrap 95% CI: **+0.81 to +10.03 points**;
- conversation-cluster bootstrap 95% CI: **+0.32 to +11.33 points**;
- 51 questions improved, 114 tied, and 35 regressed.

| Type | Baseline → reranked change |
|---|---:|
| Multi-hop | +7.52 pp |
| Open-domain | +12.06 pp |
| Temporal | +2.39 pp |
| Single-hop | -0.69 pp |

The targeted open-domain and multi-hop evidence-selection failures improve
substantially. The small single-hop regression is retained rather than hidden:
the predeclared overall paired gate passes, but future work should recover it
without giving back the multi-hop/open-domain gain.

## Product Integration and Subsequent Fidelity Repair

The engine still does not own or initialize a reranker. The additive
`Memory::repackage_reranked` API accepts only finite, unique
`RerankedCandidate { node_id, score }` values already present in the cognitive
readout. It then:

- applies deterministic score ordering with cognitive order as the tie-breaker;
- uses shared package assembly, result limiting, and token accounting;
- rebuilds access and co-readout commit evidence for only the fragments actually
  exposed;
- retains source path-current evidence only where both endpoints survive;
- rejects unknown, duplicate, or non-finite consumer output without mutation.

The benchmark's prior shadow package and the product API path matched on all
200 answer strings, all 200 per-question F1 values, and all 200 ordered
node/score/text evidence lists. The old shadow report counted the entire
pre-trim package in `context_tokens`; the product path correctly reports the
final ten fragments (mean 1,030 estimated tokens, p95 1,364).

No default search, embedding model, readout coefficient, graph mechanic, or
existing public signature changes.

Subsequent audit found that schema-v15 did not exercise a fully equivalent
product wire: the benchmark fragment renderer omitted `Fragment.name` while
adding dataset session dates unavailable in `Recall::as_context()`. The initial
reranking API also did not reapply validity windows or the source packaging
mode and could discover tensions only from the source final package.

Schema v16 repairs these gaps additively:

- `Memory::repackage_reranked_at` reapplies contradiction discovery, source
  packaging mode, and validity at the explicit query time;
- the existing `repackage_reranked` uses present-time validity;
- the default answer route consumes exact `Recall::as_context()` output;
- candidate, reranker, delivered, and final rendered coverage are reported
  separately.

The historical 40.61% result remains evidence for second-stage ranking, but its
absolute score and +5.32-point delta must be rerun before being described as a
current product-wire result.

## Cost and Rejected Alternatives

The accepted BGE model occupies about 1.1 GB locally. On this Mac, reranking 100
candidates plus first-stage search measured mean 4.01 s, p50 4.05 s, and p95
4.99 s. It is therefore an explicit high-quality profile, not the 1.5-second
fail-open hook path.

| Candidate | Sample | F1 | Paired conclusion | Search + rerank |
|---|---:|---:|---|---:|
| BGE reranker, 100 candidates | 200 | **40.61%** | **accepted, +5.32 pp** | mean 4.01 s |
| BGE reranker, 20 candidates | 200 | 38.95% | +3.66 pp, CI crosses 0 | mean 0.94 s |
| Jina turbo, 100 candidates | 200 | 33.07% | -2.22 pp, CI crosses 0 | mean 0.67 s |
| Cognitive/embed/text RRF | 100 | 35.30% | +0.42 pp, CI crosses 0 | mean 0.04 s |
| Indiscriminate final L2 hydration | 100 | 32.20% | -2.68 pp, rejected | mean 0.05 s |

Reducing the accepted package from ten to eight items also failed: the n=100
BGE result fell 38.99% → 35.83% (-3.16 points). The evidence says relevance
selection matters; merely shrinking the prompt or hydrating more content does
not.

The original n=100 BGE reports were produced before reranker time was included
inside `search_latency_ms`, so their ~35 ms values are not valid reranker
latencies. The n=200 product-path run above includes the complete second stage.

## Reproduction

```bash
cargo bench --features embed --bench local_answer -- \
  --dataset locomo --skip-adversarial --stratify 50 --sample-seed 42 \
  --top-k 10 --skip-local-judge --retrieval-only \
  --embedding-model bge-base-en-v1.5 \
  --consumer-cross-encoder BAAI/bge-reranker-base \
  --consumer-candidate-k 100 --first-stage-seed-limit 10 \
  --baseline-reader-model qwen3.5:35b-a3b --reader-no-think \
  --reader-temperature 0 --reader-top-p 0.8 --reader-top-k 20 \
  --reader-presence-penalty 1.5 --generation-seed 42 \
  --reader-num-ctx 32768 --reader-num-predict 512 \
  --embed-cache .fastembed_cache/local-answer.sqlite --allow-download \
  --output benches/eval/results/local-answer-locomo-qwen35-bge-cross-encoder-base-product-api-v1-prompt-v6-greedy-top10-n200-seed42.json \
  --force
```

## Immutable Local Evidence

| Evidence | SHA-256 |
|---|---|
| Shipped baseline n=200 | `8ee8de15590076d3ee9fdcc60f7a4a064abaab06ca27b3657cad1529e36bb9ff` |
| Accepted BGE product path n=200 | `d972254e7d6cdd2c86f86543b698e21bff37bef6fa51dde47cd0498396a72b00` |
| Rejected BGE 20-candidate profile | `2915f1f8e0a6bf6464b84a9f211e89a508e4a3d62e9e68822251d25d0d8b0c42` |
| Rejected Jina turbo profile | `d6e870b86d4ea67fccbf27ff74fc9f281d29094bf5dc8e1b794db95895e031cb` |

## Decision

- Accept BGE reranker-base over 100 cognitive candidates as the measured local
  high-quality profile.
- Keep native cognitive search as the fast/default path.
- Keep the concrete reranker in the consumer/benchmark layer; the engine owns
  validation, packaging, and commit correctness only.
- Do not promote the 20-candidate or Jina alternatives merely because they are
  faster; neither clears the paired answer-quality gate.
- Require a larger/full-split confirmation before making a public leaderboard
  claim.
