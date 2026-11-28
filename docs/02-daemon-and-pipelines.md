# 02 — Daemon and Pipelines

Exactly one process writes. `anamnesisd` is Neo4j's only bolt client. Callers
talk to it over a UDS, or over the optional authenticated TCP listener
described in [deploy](deploy.md). Harnesses are external projects; this repo
ships the daemon, its ops CLI and the RPC contract (D39).

```text
  external harnesses ─┐  (separate repos, attach over the RPC contract)
  anamnesis ops CLI ──┼─ UDS <runtime root>/anamnesis.sock ─▶ anamnesisd ─ bolt(127.0.0.1) ─▶ Neo4j (Docker)
                      ┘                             │
                                                  ├─ objects/
                                                  ├─ write queue (serialized)
                                                  ├─ read pool  (recall, concurrent)
                                                  ├─ extraction scheduler (coverage scan, extraction-state.json)
                                                  ├─ embedding lane (missing-vector scan, embedding-state.json)
                                                  └─ identity lane (D59)
```

Starting, stopping and supervising the daemon and the Neo4j container are
operator actions (`anamnesis-ops up | down | foreground | managed`, systemd
units); see [deploy](deploy.md). One daemon owns a runtime root at a time.

The write fence is in Neo4j. Before serving, a daemon increments
`Meta.writer_epoch` in a transaction and records the returned epoch. Every
later write transaction first locks Meta and requires that exact epoch. If an
old daemon already holds that lock, takeover waits for its transaction to
resolve before incrementing; if it hasn't locked Meta, its later epoch
predicate fails with `ownership_lost`. No transaction from the old epoch can
commit after the new daemon starts serving (D37). Restore activation fences
the live writer before any root rename (docs/01 §9).

## 1. Write serialization

Every write inside the daemon goes through one queue. Neo4j tolerates
concurrent write transactions; what we want is the guarantee that "writes have
a global queue order". `ingest_seq` orders Episode creation;
`structure_revision` advances only when that order changes the serving view.

### structure_revision

A single integer on `(:Meta {key: 'meta', structure_revision})`.
**Incremented on:**

- semantic original Element/Link or session-topology CREATE/DELETE
- Element or Link CREATE in the selected extraction generation
- an extraction selector switch (cutover)

**Not incremented on:** Hit, Usage or UsageOutcome CREATE, policy events
(they use `policy_revision`), hit and utility cache SET, embedding vector
writes, extraction coverage advance, writes to `embedding-state.json` and
`extraction-state.json`, or writes to a generation that isn't selected. None
of these change the serving structure recall sees. The increment happens in
the same transaction as the visible write or selector change that caused it.

`Meta.ingest_seq` is a separate monotonic write cursor allocated in every
Episode CREATE transaction. It orders generation catch-up but is never used
as a recall consistency revision. It is allocated as late as that transaction
allows: the single `(:Meta {key:'meta'})` node is the one object every
concurrent remember must lock, and Neo4j holds a write lock until commit, so
an early increment serializes unrelated remembers for their whole duration.
Allocating inside the transaction keeps the sequence gapless: an
aborted remember (`revision_conflict`, `stale_revision`) consumes no number,
which the coverage cursor (docs/01 §4) depends on. `Meta.key` is unique, so remembers racing on a cold
database cannot each MERGE their own Meta node and hand out the same number.

### Logical server time

Write transactions issue `server_time =
max(wall_clock_ms, Meta.last_server_time + 1)` and persist it on Meta. Read
requests use the maximum of wall clock, persisted time and the process-local
last issued value without writing. Forgetting still clamps every elapsed
interval to nonnegative (docs/04), so imported old Hits and wall-clock
regression cannot produce `R>1` or lower stability.

### policy_revision and the publication barrier (D43)

`Meta.policy_revision` is separate from structural consistency. A policy
command appends its `PolicyEvent` and increments this revision in one
transaction under the Meta write barrier. Recall takes the same barrier for
its policy read, candidate reads, cached dynamics, packing and Usage creation,
so a response is never assembled under two policies. Once a policy command
acknowledges, no later response is published under its predecessor.
Already-delivered bytes can't be withdrawn.

Extraction, embedding recovery and `report_outcome` revalidate current policy
before they commit. A report about a prior Usage is refused with
`policy_denied` if any credited source is now denied, and no Hit is written.
Unavailable policy authority means retryable `policy_unavailable`, never an
empty-policy fallback.

`structure_revision` alone is not a replay token. Index, cache, policy,
generation and channel state determine the numeric result. Usage captures the
ordered sources of included primaries (D62).

## 2. RPC

JSON-RPC 2.0 over UDS. Each message is `[u32be byte_length][UTF-8 JSON]`.
The daemon reads the four-byte length first and closes the connection if it
exceeds the cap, before allocating a body buffer. The contract is defined with
zod in `packages/protocol`; server and clients share the same schema.

The method list is `RPC_METHODS` in `packages/protocol/src/rpc.ts`. `hello`
returns the same list in `capabilities.methods`.

| Method | Writes | Meaning |
|---|---|---|
| `hello {client, version:2, token}` | none | Authenticate the connection as the installation principal and receive capabilities and limits |
| `status` | none | Storage state, revisions, selected generation, worker states and embedding backlog |
| `shutdown` | none | Stop the daemon |
| `object.begin` / `object.chunk` / `object.commit` | objects/ | Bounded payload upload (below) |
| `remember {episode, source_revision, expected_previous_revision_key, payload_hash?, origin_role?, lineage_mode?, parent_recall_ids?}` | yes | Ingest one Episode. Idempotent (§3) |
| `remember.batch {deliveries[1..32]}` | yes | `remember` for each delivery in order, in one serial turn, one result per delivery. A non-retryable refusal (`{state: refused, code, message}`) answers that delivery alone. Any other failure fails the request at that delivery, so a later delivery is never admitted before an earlier one, and the deliveries already admitted answer as stored when sent again |
| `recall {query, session?, T?, limit?, window?, budget?}` | Usage | Serve one recall and store its ordered selection ([05-recall](05-recall.md)) |
| `report_outcome {usage_id, idempotency_key, reward}` | UsageOutcome, Hit | Revalidate current policy, then accept or replay the clamped reward (docs/04 §6) |
| `policy.set {policy_id, selector, scope}` / `policy.revoke {policy_id}` | PolicyEvent | Deny or revoke (below) |
| `projection {T?}` | none | Read-only Entity and Fact snapshot for `anamnesis-ops projection write` (docs/01 §4) |
| `graph.envelope {seed_ids[1..128], T?}` | none | Bounded ConductingArc diagnostic read ([06-envelope-ppr](06-envelope-ppr.md)). Recall does not call it |
| `extraction.audit.create` / `.run` / `.status` | journal, derived | On-demand entry into the extraction pipeline (§5) |
| `embedding.recover` / `embedding.requeue` / `embedding.status` | vector, ledger | Embedding recovery (below) |
| `hit-cache.verify` / `hit-cache.rebuild` | HitCache | Compare or replay the Hit ledger against the cache |
| `identity.status` / `.rebuild` / `.correct` / `.directory`, `people.list` / `.show` | identity layer | Identity lane control and reads (§5.5) |
| `spaces.import` / `.list` / `.rebuild`, `replies.rebuild` | labels, REPLIES_TO | Room and reply caches (docs/01 §5) |
| `backup` / `restore`, `backup.status` / `restore.status` | archive | Journaled operations (docs/01 §9) |

Every response carries `structure_revision`, `policy_revision` and
`server_time`; the revisions are null when storage is unavailable. Error codes
are the closed `RpcErrorCode` enum, each with a `retryable` flag. JSON-RPC
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
empty `results` and `context_text`; ranks and the Usage selection include only
actually included primaries. No budget uses the configured server output cap. The
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
    "selector": {"source": "slack"},
    "scope": "content"
  }
}
```

`policy.revoke` takes `{policy_id}`. Both need a successful `hello`. The
selector holds `episode_id`, `source` or both, and `scope` is `content`; the
schema rejects anything else, including an empty selector. The result echoes
the command with `action`, `applied`, the effective `policy_revision` and
`evaluator: "episode-source-v1"`. A repeated identical command is a no-op, an
unknown revoke target is `unknown_policy`, and Neo4j unavailable means
retryable `storage_unavailable` before anything is accepted. The policy rules
are in docs/01 §3.2.

### Embedding recovery commands (D54)

`embedding.recover {operation_id, fact_id}` retries one Fact under a fresh
UUIDv7 operation. `embedding.requeue {limit ≤ 1000, reasons?}` drops
quarantined ledger entries so the scan finds those Facts again.
`embedding.status {operation_id}` reports one attempt. Reusing an operation
ID for a different Fact or profile is `idempotency_conflict`. Without a
configured provider they answer `embedding_not_configured`. The contract is
in [embedding-recall](../app/anamnesis/embedding-recall.md).

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
`upload_id` is unguessable and bound to the authenticated connection. Hashes accepted
from RPC or restore input must match `^[0-9a-f]{64}$` before path
construction. There is no client-side filesystem write path.
Startup charges surviving temp files to the global quota before accepting an
upload. Data without a committed sidecar is an orphan; a sidecar without
matching data is corruption. Short writes/ENOSPC roll the temp back to its
previous verified length and return `resource_exhausted` before ack.

## 3. Hot path — remember

Metadata-free `remember` keeps the pre-lineage contract. Supplying any
role or lineage field selects the authenticated lineage path, which needs all
three admission fields for a new revision. An existing stored row's digest
version always wins before metadata or parents are validated. Parent custody
is the server-issued hello-connection binding, not the client label. The
admission contract is in
[episode-lineage](../app/anamnesis/episode-lineage.md).

```text
  remember(episode, payload_hash?)
    1. Contract validation (the two Episode schemas, origin, time, size caps)
       origin_key   = sha256(source, session, actor, record)
       session_key  = sha256(source, session)
       source_revision = adapter-issued stable revision token
       payload_hash = committed object hash if present
       revision_key = sha256(origin_key, source_revision)
       Do not resolve role/lineage or read parent Usage here. Step 3a
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
                       parent Usage, even if the parent is gone. Any changed
                       version-1 field → reject revision_conflict. Newly
                       supplied origin_role or lineage values sit outside
                       version-1 identity: they create no lineage properties, repair
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
                       Usage bound to this authenticated caller.
                       Resolve the bounded Episode lineage body under docs/01 §3.3;
                       lineage_digest = sha256(RFC-8785 canonical body).
                       Set server-selected episode_digest_version = 2 and
                       digest = sha256(RFC-8785 canonical JSON of
                         {episode_digest_version: 2, schema, content, properties,
                          time, payload_hash, previous_revision_key, origin_role,
                          lineage_digest}).
                       Only then CREATE below.
         b. Under the Meta writer fence, find the newest Episode for
            origin_key using episode_origin_head (ingest_seq DESC)
              no head + previous=null         → CREATE first Episode
              head=previous_revision_key      → CREATE new Episode,
                                                 new -[:INVALIDATES]-> previous
              otherwise                       → reject stale_revision
         c. MERGE Payload metadata, HAS_PAYLOAD
         d. update the rebuildable session topology around this Episode
            in total order (time_utc ASC, ingest_seq ASC)
         e. Initialize hit cache: s = S0(m0), t_last_hit = server_time, hit_count = 0   (docs/04 §2)
         f. increment Meta.ingest_seq and assign it to the new Episode.
            Every concurrent remember contends for this one node's write lock,
            which Neo4j holds until commit, so the increment rides on the last
            statement that nothing else in the transaction depends on
         f2. SET lineage properties on this newly created version-2
            Episode (docs/01 §3.3); an existing version-1 Episode never reaches
            this step:
            copy each parent Usage's bounded delivered snapshot, pair the
            parent IDs with their selection digests, sort by recall ID, store
            the sorted distinct root union and 1 + max(item.echo_depth).
            Every parent's created_at must precede this ingestion. Either cap
            overflowing sets lineage_complete=false; an exact retry of the
            same body is a no-op and a changed body is revision_conflict
         g. structure_revision += 1
    4. Return {id, created: bool, ingest_seq, structure_revision}
```

`origin_key` identifies the logical source and is shared by all revisions.
`source_revision` identifies an occurrence and must be reused on retry; mutable
source adapters must issue a new token even when content reverts from A→B→A.
Append-only adapters use their immutable record ID as both `record` and
`source_revision`. `revision_key` is unique, while `digest` detects an adapter
that mutates one token; the stored `episode_digest_version` decides which body
that comparison uses, and an existing row's version always wins over the
version a new request would have created (docs/01 §1).
The origin head is the newest Episode by indexed `ingest_seq` under the
Meta writer fence, not a separate CAS cache.

remember calls no LLM. It does not embed either — ingestion must succeed even
when the embedding service is down. The extraction scheduler materializes Facts;
the embedding worker scans all generations for Facts without a vector of the
configured profile. A turn on the daemon's serial owner only finds the next
batch of up to 32 Facts; their provider calls and commits run outside the turn,
so no RPC waits for the provider, and the batch's end wakes the worker to scan
again. Its attempt and
quarantine state is in `embedding-state.json` (version 2, keyed by Fact ID),
not graph Outbox rows. It sends Fact content, checks its digest and revision,
and authorizes its source Episodes through `DERIVED_FROM`. A transient failure
defers with bounded backoff; a deterministic failure quarantines that Fact.
BM25 and session recall remain available, and the Fact vector channel searches
only the selected generation. Activation requires vector coverage of every
materialized Fact of the target generation; zero Facts are covered.

### Revisions vs corrections

A new `source_revision` arriving under the same `origin_key` is a *revision*
(document edit, message edit), names its `previous_revision_key`, and is
expressed as INVALIDATES between Episodes. A user's correction utterance is
different: it is an ordinary new Episode with a new `origin_key`, and its
meaning appears at extraction time as INVALIDATES between Facts
([03-time](03-time.md) §5).

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
its `structure_revision` increment are atomic.

## 4. Ingestion when Neo4j is unavailable

`remember` and `remember.batch` write to Neo4j during the request. If storage
is unavailable, the RPC returns retryable `storage_unavailable` and admits
nothing for the failed write. Clients retain the input and retry. If a batch
loses storage after committing an earlier item, its committed prefix remains;
no later item is attempted. Retrying the same body under a committed
`revision_key` returns the existing Episode with `created: false`. The writer
checks the body against the retained row and refuses a different body with
`revision_conflict`. This also resolves an uncertain transport outcome.

Object uploads remain independent of Neo4j. Startup, status, and remember can
recheck storage; a bounded recovery probe wakes the identity, embedding, and
extraction workers when storage returns.

On upgrade, startup ignores an absent or empty legacy `spool/`. A nonempty
legacy directory may contain unapplied acknowledged writes: startup fails with
its path and leaves it untouched for recovery before upgrading. An absent or
empty legacy `deliveries/` is also fine; leftover pre-commit reservation files
may be removed at startup with a log event. They were never acknowledgements.

While Neo4j is unavailable, recall returns retryable `storage_unavailable`
([05-recall](05-recall.md) §8).

## 5. Cold path — extraction worker

The extraction scheduler (D55) finds its work
by what coverage has not sealed: Episodes whose `ingest_seq` is above the
generation's `ExtractionCoverage.covered_ingest_seq`, read in a bounded
window in sequence order. There is no extraction queue label in the graph.
`ANAMNESIS_EXTRACTION_LANE=off` leaves the shared LLM provider configured but
does not create the scheduler. The daemon then advertises
`capabilities.extraction:false`, reports `workers.extraction` as
`{"state":"unconfigured"}`, and never wakes this lane. The default is `on`.
The pipeline a scan resolves for an Episode (claim task, lease, attempts,
judge task and input, up to 64 decisions) lives in the
`extraction-state.json` journal under the runtime root, keyed by
`pipeline_id` and looked up by work key `<generation_id>:<source_id>`.
Leases, CAS on `expected_version`, retry and settle rules, replay of
identical completions and the `stale_input`, `lease_expired`,
`coverage_frozen`, `policy_denied`, `span_mismatch` and `provider_mismatch`
fences are enforced against the journal entry inside the same transaction
that still reads the Episode head, policy and writer epoch from the graph.
Source text leaves the database only after lease and policy checks. A
pipeline whose judge attempt succeeded materializes exactly as described
below; materialization is idempotent through custody occurrence keys and
Fact digest uniqueness. Coverage seals a source only once its pipeline is
terminal (custody written, or a content-free omission for a failed or
cancelled stage); the sealed entry is then pruned from the journal,
`extraction.audit.status` answers `unknown` for it and a retry of one of
its tasks fails with `unknown_ModelTask` ahead of the `coverage_frozen`
fence. Every transition emits
one structured audit line (`extraction.task.leased` before the provider
call, `extraction.attempt.recorded`, `extraction.task.settled`,
`extraction.pipeline.materialized`, `extraction.pipeline.pruned` and the
rest), which is the durable record. A daemon lost between lease and attempt
leaves a `leased` task in the file; the next scan finds the still-uncovered
Episode, settles the task as `worker_lost` or `expired`, retries within the
lost-lease budget and re-runs it.

### 5.4 Windowed extraction and the Jev gate (D56)

Queued claim work is grouped by `session_key` (`sha256(origin_source,
origin_session)`, so two adapters reusing one session string never share a
group) and split in ingest order into near-equal windows, with `ceil(n / 8)`
windows for a group of `n` Episodes. A window reads at most three preceding
Episodes with the same `session_key` as context; context is read-only and is
never a claim source. One
`claim_window` call produces per-Episode claims, while each Episode retains
its own custody entry, extraction attempt, pipeline and `DERIVED_FROM` edges.
The judge and relation stages remain per Episode. A judge over zero claims is
answered locally with no decisions, the only valid answer, without a provider
call.

With an extraction scope configured (`ANAMNESIS_EXTRACTION_SCOPE_FILE`: listed
sources in full, plus listed rooms of other sources and their thread sessions),
a window outside it is cancelled member by member with `out_of_scope` before
the gate runs, without reading text. These omissions increase
`out_of_scope_total`. Coverage is one-way, so a room listed after extraction
passed its Episodes is not extracted in that generation.

When configured, the Jev pre-gate scores one window. If its maximum score is
below the threshold, each member is cancelled with `gate_skipped` and the
journal entry records the model, score, window score, threshold and decision
time. These omissions increase `gated_total`, not `failed_total`. A gate
failure never blocks extraction: the window proceeds without skipping.
Restart and lease recovery semantics are unchanged; a lost claim-window
lease is settled and retried through the normal scheduler.

After the claim and judge stages, a pipeline materializes in one write
transaction that revalidates its premises: the source head, the policy
revision and the parent attempt binding. A changed premise leaves a
content-free failure and the scheduler retries. A denied source leaves a
content-free cancelled attempt and writes nothing. Facts go through the
validated semantic path only (D52), each with 1 to 16 source Episode IDs and
matching direct `DERIVED_FROM` links (docs/01 §1). The relation judge (D53)
gates every Fact→Fact relation. Its candidates are the selected generation's
Facts that share an Entity, unioned with the top-8 `vec_fact` neighbours, at
most 16 after deduplication with entity-shared first. When the vector channel
is unavailable the judge runs entity-only and records the reason
(`provider_absent`, `profile_absent`, `embed_failed`, `query_failed`).
Extraction creates no Hits. An assistant Episode with unknown or truncated
lineage produces no semantic output (`echo_lineage_unavailable`,
docs/01 §3.3). The provider ABI, journal and audit lines are in
[extraction-audit](../app/anamnesis/extraction-audit.md).

Evidence is a span, not a model offset. The model returns a quote; the adapter
locates it in the immutable Episode content and computes the `[start, end)`
UTF-8 byte span. A quote that isn't a literal substring drops its claim.
Using normalized stored content:

```json
{"content":"Aé🙂Z","span":[1,7]}
```

The content is 8 UTF-8 bytes and 4 Unicode scalars; `[1,7)` selects `é🙂`.
`[2,7)` splits `é`, `[1,6)` splits the emoji, `[1,1)` is empty, and `[0,9)`
exceeds the source: all are rejected. A valid span proves byte boundaries
only, not that the text supports the claim.

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
Policies
and temporal validity are hard filters, not confidence discounts. Changing
priors produces a new generation; Hit replay never changes immutable `m0`.
An `intended` claim later fulfilled is a new `asserted` Fact whose judge
outcome is `elaboration` (RELATES_TO the intent), not a contradiction — the
plan was true as a plan. What a caller does with `hedged` or `intended` results
(filter, phrase, discount) is the caller's business; the engine's job is to
not lose the distinction.

### 5.2 Fact write authority (D60)

Only the extraction pipeline materializes production Facts. The mechanical
relation decisions in D53 remain part of that pipeline.

### 5.3 Bounded speaker and scope fields (D49)

Same-speaker and same-scope are decided from bounded stored fields, never
from prose similarity:

| Field | Meaning |
|---|---|
| `speaker_key` | Stable authenticated speaker identifier inside one `(origin_source, origin_actor)` namespace |
| `subject_keys` | 1..16 sorted resolved Entity IDs, or null. No literal, normalized-string or display-name fallback exists |
| `predicate_text` | NFC case-preserving source-language relation, 1..256 scalars |
| `scope`, `scope_complete` | Closed bounded scope object (docs/01 §1) and whether every component resolved |
| `modality` | The §5.1 enum |

Two Facts have the same speaker only when both `speaker_key` values are
non-null and equal. A display name, alias, pronoun, fuzzy match or shared
account never implies speaker equality. An unresolved subject stores
`subject_keys = null`; falling back to literal text would assert the global
equivalence D49 refuses. The fields are stored and validated. Nothing computes
a duplicate-group key from them; occurrence grouping is a
[09-roadmap](09-roadmap.md) target.

### 5.5 Identity lane (D59)

The identity lane maintains the identity layer (docs/01 §4). It is ready only
while no foreground request is queued, so bulk load admission has priority.
One turn performs the first applicable action:

```text
  1. store settled judge verdicts as IdentityDecision records
  2. scan one page (<= 500 Episodes) above covered_ingest_seq
  3. plan with recomputed rules and build a Person generation when stale
  4. start the first pending judge call, at most one in flight
```

A plan runs after a scan, stored verdict, directory import, operator correction
or rebuild. The `judge_identity` call uses the configured extraction provider,
the same pacer as extraction, and `prompts/judge-identity.v1.md`. It runs
outside the serial slot; a later turn stores its verdict. A settled verdict
therefore makes the scheduler return `more` until storage completes. Provider
errors leave the pair pending for a later retry. Without a judge the lane still
scans, recomputes rules and builds generations; pending judged pairs remain
separate and appear in `identity.status`.

### Idempotency

Fact, Entity and link identities include `generation` and every
meaning-bearing immutable field. Extracting the same Episode twice in one
generation makes exact retries collide as no-ops, and a new generation creates
its own output (docs/01 §1, §4). A failed or cancelled stage is sealed as a
content-free omission; it doesn't block later sources.

## 6. Maintenance

No scheduled maintenance job exists. Caches are maintained in the
transactions that write their inputs (hit cache, ConductingArc, session
topology) or rebuilt by explicit RPCs (`hit-cache.rebuild`, `spaces.rebuild`,
`replies.rebuild`, `identity.rebuild`).

## 7. Dreaming

Not implemented. No `dream` RPC, job store or GDS community lane exists; the
daemon answers `unsupported_method`. The target is listed in
[09-roadmap](09-roadmap.md).

## 8. What is not on the recall path

- No LLM calls. Candidates, seeds, PPR, RRF and assembly are deterministic
  numeric work.
- Recall creates one Usage in its serving transaction. Socket completion has no durable side effects.
- No GDS calls.

## 9. Connection states and degradation

| State | remember | recall | extraction | embedding |
|---|---|---|---|---|
| Neo4j up, providers up | normal | normal | normal | normal |
| Neo4j up, embedding provider down | normal | vector channel dropped, with `vector_reason` in diagnostics | relation judge runs entity-only | Facts defer with backoff, then quarantine |
| Neo4j up, LLM down | normal | normal | attempts fail and retry within their budget | normal |
| Neo4j down | retryable `storage_unavailable` | retryable `storage_unavailable` | paused | paused |

A successful recall always has a durable Usage, even for an empty result.
`report_outcome`, `policy.set`, `policy.revoke` and the embedding commands
return retryable `storage_unavailable` when Neo4j is unavailable, before
anything is accepted. With Neo4j available, unavailable policy authority
returns retryable `policy_unavailable`. A missing Usage returns
`unknown_usage` only after a successful store lookup. When storage returns, a
bounded recovery probe wakes the identity, embedding and extraction workers.

## 10. Security boundary

Personal, single-user, localhost. The boundary is the OS user.

| Control | Rule |
|---|---|
| data directory | runtime root mode 0700; files 0600 |
| UDS | `anamnesis.sock` mode 0600 plus the installation token (`token`, 0600), required by `hello` and compared in constant time |
| TCP | off by default. `ANAMNESIS_LISTEN` needs a bearer token file; the first frame authenticates and a mismatch is `unauthorized` ([deploy](deploy.md)) |
| control commands | Policy, embedding, identity, backup and restore operations require authenticated explicit calls. Commands are never parsed from retrieved or ingested content |
| provenance metadata | `origin_role`, `lineage_mode` and `parent_recall_ids` come from the authenticated adapter and are verified against the caller binding. Lineage is never inferred from prose (D49) |
| request caps | length-prefixed RPC frame ≤ 1 MiB; decoded chunk ≤ 512 KiB; object ≤ 64 MiB; per connection 2 uploads, 128 MiB temp and 16 queued requests; global 64 connections, 32 uploads, 1 GiB temp and 256 queued requests |
| Neo4j bind | loopback only |
| Neo4j auth | per-install credentials in `neo4j.auth` (0600). No default password |
| Neo4j plugins | GDS only in disposable validation containers ([07-gds-validation](07-gds-validation.md)); never connected to the live authority |
| LLM / embedding egress | only explicitly configured endpoints; payload classes are listed below |

### Egress payloads

No endpoint receives the database, payload bytes, credentials, the Hit ledger
or unrelated memories. Policy events and Usages never enter model requests.
Current policy is enforced before egress; already-sent remote input can't be
withdrawn. Provider responses are read under a 256 KiB cap.

| Operation | Data sent |
|---|---|
| query embedding | the caller's verbatim recall query with the profile's query prefix; no translation and no second generated query (D48) |
| Fact embedding | one Fact's content with the profile's document prefix |
| claim extraction | one window of same-session Episode content plus at most three preceding context Episodes |
| claim judge | the Episode text and its ordered claims |
| relation judge | one new Fact and at most 16 candidate Facts |
| identity judge | names, session keys, directory rows and policy-allowed samples for one account pair |

Without an embedding provider the vector channel is absent. Without an LLM
provider extraction is unconfigured. Recall itself never calls an LLM.

## 11. Transaction boundaries

| Operation | Transactions | structure_revision |
|---|---|---|
| remember | 1 (Episode, Payload, revision INVALIDATES, session topology, cache init, lineage) | +1 |
| extraction pipeline | provider calls outside any transaction; one revalidated write transaction per materialization | +1 when it writes into the selected generation |
| Fact embedding | 1 per vector | unchanged |
| recall | 1 (reads and Usage under the Meta write barrier) | unchanged |
| `report_outcome` | 1 (current-policy check, UsageOutcome, Episode Hits and caches) | unchanged |
| policy.set / policy.revoke | 1 (PolicyEvent, `policy_revision` +1 for an effective change) | unchanged |
| extraction cutover / rollback | 1 under the Meta writer fence | +1 on cutover |
