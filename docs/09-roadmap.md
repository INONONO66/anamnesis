# 09 — Roadmap

Three stages. Each stage must be a usable product on its own, and the next
stage inherits the previous one's data without migration — the originals
layer and the Hit ledger are fixed in v0.1 and never change afterwards.

```text
  v0.1  originals + forgetting + search      "a search engine that forgets"
  v0.2  derived layer + time + local PPR     "memory that follows relationships"
  v0.3  dreaming + validation + scale        "memory that organizes itself"
```

## Where the code is today

The repository at the time of this finalization (baseline bc372ca) holds
`packages/protocol`, `packages/core` and `packages/backfill`. What runs:
an originals write path into Neo4j, a Lucene fulltext index over Element
content (`element_content`, cjk analyzer), a `remember` that stores Episodes
and a `recall` that is fulltext search over them, plus source adapters for
backfill. Nothing else in this document set exists in code yet: no vector
channel, no Hit ledger or forgetting, no RecallReceipt, no utility, no
policy RPCs, no budget, no derived layer, no PPR.

Everything below is the normative target, not a description of the code.
Wherever the code at the baseline disagrees with these documents, v0.1
brings it in line: the docs lead. The gap is established by reading the
baseline against docs/01 and docs/02, not by the list in this paragraph.

One boundary is deliberately narrower than "bring it in line". The Episode
digest that ships today is D49's version 1, and v0.1 keeps it that way for
every already-stored row. Adding the version-2 digest means adding the
server-selected `episode_digest_version` discriminator and a dispatching
verifier beside the frozen version-1 serializer, so verify, retry, journal
replay, backup/restore and rebuild all run write-free over legacy rows. No
Episode is reserialized, relabeled or given lineage in place, and there is no
data migration in any stage (docs/01 §1, docs/08).

## v0.1 — originals, forgetting, search

**Goal**: Episodes go in and come out; what a caller confirms it adopted is
reinforced, and everything else is forgotten on its own clock. Merely being
returned is not adoption: exposure is audit-only and moves nothing (D45). No
derived layer, no PPR.

| Area | Contents | Docs |
|---|---|---|
| storage | Neo4j compose, schema, version-dispatched Episode digest (frozen version-1 body for stored rows, version 2 for new admissions, zero SETs either way), global `ingest_seq`, OriginHead CAS, `:Element:Episode`, Payload metadata + `objects/`, originals links, rebuildable event-time NEXT_EPISODE topology | 01 §1–2, §6–8; 02 §3 |
| daemon | `anamnesisd` UDS JSON-RPC, bounded object upload, write queue, serving revision, pure-Node nonce/heartbeat singleton lease, core RPCs | 02 §1–3 |
| security | 0700/0600 modes, UDS capability token, length-prefixed frame and global resource caps, bolt on 127.0.0.1 only, per-install random password | 02 §10 |
| spool | fsync-before-ack, `.done` after commit, drain, retention, cold-start wait | 02 §4, §9 |
| provenance | authenticated `origin_role` / `lineage_mode`, `EchoLineage` control row written in the Episode transaction, bounded parent receipts, roots and depth, `unknown` lineage never presumed independent | 01 §3.3, D49 |
| embedding | `embed_episode` Outbox worker, bounded batches, three-part embedding identity (`embedding_model_id`, `vector_index_id`, `embedding_profile_id`), per-entry state machine with bounded retry, terminal-prefix coverage, authenticated retry/skip/cancel and append-only resolution records | 01 §4, 02 §3, D51 |
| durability | write ordering, `gc --objects` safety, `anamnesis backup` / `restore`, `verify` | 01 §9 |
| time | Episode `time_*`, `ingested_at`, snapshot(T) filter (Episodes only) | 03 §1, §3 |
| forgetting | m₀, hit-cache initialization, R(t,S), Hit node + HIT_OF, S update, replay, `rebuild --hit-cache` | 04 §1–5, §7 |
| commit path | `commitHits` with producers 1 (receipt `recall_hit` + `outcome`) and 2 (exposure); only `recall_hit` moves `S`/`t_last_hit`, the rest audit-only; `hello.commit_mode`; idem_key | 04 §5–6, 05 §10 |
| receipts | append-only RecallReceipt control records (delivered IDs, snapshots, versions, channels, ranking state, digests); exact-once adoption per source and outcome per recall; explicit TTL, expired commit rejected, absence never a label | 05, D45 |
| utility | per-Episode `U` from rank-weighted outcome attribution (`Σ w_e = 1`), rebuildable utility cache, `(1 + β·U)` in score; zero and missing distinct | 04, D45 |
| policy | `policy.set` / `policy.revoke` RPCs, `anamnesis.memory-policy/1` Episodes, active-policy cache rebuild, policy-revision barrier, `content` scope over raw text, control Episodes excluded from search, structured-selector limit reported | 02, D43 |
| budget | `budget {unit, limit, tokenizer_id?}`; exact byte and scalar counts; pinned tokenizer or reject; greedy whole-bundle packing over deterministic `context_text`; `used_budget`; hard response byte cap retained | 05, D44 |
| recall | vector (nodes) + BM25 + session channels with caps, RRF, score = rel·max(m,.02)^γ·(1+β·U), policy hard filter, deterministic ordering, degradation ladder, diagnostics | 05 (without PPR, identity, relationship vectors, conflict bundles) |
| clients | ops CLI (`up/down/status/verify/backup/restore`), RPC contract export (`schemas/`); remember/recall harnesses are external (D39) | 08 |
| protocol | zod: Episode-only time requirement, `source_revision`, `previous_revision_key`, `revision_key`, `correction` schema, Hit, RPC methods | 08 |
| CI | forgetting fixtures, utility attribution fixtures, budget exactness fixtures, policy suppression fixtures, receipt exact-once fixtures, RRF invariance, ordering conventions, contract schema tests | 04 §10, 07 §6, 08 |

**Exit criteria**: ingest one month of the author's own conversation logs →
recall p50 < 50 ms (no PPR) → after 100 receipt adoptions, `verify` shows
ledger ↔ cache agreement for both the hit cache and the utility cache → 50
remembers with Neo4j killed → drain verified after recovery → `backup` then
`restore` into an empty directory passes `verify --scope all`.

**Gates that must hold before v0.1 ships** (each is a fixture, not a
reading):

- policy: after `policy.set` with `content` scope, the next ordinary recall
  returns nothing from the matched Episode text and the receipt carries the
  new policy revision; a recall that straddles the barrier is retried or
  rejected, never served torn; `policy.revoke` restores serving without
  fabricating anything that was never extracted; `rebuild` reproduces the
  active policy from the policy Episodes alone.
- budget: `used_budget` equals the exact byte, scalar or pinned-token count
  of the returned `context_text`; an unknown `tokenizer_id` is rejected; a
  bundle that doesn't fit is skipped and a later one may still be included;
  `limit: 0` yields empty results; the receipt's primary IDs equal the
  included primaries.
- receipts and utility: a second adoption of the same source under the same
  recall is a no-op, a conflicting second outcome is rejected, an expired
  receipt rejects commit, and `S`/`t_last_hit` are bit-identical before and
  after an outcome-only commit.
- lineage: an assistant Episode declaring receipts writes a lineage row whose
  roots and depth match its parents, overflow past 16 roots or depth 8 sets
  `complete=false` and `unknown`, and an echoed claim leaves `S`, `m`, utility
  and ranking bit-identical to the no-echo case.
- embedding failure: a deterministic failure (`context_overflow`,
  `wrong_dimension`, `malformed_response`, `client_error`) blocks the head
  immediately, a lost worker lease closes its attempt `worker_lost`, three transient failures block
  it after the fixed `[1000, 10000]` ms delays, the coverage cursor does not
  move past the hole, and no later entry publishes a vector across it; an
  authenticated retry or skip is the only way forward and each writes its
  resolution record.

## v0.2 — derived layer, time, local PPR

**Goal**: Facts and Entities are extracted, corrections follow time, and PPR
retrieves along relationships.

| Area | Contents | Docs |
|---|---|---|
| derived layer | `:Fact`, `:Entity`, physical generation labels/indexes, global `ingest_seq`, BUILDING/ACTIVE/CATCHING_UP/INACTIVE/RETIRED lifecycle, strict sequencer, dual-tail Outbox, atomic cutover and caught-up rollback | 01 §1, §4–5 |
| extraction | target sequencer: claim LLM (self-contained content, required `content_language`, modality, confidence as source-faithfulness, literal `evidence_quote` and derived span) → bounded generation-index reads → judge LLM → revalidated write with quote and UTF-8 boundary checks; explicit same-speaker/same-time/same-scope self-correction only, otherwise CONTRASTS; one immutable Fact per source occurrence, no "duplicate → no Fact"; policy applied before any derived write; blocked-head retry, entity/fact identity, correction context, embed stage | 02 §5, §5.1, D46, D47, D48 |
| grouping | bounded `speaker_key` / `subject_keys` / `predicate_text` / closed `scope` / `time_key` fields, `anamnesis.duplicate-group/1` key, irreflexive symmetric non-transitive `known_conflict`; local comparison only, never a global identity claim | 02 §5.3, 05 §6, D49 |
| adjudication | shadow-mode proposals with immutable attempt/proposal/review/consumption records, single-use acceptance revalidated at W1, authenticated `adjudication.correct` with a CREATE-only operator Episode and append-only replacement under the docs/03 §5 protocol, `operator_corrected` provenance | 02 §5.2, 03 §5, D50 |
| commit path | producer 3 (re_mention), audit-only for accessibility | 04 §6 |
| maintenance | hourly job: `m_cache`, hub shortlist; ConductingArc retained-graph rebuild/verify and complete generation publication. **Precedes PPR** — no missing-cache native fallback | 01 §5, 02 §6 |
| time | Fact time, derived visibility for Entity and Link, non-recursive valid(T), replacement protocol, provenance exception | 03 §3–5 |
| forgetting | Fact mass `m = m₀ · max_e R(age_e, s_e · σ_fact)` over bounded sources, σ_fact a dimensionless stability multiplier; κ conservation and merging, sources resolution | 04 §3, §5 |
| envelope | budgets, fanout, ConductingArc unique/RANGE (source_id, link_id) first-256 ordered probe before filters; atomic endpoint-row maintenance on physical create/delete/rewire/GC; complete coverage or PPR absent; non-hubs resolve captured rows via per-role link-ID indexes, never re-expand adjacency; indexed HubArc shortlist, stale-link verification, per-row arc cap, row-total initialization, 3-query tx, deadline, retained-row normalization | 01 §5, 06 §1–4 |
| PPR | CSR, iteration, convergence cap, determinism conventions | 06 §5–7 |
| recall | relationship vector channel (`queryRelationships`), seeds (hub damping on the capped probe degree), PPR list in fusion, valid filter, provenance/supersedes, bounded conflict bundles (raw CONTRASTS scan ≤ 64 rows + sentinel, per-row policy/validity checks, first 4 eligible as companions; `conflict_total` exact only when the scan exhausted and count ≤ 4, else `null`; `conflict_truncated` when eligible > 4 or raw rows remain; mandatory incomplete and redacted warnings; no automatic winner), occurrence grouping at assembly with explicit occurrence IDs, budget counts companion text, torn retry | 05 §2–7, D46 |
| policy | derived-scope suppression through conduction and provenance text; background reconciliation rebuilds indexes and caches without touching originals | 02, 05, D43 |
| GDS | disposable validation container, σ-node solver validation, L1/top-k/NDCG CI | 07 §1–2 |
| gc | RETIRED-generation-only `gc --derived`, protected rollback targets, `gc --embedding` | 01 §4 |

**Exit criteria**: extraction runs on v0.1 data with no downtime → 20 solver
validations pass → correction scenario fixtures (change / correction /
replacement) pass snapshot queries → a hub Entity with degree > 256 is
expanded through its shortlist in a recall → recall p50 < 100 ms.
ConductingArc fixtures must prove ordered index LIMIT before filtering,
both-endpoint/parallel/five-role coverage, atomic rebuild/rewire/GC, stale-row
exclusion and whole-PPR degradation on missing/incomplete cache or indexes;
no native adjacency alternative is an acceptable implementation (docs/07).

## v0.3 — dreaming, validation, scale

**Goal**: the graph builds its own topics, and the truncation quality is
measured.

| Area | Contents | Docs |
|---|---|---|
| dreaming | bounded ID/arc export → disposable networkless GDS Leiden → pinned community generation, exact-support synthesis, profile cache | 02 §7 |
| commit path | producer 4 (promotion) | 04 §6 |
| derived layer | `:Community`, cross-stream-compatible HAS_MEMBER, extraction-cutover disable/rebuild rule, majority visibility, Community mass | 01 §4, 03 §3, 04 §3 |
| recall | identity channel, `entities` block | 05 §2 |
| embedding | profile swap procedure (new property and indexes, backfill, current-watermark barrier, switch, gc), `EmbeddingQualification` records and the production-activation block | 01 §4, D51 |
| qualification (trigger-gated) | production multilingual, extractor, judge and index qualification packages: each ships only when its own machine-validated manifests and predeclared thresholds exist. Until then the v0.2 defaults stay development-scoped and reversible | 07, D48, D50, D51 |
| GDS | envelope validation overlap@20, 100k/1M scale benches, health report | 07 §3–5 |
| calibration | receipts and outcome events → refit DECAY, FACTOR, a, b, c, γ, β, ν, μ₀, σ_fact, modality and sub_kind priors, role weights, RRF weights; ablate each default; prior changes ship as a new derived generation with a recorded prior version; config version tags | 04 §9, D47 |

**Exit criteria**: on a 1M synthetic graph p50 < 100 ms / p95 < 250 ms,
envelope deadline exceeded < 1 %, overlap@20 ≥ 0.8, dreaming on 1M in
< 30 minutes.

## Not doing

- Multi-user, auth beyond the OS user, remote bolt. Personal, localhost.
- A second graph store outside Neo4j (SQLite, custom format). The previous
  roadmap is retired ([10-decision-log](10-decision-log.md) D0).
- LLM calls on the recall path. Summarization and re-ranking belong to the
  caller.
- Transaction time / bitemporal (03 §6).
- Recursive INVALIDATES (03 §4).
- Tick- or schedule-based forgetting updates (04).
- Per-link weights (D24).
- `gc --erase`, or any erasure guarantee. Policy suppresses extraction and
  serving; originals, backups and privileged raw operator access stay
  outside it (D43).
- Automatic execution of instructions found in ingested text. Policy is an
  authenticated RPC command only (D43).
- Token estimates. A `tokens` budget without a pinned installed tokenizer is
  rejected, never approximated from bytes (D44).
- Per-Fact accessibility. Hits attach to Episodes; sibling Facts of an
  adopted source refresh together (D45).
- A stability penalty. `S` only rises; outcome feeds `U` (D45, supersedes
  the D40 negative branch).
- Automatic contradiction winners by confidence or recency (D46).
- Automatic query translation or two-query fusion on the recall path. The
  caller's query is used verbatim; the offline comparison behind that default
  is in [research/retrieval-fusion](research/retrieval-fusion.md) (D48).
- English-normalized canonical Facts. `fact_language_policy` is a per-
  generation configuration, and generated English projections are neither
  persisted nor indexed under this contract (D48).
- Corroboration by repetition. Occurrence count, root count and echo depth
  never move confidence, mass, utility, ranking or a conflict winner (D49).
- Unattended automatic invalidation. Adjudication ships in shadow mode, and
  acceptance is an operator decision, not human gold (D50).
- Production embedding activation on ONLINE status or operator attestation
  alone. It stays disabled until a qualification carries machine-validated
  manifests and predeclared thresholds (D51).
- Truncating, chunking or zero-filling an embedding input to move a coverage
  cursor (D51).
