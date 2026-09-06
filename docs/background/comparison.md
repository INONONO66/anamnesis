# Comparison with Existing Memory Engines

> Non-normative background (written 2026-08, anamnesis column corrected
> 2026-09 to match the finalized design). The normative design is
> [docs/00–10](../00-overview.md); where this document's description of
> anamnesis differs from it, the normative docs win. Claims about other
> engines are as of the 2026-08 reading of their papers and repos. Nothing
> here is a measured comparison; anamnesis has no benchmark result yet.

Surveyed: Zep/Graphiti, Mem0, Supermemory, Letta (MemGPT), HippoRAG, A-MEM,
MemOS, Cognee, Memobase. Sources at the end.

## By axis

| Axis | **anamnesis** | Zep/Graphiti | Mem0 | Supermemory | Letta | HippoRAG | A-MEM | Cognee | Memobase |
|---|---|---|---|---|---|---|---|---|---|
| Original preservation | immutable, CREATE-only layer | Episode nodes (same DB) | **discarded** | Document layer (cloud) | conversation history | passages kept | notes only | relational store | blob |
| Derived rebuild | **drop everything → rebuild** | no | no | no | no | index only | no | partial | partial |
| Time model | single event-time axis + INVALIDATES; no transaction time (D4) | bi-temporal, 4 timestamps | createdAt only | dual in the paper, absent in code | none | none | creation time | ingest-centric | event timeline |
| Contradictions | invalidation events (immutable) | edge invalidation | **UPDATE/DELETE (destroys history)** | updates link + isLatest | agent's discretion | none | note edits (destroys history) | none | profile overwrite |
| Forgetting | accessibility evaluated at read time; policy suppresses, never erases (D43) | none | delete only | forgetAfter + cron | none | none | none | none | profile refresh |
| Relationship model | natural-language links + 7 roles (graphiti vocabulary) | ontology fact edges + communities | optional triplets | 3 fact-on-fact kinds | none | schemaless triplets | semantic links + tags | ontology graph | none |
| Recall | vec + FTS + seeds → PPR → weighted fusion | vec + BM25 + BFS → 5 rerankers | vector top-k | fact search → re-inject originals | tool call | PPR (the original) | similarity | vec + Cypher | profile injection |
| Deployment | npm CLI + local daemon + local Neo4j container (Docker required) | server + Neo4j | server / SaaS | cloud SaaS | server + Postgres | research code | research code | server + 3 DBs | server + Postgres |
| Data sovereignty | all local under `~/.anamnesis/`; journaled backup/restore (D38) | heavy self-hosting | SaaS-centric | none | self-hosted | local | local | self-hosted | self-hosted |

## Per engine

**Zep/Graphiti — the closest relative.** Three tiers (Episode → Entity →
Community), event-time centric, lossless invalidation, synthesized recall —
the philosophy is nearly identical. Differences: (1) Zep keeps originals in
the same DB as the derived graph; the design intent here is to separate the
layers by write discipline so the derived graph is rebuildable. Whether
graphiti can rebuild in practice wasn't tested. (2) Zep is bitemporal;
anamnesis deliberately isn't (D4). Invalidation-as-event covers "what was
true when" with one time axis, and gives up "what did the system believe
when" beyond `ingested_at` and generation numbers. That's a trade, not a
superset. (3) The 600k+ tokens per conversation figure comes from one
field report on a graphiti fork (see graphiti-lessons.md §3), not from a
controlled measurement; anamnesis avoids per-node summary caching on the
write path and defers synthesis to dreaming.

**Mem0 — cautionary tale and evidence.** Its result that natural-language
facts beat graph triplets (LOCOMO) and the efficiency of minimal storage (~7k
tokens per conversation, p50 148 ms) are taken as design input. Discarding
originals and destroying history with UPDATE/DELETE is the reason our design
exists: Mem0 cannot answer "until when was that true" and cannot
retroactively apply pipeline improvements. Note that anamnesis also has a
"forget" operation, but it's suppression by policy (D43): originals stay,
ordinary extraction and serving stop. Mem0's delete is erasure; ours is not,
and we don't claim GDPR-style erasure.

**Supermemory — convergent data model, opposite form.** Two tiers
(originals/facts), fact-on-fact links (updates/extends/derives), minimal
relation types, profile cache — our INVALIDATES, time-limited memory and
profile materialization were validated here. Its large lead over Zep on
LongMemEval (multi-session 71.4 % vs 57.9 %, temporal 76.7 % vs 62.4 %) is
evidence for the "minimal structure + natural language" line, though it's
their number on their harness. Differences: cloud black box vs a local Neo4j
database plus content-addressed `objects/`, inspectable and backed up as one
journaled unit; cron-marked forgetting vs read-time evaluation.

**Letta (MemGPT) — a consumer, not a competitor.** An interface layer where
the agent manages its own memory via tool calls; the storage layer is
ordinary. A Letta-style agent would be an external harness attaching over the
UDS RPC contract (D39); the repo ships no MCP or agent integration itself.

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

## Where anamnesis is positioned

These are design targets, not measured advantages. None of the surveyed
engines was tested against them.

1. **Full rebuildability as a stated invariant.** Originals and derived
   layers are separated by write discipline (docs/00 invariant 2). We didn't
   find another engine that documents "delete all derived data and start
   over" as a supported operation; that's a reading of their docs, not a
   test.
2. **snapshot(T) as a first-class recall parameter with retroactive
   backfill.** It depends on the originals/derived separation and on a
   single event-time axis. Zep's bitemporal model answers a different, wider
   question at a cost we chose not to pay (D4).
3. **A local daemon with a local store.** The research code (HippoRAG,
   A-MEM) isn't a product, and the products (Zep, Mem0, Supermemory) are
   server or SaaS. anamnesis needs a Neo4j container on the same machine, so
   "serverless" would overstate it; "localhost only, no account, no remote
   service" is the accurate cell.
4. **Accessibility separate from utility, and forgetting that isn't
   deletion.** `S` and `U` are separate quantities (D45); policy suppresses
   rather than erases (D43). Whether that produces better recall is exactly
   what the v0.3 calibration is for.

## Accepted trade-offs

- Multi-user, team sharing and cross-device sync are out of scope (the
  append-only originals layer keeps a later merge-based extension possible).
- No benchmark yet. Running the LongMemEval harness ourselves and comparing
  on the same table with Zep (58–62 %) and Supermemory (71–77 %) is a
  candidate milestone after v0.3; it isn't in [docs/09](../09-roadmap.md)
  and no result is claimed.

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
