# Hook Triggering

Hooks connect best-effort capture and proactive recall to an agent host. They
are clients of the shared daemon and use the same conversation-ingest and
`search_reranked` paths as MCP and direct integrations. Optional derived
formation remains a consumer/daemon concern with its own admission contract.
The decision rationale is recorded in
[ADR-0011](../adr/0011-activation-gated-triggering.md).

## Why a hook exists

MCP tools are invoked at the model's discretion. They provide deliberate read
and write operations, but they cannot guarantee that relevant memory is
consulted before an answer. A host hook can request recall at a stable lifecycle
boundary and inject context only when the engine reports a useful result.

Always-on injection is unsafe: it adds blocking latency, consumes context, and
can introduce topically similar distractors. The hook therefore performs a
bounded local recall and applies an activation/relevance gate. Below the gate it
emits no context.

## Event contract

| Hook | Action |
|---|---|
| `SessionStart` | Resolve namespace and seed a small project/global context when eligible. Trigger deferred formation work without blocking the session on it. |
| `UserPromptSubmit` | Run canonical reranked recall against the prompt and inject only an eligible, token-bounded product rendering. |
| `Stop` | Capture the recent text-bearing turn window idempotently. |
| `PreCompact` | Flush the longer raw tail before host context compaction. |
| `SessionEnd` | Submit a final tail when the host supports this event; other supported events remain best-effort when it is absent. |

Host-specific event support and wire shapes are adapters. Capture, formation,
recall, selection, and rendering policy remain shared product code.

## Recall gate

The hook uses the filtered product result, not an independent keyword search:

```text
result = search_reranked(prompt, configured_budget)
if result.recall.has_evidence
   and result.recall.readout_score >= readout_threshold
   and result.recall.relevance >= relevance_threshold:
    inject(render_context_for_plan_with(result.plan, result.recall, configured_style))
else:
    inject(nothing)
```

The readout and relevance thresholds, candidate width, and token budget are
versioned calibrated policy. They are fitted from accepted-context labels and
reported by recall telemetry; they are not universal constants. The
`RerankedRecall` retains the exact inferred plan, and rendering passes that plan
to `Memory::render_context_for_plan_with` over the selected package. This keeps
answer shape, derivation, temporal constraint, coverage, and reader guidance
consistent across retrieval and rendering. The hook does not maintain an
independent selection or rendering policy.

## Formation and admission

At supported capture events, the hook submits a bounded recent transcript
window to the daemon. Repeated delivery is idempotent, but capture is
best-effort: a missing host event, unreadable transcript, timeout, or
unreachable daemon can leave a turn uncaptured. Once a raw source is persisted,
later formation failure does not remove it.

Derived formation runs in the consumer/daemon layer and may use a configured
provider; the shipped default is local. Under the contract proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md),
every derived item crosses the same source-grounding and admission transaction
used by other clients. Hook capture does not grant a derived fact a higher trust
class and does not write directly to graph truth.

## Reinforcement gate

Proactive hook recall is read-only. Retrieving or injecting hook context does
not append access traces or strengthen edges, because exposure alone does not
show that the model consumed the evidence.

Explicit MCP `recall` has a separate deliberate-use contract. With the server's
reinforcement option enabled, a successful explicit call commits the package it
returns; disabling that option keeps the call read-only. Direct `Memory`
consumers make the same choice explicitly by calling `Memory::used`. This
distinction prevents proactive injection from training on its own exposure
while preserving an intentional use signal for pull-based clients.

## Latency and failure

The daemon keeps graph and local embedding/reranker state warm. Hook latency is
measured from query receipt through exact context rendering; consumer prompt
wrapping and model generation are separate. Every stage has a bounded budget.

Hooks are fail-open with respect to the host prompt: daemon, model, formation,
telemetry, or rendering failure emits no additional context and never blocks or
alters the user's prompt. A capture failure is audited when possible and does
not erase an already persisted raw source.

## Policy inputs

- activation/readout threshold;
- relevance threshold;
- candidate, final-evidence, and token limits;
- per-event capture window;
- formation batch and timeout limits;
- namespace/scope resolution; and
- reinforcement policy for explicit, pull-based recall.

## Invariants

- Hook, MCP, plugin, and direct crate recall share one ranking, selection, and
  rendering policy.
- A hook never opens the database beside the daemon.
- Below-threshold recall injects nothing.
- A persisted raw source survives optional formation failure; hook delivery
  itself remains best-effort.
- Routing-only facts can contribute only by hydrating their source evidence.
- Proactive hook retrieval never reinforces memory. Explicit MCP and direct
  crate calls follow their separately declared use/commit contract.
- Telemetry stores bounded decision metadata, not raw prompts or rendered
  context.
