# Calibration Records

Per [ADR-0010](../adr/0010-calibrated-priors-not-laws.md), every refit of a
calibrated prior records its data, procedure, and result here. Refit when graph
topology, agent behavior, embedding geometry, or dataset changes.

Evidence file names below refer to local benchmark outputs (not committed —
they are multi-MB run reports). Retrieval/answer records use
`cargo bench --features embed --bench local_answer`; mechanics calibration
records use `real_memory`. Every number is reproducible from the named dataset,
configuration, and frozen artifacts; reader-free runs are deterministic for a
fixed dataset, embedding model, and reranker.

## 2026-08-01 — bounded dense union and adaptive product delivery

- **Data:** LoCoMo seed 42, 25 questions per non-adversarial type, n=100;
  `Xenova/bge-base-en-v1.5`, `BAAI/bge-reranker-base`, the frozen
  reference-blind Qwen 3.6 extraction artifact, candidate@50, requested
  final@20, and the detailed product renderer. The run was `--predict-only`,
  recorded `local_only=true`, made no reader, judge, remote-provider, or other
  LLM call, and cost $0 externally.
- **Production policy:** collection, relationship, and inference queries batch
  the original query with at most two deterministic dense entity/decomposition
  surfaces. Stored embeddings are scanned once. Auxiliary lanes are
  deduplicated into one bounded union with a fixed 0.25 RRF prior, so multiple
  anchors cannot outvote the complete query by vote count alone. Direct and
  temporal source search remains unchanged.
- **Delivery policy:** automatic direct-query selection freezes the first eight
  reranker rows, then prefers canonical sources not already represented in the
  tail. Direct one-fact queries use at most 12 fragments; temporal and
  completeness-sensitive queries retain the requested 20. This is the default
  `Memory::search_reranked` behavior used by MCP, hooks, plugins, direct
  `Memory` callers, and the benchmark product-wire lane. A public additive
  option disables adaptive delivery when an exact fixed width is required.

| Type | candidate@50 | raw reranker@20 | delivered | rendered | vs v175 rendered |
|---|---:|---:|---:|---:|---:|
| multi-hop | 67.90% | 64.24% | 63.44% | **67.57%** | **+2.67 pp** |
| open-domain | 59.00% | 53.67% | 59.67% | **63.00%** | **+1.33 pp** |
| single-hop | 96.00% | 96.00% | 92.00% | **96.00%** | 0.00 pp |
| temporal | 92.00% | 84.67% | 86.67% | **96.00%** | 0.00 pp |
| **overall** | **78.73%** | **74.64%** | **75.44%** | **80.64%** | **+1.00 pp** |

Rendered Hit remains **89%**. There are three positive per-question rendered
changes and zero negative changes against v175. Relative to the pre-structural
v155 production baseline, cumulative rendered gains are **+8.17 pp**
Multi-hop and **+6.97 pp** Open-domain, with Single-hop and Temporal still at
96%.

- **Context:** 20 direct-fact questions receive 12 fragments and the other 80
  receive 20, reducing the overall mean from 20 to 18.4. On those 20 adaptive
  questions, mean product-context characters fall from 11,997 to 8,666
  (-27.8%) and measured context tokens from 2,228 to 1,444 (-35.2%). Across all
  questions, tokens fall from 1,929 to 1,743 (-9.6%); characters rise from
  10,842 to 11,160 (+2.9%) because recovered complex evidence is longer.
- **Latency:** 1.51 s mean, 2.26 s p95, and 2.69 s maximum. The required p95
  remains below the 4 s fail-open boundary; differences from prior warm-cache
  runs are not attributed solely to this policy.
- **Harness correction:** the product-wire harness previously called
  `Memory::rerank_search_result_at` and then repeated automatic deep selection.
  It now retains the raw ranking for the reranker diagnostic and consumes the
  already-selected production package exactly once. Consequently, the v207
  raw-reranker column is not directly comparable to v175's old selected/reranker
  label; delivered and rendered product surfaces remain the quality gates.
- **Rejected variants:** independent equal-weight auxiliary lanes regressed
  overall rendered recall to 78.81% and Open-domain to 59.00%; a broad
  fact-shaped adaptive cap regressed Temporal. Neither remains in production.
  The retained union is a single lower-prior lane, and the cap is restricted to
  direct facts, including an explicit temporal classification for “over time.”
- **Evidence:**
  `local-answer-locomo-v207-final-temporal-safe-dense-adaptive-production-n100.json`.

## 2026-08-01 — query-scoped local reader and speaker ownership gate

- **Data:** frozen LoCoMo seed-42 production contexts from the local Qwen 3.6
  extraction artifact; Multi-hop n=16 and Open-domain n=20 type screens.
  Retrieval uses the exact `Memory::search_reranked` product path with
  candidate@50, reranker/delivered@20, and the product detailed renderer.
- **Provider:** loopback OMLX `Qwen3.6-35B-A3B-4bit`, digest
  `b8be5fc144324bb58269ab045a15ecbcf55baa6d61960757861581d389a54145`,
  `enable_thinking=false`, temperature 0, seed 42. Reader and semantic judge
  reports both record `local_only=true`; all API-key variables were removed
  from the run environment. External cost was $0.
- **Reader contract:** answer-shape rules are query-scoped rather than one
  global instruction block. Reflection list items must cite visible
  `turn-source` ids. Final list completeness and answer-shape reconciliation
  are allowed only for unambiguous, source-valid reflection JSON. A
  deterministic ownership guard rejects a different speaker's direct response
  to the named subject's second-person question; an explicit attribution such
  as `you will visit` remains valid. No expected answer, gold label, or judge
  response is available to these checks.

| Gate | Semantic accuracy | Official F1 | Candidate recall | Delivered recall | Rendered recall / Hit |
|---|---:|---:|---:|---:|---:|
| Multi-hop n=16 | **75.0%** | **41.75%** | 64.43% | 58.18% | **63.39% / 87.5%** |
| Open-domain n=20 | **70.0%** | **44.01%** | 57.92% | 63.75% | **65.42% / 70.0%** |

Candidate and delivered recall use the same annotated-gold denominator but are
not subset surfaces: candidate recall scores provenance on the first 50 raw
cognitive trace nodes, while delivered recall scores the hydrated and
repackaged product fragments after canonical-source and atomic-source routing.
That source expansion is why the verified Open-domain delivered value can be
higher than its candidate value.

- **Paired checks:** the Multi-hop question set is identical to the earlier
  v190e/v190f gate. Rendered recall rose from 59.23% to 63.39%; semantic
  accuracy rose from 56.25% to 75.0%, and the countries answer remains exactly
  `Italy, Turkey, Mexico, Canada, Greenland` without cross-speaker Japan
  contamination. On the identical Open-domain v198a set, semantic accuracy
  and official F1 remain exactly 70.0% and 44.01%; the degree, creator,
  explicit-alternative, named-company, named-animal, and both Universal
  Studios state answers remain correct.
- **Interpretation:** these small type screens are regression gates, not a new
  unified n=100 or full-LoCoMo headline and not directly comparable to public
  LLM-judge scores from other memory systems. The engine remains model-free;
  the reader contract is a reference consumer policy over the exact product
  context and provenance surface.
- **Evidence:**
  `local-answer-locomo-v199a-final-multi-hop-n16.json`,
  `local-answer-locomo-v200b-subject-owned-multi-hop-n16-answer-judge.json`,
  `local-answer-locomo-v193a-creator-window-open-domain-n20.json`, and
  `local-answer-locomo-v201a-subject-owned-open-domain-n20-answer-judge.json`.

## 2026-07-30 — production deep-selection and isolated atomic-fact routing

- **Data:** LoCoMo seed 42, 25 questions per non-adversarial type, n=100;
  `Xenova/bge-base-en-v1.5` embeddings, `BAAI/bge-reranker-base`, frozen
  reference-blind Qwen 3.6 extraction artifact, and the exact
  `Memory::search_reranked` product path.
- **Architecture:** the first 30 cognitive rows remain unchanged. The 50
  reranker documents are coverage-aware selections from a 200-row diagnostic
  trace using query facets, canonical raw sources, source sessions, and a
  bounded temporal bridge; source-aware assembly then delivers at most 20.
  Reviewed atomic facts live in a separate SQLite sidecar and can route only
  their cited live Episodic sources into this path; they never become graph
  nodes or enter node FTS.
- **Reader-free result:** overall candidate@50 / selected@20 / rendered recall
  is **78.48% / 76.31% / 79.64%**, with rendered Hit **89%**.

| Type | Baseline rendered | Current rendered | Delta |
|---|---:|---:|---:|
| multi-hop | 59.40% | **64.90%** | **+5.50 pp** |
| open-domain | 56.03% | **61.67%** | **+5.64 pp** |
| single-hop | 96.00% | **96.00%** | 0.00 pp |
| temporal | 96.00% | **96.00%** | 0.00 pp |

- **Latency:** 1.71 s mean, 2.52 s p95, and 2.97 s maximum, below the
  production 4 s fail-open boundary.
- **Local reader contract smoke:** loopback OMLX
  `Qwen3.6-35B-A3B-4bit`, no API key, `enable_thinking=false`, n=8 balanced
  integration sample. Official F1 was 61.28%; all four reflection responses
  parsed as complete JSON, and Collection items carried source IDs present in
  the supplied evidence. This small run validates the provider/contract path;
  it is not promoted as an n=100 answer-quality headline.
- **Calibration:** no graph/readout coefficient or embedding model calibration
  changed. The gain comes from production preselection, isolated fact routing,
  and source-aware final assembly.
- **Evidence:** local reports
  `local-answer-locomo-v175-production-full-n100.json`,
  `/tmp/anamnesis-v176-omlx-qwen-gate-source-n8.json`, and
  `/tmp/anamnesis-v176-omlx-qwen-gate-answer-n8.json`.

## 2026-07-29 — production reranked-recall widths and local Qwen gate

- **Data:** LoCoMo seed 42, 25 questions per non-adversarial type, n=100;
  `Xenova/bge-base-en-v1.5` embeddings, `BAAI/bge-reranker-base`, exact
  product `Memory::search_reranked` packaging.
- **Width screen:** candidate@20 rendered recall was 67.98%; candidate@50
  reached 76.86% at 1.69 s mean / 2.68 s p95; candidate@100 reached 76.14%
  at 2.77 s mean / 4.49 s p95. Candidate@50 is promoted. Candidate@100 is
  rejected because broader raw recall did not survive final selection and
  exceeded the latency boundary.
- **Final-context screen:** with search@20 and candidate@50 fixed, final@8,
  final@12, and final@20 produced Qwen 3.6 n=20 semantic accuracy of
  60% / 60% / 65% and official F1 of 41.71% / 42.31% / 43.35%.
  Final@20 is promoted; prompt size averaged about 1,841 tokens.
- **Selection:** direct queries retain relevance order for post-package
  `knowledge_only` safety. Inference and temporal queries use canonical-source
  coverage. Enumeration, relationship, and frequency queries use bounded
  source-session coverage.
- **Local reader gate:** loopback OMLX `Qwen3.6-35B-A3B-4bit`,
  `enable_thinking=false`, complex-only reflection, n=100: 66% semantic
  accuracy, 49.11% official F1, zero judge parse failures. Type semantic
  accuracy is 48% multi-hop, 52% open-domain, 84% single-hop, and 80%
  temporal.
- **Frontier reader screen:** after explicit approval, GPT-4o answered the
  exact frozen product contexts with the same complex-only reflection policy
  and a GPT-4o judge. Official F1 was 45.44%, versus Qwen's 49.11%:
  +1.48 pp multi-hop, -9.51 pp open-domain, -16.24 pp single-hop, and
  +9.59 pp temporal. The GPT judge score was 55% with zero parse failures but
  is not directly comparable to the Qwen-judge semantic score. The run used
  492,561 input and 11,410 output tokens; the harness estimated $1.345503
  under its declared GPT-4o prices, below the authorized $5 cap.
- **Operational decision:** MCP hooks and the benchmark share engine constants
  for search@20, candidate@50, and final@20. Hook fail-open moves from 3 s to
  4 s under the existing 5 s plugin backstop because the measured maximum was
  3.09 s.
- **Calibration:** no graph/readout coefficient or embedding model calibration
  changed. These are product orchestration and evidence-selection defaults.
- **Evidence:** `local-answer-locomo-v155-final-production-default-c50-search20-final20-n100-source.json`
  and
  `local-answer-locomo-v156-final-omlx-qwen36-nothink-production-c50-final20-reflect-complex-n100-answer-judge.json`
  and
  `local-answer-locomo-v157-final-gpt4o-production-c50-final20-reflect-complex-n100-answer-judge.json`.

## 2026-07-25 — timestamped Qwen 3.6 product wire (no calibration change)

- **Data:** pinned LoCoMo loader, seed 42, 25 questions per non-adversarial
  category, n=100; local `qwen3.6:35b-a3b`, BGE-base embedding and reranker,
  candidate@100.
- **Fidelity repairs:** dataset epoch seconds are converted to engine epoch
  milliseconds; MCP and benchmark use `Memory::render_context` with source
  observation/validity time; consumer reranking reassembles the final selected
  set against the full token budget.
- **Top-10 product wire:** raw F1 0.4314, selected recall 0.6215, true rendered
  recall 0.7283, exact rendered Hit 0.8400. Synthetic label-only fragments are
  0/1,000.
- **Top-20 candidate:** raw F1 0.4712, selected recall 0.7302, true rendered
  recall 0.7864, exact rendered Hit 0.8800.
- **Paired evidence:** top-20 versus top-10 is +0.0399 raw F1; question
  bootstrap 95% CI +0.0031 to +0.0818, conversation-cluster CI -0.0105 to
  +0.1066; 14 wins, 76 ties, 10 losses.
- **Decision:** keep top-20 as a high-quality candidate, not a promoted default.
  Conversation-level heterogeneity and multi-hop recall/F1 divergence remain.
- **Calibration:** no readout coefficient, embedding default, or core model
  calibration changed. Timestamp rendering and final-set budget reclamation are
  correctness repairs.
- **Evidence:**
  [local-answer-product-wire-v17-qwen36-n100-2026-07-25.md](local-answer-product-wire-v17-qwen36-n100-2026-07-25.md).

## 2026-07-25 — local second-stage reranking (high-quality profile)

> Historical schema-v15 evidence. A later fidelity audit found that its answer
> renderer was not the product `Recall::as_context()` wire and that reranked
> packaging did not reapply validity/mode semantics. Schema v16 repairs both;
> the result below remains evidence that second-stage ranking helped that
> harness, but requires product-wire reconfirmation before current promotion.

- **Data:** pinned LoCoMo loader, seed 42, 50 questions per non-adversarial
  category, 200 questions total. Frozen local Qwen3.5 35B-A3B greedy reader and
  official deterministic LoCoMo token F1.
- **Baseline:** shipped cognitive top 10 scored 0.3529 F1, Recall@10 0.5283,
  hit@10 0.6450.
- **Accepted candidate:** the first 100 cognitive readout candidates reranked
  locally by `BAAI/bge-reranker-base`, then packaged at top 10. F1 0.4061,
  Recall@10 0.6165, hit@10 0.7400.
- **Paired evidence:** +0.0532 F1; question bootstrap 95% CI +0.0081 to
  +0.1003; conversation-cluster bootstrap +0.0032 to +0.1133; 51 wins,
  114 ties, 35 losses.
- **Rejected fast profiles:** BGE over 20 candidates scored 0.3895
  (+0.0366, CI crosses zero); Jina turbo over 100 candidates scored 0.3307
  (-0.0222, CI crosses zero). Retrieval-only RRF and indiscriminate L2
  hydration also failed the answer gate.
- **Runtime decision:** BGE top 100 measured mean 4.01 s / p95 4.99 s and
  occupies about 1.1 GB. It is an explicit high-quality profile, not the
  latency-sensitive hook default.
- **Product boundary:** no coefficient or default-model calibration changes.
  The additive, model-agnostic `Memory::repackage_reranked` surface validates
  consumer scores, reuses native packaging, and aligns reinforcement with the
  final exposed fragments. The concrete reranker remains outside the engine.
- **Evidence:** `local-answer-locomo-qwen35-bge-baseline-v0-prompt-v6-greedy-top10-n200-seed42.json`,
  `local-answer-locomo-qwen35-bge-cross-encoder-base-product-api-v1-prompt-v6-greedy-top10-n200-seed42.json`,
  and
  [local-answer-reranking-n200-2026-07-25.md](local-answer-reranking-n200-2026-07-25.md).

## 2026-07-25 — top-10 readout and embedding audit (no calibration change)

- **Data:** current pinned LoCoMo loader, all 1,540 non-adversarial questions,
  top 10, no warmup. Feature dump contains 200 candidates per question.
- **Current BGE result:** Recall@10 0.6955, MRR 0.4620, NDCG@10 0.4971.
- **Split:** even conversations train (788 questions), odd conversations dev
  (752 questions). The shipped point (`w_a=.25`, `w_phi=16`, `w_s=w_z=0`)
  produced dev NDCG 0.4985, MRR 0.4658, Recall@10 0.6939.
- **Rejected base refit:** `[w_a=0, w_phi=16, w_s=.25, w_z=0]` improved train
  NDCG but reduced dev NDCG to 0.4964, MRR to 0.4618, and recall to 0.6925.
- **Rejected raw-signal fit:** holding shipped coefficients fixed and fitting
  raw embedding cosine plus lexical score selected lexical weight 2. Dev NDCG
  moved only 0.4985 → 0.4987 while recall fell 0.6939 → 0.6914. This does not
  clear a calibration-change gate.
- **Corrected embedding audit:** the first E5-large run was raw/unprefixed and
  is invalid as an asymmetric E5 result. FastEmbed reports that variant as
  `Qdrant/multilingual-e5-large-onnx`, which the old `intfloat/`-only detector
  missed. With the real query/passage protocol, E5-large reached Recall@10
  0.7199, MRR 0.5355, and NDCG@10 0.5543. Paired gains over BGE were +0.0244
  Recall (95% CI +0.0127 to +0.0362), +0.0735 MRR (+0.0583 to +0.0888), and
  +0.0571 NDCG (+0.0451 to +0.0690).
- **Rejected default replacement:** despite the corrected retrieval gain,
  paired greedy n=100 LoCoMo answer F1 moved 0.3488 → 0.3444 (-0.0044; 95% CI
  -0.0652 to +0.0543). Multi-hop, open-domain, and single-hop F1 rose slightly,
  while temporal fell. The result does not clear the reader-facing promotion
  gate.
- **MCP-default reference:** correctly prefixed E5-small is statistically tied
  with BGE on full retrieval: Recall@10 0.6926, MRR 0.4761, NDCG@10 0.5034.
  Its paired greedy n=100 answer F1 was 0.3270 versus BGE's 0.3488 (-0.0218;
  95% CI -0.0767 to +0.0305), so it also does not clear the reader-facing
  promotion gate.
  FastEmbed already reported this model with the recognized `intfloat/`
  identity, so its durable identity remains unchanged and no no-op migration is
  introduced.
- **Decision:** retain the 2026-06-11 v2 coefficients and BGE-base default.
  Retain the migration-safe query/passage correctness fix for formerly raw
  Qdrant E5 identities. Retrieval-only improvement is not sufficient; a future
  refit or model replacement must improve paired reader-facing answer F1.
- **Evidence:** `real-memory-locomo-official-v3-top10-20260725.json`,
  `locomo-features-official-v4-signals-top10-20260725.jsonl`,
  `real-memory-locomo-e5-small-query-passage-v1-official-top10-20260725.json`,
  `local-answer-locomo-qwen35-e5-small-prompt-v6-greedy-top10-n100-seed42.json`,
  `real-memory-locomo-e5-large-query-passage-v1-official-top10-20260725.json`,
  `local-answer-locomo-qwen35-e5-large-query-passage-v1-prompt-v6-greedy-top10-n100-seed42.json`,
  and
  [local-answer-greedy-n100-2026-07-25.md](local-answer-greedy-n100-2026-07-25.md).

## 2026-06-11 v2 — readout coefficients refit (deduped NDCG objective)

- **Supersedes** the v1 fit below. Tool change: `fit_readout` now replays the
  report's novelty-deduped gains in re-ranked order and optimizes mean
  NDCG@20 (rows carry `matched_units` + `total_relevant`).
- **Values:** `w_a = 0.25`, `w_phi = 16.0`, `w_s = 0.0`, `w_z = 0.0`;
  `w_scope = w_trust = w_stress = 1.0` (unfit, declared priors).
- **Live confirmation (LoCoMo full non-adversarial, includes the relative-time
  cue + LoCoMo question-time fallback changes of the same date):** Recall@20
  0.540 → **0.776**, MRR 0.188 → **0.291**, NDCG 0.256 → **0.386**, hit@20
  0.614 → 0.846. Dev-half (never seen by the fit): 0.778 / 0.287. Offline
  replay now agrees with live (predicted 0.756 / 0.286).
- **Why `w_s = 0`:** with no usage data the salience projection
  `s_i = logistic(A_i)` carries only the creation-time reservoir (encoding
  surprise) — in logit space it spans ≈6–14 and is noise w.r.t. query
  alignment, the same pathology the A_i phi exclusion removed, re-entering
  through the salience channel. REFIT with real usage/commit data before
  relying on salience at readout in long-lived deployments.
- **Method lesson:** a per-node, static-surface proxy objective diverged from
  the live metric; the replayed-dedup objective on the dumped surface closed
  the gap. Fitted points must still be live-confirmed (the dump cannot see
  nodes outside its 200-row surface).
- **Evidence:** `real-memory-locomo-fit2pt-20260611.json`,
  `fit2-readout-20260611.json`, `locomo-features-v2-20260611.jsonl`.

## 2026-06-11 — readout coefficients (`w_a`, `w_phi`, `w_s`, `w_z`)

- **Data:** LoCoMo-10 non-adversarial (1540 questions), retrieval-only dry run,
  `Xenova/bge-base-en-v1.5` embeddings, no warmup. Per-candidate readout
  feature rows dumped with `--dump-features` (`trace.readout`, cap 200/question).
- **Split:** even `sample_index` conversations = train, odd = dev. Weights were
  never fit on the dev half; full-set numbers below therefore include the train
  half and overstate slightly relative to dev.
- **Procedure:** `fit_readout` coordinate search, grid
  `{0, 0.25, 0.5, 1, 1.5, 2, 4}`, objective mean per-node MRR@20 (a proxy for
  the novelty-deduped report MRR; see the tool header).
- **Values:** `w_a = 4.0`, `w_phi = 4.0`, `w_s = 1.0`, `w_z = 0.0`;
  `w_scope = w_trust = w_stress = 1.0` (constant in the fit data — left as
  declared priors).
- **Result:** train MRR 0.1722 → 0.1924 (+11.7%), dev MRR 0.1629 → 0.1831
  (+12.4%) over unit coefficients. Full-set re-measurement with the applied
  weights: Recall@20 0.5084 → 0.5404, MRR 0.1677 → 0.1878, hit@20
  0.582 → 0.614 (dev-half only: Recall@20 0.5258, MRR 0.1831).
- **Interpretation:** `w_z = 0` removes the double-counted activation signal —
  the hot-path approximation `Z_i = -ln(a_i)` (energy.md) duplicates
  `logit(a_i)` for small activations, so unit weights effectively scored
  activation twice. The seven-term form is unchanged; only the coefficient is
  calibrated off. `beta_prior = 1` (potential-field) untouched — it is derived,
  not a knob.
- **Companion code fix (same date):** readout `phi_i` is alignment-only — the
  prior `A_i` was excluded from the readout-side potential because the cached
  reservoir (creation base-level + encoding-surprise prior, ≈3–12 log-odds)
  drowned the bounded alignment features and the same reservoir already enters
  the score as `logit(s_i)`. readout-scoring.md lists `A_i` as "read input and
  tie-breaker"; the seed field keeps `beta_prior · A_i` per
  potential-landscape.md. Measured effect of the bad state: LoCoMo Recall@20
  0.508 → 0.228 (with the speaker-cue regression compounding).
- **Alternative point (recall-leaning, not shipped):** extended-grid refit
  found `w_a=0.25, w_phi=16` with proxy dev MRR 0.2847 — live re-measurement
  gave Recall@20 0.577 / hit@20 0.658 but report MRR 0.153 and NDCG 0.242
  (worse than the shipped point: 0.540 / 0.188 / 0.256). The per-node proxy
  diverges once live scoring re-selects the top-200 trace cap; a deduped,
  live-surface objective is future work. Evidence:
  `real-memory-locomo-postfit2-20260611.json`.
- **Negative results worth keeping:**
  - RWR visit budget ×10/×20 changes nothing (identical metrics, +18ms p50):
    the activation set already converges under the default budget.
  - Seed-limit expansion (40/80 vs top-k=20) *hurts*: Recall@20
    0.508 → 0.376 → 0.249. Restart mass spreads over low-quality fused
    candidates; candidate starvation is not the bottleneck.
  - Speaker entity-tag cues (one tag ≈ half a conversation's nodes; the entity
    collector returns NodeId-ordered arbitrary matches) flood seed fusion:
    Recall@20 0.504 → 0.285. Bench default is cues-off (`--speaker-cues` to
    re-enable for ablations) until the entity channel is selectivity-aware.
- **LongMemEval-S full official split (500 questions), v2 weights:** Recall@20
  0.938, MRR 0.872, NDCG 0.808, hit@1 0.826, hit@20 0.980, p50 25.7ms.
  Per type: knowledge-update 0.981, multi-session 0.924, temporal-reasoning
  0.884, single-session-assistant 1.000, single-session-user 0.986,
  single-session-preference 0.900. Evidence:
  `real-memory-longmemeval-full500-20260611.json`.
- **LongMemEval-S stratified check (30/type, 180 questions, all six types):**
  with the shipped point — Recall@20 0.896, MRR 0.817, NDCG 0.770, hit@1
  0.744, p50 17.6ms. Hard types hold up: multi-session 0.752,
  temporal-reasoning 0.839, knowledge-update 0.983. (The prior 2026-06-10
  measurement covered only 50 single-session-user questions: 0.90 / 0.6725.)
  Evidence: `real-memory-longmemeval-strat30-postfit-20260611.json`.
- **Evidence:** `real-memory-locomo-fixed-20260611.json`,
  `fit-readout-20260611.json`,
  `abl-*.json` (ablation matrix),
  `real-memory-locomo-postfit-20260611.json`.
