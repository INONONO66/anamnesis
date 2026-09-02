# Comparison with Existing Memory Engines

> Non-normative background (written 2026-08). The normative design is
> [docs/00–10](../00-overview.md); where this document's description of
> anamnesis differs from it, the normative docs win.

Surveyed: Zep/Graphiti, Mem0, Supermemory, Letta (MemGPT), HippoRAG, A-MEM,
MemOS, Cognee, Memobase. Sources at the end.

## By axis

| Axis | **anamnesis** | Zep/Graphiti | Mem0 | Supermemory | Letta | HippoRAG | A-MEM | Cognee | Memobase |
|---|---|---|---|---|---|---|---|---|---|
| Original preservation | immutable, CREATE-only layer | Episode nodes (same DB) | **discarded** | Document layer (cloud) | conversation history | passages kept | notes only | relational store | blob |
| Derived rebuild | **drop everything → rebuild** | no | no | no | no | index only | no | partial | partial |
| Time model | single event-time axis + INVALIDATES | bi-temporal, 4 timestamps | createdAt only | dual in the paper, absent in code | none | none | creation time | ingest-centric | event timeline |
| Contradictions | invalidation events (immutable) | edge invalidation | **UPDATE/DELETE (destroys history)** | updates link + isLatest | agent's discretion | none | note edits (destroys history) | none | profile overwrite |
| Forgetting | mass evaluated at read time | none | delete only | forgetAfter + cron | none | none | none | none | profile refresh |
| Relationship model | natural-language links + 7 roles (graphiti vocabulary) | ontology fact edges + communities | optional triplets | 3 fact-on-fact kinds | none | schemaless triplets | semantic links + tags | ontology graph | none |
| Recall | vec + FTS + seeds → PPR → weighted fusion | vec + BM25 + BFS → 5 rerankers | vector top-k | fact search → re-inject originals | tool call | PPR (the original) | similarity | vec + Cypher | profile injection |
| Deployment | **npm, local daemon, zero servers** | server + Neo4j | server / SaaS | cloud SaaS | server + Postgres | research code | research code | server + 3 DBs | server + Postgres |
| Data sovereignty | all local, copy = backup | heavy self-hosting | SaaS-centric | none | self-hosted | local | local | self-hosted | self-hosted |

## Per engine

**Zep/Graphiti — the closest relative.** Three tiers (Episode → Entity →
Community), event-time centric, lossless invalidation, synthesized recall —
the philosophy is nearly identical. Differences: (1) Zep keeps originals in
the same DB as the derived graph and cannot rebuild; we separate the layers
by write discipline. (2) Invalidation-as-event gives the expressiveness of
bi-temporal 4-timestamps with fewer concepts. (3) Neo4j server product plus
per-node summary caching leads to token explosion (600k+ observed per
conversation) — we compute summaries as views.

**Mem0 — cautionary tale and evidence.** Its result that natural-language
facts beat graph triplets (LOCOMO) and the efficiency of minimal storage (~7k
tokens per conversation, p50 148 ms) are adopted. Discarding originals and
destroying history with UPDATE/DELETE is the reason our design exists — Mem0
cannot answer "until when was that true" and cannot retroactively apply
pipeline improvements.

**Supermemory — convergent data model, opposite form.** Two tiers
(originals/facts), fact-on-fact links (updates/extends/derives), minimal
relation types, profile cache — our INVALIDATES, time-limited memory and
profile materialization were validated here. Its large lead over Zep on
LongMemEval (multi-session 71.4 % vs 57.9 %, temporal 76.7 % vs 62.4 %) is
evidence for the "minimal structure + natural language" line. Differences:
cloud black box vs our local files (readable, editable, backup-able);
cron-marked forgetting vs read-time evaluation.

**Letta (MemGPT) — a consumer, not a competitor.** An interface layer where
the agent manages its own memory via tool calls; the storage layer is
ordinary. Letta-style agents sit on top of our MCP.

**HippoRAG — source of the recall component.** PPR associative recall and
node specificity are borrowed. Research code for static corpora — no time,
no contradictions, no ingest pipeline.

**A-MEM — idea adopted, method rejected.** "New memories change the meaning
of old ones" (memory evolution) is absorbed through mass and dreaming, but
editing existing notes in place violates immutability and is rejected —
replaced by adding derived elements and recomputing.

**MemOS — attitude only.** Unifying plaintext/activation/parameter memory in
a MemCube is out of scope. We share only the stance that memory is a
first-class resource with a lifecycle.

**Cognee — same division of labor, different physics.** Relational =
originals/provenance, vector and graph = derived indexes, the same split — but
spread across three physical DBs, making deployment heavy, and its ontology
validation line runs opposite to our natural-language minimalism.

**Memobase — an operational pattern borrowed.** Its buffer → flush cold-path
batching is the prototype of our Outbox consumption.

## What only we have

1. **Full rebuildability** — originals and derived layers separated by write
   discipline. No other engine can "delete all derived data and start over".
2. **snapshot(T) as a first-class operation with clean retroactive
   backfill** — requires the originals/derived separation, which makes it
   structurally hard for the others.
3. **A serverless local resident engine** — the research code (HippoRAG,
   A-MEM) is not a product, and the products (Zep, Mem0, Supermemory) are all
   server/SaaS. That empty cell is our position.

## Accepted trade-offs

- Multi-user, team sharing and cross-device sync are out of scope (the
  append-only originals layer keeps a later merge-based extension possible).
- No benchmark yet — running the LongMemEval harness ourselves in v0.3 and
  comparing on the same table with Zep (58–62 %) and Supermemory (71–77 %) is
  a milestone.

## Sources

- Zep/Graphiti: <https://arxiv.org/html/2501.13956v1>,
  <https://github.com/getzep/graphiti>
- Mem0: <https://arxiv.org/html/2504.19413v1>
- Supermemory: <https://zebang.li/blog/supermemory-architecture-en>,
  <https://supermemory.ai/docs/concepts/how-it-works>
- MemGPT/Letta: <https://arxiv.org/abs/2310.08560>
- HippoRAG: <https://arxiv.org/abs/2405.14831>
- A-MEM: <https://arxiv.org/abs/2502.12110>
- MemOS: <https://arxiv.org/abs/2505.22101>
- Cognee: <https://docs.cognee.ai/core-concepts/architecture>
- Memobase: <https://github.com/memodb-io/memobase>
