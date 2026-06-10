# Calibration Records

Per [ADR-0010](../adr/0010-calibrated-priors-not-laws.md), every refit of a
calibrated prior records its data, procedure, and result here. Refit when graph
topology, agent behavior, embedding geometry, or dataset changes.

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
- **Negative results worth keeping:**
  - Seed-limit expansion (40/80 vs top-k=20) *hurts*: Recall@20
    0.508 → 0.376 → 0.249. Restart mass spreads over low-quality fused
    candidates; candidate starvation is not the bottleneck.
  - Speaker entity-tag cues (one tag ≈ half a conversation's nodes; the entity
    collector returns NodeId-ordered arbitrary matches) flood seed fusion:
    Recall@20 0.504 → 0.285. Bench default is cues-off (`--speaker-cues` to
    re-enable for ablations) until the entity channel is selectivity-aware.
- **Evidence:** `.omo/evidence/real-memory-locomo-fixed-20260611.json`,
  `.omo/evidence/fit-readout-20260611.json`,
  `.omo/evidence/abl-*.json` (ablation matrix),
  `.omo/evidence/real-memory-locomo-postfit-20260611.json`.
