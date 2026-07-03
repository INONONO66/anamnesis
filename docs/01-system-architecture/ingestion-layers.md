# Ingestion Layering — Storage Mechanism vs Formation Policy

Anamnesis separates **what is stored** from **which facts are formed, and when**. The engine
and its write tools are a storage **mechanism**; deciding what to distill raw fragments into,
and when, is **formation**, and formation is a **consumer-layer policy** — not an engine
property. This document states that separation explicitly so ingestion is not mistaken for a
fixed architectural trait.

## The two layers

| Layer | Owner | Responsibility | LLM? |
|---|---|---|---|
| **Storage mechanism** | Engine + daemon + MCP write tools (`remember`, `relate`, `ingest_conversation`) | Persist exactly what it is given — a note, an edge, a set of turns — deterministically. No opinion about fact quality or granularity. | Never — the core is LLM-free (see [overview](overview.md#core-boundary)) |
| **Formation policy** | The consumer (the plugin, the `Memory` recipe, or any caller) | Decide *what* to write and *when* to distill raw fragments into compact facts / reasoning edges. | Consumer's choice |

The mechanism is fixed and lossless. The policy is where all ingestion opinion lives, and it is
swappable per consumer. This is the ingestion-side statement of the [daemon-core / distinct-clients
split](../adr/0012-daemon-core-mcp-plugin-clients.md): the MCP write tools carry out calls; they do
not judge them.

## Formation is composable, not a single strategy

Because storage is a separate layer, a consumer may compose several formation policies over the
same mechanism. The shipped plugin runs two at once (see
[ADR-0013](../adr/0013-reasoning-capture-pipeline.md)):

1. **Passive raw-fragment capture.** Hooks ingest turns verbatim as `Episodic`, synchronously,
   no LLM. Nothing is judged or dropped.
2. **Deferred agent-side extraction.** The next connected agent distills the un-extracted queue
   with *its own* LLM and emits `remember` / `relate` — reasoning structure, formed after the
   fact, at no extra API cost.

A consumer that wants immediate distilled facts MAY add a third policy — **synchronous
pre-extraction** before calling the mechanism — without any engine change. All three write
through the same mechanism; none of them is baked into the engine.

## Why the separation is load-bearing: fragments, not summaries

Keeping storage (raw, lossless fragments) separate from formation (which facts, when) is what
makes fragments-not-summaries hold in practice:

- The raw fragment is **always preserved**, so a missing, late, or wrong extraction never
  destroys the source.
- Formation is therefore **re-runnable**: a better policy, or a later agent, can re-distill the
  same fragments.

Coupling storage to a single distil-at-write step would forfeit this — a formation error would be
baked into the store with no surviving source to recover from. The layer split is precisely the
design choice that avoids that failure mode.

## Rule for contributors

Do not describe Anamnesis ingestion as a fixed "raw turns + deferred extraction" property. That
is the **default plugin policy** — one instance of the formation layer, not an engine limit. New
ingestion behavior is a **policy change at the consumer/plugin layer**; the storage mechanism
stays LLM-free and lossless. New agent-write capabilities go on the MCP client, new automatic
capture/formation goes on the plugin client, and the engine keeps storing exactly what it is told
(cf. [ADR-0012](../adr/0012-daemon-core-mcp-plugin-clients.md),
[ADR-0013](../adr/0013-reasoning-capture-pipeline.md), [framework-layer](framework-layer.md)).
