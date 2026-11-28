# 10 — Decision Log

Each entry is **decision / alternative / reason**. Entries that are current
contracts are kept in full here, with a dated **Status** line where later code
narrowed them. A superseded or historical entry keeps its heading and one line
naming what replaced it; its original text is in
[history/decision-log-archive](history/decision-log-archive.md) under the same heading. Every `Dnn`
keeps its number.

| Kept in full | Superseded or released (one line) | Historical (one line) |
|---|---|---|
| D1–D6, D10, D11, D14, D16, D19–D21, D25, D26, D28, D31, D36–D39, D41–D44, D46–D49, D52–D62 | D13, D22, D24, D27, D29, D33, D40, D45, D50 | D0, D7–D9, D12, D15, D17, D18, D23, D30, D32, D34, D35, D51 |


## D0 — main retired, anamnesis2 is the mainline, previous drafts retired

Historical: `main` and the 2026-08 drafts were retired. `anamnesis2` is the main branch (docs/08). Original text: [archive](history/decision-log-archive.md#d0--main-retired-anamnesis2-is-the-mainline-previous-drafts-retired).

## D1 — Hits attach to Episodes only

**Decision**: only `(:Hit)-[:HIT_OF]->(:Episode)`. Adoption of a derived
element is resolved to its source Episodes (docs/04 §5).

**Alternative**: the proposal — a Hit points directly at the adopted element,
Facts included.

**Reason**: derived IDs change on generation switches, re-extraction and the
replacement protocol. An immutable ledger pointing at IDs that will disappear
either resets forgetting state on every switch or needs an ID remapping table
in the ledger. Episodes are CREATE-only, so their IDs are permanent. As a side
effect, "reinforcement history carries over when A′ replaces A" comes for
free.

## D2 — mass is on the now axis, independent of snapshot(T)

**Decision**: `m(now)`. snapshot(T) decides visibility only (docs/04 §4).

**Alternative**: replay the ledger up to T for `m(T)`.

**Reason**: the two axes mean different things — snapshot is the world,
forgetting is the mind. Mixing them makes memories "just born at T" unduly
fresh in answers about the past. No question wants `m(T)`, and if one ever
does it is a single `until` argument on replay.

## D3 — INVALIDATES is non-recursive; restoration is a replacement Fact

**Decision**: `valid(x,T)` looks only at the time of the INVALIDATES sources
pointing at x. A wrongly invalidated A is restored as A′ (copy of content and
time, DERIVED_FROM A and C, INVALIDATES A) (docs/03 §4–5).

**Alternative**: recursion — the original revives when its invalidator is
invalidated.

**Reason**: recursion is the doorway to cycles, depth and rule explosion, and
a wrongly revived fact produces silent wrong answers. Replacement costs one
Fact and keeps validity a one-hop check. Its failure mode is "not visible",
which the user can fix by saying it again — safer.

## D4 — no transaction time

**Decision**: one event time. `Episode.ingested_at` is for audit and is not
used by snapshot (docs/03 §6).

**Alternative**: bitemporal.

**Reason**: users ask "what was the world like then". "What did the system
know then" is a debugging question, and generation integers, Hit.t and
ingested_at are partial substitutes. Bitemporal attaches a second interval
axis to every derived element, correction and switch.

## D5 — payload bytes live outside Neo4j; the authority is Neo4j + objects/

**Status (2026-11-29)**: the authority is unchanged. The spool this entry mentions was removed by [D61](#d61--remember-commits-directly-to-neo4j-2026-11-28).

**Decision**: content-addressed files in `~/.anamnesis/objects/` plus a
`(:Payload)` metadata node (docs/01 §2). The data authority is exactly the
Neo4j database and `objects/`. The spool is a transient queue, deleted after a
verified drain, never part of the authority (docs/01 §9). This supersedes the
earlier "single Neo4j store" wording: Neo4j is the single **graph and index**
store.

**Alternative**: the previous drafts — `(:Payload {bytes})` as base64, one
durable location.

**Reason**: as document revisions accumulate, raw text takes over the
property store and the page cache. Neo4j is a graph and index store, not a
blob store. The cost is a two-part backup. Community Edition requires an
offline dump; a fixed hash manifest lets object copying continue after the
database restarts while new remembers fsync to the live spool (docs/01 §9).
Confirmed by the owner in the 2026-09 review.

## D6 — `structure_revision` does not bump on Hit or cache writes

**Decision**: `structure_revision` is a serving-view revision: +1 on original
or active-generation structure changes and selector switches. BUILDING /
CATCHING_UP writes and RETIRED-generation GC do not bump it. `ingest_seq` is
the separate monotonic Episode cursor (docs/01 §4, 02 §1).

**Alternative**: the proposal's `recall_revision` — incremented on every
write.

**Reason**: what recall must detect is a *serving-view* change. Hits and
`m_cache` do not alter candidate membership; hidden-generation embeddings do
not serve. Active embedding coverage does alter vector results and therefore
does bump the revision, while activation waits for complete target coverage.

## D7 — fanout is derived from the budget

Historical: derived fanout for the planned two-hop online envelope. The envelope is a [09-roadmap](09-roadmap.md) target. Original text: [archive](history/decision-log-archive.md#d7--fanout-is-derived-from-the-budget).

## D8 — fanout tie-break is `id DESC`

Historical: the fanout tie-break for the planned online envelope (docs/09). Original text: [archive](history/decision-log-archive.md#d8--fanout-tie-break-is-id-desc).

## D9 — wall-clock deadlines guard Neo4j channels, never PPR

Historical: wall-clock deadlines for channels and the envelope. Recall has no per-channel timeout (docs/05 §8). Original text: [archive](history/decision-log-archive.md#d9--wall-clock-deadlines-guard-neo4j-channels-never-ppr).

## D10 — normalize retained visible rows; uniform only for dangling

**Decision**: `W_ij = w_role/Z_i`, where `Z_i` is the sum of role weights over
the retained visible links actually passed to the solver. A row with `Z_i=0`
uses the fixed uniform distribution (docs/06 §4).

**Alternative**: divide by a physical degree that includes links excluded by
snapshot, generation and envelope filters, then redistribute the missing mass.

**Reason**: the physical denominator and visible GDS projection are different
graphs. One active and one retired edge gave local `W=.5, leak=.5` while GDS
used `W=1`. Normalizing the graph actually supplied to each solver makes the
same-envelope contract exact. Boundary amplification becomes a measurable
envelope-quality cost in docs/07 rather than a hidden model mismatch.

## D11 — maxIter 64, τ 1e-4, error bound 5.7e-4

**Decision**: docs/06 §5.

**Alternative**: the proposal — 20 fixed iterations.

**Reason**: at α = 0.85, 20 iterations give a worst-case contraction of
`0.85^20 ≈ 0.039`, so a fixed 20 cannot *guarantee* a residual of 1e-4; a
particular graph may converge faster, but the bound doesn't promise it, and
the error bound is set by the final residual, not by the iteration count. 64
is a cap that guarantees τ is reached (`2·0.85^61 < 1e-4`). How many
iterations typical envelopes actually need is unmeasured; "20–30" was an
expectation, and the iteration count is a reported diagnostic to be
confirmed by the docs/07 gates. The L1 tolerance of 7e-4 in 07 comes from
this bound.

## D12 — κ conservation and merge cap

Historical: κ conservation and merge cap of the removed commit namespace. The credit rule is docs/04 §5. Original text: [archive](history/decision-log-archive.md#d12--κ-conservation-and-merge-cap).

## D13 — commit is recomputed by the server; auto/receipt modes

**Superseded** by [D62](#d62--record-recall-selection-and-one-explicit-outcome-2026-11-2829) (merged 2026-11-29): explicit `report_outcome` replaces the receipt and auto commit modes. Original text: [archive](history/decision-log-archive.md#d13--commit-is-recomputed-by-the-server-autoreceipt-modes).

## D14 — Fact mass = max over bounded source Episodes · σ_fact

**Decision**: docs/04 §3.

**Alternative**: sum, mean, or per-Fact state.

**Reason**: per-Fact state conflicts with D1. A sum inflates with the number
of sources; a mean dilutes recent reinforcement. Max is monotone and
conservative and matches the intuition "as alive as its most recently handled
source". σ_fact models the gist outliving the detail.

**Status (2026-09 finalization, D45)**: holds, with one correction to the
intuition above. Max selects the source with the highest current retention,
which is not always the most recently handled one (an older source with a
larger `S` can win). `σ_fact = 30` is a dimensionless multiplier on the
source's stability (`s · σ_fact`), not a duration in days, and it's an
assumption to ablate, not a measured value. Utility `U` of a Fact is the mean of its sources' `U`, an
explicit coupled heuristic with no independence claim.

## D15 — Entity and Community materialize visibility thresholds, not event time

Historical: materialized Entity and Community visibility thresholds. Entities are admitted by their policy witness (docs/01 §3.2); Community is a roadmap target. Original text: [archive](history/decision-log-archive.md#d15--entity-and-community-materialize-visibility-thresholds-not-event-time).

## D16 — lifecycle generations with monotonic catch-up and atomic cutover

**Status (2026-11-29)**: generation states are `active`, `catching_up`, `cutover` and `retired`, with the cutover conditions in docs/01 §4. Bound generation receipts were removed by D60.

**Decision**: docs/01 §4.

**Alternative**: per-Episode supersession with mixed generations visible at
the same time.

**Reason**: `ingest_seq` gives backfills and concurrent remembers one immutable
order. State+watermark capture and the unique Outbox key make the backlog /
dual-tail handoff atomic. Each target has one sequence head; extraction uses
synchronous fulltext and an exact bounded candidate digest, so async embedding
or retry timing cannot reorder semantic state. New remembers dual-tail ACTIVE
and BUILDING/CATCHING_UP targets. Physical generation labels/properties isolate indexes before top-k. A
cutover transaction holds the write queue while it proves
`covered_ingest_seq = Meta.ingest_seq`, preventing a tail race. ACTIVE is
appendable; rollback first catches an INACTIVE generation up through the same
barrier. Hidden writes and hidden GC never perturb serving recall.

## D17 — DELETE only through guarded `gc`

Historical: guarded `gc` modes. No `gc` command exists (docs/09). Original text: [archive](history/decision-log-archive.md#d17--delete-only-through-guarded-gc).

## D18 — corrections are backdated (`C.time := B.time`)

Historical: backdated corrections and the replacement protocol. Not built; INVALIDATES takes the invalidating Fact's own time (docs/03 §5). Original text: [archive](history/decision-log-archive.md#d18--corrections-are-backdated-ctime--btime).

## D19 — GDS reproduces retained rows and explicit dangling rows

**Decision**: GDS receives the retained role-weighted links and normalizes
them exactly as TypeScript does. Only a dangling row is expanded to explicit
uniform edges. A virtual source node σ carries weights `s_i`. For the
probability-normalized stationary vector of the augmented graph, the
restriction to V equals `α·p*`; that identity holds for normalized
stationary probabilities only, not for raw GDS scores, which may carry an
arbitrary positive scale. The actual comparison therefore restricts the raw
GDS output to V and renormalizes by its observed sum on V, then checks the
residual; it never divides raw scores by α (docs/07 §2).

**Alternative**: leave dangling handling and weighted source teleport to GDS
and widen the tolerance.

**Reason**: if the definitions differ, a discrepancy cannot be told apart
from a solver bug. Explicit dangling rows are at most 4M relationships at the
2,000-node envelope cap and run offline.

## D20 — top-k validation distinguishes clear and close boundaries

**Decision**: docs/07 §2.

**Reason**: two solvers within tolerance may legitimately order nodes whose p
values are within 7e-4 differently. Counting that as failure measures noise
and creates pressure to loosen thresholds.

## D21 — Links are real relationship types, seven roles, fixed

**Status (2026-11-29)**: there are eight roles; D58 added `REPLIES_TO` (`packages/protocol/src/link.ts`).

**Decision**: docs/01 §5.

**Alternative**: one relationship type with role as a property; roles open for
extension.

**Reason**: real types are what let type-filtered expansion use the
relationship-type store and bounded index-backed patterns instead of a
property predicate on every relationship. That is a bounded, typed pattern,
not an O(1) promise: a degree `COUNT{}` over an arbitrary node can cost work
proportional to the degree, so the hub test doesn't use one. It's a bounded
indexed probe over the physical conducting relationships with `LIMIT 256`:
fewer than 256 rows is the exact degree, saturation marks a hub, and hubs
expand only through the `HubArc` cache under the non-hub scan cap (D30,
D33). Seed damping uses the same capped degree. Variety is absorbed
by `RELATES_TO.content`, so there
is little pressure to add roles, and adding one changes PPR conduction rules,
the lattice and the validation all at once.

## D22 — Neo4j-down remember uses a bounded spool

**Superseded** by [D61](#d61--remember-commits-directly-to-neo4j-2026-11-28) (merged 2026-11-29): `remember` commits directly; a nonempty legacy spool is refused on upgrade. Original text: [archive](history/decision-log-archive.md#d22--neo4j-down-remember-uses-a-bounded-spool).

## D23 — valid(T) only at assembly

Historical: `valid(T)` only at assembly. Validity is now also a candidate-stage predicate (docs/03 §7). Original text: [archive](history/decision-log-archive.md#d23--validt-only-at-assembly).

## D24 — role weights only; each retained row normalizes itself

**Released.** LO released the role-only, no-per-link-weight promise on 2026-10-23. The solver still uses per-role weights and rejects per-arc weights (docs/06 §4); that is current code, not a binding promise. Original text: [archive](history/decision-log-archive.md#d24--role-weights-only-each-retained-row-normalizes-itself).

## D25 — derived idempotency keys include the generation; originals-layer links have none

**Decision**: derived link `idem_key = sha256(from, to, role, content,
generation)`. Fact identity hashes generation, schema, content, properties,
time, sub-kind, Entity bindings, source Episode IDs and synthesis support IDs.
Entity identity hashes generation, normalized name and entity kind.
`HAS_PAYLOAD`, `HIT_OF` and revision `INVALIDATES` are originals-layer links
with no generation. `NEXT_EPISODE` is a rebuildable topology-cache link keyed
by `session_key = sha256(origin_source,origin_session)` plus
predecessor/successor (docs/01 §1, §4–5; 02 §3).

**Alternative**: the first draft — `sha256(from, to, role, content)` for every
link, with all links implicitly in the extraction stream.

**Reason**: re-creating the same relationship in generation 43 must not collide
with its generation-42 copy. NEXT_EPISODE cannot be immutable because a
backfill must splice the session chain; treating it as cache preserves
CREATE-only Episodes and deterministic replay. Found by the PR reviews of
2026-09.

## D26 — revision occurrence identity is separate from content integrity

**Decision**: `origin_key = sha256(source, session, actor, record)` is indexed
and shared by every revision of a source. The adapter supplies a stable,
per-occurrence `source_revision`; `revision_key = sha256(origin_key,
source_revision)` is unique and drives idempotency. `digest` separately hashes
the canonical schema, content, properties, time, payload hash and
`previous_revision_key`. The indexed Episode origin-head lookup under the
Meta writer fence plus explicit `previous_revision_key` serializes the immutable
revision chain (docs/01 §1, 02 §3).

**Alternative**: derive revision identity from content digest.

**Reason**: digest identity cannot represent A→B→A: the last A resolves to the
already-invalidated first A. It also missed changes to payload, time, schema
and properties. Occurrence identity permits a true revert; the canonical
digest detects conflicting retries of that occurrence. Found by the PR
reviews of 2026-09.

**Status (2026-09 finalization, D49)**: narrowed, not replaced. The digest
body described above is exactly version 1, and it stays frozen for every
Episode already stored under it. D49 adds `origin_role` and `lineage_digest`
only to the version-2 body used by new admissions, and the stored row's
version decides which body a retry recomputes. Revision identity, the
indexed origin-head lookup and idempotency are unchanged in both versions.

## D27 — one commit path for every Hit

**Superseded** by [D62](#d62--record-recall-selection-and-one-explicit-outcome-2026-11-2829) (merged 2026-11-29): the common commit path and its receipt and exposure producers are gone; Hits come only from `report_outcome`. Original text: [archive](history/decision-log-archive.md#d27--one-commit-path-for-every-hit).

## D28 — extraction is sequenced claim-LLM → bounded read → judge-LLM → write

**Decision**: a target-generation sequencer reserves one ingest sequence; the
claim LLM runs first, bounded generation-index reads fetch candidates, the
judge LLM decides, and the write transaction re-checks the target, source head
and every premise. Three automatic failures pause that same sequence head;
nothing overtakes it.
Historical Facts remain time-correct because Fact validity consults the
materialized source revision chain (docs/02 §5, 03 §4).

**Alternative**: the first draft's "all in one transaction (LLM outside)",
which was not implementable as stated and had no stale-read handling.

**Reason**: a judge verdict is a function of its premises. If a candidate Fact
was invalidated or retired while the LLM ran, writing INVALIDATES or a
re_mention against it is wrong; re-running is cheap. Found by the PR review of
2026-09.

## D29 — `m_cache` and hub shortlists are a v0.2 maintenance job, not dreaming

**Superseded** by [D60](#d60--production-facts-come-only-from-extraction) (merged 2026-11-29): hub shortlists are removed and no maintenance job exists. ConductingArc is maintained with its physical links (docs/01 §5). Original text: [archive](history/decision-log-archive.md#d29--m_cache-and-hub-shortlists-are-a-v02-maintenance-job-not-dreaming).

## D30 — hard bounds on every recall stage, including query work

Historical: hard bounds for every planned recall stage. The shipped bounds are in docs/05. Original text: [archive](history/decision-log-archive.md#d30--hard-bounds-on-every-recall-stage-including-query-work).

## D31 — security and durability rules are normative

**Status (2026-11-29)**: spool guarantees here are void since [D61](#d61--remember-commits-directly-to-neo4j-2026-11-28). The optional TCP listener is operator-configured with a bearer token (docs/02 §10).

**Decision**: `~/.anamnesis` 0700, UDS 0600 with a 0600 capability token,
length-prefixed frames and global resource quotas, daemon-owned chunked object
upload, validated content hashes, bolt on 127.0.0.1 only, per-install random
Neo4j password, daemon singleton lock, checksummed spool/cursor journals,
object-lease GC, journaled Community-offline backup and preflighted
staging-root restore (docs/01 §9, 02 §2/§10).

**Alternative**: leave these to implementation.

**Reason**: a normative document that leaves the trust boundary and the
crash-consistency rules implicit will be implemented inconsistently; a
default database password is the kind of gap an unwritten rule leaves open.
Found by the PR review of 2026-09.

## D32 — Community generations pin an extraction snapshot

Historical: Community generations for dreaming (docs/09). Original text: [archive](history/decision-log-archive.md#d32--community-generations-pin-an-extraction-snapshot).

## D33 — HubArc cache and source-local directed truncation

**Superseded** by [D60](#d60--production-facts-come-only-from-extraction) (merged 2026-11-29): the HubArc cache is removed. ConductingArc and its ordered probes remain (docs/06 §2). Original text: [archive](history/decision-log-archive.md#d33--hubarc-cache-and-source-local-directed-truncation).

## D34 — visibility thresholds are materialized

Historical: materialized visibility thresholds for Entity and Community (docs/09). Original text: [archive](history/decision-log-archive.md#d34--visibility-thresholds-are-materialized).

## D35 — synthesis inputs equal the bounded support set

Historical: synthesis inputs for dreaming (docs/09). Original text: [archive](history/decision-log-archive.md#d35--synthesis-inputs-equal-the-bounded-support-set).

## D36 — elapsed time is nonnegative and server writes use logical time

**Decision**: every forgetting interval is `max(0,t₁−t₀)`. Write timestamps
use `max(wall_clock, Meta.last_server_time+1)` and replay retains the maximum
seen Hit time (docs/02 §1, 04).

**Reason**: wall-clock regression or imported pre-ingestion Hits must never
make retention exceed one, make the power-law base negative, or reduce
stability.

## D37 — Neo4j and filesystem epochs fence stale daemons

**Status (2026-11-29)**: the Neo4j writer epoch is the write fence; spool namespaces no longer exist ([D61](#d61--remember-commits-directly-to-neo4j-2026-11-28)).

**Decision**: the filesystem lease discovers a likely singleton; a Neo4j
`writer_epoch` is the actual fence. Startup increments it while locking Meta,
and every write transaction locks Meta and requires its captured epoch
(docs/02). A separate non-reusable `fs_epoch` owns epoch-namespaced spool
journals; object publication and spool append reassert the fixed parent
pointer before mutation and ack.

**Reason**: heartbeat expiry cannot distinguish a dead daemon from a paused
one, and Neo4j is unavailable on the exact path where spooling matters.
Database locking fences transactions; epoch-owned files prevent byte
interleaving and make any post-takeover filesystem write non-authoritative or
unacknowledged.

## D38 — backup and restore are discoverable journaled operations

**Decision**: backup state lives at a fixed live-root path and records the
destination; restore stages in a same-filesystem sibling and activates under a
fixed parent journal/operation lock. Every root rename has a write-ahead phase
and path-existence recovery rule; backup and restore refuse each other's live
journal. Sidecar-last commits objects, and framed spool/cursor records
quarantine the whole journal on checksum corruption (docs/01 §9).

**Reason**: arbitrary destinations, cross-device renames and two unjournaled
root renames are not crash-total. A failed restore must leave or recover the
previous live root automatically.

## D39 — harnesses are external; the repo surface is the daemon and its contract

**Decision**: this repo ships `anamnesisd`, the RPC contract
(`@anamnesis/protocol`, schema-exported) and a daemon-ops CLI
(`up/down/status/verify/gen/gc/dream/bench/backup/restore`). Every harness —
anything that injects or retrieves text — is a separate project owned by the
operator, attaching over the UDS JSON-RPC surface.
`remember`/`recall` are API-only — the CLI does not wrap them.

**Reason**: harness frameworks churn far faster than a memory engine should.
Keeping them out of the repo makes the RPC contract the single product
surface — versioned, schema-exported, harness-agnostic — and new agent
frameworks require zero changes here. The daemon protocol already assumed
untrusted external callers (capability token, caps, commit modes), so nothing
about the security or Hit-commit model changes.

## D40 — a signed outcome verdict is the only reinforcement that can lower stability

**Superseded** by [D62](#d62--record-recall-selection-and-one-explicit-outcome-2026-11-2829) (merged 2026-11-29): the signed verdict commit is replaced by one explicit `report_outcome` per Usage. Original text: [archive](history/decision-log-archive.md#d40--a-signed-outcome-verdict-is-the-only-reinforcement-that-can-lower-stability).

## D41 — every recall result carries a closed `epistemic` grade derived from its producer

**Status (2026-11-29)**: the wire values are `observed` and `derived`. No synthesis producer exists, so nothing else is emitted (docs/05 §6).

**Decision**: recall results include `epistemic ∈ {observed, extracted,
synthesized}`, computed at assembly time from `schema` — ingest originals are
`observed`, extraction claims and mappings are `extracted`, dreaming syntheses
are `synthesized`. Nothing is stored; a new schema is mapped to one of the three
when registered (docs/05 §6).

**Alternative**: leave it implicit in `schema`. The registry is open-ended and
grows with every extractor, so a caller wanting to grade trust would have to
track the whole registry; and provenance already answers the question fully but
only by walking `derived_from` per result.

**Reason**: who wrote a memory is the cheapest honest signal of how far to trust
it — the engine knows exactly which process produced each element, and the
distance from the source (said → model read → model combined) is a fact about
the data, not a framing choice. Exposing it as a fixed enum lets a caller weight,
filter, or phrase by grade without parsing the schema registry, while
`provenance` remains the full chain for anyone who needs it. Adapted from omo's
author-as-trust-tier; anamnesis derives it from provenance instead of recording
an author field.

**Status (2026-09 finalization, D47)**: holds, with the wording tightened.
`epistemic` is provenance distance, the number of model steps between the
source utterance and the result. It is not a trust probability and it is not
calibrated against outcomes; a caller who wants a trust estimate combines it
with `confidence`, `modality`, `U` and its own judgment.

## D42 — extraction keeps the speech act and the evidence span; it never records-then-invalidates within one Episode

> **Status: superseded in part by D46, D47, D48 and D49 (2026-09
> finalization).** D48 adds a literal `evidence_quote` from which the stored
> span is derived, plus a required immutable `content_language`; D49 makes
> same-speaker, same-time and same-scope decidable from bounded stored fields
> rather than judgment. Neither weakens the rules below.
> What survives: the closed `modality` enum, `modality` in Fact identity and
> as a factor of `m₀`, `confidence` stored but outside identity, a
> fail-closed `span` check. What changed: (1) `span` validates only that the
> slice is nonempty and lies on UTF-8 boundaries inside the Episode; it is
> not proof of entailment or a hallucination detector. (2) `confidence`
> means source-faithfulness, how faithfully the claim restates what the
> source said, not world truth. (3) Intra-Episode suppression applies only to
> an explicit same-speaker, same-time, same-scope self-correction. Differing
> reports, times or modalities stay as separate Facts; an unresolved pair
> gets CONTRASTS. (4) Generation identity includes `modality`, omits
> `confidence`, and records the prior/calibration version. The text below is
> the original reasoning.

**Decision (historical)**: a claim carries `modality ∈ {asserted, reported, hedged,
intended, hypothetical}`, `confidence ∈ [0,1]`, and an optional `span` of byte
offsets into the Episode. `modality` is part of Fact identity and a factor of
`m₀`; `confidence` is stored but not identity. A present `span` is validated at
write — a slice that is not in the Episode rejects the claim. Claims are emitted
in Episode order and a later claim contradicting an earlier one in the same
Episode suppresses it before anything is written (docs/02 §5, §5.1; docs/01 §1;
docs/04 §1; docs/05 §6).

**Alternative**: (a) drop hedges and intents as "not durable" (senpi's facts
extractor: "omit guesses, plans not adopted"), or (b) store them as low-score
facts flagged by regex at read time (memkraft `confidence.py`). (c) No span —
trust `DERIVED_FROM` alone. (d) Let intra-Episode self-corrections become Fact +
INVALIDATES like any other contradiction.

**Reason**: (a) forgets that an intent existed and (b) lets an intent be counted
as a done thing until a regex catches it; storing the speech act as a closed
enum keeps both the recall and the distinction, and lets the modality prior be
refit against outcome verdicts instead of guessed. A span makes `extracted`
auditable against `observed` — a caller can show the quote — and a fabricated
span is the one hallucination signal the engine can verify with no model, so it
is fail-closed. Suppressing intra-Episode contradictions keeps the graph free of
Facts that were wrong before they were written. `confidence` stays out of
identity because a scalar the model does not emit deterministically must not
fork otherwise-identical Facts. Adapted from senpi's self-contained-record and
same-transcript-contradiction rules and memkraft's uncertainty markers; both
are done here at write time as stored fields rather than at read time as
prompts or regexes.

## D43 — policy is suppression, never erasure

**Status (2026-11-29)**: the shipped selector is `episode_id` and/or `source` with `content` scope only (docs/01 §3.2). The other selectors and the `derived` scope are not built (docs/09).

**Decision**: two authenticated RPCs, `policy.set` and `policy.revoke`, each
append an `anamnesis.memory-policy/1` Episode; the active policy is a
rebuildable cache over those Episodes. A policy has `policy_id`, `action ∈
{deny, revoke}`, a selector `{subject_entity_id?, schema?, sub_kind?,
modality?, literal?}` with at least one field, fields ANDed, `literal` an
NFC-normalized case-sensitive substring (no regex), and a `scope`. `derived`
scope matches the canonical claim and its resolved entities before any
derived write or Hit. `content` scope also matches the raw Episode text and
suppresses ordinary original recall. A policy takes effect for ordinary
serving after a policy-revision barrier; background reconciliation then
removes denied derived items from indexes and caches by rebuilding, not by
touching originals. Suppression applies to extraction, re_mention, dreaming,
candidates, conduction, assembly, companion and provenance text: a denied
source can't support a visible derived result. A policy change during a
recall or commit forces retry or reject before the response or feedback, never
torn serving. Policy Episodes keep their audit metadata but are excluded from
memory search. `revoke` doesn't fabricate Facts that were suppressed before
extraction; re-extraction is an explicit operation. Historical `T` never
bypasses current policy. Existing backups and privileged raw operator access
stay outside suppression. There is no `gc --erase` and no GDPR-style erasure
guarantee. Instructions found in ingested text are never executed as policy
(docs/02, docs/05).

**Alternative**: (a) physical deletion of matched Episodes and everything
derived from them; (b) read-time regex filters on results; (c) letting a
"forget this" utterance in an ingested transcript act as a command.

**Reason**: (a) breaks invariant 1 (CREATE-only originals), makes replay of
the Hit ledger and generations non-deterministic, and still can't reach
backups, so it would promise an erasure it can't keep. (b) leaks through
conduction and provenance: a denied Fact still pulls its neighborhood into
the envelope and its text into `derived_from`. (c) is prompt injection with
write access. Structured selectors can't reach text that was never extracted;
the contract says so rather than pretending `content` scope is optional.
Naming the limits is the honest version of the feature.

## D44 — recall budget in exact units with a pinned tokenizer

**Status (2026-11-29)**: [D62](#d62--record-recall-selection-and-one-explicit-outcome-2026-11-2829) replaced D44's receipt with the Usage; the exact budget rules stand.

**Decision**: `recall` accepts an optional `budget {unit ∈ {utf8_bytes,
unicode_scalars, tokens}, limit: nonnegative integer, tokenizer_id?}`. `tokens`
requires an installed `tokenizer_id` pinned by version or digest; an unknown
id is rejected, with no estimate fallback. Byte and scalar counts are exact.
`context_text` is a deterministic LF-separated rendering of the complete
included items plus their mandatory source, contrast and supersedes content;
the same structured results always render to the same text. `used_budget`
counts the exact final `context_text` including separators; transport JSON
and diagnostics are excluded. Packing is greedy over primaries in final
score order: for each candidate bundle, construct the actual deduplicated
prospective `context_text` and include the bundle only if the whole text
fits. An oversized bundle is skipped and later ones are still considered.
Claims and required contradiction warnings are never truncated. `limit: 0`
yields empty context and results. `limit` counts primary bundles (it's the
canonical RPC field, not `k`); companions are bounded separately (D46). Ranks
and the receipt describe included primaries only. Without a budget the
configured server output cap applies. The hard RPC response byte cap stays
in force under any budget (docs/05).

**Alternative**: (a) approximate tokens as `bytes / 4`; (b) truncate the last
item to fill the budget exactly; (c) count the whole JSON response.

**Reason**: (a) is wrong by a factor that depends on script and tokenizer
version, and a caller who sets a token budget is doing so because their
context window is measured in that tokenizer's tokens. Being off means either
an overflowed prompt or wasted room; a rejected request is diagnosable. (b)
produces half a claim or a claim without its contradiction, and the receipt
would then attest to something the caller never saw whole. (c) makes the
budget depend on field names and formatting rather than on delivered
memory. A caller who needs to bound the transport has the byte cap.

## D45 — accessibility and utility are separate; only adoption moves S

**Released and superseded.** LO released D45's accessibility, adoption-only-moves-S and immutable-m0 promises on 2026-10-23. [D62](#d62--record-recall-selection-and-one-explicit-outcome-2026-11-2829) (merged 2026-11-29) replaced its feedback path: signed outcome credit now moves stability both ways within `[S0, S_max]` (docs/04 §5, §7). Accessibility and utility stay separate quantities (docs/04). Original text: [archive](history/decision-log-archive.md#d45--accessibility-and-utility-are-separate-only-adoption-moves-s).

## D46 — one Fact per occurrence, contradictions as bounded companion bundles

**Status (2026-11-29)**: one Fact per occurrence and bounded CONTRASTS companions ship. Occurrence grouping and the counted conflict scan don't (docs/09).

> **Status: narrowed by D49 (2026-09 finalization).** The grouping key and
> the `known_conflict` predicate are now defined over bounded stored fields
> (`speaker_key`, `subject_keys`, `predicate_text`, closed `scope`,
> `time_key`, `modality`) under `grouping_version`, and echo lineage is added
> to the list of things that never boost a duplicate. Nothing below is
> relaxed.

**Decision**: a semantic duplicate preserves every original and each
occurrence's extracted assertion and provenance. There is no "duplicate →
no Fact". Each source occurrence yields its own immutable Fact, linked to
the existing one by `DERIVED_FROM` / `RELATES_TO`, with an optional
`re_mention` audit Hit and no automatic truth or confidence boost. Duplicates
are grouped only at assembly, with a representative plus explicit occurrence
IDs and truncation metadata; the grouping key preserves subject, predicate,
time and modality, and no global semantic merge is promised.

Conflicts are completed after primary ranking and before budget packing.
For each primary, recall scans its raw `CONTRASTS` adjacency in
deterministic ID order, at most 64 rows plus one sentinel row, and runs the
bounded per-row policy and validity checks on those rows. The first 4
eligible rows become the companions. There is no cache of eligible peers
keyed by an arbitrary `T`; eligibility is evaluated on the scanned rows
under the request's snapshot and current policy. Reporting follows from what
the scan can actually know: `conflict_total` is the exact eligible count only
when the raw scan was exhausted within 64 rows and that count is at most 4;
in every other case it is `null`, never an estimate and never a value
inferred from a `limit + 1` read. `conflict_truncated` is set when more than
4 eligible peers were found or the sentinel row shows raw rows beyond 64.
An incomplete bundle carries a mandatory incomplete warning; a policy-hidden
peer yields a mandatory redacted conflict indicator with no text and no ID.
No unbounded `COUNT` runs on the recall path. Field names
(`conflict_total`, `conflict_truncated`, `conflict_redacted`) are owned by
docs/05 and coordinated through the lead. The bundle is returned even when
the peer wasn't a retrieval candidate. There's no automatic winner by
confidence or recency. The budget counts companion text and both warnings;
a bundle that doesn't fit whole is skipped (D44). There's no both-sides guarantee beyond this stated bound
(docs/02, docs/05).

**Alternative**: dedupe at extraction (drop the second occurrence, raise the
first's confidence); or surface only the contradiction peers that happened
to be retrieved.

**Reason**: the second occurrence is evidence with its own time, speaker and
modality; collapsing it destroys the provenance that D42's span and
`epistemic` exist to expose, and a confidence bump from repetition is the
"repeated therefore true" error. Peer completion after ranking keeps the
ranking stage bounded while ensuring a caller never sees one side of a known
contradiction merely because the other side scored low. The bound of 4 and
the truncation flag keep the work bounded and the omission visible.

## D47 — calibration, oracle and replay are defined, not implied

**Decision**: every quantity that enters a score has a stated status.
Assumptions to ablate: `DECAY`, `FACTOR`, `a`, `b`, `c`, `γ`, `β`, `ν`,
`μ₀`, `σ_fact`, the `sub_kind` and `modality` priors, role weights and RRF
weights. Observed but proxy: `U`, a reported utility, not truth or causal
attribution. Provenance distance, not trust probability: `epistemic`.
Source-faithfulness, not world truth: `confidence`. The PPR baseline uses
`α = 0.85` as the damping factor (not its complement), the uniform dangling
policy, rank origin 1, and RRF example values within `1/61`; the virtual
source is normalized over `V`. Mass gates the envelope as well as the final
score, and docs/06–07 describe that gating rather than presenting the
envelope as mass-neutral. Local residual, truncation and retrieval utility
are three separate measurements; close ties don't imply `overlap ≥ 0.95`,
so validation uses the residual bound plus a tie-aware, report-only overlap.
The GDS baseline is pinned to release 2.13.12 with no claims about master or
latest. Determinism is conditional on the captured candidate, index, cache
and degradation state; `structure_revision` is a serving-view revision, not
a full replay token. The policy barrier is stricter than the torn structural
result check (docs/04 §9, docs/06, docs/07).

**Alternative**: present the defaults as tuned values and the GDS comparison
as ground truth.

**Reason**: the earlier text mixed literature values, guesses and measured
numbers in one table and let a reader assume that agreement with GDS on a
truncated envelope certified retrieval quality. Naming what each number is,
what the oracle actually measures, and what replay can and can't reproduce
is what makes the CI gates in docs/07 falsifiable rather than decorative.

## D48 — source-language Facts and original-query recall are the reversible default

**Decision**: a Fact carries a required, immutable, meaning-bearing
`content_language` matching `^[a-z]{2,8}(-[a-z0-9]{1,8})*$` in at most 35
ASCII characters, `mul` for materially multilingual prose or `und` when the
evidence is nonlinguistic or insufficient. Extraction preserves the source
Episode's language by default; mixed and code material stays mixed.
`evidence_quote`, spans, names, paths, URLs, identifiers and code are always
source-exact. English Fact content is an **optional extraction-generation
configuration**, never a mutation of a source-language Fact and never an
in-place rewrite: a generation records `fact_language_policy ∈ {source, en}`,
the prompt artifact digest, `extractor_profile_id` and the validator version.
An `en` generation accepts only `content_language=en`; a claim it cannot
render source-faithfully in English is rejected as
`language_policy_mismatch`, with no per-claim fallback. Changing policy opens
a replacement extraction generation and uses the normal cutover/rollback
path. Model-generated translations and transliterations never replace a
source name, never enter Entity `normalized_name` or aliases, and never carry
source authority; separate English projections are nonauthoritative
development artifacts that are neither persisted nor indexed until a later
decision pins their schema, prompt and digest. Recall keeps the caller's
original query verbatim: BM25 uses that query and the vector channel applies
only the active embedding profile's pinned wrapper around the same text.
Global original Episodes and active-generation Facts share the existing
vector and BM25 channels and their existing caps, with no language quota and
no added channel. Automatic translation and two-query RRF stay absent from
runtime (docs/01 §1, docs/02 §5, docs/05 §1–2).

**Alternative**: (a) normalize every Fact to English at extraction; (b) issue
a translated second query and fuse it with the original at runtime; (c) admit
English projections as searchable rows alongside their source Facts.

**Reason**: (a) rewrites the evidence the rest of the contract depends on.
The extraction spotcheck showed generated projections inventing name
renderings (`Jihyun`, `(Sato)`) and one model collapsing distinct
Latin/Cyrillic identifiers, so a translated record cannot be the canonical
one. (b) was exercised offline in
[research/retrieval-fusion](research/retrieval-fusion.md) at commit
`4bbbd7e07a41592bc3d1fccba512138643a89648`: on the same source-informed
20-query development set the original-query union scored 20/20 Hit@1 and
equal-weight two-query RRF scored 19/20. That set has seven substantive
targets, human translations, no realistic distractor population and saturated
Hit@3, so it supports a reversible default, not a superiority claim in either
direction. A runtime fusion decision would still have to pin its translator,
candidate quota, fusion and failure rules and receipt fields. (c) doubles the
candidate population with unversioned model output; keeping the policy a
generation configuration makes the whole choice reversible by cutover.

## D49 — provenance roots bound echo accounting; pairwise predicates assert no global identity

**Status (2026-11-29)**: LO released D49's policy promise on 2026-10-23. The digest versions, lineage bounds and echo accounting below are still the stored contract (docs/01 §1, §3.3); the release changed no stored data. [D62](#d62--record-recall-selection-and-one-explicit-outcome-2026-11-2829) replaced the receipts D49 refers to with Usage; the `lineage_mode: "receipts"` literal now names parent Usage IDs.

**Decision**: Episode digest identity gains a server-selected
`episode_digest_version` **prospectively**. An Episode stored without that
property is immutable version 1 and keeps the frozen pre-D49 insertion-ordered
`JSON.stringify` body; every revision admitted after D49 implementation stores
version 2, whose RFC-8785 body adds `episode_digest_version`, `origin_role`
and `lineage_digest`; any other stored value is `unsupported_digest_version`.
The stored row's version wins over a caller's or a journal record's creation
version, so verify, exact retry, journal replay, backup/restore and rebuild
dispatch on it and never SET `episode_digest_version`, `digest`, `origin_role`
or `lineage_digest` on a legacy Episode, which also never gains
lineage properties in place. There is no data migration, and this PR ships no
compatibility code (docs/01 §1, docs/02 §3 and §4, docs/08, docs/09).

On top of that boundary, the authenticated adapter labels each Episode
`origin_role ∈ {user, assistant, tool, document, operator}` and
`lineage_mode ∈ {direct, receipts}`. Direct input has no parents, depth 0 and
its own Episode ID as its single root; an assistant turn that received
anamnesis context must use `receipts` and supply 1..4 distinct
`parent_recall_ids`. The daemon verifies the caller binding and writes
Episode lineage properties `{lineage_mode, parent_recall_ids[0..4],
context_digests[0..4], root_episode_ids[0..16], echo_depth: 0..8,
lineage_complete, lineage_digest}` in the Episode transaction; those retained
properties, not an expiring receipt, are replay authority. `context_digests`
copies each parent receipt's
stored `selection_digest`, the SHA-256 over the RFC-8785 canonical ordered
array of at most 64 delivered `{element_id, root_episode_ids, echo_depth,
complete}` records, so the receipt schema retains exactly the value lineage
names. Each Fact copies immutable bounded metadata:
`echo_state ∈ {direct, known_echo, context_derived, unknown}`,
`echo_of_element_id`, sorted `parent_recall_ids`, sorted
`corroboration_root_episode_ids[0..16]`, `echo_depth` and
`echo_lineage_truncated`. Overflow past 16 roots or depth 8 sets
`complete=false`, `echo_state=unknown` and `echo_lineage_truncated=true`; no
omitted ancestor becomes a new root. Assistant input without authenticated
receipt metadata is `unknown`, never presumed independent. The gate covers
the source Episode itself: an assistant Episode with unknown or incomplete
lineage, and every output derived from it, is stored but ineligible for
semantic candidates, synthesis and invalidation
(`echo_lineage_unavailable`). A synthesis has no Episode lineage row to copy,
so it materializes one from its exact 1..16 non-synthesis supports before Fact
identity: no `echo_of_element_id`, the first 4 of the sorted distinct
`parent_recall_ids` union, the first 16 of the sorted distinct root union, and
`max(support.echo_depth)` with no added receipt hop. It is `context_derived`
only when every support is lineage-complete and neither union truncates;
otherwise it is `unknown`, truncated and ineligible. Occurrence count,
root count, echo state and echo depth never change confidence, `m0`, mass,
utility, rank or adjudication, and never elect a conflict winner.

The same decision fixes the bounded fields that make same-speaker, same-time
and same-scope decidable: `speaker_key`, `subject_keys` (1..16 sorted resolved
Entity IDs or null, with no literal or normalized-string fallback),
`predicate_text`, a closed `scope` object with `scope_complete`, `time_key`
and the existing `modality` enum. `same_speaker` requires two non-null equal
`speaker_key` values inside one `(origin_source, origin_actor)` namespace; no
display name, alias, pronoun, fuzzy match or shared account implies speaker
equality. For the L1b intra-Episode rule `same_time` means the same immutable
Episode ID, while cross-Episode grouping requires exact `time_utc` and
`time_precision`. Scope equality has two distinct predicates that are never
interchanged: `same_scope_l1b` needs equal non-null `correction_scope_text`
plus equal resolved subjects, predicate and attribution while the corrected
value and time fields may differ, and `same_scope_group` needs
`scope_complete=true` with a byte-identical RFC-8785 `scope`. Requiring the
complete grouping scope for a correction would make every real self-correction
fail.
When every component resolves, assembly pins
`grouping_version = "anamnesis.duplicate-group/1"` and computes
`duplicate_group_key` over subject keys, predicate key, time key, scope key
and modality; an unresolved subject, empty predicate or incomplete scope
disables grouping. `known_conflict(f,g)` is exactly one active-generation
CONTRASTS relationship with canonical endpoints; it is irreflexive, symmetric
and not transitive (docs/01 §1, docs/02 §3 and §5, docs/05 §6).

**Alternative**: (a) count repeated assistant restatements as corroboration;
(b) infer lineage from prose; (c) treat the grouping key or `speaker_key` as
a global identity claim across generations and adapters; (d) recompute every
stored Episode under the new digest body, whether by rewriting the rows or by
reinterpreting the old body with the new serializer.

**Reason**: (a) is the "repeated therefore true" error D46 already rejects,
one step removed: the model repeating retrieved text is the system's own
output coming back, so counting it as new evidence inflates whatever it
echoes. (b) is prompt injection with provenance authority. (c) would make a
bounded local comparison masquerade as an entity-resolution result; the key
is local to one pinned generation and one bounded candidate set, and it never
erases an occurrence. Bounding the lineage to four parents, sixteen roots and
depth eight keeps every online check a materialized-row read, with no
ancestor traversal on the recall path. (d) has two failure modes and no
upside: rewriting breaks invariant 1's CREATE-only originals, and
reinterpreting makes every existing row fail verification, since the frozen
body has neither the discriminator nor the two lineage fields. A version
discriminator that the server owns and the stored row decides keeps retry,
journal replay and rebuild deterministic while costing one property on new
Episodes.

## D50 — adjudication is shadow-first and operator repair is append-only

**Superseded** by [D60](#d60--production-facts-come-only-from-extraction) (merged 2026-11-29): the shadow adjudicator, operator repair and bound generation receipts are removed. Only extraction writes production Facts. Original text: [archive](history/decision-log-archive.md#d50--adjudication-is-shadow-first-and-operator-repair-is-append-only).

## D51 — separate model and index fingerprints pin one embedding profile; a permanent hole blocks coverage

Historical: separate model and index fingerprints, qualification and the embedding build lifecycle. The shipped lane uses one profile ID per profile (D54, D56, docs/01 §4); the rest is in docs/09. Original text: [archive](history/decision-log-archive.md#d51--separate-model-and-index-fingerprints-pin-one-embedding-profile-a-permanent-hole-blocks-coverage).

## D52 — the automatic extraction pipeline writes Facts only through the validated semantic path; relations are never guessed

**Decision**: `materializeExtractionPipelineTx` no longer has a private
write path. A judge-approved claim becomes a Fact only when it passes
`validateSemanticClaim` and is written by the same `writeValidatedFactTx`
that serves adjudication proposals (D50), so every Fact carries
`meaning_digest`, a real `sub_kind`/`modality`, model-reported
`confidence` as its mass, Entity nodes resolved against the generation's
`entity_key` registry (existing → reuse, new → create with an
Entity witness properties, unresolved → mention dropped), and a resolved time
(`time_basis: claim` when the model states an absolute date, otherwise the
Episode's time as `inherited`). The claim schema gains optional
`confidence`, `entities`, `time`, `sub_kind` and `speech_act`; the
extraction prompt asks for them. A claim without any confidence is retained
as an audit decision only and never becomes a Fact; a claim the validator
refuses is recorded as content-free custody in `materialization-state.json`
(`created:false, refused:<code>`) so the occurrence stays idempotent.

The former word-overlap heuristic that linked Facts of one Episode with
`CONTRASTS "conflicting extracted claims"` is removed. Fact→Fact
`CONTRASTS` and `INVALIDATES` are emitted only by an explicit relation
judge (still to be shipped) or an operator; the automatic path emits neither.
Activation's witness check counts witnesses per distinct Entity, since several
Facts of one generation may legitimately mention one Entity.

**Alternative**: (a) keep the private path and add fields to it; (b) keep the
heuristic as a "cheap contradiction detector" until a judge exists; (c) fall
back to `confidence: 1` or the judge's `retain` as implicit confidence.

**Reason**: (a) duplicated identity, entity and time rules that D46/D49/D50
already fixed in one place, and had drifted (fixture actor, mass 1.0, no
`meaning_digest`, Entities that were Facts by label). (b) produced links
between claims that merely shared vocabulary, which recall then surfaced as
"conflicts"; a wrong relation is worse than none because D46 companion bundles
present it as evidence. (c) fabricates the one number that docs/04 and
docs/05 consume as Fact mass; an absent measurement must stay absent.

## D53 — Fact→Fact relations come from an LLM relation judge that gates the validated write

**Decision**: When an extraction provider is configured, every validated claim of a source is judged (`judge_relations`, prompt `judge-relations.v1.md`) against at most 16 ACTIVE Facts of the writable generation, the union of the Facts that share a MENTIONS Entity with the claim and the top-8 `vec_fact` vector neighbours of the claim's content, deduplicated by Fact id with the entity-shared Facts first (so 16 or more entity-shared Facts leave no room for neighbours) (#265); when the vector channel is unavailable the set falls back to the entity-shared Facts alone and the premise records which of `provider_absent`, `profile_absent` (no EmbeddingProfile node for the configured provider), `embed_failed` or `query_failed` it was; the premise `body_digest` stays a pure function of the fact and its candidates, so the same inputs give the same bytes; the digest-bound premise is persisted once per occurrence in `ExtractionJournalEntry.relations`, the verdict is accepted only when it echoes that digest and covers exactly the candidates, and no Fact of the source is written while a verdict is owed (`relation_judge: pending`). Verdicts apply mechanically: `contrasts` → CONTRASTS new→candidate (both stay ACTIVE), `invalidates` → INVALIDATES new→candidate with `effective_time_utc` = the new Fact's time and never onto a Fact that already invalidates one (`chain_refused`), `duplicate` → the Fact is not written (`duplicate:` operation with `duplicate_of`), confidence below 0.6 → `unrelated`; every decision is recorded in the materialization result in `materialization-state.json`. Vector neighbours come from the `vec_fact_<profileId>` index of the active embedding profile (#265).

**Alternative**: judge inside the extraction claim call; write the Fact first and link afterwards; chain INVALIDATES transitively.

**Reason**: a verdict must bind to the exact candidate set it saw, retries must be idempotent (custody is the barrier), and a duplicate that was already written would need deletion; a non-recursive INVALIDATES keeps `valid(T)` a single seek (docs/01 §5).

**Exhaustion (addendum)**: a pending relation verdict is attempt-bounded like every other stage. When every owed premise of a pipeline has failed `maxAttempts` times (failures in `ExtractionJournalEntry.relations`), the scheduler calls `Store.sealFactRelationOmission`, which records the source's custody content-free in `materialization-state.json` with `result.omitted = "relation_judge_exhausted"` (plus the failure count and the unjudged occurrence keys). No Fact of that source is written — a Fact whose relations were never judged would be exactly the unguessed relation D52 forbids — and the pipeline reads as `relation_judge: "omitted"` (terminal, counted as a failed outcome). Because custody exists, coverage and cutover treat the source like a refused one, so a provider outage during initial catch-up can no longer leave the generation stuck in `catching_up` with `activation_prerequisite_unavailable`. The seal is idempotent and refuses (`relation_omission_premature`) while the budget is not spent.

## D54 — Embedding lane without graph ledgers (#240)

**Decision**: Neo4j holds only real memory for the embedding lane: `Element:Episode`, `EmbeddingVector`, `Meta` and the rest of the memory labels. The `EmbeddingAttempt` and `Outbox` labels that the lane used as ledgers are gone, together with their `embedding_attempt_id` constraint and `outbox_pending` index; `remember` no longer CREATEs an Outbox row for embedding. Work is discovered by what is missing, not by what was queued: an Episode needs embedding when no `EmbeddingVector {episode_id, profile_id}` exists for the active profile, served by a new composite index on `(episode_id, profile_id)`. Attempt and quarantine state lives in one JSON file, `embedding-state.json` under the runtime root (`ANAMNESIS_RUNTIME_ROOT`), written atomically (`.tmp` then rename) and held in memory only when no path is configured (tests, scripts). Ledger schema v1:

```text
{ "version": 1,
  "episodes": { "<episode_id>": {
      "profile_id": "<64 hex>",
      "state": "deferred" | "quarantined",
      "deferrals": <provider_unavailable deferrals so far>,
      "retry_after": <epoch ms> | null,
      "attempts": [ <RpcEmbeddingAttempt>, ... most recent last, capped at 16 ] } } }
```

An entry exists only for an Episode that has no vector for its profile; a successful embedding deletes it together with the Episode's earlier failed attempts (their operation IDs then report `unknown`). An Episode that already holds its profile's vector is never sent to the provider again: `embedding.recover` under any further operation id answers with the attempt that wrote the vector, the submitted id did no work and is recorded nowhere, and a lingering entry of that profile for the Episode is deleted (another profile's entry is that profile's history and stays). Every drain with a provider configured first reconciles the ledger against the graph (an entry whose Episode holds the vector of the entry's own profile, as a process loss between the vector commit and the entry's deletion or a seeded ledger leaves it, is dropped; with embeddings disabled the drain returns before reading the ledger), and only the active profile's entries hide an Episode from the missing-vector scan: another profile's entry is superseded by the first attempt under the active one. Nothing "pending" is persisted: an attempt in flight is not written down, and process loss means the missing-vector scan simply finds the Episode again. Outcome rules keep the existing `RpcEmbeddingAttempt.reason` values: `provider_rejected` (any non-transient HTTP status), `input_too_large`, `profile_mismatch`, `invalid_vector` and `stale_input` quarantine on the first attempt; `provider_unavailable` (timeout, 408/425/429/5xx, dead socket) defers with `deferrals + 1` and `retry_after = now + min(30 s · 2^(deferrals-1), 1 h)`; after `EMBEDDING_MAX_DEFERRALS = 8` the next transient failure quarantines as `provider_unavailable_exhausted`. `embedding.recover` never exhausts. `embedding.requeue {limit, reasons?}` deletes quarantined entries of the active profile in episode-id order, filtered by the latest attempt's reason when given, and returns `{requeued: n}`; entries whose Episode already holds a vector are dropped uncounted, and a second call with nothing left returns `{requeued: 0}`. `embedding.status` answers from the ledger first and otherwise synthesizes a `succeeded` attempt from the `EmbeddingVector` that carries the `operation_id` (the vector now also records model, incarnation, dimensions and timestamps); recover stays idempotent by `operation_id`, and the same ID bound to another Episode or profile is an `idempotency_conflict`. The RPC shapes and the public `recoverEmbedding` / `embeddingStatus` / `requeueQuarantinedEmbeddings` / `drainEmbeddingOutbox` surface are unchanged; `Store.pending`, `Store.markProcessed`, `Store.requeue`, `Engine.digest`, `Engine.requeueEpisodes` and `ElementWriteOptions.enqueue` are removed. `status.pendingOutbox` keeps its name and now counts Episodes lacking a vector for the active profile (any profile when no provider is configured) minus quarantined entries.

**Alternative**: (a) keep `EmbeddingAttempt`/`Outbox` in Neo4j and just tidy their indexes; (b) move the ledger into a separate database or SQLite file; (c) persist in-flight "pending" rows so a crash can resume the exact attempt.

**Reason**: Neo4j is the memory; ledgers are operational state. The work itself is regenerable from the graph plus the provider (an Episode without a vector is rediscovered by the scan); the recorded failure reasons and quarantines are not, and losing the file makes every quarantined Episode eligible for the lane again, which is the accepted cost. Keeping them as labels made backups, verify and the docs/01 layer model lie about what is semantic. (b) adds a second store and a second failure mode for a few hundred bytes per quarantined Episode; a JSON file with atomic rename is enough and is readable by an operator with `cat`. (c) buys nothing: the missing-vector scan already rediscovers an Episode whose attempt was lost, and a vector is idempotent per `(episode_id, profile_id)`, so the only durable facts worth keeping are the reasons an Episode is still missing one.

## D55 — Extraction ledgers leave Neo4j (#G3-PR)

**Date**: 2026-10-04

**Decision**: The five labels the extraction lane used as its ledger, `ModelTask`, `ExtractionAttempt`, `ExtractionPipeline`, `ExtractionJudgeInput` and `ExtractionDisposition`, are gone from the graph together with their seven constraints (`model_task_id`, `model_task_work_key`, `extraction_attempt_id`, `extraction_pipeline_id`, `extraction_pipeline_judge`, `extraction_judge_input_id`, `extraction_disposition_key`). Neo4j keeps `ExtractionGeneration`, `ExtractionCoverage` and the memory a pipeline materializes. Pipeline state lives in one JSON file, `extraction-state.json` under the runtime root, written atomically through a temporary name and rename, serialized, and schema-validated on load and on every write. One entry per pipeline holds the claim task, its terminal attempt, the judge task, judge input and judge attempt, up to 64 decisions, every attempt ever recorded for the pipeline (capped at 32) and the sequence at which coverage sealed the source. Work is discovered by what coverage has not sealed, Episodes whose `ingest_seq` is above `ExtractionCoverage.covered_ingest_seq`, and the existing task for an Episode is resolved by work key `<generation_id>:<source_id>`.

The file is bounded. An entry is deleted once coverage sealed its source for its generation and materialization is terminal for it (custody written with the relation judge disabled, complete or omitted, or a sealed omission for a failed or cancelled stage); an entry with a pending relation judge waits for the verdict; a standalone task follows the same rule with a terminal attempt in place of custody. Every transition writes one structured log line (`extraction.task.created`, `extraction.task.leased` before the provider call, `extraction.attempt.recorded`, `extraction.task.settled`, `extraction.task.retried`, `extraction.task.cancelled`, `extraction.pipeline.materialized`, `extraction.pipeline.pruned`) carrying `pipeline_id`, `generation_id`, `source_id`, `task_id` and `kind`; on the production host those lines land in journald and are the durable record. The RPC shapes in `@anamnesis/protocol` are unchanged; the values are built from the journal entry. The `ExtractionStore` method set, CAS, lease epochs, attempt budgets, replay of identical completions and every fence (`attempt_conflict`, `stale_input`, `lease_expired`, `invalid_transition`, `coverage_frozen`, `policy_denied`, `span_mismatch`, `provider_mismatch`) keep their semantics against the journal for a task whose entry is present; the one exception, a task whose entry was pruned, is stated below. Production migration drops the seven constraints and deletes the five labels in batches; nothing is seeded, because unsealed pipelines are simply re-driven by the scan and materialization is idempotent.

**Alternative**: (a) keep the five labels and only trim their indexes; (b) seed the journal from the graph at migration so in-flight pipelines keep their attempt history; (c) keep every entry forever so `extraction.audit.status` never forgets.

**Reason**: Neo4j holds memory, not pipeline bookkeeping; the labels were about 400k nodes of operational state that made backups, verify and the docs/01 layer model describe scheduling as if it were semantic. D54 already did the same move for the embedding lane and the pattern held: an atomic JSON file beside the runtime root, work rediscovered from the graph, failure history as the only thing that is lost with the file. (b) buys nothing, since the roughly 130 unsealed sources at cutover are redone idempotently. (c) would grow the file with the corpus (about one pipeline per Episode); pruning sealed entries keeps it proportional to in-flight work, and the audit log already records every terminal transition.

**Trade-off accepted**: `extraction.audit.status {pipeline_id}` answers `unknown` for a pipeline whose entry has been pruned, where it used to return the sealed tasks and decisions from the graph, and every `ExtractionStore` transition on one of its tasks (`retryModelTask`, `cancelModelTask`, `settleModelTask`, `leaseModelTask`) throws `unknown_ModelTask` where a sealed task used to reach the `coverage_frozen` fence; both answers refuse the retry, and the scheduler only ever transitions tasks of a pipeline it has just read. Operators who need the history of a sealed pipeline read the audit log for its `pipeline_id`. The journal keeps an entry per pipeline in flight: a sealed omission keeps its entry only until both coverage partitions pass its source (the single tombstone-free rule), so the file size follows in-flight work, not the failure history. The migration refuses to run while a generation's two coverage cursors disagree (`check_coverage`): with no seeding, the lagging partition could neither read the deleted ledgers nor recreate the task behind the leading partition's fence; the previous daemon build reconciles the cursors from the ledgers first.

## D56 — Raw archive as collection SSOT, Episodes as provenance only, Fact-first recall, windowed extraction (#G1)

**Date**: 2026-10-11

**Decision**: Collection has a source of truth outside Neo4j: a raw archive
that keeps every original file byte-for-byte under a content address and
turns every source message into one normalized record with its verbatim
text, its origin (source, session, actor, record), its sequence, its role
and its occurrence time with precision and kind. The archive reduces the
input in exactly two ways: tool-call and tool-result parts are dropped, and
byte-identical duplicate sessions collapse into one record set that keeps
both origin paths. Nothing else changes on the way in. There is no masking,
no truncation, no dedup of merely similar messages and no distinction
between machines; a session is keyed by agent kind plus session id.

The engine admits every archive record as its own Episode, unfiltered and
unembedded. One source message is one Episode, Episodes are never merged,
and the pre-admission filtering adapters retire. Episodes are provenance
and context only and are never a vector target. The Episode embedding lane,
the `vec_episode_<profileId>` index and the `originals-hybrid-v1` increment
retire once Fact vectors ship, and the activation coverage gate of an
extraction generation (`selected_model_embedding_coverage`) then counts the
Fact vectors of that generation instead of source-Episode vectors.

Recall is Fact-first. Vectors live on Facts in `vec_fact_<profileId>`; the
vector channel searches Facts and resolves their source Episodes through
`DERIVED_FROM`. The response carries a bounded same-session window around
each source Episode, by default two Episodes before and two after under the
same session key, inside the D44 budget, so the caller reads the memory in
its original context.

Extraction submits windows instead of single Episodes: six to eight
same-session Episodes plus two or three context Episodes per provider call,
while every Episode in the window keeps its own `DERIVED_FROM` attribution.
A cheap pre-gate scores Episodes from 0 to 1 and skips windows below a
threshold. The gate is `JevGate`, typesafe-ai/jev through the Vercel AI
Gateway, with the model id fixed at construction and any other id refused;
its key is read from a file under `~/.config/anamnesis` and never logged.
The custody and restart semantics of `extraction-state.json` (D46, D49, D50,
D55) don't change.

D43 through D47 and D49 stay normative. D50 was later superseded by D60. D56 supersedes the parts of
docs/05 §2 and of docs/01 "The embedding stream" that name Episode vectors,
and the per-Episode extraction call shape in docs/02.

**Status**: Fact vectors in `vec_fact_<profileId>`, the bounded same-session
recall window, and the Fact embedding coverage gate shipped in this PR.

**Alternative**: (a) keep Episode vectors beside Fact vectors, paying the
embedding cost twice and running two coverage gates; (b) filter at
collection time, which loses provenance and makes later re-extraction
impossible; (c) merge turns into one Episode, which breaks per-message
provenance and the D49 lineage; (d) send every window to Haiku without a
gate, so cost scales with chatter.

**Reason**: the archive already exists outside the engine, and provenance
has to survive any future re-extraction, which only an unreduced record set
guarantees. Recall quality comes from Facts read with their source context,
not from similarity over raw messages. Batching cuts the per-call overhead
of extraction, and the gate drops the windows that would produce no Facts.

**Trade-off accepted**: a cold archive is re-admitted in full; no pre-filter
saves ingest cost. Recall can't find a message that produced no Fact except
through the BM25, session and exact channels. A wrong gate threshold skips
material silently, so the gate score is stored per window in
`extraction-state.json` for audit. Fact vectors are generation-scoped, so a
generation cutover re-embeds.

## D57 — known_by as a single ingested_at predicate (narrow D4 exception)

**Date**: 2026-10-17

**Decision**: `RpcRecallParams.known_by` (same type as T) restricts a recall
to what the engine had ingested by that time: only Episodes and Facts with
`ingested_at <= known_by` are served. INVALIDATES links carry `ingested_at`,
set to the invalidating element's `ingested_at` (null means always known),
and a link not yet known at `known_by` does not invalidate its target. That
is the whole mechanism: one predicate over one stored timestamp, evaluated
with the existing `visible(T)` and `valid(T)` checks. There are no
bitemporal intervals, no per-element valid-from/valid-to and no record-time
axis to recompute on a generation switch or correction. Without `known_by`
nothing changes. D4 stays normative for everything else.

The same change ships the rest of the recall-serving contract: Fact
validity and `provenance.supersedes` at assembly (#263), the Fact score
with `sigma_fact = 30` and the source-mean `U_Fact` (#267), and the `time`
window with `order` (#269); docs/05 §1 and §6 carry the details.

**Status**: shipped with #263, #267, #269 and #270.

**Alternative**: (a) keep D4 untouched and answer "what did the system know
then" only from receipts and the audit log; (b) full bitemporal storage with
a record-time interval on every element and link.

**Reason**: a caller replaying an earlier session needs the memory as it
stood then, which receipts can't reconstruct for a query that was never
asked. `ingested_at` is already written once on every Episode and Fact, so
a `<=` comparison costs one predicate per row and one property per
INVALIDATES link. (b) buys exactness on the record axis at the price D4
rejected.

**Trade-off accepted**: `known_by` is a lower bound on knowledge, not a
replay. Policy is still current policy, the active generation is still the
current one, and an element re-extracted after `known_by` under a newer
generation is not projected back. A link whose `ingested_at` is null is
treated as always known, so links written before this change can't be
excluded by `known_by`.

## D58 — Episode room/space labels and reply links are independent caches

**Date**: 2026-11-19

**Decision**: add `Episode.room` and `Episode.space` outside the immutable
`properties` JSON and digest. Resolve rooms from `properties.room`, falling
back to session, and spaces from an explicit daemon-managed Neo4j
`RoomAssignment(source, room, space, name?)` table; default `unknown:<source>`.
The table is mutable operator control, retained by the ordinary database dump.
One shared writer applies labels at admission and during indexed, paged rebuild.

Extend D21's closed lattice with `REPLIES_TO`, Episode to Episode, a
generation-free, nonconducting cache alongside session topology. Its key hashes
only endpoint IDs and role. Explicit reply addresses and Slack thread-parent
addresses resolve to the latest admitted Episode at that address. Missing
targets are counted and retried by an operator rebuild. No original, lineage,
NEXT_EPISODE edge, conducting cache, serving revision or recall result changes.

**Status**: implemented with `spaces.import/list/rebuild` and `replies.rebuild`
RPCs and corresponding operator commands. See docs/01 §5 for cursor, import,
resolution and backup contracts.

**Alternative**: filesystem roots/aliases inferred from `cwd`; immutable space
inside Episode properties; reply edges folded into NEXT_EPISODE or PPR.

**Reason**: existing Episodes already contain complete immutable evidence.
Explicit assignments can change independently, need atomic admission lookup,
and must survive backup. Graph storage reuses the writer fence and dump rather
than adding a second authority file. Room and ingest indexes bound each rebuild
transaction and make one-room reassignment independent of corpus size.

**Trade-off accepted**: the first labels backfill scans the corpus once before
room-scoped rebuilds can see historical rooms. Reply passes scan indexed Episode
pages; targets arriving later require another pass. Address collisions select
the greatest ingest sequence because the loader address contains neither actor
nor revision. The operation is deterministic for the current stored corpus,
not a historical reply-version assertion. Recall scope and Fact labels remain
outside this decision.

## D59 — Identity: accounts from Episodes, persons from judged merges, one rebuildable generation

**Date**: 2026-11-21

**Decision**: derive one `Account` per `(origin_source, origin_actor)` from
Episodes, and group accounts into `Person`s owned by one identity generation.
Rules decide owner accounts, shared directory emails and overlapping accounts
in one conversation; an LLM judge decides the rest; an operator decision
overrides both. Rule outcomes are recomputed and never stored. Stored decisions
are CREATE-only with authority `judge` or `operator`. Grouping is a pure
function rebuilt into a new generation and switched in one transaction. A
judge merge needs confidence 0.85; `unsure` stays separate. The layer is
operator data outside Element kinds and the Link lattice; recall does not read
it yet.

**Status**: implemented with the identity lane, `identity.*` and `people.*`
RPCs, operator commands, directory import and provider-backed judging. See
docs/01 §4 and docs/02 §5.5.

**Alternative**: merge Entities in place, require human confirmation for every
merge, or run LLM entity resolution during extraction.

**Reason**: Facts already carry account-level speaker identity, so grouping is
a rebuildable view over immutable Episodes. A wrong merge is repaired by a new
generation without rewriting Facts. Rules and co-presence remove obvious pairs
before a model call.

**Trade-off accepted**: co-presence is interval overlap within a session, so
two accounts of one person active in one room at the same time stay apart.
The scan ignores policy because accounts are operator data; text sent to the
judge passes extraction policy. Entity links and Person-aware extraction and
recall remain later work.

## D60 — Production Facts come only from extraction

**Date**: 2026-11-28

**Decision**: Remove the D50 shadow adjudicator, its proposal and review
contracts, and the unused bound generation identity receipt. No production
producer creates shadow proposals and no RPC reaches the adjudicator.
Production Facts come only from the extraction pipeline, including its D53
relation judge. Cutover and coverage keep their existing unbound contracts.

Remove the unused HubArc path and drop its index and the obsolete D50
constraints on store initialization. Keep ConductingArc maintenance and its
bounded probes. Saturated sources contribute no outgoing traversal; adding
a qualified hub shortlist is deferred.

## D61 — Remember commits directly to Neo4j (2026-11-28)

**Decision**: Remove the deferred ingestion queue and pre-commit delivery
reservations. Neo4j commit is the acknowledgement boundary for `remember`
and each delivery in `remember.batch`. Object uploads remain independent.

**Reason**: No repository client produces lineage-free ingestion that uses
the queue. The corpus loader already bypasses it by sending direct lineage.
Production reported an empty queue on 2026-11-23. The Neo4j commit supplies
durability, and Episode `revision_key` uniqueness plus stored-row digest
comparison give idempotent retries without filesystem reservations.

**Client contract**: Retry `storage_unavailable` with the same request. No
failed write is accepted for later processing. If a batch fails after
committing a prefix, that prefix remains committed. A committed exact retry
returns `created: false`; a different body with the same key is refused.

**Startup**: Ignore absent or empty legacy `spool/` and `deliveries/`
directories. Refuse startup if the legacy spool is nonempty, with an error
naming the directory. Preserve it for reconciliation rather than silently
dropping acknowledged writes. Leftover delivery files are only pre-commit
reservations and may be removed at startup with a log event.

## D62 — Record recall selection and one explicit outcome (2026-11-28/29)

Every successful recall creates one `Usage` in the selection transaction,
including an empty selection. Its server UUIDv7 `usage_id` replaces `recall_id`
and `expires_at` in the result. Keep `created_at`, `policy_revision`, canonical
ordered `{id,sources}` rows, and the lineage rows, selection digest and connection
binding used by D49. Selection is sufficient to attribute credit; the database
does not retain a second copy of the response, query, vectors or budget. A
socket completion has no durable effect. D43's protected-write barrier and D44's
packing rules still apply.

`report_outcome {usage_id,idempotency_key,reward}` takes a UUIDv7 key and finite
reward. Authorize all credited Episodes under current policy. Atomically create
one `UsageOutcome` with unique usage and key, clamped reward and `reported_at`.
Return `{reward,applied}`. An identical normalized replay returns false; reusing
a key with another usage or reward raises `idempotency_conflict`; a second key
on a reported usage raises `outcome_already_reported`. An absent usage raises
`unknown_usage`. Denial or unavailable storage never partially applies credit.

Normalize reciprocal rank over selected primaries, split each share equally
among its source Episodes and sum shared sources. For each Episode append one
outcome `Hit` with credit `0.2 * clamp(reward,-1,1) * share`, canonicalizing
negative zero. Empty usages produce no Hits. Fold by `(t,id)`: decay utility
with `2 ** (-elapsed / 30 days)` before each addition, clamp after each event,
and decay the cached anchor again at read. The same signed reward times share
updates stability with the existing testing-effect arithmetic and stability
bounds. Mass remains a read-time calculation; ranking retains `1 + 0.25 U`.

This supersedes the receipt and adoption contracts of D44/D45 and D49's parent
storage contract. `lineage_mode: "receipts"` and `parent_recall_ids` retain
their spelling and digest bytes. Resolve parent IDs against Usage, preserving
binding, time order, digest, authorized roots and bounded union. Remove commit
mode negotiation, expiry, transport, feedback and automatic exposure. A store
recorded before schema 2 refuses readiness on legacy receipt-family nodes or
old-format Hits (the production census found none); the check runs once, and a
store recorded at schema 2 starts without rescanning. Keep the existing archive
format.
