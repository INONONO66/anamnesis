# Local Answer Diagnostic: 7B Reader, LoCoMo n=100

Date: 2026-07-24

This is a reproducible development diagnostic, not a full-split or official
leaderboard result. “7B reader” means that both answer routes use the same
7.62B answer model. The separate 12B model is used only as an evaluator and is
not part of the product answer path.

## Protocol

| Item | Value |
|---|---|
| Dataset | LoCoMo transformed snapshot |
| Selection | 25 questions/type, adversarial excluded |
| Seed | 42, stable hash stratification |
| Total | 100 questions across all 10 conversations |
| Reader | `qwen2.5:latest` (`845dbda0ea48…`) |
| Judge | `gemma3:12b` (`f4031aab637d…`) |
| Answer prompt | `category-aware-v3` |
| Retrieval | shipped `Memory` API + BGE, top 20 |
| Runtime | Ollama 0.31.1, loopback only |

The category map follows the official LoCoMo evaluator:
`1=multi-hop`, `2=temporal`, `3=open-domain`, `4=single-hop`,
`5=adversarial`. The first attempted run exposed and corrected an earlier
category 3/4 label inversion before this report was accepted.

## Results

| Route | Correct | Accuracy | Wilson 95% CI |
|---|---:|---:|---:|
| Gold evidence + 7B | 53/100 | 53.0% | 43.3–62.5% |
| Memory package + 7B | 30/100 | 30.0% | 21.9–39.6% |

Retrieval package Recall@20 was 0.592 and Hit@20 was 0.720.

| Type | n | Gold + 7B | Memory + 7B | Hit@20 | Mean Recall@20 |
|---|---:|---:|---:|---:|---:|
| Multi-hop | 25 | 48% | 28% | 76% | 0.428 |
| Temporal | 25 | 48% | 24% | 80% | 0.767 |
| Open-domain | 25 | 40% | 4% | 52% | 0.374 |
| Single-hop | 25 | 76% | 64% | 80% | 0.800 |

Paired outcomes:

| Outcome | Questions |
|---|---:|
| Both routes correct | 24 |
| Gold only correct | 29 |
| Memory only correct | 6 |
| Both routes incorrect | 41 |

All 30 correct Memory answers occurred among the 72 questions with a retrieval
hit. None of the 28 retrieval misses produced a correct Memory answer. Even
when retrieval hit, the Memory answer rate was only 30/72 (41.7%); 17
gold-correct questions became wrong despite a hit. The measured bottleneck is
therefore both missing/partial evidence and the 7B reader's handling of a
20-item noisy context, not either component alone.

## Validation and Limitations

- All 100 questions contain exactly the two expected routes and all 200 judge
  responses parsed.
- A deterministic every-fifth-question audit covered 20 questions and 40
  route judgments; the manual decision agreed with the recorded judge verdict
  in all 40 cases.
- The result JSON SHA-256 is
  `c374ce3422f1c199af0fe3155262fb07d9304a3ddc7e5da3c74d9445ad454f57`.
- This n=100 sample is suitable for development and regression diagnosis.
  It is not a headline claim; run the full non-adversarial split and retain a
  larger blinded judge audit for that purpose.
- The 12B judge is local but is still an LLM judge. Report deterministic
  answer metrics alongside it before using a future full run externally.
