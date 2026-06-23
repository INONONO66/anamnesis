# 0011. Activation-Gated Hook Triggering, Not Flat-Profile Injection

- Status: Proposed
- Date: 2026-06-17
- Related: [0004 query-as-field-and-commit](0004-query-as-field-and-commit.md), [0006 frustration-not-deletion](0006-frustration-not-deletion.md), [0008 power-law dissipation](0008-powerlaw-dissipation.md), [0010 calibrated priors](0010-calibrated-priors-not-laws.md), [hook-triggering design](../05-context-retrieval/hook-triggering.md)

## Context

To make an agent *use* memory (not just store it), a general MCP server is not enough: tool descriptions and the MCP `instructions` field guide tool *selection* but cannot instill the *policy* "consult memory before answering." Empirically, even with an aggressive `recall` description (`"ALWAYS call before answering"`) and a recall-mandate `instructions` field, our own transcripts show `recall` is invoked ~5× less than `remember` — memory is written but rarely read. The reliable lever is a client-side **hook** (e.g. a Claude Code plugin).

We surveyed how the ecosystem triggers memory via hooks (supermemory, mem0, claude-mem, basic-memory, doobidoo/mcp-memory-service). The dominant pattern:

- **Recall**: a static profile/recent-memory dump injected once at `SessionStart`; per-turn they inject a **directive** ("silently decide whether recalling would help — *don't search reflexively*") and let the model invoke search itself, OR gate a pull behind a confidence threshold + cooldown. **Blind per-turn search is deliberately avoided.**
- **Capture**: incremental at `Stop` / `PostToolUse` / `PreCompact` (not session-end-only).

They avoid per-turn recall for three reasons: **latency** (the `UserPromptSubmit` hook blocks the prompt), **context bloat / token cost**, and **relevance** (the model knows more than a blind keyword search).

The question this ADR settles: **should anamnesis copy that flat-profile pattern verbatim?**

## Decision

**No.** anamnesis is an associative / spreading-activation store, not a flat key-value profile, so it uses **activation-gated triggering**:

1. **Recall = activation-gated per-turn.** On each prompt, run a spreading-activation pass seeded by the prompt and **inject the readout only when the top activation clears a need-odds threshold `τ`** (`top ≥ τ`; ranked, capped to top-`k` to bound tokens). Seed `SessionStart` with high-base-level (frequently/recently used) project-scoped memories.
2. **Reinforce on use, never on every recall.** Hebbian co-activation strengthening (`commit`/`used`) fires **only when a recall was actually relevant/used**, not on every retrieval.
3. **Surface tension and reasoning chains.** When the activated set contains a `Contradicts` tension or a causal/decision chain, surface it proactively (`⚠️ contradicts prior X`; the *why*, not just the *what*).
4. **Capture incrementally** at `Stop` / `PostToolUse` / `PreCompact` — adopt the ecosystem's capture cadence as-is (this part is not flat-store-specific).

## Why copying the flat-profile pattern verbatim is wrong

The flat-store "static profile + per-turn directive, don't search reflexively" design is a **workaround for a missing relevance signal**, not a best practice to imitate. A KV/profile store cannot tell whether a stored item is *needed* for the current turn, so it must offload that judgment to the model (the directive) and otherwise dump a fixed profile. An activation store does not have that deficit:

1. **anamnesis already computes the gate.** In ACT-R rational analysis, a memory's activation `A = base-level + spreading` **reflects the log posterior odds that the memory is needed in the current context** (base level = log prior odds from recency/frequency; associative strength = log likelihood ratio from cues), and retrieval is rationally governed by a **need-probability / expected-utility threshold** — consult memories in order of expected utility and stop once `P·G < C` [1][2][3][4]. So "inject only if the graph activates above threshold" is a principled gate, not a heuristic — the very judgment a flat store must outsource, anamnesis performs natively. (Caveat: the literature says activation *reflects* need-odds under idealizing assumptions, and "needed by a human-memory model" ≠ "useful to an LLM"; we treat `τ` as a **calibrated prior** per [ADR-0010](0010-calibrated-priors-not-laws.md), not an identity.)

2. **Always-on injection is harmful, so the gate matters.** LLM accuracy degrades non-uniformly as input grows even when the task does not get harder ("context rot"), follows a U-shaped *lost-in-the-middle* curve, and **a single topically-related-but-irrelevant item measurably hurts** — and distraction is driven by *semantic overlap with the query*, i.e. exactly the near-miss an associative store surfaces [5][6][7]. Unvetted per-turn injection is therefore actively risky; the activation score is what lets us inject *only* the above-threshold subset instead of a fixed dump.

3. **Reinforcing every recall would self-destruct.** Reinforcing a system on its own retrieval interactions reproduces the documented recommender **feedback loop → rich-get-richer / Matthew / preferential-attachment** runaway (a few popular nodes concentrate all strength; diversity collapses) [8][9]. Use-gating is the first brake; anamnesis's **activation-dependent (Pavlik–Anderson) decay is the second** — massed/temporally-clustered re-encodings get a *higher* decay rate, structurally discounting runaway reinforcement [3][4][10]. A flat store has neither dynamic, so its read path is correctly write-free — but ours is a *learning* path that must be gated, not copied as read-only.

4. **A flat store structurally cannot surface contradictions or reasoning chains.** Proactively flagging a `Contradicts` tension or injecting a causal/decision chain is only possible on a typed graph ([ADR-0006](0006-frustration-not-deletion.md)). Copying the flat pattern would discard exactly the capability that differentiates anamnesis.

## Consequences

- The hook plugin must run a per-turn activation pass — cheap because the shared **on-demand daemon** keeps the graph + model warm (a CLI `recall` is a local socket round-trip, not a cloud call), which is precisely what makes activation-gated per-turn recall affordable where cloud stores cannot.
- `τ` (need-odds injection threshold) and `k` (top-k cap) join the calibrated-prior set; they must be tunable and refit, never presented as laws ([ADR-0010](0010-calibrated-priors-not-laws.md)).
- Reinforcement stays on the `commit`/`used` path ([ADR-0004](0004-query-as-field-and-commit.md)); the hook must distinguish "recalled" from "used" so it never reinforces a below-threshold or unused retrieval.
- Risk: a mis-set `τ` either floods context (too low) or starves recall (too high); mitigated by the top-`k` cap and treating `τ` as fit-from-data.

## References

1. Schooler & Anderson (2017), *The Disjunctive Memory Search Model* / ACT-R memory — activation reflects log posterior odds of being needed. http://act-r.psy.cmu.edu/wordpress/wp-content/uploads/2021/07/SchoolerAnderson2017.pdf
2. Anderson & Milson (1989), *Rational Analysis as a Link between Human Memory and Information Retrieval* — retrieval ≡ IR posterior `P(d|q) ∝ P(q|d)P(d)`. https://www.researchgate.net/publication/250059508
3. Danker & Anderson, ACT-R activation `A_i = B_i + Σ W_j S_ji` as log posterior odds. https://pmc.ncbi.nlm.nih.gov/articles/PMC2733322/
4. Stocco, Lebiere et al. (2023), *Computational Brain & Behavior* — availability reflects probability retrieval is useful now. https://link.springer.com/article/10.1007/s42113-023-00189-y
5. Chroma Research, *Context Rot* — non-uniform degradation with input length; near-miss distractors. https://www.trychroma.com/research/context-rot
6. Liu et al. (2023), *Lost in the Middle*. https://arxiv.org/abs/2307.03172
7. *Large Language Models Can Be Easily Distracted by Irrelevant Context* (2023). https://arxiv.org/abs/2302.00093
8. *Feedback Loops in Recommender Systems* — bias compounded into rich-get-richer. https://arxiv.org/abs/2007.13019
9. *Feedback loops and complex dynamics in recommender systems* (survey). https://dl.acm.org/doi/full/10.1145/3564284
10. Gershman, *Memory chapter* — need probability as power law of frequency/interval. https://gershmanlab.com/pubs/Gershman_memory_chapter.pdf
