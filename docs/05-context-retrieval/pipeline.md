# Retrieval Pipeline

The retrieval pipeline converts a query into source-aware context. The current
public product result is `RerankedRecall`, whose commit-safe `Recall` contains a
`ContextPackage`. Catalog records, catalog evidence-chain traversal, and
`EvidenceBundle` are proposed additions rather than current result types.

## Current Production Path

`Memory::search_reranked` owns the shared production policy:

| Stage | Current behavior |
|---|---|
| plan | Infer one deterministic `RecallPlan`, or accept a precomputed plan through the plan-aware APIs; preserve its query, answer shape, derivation policy, temporal constraint, and bounded coverage policy through search, reranking, selection, and rendering |
| search | Collect a bounded cognitive surface from lexical, vector, entity-tag, temporal, and graph signals |
| activate | Run additive RWR over supportive conductance; exclude `Contradicts` edges from propagation |
| compile | Compile source-grounded `EvidenceDocument` values: collection and inference plans group canonical sources and may retain a bounded Semantic scoring window; count, frequency, and eligible relationship plans use canonical raw-evidence documents |
| rerank | Ask the supplied `RerankingProvider` to score the bounded document surface |
| select | Apply the configured source-aware policy under candidate, search, and final-result limits |
| package | Rebuild a commit-safe `Recall` and `ContextPackage` from selected policy-eligible graph evidence |
| render | Compile a provider-neutral `RecallReaderContract` and use the same plan with `render_context_for_plan_with` for reader-ready context; query-only compatibility methods infer an equivalent plan locally |
| commit | Mutate access traces and conductance only when the caller explicitly marks the recall used |

The current plan distinguishes direct, enumeration, temporal, and relational
retrieval intent; fact, temporal, frequency, count, collection, relationship,
and inference answer shapes; extractive or grounded-inference derivation; and
focused, multiple, or exhaustive coverage. Answer shape describes the requested
output, while `RecallDerivation` controls how a reader may obtain it from
grounded evidence. A temporal constraint, derivation, and coverage policy remain
independent properties, so none erases the requested answer shape. `Exhaustive` selects a
completeness-oriented policy inside the bounded surface; it is not an
unbounded corpus scan or a guarantee that every matching stored item is
returned. Direct retrieval preserves a stable relevance prefix, while
completeness-sensitive plans may use a bounded tail for new canonical sources.
The configured `candidate_limit`, `search_limit`, final `limit`, selection
policy, token budget, and fixed replacement caps determine membership. Measured
elapsed time is telemetry or an external request timeout, not a semantic
candidate-membership rule.

Consumers that use an external scorer prepare one bound receipt with
`prepare_rerank_for_plan_at`, score the exact document-index order exposed by
`PreparedRerank::rerank_texts`, and consume the receipt with
`complete_prepared_rerank`. Preparation owns the canonical scoped source search
and checks and binds every source before its text is exposed. Completion accepts
the receipt only on its originating `Memory`, revalidates those allocations, and
exactly recompiles the scoring documents before packaging. An empty document
surface produces an empty commit-safe recall rather than exposing the source
search package.
The separate `rerank_documents_for_plan` and
`repackage_reranked_deep_for_plan` surfaces remain available for unbound
diagnostics, but are not a production boundary across an external provider
call.

### Reader contract

`RecallPlan::reader_contract` compiles a model-free `RecallReaderContract` for
the exact plan used by retrieval. The contract supplies provider-neutral
instructions for direct answer, reflection, verification, and repair stages;
states the requested answer role, cardinality, modality, and reasoning operator;
and exposes whether a separate evidence-analysis pass is recommended. Query-aware
rendering appends its concise guidance only when evidence is present. Direct
crate consumers can use the corresponding typed `RecallReadout` instead of
parsing rendered Markdown.

`validate_grounded_draft` verifies typed draft shape and that every citation
belongs to the delivered source set. For collection and count drafts it also
checks the item ledger and its deterministic consistency with the candidate.
These checks do not prove that a cited source semantically entails the proposed
answer or that retrieval was complete. A reader-owning consumer remains
responsible for semantic verification against the complete delivered context.
The shared recovery policy permits at most one structural repair and one
reverification, including an independent pass for an unresolved or answerable
abstention; it performs no provider call itself.

Complex dense planning remains bounded and model-free. Relationship and
inference shapes batch at most four total query-derived surfaces; collection
shapes batch at most five. The provider receives one `embed_queries` call,
stored embeddings are scanned once, and all auxiliary graph lanes are fused
into one lower-prior union. Entity-only surfaces may seed graph recall but do
not independently authorize atomic-fact routing.

Base engine search treats `ScopePath` as a ranking signal. Equal scopes, or a
pair involving universal scope, receive full compatibility; different concrete
scopes are attenuated. Prepared production reranking additionally rejects a
candidate when a concrete query scope and any bound delivery or scoring source
have different concrete scopes. Neither layer authenticates a caller or
establishes the caller's authorized scope set.

`EdgeType::Contradicts` is never a propagation hop. The frustration channel can
surface a tension only after both endpoints become active through other cues or
supportive paths.

Atomic facts form one isolated routing lane for eligible structured plans.
`AtomicFactInput` binds a compact consumer-produced routing record to raw
Episodic sources, but does not establish an explicit review decision.
`ReviewedDerivationInput` uses the same routing-only substrate for a typed
proposition with polarity, modality, review identity/profile/time, and an
idempotency key bound to the complete normalized record. It additionally
requires cited sources to have been live at review time. Neither the ordinary
fact content nor the reviewed proposition or search projection is packaged as
independent evidence; only eligible cited raw sources can enter the
reader-facing package.

Every route revalidates query time, fact observation time, raw-source creation
time, validity, retraction, source type/session identity, and
exact-or-universal scope compatibility. Ordinary recall admits current cited
raw sources. Trend recall may also admit historically valid, unretracted
evidence and can use the shape-driven dense, atomic-fact, and exact-subject
lanes. Resolvable calendar ranges admit only facts whose validity interval or
observation time overlaps the range; calendar, event-boundary, and unresolved
legacy constraints do not use dense or exact-subject expansion. Reviewed
relation-chain traversal remains disabled for every temporally constrained
plan. Frequency retains its separately bounded fact lane.

Reviewed atomic routing relations add a typed, bounded adjacency lane for
relationship and inference plans. The traversal starts from at most eight
direct fact matches, follows `reason`, `causal`, and `supports` in either
adjacency direction to depth two, visits at most 32 relation rows, and admits
at most eight new endpoint facts and eight raw sources inside the existing
candidate tail. Stored direction remains part of the relation record; reverse
adjacency is retrieval connectivity, not a reversed semantic claim.
`contradicts` is retained but never propagates positive relevance. Every hop
rechecks review time, validity, retraction, concrete-scope compatibility, and
live Episodic provenance.

## Proposed: Evidence-Complete Extension (ADR-0015)

The remainder of this document specifies the pipeline proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md).
The general catalog lanes, relation-evidence model, slot ledger, immutable
source-revision hydration, `EvidenceBundle`, and explicit caller-supplied
scope-eligibility set are not implemented by the current API. The narrow typed
atomic routing relation lane above is not the proposed canonical fact-relation
catalog.

### Proposed pipeline contract

| Stage | Input | Output | Contract |
|---|---|---|---|
| parse | `SearchInput` or `Query` | validated input | Reject malformed scope, time, budget, or non-finite values |
| plan | query text, scope, time, budget | deterministic recall plan | Identify intent, entity/predicate facets, requested slots, answer shape, derivation policy, bounded coverage, and limits without a model call |
| collect | plan and indexes | attributed candidates | Collect bounded raw, graph, entity/fact, temporal, and observation lanes |
| fuse | attributed candidates | canonical-source surface | Prevent duplicate representations from multiplying source influence |
| activate | seed distribution | graph response and tensions | Run additive RWR over scope-eligible, time-valid conductance |
| expand | plan, facts, relation adjacency | bounded evidence chains | Traverse only typed, compatible relations under depth and visited-record budgets |
| rerank | evidence documents | relevance order | Score complete source-aware documents, not isolated overlapping windows |
| select | ranked documents and chains | covered evidence slots | Maximize marginal coverage, provenance, temporal consistency, and source novelty under token budget |
| hydrate | selected catalog records | authoritative source spans | Resolve facts and chains back to immutable sources after selection |
| package | plan, ledger, sources, tensions | `EvidenceBundle` and `ContextPackage` | Preserve citations, uncovered slots, and trace; do not silently repair after this boundary |
| commit | consumed bundle/trace | persistent interaction | Mutate only after the caller confirms use |

The current path already performs deterministic planning, broad source
collection, bounded reviewed atomic-relation routing, reranking, source-aware
selection, and detailed context rendering.
An ADR-0015 implementation must introduce the new stages additively and keep
direct/temporal behavior behind regression gates.

```mermaid
flowchart LR
    input["Query"] --> plan["Deterministic plan"]
    plan --> lanes["Bounded retrieval lanes"]
    lanes --> fuse["Canonical-source fusion"]
    fuse --> graph["Activation + typed chains"]
    graph --> rank["Rerank and slot-aware selection"]
    rank --> hydrate["Hydrate raw evidence"]
    hydrate --> bundle["EvidenceBundle"]
    bundle --> render["ContextPackage"]
    bundle -. "confirmed use" .-> commit["Commit"]
```

### Deterministic query planning

The plan makes retrieval policy explicit:

- recall intent: direct, temporal, collection, relationship, or inference;
- normalized entity anchors and predicate facets;
- requested evidence slots, answer shape, derivation policy, and bounded
  coverage policy;
- `as_of` and any occurred/valid-time constraints;
- caller-supplied scope eligibility; and
- candidate, traversal-depth, visited-record, and token budgets.

Planning is based on typed syntax and normalized predicates. A consumer may
supply an explicit typed intent, but the engine does not invoke a model to infer
one. Versioned locale lexicons and grammar resources are valid plan inputs when
they map consistently to the same typed intents and normalized predicates.

### Candidate lanes

Each lane returns a score, canonical source identity, and traceable reason:

- full-text and vector similarity over raw/source views;
- cognitive graph activation from bounded seeds;
- canonical entity and alias lookup;
- subject/predicate/object fact lookup;
- temporal interval and event lookup;
- eligible reviewed observations;
- explicit source or node ids supplied by the caller.

Candidate generation is broad; authority is narrow. A semantic window, routing
fact, entity mention, and reviewed claim may all point to the same raw source.
Fusion groups them by canonical source so representation count cannot spend the
evidence budget repeatedly.

### Evidence-chain expansion

Complex queries would be evaluated against evidence chains, not merely
individual high-scoring nodes. An expansion may follow `causes`, `reason_for`,
`supports`, `supersedes`, `same_event`, or `sequence` only when:

- every hop is eligible under the caller-supplied scope policy;
- every hop is temporally compatible;
- every derived record has valid source references;
- the relation fills a requested slot, resolves a typed connection between
  already relevant slots, or supplies a required supersession or validity
  qualifier; and
- depth, visited-record, and token budgets remain available.

`contradicts` is represented as a paired tension with both provenance paths
when counter-evidence is required; it is not an activation-propagation or
positive chain-continuity hop. Default traversal depth is deliberately
small, visited ids prevent cycles, and a release profile declares every
structural limit. These structural budgets determine deterministic membership.
A wall-clock deadline may abort a request with an explicit outcome, but it must
not silently decide which hop belongs to an otherwise successful result. The
trace records every admitted and rejected hop. If the catalog is empty or a
chain is invalid, raw recall proceeds without error.

### Reranking and selection

Reranking operates on evidence documents assembled from canonical sources.
Selection considers:

- query relevance;
- coverage of previously uncovered slots;
- relation compatibility and chain continuity;
- time and scope consistency;
- complete provenance;
- source/session novelty;
- contradiction coverage; and
- estimated token cost.

A deeper candidate replaces a higher-ranked candidate only when it adds
material evidence coverage or removes canonical-source redundancy. Diversity
alone is not sufficient. Direct questions preserve a stable relevance prefix;
complex questions can allocate more of the tail to missing slots and chain
bridges.

### Hydration and evidence authority

Structured facts are compact routing and reasoning units. The reader-facing
evidence remains the cited source. Hydration therefore occurs after selection:

1. resolve each selected fact or relation to `EvidenceRef` values;
2. validate source identity and current hash;
3. load the smallest source span that preserves speaker, session, time, and
   relation meaning;
4. deduplicate overlapping source windows; and
5. retain fact-to-source and chain-to-source mappings in the trace.

A failed reference removes that derived item from the package and records the
reason. It never substitutes uncited generated text.

### EvidenceBundle

The structured result contains:

| Field | Content |
|---|---|
| plan | typed query intent, facets, slots, time, scope, and budgets |
| ledger | selected facts, relations, observations, admission class, and scores |
| evidence | hydrated raw spans with stable citations |
| chains | ordered typed links and the evidence satisfying each hop |
| tensions | eligible contradictory claims and both provenance paths |
| uncovered slots | requested evidence not found within the budgets |
| trace | candidate sources, fusion, traversal, selection, hydration, and truncation decisions |

`ContextPackage` renders this neutral bundle for existing consumers. A renderer
may choose compact or detailed resolution, but it cannot change selection,
drop a required citation, or invent a missing slot.

### Adaptive budget policy

- Candidate and reranker widths are independent of delivered evidence width.
- Simple, single-source questions use a narrow final context after full
  candidate collection.
- Collection and chain-shaped questions retain enough evidence to cover their
  declared slots.
- Prefer a lower-resolution cited source over an uncited summary.
- Relevant tensions and validity qualifiers are not silently truncated.
- A zero token budget returns an empty body plus a trace explaining the limit.

### Optional consumer reflection

A consumer may reflect only when the bundle reports uncovered slots,
ambiguity, contradiction, or an unresolved chain. Reflection and answer policy
are provider-specific and remain outside the engine. At most a declared bounded
number of follow-up plans may be issued. Reflection output is untrusted until it
passes the same source-grounding and admission path as formation output.
If reflection is unavailable or fails, the deterministic result remains valid
and usable with its uncertainty and uncovered slots intact; it is not relabeled
complete.

### Read and commit

Search, planning, chain expansion, hydration, and rendering are read-only.
Commit is a separate operation over the returned trace. It records only sites
and paths the caller actually used; retrieval by itself does not update access
history, conductance, or catalog admission.

### Failure conditions

- Malformed input, scope, time, or budget returns a typed error.
- No candidates returns an empty bundle/context plus trace.
- Non-finite scores are rejected or represented as error trace entries.
- Invalid source references fail closed for the derived item.
- An over-budget chain stops deterministically and reports truncation.
- Absolute activation thresholds are not used as final selection on large
  graphs.
- Commit without a matching trace fails.
- Provider or reflection failure cannot disable deterministic raw recall.

### Cost and indexing

Hot retrieval must not scan the full graph or fact catalog. Entity aliases,
fact fields, time, scope, source identity, and relation adjacency require
indexes. Embeddings are batched, graph/catalog generation keys invalidate
caches, chain expansion is bounded by depth and visited-record count, and raw
content is loaded only for selected evidence.

Latency is reported separately for plan, embedding, candidate collection,
activation, chain expansion, reranking, selection, hydration, packaging,
rendering, and context-ready completion. It is an operational measurement, not
a source of nondeterministic result membership. Consumer reflection and
generation belong to end-to-end measurements.

## Related documents

- Evidence authority is defined in
  [evidence-model.md](../02-knowledge-model/evidence-model.md).
- Activation flow is defined in [activation-flow.md](activation-flow.md).
- Readout scoring is defined in
  [readout-scoring.md](../04-cognitive-dynamics/readout-scoring.md).
- Scope rules are defined in
  [scoping-promotion.md](../02-knowledge-model/scoping-promotion.md).
- Storage indexes are defined in [storage.md](../03-persistence/storage.md).
