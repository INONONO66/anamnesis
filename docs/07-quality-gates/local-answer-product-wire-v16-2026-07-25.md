# Local Answer Product-Wire Schema v16

Date: 2026-07-25

Status: harness fidelity gate passed; n=4 smoke only, not a quality promotion
or benchmark claim. Superseded by schema v17 for the timestamped product wire.

## Contract

The default retrieval answer lane now executes:

1. dataset turns through the public `Memory::add` windowing recipe;
2. `Memory::search_result_at_with` at the dataset question time;
3. optional consumer-owned local reranking over public `Memory::get` views;
4. `Memory::repackage_reranked_at` with the same question time;
5. exact `Recall::as_context()` output into the local reader.

No benchmark-only fragment renderer or dataset session-date injection is used
in this lane. The old enriched fragment surface remains available only through
`--diagnostic-fragment-context`.

Schema v16 reports four distinct evidence surfaces:

| Surface | Meaning |
|---|---|
| `candidate@K` | gold coverage in the cognitive pool offered to the consumer |
| `reranker@K` | gold coverage after consumer ordering at final K |
| `delivered@K` | provenance coverage after package/mode/validity/result limiting |
| `rendered` | exact gold bodies actually visible in `Recall::as_context()` |

Raw official LoCoMo F1 remains the headline answer score. The separately named
reader-surface F1 applies only a reference-blind standalone ISO-date
canonicalizer.

## Product Correctness Repair

The additive `Memory::repackage_reranked_at` path now:

- rediscovers active contradiction pairs from the reranked readout;
- reapplies the source search's packaging mode;
- filters half-open node validity windows at the explicit query time;
- limits the result and rebuilds a selected-only commit trace.

The existing `repackage_reranked` signature is preserved and applies
present-time validity. Tests pin validity boundaries, timeline ordering,
tension rediscovery, commit safety, and read-only behavior.

## Smoke Result

Frozen local Qwen3.5 35B-A3B, BGE-base embedding, seed 42, one question from
each non-adversarial LoCoMo type:

| Lane | Candidate recall@100 | Reranker recall@10 | Delivered recall@10 | Rendered recall | Raw F1 |
|---|---:|---:|---:|---:|---:|
| Cognitive baseline | 70.83% | 50.00% | 50.00% | 50.00% | 41.25% |
| BGE reranker-base | 70.83% | 62.50% | 62.50% | 62.50% | 38.68% |
| Paired change | 0.00 pp | +12.50 pp | +12.50 pp | +12.50 pp | -2.57 pp |

This sample is intentionally too small for a quality conclusion. It validates
the diagnostic objective: the committed comparator correctly reports that the
single question whose rendered recall improved nevertheless lost 30.28 Answer
F1 points, while the other three questions had tied rendered recall and a mean
+6.67-point Answer F1 change. Retrieval improvement is therefore no longer
silently equated with reader-facing improvement.

Question and conversation-cluster intervals both span zero
(`[-22.71, +15.00]` points).

### Qwen 3.6 Re-run

The identical four-question smoke was repeated with the official local
`qwen3.6:35b-a3b` tag. All retrieval, generation, context, and seed controls
were unchanged.

| Lane | Candidate recall@100 | Reranker recall@10 | Delivered recall@10 | Rendered recall | Raw F1 |
|---|---:|---:|---:|---:|---:|
| Cognitive baseline | 70.83% | 50.00% | 50.00% | 50.00% | 41.25% |
| BGE reranker-base | 70.83% | 62.50% | 62.50% | 62.50% | 41.25% |
| Paired change | 0.00 pp | +12.50 pp | +12.50 pp | +12.50 pp | 0.00 pp |

All four Answer F1 values tied between the two lanes. The one question whose
rendered recall improved also tied on Answer F1. This differs from the Qwen 3.5
smoke, where that question lost Answer F1, and reinforces that the n=4 run is a
harness smoke rather than a reader-quality conclusion.

## Evidence

Local reports are not committed:

| Report | SHA-256 |
|---|---|
| `local-answer-locomo-product-wire-v16-baseline-n4-seed42.json` | `674863686c25d4c7cffc32af9c4bcea8b302a0be445f12f59f41efdb6bdb9dc1` |
| `local-answer-locomo-product-wire-v16-bge-reranker-n4-seed42.json` | `fcfd8b86866e90f2056fd15ca5e35f314261860f160934c6f8779b8535d93a14` |
| `local-answer-locomo-product-wire-v16-qwen36-baseline-n4-seed42.json` | `5bfc2ffff3fa816aa10baf2687b9719df9831d1b0a573471c48cc1824a094c55` |
| `local-answer-locomo-product-wire-v16-qwen36-bge-reranker-n4-seed42.json` | `4ee8634ad224065e483f2dc05464811bc6d4d79d20e45b266bc71c9bb0b2f73a` |

Compare them from `crates/anamnesis` with:

```bash
python3 ../../scripts/compare_local_answer.py \
  benches/eval/results/local-answer-locomo-product-wire-v16-baseline-n4-seed42.json \
  benches/eval/results/local-answer-locomo-product-wire-v16-bge-reranker-n4-seed42.json
```

For Qwen 3.6, replace the two report names with their `qwen36-` variants.

The next quality gate is a paired product-wire n=100 baseline/reranker rerun.
