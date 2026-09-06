# Original-query and English-query fusion probe

This offline experiment reuses the captured Qwen3-Embedding-0.6B Q8_0
1024-dimensional vectors from the earlier eight-Episode, twenty-query
development pilot. It makes no model or embedding requests.

Each Episode receives the maximum cosine score of its original text and/or
admitted Fact rows. Exact-quote failures are excluded from the Fact corpus.
The fusion arm combines full Episode ranks for the original and manually
translated English query using equal weights and RRF k=60, fixed before
execution. Ties use Episode index. This is not the production bounded
Fact/Entity/PPR pipeline.

## Results

Both GPT-5.5 and Opus-5 English Fact corpora produced these Episode-level
results. GPT had 20 admitted claims; Opus had 15 after one quote rejection.

| Candidate corpus | Query | Hit@1 | Hit@3 | MRR |
|---|---|---:|---:|---:|
| Original Episodes | Original | 20/20 | 20/20 | 1.000 |
| English Facts | English | 19/20 | 20/20 | 0.967 |
| Original Episodes + English Facts | Original | 20/20 | 20/20 | 1.000 |
| Original Episodes + English Facts | English | 19/20 | 20/20 | 0.967 |
| Original Episodes + English Facts | Two-query RRF | 19/20 | 20/20 | 0.975 |

Only q20, asking for the four continuously collected agent sources, missed
top-1. Its target ranked third with the English query, second with fusion,
and first with the original query. Thus adding a translation and equal-
weight fusion did not improve the original-query baseline on this set.

The union arm preserves original evidence when Fact extraction omits a
detail. An Episode hit still does not prove the delivered claim answers the
question: the rejected Opus quote contains the list of agent sources.

## Interpretation

Retain original query text and original Episode retrieval as the default.
English search projections can remain optional and versioned; these results
do not support mandatory translation or automatic two-query fusion.

This is the same source-informed development set, not held-out data. It has
only seven substantive target Episodes, human translations, no realistic
distractor population, and saturated Hit@3. No statistical superiority or
production-quality conclusion follows. Language translation, extraction
variability and grouping effects are not isolated causal factors.

## Reproduction

The runner requires the archived manifest, vectors, query pairs and two
English-extraction receipts. Inputs and the report contain private source
material and stay outside git. Every input file's SHA-256 is included in
the output report.

```sh
bun scripts/research/retrieval-fusion.mjs \
  --manifest /path/to/manifest.json \
  --vectors /path/to/vectors.json \
  --queries /path/to/queries.json \
  --gpt /path/to/gpt-receipt.json \
  --opus /path/to/opus-receipt.json \
  --out /path/to/fusion-report.json

bun test scripts/research/retrieval-fusion.test.mjs
```

The original archive is on the experiment server under
`~/.config/anamnesis/`: retrieval vectors from
`retrieval-2026-09-06T07-31-33.298Z`, manifest from
`prompt-comparison-2026-09-06T07-02-18.973Z`, English receipts from
`prompt-comparison-2026-09-06T07-28-03.569Z`, and
`anamnesis-retrieval-queries.json`.

Four tests cover quote-issue exclusion, both RRF contributions, deterministic
ties and missing-vector rejection. Temporary mutations that dropped the
English contribution or silently accepted a missing vector were observed
failing and then restored. Actual archived inputs were exercised through the
CLI; synthetic test success alone was not used as experiment evidence.
