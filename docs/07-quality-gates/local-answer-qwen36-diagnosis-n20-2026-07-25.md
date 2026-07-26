# Local Answer Qwen 3.6 n=20 Diagnosis

Date: 2026-07-25

Status: diagnostic sample; not a quality promotion or public benchmark claim

## Frozen Setup

- Dataset: LoCoMo, five seeded questions from each non-adversarial type
- Reader: local `qwen3.6:35b-a3b`
- Embedding: `Xenova/bge-base-en-v1.5`
- Consumer reranker: `BAAI/bge-reranker-base`, cognitive candidate@100
- Final package: top 10, first-stage seed limit 10
- Product lane: exact `Recall::as_context()` surface
- Reader: no thinking, temperature 0, top-p 0.8, top-k 20,
  presence penalty 1.5, generation seed 42, context 32,768
- Metric: raw official-compatible LoCoMo token F1

## Result

| Lane | Candidate recall@100 | Final recall@10 | Rendered recall | Raw F1 |
|---|---:|---:|---:|---:|
| Cognitive product-wire | 80.42% | 46.42% | 48.50% | 34.24% |
| BGE reranked product-wire | 80.42% | 53.58% | 62.08% | 35.58% |
| BGE paired change | 0.00 pp | +7.17 pp | +13.58 pp | +1.34 pp |
| Dataset-annotated oracle evidence | — | — | — | 52.31% |
| Dated diagnostic fragments | 80.42% | 53.58% | 62.08% | 43.58% |

The BGE paired run had 1 Answer-F1 win, 19 ties, and no losses. Its question
and conversation-cluster intervals both include zero at the lower boundary.
Five questions gained rendered recall without any Answer-F1 change.

The dated diagnostic lane uses the same selected evidence but changes the
consumer surface and injects dataset session dates. It is not a product score.
Its +8.03-point result is causal evidence that product-visible metadata and
rendering materially affect the reader.

## Type Breakdown

| Type | Oracle F1 | Product-wire F1 | Candidate recall | Final recall | Rendered recall |
|---|---:|---:|---:|---:|---:|
| Single-hop | 84.91% | 83.81% | 100.00% | 100.00% | 100.00% |
| Multi-hop | 45.83% | 42.80% | 60.00% | 37.67% | 41.67% |
| Open-domain | 27.78% | 10.00% | 81.67% | 23.33% | 33.33% |
| Temporal | 50.71% | 5.71% | 80.00% | 53.33% | 73.33% |

The 16.73-point oracle-to-product gap decomposes arithmetically into:

- temporal: 11.25 points;
- open-domain: 4.44 points;
- multi-hop: 0.76 points;
- single-hop: 0.27 points.

## Root Causes

### 1. Product Context Omits Evidence Time

`Recall::as_context()` renders fragment type, name, body, and origin session,
but no node timestamp. LoCoMo temporal evidence often says `last weekend`,
`next month`, or `for a month`; the answer is only recoverable relative to the
evidence date.

Observed examples:

- Full rendered evidence produced `next month` instead of `September 2023`.
- Full rendered evidence about playing drums produced abstention instead of
  resolving one month before 27 March 2022.
- Adding session dates in the diagnostic lane changed temporal F1 from 5.71%
  to 35.71% (+30 points).

The diagnostic also changes fragment presentation, so +30 points is not a
date-only effect estimate. It is enough to establish that the current product
surface lacks information required by this benchmark and by real relative-time
memory questions.

### 2. Candidate Coverage Does Not Survive Final Selection

The cognitive candidate@100 surface contains 80.42% of annotated gold units,
but the BGE final top 10 contains 53.58%. Open-domain falls from 81.67% to
23.33%, and multi-hop from 60.00% to 37.67%.

BGE is still better than the cognitive top 10 on this sample: final recall is
+7.17 points, rendered recall +13.58 points, and raw F1 +1.34 points. The
problem is not that BGE should be removed; it is that a relevance-only top-10
selection discards too much multi-evidence coverage.

### 3. Reader Abstention Despite Visible Evidence

The product route returned `No information available` seven times. Five of
those questions had at least some annotated gold evidence in the exact rendered
context. This includes one temporal question with full rendered coverage.

That is a consumer interpretation problem, not a first-stage recall failure.
Timestamp omission explains some temporal abstentions; open-domain questions
also expose conservative inference behavior.

### 4. The Reader and Token Metric Have a Low Oracle Floor

Even gold-only evidence reaches 52.31% rather than a near-perfect score:

- four of 20 oracle answers score zero;
- open-domain oracle F1 is only 27.78%;
- semantically related pairs such as `real`/`authentic` and `drive`/`driven`
  receive little or no token overlap;
- `journaling and creative writing` against a comma-separated reference scores
  0.65 despite conveying the requested answer.

These losses must remain separate from memory quality. Raw official-compatible
F1 stays the headline metric, but a semantic/judge diagnostic is needed to
explain its floor and must be applied identically to all compared systems.

### 5. Evidence Completeness Strongly Predicts Answer Quality

- Full rendered gold coverage: 9 questions, mean F1 60.85%.
- Partial rendered coverage: 7 questions, mean F1 23.43%.
- Zero rendered coverage: 4 questions, mean F1 0%.

This validates the four-stage harness: rendered coverage, not selected node
provenance alone, is the useful bridge between retrieval and Answer F1.

## Product-Safe Priority

1. Expose evidence timestamps through the real product renderer and make the
   benchmark consume that same public output. Do not inject LoCoMo-only dates.
2. Hold candidate@100 fixed and test consumer-side coverage-aware top-10
   selection/backfill, especially temporal and multi-evidence queries.
3. Improve temporal query/readout signals for the 20% of gold units absent even
   from candidate@100.
4. Diagnose reference-blind answer canonicalization and semantic judging as a
   separate reader-surface report. Do not count it as memory improvement.
5. Confirm each change on paired n=100 Qwen 3.6 product-wire runs before
   promotion.

## Model Policy

Current and future local answer development runs use `qwen3.6:35b-a3b`.
The local Qwen 3.5 base and coder tags were removed. Historical Qwen 3.5
quality-gate documents remain unchanged as provenance and are not current
reproduction instructions.

## Evidence

Local reports are not committed:

| Report | SHA-256 |
|---|---|
| `local-answer-locomo-product-wire-v16-qwen36-baseline-n20-seed42.json` | `43859fc5005160a83fd334ebef1cef40cbe2d9625f722f03543f751eeebeeb56` |
| `local-answer-locomo-product-wire-v16-qwen36-bge-reranker-n20-seed42.json` | `ac328a86914d8b11d78271f0a406adba335bd55eabf403e26c49bddd6739fb37` |
| `local-answer-locomo-diagnostic-dated-v16-qwen36-bge-reranker-n20-seed42.json` | `e304861d1132f47635e8d300dc55566dde37986ddd5acc8af6f1cdd2c90db350` |
