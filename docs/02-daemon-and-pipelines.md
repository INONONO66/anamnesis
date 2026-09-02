# 02 — Daemon and Pipelines

Exactly one process writes. `anamnesisd` is Neo4j's only bolt client, and
every caller (CLI, MCP server, editor hooks) talks to the daemon over a UDS.

```text
  claude-code hook ─┐
  MCP server ───────┼─ UDS ~/.anamnesis/sock ─▶ anamnesisd ─ bolt(127.0.0.1) ─▶ Neo4j (Docker)
  anamnesis CLI ────┘                             │
                                                  ├─ objects/  spool/
                                                  ├─ write queue (serialized)
                                                  ├─ read pool  (recall, concurrent)
                                                  ├─ extraction worker (Outbox consumer)
                                                  ├─ maintenance (m_cache, hub shortlist — v0.2)
                                                  └─ dreaming (community, synthesis, profile — v0.3)
```

If the daemon is not running, the CLI starts it (`anamnesis up`: container →
schema → daemon). The daemon does not own the Neo4j container's lifecycle —
the CLI manages the container through compose; the daemon only observes
connection state. The daemon holds `~/.anamnesis/daemon.lock` (flock) for its
whole life; a second instance on the same data directory exits immediately.

## 1. Write serialization

Every write inside the daemon goes through one queue. Neo4j tolerates
concurrent write transactions; what we want is the guarantee that "writes have
a global order". That order is `structure_revision`.

### structure_revision

A single integer on `(:Meta {key: 'meta', structure_revision})`.
**Incremented on:**

- Element or Link CREATE
- generation selector switch (`active_*` SET)
- `gen_to` stamp
- `gc --derived` DELETE

**Not incremented on:** Hit CREATE, hit-cache / `m_cache` / shortlist SET,
embedding backfill, Outbox cursor. These do not change the *structure* recall
sees, so recall's consistency check must not trip on them. The increment
happens in the same transaction as the write that caused it.

recall reads the revision twice to detect structural change
([05-recall](05-recall.md) §7). On a mismatch it retries once; if it changes
again it proceeds with `diagnostics.torn = true` — recall never blocks writes.

## 2. RPC

JSON-RPC 2.0 over UDS. The contract is defined with zod in `packages/protocol`
and the server and clients share the same schema.

| Method | Writes | Meaning |
|---|---|---|
| `hello {client, commit_mode}` | — | Session start. `commit_mode: auto \| receipt` (docs/04 §6) |
| `remember {episode, payload?}` | yes | Ingest one Episode. Idempotent |
| `recall {query, session?, T?, k?}` | — | Read-only (docs/05) |
| `commit {recall_id, adopted[]}` | Hit | Adoption report from a receipt-mode client (docs/04 §6) |
| `status` | — | Neo4j connection, revision, active selectors, spool length, Outbox backlog |
| `verify {scope}` | — | Digests, Payloads, orphan Facts, ledger ↔ cache agreement |
| `gen {stream, action: open\|switch\|rollback}` | selector | Generation operations (docs/01 §4) |
| `maintain` | caches | Run the maintenance job now (§6) |
| `dream {phase?}` | derived, Hits | Run dreaming now (§7) |
| `gc {derived\|embedding\|objects, …}` | DELETE | Explicit cleanup (docs/01 §4, §9) |

Every response carries `structure_revision` and `server_time`. Request frames
are capped at 1 MiB and payload bytes at 64 MiB (config); larger requests are
rejected before parsing (§10).

## 3. Hot path — remember

```text
  remember(episode, payload?)
    1. Contract validation (schema registry, origin, time, size caps)
       origin_key   = sha256(source, session, actor, record)
       digest       = sha256(content)
       revision_key = sha256(origin_key, digest)
    2. If payload: write-once into objects/ (temp name → fsync → rename → fsync dir)
    3. One Neo4j transaction
         a. MATCH (:Episode {revision_key})
              exists                          → no-op, return existing id, created = false
         b. MATCH latest (:Episode {origin_key}) by ingested_at
              none                            → CREATE Episode
              exists (a different revision)   → CREATE new Episode, new -[:INVALIDATES]-> latest  (revision chain)
         c. MERGE Payload metadata, HAS_PAYLOAD
         d. NEXT_EPISODE from the previous Episode of the same origin_session
         e. Initialize hit cache: s = S0(m0), t_last_hit = server_time, hit_count = 0   (docs/04 §2)
         f. CREATE Outbox {episode_id, stage: extract}
         g. structure_revision += 1
    4. Return {id, created: bool, structure_revision}
```

`origin_key` identifies the logical source and is shared by all revisions;
`revision_key` identifies one revision and is unique (docs/01 §1). The
uniqueness constraint is on `revision_key`, so a revision never violates it.

remember calls no LLM. It does not embed either — ingestion must succeed even
when the embedding service is down. Extraction and embedding are driven by the
Outbox.

### Revisions vs corrections

A different digest arriving under the same `origin_key` is a *revision*
(document edit, message edit) and is expressed as INVALIDATES between
Episodes. This is distinct from a user's *correction utterance*
(`anamnesis.correction/1`) — that is a new Episode with a new origin_key, and
the correction's meaning appears at extraction time as INVALIDATES between
Facts ([03-time](03-time.md) §5).

## 4. Spool — when Neo4j is unavailable

If the Neo4j container is down or still starting, remember does not fail.

```text
  ~/.anamnesis/spool/<yyyymmdd>.jsonl     append-only, one line = one remember request (payload by hash)
  ~/.anamnesis/spool/<yyyymmdd>.done      offset of the last drained line
```

- The objects/ write is independent of Neo4j and proceeds as usual.
- The line is appended and **fsynced before the ack**. Response:
  `{spooled: true, spool_seq}`. There is no id yet and nothing is "created" —
  callers find the Episode later by origin_key.
- When Neo4j is back, drain: execute §3 step 3 for each line in file order,
  line order. For each line the transaction commits first, then the `.done`
  offset is advanced and fsynced. A crash between the two replays the line,
  and `revision_key` makes the replay a no-op.
- Spool files are deleted after every line is done and `verify` has confirmed
  the drained Episodes (default 7 days later). The spool is a queue, not part
  of the authority (docs/01 §9).
- recall does not read the spool. While Neo4j is unavailable, recall waits for
  warmup and then returns an empty success ([05-recall](05-recall.md) §8).

## 5. Cold path — extraction worker

Consumes Outbox entries with `stage: extract`. Per Episode, order-independent,
safe to re-run. LLM calls cannot sit inside a transaction, so extraction is a
**read transaction → LLM → write transaction with re-validation**.

```text
  extract(episode)
    ── read tx ──────────────────────────────────────────────────────────────
    R1. g = active[extraction], rev = structure_revision
    R2. Entity candidates: fulltext(name) ∪ vector(description) ∪ recent MENTIONS in the session   (≤ 64)
    R3. Fact candidates: valid Facts on those Entities                                              (≤ 128)
        record for each: id, gen_to (null), and whether it is currently valid
    ── LLM (outside any tx) ─────────────────────────────────────────────────
    L1. Extraction → claims[] {content, sub_kind, time_hint, entities[], mode?}
    L2. Time resolution (docs/03 §2): explicit > relative (against Episode time) > inherit
    L3. Entity judge: match an existing candidate / create new
    L4. Claim judge against Fact candidates:
          new                        → Fact + DERIVED_FROM Episode + MENTIONS
          duplicate of F             → no Fact. re_mention Hit on sources(F)
          elaboration of F           → Fact + RELATES_TO F
          contradiction, resolved    → Fact + INVALIDATES F   (mode: change | correction, docs/03 §5)
          contradiction, unresolved  → Fact + CONTRASTS F
    ── write tx ─────────────────────────────────────────────────────────────
    W1. Re-validate: active[extraction] == g, and every referenced candidate Fact/Entity still has
        gen_to IS NULL and (for duplicate/elaboration/contradiction targets) is still valid
          any check fails → abort tx, re-queue the Outbox entry (bounded: 3 attempts, then DLQ mark)
    W2. CREATE Facts (gen_from = g, idem_key) and links (idem_key). Collisions are no-ops
    W3. re_mention Hits through the commit path (docs/04 §6), namespace extract:<episode_id>
    W4. CREATE Outbox {fact_ids, stage: embed}; mark self processed
    W5. structure_revision += 1
```

`structure_revision` may legitimately change between R1 and W1 (other
Episodes being extracted); only the checks in W1 matter. A retired or
invalidated candidate means the judge's premises changed — re-running the
judge is cheaper than writing a wrong INVALIDATES.

The embed stage is a separate Outbox entry that SETs the
`embedding_<active>` property. If the embedding service is unavailable the
entry stays and is retried — meanwhile the Fact is unreachable through the
vector channel and is reached through BM25 and PPR only.

### Idempotency and the DLQ

Because `idem_key` on Facts and links includes `gen_from`, extracting the same
Episode twice in one generation makes every second write collide → no-op,
while a new generation creates its own output (docs/01 §4). An entry that fails
W1 three times, or whose LLM call fails N times, is marked `dlq` on the Outbox
node (a cursor, not an element) and reported by `status`; `anamnesis outbox
retry` re-queues it.

## 6. Maintenance (v0.2)

A scheduled job (default hourly) with no LLM and no GDS. It exists so that the
envelope has what it needs from the first version that runs PPR.

```text
  m_cache        compute m(now) for every Element → SET Element.m_cache
                 · used to order envelope fanout (docs/06 §2). An hour of staleness is fine
  hub shortlist  for each Entity with deg ≥ HUB_DEGREE (256): top-32 neighbors by m_cache, deterministic order
                 · SET Entity.shortlist = [id…]   (docs/06 §3)
```

Both are cache-layer SETs and do not bump `structure_revision`. `maintain`
runs it on demand.

## 7. Dreaming (v0.3)

Periodic (default: nightly) or via the `dream` RPC. The only process that
looks at global structure, and the place where GDS is used if at all.

```text
  phase 1  community      Leiden (GDS) on the conducting graph → new community generation
                          · nodes: visible Entity and Fact; edges: MENTIONS, RELATES_TO
                          · one Community node + HAS_MEMBER per community (gen_from = new g_c)
                          · summary content by LLM; on failure content = member names joined
                          · sample validation (member-count distribution, Jaccard vs previous) → switch active[community]
  phase 2  synthesis      bundles of Facts within one Community → anamnesis.synthesis/1 Fact
                          · many DERIVED_FROM. Appended to the current extraction generation g
                          · promotion Hits on the sources of the member Facts, through the commit path,
                            namespace dream:<synthesis_fact_id>   (docs/04 §5–6)
  phase 3  profile        cache the top Facts around identity anchors (the user, the assistant) (docs/05 §2)
```

Dreaming never touches the originals layer except through the commit path
(promotion Hits). On failure it discards partial results (per transaction) and
tries again next cycle.

## 8. What is not on the recall path

- No LLM calls. Candidates, seeds, PPR, RRF and assembly are deterministic
  numeric work.
- No writes inside the request handler. The auto-mode exposure Hit is a
  separate step after the response has been sent, through the commit path
  (docs/05 §10).
- No GDS calls.

## 9. Connection states and degradation

| State | remember | recall | extraction | maintenance / dreaming |
|---|---|---|---|---|
| Neo4j up, embed up | normal | normal | normal | normal |
| Neo4j up, embed down | normal | vector channel dropped (`channels_used`) | embed stage backs up | dreaming phase 2 skipped |
| Neo4j cold start (≤ warmup_wait 20 s) | spool | wait, then empty success `neo4j_unavailable` | paused | paused |
| Neo4j down | spool | empty success `neo4j_unavailable` | paused | paused |
| LLM down | normal | normal | backs up (retry) | maintenance normal; dreaming phase 1 summary fallback, phase 2 skipped |

An "empty success" is `{results: [], diagnostics: {reason}}`, not an
exception — host hooks must keep exiting 0.

## 10. Security boundary

Personal, single-user, localhost. The boundary is the OS user.

| Control | Rule |
|---|---|
| data directory | `~/.anamnesis/` mode 0700; files 0600 |
| UDS | `sock` mode 0600. On accept, the peer's UID is read (`SO_PEERCRED` on Linux, `getpeereid` on macOS) and must equal the daemon's UID |
| request caps | RPC frame ≤ 1 MiB, payload ≤ 64 MiB, `remember` batch ≤ 256 per frame. Rejected before parsing |
| Neo4j bind | container publishes `127.0.0.1:7687` only; no HTTP port published |
| Neo4j auth | password generated per install (32 random bytes, base64) into `neo4j.auth` (0600) and injected into compose. No default password anywhere |
| Neo4j plugins | GDS only under `--profile gds`; the default profile mounts no plugins |
| daemon singleton | `daemon.lock` via flock; the CLI connects to the existing daemon instead of starting another |
| LLM / embedding egress | only the configured endpoints; content sent to them is the Episode content being extracted, nothing else. Off = extraction backs up, nothing else changes |

## 11. Transaction boundaries

| Operation | Transactions | revision |
|---|---|---|
| remember | 1 (Episode, Payload, revision INVALIDATES, NEXT, cache init, Outbox) | +1 |
| extract (one Episode) | read tx + write tx (re-validated) | +1 |
| embed backfill | 1 per batch | — |
| commit (Hit) | 1 (Hit CREATE + cache SET, once per source Episode) | — |
| gen switch / rollback | 1 (Meta SET) | +1 |
| maintenance | 1 per batch (cache SET) | — |
| dreaming phase | ≥ 1 per phase, phase discarded as a unit on failure | phases 1–2: +1, 3: — |
| gc | 1 per batch | +1 for derived/embedding (structural); — for objects |
