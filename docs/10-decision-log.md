# 10 — Decision Log

Decisions from the September 2026 design review. Each entry is **decision /
alternative / reason**. "The proposal" is the twelve-section design text
under review; "the previous drafts" are the retired docs/00–11 (2026-08).

D43 through D47 come from the design finalization of 2026-09 (baseline
bc372ca). Where they supersede an earlier entry, the earlier entry stays in
place with a **Status** line naming what changed and what still holds. The
original rationale is kept so the reasoning can be audited, not to be read as
current policy. Affected: D14 (clarified), D27 and D40 (outcome no longer
touches `S`), D41 (clarified), D42 (span and intra-Episode rules narrowed).

## D0 — main retired, anamnesis2 is the mainline, previous drafts retired

**Decision**: `main` (Rust, SQLite, trace-native roadmap) is retired. The
previous drafts docs/00–11 on `anamnesis2` are retired as well and replaced by
this document set.

**Reason**: the previous drafts treated Neo4j as "one store among several"
and reserved a custom storage engine. The proposal fixed Neo4j as the only
graph store, and on top of that the previous data model (payload bytes as
base64 on a node, Hits on arbitrary Elements, a single `recall_revision`)
conflicts with the decisions below. Rewriting is shorter than patching.
Comparison and field lessons remain as non-normative material in
`docs/background/`.

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

**Decision**: `fanout₁ = clamp(⌊640/|S|⌋, 4, 32)`,
`fanout₂ = clamp(⌊1232/|H₁|⌋, 2, 16)`; a hop that still exceeds its budget
because of the clamp minimum is truncated to the budget by
`coalesce(m_cache,m0) DESC, id DESC` (docs/06 §1).

**Alternative**: the proposal — fixed 32/16.

**Reason**: 128 seeds × fanout 32 = 4,096 > the 2,000-node limit. A fixed
value either overruns or starves the budget depending on the seed count. The
budget is the invariant; fanout is derived.

## D8 — fanout tie-break is `id DESC`

**Decision**: `w_role DESC, coalesce(m.m_cache,m.m0) DESC, m.id DESC`
(docs/06 §2). `m0` is the total fallback before maintenance.

**Alternative**: `id ASC` (uniform with every other ordering).

**Reason**: UUIDv7 is time-ordered. ASC truncation systematically discards
recent memories. Truncation bias cannot be eliminated, so we choose the one
that keeps the recent. The `id ASC` used elsewhere (results, PPR list) breaks
ties without truncating, so it carries no bias.

## D9 — wall-clock deadlines guard Neo4j channels, never PPR

**Decision**: a candidate channel transaction over 50 ms drops that channel;
the envelope transaction over 100 ms drops PPR. There is no deadline on the
bounded PPR iterations (docs/05 §2/§4–5, 06 §5).

**Alternative**: the proposal — a 50 ms wall-clock deadline on the whole PPR,
dropping PPR when exceeded.

**Reason**: the time is spent in Neo4j round trips; PPR is a few ms. A
deadline on PPR makes the same input produce different output under load
(breaks determinism). The principle of never running PPR on a partial
envelope stands — partial results are silent bias.

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

**Decision**: every Fact materializes 1–16 source Episode IDs.
`κ_eff = κ/|sources|`; the same namespace and Episode merge into one Hit with
`kappa_eff ≤ κ` (docs/01 §1, 04 §5).

**Alternative**: full κ per source.

**Reason**: if adopting a well-sourced fact reinforces more in total, the
number of sources dominates mass. Conservation keeps "one adoption = κ worth
of attention"; materialization makes the lookup total and bounded.

## D13 — commit is recomputed by the server; auto/receipt modes

**Decision**: the client sends only `adopted[]`. Mode is declared in hello;
explicit commits from auto clients are rejected (docs/04 §6).

**Alternative**: the client computes κ or S′ and sends it; no modes, every
client may commit.

**Reason**: ledger integrity has to be guarded by one server. A client that
cannot observe adoption (a context-injection harness) reporting adoption pollutes
the ledger. Exposure's low κ is the price of that uncertainty.

**Status (2026-09 finalization, D45)**: server-side recompute and the two
modes hold. Two details above are historical. The client now sends
`adopted[]` and, optionally, one `reward` per recall against a receipt, and
both are checked exact-once against the RecallReceipt. Exposure no longer
carries a "low κ"; it is audit-only and moves neither `S` nor `t_last_hit`.
The price of auto-mode uncertainty is that auto clients never reinforce at
all, not that they reinforce weakly.

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

**Decision**: Entity caches the minimum visible mention time; Community stores
the majority threshold captured from its pinned member snapshot. Links remain
visible through their endpoints (docs/03 §3).

**Alternative**: evaluate MENTIONS and HAS_MEMBER subqueries on every recall.

**Reason**: nested visibility scans defeat the envelope's work bound.
Thresholds are rebuildable derived/cache values, and a Community generation
pins the extraction snapshot from which its threshold was computed.

## D16 — lifecycle generations with monotonic catch-up and atomic cutover

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

**Decision**: docs/01 §4, §8. GC refuses ACTIVE, BUILDING,
CATCHING_UP and the configured rollback target. RETIRED generation deletion
does not bump the serving revision.

**Alternative**: automatic deletion on switch.

**Reason**: automatic deletion removes rollback. Disk is cheap and rollback is
expensive. A single DELETE path is easy to review.

## D18 — corrections are backdated (`C.time := B.time`)

**Decision**: docs/03 §5.

**Alternative**: corrections carry their own event time plus a flag.

**Reason**: it follows directly from Fact.time being "effective time". A
correction says "B was wrong from the start"; if B looked valid in
snapshot(B.time ≤ T < C.time) the definition would be betrayed. A flag puts a
branch in every snapshot query.

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

**Decision**: docs/02 §4, §9. Remember fsync-spools until the global
capacity/free-space boundary, then returns retryable `resource_exhausted`;
recall returns a diagnosed empty success.

**Alternative**: recall reads the spool for a partial answer; remember returns
failure.

**Reason**: fsync-before-ack prevents silent loss, but no system can promise
success under ENOSPC. Recalling from the spool would create a second search
path, which is a second store.

**Status (2026-09 finalization, D43–D45)**: recall's diagnosed empty
success holds, with a narrower meaning: it is a diagnostic-only reply with no
items, no context, no budget use and `recall_id: null`, so nothing about it
can be committed (docs/05 degradation ladder). That "no memory available"
exception is distinct from serving under unknown policy, which is never
allowed. Everything that needs receipt or policy authority fails closed with
a named retryable error: `commit`, `policy.set` and `policy.revoke` return
`storage_unavailable` while Neo4j is down, a recall or commit without
loadable policy state returns `policy_unavailable`, and a recall whose
receipt can't be persisted returns `receipt_unavailable` rather than
unreceipted results. A policy command never reports an unenforced deny as
active (docs/02).

## D23 — valid(T) only at assembly

**Decision**: candidates and the envelope use `visible(T)` only; `valid(T)` is
applied at result assembly (docs/03 §7, 05 §6).

**Reason**: an invalid Fact still conducts — the Entities it connects are
still relevant. Removing it at the candidate stage cuts off the whole
neighborhood of a corrected topic. Removing it from the results while exposing
it under `supersedes` explains "why that fact is not showing".

**Status (2026-09 finalization, D43)**: validity stays an assembly-time
filter. Policy does not: the current policy is a hard filter at every stage,
candidates, seeds, envelope conduction, companions, assembly and provenance
text, under the policy-revision barrier. "Candidates and the envelope use
`visible(T)` only" now reads "`visible(T)` and not denied by current
policy"; a denied element neither surfaces nor conducts.

## D24 — role weights only; each retained row normalizes itself

**Decision**: links carry no per-link `weight`. PPR transition strength is a
finite positive per-role constant `w_role`. `Z_i` sums weights over the
retained arc multiset; `W_ij` sums every parallel arc from i to j and divides
by `Z_i` (docs/01 §5, 06 §4).

**Alternative**: keep a per-link weight in (0, 1] and normalize by the
unweighted count, as the first draft of this document set did.

**Reason**: count normalization plus sub-unit weights leaked mass even on a
full graph, while physical-degree normalization counted retired and
snapshot-hidden links that GDS correctly excluded. Retained-row normalization
uses the same edge universe in both solvers for any positive role weights.
Removing per-link weights also keeps ordering and calibration small. Found by
the PR reviews of 2026-09.

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
`previous_revision_key`. An
`OriginHead` CAS plus explicit `previous_revision_key` serializes the immutable
revision chain (docs/01 §1, 02 §3).

**Alternative**: derive revision identity from content digest.

**Reason**: digest identity cannot represent A→B→A: the last A resolves to the
already-invalidated first A. It also missed changes to payload, time, schema
and properties. Occurrence identity permits a true revert; the canonical
digest detects conflicting retries of that occurrence. Found by the PR
reviews of 2026-09.

## D27 — one commit path for every Hit

**Decision**: a single internal `commitHits(namespace, kind, elements, κ_of?)`
with five producers — receipt `commit` RPC (adoption `recall_hit` and signed
`outcome`), post-response exposure, extraction re_mention, dreaming promotion —
each with its own namespace (docs/04 §5–6). The recall request handler writes
nothing; exposure runs after the response. The signed `outcome` reuses this same
function — `κ_of` supplies a per-result rank-decayed κ that may be negative, and
the negative branch of the S update lives beside the positive one (D40).

**Alternative**: the first draft described the producers separately and
contradicted itself ("dreaming never creates Hits" vs "promotion Hits";
"recall is read-only" vs "auto recall records exposure").

**Reason**: the ledger's invariants (idempotency, cache validation, server-side
S′) must be enforced in one place, and the invariants in docs/00 must be
literally true. Found by the PR review of 2026-09.

**Status (2026-09 finalization, D45)**: the single commit path holds. The
sentence about a negative `κ_of` and a negative branch of the S update is
historical: `outcome` still flows through `commitHits` for idempotency and
audit, but it never modifies `S` or `t_last_hit`. It feeds the separate
utility `U`.

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

**Decision**: an hourly job with no LLM and no GDS computes `m_cache` and the
indexed `HubArc` shortlist cache nodes; it ships in the same version as PPR
(docs/02 §6, 06 §3, 09).

**Alternative**: the first draft placed both in v0.3 dreaming while v0.2 PPR
already depended on them — hubs would not have expanded at all and fanout
ordering would have degenerated to role weight then id.

**Reason**: the envelope's two bias controls must exist from the first
version that has an envelope. Neither needs the expensive parts of dreaming.
Found by the PR review of 2026-09.

## D30 — hard bounds on every recall stage, including query work

**Decision**: channel sizes are fixed (vector 64 nodes + 16 relationships,
BM25 64, session 32 Episodes + 64 Facts, identity 18 → ≤ 274 candidates);
assembly handles ≤ 530 elements and at most 16 materialized source Episodes
per element. The session channel performs 32 composite index seeks and returns
at most two non-synthesis Facts each. Candidate indexes use generation-specific
partitions, `k_fetch≤256` and per-channel deadlines. Entity/Community
visibility is one threshold comparison plus one Entity witness row read. The
hub test is a probe that stops at 256 relationships. Envelope expansion scans
fewer than 256
relationships for a non-hub; a hub uses at most 32 cached link tuples. The
final link query returns at most `L=10` directed arcs per row and never expands
hub adjacency. Generation-scoped indexes isolate hidden data before top-k
(docs/05 §2, §6; 06 §1–2).

**Alternative**: the first draft bounded the PPR arrays but left the session
channel's Fact fan-out and the induced-link query unbounded.

**Reason**: "bounded" has to mean bounded work, not only bounded output. Found
by the PR review of 2026-09.

## D31 — security and durability rules are normative

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

**Decision**: every Community generation stores
`source_extraction_generation`, `source_covered_ingest_seq`,
`source_structure_revision` and an ordered export digest. HAS_MEMBER
belongs to the Community generation but may target only members from that
pinned extraction generation and covered prefix. The GDS export and write
transaction both enforce the pins. An extraction cutover sets the Community
selector to null; dreaming later switches a newly pinned generation.

**Reason**: independent selector integers cannot be required to match, and an
old Community must not follow hidden extraction endpoints after cutover.

## D33 — HubArc cache and source-local directed truncation

**Decision**: hubs use indexed `HubArc` cache nodes, not list-of-map
properties. HubArc has one schema
`{hub_id, rank:0..31, link_id, neighbor_id, role, stream, generation?,
source_extraction_generation?}`. Consumers filter all 32 for eligibility,
then take the first `L`; Link deletion removes matching HubArcs atomically.
Each source row selects at most `L` directed arcs independently; the CSR does
not force a selected physical link into the reverse row (docs/06 §2–3).

**Reason**: Neo4j properties cannot store lists of maps. Forced symmetric
insertion also let many leaf selections overflow a hub row and contradicted
the dangling rule. Source-local arcs preserve the planner-independent
`|V|·L` bound; envelope validation measures the asymmetry cost.

## D34 — visibility thresholds are materialized

**Decision**: Entity caches its earliest mention time and Community stores the
majority threshold captured from its pinned member snapshot. Recall evaluates
both with one property comparison (docs/03 §3).

**Reason**: evaluating Entity EXISTS and Community member counts inside every
envelope row made the stated inspection bound false.

## D35 — synthesis inputs equal the bounded support set

**Decision**: a synthesis LLM receives exactly 1–16 non-synthesis Facts under
a pinned serving revision.
Those same IDs become `support_fact_ids` and semantic DERIVED_FROM links; the
write transaction revalidates their selectors, covered prefix and validity.

**Reason**: if the LLM consumed a larger bundle than the stored support set,
an omitted contributor could become invalid while its conclusion remained
valid. Exact bounded support makes invalidation one level and total.

## D36 — elapsed time is nonnegative and server writes use logical time

**Decision**: every forgetting interval is `max(0,t₁−t₀)`. Write timestamps
use `max(wall_clock, Meta.last_server_time+1)` and replay retains the maximum
seen Hit time (docs/02 §1, 04).

**Reason**: wall-clock regression or imported pre-ingestion Hits must never
make retention exceed one, make the power-law base negative, or reduce
stability.

## D37 — Neo4j and filesystem epochs fence stale daemons

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

> **Status: superseded in part by D45 (2026-09 finalization).** What
> survives: the `commit` RPC accepts `reward ∈ [−1,1]` per recall, the
> verdict is idempotent on the recall UUID, and it's the negative label the
> refitting sample needs. What's withdrawn: the negative branch of the S
> update, the floor at `S0(m₀)`, and `κ_signal = reward/(rank+1)` as a
> reinforcement quantity. Outcome no longer touches `S` or `t_last_hit` at
> all; it feeds a separate per-Episode utility `U` with rank-weighted
> attribution recorded in the RecallReceipt. The text below is kept as the
> original reasoning.

**Decision (historical)**: the receipt `commit` RPC accepts `reward ∈ [−1,1]`, a verdict on
whether the recalled context led to a good result. It becomes an `outcome` Hit
per source Episode with `κ_signal = reward·/(rank+1)` — rank-decayed so the top
result carries the most credit or blame — and drives a negative branch of the S
update, `s′ = max(S0(m₀), s·(1 + d·κ_eff·(1−R_hit)))`, floored at birth
stability. Every other Hit kind stays strictly positive. The verdict spans all
results of the recall and is idempotent on the recall UUID (docs/04 §5.1, §6,
§9–§10; docs/05 §6, §10).

**Alternative**: adoption (`recall_hit`) as the sole receipt signal. That only
ever raises stability, so a memory that keeps surfacing and keeps producing bad
answers is reinforced by its own exposure, and §9 refitting has no negative
label — adoption cannot distinguish "not shown" from "shown and wrong".

**Reason**: the loop from a recall to its downstream result is the one signal a
pure engine can accept from an external caller without taking on harness
concerns — a bounded scalar against a recall UUID, no per-item labeling, no
prompt framing. It is FSRS's "again" grade, which the ledger otherwise lacks,
and it supplies the negative half of the refitting sample. The floor at
`S0(m₀)` keeps a penalty from erasing a memory: a bad outcome demotes toward
"just learned", and forgetting still happens only through elapsed time.
Adapted from memkraft's accountable outcome loop (usage_id → report_outcome →
rank-decayed credit); anamnesis attributes it to the immutable Hit ledger and
replays it like every other kind rather than storing a mutable utility score.

**Why it was changed**: coupling a downstream verdict to `S` made one number
answer two questions, "how accessible" and "how useful", and the penalty path
rewound the accessibility of every sibling Fact of a source because Hits are
Episode-scoped (D1). Keeping the ledger and the attribution but routing the
reward into `U` keeps the negative label without a second forgetting law.
The utility cache is still rebuildable from Hit outcome events, so the
"replay, don't store" intent survives.

## D41 — every recall result carries a closed `epistemic` grade derived from its producer

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

> **Status: superseded in part by D46 and D47 (2026-09 finalization).**
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

**Decision**: the Episode-only Hit ledger and derived max-source
accessibility stand (D1, D14). Only `recall_hit`, a confirmed adoption,
updates `S` and `t_last_hit`. `exposure`, `re_mention`, `promotion` and
`outcome` are audit-only for accessibility. Adoption uses the existing
positive formula with the dimensionless `(S / 1 day)^-c` factor, capped at
`S_max`; the cap makes reinforcement weakly, not strictly, monotone. Episode
`S₀` is initialized from the original `m₀` at ingestion. `m₀ =
confidence · prior(sub_kind) · prior(modality)` is immutable; a synthesis
carries a required explicit `modality` judged from its content and a
`confidence` that measures faithfulness to its support, never a default and
never a product of uncalibrated marginals. Raw model outputs and the
prior/config version are retained; priors change through a new derived
generation, never by replaying Hits over a stored `m₀`.

Outcome is a separate per-Episode utility, `U = (ν·μ₀ + Σ w·r) / (ν + Σ w)`,
with `ν = 4`, `μ₀ = 0` as illustrative defaults for calibration, `r ∈ [−1,1]`,
`w ≥ 0`. For the result set in the receipt (adopted IDs if present, else the
returned primaries; empty means no item attribution but the receipt-level
outcome is kept), rank weight `a_j = (1/(rank_j+1)) / Σ_l (1/(rank_l+1))`,
source share `b_je = 1/n` over the result's sources, `w_e = Σ_j a_j·b_je`, so
`Σ_e w_e = 1` with no extra cap that loses credit. Original ranks and exact
attribution are stored. Ranks are 0-based in receipts and 1-based in channel
RRF. Outcome never rewinds or resets time and never modifies `S`; zero and
missing are different values. Impressions live in append-only
`RecallReceipt` control records, not semantic Episodes: delivered primary and
companion IDs, source snapshots, policy/config/generation versions, selected
channels, ranking state, budget and result digests. Source IDs are immutable
and derived snapshots are bounded for replay; full raw text isn't duplicated.
Receipts enforce exact-once adoption per recall and source and exact-once
outcome per recall; a conflicting duplicate reward is rejected. TTL is
explicit config; an expired receipt rejects commit and absence is never a
negative label. The utility cache is rebuildable from retained Hit outcome
events after receipts expire.

Score: `score = relevance · max(m, 0.02)^γ · (1 + β·U)` with `m = m₀·A`,
`γ = 0.5`, `β = 0.25`, `β ∈ [0,1)`; RRF weights as before. `U_Fact` is the
mean of source `U`. Outcome-only events change `U`, not mass. All defaults
are assumptions to ablate. Policy and validity are hard filters, never
confidence adjustments (docs/04, docs/05).

**Alternative**: D40's negative branch of the S update; or a mutable utility
score stored on the Fact.

**Reason**: `S` models how readily a memory returns; a bad downstream result
says the memory was unhelpful, not that it's fading. Folding one into the
other made every sibling Fact of a source pay for one bad answer (Episode
attribution is collateral by design, and this decision says so instead of
claiming per-Fact selectivity). A stored per-Fact score violates D1. Keeping
outcome in the ledger, attributing it exactly once with weights that sum to
one, and reporting `U` as a proxy rather than a truth or causal claim gives
the refitting sample its negative label without a second forgetting law.
Comparisons in calibration are adoption-plus-negative against adoption-only,
never against no event, and a whole-recall reward is one label, not many.

## D46 — one Fact per occurrence, contradictions as bounded companion bundles

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
