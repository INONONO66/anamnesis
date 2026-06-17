# Hook Triggering — making the agent actually use memory

> Design for a Claude Code hook plugin (and equivalent IDE hooks) that drives
> anamnesis. The *why* — and why this differs from supermemory/mem0 — is the
> decision record [ADR-0011](../adr/0011-activation-gated-triggering.md). This
> page is the practical strategy.

## The problem a hook solves

A general MCP server cannot make the agent *consult* memory. Tool descriptions
and the MCP `instructions` field steer tool *selection*, not the *policy*
"recall before answering." Measured on our own transcripts: even with `recall`
described as `"ALWAYS call before answering"` and a recall-mandate `instructions`
field, `recall` fired ~5× less than `remember`. Memory accumulated but was not
read. The reliable lever — used by every serious memory product — is a
**client-side hook**.

## What the ecosystem does (surveyed from source)

| tool | recall / inject | capture | per-turn recall? |
|:--|:--|:--|:--|
| supermemory | `SessionStart` profile pull + per-turn **directive** | `Stop` (signal-gated) | directive only — "don't search reflexively" |
| mem0 | `SessionStart` pull + per-turn **rubric** + regex-gated pulls | every 3rd msg + `Stop` + `PreCompact` | rubric only (deduped to once/session) |
| claude-mem | `SessionStart` index only | `PostToolUse` (every tool) + `Stop` summary | no |
| basic-memory | `SessionStart` only | `PreCompact` only | no |
| doobidoo | `SessionStart` + opt-in per-turn **gated** (confidence ≥ 0.6 + 30 s cooldown) | `SessionEnd` | gated only |

**The convergent pattern**: inject once at `SessionStart`; for per-turn, inject a
*directive* (or a confidence-gated pull) rather than a blind search; capture
incrementally. They avoid blind per-turn recall because of **latency** (the
`UserPromptSubmit` hook blocks the prompt), **context bloat / token cost**, and
**relevance** (a blind keyword search knows less than the model).

## Why anamnesis does NOT copy this

The "directive, don't search" compromise is a **workaround for a missing
relevance signal** — a flat store cannot tell whether an item is *needed*, so it
offloads that judgment to the model. anamnesis computes the signal natively:
activation = log posterior odds that a memory is needed in context (ACT-R), so it
can **self-gate**. The full argument + citations: [ADR-0011](../adr/0011-activation-gated-triggering.md).

## The anamnesis strategy

| hook | action |
|:--|:--|
| `SessionStart` | Seed: inject top high-**base-level** (recently/frequently used) memories scoped to the resolved project/global graph — the "what I know about this work" prime. Capped to top-`k`. |
| `UserPromptSubmit` | **Activation-gated recall.** Spread activation seeded by the prompt; inject the readout **only if top activation ≥ `τ`** (need-odds threshold), ranked, top-`k` capped. Below `τ` → inject nothing (no bloat). If the activated set holds a `Contradicts` tension or a causal/decision chain, surface it first (`⚠️ contradicts prior X`). |
| `PostToolUse` | Incremental capture of significant tool effects (Edit/Write/Bash/Task), signal-gated, via `remember` (insight) — conversation flow may go through `ingest_conversation` to build temporal chains. |
| `Stop` | Capture the turn's distilled outcome (`remember`), signal-gated. **Reinforce** (`used`) only the retrievals the turn actually consumed. |
| `PreCompact` | Safety capture of session state before the window collapses. |

### Activation-score gate (the core mechanism)

The readout already produces a per-candidate activation score (the same score
`recall` ranks by). The gate is:

```
hits = recall(prompt)                  # spreading activation from the prompt
if hits and hits[0].score >= τ:        # τ = need-odds injection threshold
    inject(as_context(hits[:k]))       # top-k, rendered: identity / chains / tensions
else:
    inject(nothing)                    # the graph said "nothing relevant" — trust it
```

`τ` and `k` are **calibrated priors** ([ADR-0010](0010-calibrated-priors-not-laws.md)),
tuned from accepted-context labels, never fixed laws. This buys the *relevance*
benefit of per-turn recall that flat stores forgo, without the bloat — because
below-threshold turns inject nothing.

### Reinforcement gate (use, not recall)

Reinforcement (`commit`/`used`, [ADR-0004](0004-query-as-field-and-commit.md))
fires **only when a recall was injected AND used**, never on every retrieval.
Reinforcing every recall reproduces the recommender feedback loop
(rich-get-richer / Matthew effect); use-gating is the first brake and the
activation-dependent (Pavlik–Anderson) decay ([ADR-0008](0008-powerlaw-dissipation.md))
is the second — massed reinforcement self-discounts. The hook therefore tracks
"recalled" vs "used" and only commits the latter.

### Latency: why per-turn recall is affordable here

The cited cost of per-turn recall is the cloud round-trip on a blocking
`UserPromptSubmit` hook. anamnesis runs the recall against the **on-demand shared
daemon** — graph and embedding model stay warm, so a `recall` is a local Unix-socket
call (single-digit–ms graph traversal + one local embed), not a network hop. The
daemon is the enabling infrastructure that makes activation-gated per-turn recall
practical where flat cloud stores must fall back to a directive.

## Tunable priors (refit, don't hardcode)

- `τ` — need-odds injection threshold (per-turn recall gate).
- `k` — top-k cap on injected memories (token budget).
- `SessionStart` seed size and base-level floor.
- capture signal keywords / min-length (bloat control on capture).
- reinforcement: require explicit "used" signal.

## Open questions

- Detecting "used" cleanly from the transcript (which injected memory the turn relied on) to gate reinforcement precisely.
- Calibrating `τ` against a labelled accepted-context set.
- Whether `SessionStart` should also tick (advance forgetting) for long-idle graphs.
