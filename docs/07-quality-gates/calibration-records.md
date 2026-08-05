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

These production defaults are shared by every supported entry point. The same
`Memory::search_reranked` and query-aware context-rendering path is used by
direct crate consumers, MCP, hooks, the plugin, and the production-path benchmark
route.

| Setting | Active value |
|---|---:|
| cognitive source-search limit | `20` |
| reranker candidate-document limit | `50` |
| requested final evidence limit | `20` |
| simple direct-query delivery cap | `12` |
| dense query batch, relationship/inference | at most `4` total surfaces |
| dense query batch, collection | at most `5` total surfaces |
| auxiliary dense-union RRF prior | `0.25` |
| reviewed relation expansion | `8` seeds, depth `2`, `32` relations, `8` endpoint facts, `8` raw sources |
| context-ready p95 release boundary | `4 s` |

Direct and temporal plans preserve the original-query source search. Bounded
collection, relationship, and inference plans may add deterministic clause,
predicate, decomposition, or entity surfaces. Stored embeddings are scanned
once, auxiliary results are deduplicated into one lower-prior union, and
source-aware selection operates after reranking. Atomic facts remain in their
isolated index and can route only to cited, live, scope-valid raw sources.
Relationship and inference plans may additionally traverse the bounded reviewed
relation lane; relation and fact text never becomes reader evidence.


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
