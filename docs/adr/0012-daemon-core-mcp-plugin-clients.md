# 0012. Daemon as Shared Core; MCP and Plugin as Distinct Clients

- Status: Accepted
- Date: 2026-06-23
- Related: [0011 activation-gated triggering](0011-activation-gated-triggering.md), [hook-triggering design](../05-context-retrieval/hook-triggering.md), [0010 calibrated priors](0010-calibrated-priors-not-laws.md)

## Context

anamnesis runs a **local embedding model** (bge-base) over an **in-memory graph cache** on top of SQLite. It exposes memory through two surfaces that look unrelated but must share one engine state:

- **MCP tools** the agent calls deliberately — `recall` / `remember` / `relate` / `ingest_conversation`.
- **Client-side hooks** (Claude Code / Codex `SessionStart` + `UserPromptSubmit`) that inject a proactive recall before the model answers ([ADR-0011](0011-activation-gated-triggering.md)).

Two questions this ADR settles: (1) how do these surfaces reach the engine, and (2) what is their relationship to each other — is the hook "an MCP thing", should the plugin embed the MCP server, may either open the DB directly?

## Decision

Adopt a **three-layer split** where one daemon is the core and the other two surfaces are *clients* of it:

1. **daemon — the core.** `anamnesis-mcp daemon` owns the DB + in-memory graph + the embedding model (loaded once). It is **on-demand** (the first client auto-spawns it), **ref-counted** (grace-exits when idle, `ANAMNESIS_DAEMON_GRACE_SECS`), and serves N clients over a **per-DB unix socket**. The single-writer lock lives on the daemon, so it is the *only* process that opens the DB.
2. **MCP — a daemon client.** `anamnesis-mcp serve` is a thin stdio↔socket proxy the agent's MCP client speaks to; it carries the agent-judged **capture + reinforcement** half (`remember`/`relate`, and deliberate `recall` whose call *is* the "used" signal).
3. **plugin — a daemon client.** `anamnesis-mcp hook <event>` reads the hook JSON, does a gated read-only recall over the socket, and emits `additionalContext`. It carries the **proactive recall** half.

**MCP and plugin are distinct clients of the same daemon.** The hook does not use MCP *semantics* — it only needs a recall against the warm engine; reusing the MCP wire over the socket is incidental, not required. The plugin **does not bundle/embed the MCP server**, and **neither client opens the DB directly** (the lock would reject it). There is no `--embedded` hook mode.

## Why this shape

1. **The daemon is forced by local-first, not chosen for its own sake.** A local embedding model + in-memory SoA graph means each fresh process would reload the model (multi-second) and fight the single-writer lock. `tokio` async serializes concurrency *within* one process but cannot coordinate separate OS processes. A warm, single-owner persistent process is the only way to make per-prompt recall affordable and lock-safe — which is exactly what the daemon is.
2. **MCP and hooks are complementary, not redundant ([ADR-0011](0011-activation-gated-triggering.md)).** MCP alone cannot drive *proactive* recall (it is pull-based); hooks alone cannot make the *insight-vs-noise* judgment the agent makes when it calls `remember`/`relate`, nor produce the deliberate-use signal that gates reinforcement. Both halves are required, so both stay — as separate clients.
3. **Keep them distinct (do not fold MCP into the plugin).** Embedding the MCP server in the plugin manifest (`mcpServers`) is possible, but separate registration keeps clean boundaries and independent lifecycles. The two-step install is an accepted cost; one-install bundling stays a future ergonomic option, not the default.
4. **Convergent-evolution validation.** A survey of mem0 (platform + OpenMemory), supermemory, Zep/Graphiti, basic-memory, and claude-mem shows the *on-demand local model-warming daemon shared by both MCP and hooks* is essentially unique to anamnesis. The reason is the driver in (1): every cloud-offloading tool moves embeddings/inference to a remote API and can therefore be a stateless HTTP client or a per-session stdio process — no local warmth, no daemon. The **only** close analog, **claude-mem**, runs local ONNX embeddings + a vector store in an on-demand local worker shared by hooks and MCP — independently converging on the same shape (it uses a TCP-port singleton rather than our unix-socket + ref-counted grace-exit, and still offloads its LLM-compression step). The daemon is therefore the necessary cost of local-first, corroborated by the one other local-model tool arriving at the same design.

## Consequences

- The daemon is **non-removable** while the local-model + in-memory-graph design holds. If embeddings were ever offloaded to a remote service, this layer could collapse into stateless clients — a hypothetical, not a plan.
- The hook path stays a daemon client (no embedded mode) for warm-model + lock safety; both clients must tolerate the daemon being absent or restarting (the hook is fail-open; the MCP launcher respawns the daemon).
- The socket protocol is MCP today (reused for the agent path). A bespoke lightweight hook protocol would shave the MCP handshake off the hook path, but adds a second daemon surface for marginal latency — **explicitly deferred**.
- New work must respect this split: reach the engine *through the daemon*; new agent capabilities go on the MCP client, new proactive/automatic behaviors go on the plugin client, and shared state lives in the daemon — never a second DB opener.

## References

- [ADR-0011 activation-gated triggering](0011-activation-gated-triggering.md) and [hook-triggering](../05-context-retrieval/hook-triggering.md) — the "MCP can't do proactive recall" / complementarity argument.
- claude-mem worker-service (on-demand local worker; ONNX MiniLM + Chroma, shared by hooks + MCP): https://docs.claude-mem.ai/architecture/worker-service
- OpenMemory / self-hosted mem0 (long-running server, cloud-default embeddings): https://mem0.ai/blog/self-host-mem0-docker
- supermemory Claude Code integration (cloud core; hooks + MCP share one HTTP API): https://supermemory.ai/docs/integrations/claude-code
- Graphiti / Zep MCP server (server + graph DB, cloud LLM/embeddings): https://github.com/getzep/graphiti
