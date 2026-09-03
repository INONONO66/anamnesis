# 10 — Decision Log

Decisions from the September 2026 design review. Each entry is **decision /
alternative / reason**. "The proposal" is the twelve-section design text
under review; "the previous drafts" are the retired docs/00–11 (2026-08).

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

**Reason**: at α = 0.85, 20 iterations give `0.85^20 ≈ 0.039` — the residual
cannot reach 1e-4, and the error bound is set by the final residual, not by
the iteration count. 64 is a cap that guarantees τ is reached
(`2·0.85^61 < 1e-4`) while runs stop at 20–30 in practice. The L1 tolerance of
7e-4 in 07 comes from this bound.

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

## D14 — Fact mass = max over bounded source Episodes · σ_fact

**Decision**: docs/04 §3.

**Alternative**: sum, mean, or per-Fact state.

**Reason**: per-Fact state conflicts with D1. A sum inflates with the number
of sources; a mean dilutes recent reinforcement. Max is monotone and
conservative and matches the intuition "as alive as its most recently handled
source". σ_fact models the gist outliving the detail.

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
uniform edges. A virtual source node σ carries weights `s_i`; the GDS vector
on V equals `α·p*` (docs/07 §2).

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

**Reason**: real types are what make `COUNT{}` and type-filtered expansion
O(1) and index-backed. Variety is absorbed by `RELATES_TO.content`, so there
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

## D23 — valid(T) only at assembly

**Decision**: candidates and the envelope use `visible(T)` only; `valid(T)` is
applied at result assembly (docs/03 §7, 05 §6).

**Reason**: an invalid Fact still conducts — the Entities it connects are
still relevant. Removing it at the candidate stage cuts off the whole
neighborhood of a corrected topic. Removing it from the results while exposing
it under `supersedes` explains "why that fact is not showing".

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
visibility is an O(1) threshold comparison. Envelope expansion scans fewer than 256
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
crash-consistency rules implicit will be implemented inconsistently; the
current code's default password is the example. Found by the PR review of
2026-09.

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

**Decision**: the receipt `commit` RPC accepts `reward ∈ [−1,1]`, a verdict on
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
