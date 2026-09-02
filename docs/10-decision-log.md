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
blob store. The cost is a two-part backup; because bytes are written and
fsynced before any transaction references them and are never modified,
"dump first, copy objects/ after" is consistent without pausing writes
(docs/01 §9). Confirmed by the owner in the 2026-09 review.

## D6 — `structure_revision` does not bump on Hit or cache writes

**Decision**: +1 only on Element/Link CREATE, generation switch, gen_to stamp
and derived DELETE (docs/02 §1).

**Alternative**: the proposal's `recall_revision` — incremented on every
write.

**Reason**: what recall must detect is *structural* change. If Hits, m_cache
and embedding backfill bump it too, every recall in a busy session is torn —
recall trips over its own exposure Hits.

## D7 — fanout is derived from the budget

**Decision**: `fanout₁ = clamp(⌊640/|S|⌋, 4, 32)`,
`fanout₂ = clamp(⌊1232/|H₁|⌋, 2, 16)`; a hop that still exceeds its budget
because of the clamp minimum is truncated to the budget by
`m_cache DESC, id DESC` (docs/06 §1).

**Alternative**: the proposal — fixed 32/16.

**Reason**: 128 seeds × fanout 32 = 4,096 > the 2,000-node limit. A fixed
value either overruns or starves the budget depending on the seed count. The
budget is the invariant; fanout is derived.

## D8 — fanout tie-break is `id DESC`

**Decision**: `w_role DESC, m.m_cache DESC, m.id DESC` (docs/06 §2).

**Alternative**: `id ASC` (uniform with every other ordering).

**Reason**: UUIDv7 is time-ordered. ASC truncation systematically discards
recent memories. Truncation bias cannot be eliminated, so we choose the one
that keeps the recent. The `id ASC` used elsewhere (results, PPR list) breaks
ties without truncating, so it carries no bias.

## D9 — the deadline is on the envelope transaction only, not on PPR

**Decision**: envelope tx over 100 ms → drop the whole PPR channel. No
deadline on the PPR iterations (docs/05 §4–5, 06 §5).

**Alternative**: the proposal — a 50 ms wall-clock deadline on the whole PPR,
dropping PPR when exceeded.

**Reason**: the time is spent in Neo4j round trips; PPR is a few ms. A
deadline on PPR makes the same input produce different output under load
(breaks determinism). The principle of never running PPR on a partial
envelope stands — partial results are silent bias.

## D10 — true-degree normalization with uniform leak

**Decision**: `W_ij = w_role / D_i` (D_i = true weighted conducting degree,
including edges outside the envelope — see D24), `leak_i = 1 − Σ_j W_ij`
redistributed uniformly (docs/06 §4).

**Alternative**: normalize by in-envelope degree, uniform redistribution for
dangling nodes only.

**Reason**: in-envelope normalization makes boundary nodes stronger
conductors than they are, piling mass up at the envelope's edge. True degree
is O(1) via `COUNT{}`, and this normalization is what makes "a larger envelope
converges toward full PPR" hold, which gives 07's validation its meaning. It is
also what turns hubs into drains.

## D11 — maxIter 64, τ 1e-4, error bound 6.7e-4

**Decision**: docs/06 §5.

**Alternative**: the proposal — 20 fixed iterations.

**Reason**: at α = 0.85, 20 iterations give `0.85^20 ≈ 0.039` — the residual
cannot reach 1e-4, and the error bound is set by the final residual, not by
the iteration count. 64 is a cap that guarantees τ is reached
(`2·0.85^61 < 1e-4`) while runs stop at 20–30 in practice. The L1 tolerance of
7e-4 in 07 comes from this bound.

## D12 — κ conservation and merge cap

**Decision**: `κ_eff = κ/|sources|`; the same recall and Episode merge into
one Hit with `kappa_eff ≤ κ` (docs/04 §5).

**Alternative**: full κ per source.

**Reason**: if adopting a well-sourced fact reinforces more in total, the
number of sources dominates mass. Conservation keeps "one adoption = κ worth
of attention".

## D13 — commit is recomputed by the server; auto/receipt modes

**Decision**: the client sends only `adopted[]`. Mode is declared in hello;
explicit commits from auto clients are rejected (docs/04 §6).

**Alternative**: the client computes κ or S′ and sends it; no modes, every
client may commit.

**Reason**: ledger integrity has to be guarded by one server. A client that
cannot observe adoption (a context-injection hook) reporting adoption pollutes
the ledger. Exposure's low κ is the price of that uncertainty.

## D14 — Fact mass = max over source Episodes · σ_fact

**Decision**: docs/04 §3.

**Alternative**: sum, mean, or per-Fact state.

**Reason**: per-Fact state conflicts with D1. A sum inflates with the number
of sources; a mean dilutes recent reinforcement. Max is monotone and
conservative and matches the intuition "as alive as its most recently handled
source". σ_fact models the gist outliving the detail.

## D15 — Entity, Community and Link store no time

**Decision**: derived visibility. Communities use a majority rule (docs/03 §3).

**Alternative**: store first-mention time.

**Reason**: storing it creates two truths on re-extraction. Derivation always
agrees with the originals. The Entity test is one EXISTS subquery and only
runs at the candidate stage.

## D16 — per-stream generations, write-once gen_from/gen_to, per-Episode switch

**Decision**: docs/01 §4.

**Alternative**: the proposal — one generation, atomic switch after full
re-extraction.

**Reason**: community recomputation has no reason to invalidate extraction,
and an embedding swap does not change extraction. Full re-extraction costs LLM
time, so "no new output visible until the switch" becomes days. Per-Episode
supersession lets two generations be visible at once while guaranteeing that
for any one Episode only one is. Rollback is a single selector.

## D17 — DELETE only through `gc`, derived layer only

**Decision**: docs/01 §4, §8.

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

## D19 — GDS solver validation reproduces the leak on the graph side

**Decision**: a dense complement row `W_ij + leak_i/|V|` and a virtual source
node σ with weights s_i; the GDS vector on V equals α·p* (docs/07 §2).

**Alternative**: leave dangling handling and uniform source teleport to GDS
and widen the tolerance.

**Reason**: if the definitions differ, a discrepancy cannot be told apart from
a solver bug. Match the definition on the graph side and take the tolerance
from the mathematical bound.

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

## D22 — while Neo4j is down, remember spools and recall returns an empty success

**Decision**: docs/02 §4, §9.

**Alternative**: recall reads the spool for a partial answer; remember returns
failure.

**Reason**: losing a memory (remember failing) is irreversible; not being able
to retrieve one (empty recall) is temporary. Recalling from the spool creates
a second search path, which is a second store.

## D23 — valid(T) only at assembly

**Decision**: candidates and the envelope use `visible(T)` only; `valid(T)` is
applied at result assembly (docs/03 §7, 05 §6).

**Reason**: an invalid Fact still conducts — the Entities it connects are
still relevant. Removing it at the candidate stage cuts off the whole
neighborhood of a corrected topic. Removing it from the results while exposing
it under `supersedes` explains "why that fact is not showing".

## D24 — role weights only; degree is the true weighted degree

**Decision**: links carry no per-link `weight`. PPR transition strength is a
per-role constant `w_role`. `D_i = Σ_role w_role · deg_role(i)` from five O(1)
counts; `W_ij = w_role(ij) / D_i` (docs/01 §5, 06 §4).

**Alternative**: keep a per-link weight in (0, 1] and normalize by the
unweighted count, as the first draft of this document set did.

**Reason**: with count normalization and sub-unit weights, a "weak link"
leaked mass to uniform even on the full graph — an odd semantics — and the
local operator disagreed with GDS's weight-sum normalization, so the
full-graph GDS baseline was only valid at weight 1.0. With the weighted true
degree, full-graph rows sum to exactly 1 for any positive role weights, leak is
purely a boundary effect, `leak ≥ 0` even for weights above 1, and GDS agrees
without restrictions. A per-link weight would need a per-node weighted-degree
cache to stay O(1); nothing in the pipelines produced one anyway. Found by
the PR review of 2026-09.

## D25 — derived idempotency keys include the generation; originals-layer links have none

**Decision**: derived link `idem_key = sha256(from, to, role, content,
gen_from)`, Fact `idem_key = sha256(gen_from, sorted direct sources,
content)`. `NEXT_EPISODE`, `HAS_PAYLOAD`, `HIT_OF` and revision `INVALIDATES`
are originals-layer links with no generation and `idem_key = sha256(from, to,
role)`. Every generation filter is `gen_from IS NULL OR (…)` (docs/01 §4–5).

**Alternative**: the first draft — `sha256(from, to, role, content)` for every
link, with all links implicitly in the extraction stream.

**Reason**: re-creating the same relationship in generation 43 collided with
its retired generation-42 copy, which made per-Episode supersession impossible
to implement as written. And `NEXT_EPISODE` is written by remember, not by a
pipeline, so it had no generation to carry. Found by the PR review of 2026-09.

## D26 — `origin_key` is logical identity; `revision_key` is the unique key

**Decision**: `origin_key = sha256(source, session, actor, record)` is indexed
and shared by every revision of a source; `revision_key = sha256(origin_key,
digest)` is unique and is what remember's idempotency and the spool drain use
(docs/01 §1, 02 §3).

**Alternative**: the first draft — `origin_key` unique, with a revision
creating a second Episode under the same key (a constraint violation); the
current code's workaround of suffixing the record with a hash.

**Reason**: a revision must be a new Episode (CREATE-only) and must be
findable through the logical identity of the source. Two keys, two jobs. Found
by the PR review of 2026-09.

## D27 — one commit path for every Hit

**Decision**: a single internal `commitHits(namespace, kind, adopted)` with
four producers — receipt `commit` RPC, post-response exposure, extraction
re_mention, dreaming promotion — each with its own namespace (docs/04 §5–6).
The recall request handler writes nothing; exposure runs after the response.

**Alternative**: the first draft described the producers separately and
contradicted itself ("dreaming never creates Hits" vs "promotion Hits";
"recall is read-only" vs "auto recall records exposure").

**Reason**: the ledger's invariants (idempotency, cache validation, server-side
S′) must be enforced in one place, and the invariants in docs/00 must be
literally true. Found by the PR review of 2026-09.

## D28 — extraction is read tx → LLM → write tx with re-validation

**Decision**: candidates are read in one transaction, the LLM runs outside
any transaction, and the write transaction first re-checks that the active
generation is unchanged and that every candidate the judge relied on is still
un-retired and (where it matters) still valid; otherwise it aborts and
re-queues, three times, then DLQ (docs/02 §5).

**Alternative**: the first draft's "all in one transaction (LLM outside)",
which was not implementable as stated and had no stale-read handling.

**Reason**: a judge verdict is a function of its premises. If a candidate Fact
was invalidated or retired while the LLM ran, writing INVALIDATES or a
re_mention against it is wrong; re-running is cheap. Found by the PR review of
2026-09.

## D29 — `m_cache` and hub shortlists are a v0.2 maintenance job, not dreaming

**Decision**: an hourly job with no LLM and no GDS computes `m_cache` and the
hub shortlists; it ships in the same version as PPR (docs/02 §6, 09).

**Alternative**: the first draft placed both in v0.3 dreaming while v0.2 PPR
already depended on them — hubs would not have expanded at all and fanout
ordering would have degenerated to role weight then id.

**Reason**: the envelope's two bias controls must exist from the first
version that has an envelope. Neither needs the expensive parts of dreaming.
Found by the PR review of 2026-09.

## D30 — hard bounds on every recall stage, including query work

**Decision**: channel sizes are fixed (vector 64 nodes + 16 relationships,
BM25 64, session 32 Episodes + 64 Facts, identity 18 → ≤ 274 candidates);
assembly handles ≤ 530 elements; envelope links are fetched per node with
`LIMIT L = 10` so no query sorts an unbounded set (docs/05 §2, §6; 06 §1–2).

**Alternative**: the first draft bounded the PPR arrays but left the session
channel's Fact fan-out and the induced-link query unbounded.

**Reason**: "bounded" has to mean bounded work, not only bounded output. Found
by the PR review of 2026-09.

## D31 — security and durability rules are normative

**Decision**: `~/.anamnesis` 0700, UDS 0600 with peer-UID check, request and
payload caps, bolt on 127.0.0.1 only, per-install random Neo4j password,
daemon singleton lock, spool fsync-before-ack, `.done` after commit,
`gc --objects` protected against undrained spool references, documented
backup and restore (docs/01 §9, 02 §10).

**Alternative**: leave these to implementation.

**Reason**: a normative document that leaves the trust boundary and the
crash-consistency rules implicit will be implemented inconsistently; the
current code's default password is the example. Found by the PR review of
2026-09.
