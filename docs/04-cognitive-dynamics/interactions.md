# Interaction Model

Interactions are the only way persistent cognitive quantities change. Read-only retrieval computes transient activation. Commit records that a caller actually used a site, path, feedback signal, or tension, and only then integrates work into retained action or conductance.

## Stored Quantities

| Quantity | Meaning |
|---|---|
| `A_i` retained action | Composite log need-odds that site `i` will be useful; `A_i = B_i + P_i` |
| `B_i` base-level | Multi-trace ACT-R base-level activation derived on demand from the node's access-trace history; owns forgetting and use-driven reinforcement |
| `P_i` evidence prior | Separate persistent prior (encoding surprise, feedback / social reinforcement, peer trust); decay-exempt |
| `C_ij` conductance | Log likelihood ratio contributed by cue/path `j -> i` |

Public salience is the bounded logistic projection of the sum `B_i + P_i`; edge weight projects `C_ij`. The base level is computed from the access-trace history rather than stored as a scalar, while `P_i` and `C_ij` are maintained reservoirs.

## Principles

- Retrieval is read-only.
- Commit is explicit.
- Every persistent delta must have an interaction trace.
- Reinforcement is bounded and usually based on prediction error.
- Path strengthening is based on committed flux, not merely being traversed.
- Time is an interaction: an unused site's base level `B_i` falls as its traces age, and unused edges leak.
- Contradiction activation records tension frequency, not truth.

## State And Events

| Event | Persistent Effect |
|---|---|
| `SiteInserted` | Seeds `P_i` from encoding surprise and a creation trace for `B_i`; optional coupling |
| `Accessed` | Appends an access trace stamped at `now` (no scalar access gain) |
| `FeedbackReceived` | Applies positive or negative prediction-error update to `P_i` |
| `CoReadout` | Strengthens site pairs read out together |
| `PathUsed` | Strengthens edges actually used by context generation |
| `TimeElapsed` | Leaks idle edges; `B_i` is recomputed at `now` from traces |
| `TensionActivated` | Records that a contradiction was presented |
| `Crystallized` | Adds synthesis site and source links |

## Physical Interpretation Of Retrieval

```text
query field Q
  -> seed distribution
  -> activation flow a*
  -> path current I_ij = a_i * g_ij   (g_ij = project_conductance(C_ij) * edge_type_factor_ij)
  -> readout sites
  -> optional commit trace
```

`a*`, `I_ij`, impedance, and stress are transient. They become persistent only through a committed event that names what was used.

## Read-Only Vs Commit

| Phase | Reads | Writes |
|---|---|---|
| retrieval | graph, traces, `P_i`, conductance, projections | nothing persistent |
| commit | retrieval trace, usage event | access traces (for `B_i`), `P_i`, conductance, timestamps |

The same query can be rerun safely before commit. Commit must validate that the trace corresponds to the graph state it updates.

## Interaction Types

| Interaction | Inputs | Main Delta |
|---|---|---|
| `SiteInserted` | observation, surprise, nearest sites | `P_i ← k·eps` encoding-surprise prior, creation trace seeds `B_i`, weak `dC_ij` |
| `Accessed` | site id, timestamp, readout work | append access trace at `now` (raises `B_i`) |
| `FeedbackReceived` | target sites, reward signal | `dP_i` via prediction error |
| `CoReadout` | site pairs, activations | `dC_ij` pair flux |
| `PathUsed` | edge ids, path current | `dC_ij` path flux |
| `TimeElapsed` | now, checkpoints | edge leakage; `B_i` recomputed at `now` |
| `TensionActivated` | contradiction pair, stress | tension trace |
| `Crystallized` | source sites, synthesis content | new site (weighted-from-sources prior + creation trace), seed `dC_sj` on `ConsolidatedFrom` edges; sources unchanged |

## Derived Deltas

Reinforcement uses a single learning rate `eta = 1 - 0.5^(1/N)`, derived from one target co-activation count `N` as in [conductance.md](conductance.md). The same `eta` drives feedback (`dP_i`) and conductance updates. Use-driven reinforcement of `B_i` is not a scalar add: a committed access appends a trace, and repeated access is bounded by the 32-trace window. Per-channel rates are an optional later refit of one `N`, not separate constants.

### `SiteInserted` - Surprise Gate

```text
P_i = k * surprise(observation, predicted_embedding, precision)
```

The encoding-surprise prior `P_i` is proportional to how much the observation changes the graph's expectation (ADR-0009). Allocation also lays down a creation trace stamped at `now`, which seeds `B_i`. Familiar but useful input routes to an existing site instead of being rejected.

### `Accessed` - Readout Work

```text
m_now = ln( Σ_j (now − t_j)^(−d_j) )         // activation from EXISTING traces, before appending
d_now = m_type · ( c · e^{m_now} + α )       // computed once, then frozen with the new trace
traces_i ← append(traces_i, (now, d_now))    // bounded 32-trace window; each trace stores its timestamp AND its decay rate
B_i = ln( Σ_j (now − t_j)^(−d_j) )
```

A committed access appends a trace stamped at `now`, carrying its own decay rate `d_now` computed from the activation of the existing traces and then frozen; it does not apply a scalar `access_gain`. Decay-first ordering is intrinsic because `B_i` ages all prior traces to `now` inside the same sum that adds the new trace. The bounded trace window keeps repeated access from driving `B_i` without limit.

### `FeedbackReceived` - Rescorla-Wagner

```text
dP_i = eta * (lambda - predicted_i)
```

Feedback updates the decay-exempt evidence prior `P_i`, not the base level. Already well-predicted sites move less. Negative feedback can lower `P_i` but must preserve provenance and source content.

### `CoReadout` / `PathUsed` - Hebbian-Oja Conductance

```text
dC_ij = eta * flux_ij * (1 - C_ij)
```

`flux_ij` comes from committed path current or co-readout activation. The Oja-style bound prevents runaway hubs.

### `TimeElapsed` - Power-Law Dissipation

```text
B_i = ln( Σ_j (now − t_j)^(−d_j) )   // recomputed at now; each d_j is static from when its trace was created
C_ij' = leak_idle_edge(C_ij, idle_days)
```

Time does not apply a scalar shift to retained action. `B_i` simply reflects the current `now` against the stored access traces, so forgetting is a consequence of reading `B_i` later rather than a maintained subtraction. The evidence prior `P_i` is decay-exempt and unchanged by elapsed time. Edge conductance leaks only when idle.

## Splay-Tree Analogy

The behavior is splay-like but not structural rotation. Repeated access makes the accessed site and used paths easier to retrieve next time:

```text
splay tree:      access moves a node near the root
conductive graph: access appends a trace that raises B_i, and raises used conductance
```

The graph topology does not rotate. Instead, impedance falls along paths that repeatedly carry committed current.

## Contradictions And Local Relaxation

Contradiction activation records tension frequency. It does not choose a winner. Resolution requires new evidence, supersession, or explicit consumer action.

## Access Timestamp Rules

- Append an access trace and update `accessed_at` only on committed access.
- The appended trace is stamped at `now`; `B_i` ages prior traces to `now` whenever it is read, so no separate decay step precedes the append.
- Do not append traces or update access timestamps during candidate generation or read-only query.

## Related Documents

- Conductance updates are defined in [conductance.md](conductance.md).
- Dissipation is defined in [dissipation.md](dissipation.md).
- Frustration and tension are defined in [frustration.md](frustration.md).
- Readout work is defined in [readout-scoring.md](readout-scoring.md).
