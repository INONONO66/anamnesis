# 0015. Evidence-Grounded Formation and Chain Retrieval

- Status: Proposed
- Date: 2026-08-04
- Related: [ADR-0004](0004-query-as-field-and-commit.md), [ADR-0006](0006-frustration-not-deletion.md), [ADR-0012](0012-daemon-core-mcp-plugin-clients.md), [ADR-0013](0013-reasoning-capture-pipeline.md), [ADR-0014](0014-shrink-to-product.md)

`Proposed` records the decision state, not implementation readiness. Accepting
this ADR means accepting the architecture and its boundaries. Enabling it in
the default runtime path is a separate promotion decision governed by the
implementation criteria below.

## Context

Lossless fragments and spreading activation are necessary but not sufficient for
evidence-complete recall. A direct question can often be answered from one raw
turn. A collection, relationship, temporal comparison, or inference may require
several facts from different sessions, a typed connection between them, and an
explicit account of which source supports each part.

The system therefore needs one contract spanning formation, admission,
retrieval, and packaging. Without that contract, derived facts can be mistaken
for authoritative sources, aliases can fragment one entity, temporal adjacency
can be mistaken for causality, and a high-scoring candidate set can still omit a
required answer slot. The contract must improve evidence completeness without
moving model inference into the engine or widening the public API beyond what a
consumer can use safely.

## Decision

Anamnesis will add an **evidence catalog** beside the cognitive graph and will
retrieve **bounded evidence chains** into one source-grounded
`EvidenceBundle`. The graph remains the substrate for cognitive dynamics. The
catalog is a retrieval representation for entities, atomic facts, typed fact
relations, observations, and their source references; it does not expand
`KnowledgeType` into a second domain ontology.

The current v13 storage schema contains a narrower compatibility slice:
reviewed typed relations between isolated atomic routing facts, with bounded
depth-two traversal that can route only to live raw Episodic sources. Those
records do not carry relation-specific evidence references, are never rendered
as evidence, and are not the canonical catalog relations decided by this ADR.
Each current citation is fail-closed against a storage-owned, durable source
allocation generation and an authority-field fingerprint; a missing legacy
binding, changed source, or reused numeric node id is ineligible. This prevents
source-authority aliasing but is not an immutable source-revision history.
Their defaulted `StorageAdapter` methods preserve the existing generic
`Memory<S>` recall surface; the complete catalog remains an additive extension.

The engine remains synchronous, local-first, deterministic for fixed inputs,
and free of LLM and network calls. Consumers own extraction, reflection, and
multimodal interpretation. Every automatic or model-produced record crosses
the same grounding and admission boundary before it can affect retrieval.

### Formation authority classes

| Class | Meaning | Admission | Retrieval authority |
|---|---|---|---|
| **Source revision** | Immutable revision of a raw turn, document fragment, or normalized media segment | Lossless ingest with stable source and revision identities | Canonical evidence |
| **Grounded routing fact** | Structured claim whose cited span validates against a source revision | May be admitted automatically after deterministic validation | Routing only; must resolve back to source evidence |
| **Reviewed claim** | Fact or relation accepted by an explicit consumer review policy | Review record and source citations required | May participate in knowledge and chain retrieval |
| **Observation** | Versioned synthesis over cited facts or sources | Every asserted proposition is grounded in admitted inputs or accepted by an explicit review policy; derivation identity, freshness state, and citations are required | Readable synthesis accompanied by its evidence and support map |

Promotion and revocation are explicit, fallible, and observable. A derived
record never overwrites or deletes its source revision. A live graph node may
remain a mutable compatibility view, but any cited bytes belong to an immutable
`SourceRevision`; updating the live node creates a new revision before changing
that view. An authorized hard deletion must atomically make every dependent
record ineligible. If retention policy also deletes historical revisions, the
system keeps only a deletion tombstone and no longer claims that those records
are traceable. A grounded routing fact is never rendered as independent truth;
it can only improve the route to authoritative source material.

Every derived record carries formation provenance: producer identity, policy or
profile version, formation time, source references, grounding result, admission
class, and review state. Admission is a deterministic transaction over already
formed typed candidates; extraction itself remains consumer-owned. Reprocessing
creates an immutable new version and moves an explicit active-version pointer;
it does not mutate the meaning of an existing version.

### Canonical evidence model

The logical catalog contains:

- **Entity records** with a stable, scope-neutral identity, canonical label,
  optional consumer-defined kind, and provenance-bearing aliases and mentions.
  Visibility belongs to each alias, mention, fact, and source reference rather
  than to the entity id itself.
- **Fact records** with subject, canonical predicate, object entity or typed
  value, polarity, modality, confidence, time, scope, admission class, and at
  least one evidence reference. A versioned searchable projection and optional
  embedding with its embedding-space identity are retrieval fields, never fact
  identity.
- **Fact relations** connecting facts with a directed type such as `causes`,
  `reason_for`, `supports`, `contradicts`, `supersedes`, `same_event`, or
  `sequence`. Every relation has its own evidence references, admission class,
  formation provenance, visibility, and temporal constraints; endpoint evidence
  alone does not establish the relation. Custom relation types remain additive.
- **Evidence references** pointing to an immutable source revision plus an exact
  text span and content hash, or to a registered media asset plus a region or
  time range, asset hash, normalized source revision, and consumer-produced
  validation receipt.
- **Observations** that cite the facts and sources from which they were derived,
  map every asserted proposition to admitted support, retain an immutable
  derivation version, and expose current, stale, or revoked state.

Temporal sequence alone does not create a causal relation. Alias similarity
alone does not merge entities. Both operations require explicit evidence and a
declared policy.

### Time and scope

Catalog records distinguish record time, formation time, observed time,
occurred time, validity interval, and access time. Missing observation or
occurrence time remains unknown and is not copied from a neighboring axis.
Missing validity bounds retain the existing half-open-interval meaning: absent
`valid_from` or `valid_until` is unbounded on that side. Supersession closes the
appropriate validity interval while preserving the historical source revision.

The current `ScopePath` score is a compatibility ranking signal, not an
authorization check. ADR-0015 introduces an additive `AuthorizedScopeSet`
query capability whose membership is a hard eligibility gate before every
catalog lane, graph seed, chain hop, hydration, and packaging step. Consumers
decide which scopes enter that set; the engine does not grant authorization.
Legacy callers that supply only `ScopePath` retain their existing ranking
semantics and cannot claim authorization enforcement.

A derived record cannot be visible more broadly than the intersection of its
evidence references. Each retrieval hop must be a member of the authorized set,
temporally compatible with the query, and compatible with the other hops in the
chain. Scope-neutral entity identity never makes a private alias or mention
visible.

### Retrieval and packaging

The canonical recall path is:

1. Compile the query into a deterministic plan containing entity anchors,
   predicate facets, requested evidence slots, temporal constraints, scope, and
   answer shape.
2. Collect bounded candidates from raw lexical/vector recall, cognitive graph
   activation, entity/fact indexes, temporal indexes, and eligible
   observations.
3. Fuse candidates by canonical evidence identity—source revision plus
   overlapping span or media region—so duplicate representations cannot consume
   the evidence budget repeatedly. Distinct revisions remain distinct evidence
   when a query asks about change over time.
4. Expand compatible typed relations into bounded evidence chains. Depth,
   visited-record, candidate, and token budgets are deterministic mandatory
   limits. Wall-clock latency is observed but does not choose which records are
   returned for otherwise identical inputs.
5. Select for marginal slot coverage, relation compatibility, temporal
   consistency, provenance completeness, and source novelty as well as query
   relevance. A chain is admitted only when it fills a requested slot, resolves
   a typed relation, or supplies a necessary contradiction, supersession, or
   validity qualifier.
6. Hydrate only the selected facts and chains back to authoritative raw source
   spans.
7. Package one `EvidenceBundle` containing the query plan, structured ledger,
   cited raw evidence, active tensions, uncovered slots, and a retrieval trace.

The existing `ContextPackage` remains the compatibility rendering of this
bundle. Renderers and consumers must not mutate evidence membership, citations,
or selection after the bundle boundary. A final-answer consumer may summarize
or reason over the immutable bundle, but it cannot present that transformation
as new retrieved evidence. Simple direct questions may use a narrow package;
complex and completeness-sensitive questions may use a wider package within the
declared token budget.

Runtime retrieval policy depends only on declared caller inputs and versioned
resources. Locale lexicons and grammar rules are permitted when they map to the
same typed intents and normalized predicates for every caller. When structured
evidence is absent or invalid, raw retrieval continues normally.

`Contradicts` is never a propagating relevance bridge. When one side is already
eligible, constraint expansion may add the visible counterclaim and both
provenance paths to the tension channel. It cannot seed unrelated traversal,
satisfy a positive relation hop, or contribute positive chain continuity.

### Consumer-owned reflection and multimodal formation

A consumer may perform one bounded reflection or follow-up recall when the
bundle reports uncovered slots, ambiguity, contradiction, or an unresolved
chain. Reflection is optional, provider-specific, and outside the core. Its
output cannot mutate storage directly; persistence requires the same grounding
and admission transaction as any other derived record. If no model is available
or reflection fails, the deterministic bundle remains valid and usable with its
uncovered slots and uncertainty preserved; it is not relabeled complete.

Media decoding and interpretation also remain consumer-side. A multimodal
adapter registers immutable asset metadata, stores a normalized source revision,
and supplies a validation receipt binding adapter/profile identity, asset hash,
region or time range, normalized-content hash, and formation time. The core
validates the receipt shape, registered identities, hashes, and range bounds; it
does not reopen, decode, or semantically interpret media. A media-derived claim
that lacks independently checkable textual grounding remains routing-only unless
an explicit review policy accepts it.

### Persistence and performance

The catalog requires indexed lookup by entity alias, subject, predicate,
object, source revision, scope, time, admission class, searchable projection,
embedding space, and fact-relation adjacency. Hot retrieval must not scan the
full catalog. Lexical/vector collection operates on bounded shortlists,
embeddings are batched, raw source hydration happens after selection, and caches
are keyed by graph/catalog generation and invalidated by relevant mutations.

Tracing separates query planning, embedding, candidate collection, chain
expansion, reranking, selection, hydration, packaging, rendering, and
context-ready latency. Consumer reflection and answer generation are reported
outside the engine boundary. A caller cancellation or emergency wall-clock
deadline returns an explicit incomplete/cancelled outcome and is not part of
the deterministic normal-result contract.

### Public surface and compatibility

`Memory` remains the default, compact consumer surface. Direct crate users,
MCP, hooks, and plugins submit typed candidates to the same deterministic
grounding/admission transaction and use the same recall path. Extractor output
parity is a consumer-profile concern; the engine does not reproduce extraction
from a source/profile pair.
Canonical catalog and bundle types are additive; existing `AtomicFact` and
`ContextPackage` signatures do not change. The existing `StorageAdapter` trait
also remains source-compatible. Catalog-capable paths use a new additive
extension trait (for example `EvidenceCatalogStorage: StorageAdapter`), and
catalog-specific APIs are defined only on `Memory<S>` implementations whose
storage satisfies that bound. Existing adapters retain every existing raw
ingest/recall API and deterministic empty-catalog behavior; they do not gain new
required methods or a compile break. Storage migrations preserve raw revisions,
ids, provenance, scope, validity, and rollback safety.

A source revision write or live-source mutation and all dependent catalog
invalidation, graph projection changes, index generations, and mutation events
commit atomically. Events retain the existing dependency-before-dependent order,
and rollback emits none. Snapshot, clone, and restore include source revisions,
catalog records, admission state, active-version pointers, index generations,
and the cognitive graph as one consistent state.

### Evaluation integrity

Evaluation adapters convert inputs, invoke the public runtime path, and measure
outputs; they do not define admission, scoring, routing, selection, or rendering
policy. A valid evaluation records source revisions, input-set identity,
formation profile, model digests, configuration, selection, and rendered-context
hashes. Formation and retrieval receive only inputs declared by their runtime
contracts. Evaluation-only labels, reference annotations, and scorer artifacts
remain confined to the evaluator.

Candidate recall, selected-chain completeness, delivered-source recall, answer
quality, semantic judgment, and latency are separate metrics. Development
samples are labeled as such. Implementation promotion evidence uses a
predeclared representative or held-out input set, per-class results,
conversation-cluster regression checks, and the same public path used in normal
operation.

## Invariants

- An immutable source revision is canonical evidence. Derived data never
  overwrites it; authorized deletion makes dependents ineligible before the
  revision disappears.
- Every derived record resolves to at least one independently verifiable
  evidence reference. Each relation has relation-specific support, and every
  proposition in an observation maps to cited support whose use is grounded or
  accepted by an explicit review decision; review never replaces provenance.
- Grounded routing facts cannot be presented without their authoritative source.
- Entity identity is scope-neutral. Alias, mention, fact, relation, observation,
  and evidence-reference visibility is checked independently.
- Legacy `ScopePath` affects ranking only. Only the additive
  `AuthorizedScopeSet` capability is a hard visibility gate.
- Invalid or mutually incompatible temporal hops are excluded; absent validity
  bounds are unbounded, while absent observation/occurrence time is unknown.
- Contradictory claims and both provenance paths are preserved, and
  `Contradicts` never propagates positive relevance.
- Candidate representations and delivered evidence are distinct and traceable.
- Normal retrieval results depend on deterministic structural budgets, never on
  elapsed wall time.
- Read-only recall and consumer reflection do not mutate graph or catalog state.
- Source/catalog/projection mutations, generation changes, and events are
  atomic; rollback emits no mutation event.
- Snapshot, clone, and restore cover the graph, catalog, revisions, admission
  state, active-version pointers, and generation keys together.
- The core performs no LLM call, network call, media inference, or session
  orchestration.
- Normal operation and evaluation share admission, search, selection,
  packaging, and rendering policy. Extraction remains consumer-owned.
- Public API evolution and durable migrations are additive and fallible.

## Decision acceptance

This ADR may move from `Proposed` to `Accepted` when maintainers approve the
authority model, engine/consumer boundary, compatibility strategy, and
deterministic retrieval contract above. Acceptance authorizes implementation;
it does not assert that the implementation or release gates already pass.

## Implementation promotion criteria

- The same source revisions, typed candidates, profile identity, and review
  records produce equivalent canonical records and evidence references through
  direct `Memory`, daemon/MCP, plugin, and evaluation admission entry points.
- An invalid source span/hash, media validation receipt/range, authorized scope,
  or time interval fails closed for the derived record while leaving eligible
  raw recall available.
- Source update, retraction, hard deletion, or visibility change atomically
  updates or revokes every dependent record, projection, index generation, and
  event; failure rolls back the whole mutation.
- Every packaged fact and relation, and every asserted observation proposition,
  is traceable to independently admitted raw evidence. Explicit review may
  accept the asserted linkage but cannot replace that provenance.
- Property tests show no `AuthorizedScopeSet` widening, disjoint-scope chain,
  invalid-time hop, positive propagation through `Contradicts`, mutation during
  read-only recall, unbounded traversal, or wall-clock-dependent normal result.
- Legacy `ScopePath` callers retain the documented ranking behavior and do not
  gain an authorization claim.
- Adding chain retrieval does not regress declared direct and temporal gates; a
  chain is selected only when it adds a requested slot, resolves a typed
  relation, or supplies a necessary contradiction, supersession, or validity
  qualifier.
- Empty catalogs, unavailable extractors, and failed consumer reflection retain
  deterministic raw recall and preserve explicit uncovered slots.
- Indexed lexical/vector/catalog retrieval matches an exhaustive reference
  implementation, including deterministic ordering and trace decisions, on
  bounded fixtures.
- Snapshot/clone/restore and migration/rollback tests preserve revisions,
  catalog state, graph projections, admissions, active versions, generation
  keys, and event ordering.
- A minimal third-party `StorageAdapter` continues to compile unchanged;
  catalog operations are available only through the additive extension trait.
- Quality reports expose candidate, selected, and delivered evidence
  completeness by declared query class, together with cancellation,
  incompleteness, and each latency boundary.
- Formation and retrieval accept only inputs declared by their public runtime
  contracts.
- Workspace tests, migration/rollback tests, clippy with warnings denied, and
  rustdoc with warnings denied pass before implementation promotion.

## Consequences

- Complex recall becomes an evidence-completeness problem rather than a request
  for an ever-wider flat top-k.
- Grounded extraction can improve routing without being granted unearned truth
  authority.
- The logical catalog and its indexes add schema, migration, and invalidation
  cost.
- Chain retrieval adds bounded latency and requires stronger stage-level
  observability.
- Optional reflection and multimodal support can evolve at consumer boundaries
  without compromising an LLM-free core.
- If promoted, this evidence-formation contract supersedes the
  extraction-specific portion of ADR-0013. ADR-0014's reduced public-surface
  boundary remains in force.
