# 0011. Activation-Gated Hook Triggering

- Status: Accepted (implemented 2026-06-23, v0.8.0)
- Date: 2026-06-17
- Related: [ADR-0004](0004-query-as-field-and-commit.md), [ADR-0006](0006-frustration-not-deletion.md), [ADR-0008](0008-powerlaw-dissipation.md), [ADR-0010](0010-calibrated-priors-not-laws.md), [hook-triggering design](../05-context-retrieval/hook-triggering.md)

## Context

An MCP server exposes memory tools but cannot require a model to call them
before answering. A client hook can request recall at a reliable lifecycle
boundary, but blind per-turn injection creates three risks: blocking latency,
token bloat, and semantically related distractors.

Anamnesis already computes query-local activation and relevance. The triggering
contract should use those signals to decide whether any memory enters context,
while keeping proactive hook recall read-only. Pull-based clients retain a
separate explicit-use contract.

## Decision

Use **activation-gated triggering**:

1. On a prompt, run the canonical bounded recall path and inject only when its
   filtered top result clears the configured activation/readout and relevance
   thresholds.
2. Cap delivered evidence and token use independently from the broader
   candidate surface.
3. Never reinforce proactive hook retrieval or injection. Explicit MCP recall
   may commit its returned package under its configured deliberate-use policy;
   direct crate consumers commit by calling `Memory::used`.
4. Surface relevant contradiction bundles and typed reasoning chains with both
   provenance paths.
5. Attempt bounded raw-turn capture at supported lifecycle events and keep
   optional derived formation outside the engine. Repeated delivered windows
   are idempotent; host delivery remains best-effort.

## Rationale

ACT-R activation can be interpreted as a calibrated estimate of retrieval need
under its modeling assumptions [1][2][3][4]. It is not an identity between
human recall and LLM usefulness, so thresholds remain fitted priors under
[ADR-0010](0010-calibrated-priors-not-laws.md).

Context quality degrades non-uniformly as irrelevant or near-miss material is
added [5][6][7]. Gating and token budgets therefore protect answer quality as
well as latency.

Reinforcing proactive injection on exposure alone would create a
self-amplifying popularity loop [8][9]. Keeping that path read-only is the
primary brake. Explicit pull-based recall can still declare use, and the
activation-dependent decay in [ADR-0008](0008-powerlaw-dissipation.md) further
discounts massed re-presentation.

Typed contradictions and reasoning chains require graph-aware packaging. The
hook consumes the canonical context package instead of reducing recall to a
fixed profile dump.

## Consequences

- Hook recall requires a warm, bounded local path; the shared daemon provides
  model reuse and single-writer safety.
- Thresholds, candidate width, final evidence width, and token budget are
  versioned calibrated policy.
- A low threshold can flood context and a high threshold can starve recall;
  telemetry reports eligibility and abstention without recording prompt text.
- Hook, MCP, plugin, and direct `Memory` integrations must share retrieval,
  selection, and rendering behavior.
- Proactive hook retrieval remains read-only. Explicit MCP recall and direct
  `Memory` consumers follow their separately declared commit contracts.

## References

1. Schooler & Anderson (2017), *The Disjunctive Memory Search Model* / ACT-R memory. <http://act-r.psy.cmu.edu/wordpress/wp-content/uploads/2021/07/SchoolerAnderson2017.pdf>
2. Anderson & Milson (1989), *Rational Analysis as a Link between Human Memory and Information Retrieval*. <https://www.researchgate.net/publication/250059508>
3. Danker & Anderson, ACT-R activation as log posterior odds. <https://pmc.ncbi.nlm.nih.gov/articles/PMC2733322/>
4. Stocco, Lebiere et al. (2023), availability and retrieval utility. <https://link.springer.com/article/10.1007/s42113-023-00189-y>
5. Chroma Research, *Context Rot*. <https://www.trychroma.com/research/context-rot>
6. Liu et al. (2023), *Lost in the Middle*. <https://arxiv.org/abs/2307.03172>
7. *Large Language Models Can Be Easily Distracted by Irrelevant Context* (2023). <https://arxiv.org/abs/2302.00093>
8. *Feedback Loops in Recommender Systems*. <https://arxiv.org/abs/2007.13019>
9. *Feedback loops and complex dynamics in recommender systems*. <https://dl.acm.org/doi/full/10.1145/3564284>
