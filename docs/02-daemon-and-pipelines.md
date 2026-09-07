# 02 — Daemon and Pipelines

Exactly one process writes. `anamnesisd` is Neo4j's only bolt client, and
every caller talks to the daemon over a UDS. Harnesses are external projects
maintained by the operator; this repo ships only the daemon, its ops CLI and
the RPC contract (D39).

This is the normative target architecture, not a claim that every stage ships
today. The current implementation supports original Episode storage/fulltext
recall; the roadmap distinguishes later extraction, policy, receipts, PPR and
dreaming work. D43–D51 finalize the contracts below without authorizing code
changes.

```text
  external harnesses ─┐  (separate repos, attach over the RPC contract)
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

- semantic original Element/Link or session-topology CREATE/DELETE
- Element or Link CREATE in the currently ACTIVE generation
- an ACTIVE Entity's `visible_from_utc` moves earlier
- selected model's global Episode or ACTIVE-generation embedding coverage advances
- generation selector switch (`active_*` SET)

**Not incremented on:** Hit or RecallReceipt CREATE, policy control Episode
CREATE (uses `policy_revision`), hit/utility-cache / `m_cache` / shortlist SET,
hidden-generation embedding backfill, Outbox cursor,
BUILDING/CATCHING_UP generation writes, or
GC of hidden RETIRED generations. These do not change the *serving structure*
recall sees, so recall's consistency check must not trip on them. The
increment happens in the same transaction as the visible write or selector
change that caused it.

`Meta.ingest_seq` is a separate monotonic write cursor allocated in every
Episode CREATE transaction. It orders generation catch-up but is never used
as a recall consistency revision. It is allocated as late as that transaction
allows: the single `(:Meta {key:'meta'})` node is the one object every
concurrent remember must lock, and Neo4j holds a write lock until commit, so
an early increment serializes unrelated remembers for their whole duration.
Allocating inside the transaction is what keeps the sequence gapless — an
aborted remember (`revision_conflict`, `stale_revision`) consumes no number,
which the cutover barrier's `covered_ingest_seq = Meta.ingest_seq` proof
(docs/01 §4) depends on. `Meta.key` is unique, so remembers racing on a cold
database cannot each MERGE their own Meta node and hand out the same number.

### Logical server time

Write transactions issue `server_time =
max(wall_clock_ms, Meta.last_server_time + 1)` and persist it on Meta. Read
requests use the maximum of wall clock, persisted time and the process-local
last issued value without writing. Forgetting still clamps every elapsed
interval to nonnegative (docs/04), so imported old Hits and wall-clock
regression cannot produce `R>1` or lower stability.

recall reads the revision twice to detect structural change
([05-recall](05-recall.md) §7). On a mismatch it retries once; if it changes
again it proceeds with `diagnostics.torn = true` — numeric recall work does not
hold the write queue; only bounded control publication uses its barrier.

### policy_revision — a stricter publication barrier (D43)

`Meta.policy_revision` is separate from structural consistency. A policy command
atomically appends its control Episode, publishes the rebuilt active-policy
cache, and increments this revision under the write queue. A recall pins the
revision before reads, checks every stage against that policy, and rechecks
under a short publication barrier before durable receipt creation and socket
publication. Policy commands and response publication serialize at this
barrier: once a policy command acknowledges, no later response is published
under its predecessor policy. Already-delivered bytes cannot be withdrawn.

On mismatch, retry the recall under current policy; if that retry also races,
reject `policy_changed`. `diagnostics.torn` never permits torn-policy serving.
Feedback, extraction, embedding and dreaming likewise revalidate policy before
their transaction commits. A commit based on an old receipt must revalidate
the recorded results and sources under current policy; reject it atomically
if any credited item/source is now denied. No denied Hit is written, even
when the receipt's original policy allowed it. Policy cache unavailable means
retryable `policy_unavailable`, not an empty-policy fallback. Background reconciliation is not
the immediate enforcement mechanism.

Policy reconciliation is not semantic re-extraction. It rebuilds the serving
view without denied content while preserving content-free invalidation
evidence and target/effective-time/generation markers (docs/01 §4). Markers
are private control/cache state, never conductors or returned text/IDs.
Generation activation requires exact source-validity/invalidation equivalence
at every supported T; missing evidence, mapping or marker coverage fails
closed. In particular, `A <-INVALIDATES- B; deny B; rebuild` leaves A invalid
from B's effective time onward, even though B cannot be returned.

`structure_revision` alone is not a replay token. Captured index/candidate,
cache, policy, config, generation, and channel-degradation state determine
the numeric result; receipts retain bounded replay evidence (D47, docs/05).

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
| `recall {query, session?, T?, limit?, budget?}` | dispatcher receipt | Read-only semantic handler; dispatcher durably appends control impression before publication (docs/05) |
| `commit {recall_id, adopted?, reward?}` | Hit | Receipt-mode client's adoption report (`recall_hit`) and/or signed outcome verdict (`outcome`, `reward ∈ [−1,1]`) (docs/04 §6) |
| `policy.set {policy_id, selector, scope}` | policy Episode/cache | Explicit authenticated deny command; idempotent by policy ID and canonical body |
| `policy.revoke {policy_id}` | policy Episode/cache | Explicit authenticated revocation of that immutable deny |
| `adjudication.review {review_id, proposal_id, action: accept\|reject, reason}` | review record | Authenticated operator decision on one shadow proposal; `SHADOW → ACCEPTED\|REJECTED` only (§5.2) |
| `adjudication.correct {correction_id, action, proposal_id?, target_generation, source_episode_id, proposed_claim_digest, candidate_digest, replacement_verdict?, target_ids[0..8], effective_time_basis, bad_evidence_ids[0..8], reason}` | operator Episode, correction record, replacement Fact | Authenticated append-only repair of an adjudicator mistake (§5.2) |
| `embedding.retry {operation_id, embedding_model_id, stream, generation, ingest_seq, item_ordinal, reason}` | resolution record | Move the exact BLOCKED head back to PENDING (docs/01 §4) |
| `embedding.skip {operation_id, embedding_model_id, stream, generation, ingest_seq, item_ordinal, reason}` | resolution record | Resolve one BLOCKED head as `RESOLVED_NO_VECTOR`; permanent vector exclusion for that source |
| `embedding.cancel {operation_id, embedding_profile_id, reason}` | resolution record | Cancel a non-active target build under the write-queue barrier |
| `status` | — | Neo4j connection, revision, active selectors, spool length, Outbox backlog, blocked embedding heads |
| `verify {scope}` | — | Digests, Payloads, orphan Facts, ledger ↔ cache agreement |
| `gen {stream, action: build\|status\|activate\|rollback\|retire\|qualify}` | selector, qualification record | Lifecycle operations; activate/rollback enforce the catch-up barrier, `qualify` appends an `EmbeddingQualification` and activates nothing by itself (docs/01 §4) |
| `maintain` | caches | Run the maintenance job now (§6) |
| `dream {phase?}` | derived, Hits | Run dreaming now (§7) |
| `gc {derived\|embedding\|objects, …}` | DELETE | Explicit cleanup (docs/01 §4, §9) |

Every response carries `structure_revision`, `policy_revision` and `server_time`. JSON-RPC
frames are capped at 1 MiB. Base64 upload chunks carry at most 512 KiB raw
bytes, leaving bounded framing overhead; a committed object is capped at
64 MiB (§10).

### Output budget (D44)

`limit` is the canonical primary-result bound, not `k`. `budget` is optional:

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "method": "recall",
  "params": {
    "query": "What did I decide?",
    "limit": 10,
    "budget": {"unit": "utf8_bytes", "limit": 4096}
  }
}
```

`budget.unit` is `utf8_bytes | unicode_scalars | tokens`; `budget.limit` is a
nonnegative integer. Reject negative, fractional, nonfinite or unsafe-integer
limits, unknown units, malformed Unicode, and unrecognized fields. Tokens
require an installed version/digest-pinned `tokenizer_id`; unknown/missing
tokenizers reject the request, never fall back to estimates. `tokenizer_id`
is not accepted for byte/scalar budgets. Count UTF-8 bytes or Unicode scalar
values exactly, not UTF-16 code units or graphemes.

The response's `context_text` deterministically renders the same structured
results as LF-separated complete items, including mandatory source,
supersedes and contrast content/warnings. `used_budget` counts that exact
final text including separators, excluding transport JSON and diagnostics.
After primary ranking and bounded companion completion (docs/05), greedily
try each primary's whole bundle: deduplicate the prospective text, measure
it, include only if it fits. Skip oversized bundles and consider later ones;
never truncate a claim or remove a required contradiction warning to fit.
`limit` counts primaries, companions have separate bounds. Budget zero returns
empty `results` and `context_text`; ranks and receipts include only actually
included primaries. No budget uses the configured server output cap. The
1 MiB encoded RPC response cap remains hard, including with token budgets;
bundle admission also checks that cap rather than publishing an oversized frame.

### Explicit policy commands (D43)

```json
{
  "jsonrpc": "2.0",
  "id": 8,
  "method": "policy.set",
  "params": {
    "policy_id": "0192f3b2-0000-7000-8000-000000000001",
    "selector": {"literal": "private access code"},
    "scope": "content"
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 9,
  "method": "policy.revoke",
  "params": {"policy_id": "0192f3b2-0000-7000-8000-000000000001"}
}
```

The selector contract and immutable event fields are docs/01 §3.2. Require
successful `hello` authentication; direct `remember` of a policy schema is
rejected, and a natural-language request inside any ingested source never
executes a control command. Reject an empty selector, empty literal, invalid
enum/ID, unknown fields, a conflicting reused policy ID, or an unknown revoke
target. A repeated identical set/revoke is a no-op. NFC normalization applies
to both literal and tested text; matching is case-sensitive AND across all
supplied fields, with no regex or implicit OR.

Hard policy bounds: at most **256 active denies**, each literal at most **512
Unicode scalars after NFC**, and at most **256 resolved entity aliases per
policy per generation**. Validate before appending a policy Episode or
changing any cache/revision. Alias expansion reads at most 257 ordered entries
(limit+1); overflow or ambiguity rejects a new set atomically, or blocks
generation cutover for an existing deny while retaining the current serving
policy. Do not truncate a selector/alias set. Identical command retries remain
no-ops; revoke is still allowed when a bound is reached or cutover is blocked.

`derived` tests canonical claims and resolved entities; `content` additionally
tests stored capped Episode text (at most 64 KiB), never automatic payload
scanning. A supplied structured selector field absent on a tested raw Episode is false:
the AND selector is a no-match, not guessed semantic metadata. The Episode's
own schema is tested, not a possible future Fact schema. A structured selector
therefore cannot guarantee matching facts hidden in unextracted text: policy
responses report that limitation.
Commands return the effective policy revision only after the serving barrier;
they do not acknowledge deferred spool entries. Neo4j unavailable means
`policy.set`, `policy.revoke` and `commit` fail with retryable
`storage_unavailable`, never report unapplied effects as accepted. Revocation
does not implicitly re-extract skipped assertions. Background rebuild removes
denied derived material from serving indexes/caches, not immutable originals,
operator raw access or old backups. There is no erase operation.

### Adjudication and embedding control commands (D50, D51)

These five methods and the `gen` action `qualify` are control-only. They
require successful `hello` authentication, and no text, retrieved memory or
model output can invoke them. Every one is idempotent by its own UUIDv7
operation ID: a byte-identical canonical body is a no-op, and a different body
under the same ID rejects (`review_conflict`, `correction_conflict` or
`operation_conflict`).

```json
{
  "jsonrpc": "2.0",
  "id": 11,
  "method": "adjudication.review",
  "params": {
    "review_id": "0192f3b2-0000-7000-8000-000000000010",
    "proposal_id": "0192f3b2-0000-7000-8000-000000000011",
    "action": "accept",
    "reason": "Verdict and target set match the source spans."
  }
}
```

`reason` is 1..512 Unicode scalars. A review performs only
`SHADOW → ACCEPTED | REJECTED`; any second terminal review and every unlisted
transition rejects atomically. Acceptance means the exact proposal may be
consumed once by its named target generation, and it is an operator decision,
not a human-gold label.

`adjudication.correct` has `action ∈ {replace_wrong_invalidation,
add_missed_invalidation, override_relation, unresolved}` with 1..512-scalar
`reason` and distinct IDs. `replace_wrong_invalidation` requires exactly one
target, 1..8 bad evidence IDs that all name that target, and an after verdict
in `NEW | DUPLICATE_OCCURRENCE | ELABORATION | UNRESOLVED_CONTRADICTION`.
`add_missed_invalidation` requires no bad evidence and an after verdict
`CHANGE | CORRECTION`. `override_relation` applies only before a semantic
write and takes any of the six verdicts. `unresolved` requires
`UNRESOLVED_CONTRADICTION`, appends audit only and leaves the existing pair
unchanged. The target generation and candidate digest must still be current
under the write lock; a stale request rejects rather than being redirected.

```json
{
  "jsonrpc": "2.0",
  "id": 12,
  "method": "embedding.skip",
  "params": {
    "operation_id": "0192f3b2-0000-7000-8000-000000000012",
    "embedding_model_id": "711660f809e00490d029df7e65b19dfccc0dda52c81a8a93a5ab0fed796e13e9",
    "stream": "episode",
    "generation": 0,
    "ingest_seq": 90210,
    "item_ordinal": 0,
    "reason": "Source exceeds the pinned context and no larger-context profile is built yet."
  }
}
```

`embedding.retry` and `embedding.skip` require the named row to be the current
BLOCKED tuple head; stale or non-head operations reject. A skip permanently
excludes that source from the model's vector channel and is rejected when it
would exceed an ACTIVE dependent profile's cumulative bound, whose default is
zero. `embedding.cancel` may not target the active profile. Neo4j unavailable
means all five return retryable `storage_unavailable` before any acceptance.

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
    1. Contract validation (semantic schema registry, origin, time, size caps;
       policy/control schemas require their explicit command path)
       origin_key   = sha256(source, session, actor, record)
       session_key  = sha256(source, session)
       source_revision = adapter-issued stable revision token
       payload_hash = committed object hash if present
       revision_key = sha256(origin_key, source_revision)
       Do not resolve role/lineage or read parent receipts here. Step 3a
       first selects the existing row's digest version; callers cannot
       supply or downgrade that version.
    2. If payload_hash: require the committed object to exist and match its metadata
    3. One Neo4j transaction
         a. MATCH (:Episode {revision_key}); an existing row is recomputed
            under its own stored episode_digest_version, and an absent value
            on that row is version 1
              exists, version 1 (absent value) → recompute the frozen
                       insertion-ordered JSON.stringify body of docs/01 §1,
                       never the version-2 RFC-8785 body. Same digest → no-op,
                       return existing id, created = false before validating
                       newly supplied role/lineage metadata or reading any
                       parent receipt, including expired parents. Any changed
                       version-1 field → reject revision_conflict. Newly
                       supplied origin_role or lineage values sit outside
                       version-1 identity: they create no EchoLineage, repair
                       no lineage, change no eligibility and mutate nothing
              exists, version 2                → recompute the version-2 body.
                       Same digest → no-op. A changed origin_role, a changed
                       lineage body or any changed earlier digest field under
                       this revision_key → reject revision_conflict
              exists, any other stored value   → reject
                       unsupported_digest_version
              absent                           → validate new version-2 admission:
                       origin_role = user | assistant | tool | document | operator
                       lineage_mode = direct | receipts; receipts requires
                       1..4 distinct parent_recall_ids, each an existing
                       receipt bound to this authenticated caller.
                       Resolve the bounded EchoLineage body under docs/01 §3.3;
                       lineage_digest = sha256(RFC-8785 canonical body).
                       Set server-selected episode_digest_version = 2 and
                       digest = sha256(RFC-8785 canonical JSON of
                         {episode_digest_version: 2, schema, content, properties,
                          time, payload_hash, previous_revision_key, origin_role,
                          lineage_digest}).
                       Only then CREATE below.
         b. CAS (:OriginHead {origin_key})
              no head + previous=null         → CREATE first Episode
              head=previous_revision_key      → CREATE new Episode,
                                                 new -[:INVALIDATES]-> previous,
                                                 SET head=new revision_key
              otherwise                       → reject stale_revision
         c. MERGE Payload metadata, HAS_PAYLOAD
         d. update the rebuildable session topology around this Episode
            in total order (time_utc ASC, ingest_seq ASC)
         e. Initialize hit cache: s = S0(m0), t_last_hit = server_time, hit_count = 0   (docs/04 §2)
         f. increment Meta.ingest_seq and assign it to the new Episode.
            Every concurrent remember contends for this one node's write lock,
            which Neo4j holds until commit, so the increment rides on the last
            statement that nothing else in the transaction depends on
         f2. CREATE the EchoLineage row for this newly created version-2
            Episode (docs/01 §3.3); an existing version-1 row never reaches
            this step:
            copy each parent receipt's bounded delivered snapshot, pair the
            parent IDs with their selection digests, sort by recall ID, store
            the sorted distinct root union and 1 + max(item.echo_depth).
            Every parent's created_at must precede this ingestion. Either cap
            overflowing sets complete=false; an exact retry of the same body
            is a no-op and a different body is idempotency_conflict
         g. For each distinct M in {active model, target model if set},
              CREATE Outbox
              {episode_id, stage: embed_episode, target_generation: 0,
               ingest_seq, embedding_model_id: M, model_key: M}
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
that mutates one token; the stored `episode_digest_version` decides which body
that comparison uses, and an existing row's version always wins over the
version a new caller or journal entry would have created (docs/01 §1). `OriginHead` is a rebuildable CAS cache over the
immutable INVALIDATES chain.

remember calls no LLM. It does not embed either — ingestion must succeed even
when the embedding service is down. Extraction and embedding are driven by the
Outbox.

The v0.1 Episode embedding worker consumes the captured
`{stage: embed_episode,target_generation:0,ingest_seq,embedding_model_id,model_key}`,
sends only the Episode's normalized content to that model endpoint, SETs
`embedding_m_<modelhex>` and advances its model-scoped terminal-prefix cursor
in a bounded batch. A transient failure retries under the bounded schedule and
a deterministic one blocks that head; either way BM25 and session recall stay
available and the cursor never moves past the hole (docs/01 §4). This producer
ships in the same milestone as Episode vector recall.

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
  `record_uuid`, `origin_key`, `revision_key`,
  `previous_revision_key` and the server-selected **creation**
  `episode_digest_version`. Every newly admitted record carries version 2;
  a legacy acknowledged record with no version replays as version 1.
- When Neo4j is back, drain the dependency-ready records in the deterministic
  cross-epoch order from §0. For each record the transaction commits first,
  then its checksummed `.done` cursor is appended and fsynced. Cursors advance
  only over the contiguous processed prefix of each journal. A crash between
  commit and cursor replays the record, and `revision_key` makes it a no-op.
- At drain, an Episode already stored under that `revision_key` is
  authoritative and its stored version verifies the retry; the captured
  journal version controls only creation when the key is absent. So a new
  version-2 journal retry of an existing version-1 Episode stays a version-1
  no-op with zero SETs, while an old versionless acknowledged entry still
  creates the version-1 Episode it promised.
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
        capture policy_revision; control Episodes complete as no-ops
        content-denied input creates no derived output or re_mention Hit
    ── extraction LLM (outside a tx) ────────────────────────────────────────
    L1. Episode → at most 32 claims[], in Episode order:
          {content, content_language, sub_kind, modality, confidence,
           evidence_quote?, evidence_kind?, span?, time_hint, entities[≤16],
           predicate_text, scope, scope_complete,
           corrects_local_claim_index?, correction_scope_text?, mode?}
        · content is self-contained: pronouns and deixis resolved to the named
          referent, so the claim reads correctly with no Episode in view
        · content_language follows the generation's fact_language_policy
          (D48). A source generation keeps the Episode's language, using mul
          for materially multilingual prose and und when the evidence is
          nonlinguistic; an en generation accepts only en and rejects a claim
          it cannot render source-faithfully as language_policy_mismatch,
          with no per-claim fallback
        · names, identifiers, paths, URLs, code and quotes stay source-exact.
          Never transliterate a name into claim content, an Entity
          normalized_name or an alias; visually similar identifiers in
          different scripts are different identifiers
        · modality ∈ {asserted, reported, hedged, intended, hypothetical} — the
          speech act, not the confidence (§5.1). A hedge or an intent is kept
          and labelled, never silently promoted to a fact or silently dropped
        · confidence ∈ [0,1] — the judge's belief that the claim is what the
          Episode says, given its modality. It never bypasses the mechanical
          evidence checks in W2 or semantic admission
        · evidence_quote, when present, is 1..8,192 UTF-8 bytes and a
          byte-for-byte contiguous substring of the immutable Episode content
        · span = [start, end) UTF-8 byte offsets into Episode.content that the
          claim rests on; the worker derives it from the quote's unique
          occurrence (W2)
        · predicate_text, scope and scope_complete are the bounded
          same-speaker/time/scope fields below; they are meaning-bearing
    L1b. Suppress an earlier claim only when this later claim sets
        corrects_local_claim_index to that earlier index and same-speaker,
        same-time, same_scope_l1b and same-modality all hold, and the later
        valid source span explicitly corrects or retracts it. same_scope_l1b
        is the narrower correction predicate in §5.3, not the complete
        byte-identical grouping scope. Different reports,
        times or modalities remain separate assertions; unresolved
        contradictions produce CONTRASTS, never a last-sentence-wins rule
    L2. Time resolution: explicit > relative against Episode.time > inherit.
        Never against wall clock
    ── bounded read tx, all indexes scoped to target_generation ─────────────
    R1. For each extracted entity mention: synchronous fulltext top 16.
        Deduplicate by Entity id; score DESC, id ASC; keep global top 64
        (≤ 16 mentions × 16 raw rows = 256 inspected result rows)
    R2. For each claim: synchronous Fact fulltext top 64 + session top 32.
        Deduplicate by Fact id; equal-weight RRF, id ASC; keep top 128.
        Candidate text stays in its stored source language; no translated
        query or projection enters this candidate set (D48)
    R3. For every candidate invalidator B, indexed lookup of at most eight
        outgoing INVALIDATES targets supplies replacement context, including
        invalid A, its authority and content
    R4. candidate_digest = sha256(canonical ordered candidate IDs, scores,
        validity bits, replacement-context edge IDs and policy_revision)
        All R1–R4 candidates and text obey current policy, including context
        obtained through otherwise snapshot-exempt provenance
    ── judge LLM (outside a tx) ─────────────────────────────────────────────
    L3. Entity judge: match an existing candidate / create new with
        entity_key = sha256(target_generation, normalized_name, entity_kind).
        A mention with no resolvable referent (bare pronoun, "the client")
        creates no Entity; the literal stays in the claim's content
    L4. Claim judge against Fact candidates, under the pinned judge_profile_id:
          new                        → Fact + DERIVED_FROM Episode + MENTIONS
          duplicate of F             → new occurrence Fact + DERIVED_FROM Episode,
                                        RELATES_TO F; optional audit re_mention
          elaboration of F           → Fact + RELATES_TO F
          contradiction, resolved    → Fact + at most 8 INVALIDATES targets
                                        (mode: change | correction, docs/03 §5)
          contradiction, unresolved  → Fact + CONTRASTS F
        In the shadow default (§5.2) this stage writes an AdjudicationAttempt
        and, on a valid output, an immutable AdjudicationProposal instead of
        the Fact-to-Fact relations above; only an authenticated accepted
        proposal reaches W1. L4 is also the only stage that may mark a Fact a
        known echo, and only against an exact delivered result ID in a
        complete parent receipt (docs/01 §3.3)
    ── write tx ─────────────────────────────────────────────────────────────
    W1. Re-validate:
        · policy_revision unchanged
        · normal path: active[extraction] == target_generation
        · rebuild path: target state is still BUILDING or CATCHING_UP
        · ingest_seq still equals Generation.next_ingest_seq
        · OriginHead[episode.origin_key] still equals input_head
        · rerun the bounded reads and require the exact candidate_digest
        · every referenced candidate and replacement-context edge is unchanged
        · for an accepted adjudication proposal, compare its **stored** fields
          directly: target_generation still ACTIVE,
          OriginHead[episode.origin_key] == proposal.source_head_revision_key,
          Meta.policy_revision == proposal.policy_revision, the revalidated
          claim reproduces proposal.proposed_claim_digest, and the repeated
          bounded read reproduces proposal.candidate_digest. Never infer
          proposal-time source or policy state from the current graph or from
          the candidate digest; any mismatch is stale acceptance, produces no
          output and no consumption row, and needs a new attempt (§5.2)
        · the Episode's lineage is complete: an assistant Episode with unknown
          or truncated lineage produces no semantic output, and neither does
          anything derived from it (echo_lineage_unavailable, docs/01 §3.3)
        any check fails → abort tx and retry the same sequence head
        Under the pinned policy, match canonical claims, resolved entities,
        links and supporting sources. Suppress denied claims and their writes/
        Hits as deterministic no-output, not as a retryable failure. Do not
        create Entities or relations supported only by suppressed claims
    W2. Evidence check, before anything is created: a present
        `evidence_quote` must be 1..8,192 UTF-8 bytes and a byte-for-byte
        contiguous substring of the immutable Episode content. Derive the
        canonical [start, end) UTF-8 byte span from that unique occurrence.
        A nonliteral quote, or a repeated quote with no supplied valid span
        whose exact slice equals it, rejects that claim as evidence_mismatch.
        Never normalize, translate, repair or silently choose an occurrence.
        The normative stored evidence is the validated span and its exact
        source slice; evidence_quote is retained in bounded audit output, not
        as a second mutable authority. One span remains the v0.2 contract: a
        claim needing disjoint evidence is narrowed, represented by one
        encompassing bounded span, or rejected, and multi-span support needs
        a schema and version change. A claim may omit evidence only when the
        extractor explicitly returns evidence_kind = no_single_locus and the
        generation configuration permits it; automatic writes initially set
        that permission to false, and such a claim is never described as
        mechanically grounded.
        Span check: a present `span` must slice
        Episode.content to a non-empty string on UTF-8 boundaries, else the
        claim is rejected (`diagnostics.extract.span_mismatch += 1`, the
        Episode still completes). Require integer offsets with
        0 <= start < end <= utf8_byte_length(content). This verifies only
        nonempty byte boundaries, not quotation, entailment or hallucination;
        a valid span can still point at text that does not support the claim.
        For each surviving new Fact, materialize 1–16 source Episode IDs and
        matching direct DERIVED_FROM links (docs/01 §1), the primary link
        carrying `span`; then CREATE Facts and links in target_generation. Every derived endpoint is in that same generation;
        cross-generation extraction links are rejected. Fact identity hashes
        generation, schema, content, content_language, meaning-bearing
        properties (including predicate_text, scope and scope_complete), time,
        sub_kind, modality, primary Episode, lineage fields, entity
        bindings, source Episodes and synthesis supports; Entity identity uses entity_key.
        Copy the Episode's bounded lineage onto each Fact: a known echo takes
        the delivered item's roots and 1 + item.echo_depth, a context-derived
        Fact takes the Episode root union and depth, and neither adds the
        assistant Episode as another root (docs/01 §3.3).
        Exact retry collisions are no-ops. `confidence` is stored but not part
        of identity: two runs that agree on meaning and differ only in belief
        collide, and the first write's confidence stands — model
        nondeterminism on a scalar must not fork Facts. Retain bounded raw
        output and prior/calibration/judge versions separately from identity;
        reject oversized output rather than discarding required audit fields.
        SET each mentioned
        Entity's visible_from_utc to min(current, mention source time)
    W3. Optional re_mention audit Hits through the commit path (docs/04 §6),
        namespace extract:<episode_id>; no change to S or t_last_hit
    W4. For each distinct M in {active model, target model if set},
        CREATE Outbox
        {stage: embed_derived, target_generation, ingest_seq, fact_ids,
         embedding_model_id: M, model_key: M}; uniqueness is
        (stage,target_generation,ingest_seq,model_key). With no derived
        vectors, the sequence gets its ordinal-0 sentinel and M's coverage
        cursor advances as a no-op
    W5. append the AdjudicationConsumption row for any accepted proposal this
        transaction consumed; advance next_ingest_seq/covered_ingest_seq to
        this committed sequence;
        if structural output was created in the ACTIVE generation,
        structure_revision += 1. BUILDING/CATCHING_UP writes do not bump it;
        an ACTIVE duplicate occurrence creates structural output and does
```

`structure_revision` may legitimately change between R1 and W1 (other
Episodes being extracted); only the checks in W1 matter. A changed source
head or candidate means the judge's premises changed — re-running is cheaper
than committing a stale relation. A historical input Episode may still be
extracted after its head changed on an earlier run: its derived Facts inherit
the bounded source authority's temporal validity (docs/03 §3–4), so they serve
historical snapshots without becoming current again.

A source/span example, using normalized stored content (not a complete RPC):

```json
{"content":"Aé🙂Z","span":[1,7]}
```

The content is 8 UTF-8 bytes and 4 Unicode scalars; `[1,7)` selects `é🙂`.
`[2,7)` splits `é`, `[1,6)` splits the emoji, `[1,1)` is empty, and `[0,9)`
exceeds the source: all are rejected. A valid `[0,1)` merely selects `A`;
it cannot prove an arbitrary extracted claim is supported. Omitted span is
allowed only for no single locus; malformed-present is not treated as omitted.

The `embed_derived` stage SETs the named model property and advances
`EmbeddingCoverage(target_generation, embedding_model_id)` only through a
contiguous terminal prefix. Global Episode embedding jobs use the same
model-scoped cursor with `stream=episode,generation=0`. If either cursor
affects the active profile's serving vector set, the same transaction
increments `structure_revision`.

Embedding work runs the per-entry state machine in docs/01 §4:
`PENDING → RUNNING → SUCCEEDED | NO_VECTOR_REQUIRED | RETRY_WAIT | BLOCKED`,
with `BLOCKED → PENDING` on authenticated retry,
`BLOCKED → RESOLVED_NO_VECTOR` on authenticated skip, and
`PENDING | RUNNING | RETRY_WAIT | BLOCKED → CANCELLED` for unreferenced target
work only. Attempt outcome is `succeeded | no_vector_required |
transient_failure | permanent_failure | worker_lost | cancelled`, and
`error_code` is exactly the docs/01 §4 enum:

```text
  retried (three attempts per cycle, fixed [1000, 10000] ms delays, no jitter;
  the third transient failure BLOCKS the head):
    unavailable | timeout | rate_limited | server_error

  BLOCKED immediately, never retried:
    invalid_input | context_overflow | zero_norm | nonfinite |
    wrong_dimension | profile_mismatch | cardinality_mismatch |
    malformed_response | client_error

  lease loss: worker_lost closes the RUNNING attempt and follows the same
              retry and third-failure rule; it never resets a counter
  cancellation: cancelled is terminal and follows no retry rule
```

`client_error` is the exact name for a deterministic 4xx response and
`malformed_response` for output that is not a valid embedding payload; neither
is a transient class, and "4xx" is not itself a state. Every attempt leaves
one durable terminal `EmbeddingAttempt` row with a bounded error code and
digest and no source text. A blocked head freezes that model's cursor and
leaves later entries unpublished, so no vector ever appears across a hole; the
active profile keeps serving its prior prefix while BM25, session and PPR
recall continue. Nothing is truncated, chunked, zero-filled or silently
skipped to move the cursor. The build lifecycle
(`BUILDING | BLOCKED | ACTIVE | INACTIVE | CANCELLED | RETIRED`), the
qualification requirement and the current-watermark cutover barrier are in
docs/01 §4.

### 5.1 Modality — the speech act is a stored field

Extractors face a choice on "I'm thinking of getting an orchid" or "I think we
deployed on Friday": drop it as not durable, or store it as a weak fact. Both
lose information — the first forgets that an intent existed, the second lets
an intent be counted as an acquisition. The engine stores the claim with the
speech act it came in:

| `modality` | The Episode… | Example |
|---|---|---|
| `asserted` | states it as so | "I moved to Busan" |
| `reported` | attributes it to someone else | "Bob said the deploy failed" |
| `hedged` | states it with an uncertainty marker | "I think we deployed Friday" |
| `intended` | states a plan or wish, not a done thing | "I'm going to switch to Neo4j" |
| `hypothetical` | states it under a condition or as a possibility | "if we hit 10k users we'd shard" |

`modality` is meaning-bearing: it is part of Fact identity (docs/01 §1), an
input to `m₀` (docs/04 §1), and exposed on every recall result (docs/05 §6).
It is required on synthesis too: judge the synthesis content's modality and
confidence in its support-faithfulness; neither an arbitrary asserted default
nor multiplication of uncalibrated marginal confidences is allowed. Policies
and temporal validity are hard filters, not confidence discounts. Changing
priors produces a new generation; Hit replay never changes immutable `m0`.
An `intended` claim later fulfilled is a new `asserted` Fact whose judge
outcome is `elaboration` (RELATES_TO the intent), not a contradiction — the
plan was true as a plan. What a caller does with `hedged` or `intended` results
(filter, phrase, discount) is the caller's business; the engine's job is to
not lose the distinction.

### 5.2 Shadow adjudication and operator repair (D50)

The adjudication default is **development-scoped and shadow-first**. For the
frozen conformance prompt `scripts/research/adjudication-prompt.md` (SHA-256
`94a74ca2825e17d09188ae0af97c915c7a79a64a45ac998e2f584d8878c3275b`) the
default judge profile is `claude-opus-5` on Messages with thinking disabled,
and `gpt-5.5` on Responses with reasoning effort `none` is the comparator.
That choice is role-specific: it approves no extractor, transfers to no other
role, and is not a global model ranking. It ships in shadow mode, so the L4
stage emits proposed verdicts and audit records that create no Fact and no
semantic link.

```text
  AdjudicationAttempt {attempt_id, target_generation, episode_id,
                       source_head_revision_key, policy_revision,
                       candidate_digest, judge_profile_id, started_at,
                       finished_at, outcome, error_code?, error_digest?}
                        immutable, one per call; outcome is succeeded |
                        transport_error | parse_error | validation_error, and a
                        failed call yields no proposal at all
  AdjudicationProposal {proposal_id, target_generation, episode_id,
                        source_head_revision_key, policy_revision,
                        proposed_claim_digest, candidate_digest, verdict,
                        target_ids[0..8], evidence_ids[1..32],
                        effective_time_basis, reason, judge_profile_id}
                        immutable, created only from a valid strict output;
                        proposal_id equals its successful attempt_id
  AdjudicationReview    append-only accept/reject; state SHADOW | ACCEPTED |
                        REJECTED is materialized from these rows
  AdjudicationConsumption  one row per accepted proposal, written in the same
                        W5 transaction that used it (single use)
```

`source_head_revision_key` is the exact 64-lowercase-hex `OriginHead` value
captured before the bounded candidate read and the model call;
`policy_revision` is the nonnegative safe integer captured at that same point.
Both are **persisted premises**, not values reconstructed later from the
graph, and the proposal copies them, the target, Episode, candidate digest and
judge profile byte-for-byte from its successful attempt.

Before the call, the worker mechanically validates the complete L1 claim,
derives its canonical evidence span, resolves its time and materializes:

```text
  validated_l1_claim = {
    content, content_language, sub_kind, modality, confidence,
    evidence_quote: string | null,
    evidence_kind: no_single_locus | null,
    span: [start,end] | null,
    time_value, time_utc, time_precision,
    entities: 0..16 complete generation-schema-validated Entity mentions,
    speaker_key, subject_keys, predicate_text, scope, scope_complete,
    corrects_local_claim_index: integer | null,
    correction_scope_text: string | null,
    mode: change | correction | null
  }
  proposed_claim_digest = sha256(UTF-8(RFC-8785(validated_l1_claim)))
```

Every key is present, absent optional values are JSON `null`, `entities` stays
in extraction order as the complete validated array (not display strings or
post-resolution IDs), and fields declared sorted in §5.3 use that byte order.
The three time fields are the resolved stored Fact time, never an unresolved
hint. This digest adds no second evidence or time authority; bounds remain
those of W2 and docs/01–docs/03.

Because failures never become proposals, no denominator is silently invented
for a later "accuracy" claim. Acceptance is an operator decision, not human
gold, and it enables no unattended writing anywhere else. Unattended
invalidation stays out of scope until a new decision supplies independent,
production-shaped labels with declared false-invalidation, missed-update,
candidate-completeness, transport and parse bounds; the 36 synthetic
conformance cases and the 119 historical pseudo-label cases do not qualify
([research/adjudication-conformance](research/adjudication-conformance.md)).

**Operator repair** is authenticated, append-only audit authority. It never
rewrites or deletes a historical Fact, edge or `InvalidationEvidence`. When a
repair creates a replacement Fact, the daemon first appends a CREATE-only
`anamnesis.operator-adjudication/1` Episode, excluded from ordinary search,
extraction and PPR, whose deterministic renderer never impersonates user
prose. Restoring a wrongly invalidated `A` appends `A-prime` in `A`'s ACTIVE
generation under the docs/03 §5 replacement protocol: every bad edge is
retained, at most 65 incoming evidence rows are inspected, and at most 64
retained evidence IDs move forward as content-free markers. `A-prime` keeps
`A.time`, so it serves every `T >= A.time` once the correction commits, while
operator acceptance time stays audit-only. Ordinary recall labels the repaired
provenance `operator_corrected` (docs/05 §6).

### 5.3 Bounded grouping predicates (D49)

Same-speaker, same-time and same-scope are decided from bounded stored fields,
never from prose similarity:

| Field | Meaning |
|---|---|
| `speaker_key` | Stable authenticated speaker identifier inside one `(origin_source, origin_actor)` namespace |
| `subject_keys` | 1..16 sorted resolved Entity IDs, or null. No literal, normalized-string or display-name fallback exists |
| `predicate_text` | NFC case-preserving source-language relation, 1..256 scalars |
| `scope`, `scope_complete` | Closed bounded scope object (docs/01 §1) and whether every component resolved |
| `time_key` | Exact `(time_utc, time_precision)` pair |
| `modality` | The §5.1 enum |
| `corrects_local_claim_index`, `correction_scope_text` | Audit-only extractor fields: null or an earlier index in 0..31, and null or NFC slot text of 1..256 scalars |

`same_speaker(f,g)` requires two non-null equal `speaker_key` values in the
same namespace. A display name, alias, pronoun, fuzzy match or shared account
never implies speaker equality, and a null key makes the predicate false. An
unresolved subject stores `subject_keys = null`; falling back to literal text
would assert the global equivalence D49 refuses.

Scope equality has **two distinct predicates**, and they are not
interchangeable:

```text
  same_scope_l1b   = equal non-null correction_scope_text
                     + equal resolved subjects, predicate_text and
                       scope.attribution_speaker_keys
                     (the explicitly corrected value and time fields may differ)
  same_scope_group = scope_complete = true
                     + byte-identical RFC-8785 scope
```

The L1b rule uses `same_scope_l1b` precisely because a correction changes the
value it corrects; requiring a byte-identical complete scope there would make
every real self-correction fail. Grouping uses `same_scope_group`, which
admits nothing partial. Likewise `same_time` for L1b means the same immutable
Episode ID, with "later" requiring the correcting span's `start` to exceed the
draft's, while cross-Episode grouping and adjudication require the exact same
`time_key`. `same_modality` is exact §5.1 enum equality.

L1b suppresses an earlier local claim only when the later output sets
`corrects_local_claim_index` to that earlier index and same-speaker,
same-time, `same_scope_l1b` and same-modality all hold over a later valid
source span that explicitly corrects or retracts it. Otherwise both
occurrences survive and L4 decides their relation.

When every component resolves, assembly pins
`grouping_version = "anamnesis.duplicate-group/1"` and computes
`duplicate_group_key` over the sorted subject keys, predicate key, time key,
scope key and modality. A null subject, an empty predicate or an incomplete
scope disables grouping for that claim; the key is a local comparison inside
one pinned generation and one bounded candidate set, never a global identity
claim, and it never erases an occurrence.
`known_conflict(f,g)` is exactly one active-generation `CONTRASTS`
relationship with canonical endpoints: irreflexive, symmetric, not transitive.

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
                 · used to order bounded envelope fanout (docs/06 §2), hence gates
                   inclusion as well as final ranking; capture staleness for replay
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
Policy is checked on every consumption; stale mass/shortlists cannot bypass
suppression. Policy reconciliation rebuilds affected shortlists/profile caches
and generation indexes without denied material.

## 7. Dreaming (v0.3)

Periodic (default: nightly) or via the `dream` RPC. The only process that
looks at global structure, and the place where GDS is used if at all.

```text
  phase 1  community      pin source_extraction_generation = active[extraction]
                          source_covered_ingest_seq = Generation.covered_ingest_seq
                          and source_structure_revision = structure_revision
                          · pin policy_revision; export only allowed nodes/arcs
                            with allowed provenance/support witnesses
                          · export only IDs and MENTIONS/RELATES_TO arcs for Facts with
                            max_source_ingest_seq ≤ the pin to
                            ~/.anamnesis/tmp/dream/<operation_id>/, capped at 8 GiB
                          · start only with ≥ 10 GiB free so the spool's 2 GiB floor
                            remains reserved; delete+directory-fsync the temp tree in finally
                          · load that file into a disposable GDS container; Leiden returns
                            only {element_id, community_assignment}
                          · baseline GDS 2.13.12 image is digest-pinned, network=none, temporary volume;
                            no live credential, content or write path enters it
                          · source_export_digest = SHA-256 of ordered exported
                            node IDs, arcs and captured visibility thresholds
                          · one Community node + HAS_MEMBER per community (generation = new g_c)
                          · HAS_MEMBER captures each member's visible_from_utc;
                            Community.visible_from_utc is the half-members threshold (docs/03 §3)
                          · summary content by LLM; on failure content = member names joined
                          · write tx requires unchanged policy_revision, source_structure_revision,
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
                          · all supports and source Episodes must be allowed; judge
                            synthesis modality and support-faithfulness confidence,
                            retaining raw output and prior/calibration versions
                          · support_fact_ids and semantic DERIVED_FROM links equal the bundle
                          · materialize synthesis lineage from that exact support set before
                            Fact identity: no echo_of_element_id, first_4 of the sorted
                            distinct parent_recall_ids union, first_16 of the sorted distinct
                            corroboration root union, echo_depth = max(support depth) with no
                            added hop; context_derived only when every support is
                            lineage-complete and neither union truncates, otherwise unknown +
                            echo_lineage_truncated and ineligible for serving, support and
                            invalidation (echo_lineage_unavailable, docs/01 §3.3)
                          · materialized authority = top 16 union of those supports' Episodes
                          · write tx requires unchanged policy revision, structure revision, selectors,
                            generation/prefix and support ID/validity digest;
                            otherwise discard/retry
                          · append to active extraction generation g
                          · audit-only promotion on support Facts resolves to Episode Hits
                            through the commit path; no S/t_last_hit reinforcement,
                            namespace dream:<synthesis_fact_id>   (docs/04 §5–6)
  phase 3  profile        (:ProfileCache) top Facts around identity anchors
                          ORDER BY score DESC, Fact.id ASC; pin extraction/community selectors
                          and policy revision; reject denied content/supports before publishing
```

Dreaming never touches the originals layer except through the commit path
(promotion Hits). On failure it discards partial results (per transaction) and
tries again next cycle.

## 8. What is not on the recall path

- No LLM calls. Candidates, seeds, PPR, RRF and assembly are deterministic
  numeric work.
- No semantic memory writes. The numeric recall handler is read-only; the RPC
  dispatcher's explicit control-publication exception appends a bounded durable
  RecallReceipt before publishing the response (docs/01 §3.1); failure rejects
  publication rather than returning a commit-capable but untracked result.
  Auto-mode exposure is audit-only, after the response, through the commit
  path and a fresh policy check (docs/05 §10).
- No GDS calls.

## 9. Connection states and degradation

| State | remember | recall | extraction | maintenance / dreaming |
|---|---|---|---|---|
| Neo4j up, embed up | normal | normal | normal | normal |
| Neo4j up, embed down | normal | vector channel dropped (`channels_used`) | embed stage backs up (transient retry, then a BLOCKED head) | dreaming phase 2 skipped |
| Neo4j up, embedding head BLOCKED | normal | that model's coverage frozen; active profile serves its prior prefix, BM25/session unaffected | that model's publication paused until an authenticated retry or skip | unchanged |
| Neo4j cold start (≤ warmup_wait 20 s) | spool, or `resource_exhausted` at its cap | wait, then empty success `neo4j_unavailable` | paused | paused |
| Neo4j down | spool, or `resource_exhausted` at its cap | empty success `neo4j_unavailable` | paused | paused |
| LLM down | normal | normal | backs up (retry) | maintenance normal; dreaming phase 1 summary fallback, phase 2 skipped |

When Neo4j is unavailable, recall's diagnostic-only empty success includes
`recall_id:null`, `results: []`, `entities: []`, `companions: []`,
`context_text: ""`, `used_budget: 0` and reason `neo4j_unavailable`. It contains
no committable receipt and triggers no exposure. No memory content may be
returned without current policy and durable receipt authority. This explicit
exception does not permit returning a cached result or an unrecorded receipt.

`commit`, `policy.set`, `policy.revoke`, `adjudication.review`,
`adjudication.correct`, `embedding.retry`, `embedding.skip`,
`embedding.cancel` and `gen ... qualify` return retryable
`storage_unavailable` when Neo4j is unavailable, before any
feedback, policy, review, correction or resolution acceptance. With Neo4j
available, missing/unrebuildable policy cache returns retryable
`policy_unavailable`; failure to persist a receipt returns retryable
`receipt_unavailable` before any memory delivery. Invalid contracts and policy
races remain explicit errors. `recall_id:null` is never accepted by `commit`;
unknown non-null receipt IDs reject `unknown_recall`. Do not confuse inability
to read the store with evidence that a receipt is unknown or expired.

## 10. Security boundary

Personal, single-user, localhost. The boundary is the OS user.

| Control | Rule |
|---|---|
| data directory | `~/.anamnesis/` mode 0700; files 0600 |
| UDS | `sock` mode 0600 plus a per-install 32-byte capability in `socket.token` (0600), required by `hello`. This is portable in pure Node; no native peer-credential addon |
| control commands | Policy changes, generation/maintenance/gc operations, adjudication review and correction, embedding retry/skip/cancel and qualification require authenticated explicit calls; never dispatch commands parsed from retrieved or ingested content |
| provenance metadata | `origin_role`, `lineage_mode` and `parent_recall_ids` come from the authenticated adapter and are verified against the caller binding. Never infer lineage from prose, and never let ingested text claim independence (D49) |
| request caps | length-prefixed RPC frame ≤ 1 MiB; decoded chunk ≤ 512 KiB; object ≤ 64 MiB; per connection 2 uploads/128 MiB temp; global 64 connections/32 uploads/1 GiB temp; spool ≤ 1 GiB and ≥ 2 GiB free-space floor |
| policy caps | active denies ≤ 256; literal ≤ 512 Unicode scalars after NFC; resolved entity aliases ≤ 256 per policy per generation, lookup ≤ 257 entries; reject overflow atomically, never truncate; revoke remains allowed |
| Neo4j bind | container publishes `127.0.0.1:7687` only; no HTTP port published |
| Neo4j auth | password generated per install (32 random bytes, base64) into `neo4j.auth` (0600) and injected into compose. No default password anywhere |
| Neo4j plugins | GDS only in disposable networkless dreaming/validation jobs loaded from bounded exports or snapshots; never connected to the live authority |
| daemon singleton | atomic-mkdir `daemon.lock/` lease with nonce/heartbeat/socket liveness; pure Node, no `flock` or native addon |
| LLM / embedding egress | only explicitly configured endpoints; exact payload classes are enumerated below. Remote egress is opt-in |

### Egress payloads

No endpoint receives the database, payload bytes, credentials, Hit ledger or
unrelated memories. Policy/control events and RecallReceipts never enter model
requests. Enforce
current policy before egress and recheck before publishing model output;
already-sent remote input cannot be withdrawn. Every encoded outbound request
body is capped at 256 KiB. Embedding batches split before the cap; candidate
snippets are capped at
1 KiB each and lowest-ranked candidates are removed deterministically until
the body fits. The primary Episode/claim/support input is never truncated
beyond its schema content cap.

| Operation | Data sent |
|---|---|
| query embedding | the caller's verbatim recall query text inside the active profile's pinned query template; no translation and no second generated query (D48) |
| Episode embedding | bounded batch of Episode normalized content |
| Fact / Entity / relationship embedding | bounded batch of Fact, Entity or RELATES_TO content |
| claim extraction | one Episode's normalized content |
| payload-section extraction | decoded text sections ≤ 64 KiB each; local by default, separately opted in for remote |
| Entity / Fact judge | extracted claims plus at most 64 Entity and 128 Fact candidate snippets and their IDs/times, in their stored source language |
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
| embed backfill | 1 per bounded batch, terminal-prefix coverage cursor; the committed prefix stops at the first failure | +1 only for ACTIVE coverage |
| adjudication.review / adjudication.correct | 1 bounded append-only control tx (review or correction, operator Episode, replacement Fact) | +1 only when a correction creates ACTIVE structural output |
| embedding.retry / skip / cancel / qualify | 1 bounded control tx per operation; cancel takes the write-queue barrier | +1 only when a skip releases ACTIVE coverage |
| recall impression | 1 bounded control transaction before response publication, under policy barrier | — |
| policy.set / policy.revoke | 1 (immutable control Episode, active-policy publication, contiguous cursor no-ops) | policy_revision +1 for effective change; structure unchanged |
| policy reconciliation | bounded serving-view rebuild; preserve invalidation evidence/markers, no new extraction judgment | hidden work —; activation +1 only after validity-preservation gate |
| standalone commit / exposure | 1 (feedback acceptance + Episode Hits + appropriate adoption/utility caches); revalidate policy | — |
| re_mention / promotion | joins the owning extraction/dreaming write tx | Hit portion — |
| generation build/catch-up | bounded write tx per Episode | — while hidden |
| generation switch / caught-up rollback | one cutover tx under write queue | +1 |
| maintenance | 1 per batch (cache SET) | — |
| dreaming phase | ≥ 1 per phase, phase discarded as a unit on failure | +1 only for active-view structural output or selector switch |
| gc | 1 per batch | —; operational/active generations are refused |
