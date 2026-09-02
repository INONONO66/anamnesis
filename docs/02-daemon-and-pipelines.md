# 02 — Daemon and Pipelines

Exactly one process writes. `anamnesisd` is Neo4j's only bolt client, and
every caller (CLI, MCP server, editor hooks) talks to the daemon over a UDS.

```text
  claude-code hook ─┐
  MCP server ───────┼─ UDS ~/.anamnesis/sock ─▶ anamnesisd ─ bolt(localhost) ─▶ Neo4j (Docker)
  anamnesis CLI ────┘                             │
                                                  ├─ objects/  spool/
                                                  ├─ write queue (serialized)
                                                  ├─ read pool  (recall, concurrent)
                                                  ├─ extraction worker (Outbox consumer)
                                                  └─ dreaming (scheduled / manual)
```

If the daemon is not running, the CLI starts it (`anamnesis up`: container →
schema → daemon). The daemon does not own the Neo4j container's lifecycle —
the CLI manages the container through compose; the daemon only observes
connection state.

## 1. Write serialization

Every write inside the daemon goes through one queue. Neo4j tolerates
concurrent write transactions; what we want is the guarantee that "writes have
a global order". That order is `structure_revision`.

### structure_revision

A single integer on `(:Meta {structure_revision})`. **Incremented on:**

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
| `commit {recall_id, adopted[], kind}` | Hit | Adoption report from a receipt-mode client (docs/04 §6) |
| `status` | — | Neo4j connection, revision, active generations, spool length, Outbox backlog |
| `verify {scope}` | — | Digests, Payloads, ledger ↔ cache agreement |
| `gen {stream, action: open\|switch\|rollback}` | selector | Generation operations (docs/01 §4) |
| `dream {phase?}` | derived, caches | Manual dreaming trigger (§6) |
| `gc {derived\|objects, …}` | DELETE | Explicit cleanup (docs/01 §4, §8) |

Every response carries `structure_revision` and `server_time`.

## 3. Hot path — remember

```text
  remember(episode, payload?)
    1. Contract validation (schema registry, origin, time)
    2. If payload: write-once into objects/ (temp name → fsync → rename)
    3. One Neo4j transaction
         a. Look up origin_key
              absent                → CREATE Episode
              present, same digest  → no-op, return existing id
              present, other digest → CREATE new Episode, new -[:INVALIDATES]-> old  (revision)
         b. MERGE Payload metadata, HAS_PAYLOAD
         c. NEXT_EPISODE from the previous Episode of the same origin_session
         d. Initialize hit cache: s = S0(m0), t_last_hit = server_time, hit_count = 0   (docs/04 §2)
         e. CREATE Outbox {episode_id, stage: extract}
         f. structure_revision += 1
    4. Return {id, created: bool, structure_revision}
```

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
```

- The objects/ write is independent of Neo4j and proceeds as usual.
- Response: `{spooled: true, spool_seq}`. There is no id yet — callers find
  the Episode later by origin_key.
- When Neo4j is back, drain: execute §3 step 3 for each line in file order,
  line order. Idempotency and revision detection happen at drain time.
  Processed lines are marked in a separate `.done` offset file; spool files
  are never deleted (they are a backup unit).
- recall does not read the spool. While Neo4j is unavailable, recall waits for
  warmup and then returns an empty success ([05-recall](05-recall.md) §8).

## 5. Cold path — extraction worker

Consumes Outbox entries with `stage: extract`. Per Episode, order-independent,
safe to re-run.

```text
  extract(episode)                          current active[extraction] = g
    1. Extraction LLM call → claims[] {content, sub_kind, time_hint, entities[], mode?}
    2. Time resolution (docs/03 §2): explicit > relative (against Episode time) > inherit Episode time
    3. Entity resolution
         · candidates: fulltext(name) ∪ vector(description) ∪ recent MENTIONS in the session
         · judge LLM: match an existing Entity / create new
         · actor mappings are recorded as anamnesis.mapping/1 Facts (the Entity is never overwritten)
    4. Judge — compare each claim with candidate Facts (valid Facts on the same Entity)
         new              → CREATE Fact(gen_from = g), DERIVED_FROM → Episode, MENTIONS
         duplicate        → no Fact. re_mention Hit on the existing Fact's source Episodes (docs/04 §5)
         elaboration      → CREATE Fact + RELATES_TO existing
         contradiction, resolved   → CREATE Fact + INVALIDATES existing   (mode: change | correction, docs/03 §5)
         contradiction, unresolved → CREATE Fact + CONTRASTS existing
    5. CREATE Outbox {fact_ids, stage: embed}; mark self processed
    6. All of the above in one transaction (LLM calls happen outside it). structure_revision += 1
```

The embed stage is a separate Outbox entry that SETs the
`embedding_<active>` property. If the embedding service is unavailable the
entry stays and is retried — meanwhile the Fact is unreachable through the
vector channel and is reached through BM25 and PPR only.

### Idempotency

Because `idem_key` on Facts and Links is unique, extracting the same Episode
twice makes every second write collide → no-op. If LLM output drifts and
produces different content, a new Fact appears — that is a re-extraction
policy question, not a determinism bug: within one generation we never
re-extract, and an extractor version change goes to a new generation
(docs/01 §4).

## 6. Dreaming

Periodic (default: nightly) or via the `dream` RPC. The only online process
that looks at global structure, and the place where GDS is used if at all.

```text
  phase 1  community      Leiden (GDS) on the conducting graph → new community generation
                          · nodes: visible Entity and Fact; edges: MENTIONS, RELATES_TO
                          · one Community node + HAS_MEMBER per community (gen_from = new g_c)
                          · summary content by LLM; on failure content = member names joined
                          · sample validation (member-count distribution, Jaccard vs previous) → switch active[community]
  phase 2  synthesis      bundles of Facts within one Community → anamnesis.synthesis/1 Fact
                          · many DERIVED_FROM. Appended to the current extraction generation g
  phase 3  hub shortlist  for each Entity with deg ≥ HUB_DEGREE (256): top-32 neighbors (docs/06 §3)
                          · ranked by neighbor m_cache × link weight, deterministic ordering
                          · SET Entity.shortlist = [id…]
  phase 4  m_cache        compute m(now) for every Element → SET Element.m_cache
                          · used to order envelope fanout. A day of staleness is fine — it prevents bias, not inaccuracy
  phase 5  profile        cache the top Facts around identity anchors (the user, the assistant) (docs/05 §2)
```

Dreaming never touches the originals layer and never creates Hits. On failure
it discards partial results (per transaction) and tries again next cycle.

## 7. What is not on the recall path

- No LLM calls. Candidates, seeds, PPR, RRF and assembly are deterministic
  numeric work.
- No writes. Hits are created only by commit (receipt) or the auto-mode
  post-processing (exposure).
- No GDS calls.

## 8. Connection states and degradation

| State | remember | recall | extraction | dreaming |
|---|---|---|---|---|
| Neo4j up, embed up | normal | normal | normal | normal |
| Neo4j up, embed down | normal | vector channel dropped (`channels_used`) | embed stage backs up | phase 2 skipped |
| Neo4j cold start (≤ warmup_wait 20 s) | spool | wait, then empty success `neo4j_unavailable` | paused | paused |
| Neo4j down | spool | empty success `neo4j_unavailable` | paused | paused |
| LLM down | normal | normal | backs up (retry) | phase 1 summary fallback, phase 2 skipped |

An "empty success" is `{results: [], diagnostics: {reason}}`, not an
exception — host hooks must keep exiting 0.

## 9. Transaction boundaries

| Operation | Transactions | revision |
|---|---|---|
| remember | 1 (Episode, Payload, NEXT, cache init, Outbox) | +1 |
| extract (one Episode) | 1 (Facts, Links, Outbox) | +1 |
| embed backfill | 1 per batch | — |
| commit (Hit) | 1 (Hit CREATE + cache SET, once per source Episode) | — |
| gen switch / rollback | 1 (Meta SET) | +1 |
| dreaming phase | ≥ 1 per phase, phase discarded as a unit on failure | phases 1–2: +1, 3–5: — |
| gc | 1 per batch | +1 (derived DELETE is a structural change) |
