# Calibration Registry

This registry identifies versioned runtime defaults and the evidence required
to change them.

Per [ADR-0010](../adr/0010-calibrated-priors-not-laws.md), fitted values are
valid only for their declared data and objective. A proposed fit must disclose the source revision,
dataset fingerprint, split, objective, model identity, runtime controls, and
retained evidence before it replaces an entry here.

Generated benchmark reports are not committed. Reproduction requires the
matching source revision, dataset fingerprint, binary, model weights,
configuration, and any declared formation artifact.

## Readout compatibility defaults

The seven coefficients form one versioned readout object. Their authoritative
values live in `mechanics::priors`; this table mirrors that public behavior.

| Parameter | Active value | Role |
|---|---:|---|
| `w_a` | `0.25` | activation log-odds |
| `w_phi` | `16.0` | query potential |
| `w_s` | `0.0` | salience log-odds |
| `w_z` | `0.0` | impedance penalty |
| `w_scope` | `1.0` | scope compatibility |
| `w_trust` | `1.0` | neutral trust compatibility |
| `w_stress` | `1.0` | contradiction stress penalty |

Changing one coefficient changes the complete readout object and therefore
requires the replacement evidence listed below. These defaults do not imply
transfer to a dataset or workload that was not part of that evidence.

## Active reranked-recall profile

These production defaults are shared by every supported entry point. Direct
crate consumers, MCP, hooks, and the plugin use `Memory::search_reranked` or the
bound `PreparedRerank` handoff and preserve the resulting plan through
plan-aware rendering. The qualifying live-reranker benchmark route uses
`search_reranked_for_plan_at` and the same plan-aware renderer. It performs a
second deterministic source search only after the product package and measured
latency are frozen, solely to record candidate and feature diagnostics; that
search cannot affect returned evidence.

| Setting | Active value |
|---|---:|
| cognitive source-search limit | `20` |
| reranker candidate-document limit | `50` |
| requested final evidence limit | `20` |
| simple direct-query delivery cap | `12` |
| dense query batch, relationship/inference | at most `4` total surfaces |
| dense query batch, collection | at most `5` total surfaces |
| auxiliary dense-union RRF prior | `0.25` |
| bounded recall coverage | `Focused` by default, `Multiple` for ordinary collections, `Exhaustive` for count/frequency and explicitly complete collections |
| collection and inference document compilation | canonical-source grouping with an optional bounded Semantic rerank surface; reader-facing text remains canonical raw evidence |
| reviewed relation expansion | `8` seeds, depth `2`, `32` relations, `8` endpoint facts, `8` raw sources |
| context-ready p95 release boundary | `4 s` |

Direct plans and calendar-, event-, or unresolved legacy-constrained temporal
plans preserve the original-query source search. Trend plans may use the same
bounded, shape-driven query surfaces as their answer shape. Other bounded
collection, relationship, and inference plans may add deterministic clause,
predicate, decomposition, or entity surfaces. Stored embeddings are scanned
once, auxiliary results are deduplicated into one lower-prior union, and
source-aware selection operates after reranking. Atomic facts remain in their
isolated index and route only to cited, scope-valid raw sources that satisfy the
plan's validity policy: current for ordinary recall, or historically valid and
unretracted for Trend. Eligible relationship and inference plans may
additionally traverse the bounded reviewed relation lane; relation and fact
text never becomes reader evidence.
`Exhaustive` is a completeness-oriented policy inside the declared search,
candidate, final-result, and token limits. It neither performs an unbounded
storage scan nor guarantees corpus-complete recall. Coverage affects bounded
preselection and final selection rather than changing the authoritative
reader-evidence representation.

## Replacement criteria

Replace an active calibration only when a declared fit and held-out validation
cover the affected signal. At minimum:

- embedding or feature-geometry changes require a new retrieval fit;
- graph-topology or query-planning changes require live end-to-end validation,
  not only feature replay;
- salience weighting requires explicit committed-use labels;
- latency and category floors are reported separately from aggregate quality;
- reader-free retrieval changes are checked with the separately declared
  end-to-end reader protocol; and
- the new record names retained artifacts and states their actual availability.
