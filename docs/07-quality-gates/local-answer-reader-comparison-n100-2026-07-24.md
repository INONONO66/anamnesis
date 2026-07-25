# Local Answer Reader Comparison: 7B vs 35B-A3B

Date: 2026-07-24

This development diagnostic changes only the answer reader over the same
seeded LoCoMo questions and contexts. It is not a full-split or official
leaderboard result.

## Frozen Protocol

| Item | Value |
|---|---|
| Dataset | LoCoMo transformed snapshot |
| Selection | 25 questions/type, adversarial excluded |
| Seed | 42, stable hash stratification |
| Questions | 100 across all 10 conversations |
| 7B reader | `qwen2.5:latest` (`845dbda0ea48…`) |
| 35B-A3B reader | `qwen3.5:35b-a3b` (`3460ffeede54…`) |
| Judge | `gemma3:12b` (`f4031aab637d…`) |
| Prompt | `category-aware-v3` |
| Retrieval | shipped `Memory` API + BGE, top 20 |
| Retrieval result | Recall@20 0.592, Hit@20 0.720 |

## Raw LLM-Judge Results

| Context | 7B | 35B-A3B | Change |
|---|---:|---:|---:|
| Gold evidence | 53% | 65% | +12 pp |
| Memory package | 30% | 49% | +19 pp |
| Gold-to-Memory gap | 23 pp | 16 pp | -7 pp |

The 35B-A3B Wilson 95% intervals are 55.3–73.6% for Gold and 39.4–58.7%
for Memory.

| Type | Gold 7B | Gold 35B | Memory 7B | Memory 35B |
|---|---:|---:|---:|---:|
| Multi-hop | 48% | 64% | 28% | 36% |
| Temporal | 48% | 40% | 24% | 56% |
| Open-domain | 40% | 64% | 4% | 32% |
| Single-hop | 76% | 92% | 64% | 72% |

At the individual-question level, the 35B reader improved 20 and regressed 8
Gold answers relative to 7B. On Memory context it improved 21 and regressed 2.
The larger reader is materially better on the product context, but not
monotonic: Gold temporal accuracy dropped on this sample.

## Judge Audit

All 200 judge responses parsed. A deterministic every-fifth-question audit
checked 20 questions and 40 route judgments. Manual review agreed on 38/40
(95%).

The two known disagreements were:

- Gold `locomo-6-qa-26`: the candidate omitted Greenland, but the judge marked
  it correct. Correcting this lowers Gold from 65% to 64%.
- Memory `locomo-5-qa-10`: “next month (relative to 2023-05-11)” denotes June
  2023, but the judge marked it incorrect. Correcting this raises Memory from
  49% to 50%.

These are audit annotations; the immutable raw JSON retains the original judge
responses and summary. A full external result needs a larger blinded audit or
deterministic companion metrics.

## Interpretation

The reader upgrade recovers a substantial part of the end-to-end loss:
Memory accuracy rises by 19 percentage points and the Gold-to-Memory gap
narrows by 7 points. Retrieval remains a separate bottleneck because the
reader receives at least one labeled hit for only 72% of questions and mean
evidence recall is 59.2%.

The raw 35B result JSON SHA-256 is
`b2ceb2f995362582b274d9661555511e6d11273c2c853871fd8211de0f184f4e`.
