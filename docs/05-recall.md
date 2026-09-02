# 05 — Recall

recall is read-only, LLM-free and deterministic. The same graph state and the
same `(query, T, now)` produce the same result.

```text
  recall(query, session?, T = now, k = 10)
    │
    ├─ ① pin         recall_id · T · now · R0 = structure_revision · active[*]
    │
    ├─ ② candidates  vector(64) ── BM25(64) ── session(32) ── identity(anchors + profile 16)
    │                all with visible(T) and visible_gen in the WHERE clause
    │
    ├─ ③ seeds       channel RRF → hub damping → normalize → top 128
    │
    ├─ ④ envelope    one Neo4j tx, ≤ 2,000 nodes / 20,000 links       ┐
    ├─ ⑤ PPR         TypeScript, CSR, α = 0.85                        ┘ docs/06
    │
    ├─ ⑥ fusion      RRF( vector, BM25, session, identity, PPR ) = relevance
    │
    ├─ ⑦ assembly    exact m(now) · score = relevance · max(m, ε)^γ · valid(T) · provenance
    │
    ├─ ⑧ consistency R1 = structure_revision. R0 ≠ R1 → retry once, still different → torn
    │
    └─ ⑨ response    results[k] · entities · diagnostics
                     (auto mode) → asynchronous exposure Hits
```

## 1. Pin

The following are fixed at the start of the request and never change.

| Value | Meaning |
|---|---|
| `recall_id` | UUIDv7. Key for commit, Hits and logs |
| `T` | Snapshot time. Defaults to now. For questions about the past ("what was I doing in 2023") the caller supplies it |
| `now` | Server ms. Reference for forgetting (docs/04 §4) |
| `R0` | `structure_revision` at start |
| `active[*]` | The three stream selectors. A switch mid-request is caught by §7 |
| query embedding | One call to the embedding service. On failure the vector channel is absent |

## 2. Candidate channels

| Channel | Source | Size | Ranking |
|---|---|---|---|
| `vector` | `vec_<active>` HNSW over Fact ∪ Episode (content) — `queryNodes`; plus RELATES_TO (content, v0.2) — `queryRelationships`, each hit contributing **both endpoints** with the relationship's score | 64 nodes + 16 relationships (≤ 32 endpoints) | cosine DESC, id ASC |
| `bm25` | fulltext `element_content` (cjk) | 64 | Lucene score DESC, id ASC |
| `session` | the last 32 Episodes of `origin_session` plus the most recent 64 Facts DERIVED_FROM them | ≤ 32 + 64 | recency (time_utc DESC, id ASC) |
| `identity` | user and agent Entity anchors + profile cache (dreaming, top-16 Facts) | ≤ 18 | cache order |

Candidate total ≤ 96 + 64 + 96 + 18 = **274**. Every later stage is bounded
by this number plus the envelope (§6).

- Channel queries put **`visible(T) ∧ visible_gen` in the WHERE clause**.
  Filtering after LIMIT leaves the candidate set empty for past T. HNSW
  over-fetches to satisfy the filter: `k_fetch = 4 × k`, and if fewer than k
  survive, we proceed with what we have.
- `valid(T)` is not applied here (docs/03 §7). Invalid Facts still conduct.
- The session channel is what catches "the thing I mentioned a moment ago".
  The identity channel is a weak bias that lets spreading start around the
  self regardless of the query.

## 3. Seeds

Seed mass of candidate x:

```text
  raw(x)  = Σ_c  w_c / (k_rrf + rank_c(x))         k_rrf = 60
            w_vector = 0.35, w_bm25 = 0.35, w_session = 0.2, w_identity = 0.1
            weights of absent channels are redistributed proportionally to the rest
  hub(x)  = 1 / log₂(2 + deg(x))                   deg = conducting-role degree (Neo4j COUNT{})
  seed(x) = raw(x) · hub(x) / Σ raw · hub          Σ seed = 1
```

Only the top **128** by `seed DESC, id ASC` become the PPR personalization
vector s. The remaining candidates do not seed but stay in the channel lists
used in ⑥.

Note that these seed weights are a different set from the fusion weights in §6
— seeding decides where spreading starts; fusion decides what is returned.

Why hub damping: anchors such as "me" or "the company" appear in every channel
and have huge degree; without damping, PPR mass disperses over their thousands
of neighbors and query specificity is lost.

## 4–5. Envelope and PPR

[06-envelope-ppr](06-envelope-ppr.md). Only the contract needed here:

- Input: 128 seeds, T, active[*]. Output: p over the envelope nodes (Σ p ≤ 1),
  or absent.
- If the envelope transaction does not finish within **100 ms** (config), the
  PPR channel is **dropped entirely.** Running PPR on a partial envelope
  biases results silently and is not reproducible.
- PPR computation itself has no deadline — it is bounded in size and takes a
  few ms, and a deadline would make the same input produce different output.

## 6. Fusion and assembly

### relevance — RRF

```text
  relevance(x) = Σ_L  w_L / (60 + rank_L(x))
                 L ∈ { vector .25, bm25 .25, ppr .30, session .15, identity .05 }
                 weights of absent lists are redistributed proportionally to the rest
  ppr list     = envelope nodes by p DESC, id ASC (only the top 256 are ranked)
```

RRF ignores the scale of channel scores and uses ranks only, because cosine,
Lucene and PPR mass have no common unit. All `w` are calibration targets.

### Mass and score

For the candidate union (all lists ∪ envelope top 256; ≤ 274 + 256 = **530**
elements), the source Episodes' cache `(s, t_last_hit)` is read **in one
Cypher batch** (≤ 530 × 3 sources, bounded by the DERIVED_FROM depth) and
exact `m(now)` is computed (docs/04 §3). Then

```text
  score(x) = relevance(x) · max(m(x), ε)^γ            γ = 0.5, ε = 0.02
```

### Filters and ordering

1. **Kind**: results are Facts and Episodes. Entities and Communities are not
   results; they are returned separately in the `entities` block (anchors,
   top 8).
2. **valid(T)**: invalid Facts leave the results. If the Fact that invalidated
   one is in the results, the invalidated Fact is attached under
   `provenance.supersedes`.
3. **Ordering**: `score DESC, relevance DESC, mass DESC, id ASC`. Top k.

### Result item

```jsonc
{
  "id": "…", "kind": "Fact", "schema": "anamnesis.claim/1", "sub_kind": "preference",
  "content": "…", "time": {"utc": 1700000000000, "precision": "day"},
  "score": 0.041, "relevance": 0.052, "mass": 0.62,
  "sources": ["<episode-id>", "…"],                // for Hit attribution. commit uses this as is
  "provenance": {
    "derived_from": [{"id": "…", "kind": "Episode", "visible_at_T": true}],
    "supersedes":   [{"id": "…", "content": "…", "mode": "correction"}],
    "contrasts":    ["<fact-id>"]
  },
  "channels": ["vector", "ppr"]                     // which lists it appeared in
}
```

`provenance.derived_from` is always filled, under the snapshot exception
(docs/03 §3). `contrasts` lists only valid Facts that are also in the results
— contradictions are not hidden, they are shown side by side.

## 7. Consistency — structure_revision

```text
  read R0 → ②③④ → read R1
    R0 == R1  → proceed
    R0 != R1  → retry once from ② (R0 := R1)
                  different again → proceed, diagnostics.torn = true
```

- torn means the candidates and the envelope may have seen different
  revisions. It does not mean the result is wrong; it means reproducibility is
  not guaranteed.
- Hits, cache writes and embedding backfill do not bump the revision
  (docs/02 §1), so a burst of recalls does not make itself torn.
- Mass is read **once**, in ⑦. If a commit lands mid-assembly and changes s,
  this response finishes with the value it read.

## 8. Degradation ladder

Top to bottom; nothing degrades into a silent partial result — whole
**channels** drop out.

| Situation | Behavior | diagnostics |
|---|---|---|
| Embedding service failure | vector channel absent. Seeds from bm25, session, identity | `channels_used` lacks vector |
| Fulltext error | bm25 absent | 〃 |
| Both vector and bm25 absent | seeds from session and identity only → PPR still runs | 〃 |
| Zero candidates | no envelope, empty result | `reason: no_candidates` |
| Envelope tx > 100 ms | PPR list absent. Channel RRF only | `ppr_used: false, ppr_reason: envelope_timeout` |
| Envelope limit truncation | normal (truncation is defined behavior) | `envelope: {nodes, links, truncated_links}` |
| Revision mismatch twice | proceed | `torn: true` |
| Neo4j cold start | wait ≤ warmup_wait (20 s), then empty success | `reason: neo4j_unavailable` |
| Neo4j down | empty success | 〃 |

An empty success is `{results: [], entities: [], diagnostics}`. The only
thing that throws is a contract violation (schema error).

## 9. Diagnostics and budget

```jsonc
"diagnostics": {
  "recall_id": "…", "T": 1725000000000, "now": 1725000000123,
  "structure_revision": 4021, "torn": false,
  "channels_used": ["vector", "bm25", "session", "identity"],
  "ppr_used": true, "seeds": 97,
  "envelope": {"nodes": 1412, "links": 9930, "hops": 2, "truncated_links": 0, "hubs_expanded": 2},
  "ppr": {"iterations": 23, "residual": 8.1e-5},
  "timings_ms": {"embed": 11, "candidates": 9, "envelope": 31, "ppr": 3, "assemble": 6, "total": 62}
}
```

Target: **p50 < 100 ms, p95 < 250 ms** (including embedding, 1M-element
graph, local M-series). Per-stage budgets are set from measurements; the only
hard deadline is the 100 ms envelope tx.

## 10. Exposure in auto mode

The recall request handler itself writes nothing. For a `commit_mode = auto`
client, **after the response is on the socket**, the daemon calls the commit
path with `exposure` (κ 0.15) for the top 3 results
(`commitHits(recall_id, exposure, …)`, docs/04 §6). This is not part of the
response latency, and a failure does not affect the recall result. It is not
done for receipt clients — an adoption report will follow.

## 11. Determinism

The same `(graph state, query embedding, T, now, session)` gives the same
result. Guaranteed by:

- every list's tie-break is stated as `id ASC` (docs/06 §7)
- envelope truncation order is defined
- PPR converges with a fixed iteration cap and fixed residual, no deadline
- mass is computed from values read once

Since `now` differs on every request, no two requests are strictly identical —
tests inject `now`.
