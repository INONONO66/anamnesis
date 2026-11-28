# Decision log archive

This file is non-normative. It keeps the original text of decisions that were
superseded, released or never built, under the same headings as
[10-decision-log](../10-decision-log.md), so old links resolve. Each entry's
current status is the one line in the live log.

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

## D15 — Entity and Community materialize visibility thresholds, not event time

**Decision**: Entity caches the minimum visible mention time; Community stores
the majority threshold captured from its pinned member snapshot. Links remain
visible through their endpoints (docs/03 §3).

**Alternative**: evaluate MENTIONS and HAS_MEMBER subqueries on every recall.

**Reason**: nested visibility scans defeat the envelope's work bound.
Thresholds are rebuildable derived/cache values, and a Community generation
pins the extraction snapshot from which its threshold was computed.

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

## D22 — Neo4j-down remember uses a bounded spool

**Superseded by D61.**

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

## D50 — adjudication is shadow-first and operator repair is append-only

**Status**: superseded by D60.

**Decision**: for the frozen conformance prompt
`scripts/research/adjudication-prompt.md` (SHA-256
`94a74ca2825e17d09188ae0af97c915c7a79a64a45ac998e2f584d8878c3275b`),
`claude-opus-5` on Messages with thinking disabled is the **development**
adjudication default and `gpt-5.5` on Responses with reasoning effort `none`
remains the comparator. This is a role-specific choice: it approves no
extractor, transfers to no other role, and names no global winner. The
default runs in **shadow/no-write mode**, emitting proposed verdicts and
audit records that create no Fact and no semantic link. Every call appends an
immutable `AdjudicationAttempt` carrying the `source_head_revision_key` and
`policy_revision` captured before the bounded candidate read and the model
call, so a transport, parse or validation failure creates no proposal and
never leaves a denominator. A valid strict output creates an immutable
`AdjudicationProposal` that copies those two premises byte-for-byte and adds
`proposed_claim_digest = sha256(RFC-8785(validated complete L1 claim object))`
over content and language, sub-kind, modality, confidence, evidence
quote/kind/span, resolved time, Entity mentions, predicate and scope fields
and the local-correction fields. Its materialized state is
`SHADOW | ACCEPTED | REJECTED` rebuilt from append-only reviews; only an
authenticated `adjudication.review` moves it, and an ACCEPTED proposal may be
consumed once, by its named target generation, after W1 compares those
**stored** values directly, never inferring proposal-time source or policy
state from the current graph.
Acceptance is an operator decision, not human gold, and enables no unattended
writes globally. Unattended invalidation requires a new decision backed by
independent, production-shaped labels and declared false-invalidation,
missed-update, candidate-completeness, transport and parse bounds; neither the
119 historical pseudo-label cases nor the 36 synthetic conformance cases
qualifies.

Operator correction of an adjudicator mistake is authenticated, append-only
audit authority through `adjudication.correct`. It never rewrites or deletes a
historical Fact, edge or `InvalidationEvidence`. A repair that creates a
replacement Fact first appends a CREATE-only
`anamnesis.operator-adjudication/1` Episode, excluded from ordinary search,
extraction and PPR, whose deterministic renderer never impersonates user
prose. Restoring a wrongly invalidated A appends A-prime in A's ACTIVE
generation under the docs/03 §5 replacement protocol, retains every bad edge,
inspects at most 65 incoming evidence rows and carries at most 64 retained
evidence IDs forward as content-free markers. A-prime keeps `A.time`, so it
serves every `T >= A.time` after the correction commits; operator acceptance
time is audit-only. Ordinary recall labels the repaired provenance
`operator_corrected` (docs/01 §4, docs/02 §2 and §5, docs/03 §5).

**Alternative**: (a) let the measured conformance leader write `INVALIDATES`
unattended; (b) repair a mistake by deleting the bad edge or rewriting the
Fact; (c) express the repair as a synthetic user correction Episode.

**Reason**: (a) reads 36/36 on a synthetic specification screen as a
production error bound. The screen has two disclosed arguable gold choices and
no production-shaped negatives; the historical predecessor evidence that
motivates conservatism (a predecessor report's 54% invalidated-state
observation, a repair job that restored 97,163 of 101,914 edges by the same
model's own judgment) is an evidence chain, not independent accuracy. Those
are secondary reported operations from a private predecessor report, neither
replayed truth labels nor current anamnesis deployment metrics. (b) breaks invariant 1 and makes
validity replay non-deterministic. (c) would fabricate something the user
never said, exactly the failure the correction is supposed to fix; an operator
Episode with a deterministic renderer keeps the audit trail honest about who
acted.

## D51 — separate model and index fingerprints pin one embedding profile; a permanent hole blocks coverage

**Decision**: embedding identity is three SHA-256 IDs over canonical RFC-8785
objects. `embedding_model_id` covers the artifact repository/revision/file
digest/size/quantization, tokenizer, request serialization, pooling,
dimension, normalization, document prefix, exact query template and context;
`vector_index_id` covers that model ID plus layout version, Neo4j version
family/provider, dimension, similarity, index quantization and HNSW settings;
`embedding_profile_id` covers the exact pair and is the value of
`active[embedding]`. A model-field change builds new vector properties and
indexes; an index-only change may reuse vectors but never mutates an active
index in place. `ONLINE` means only that an index can answer queries: it is
not quality approval, and activation additionally requires an authenticated
append-only `EmbeddingQualification`. Production activation stays disabled
until a qualification references machine-validated manifests from the
applicable M4/M5 gate with predeclared quality and resource thresholds.

Per-entry embedding work is a state machine
(`PENDING → RUNNING → SUCCEEDED | NO_VECTOR_REQUIRED | RETRY_WAIT | BLOCKED`,
with `BLOCKED → PENDING` on authenticated retry and
`BLOCKED → RESOLVED_NO_VECTOR` on authenticated skip). Only transient
`unavailable`, `timeout`, `rate_limited` and `server_error` classes retry:
three attempts per cycle with fixed `[1000, 10000]` ms delays, then BLOCKED.
Invalid input, context overflow, zero/nonfinite/wrong-dimension vectors,
profile mismatch, cardinality mismatch, `malformed_response` and deterministic
`client_error` (4xx) block immediately, while a lost worker lease closes its
attempt `worker_lost` under the same retry and third-failure rule. Never truncate, chunk, silently skip, synthesize a zero vector or
advance coverage past a nonterminal entry. Coverage advances only over a
contiguous terminal prefix, so a BLOCKED head freezes that model's cursor
while BM25 and session recall continue and the active profile keeps serving
its prior prefix. Recovery is authenticated and append-only
(`embedding.retry`, `embedding.skip`, `embedding.cancel`), each writing an
`EmbeddingResolution`; a skip permanently excludes that source from the
model's vector channel and the default activation policy permits zero skips.
Input transformation requires a new model ID and a complete build, never an
operator mutation of one job. Cutover still compares against **current**
`Meta.ingest_seq` and current active-generation cursors under the write
barrier. Whenever recall selects the vector channel, diagnostics and the
durable receipt record `embedding_profile_id` once plus an
`embedding_coverages` array holding every applicable partition of that
profile: the `(episode, 0)` row always, and the
`(extraction, active[extraction])` row when an active extraction generation
exists. Each row names its own `stream`, `generation`, `health`,
`covered_ingest_seq`, `required_ingest_seq`, `lag` and `omission_digest`, so
two partitions blocked at once with different cursors stay separately
replayable and no served prefix is anonymous (docs/01 §4, docs/02 §5,
docs/05 §8–§9).

The experimental baseline is the resolved Qwen3-Embedding-0.6B Q8_0 artifact
(`embedding_profile_id`
`16d404a70ca92beccbe06fae1c1bc400d924223a09c498c7f01278b0f795405b`) on the
pinned llama.cpp configuration and Neo4j 5.26 cosine index settings in
docs/01 §4.

**Alternative**: (a) one opaque `model_id` string as today; (b) chunk or
truncate an oversized input so the cursor can advance; (c) let ONLINE indexes
plus operator attestation qualify a production cutover.

**Reason**: (a) cannot express that Q8_0 and FP16 of the same revision are
different retrieval systems, or that an HNSW parameter change invalidates an
index but not its vectors; parity between quantizations is unmeasured, so the
IDs must differ. (b) silently changes what a vector means and hides the
omission from every later comparison. One observed 5,002-token request was
rejected with HTTP 400 by the pinned 4,096-token server, and exact boundary,
batch and throughput behavior remains unmeasured, so failing closed with an
auditable BLOCKED head is the only honest option. (c) confuses an index that
answers queries with an index that answers them well; the qualification record
is what makes the distinction reviewable.
