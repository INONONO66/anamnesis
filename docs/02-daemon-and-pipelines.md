# 02 — Daemon and Pipelines

Exactly one process writes. `anamnesisd` is Neo4j's only bolt client, and
every caller talks to the daemon over a UDS. Harnesses — MCP bridges, editor
hooks, agent adapters — are external projects maintained by the operator;
this repo ships only the daemon, its ops CLI and the RPC contract (D39).

```text
  external harnesses ─┐  (MCP bridge, editor hook, custom agent — separate repos)
  anamnesis ops CLI ──┼─ UDS ~/.anamnesis/sock ─▶ anamnesisd ─ bolt(127.0.0.1) ─▶ Neo4j (Docker)
                      ┘                             │
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
connection state. The daemon holds a pure-Node singleton lease at
`~/.anamnesis/daemon.lock/`: atomic `mkdir`, random owner nonce and a 2 s
heartbeat. A contender exits when the heartbeat/socket is live; after 10 s of
staleness plus a failed socket connection it atomically renames the stale
directory and retries. Every writer verifies the owner nonce before opening a
transaction.

The filesystem lease is discovery, not the write fence. Before serving, a
daemon increments `Meta.writer_epoch` in a Neo4j transaction and records the
returned epoch. Every later write transaction first locks Meta and requires
that exact epoch. If an old daemon already holds that lock, takeover waits for
its transaction to resolve before incrementing; if it has not locked Meta, its
later epoch predicate fails. No transaction from the old epoch can commit
after the new daemon starts serving.

Neo4j cannot fence writes while it is down, so filesystem authority has its
own non-reusable UUID `fs_epoch`. The lease winner atomically replaces and
directory-fsyncs a fixed parent pointer
`~/.anamnesis-writer.<root_hash>.current = {fs_epoch, owner_nonce, pid}`.

- Spool paths are epoch-owned:
  `spool/<fs_epoch>/<yyyymmdd>.journal` and matching `.done`. Different
  daemons can never interleave bytes or cursors in one file.
- Every object publish and spool append reads the fixed pointer immediately
  before mutation and again before ack. Losing ownership aborts the ack.
  A publication that completed just before loss is an idempotent object; an
  unacknowledged appended record may drain but the caller retries safely.
- Each epoch journal records a contiguous `local_seq`. Drain discovers every
  epoch directory and uses a dependency-aware deterministic merge: a revision
  is ready only when its predecessor is already in Neo4j or null; records
  whose predecessor exists in any pending epoch are deferred. Among ready
  records order is `(accepted_server_time, fs_epoch bytes, local_seq,
  record_uuid bytes)`.
- A no-progress pass leaves a missing-predecessor/cycle record BLOCKED and
  does not advance its journal cursor past it; `status` names the dependency.
  Thus clock regression cannot place B before acknowledged predecessor A,
  while genuine sibling revisions still resolve through authoritative CAS.
- GC treats every undrained epoch journal as a reference source. It runs only
  after the current fs_epoch is reasserted under the cross-process object
  lease.

Restore activation atomically changes the fixed pointer to
`{revoked: operation_id}`, requests daemon shutdown, waits a bounded grace
period, then terminates the recorded PID if necessary. After services stop it
requires the PID absent, socket connect to fail, and no active fs_epoch pointer
before any root rename. Failure aborts activation and restarts the old root.

## 1. Write serialization

Every write inside the daemon goes through one queue. Neo4j tolerates
concurrent write transactions; what we want is the guarantee that "writes have
a global queue order". `ingest_seq` orders Episode creation;
`structure_revision` advances only when that order changes the serving view.

### structure_revision

A single integer on `(:Meta {key: 'meta', structure_revision})`.
**Incremented on:**

- original Element/Link or session-topology CREATE/DELETE
- Element or Link CREATE in the currently ACTIVE generation
- an ACTIVE Entity's `visible_from_utc` moves earlier
- selected model's global Episode or ACTIVE-generation embedding coverage advances
- generation selector switch (`active_*` SET)

**Not incremented on:** Hit CREATE, hit-cache / `m_cache` / shortlist SET,
hidden-generation embedding backfill, Outbox cursor,
BUILDING/CATCHING_UP generation writes, or
GC of hidden RETIRED generations. These do not change the *serving structure*
recall sees, so recall's consistency check must not trip on them. The
increment happens in the same transaction as the visible write or selector
change that caused it.

`Meta.ingest_seq` is a separate monotonic write cursor allocated in every
Episode CREATE transaction. It orders generation catch-up but is never used
as a recall consistency revision.

### Logical server time

Write transactions issue `server_time =
max(wall_clock_ms, Meta.last_server_time + 1)` and persist it on Meta. Read
requests use the maximum of wall clock, persisted time and the process-local
last issued value without writing. Forgetting still clamps every elapsed
interval to nonnegative (docs/04), so imported old Hits and wall-clock
regression cannot produce `R>1` or lower stability.

recall reads the revision twice to detect structural change
([05-recall](05-recall.md) §7). On a mismatch it retries once; if it changes
again it proceeds with `diagnostics.torn = true` — recall never blocks writes.

## 2. RPC

JSON-RPC 2.0 over UDS. Each message is `[u32be byte_length][UTF-8 JSON]`.
The daemon reads the four-byte length first and closes the connection if it
exceeds the cap, before allocating a body buffer. The contract is defined with
zod in `packages/protocol`; server and clients share the same schema.

| Method | Writes | Meaning |
|---|---|---|
| `hello {client, commit_mode, token}` | — | Authenticate the UDS session; `commit_mode: auto \| receipt` (docs/04 §6) |
| `object.begin {sha256, size, media_type}` | temp file | Start or resume a bounded payload upload; existing hash is a no-op |
| `object.chunk {upload_id, seq, bytes_b64}` | temp file | Append one raw chunk ≤ 512 KiB in sequence |
| `object.commit {upload_id}` | objects/ | Verify size/hash, fsync and atomically publish the object |
| `remember {episode, payload_hash?}` | yes | Ingest one Episode referencing an already committed object. Idempotent |
| `recall {query, session?, T?, k?}` | — | Read-only (docs/05) |
| `commit {recall_id, adopted[]}` | Hit | Adoption report from a receipt-mode client (docs/04 §6) |
| `status` | — | Neo4j connection, revision, active selectors, spool length, Outbox backlog |
| `verify {scope}` | — | Digests, Payloads, orphan Facts, ledger ↔ cache agreement |
| `gen {stream, action: build\|status\|activate\|rollback\|retire}` | selector | Lifecycle operations; activate/rollback enforce the catch-up barrier (docs/01 §4) |
| `maintain` | caches | Run the maintenance job now (§6) |
| `dream {phase?}` | derived, Hits | Run dreaming now (§7) |
| `gc {derived\|embedding\|objects, …}` | DELETE | Explicit cleanup (docs/01 §4, §9) |

Every response carries `structure_revision` and `server_time`. JSON-RPC
frames are capped at 1 MiB. Base64 upload chunks carry at most 512 KiB raw
bytes, leaving bounded framing overhead; a committed object is capped at
64 MiB (§10).

### Payload upload state machine

Only the daemon writes `objects/`. A client first uploads bytes, then calls
`remember` with the resulting content hash.

```text
  object.begin(expected_sha256, size ≤ 64 MiB, media_type)
    existing committed hash → full SHA-256 re-hash and metadata verification, then
                              require requested metadata matches, otherwise
                              object_metadata_conflict; return stored metadata
    otherwise               → {upload_id, next_seq, chunk_bytes_max: 524288}

  object.chunk(upload_id, seq, bytes_b64)
    require seq = next_seq and decoded length ≤ 512 KiB
    require cumulative bytes + decoded length ≤ declared size
    append to a 0600 temp file; update rolling SHA-256; ack next_seq

  object.commit(upload_id)
    require received size = declared size and digest = expected_sha256
    fsync data and canonical {hash,size,media_type} sidecar temps
    rename data first; rename sidecar last as the logical commit marker;
    fsync(directory)
    return {hash, size, media_type}
```

Each connection may hold at most two uploads and 128 MiB of temporary bytes;
the daemon permits at most 64 connections, 32 open uploads and 1 GiB of upload
temp globally. Incomplete uploads expire after one hour and are deleted.
`upload_id` is unguessable and bound to the authenticated UDS session. Hashes accepted
from RPC, spool or restore input must match `^[0-9a-f]{64}$` before path
construction. There is no client-side filesystem write path.
Startup charges surviving temp files to the global quota before accepting an
upload. Data without a committed sidecar is an orphan; a sidecar without
matching data is corruption. Short writes/ENOSPC roll the temp back to its
previous verified length and return `resource_exhausted` before ack.

## 3. Hot path — remember

```text
  remember(episode, payload_hash?)
    1. Contract validation (schema registry, origin, time, size caps)
       origin_key   = sha256(source, session, actor, record)
       session_key  = sha256(source, session)
       source_revision = adapter-issued stable revision token
       payload_hash = committed object hash if present
       digest       = sha256(RFC-8785 canonical JSON of
                       {schema, content, properties, time, payload_hash,
                        previous_revision_key})
       revision_key = sha256(origin_key, source_revision)
    2. If payload_hash: require the committed object to exist and match its metadata
    3. One Neo4j transaction
         a. MATCH (:Episode {revision_key})
              exists, same digest             → no-op, return existing id, created = false
              exists, different digest        → reject revision_conflict
         b. CAS (:OriginHead {origin_key})
              no head + previous=null         → CREATE first Episode
              head=previous_revision_key      → CREATE new Episode,
                                                 new -[:INVALIDATES]-> previous,
                                                 SET head=new revision_key
              otherwise                       → reject stale_revision
         c. increment Meta.ingest_seq and assign it to the new Episode
         d. MERGE Payload metadata, HAS_PAYLOAD
         e. update the rebuildable session topology around this Episode
            in total order (time_utc ASC, ingest_seq ASC)
         f. Initialize hit cache: s = S0(m0), t_last_hit = server_time, hit_count = 0   (docs/04 §2)
         g. For each distinct M in {active_embedding_model,
              target_embedding_model if set}, CREATE Outbox
              {episode_id, stage: embed_episode, target_generation: 0,
               ingest_seq, model_id: M, model_key: M}
            CREATE one extraction Outbox entry with model_key='-' for ACTIVE
            and every BUILDING/CATCHING_UP target_generation
         h. structure_revision += 1
    4. Return {id, created: bool, ingest_seq, structure_revision}
```

`origin_key` identifies the logical source and is shared by all revisions.
`source_revision` identifies an occurrence and must be reused on retry; mutable
source adapters must issue a new token even when content reverts from A→B→A.
Append-only adapters use their immutable record ID as both `record` and
`source_revision`. `revision_key` is unique, while `digest` detects an adapter
that mutates one token. `OriginHead` is a rebuildable CAS cache over the
immutable INVALIDATES chain.

remember calls no LLM. It does not embed either — ingestion must succeed even
when the embedding service is down. Extraction and embedding are driven by the
Outbox.

The v0.1 Episode embedding worker consumes the captured
`{stage: embed_episode,target_generation:0,ingest_seq,model_id,model_key}`,
sends only the Episode's normalized content to that model endpoint, SETs
`embedding_<model_id>` and advances its model-scoped contiguous cursor in a
bounded batch. Failure leaves the Outbox entry pending; BM25 and session
recall remain available. This producer ships in the same milestone as Episode
vector recall.

### Revisions vs corrections

A new `source_revision` arriving under the same `origin_key` is a *revision*
(document edit, message edit), names its `previous_revision_key`, and is
expressed as INVALIDATES between Episodes. This is distinct from a user's *correction utterance*
(`anamnesis.correction/1`) — that is a new Episode with a new origin_key, and
the correction's meaning appears at extraction time as INVALIDATES between
Facts ([03-time](03-time.md) §5).

### Session topology

`NEXT_EPISODE` is a rebuildable cache, not an original fact. Within one
`session_key = sha256(origin_source, origin_session)`, Episodes have the total order:

```text
  (time_utc ASC, ingest_seq ASC)
```

The remember transaction finds the new Episode's immediate predecessor and
successor through a composite range index. It deletes the cached
predecessor→successor edge if present, then creates predecessor→new and
new→successor. A backfilled Episode therefore inserts into the correct
event-time position without mutating any Episode. The topology mutation and
its `structure_revision` increment are atomic. `verify --scope topology`
compares every cached chain with the indexed order; `rebuild --topology`
deletes and reconstructs only these cache links.

## 4. Spool — when Neo4j is unavailable

If the Neo4j container is down or still starting, remember uses the bounded
durable spool.

```text
  ~/.anamnesis/spool/<fs_epoch>/<yyyymmdd>.journal
      [u32be length][canonical JSON][32 raw SHA-256 bytes]
  ~/.anamnesis/spool/<fs_epoch>/<yyyymmdd>.done
      same framing for {offset, record_hash} cursors
```

- The objects/ write is independent of Neo4j and proceeds as usual.
- The framed record is appended and **fsynced before the ack**. Response:
  `{spooled: true, spool_seq}`. There is no id yet and nothing is "created" —
  callers find the Episode later by `revision_key`.
- Canonical JSON includes `fs_epoch`, contiguous per-epoch `local_seq`,
  `record_uuid`, `origin_key`, `revision_key` and
  `previous_revision_key`.
- When Neo4j is back, drain the dependency-ready records in the deterministic
  cross-epoch order from §0. For each record the transaction commits first,
  then its checksummed `.done` cursor is appended and fsynced. Cursors advance
  only over the contiguous processed prefix of each journal. A crash between
  commit and cursor replays the record, and `revision_key` makes it a no-op.
- Startup truncates only an incomplete final frame. Any checksum-invalid
  complete frame quarantines the **whole journal**—even at the tail—because it
  may have been acknowledged and later boundaries are not trusted. Drain does
  not advance through it; `anamnesis spool repair` is an explicit export and
  re-import workflow. ENOSPC/short append truncates to the prior verified offset,
  fsyncs, and rejects before another append. The cursor uses the last valid
  framed entry and never guesses an offset.
- Spool files are deleted after every line is done and `verify` has confirmed
  the drained Episodes (default 7 days later). The spool is a queue, not part
  of the authority (docs/01 §9).
- The spool is capped at 1 GiB and refuses an append when free space would
  fall below 2 GiB. `remember` then returns `resource_exhausted` and the caller
  retains the request for retry. Durability cannot promise success under
  ENOSPC.
- recall does not read the spool. While Neo4j is unavailable, recall waits for
  warmup and then returns an empty success ([05-recall](05-recall.md) §8).

## 5. Cold path — extraction worker

Consumes Outbox entries with `stage: extract`. Each target generation has one
sequencer: only the entry whose `ingest_seq = Generation.next_ingest_seq` may
run, so retry timing cannot reorder Entity resolution or contradiction
decisions. LLM calls cannot sit inside a transaction.

```text
  extract(episode)
    P1. target_generation and ingest_seq come from the Outbox entry
        · normal extraction: target_generation = active[extraction]
        · rebuild/rollback: target state is BUILDING or CATCHING_UP
        · require ingest_seq = Generation.next_ingest_seq
        capture input_head = OriginHead[episode.origin_key]
    ── extraction LLM (outside a tx) ────────────────────────────────────────
    L1. Episode → at most 32 claims[] {content, sub_kind, time_hint, entities[≤16], mode?}
    L2. Time resolution: explicit > relative against Episode.time > inherit
    ── bounded read tx, all indexes scoped to target_generation ─────────────
    R1. For each extracted entity mention: synchronous fulltext top 16.
        Deduplicate by Entity id; score DESC, id ASC; keep global top 64
        (≤ 16 mentions × 16 raw rows = 256 inspected result rows)
    R2. For each claim: synchronous Fact fulltext top 64 + session top 32.
        Deduplicate by Fact id; equal-weight RRF, id ASC; keep top 128
    R3. For every candidate invalidator B, indexed lookup of at most eight
        outgoing INVALIDATES targets supplies replacement context, including
        invalid A, its authority and content
    R4. candidate_digest = sha256(canonical ordered candidate IDs, scores,
        validity bits and replacement-context edge IDs)
    ── judge LLM (outside a tx) ─────────────────────────────────────────────
    L3. Entity judge: match an existing candidate / create new with
        entity_key = sha256(target_generation, normalized_name, entity_kind)
    L4. Claim judge against Fact candidates:
          new                        → Fact + DERIVED_FROM Episode + MENTIONS
          duplicate of F             → no Fact. re_mention Hit on sources(F)
          elaboration of F           → Fact + RELATES_TO F
          contradiction, resolved    → Fact + at most 8 INVALIDATES targets
                                        (mode: change | correction, docs/03 §5)
          contradiction, unresolved  → Fact + CONTRASTS F
    ── write tx ─────────────────────────────────────────────────────────────
    W1. Re-validate:
        · normal path: active[extraction] == target_generation
        · rebuild path: target state is still BUILDING or CATCHING_UP
        · ingest_seq still equals Generation.next_ingest_seq
        · OriginHead[episode.origin_key] still equals input_head
        · rerun the bounded reads and require the exact candidate_digest
        · every referenced candidate and replacement-context edge is unchanged
        any check fails → abort tx and retry the same sequence head
    W2. For each new Fact, materialize 1–16 source Episode IDs and matching
        direct DERIVED_FROM links (docs/01 §1); then CREATE Facts and links in
        target_generation. Every derived endpoint is in that same generation;
        cross-generation extraction links are rejected. Fact identity hashes
        schema, content, properties, time, sub_kind, primary Episode, entity
        bindings, source Episodes and synthesis supports; Entity identity uses entity_key.
        Exact retry collisions are no-ops. SET each mentioned Entity's
        visible_from_utc to min(current, mention source time)
    W3. re_mention Hits through the commit path (docs/04 §6), namespace extract:<episode_id>
    W4. For each distinct M in {active_embedding_model,
        target_embedding_model if set}, CREATE Outbox
        {stage: embed_derived, target_generation, ingest_seq, fact_ids,
         model_id: M, model_key: M}; uniqueness is
        (stage,target_generation,ingest_seq,model_key). With no derived
        vectors, advance M's coverage cursor as a no-op
    W5. advance next_ingest_seq/covered_ingest_seq to this committed sequence;
        if structural output was created in the ACTIVE generation,
        structure_revision += 1. BUILDING/CATCHING_UP writes and Hit-only
        duplicate outcomes do not bump it
```

`structure_revision` may legitimately change between R1 and W1 (other
Episodes being extracted); only the checks in W1 matter. A changed source
head or candidate means the judge's premises changed — re-running is cheaper
than committing a stale relation. A historical input Episode may still be
extracted after its head changed on an earlier run: its derived Facts inherit
the bounded source authority's temporal validity (docs/03 §3–4), so they serve
historical snapshots without becoming current again.

The `embed_derived` stage SETs the named model property and advances
`EmbeddingCoverage(target_generation,model_id)` only through contiguous
ingest_seq. Global Episode embedding jobs use the same model-scoped cursor
with `stream=episode,generation=0`. If either cursor affects the selected
model's serving vector set, the same transaction increments
`structure_revision`. If the embedding service is unavailable the entry stays
and is retried — meanwhile the Fact is unreachable through the vector channel
and is reached through BM25 and PPR only.

### Idempotency and a blocked sequence head

Because Fact, Entity and link identities include `generation` and all
meaning-bearing immutable fields, extracting the same Episode twice in one
generation makes exact retries collide → no-op, while semantically distinct
output cannot silently become first-write-wins. A new generation creates its
own output (docs/01 §1, §4). Three automatic failures mark the **sequence
head** BLOCKED and pause that target; no later entry may overtake it.
`status` reports the target/ingest_seq/error, and `anamnesis outbox retry`
resumes that same head. Skipping a failed Episode requires opening a new
generation with an explicit exclusion decision; the current generation never
changes history out of order.

## 6. Maintenance (v0.2)

A scheduled job (default hourly) with no LLM and no GDS. It exists so that the
envelope has what it needs from the first version that runs PPR.

```text
  m_cache        compute m(now) for every Element → SET Element.m_cache
                 · used to order envelope fanout (docs/06 §2). An hour of staleness is fine
  hub shortlist  for each Element with deg ≥ HUB_DEGREE (256): atomically rebuild
                 at most 32 cache nodes
                 (:HubArc {hub_id, rank: 0..31, link_id, neighbor_id, role,
                            stream, generation?, source_extraction_generation?})
                 ORDER BY w_role DESC, coalesce(neighbor.m_cache, neighbor.m0) DESC,
                          neighbor.id DESC, link.id ASC
```

Both are cache-layer SETs and do not bump `structure_revision`. `maintain`
runs it on demand. Any topology rewire, relationship GC or other Link DELETE
removes `HubArc {link_id}` in the same transaction; consumers never trust an
arc whose physical/cache link no longer exists.

## 7. Dreaming (v0.3)

Periodic (default: nightly) or via the `dream` RPC. The only process that
looks at global structure, and the place where GDS is used if at all.

```text
  phase 1  community      pin source_extraction_generation = active[extraction]
                          source_covered_ingest_seq = Generation.covered_ingest_seq
                          and source_structure_revision = structure_revision
                          · export only IDs and MENTIONS/RELATES_TO arcs for Facts with
                            max_source_ingest_seq ≤ the pin to
                            ~/.anamnesis/tmp/dream/<operation_id>/, capped at 8 GiB
                          · start only with ≥ 10 GiB free so the spool's 2 GiB floor
                            remains reserved; delete+directory-fsync the temp tree in finally
                          · load that file into a disposable GDS container; Leiden returns
                            only {element_id, community_assignment}
                          · container image is digest-pinned, network=none, temporary volume;
                            no live credential, content or write path enters it
                          · source_export_digest = SHA-256 of ordered exported
                            node IDs, arcs and captured visibility thresholds
                          · one Community node + HAS_MEMBER per community (generation = new g_c)
                          · HAS_MEMBER captures each member's visible_from_utc;
                            Community.visible_from_utc is the majority threshold (docs/03 §3)
                          · summary content by LLM; on failure content = member names joined
                          · write tx requires unchanged source_structure_revision,
                            active extraction selector, covered prefix, export digest
                            and every assignment ID; otherwise discard
                          · switch active[community] only while its source extraction
                            generation/prefix still match the captured values
  phase 2  synthesis      pin active community c, its extraction generation g,
                          covered prefix and source_structure_revision
                          · choose exactly 1–16 non-synthesis support Facts per synthesis
                            ORDER BY m0 DESC, time_utc DESC, id ASC
                          · the LLM receives exactly that support set; no omitted member is
                            a semantic contributor
                          · support_fact_ids and semantic DERIVED_FROM links equal the bundle
                          · materialized authority = top 16 union of those supports' Episodes
                          · write tx requires unchanged structure revision, selectors,
                            generation/prefix and support ID/validity digest;
                            otherwise discard/retry
                          · append to active extraction generation g
                          · promotion Hits on exactly the support Facts, through the commit path,
                            namespace dream:<synthesis_fact_id>   (docs/04 §5–6)
  phase 3  profile        (:ProfileCache) top Facts around identity anchors
                          ORDER BY score DESC, Fact.id ASC; pin extraction/community selectors
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
| Neo4j cold start (≤ warmup_wait 20 s) | spool, or `resource_exhausted` at its cap | wait, then empty success `neo4j_unavailable` | paused | paused |
| Neo4j down | spool, or `resource_exhausted` at its cap | empty success `neo4j_unavailable` | paused | paused |
| LLM down | normal | normal | backs up (retry) | maintenance normal; dreaming phase 1 summary fallback, phase 2 skipped |

An "empty success" is `{results: [], diagnostics: {reason}}`, not an
exception — host hooks must keep exiting 0.

## 10. Security boundary

Personal, single-user, localhost. The boundary is the OS user.

| Control | Rule |
|---|---|
| data directory | `~/.anamnesis/` mode 0700; files 0600 |
| UDS | `sock` mode 0600 plus a per-install 32-byte capability in `socket.token` (0600), required by `hello`. This is portable in pure Node; no native peer-credential addon |
| request caps | length-prefixed RPC frame ≤ 1 MiB; decoded chunk ≤ 512 KiB; object ≤ 64 MiB; per connection 2 uploads/128 MiB temp; global 64 connections/32 uploads/1 GiB temp; spool ≤ 1 GiB and ≥ 2 GiB free-space floor |
| Neo4j bind | container publishes `127.0.0.1:7687` only; no HTTP port published |
| Neo4j auth | password generated per install (32 random bytes, base64) into `neo4j.auth` (0600) and injected into compose. No default password anywhere |
| Neo4j plugins | GDS only in disposable networkless dreaming/validation jobs loaded from bounded exports or snapshots; never connected to the live authority |
| daemon singleton | atomic-mkdir `daemon.lock/` lease with nonce/heartbeat/socket liveness; pure Node, no `flock` or native addon |
| LLM / embedding egress | only explicitly configured endpoints; exact payload classes are enumerated below. Remote egress is opt-in |

### Egress payloads

No endpoint receives the database, payload bytes, credentials, Hit ledger or
unrelated memories. Every encoded outbound request body is capped at 256 KiB:
embedding batches split before the cap; candidate snippets are capped at
1 KiB each and lowest-ranked candidates are removed deterministically until
the body fits. The primary Episode/claim/support input is never truncated
beyond its schema content cap.

| Operation | Data sent |
|---|---|
| query embedding | recall query text only |
| Episode embedding | bounded batch of Episode normalized content |
| Fact / Entity / relationship embedding | bounded batch of Fact, Entity or RELATES_TO content |
| claim extraction | one Episode's normalized content |
| payload-section extraction | decoded text sections ≤ 64 KiB each; local by default, separately opted in for remote |
| Entity / Fact judge | extracted claims plus at most 64 Entity and 128 Fact candidate snippets and their IDs/times |
| dreaming summary / synthesis | one Community's bounded member names or Fact snippets, capped at 256 items / 256 KiB |

The default configuration uses loopback endpoints. Configuring a remote
endpoint requires `allow_remote_egress=true`, HTTPS, a fixed resolved host,
redirects disabled, proxy environment ignored, a 20 s timeout and a 4 MiB
response cap. Payload-derived sections additionally require
`allow_remote_payload_egress=true`; otherwise remote extraction sees only the
stored Episode excerpt. Raw payload bytes are never sent. `status` reports
which data classes each remote endpoint can receive. Disabling embedding drops
the vector channel and backs up embed jobs.
Disabling the LLM backs up extraction and uses the documented dreaming summary
fallback; recall itself remains LLM-free.

## 11. Transaction boundaries

| Operation | Transactions | revision |
|---|---|---|
| remember | 1 (Episode, Payload, revision INVALIDATES, session topology, cache init, Outbox) | +1 |
| extract (one Episode) | read tx + write tx (re-validated) | +1 only when ACTIVE structural output is created |
| embed backfill | 1 per bounded batch, contiguous coverage cursor | +1 only for ACTIVE coverage |
| standalone commit / exposure | 1 (Hit CREATE + cache SET, once per source Episode) | — |
| re_mention / promotion | joins the owning extraction/dreaming write tx | Hit portion — |
| generation build/catch-up | bounded write tx per Episode | — while hidden |
| generation switch / caught-up rollback | one cutover tx under write queue | +1 |
| maintenance | 1 per batch (cache SET) | — |
| dreaming phase | ≥ 1 per phase, phase discarded as a unit on failure | +1 only for active-view structural output or selector switch |
| gc | 1 per batch | —; operational/active generations are refused |
