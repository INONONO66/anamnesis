# Observability

Observability explains why the engine returned a context, whether the graph is healthy, and whether performance budgets were exceeded. The core library does not embed a log server; it returns structured reports.

## Observation Surfaces

| Surface | Purpose |
|---|---|
| search trace | Candidate collection, field construction, activation flow, readout |
| tick report | Dissipation volume and projection deltas |
| commit trace | Work integrated into reservoirs |
| snapshot list | Experiment and checkpoint state |

## Search Trace

Trace should include:

- input summary,
- candidate source counts,
- seed distribution,
- RWR iterations and residual,
- excluded contradiction edges,
- readout score components,
- budget use and truncation,
- selected tensions,
- scope-compatibility weights and the neutral trust compatibility input.

Trace may expose more internal scores than the final context body. It is for debugging and evaluation.

For deterministic runs, trace records candidate, search, final-result,
iteration, replacement, and token limits. Elapsed time is reported separately;
it does not explain successful result membership. An external timeout aborts
with an explicit outcome rather than silently becoming a ranking rule.

## Proposed: Formation and Evidence Trace (ADR-0015)

The evidence-complete extension in
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
would add two linked traces. They are not current engine report types.

Formation trace includes:

- immutable source and revision identity plus content hash;
- producer and formation profile version;
- cited span validation or a media asset/range validation receipt;
- assigned admission class and review state;
- scope/time validation; and
- idempotent replay, omission, promotion, staleness, and revocation outcome.

Evidence trace includes:

- typed query intent, facets, slots, scope, time, and budgets;
- candidate counts per raw, graph, fact/entity, temporal, and observation lane;
- canonical-source fusion and duplicate representations;
- admitted and rejected relation hops with reasons;
- slot coverage before and after chain expansion;
- rerank, replacement, hydration, and truncation decisions;
- every delivered claim-to-source mapping; and
- plan, embedding, collection, expansion, rerank, selection, hydration,
  packaging, rendering, and context-ready latency.

Trace records metadata and identifiers, not secret raw prompts or unrestricted
source content. Consumer reflection and answer-generation time are reported as
separate end-to-end stages.

## Graph Health

| Metric | Meaning |
|---|---|
| node_count | Total sites |
| edge_count | Total edges |
| orphan_ratio | Fraction of disconnected sites |
| contradiction_ratio | Fraction of tension edges |
| salience_entropy | Diversity of salience projections |
| conductance_entropy | Diversity of conductance projections |
| average_degree | Mean graph degree |
| scope_distribution | Site count by scope |
| stale_ratio | Sites not accessed recently |

Definitions, so the metrics are computed identically across environments:

- **disconnected site (orphan).** A site with structural degree `0` — no incoming or outgoing retained edge (`degree = in-edges + out-edges`). `orphan_ratio = disconnected_sites / node_count`, defined as `0` for an empty graph (`node_count = 0`). A too-strict conductance threshold suppresses edge creation (it gates `coupling_seed >= threshold` at link time), which is what the Operational Warnings table attributes to a high orphan ratio.
- **entropy metrics.** Shannon entropy `H = -sum_k p_k * log2(p_k)` over the projection histogram — projections binned into fixed buckets and normalized to sum to 1 (`salience_entropy` over `salience s_i`, `conductance_entropy` over the bounded `edge_weight = project_weight(C_ij)`), reported in bits. Low entropy means the projections have collapsed onto a few buckets.
- **degree.** The graph is directed (activation flow normalizes outgoing edges; see [activation-flow.md](../05-context-retrieval/activation-flow.md)), so `average_degree` is the mean total degree (in-edges + out-edges) per site.
- **stale site.** A site whose `now - accessed_at` exceeds the configured stale window. `accessed_at` advances only on committed access; query-only retrieval leaves it unchanged (see [dissipation.md](../04-cognitive-dynamics/dissipation.md)). The conductance threshold and stale window are operational [EngineConfig](../01-system-architecture/overview.md#engineconfig) knobs, not calibrated priors.

### Proposed catalog health metrics

ADR-0015 would add the following metrics after the catalog exists:

| Metric | Proposed meaning |
|---|---|
| ungrounded_derived_count | Eligible derived records with no currently valid evidence reference; must be zero |
| stale_derived_ratio | Derived records invalidated by source or profile change |
| routing_only_ratio | Share of catalog facts restricted to source routing |
| chain_completion_rate | Share of requested multi-evidence slots covered by a valid delivered chain |
| provenance_coverage | Share of delivered claims traceable to raw evidence; target is 1.0 |

## Invariant Checks

The engine should expose checks for:

- public projections within closed bounds (salience `s_i` and edge weight `w_ij` in `[0, 1]`, including clamped boundary values `0` and `1`),
- adjacency consistency,
- missing origins,
- invalid validity intervals,
- dangling edges,
- non-finite or out-of-range scope compatibility weights,
- non-finite hot fields,
- snapshot/restore consistency.

Current scope checks validate metadata and scoring behavior; they do not certify
authorization or tenancy isolation.

### Proposed catalog invariant checks

An ADR-0015 implementation would additionally check:

- missing or mismatched evidence references,
- derived-scope widening relative to caller-supplied eligibility,
- incompatible temporal relation hops,
- stale derived records marked eligible,
- catalog/graph projection identity mismatch, and
- traversal beyond declared depth or visited-record budgets, including cycles.

## Operational Warnings

| Warning | Likely Cause | Action |
|---|---|---|
| high orphan ratio | Conductance threshold too strict | Recalibrate threshold or candidate generation |
| high contradiction ratio | Over-linking entities or stale facts | Review tension handling |
| low entropy | Salience projections collapsed | Inspect dissipation and reinforcement |
| dense graph | Excess edge proposal | Apply edge budget / leakage |
| stale core | Important identity not accessed | Inspect packaging policy |

The following warnings belong to the proposed catalog extension:

| Proposed warning | Likely cause | Action |
|---|---|---|
| ungrounded derived record | Source changed, invalid span, or incomplete migration | Mark stale/revoked and rebuild from authoritative sources |
| low chain completion | Missing relation, entity fragmentation, or selection loss | Inspect slot and hop rejection trace before widening candidates |
| provenance coverage below 1.0 | Renderer or hydration contract violation | Fail the release gate; do not serve uncited derived evidence |

## Related Documents

- Performance measurement is defined in [benchmarks.md](benchmarks.md).
- Readout trace is defined in [readout-scoring.md](../04-cognitive-dynamics/readout-scoring.md).
- Storage scan cost is defined in [storage.md](../03-persistence/storage.md).
