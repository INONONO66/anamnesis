# Local Answer Product-Wire v17 Qwen 3.6 Gate

Date: 2026-07-25

Status: n=100 development gate; not a full-split or cross-system leaderboard
claim

## Product Corrections

Schema v17 makes the benchmark and MCP consume the same timestamped product
context:

1. dataset epoch seconds are converted to the engine's epoch-millisecond
   `Timestamp` contract at ingest and query boundaries;
2. additive `Memory::render_context(&Recall)` reads every packaged source node
   and renders observation time plus optional half-open validity time;
3. the MCP recall path and benchmark route both use that public renderer;
4. `Memory::repackage_reranked_at` first applies the existing packaging,
   validity, tension, and result-limit semantics, then reassembles the surviving
   set against the full token budget;
5. raw fragments, packaging mode, source provenance, validity windows, tension
   endpoints, and selected-only commit traces remain intact.

`Recall::as_context()` remains available and signature-compatible as the
original package-only renderer. No model call moved into the core.

## Frozen Run

| Item | Value |
|---|---|
| Dataset | LoCoMo, non-adversarial, seed 42, 25 per type, n=100 |
| Reader | local `qwen3.6:35b-a3b` |
| Embedding | `Xenova/bge-base-en-v1.5` |
| Consumer reranker | `BAAI/bge-reranker-base`, candidate@100 |
| Product package | final top 10, first-stage seed limit 10 |
| Reader controls | no thinking, temperature 0, top-p 0.8, top-k 20, presence penalty 1.5, seed 42 |
| Context | 32,768; output budget 512 |
| Judge | disabled |
| Headline metric | raw official-compatible LoCoMo token F1 |

## Result

| Metric | Value |
|---|---:|
| Candidate true recall@100 | 84.07% |
| Selected/delivered true recall@10 | 62.15% |
| True rendered recall | 72.83% |
| Exact rendered Hit@10 | 84.00% |
| Raw Answer F1 | **43.14%** |
| Raw F1 bootstrap 95% interval | 35.38% to 50.90% |
| Mean package tokens | 1,157 |
| Synthetic label-only fragments | 0 / 1,000 |

| Type | Candidate recall | Selected recall | Rendered recall | Hit | Raw F1 |
|---|---:|---:|---:|---:|---:|
| Multi-hop | 74.55% | 41.94% | 52.64% | 84% | 34.45% |
| Open-domain | 69.73% | 38.00% | 58.00% | 68% | 31.93% |
| Single-hop | 96.00% | 88.00% | 92.00% | 92% | 66.05% |
| Temporal | 96.00% | 80.67% | 88.67% | 92% | 40.11% |

This is the first current score from the exact timestamped Qwen 3.6 product
wire. It supersedes schema-v15 absolute answer numbers and schema-v16 smoke
scores for current development decisions.

## Paired n=20 Ablations

On the frozen five-per-type sample:

| Change | Raw F1 | Paired change | Retrieval change |
|---|---:|---:|---|
| Old package-only renderer | 35.58% | anchor | — |
| Correct timestamp units + product timestamp renderer | 43.04% | +7.46 pp | none |
| Final-set reassembly | 43.99% | +0.95 pp | none |

Timestamp rendering produced four wins, fifteen ties, and one loss. The
conversation-cluster interval was positive, while the question bootstrap still
crossed zero at n=20. Final-set reassembly removed all 45 synthetic label-only
items in 200 slots and produced one additional answer win with no losses.

These are consumer-wire and packaging-quality gains, not retrieval recall
gains. The report keeps those causal surfaces separate.

## Interpretation

- Single-hop is largely healthy.
- Temporal retrieval is high, but roughly half of the answer quality remains
  reader/temporal-reasoning loss.
- Multi-hop and open-domain lose most coverage between candidate@100 and final
  top 10; selection/coverage is now the largest memory-side target.
- A simple top-20 run before final-set reassembly raised selected recall but not
  rendered recall and slightly reduced F1. Larger K must be retested only with
  the corrected reassembly path.
- The 84% exact rendered Hit@10 is numerically near Supermemory's published
  83.5 score, but their open harness uses LLM relevance and Hit semantics.
  Cross-system superiority is therefore not claimed.

## Next Gates

1. Consumer-side coverage selection for multi-hop/open-domain, with the same
   candidate ranking and token budget.
2. Non-seed temporal scoring and deterministic question decomposition.
3. A separate local semantic-judge lane applied identically to every system.
4. Confirm the top-20 candidate at n=200 only after reducing its
   conversation-level heterogeneity.
5. Full 1,540-question run only after a paired candidate passes both
   question and conversation-cluster gates.

## Corrected Top-20 Screen

The corrected final-set reassembly made top-20 materially different from the
previous broken packaging sweep:

| Metric | Top 10 | Top 20 | Paired change |
|---|---:|---:|---:|
| Selected true recall | 62.15% | 73.02% | +10.87 pp |
| True rendered recall | 72.83% | 78.64% | +5.81 pp |
| Exact rendered Hit | 84% | 88% | +4 pp |
| Raw Answer F1 | 43.14% | **47.12%** | +3.99 pp |

The question bootstrap interval is positive (`+0.31` to `+8.18` points), but
the conversation-cluster interval still crosses zero (`-1.05` to `+10.66`
points): 14 wins, 76 ties, and 10 losses. Four of ten conversations have a
negative mean delta, and multi-hop gains +8.47 points of rendered recall while
Answer F1 changes by -0.60 points.

Top-20 is therefore a promising high-quality candidate, not yet a promoted
default. The next selection experiment must turn added coverage into compact,
distinct evidence rather than relying on more context alone.

## Evidence

Local reports are not committed:

| Report | SHA-256 |
|---|---|
| `local-answer-locomo-product-wire-v17-qwen36-bge-reranker-timestamped-n20-seed42.json` | `73897467da27db7612f839a8d3958685ac98e0d671ac506290657e9fa8051c42` |
| `local-answer-locomo-product-wire-v17-qwen36-bge-reranker-reassembled-n20-seed42.json` | `503a95d57e073ec7c458a016819819ba5d9719f7fc788083d39b1b68ea2f178d` |
| `local-answer-locomo-product-wire-v17-qwen36-bge-reranker-reassembled-n100-seed42.json` | `f915236d07fadeca2678babc33ba1485785a68c673b8bd8c8d45d78c4c1b2020` |
| `local-answer-locomo-product-wire-v17-qwen36-bge-reranker-reassembled-top20-n100-seed42.json` | `679560f5e67f2d47adcb9f055b768ea5a9ec115f0e8a339dfdf8a980188751d2` |
