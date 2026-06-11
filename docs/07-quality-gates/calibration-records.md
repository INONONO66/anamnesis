# Calibration Records

Per [ADR-0010](../adr/0010-calibrated-priors-not-laws.md), every refit of a
calibrated prior records its data, procedure, and result here. Refit when graph
topology, agent behavior, embedding geometry, or dataset changes.

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
- **LongMemEval-S stratified check (30/type, 180 questions, all six types):**
  with the shipped point — Recall@20 0.896, MRR 0.817, NDCG 0.770, hit@1
  0.744, p50 17.6ms. Hard types hold up: multi-session 0.752,
  temporal-reasoning 0.839, knowledge-update 0.983. (The prior 2026-06-10
  measurement covered only 50 single-session-user questions: 0.90 / 0.6725.)
  Evidence: `real-memory-longmemeval-strat30-postfit-20260611.json`.
- **Evidence:** `.omo/evidence/real-memory-locomo-fixed-20260611.json`,
  `.omo/evidence/fit-readout-20260611.json`,
  `.omo/evidence/abl-*.json` (ablation matrix),
  `.omo/evidence/real-memory-locomo-postfit-20260611.json`.
