# 0012. Daemon as Shared Core; MCP Adapter and Hooks as Distinct Clients

- Status: Accepted
- Date: 2026-06-23
- Related: [0011 activation-gated triggering](0011-activation-gated-triggering.md), [hook-triggering design](../05-context-retrieval/hook-triggering.md), [0010 calibrated priors](0010-calibrated-priors-not-laws.md)

## Context

anamnesis runs a **local embedding model** (bge-base) over an **in-memory graph cache** on top of SQLite. It exposes memory through two surfaces that look unrelated but must share one engine state:

- **MCP tools** the agent calls deliberately for recall, ingestion,
  relationships, inspection, and memory lifecycle management.
- **Client-side hooks** (Claude Code / Codex `SessionStart` + `UserPromptSubmit`) that inject a proactive recall before the model answers ([ADR-0011](0011-activation-gated-triggering.md)).

Two questions this ADR settles: (1) how do these surfaces reach the engine, and (2) what is their relationship to each other — is the hook "an MCP thing", should the plugin embed the MCP server, may either open the DB directly?

## Decision

Adopt a **three-layer split** where one daemon is the core and the other two surfaces are *clients* of it:

1. **daemon — the core.** `anamnesis daemon` owns the DB + in-memory graph + the embedding model (loaded once). It is **on-demand** (the first client auto-spawns it), **ref-counted** (grace-exits when idle, `ANAMNESIS_DAEMON_GRACE_SECS`), and serves N clients over a **per-DB unix socket**. In shared plugin operation it is the sole database opener; explicit embedded commands are an exclusive alternative and cannot run beside that owner.
2. **MCP adapter — a daemon client.** `anamnesis serve` is the rmcp adapter the agent's MCP client speaks to over stdio; it translates each tool call into a bespoke daemon request. It carries deliberate reads and writes. A successful explicit `recall` is the use signal when server reinforcement is enabled; mutation tools remain explicit operations.
3. **Hook runner — a daemon client.** `anamnesis hook <event>` reads hook JSON, performs gated read-only recall or best-effort capture over the socket, and emits the host hook response. It carries proactive behavior rather than an MCP tool call.

**The MCP adapter and hook runner are distinct clients of the same daemon.**
The daemon socket uses a bespoke newline-delimited request/response protocol;
rmcp is confined to the `serve` adapter. Hook and one-shot CLI processes speak
the daemon protocol directly. The distributed plugin registers both the MCP
launcher and hook manifests for one-step installation, but that packaging does
not merge their runtime transports or mutation contracts. Neither plugin
client opens the database directly, and there is no `--embedded` hook mode.

## Why this shape

1. **Local model reuse and single-writer ownership require a shared process.** A local embedding model + in-memory SoA graph means each fresh process would reload the model and contend for the single-writer lock. `tokio` async serializes concurrency *within* one process but cannot coordinate separate OS processes. A warm, single-owner daemon provides bounded recall latency and lock safety.
2. **MCP and hooks are complementary, not redundant ([ADR-0011](0011-activation-gated-triggering.md)).** MCP alone cannot drive *proactive* recall because it is pull-based. Hooks inject eligible context but remain read-only and cannot express the deliberate use or mutation signaled by explicit MCP calls. Both stay as separate clients.
3. **Bundle installation, preserve runtime separation.** The plugin manifest
   registers the MCP launcher and hooks together. `serve` still owns the rmcp
   session while `hook` remains a short-lived daemon-protocol client, so one
   installation does not create a second retrieval or write path.
4. **One state owner keeps behavior coherent.** Formation admission, retrieval,
   telemetry, and commit must observe one graph generation and one database
   lock. A shared daemon prevents hooks, MCP, and CLI clients from opening
   competing caches or applying different policy versions.

## Consequences

- The daemon remains required while the local-model + in-memory-graph design and
  single-writer ownership contract hold.
- The hook path stays a daemon client (no embedded mode) for warm-model + lock safety; both clients must tolerate the daemon being absent or restarting (the hook is fail-open; `ensure_daemon` respawns a detached daemon on connect).
- The daemon's socket protocol is the bespoke `proto` (newline-delimited request→response); rmcp/MCP lives **only** in the `serve` adapter (`server.rs` + the `serve` entrypoint). This keeps the hook, the CLI, and the daemon itself MCP-free. Daemon-backed and `--embedded` serving share the same dispatch semantics; each client still owns its transport-specific envelope and proactive-hook policy.
- Runtime integrations respect this split: deliberate agent capabilities use the MCP
  adapter, proactive behavior uses hooks, and shared plugin state reaches the
  engine through the daemon rather than a second database opener.

## References

- [ADR-0011 activation-gated triggering](0011-activation-gated-triggering.md) and [hook-triggering](../05-context-retrieval/hook-triggering.md) — lifecycle and transport complementarity.
- [Operations](../06-operations/operations.md) — daemon lifecycle, lock
  ownership, version skew, and fail-open client behavior.
