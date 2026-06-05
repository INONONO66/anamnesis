# Interaction Model

Interactions are the only way persistent cognitive quantities change. Read-only retrieval computes transient activation. Commit records that a caller actually used a site, path, feedback signal, or tension, and only then integrates work into retained action or conductance.

## Stored Quantities

| Quantity | Meaning |
|---|---|
| `A_i` retained action | Log need-odds that site `i` will be useful |
| `C_ij` conductance | Log likelihood ratio contributed by cue/path `j -> i` |

Public salience and edge weight are projections of these reservoirs.

## Principles

- Retrieval is read-only.
- Commit is explicit.
- Every persistent delta must have an interaction trace.
- Reinforcement is bounded and usually based on prediction error.
- Path strengthening is based on committed flux, not merely being traversed.
- Time is an interaction: unused reservoirs leak.
- Contradiction activation records tension frequency, not truth.

## State And Events

| Event | Persistent Effect |
|---|---|
| `SiteInserted` | Initializes retained action and optional coupling |
| `Accessed` | Applies decay then access reinforcement |
| `FeedbackReceived` | Applies positive or negative prediction-error update |
| `CoReadout` | Strengthens site pairs read out together |
| `PathUsed` | Strengthens edges actually used by context generation |
| `TimeElapsed` | Applies dissipation and edge leakage |
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
| retrieval | graph, reservoirs, projections | nothing persistent |
| commit | retrieval trace, usage event | retained action, conductance, timestamps, traces |

The same query can be rerun safely before commit. Commit must validate that the trace corresponds to the graph state it updates.

## Interaction Types

| Interaction | Inputs | Main Delta |
|---|---|---|
| `SiteInserted` | observation, surprise, nearest sites | `dA_i` initial charge, weak `dC_ij` |
| `Accessed` | site id, timestamp, readout work | `dA_i` after decay |
| `FeedbackReceived` | target sites, reward signal | `dA_i` via prediction error |
| `CoReadout` | site pairs, activations | `dC_ij` pair flux |
| `PathUsed` | edge ids, path current | `dC_ij` path flux |
| `TimeElapsed` | now, checkpoints | leakage |
| `TensionActivated` | contradiction pair, stress | tension trace |

## Derived Deltas

Reinforcement uses a single learning rate `eta = 1 - 0.5^(1/N)`, derived from one target co-activation count `N` as in [conductance.md](conductance.md). The same `eta` drives feedback and conductance updates; `access_gain` is a bounded saturating function of this family. Per-channel rates are an optional later refit of one `N`, not separate constants.

### `SiteInserted` - Surprise Gate

```text
dA_i = k * surprise(observation, predicted_embedding, precision)
```

Initial charge is proportional to how much the observation changes the graph's expectation. Familiar but useful input routes to an existing site instead of being rejected.

### `Accessed` - Readout Work

```text
A_after_decay = decay(A_i, now - accessed_at)
A_next        = A_after_decay + access_gain(readout_work)
```

Decay first, reinforce second. `access_gain` is bounded and saturating, so repeated access cannot drive retained action past its ceiling.

### `FeedbackReceived` - Rescorla-Wagner

```text
dA_i = eta * (lambda - predicted_value_i)
```

Already well-predicted sites move less. Negative feedback can lower retained action but must preserve provenance and source content.

### `CoReadout` / `PathUsed` - Hebbian-Oja Conductance

```text
dC_ij = eta * flux_ij * (1 - C_ij)
```

`flux_ij` comes from committed path current or co-readout activation. The Oja-style bound prevents runaway hubs.

### `TimeElapsed` - Power-Law Dissipation

```text
A_i' = decay(A_i, delta_days, node_type)
C_ij' = leak_idle_edge(C_ij, idle_days)
```

Time changes reservoirs only through maintenance or lazy decay before access.

## Splay-Tree Analogy

The behavior is splay-like but not structural rotation. Repeated access makes the accessed site and used paths easier to retrieve next time:

```text
splay tree:      access moves a node near the root
conductive graph: access raises retained action and used conductance
```

The graph topology does not rotate. Instead, impedance falls along paths that repeatedly carry committed current.

## Contradictions And Local Relaxation

Contradiction activation records tension frequency. It does not choose a winner. Resolution requires new evidence, supersession, or explicit consumer action.

## Access Timestamp Rules

- Update `accessed_at` only on committed access.
- Apply lazy decay before setting the new access timestamp.
- Do not update access timestamps during candidate generation or read-only query.

## Related Documents

- Conductance updates are defined in [conductance.md](conductance.md).
- Dissipation is defined in [dissipation.md](dissipation.md).
- Frustration and tension are defined in [frustration.md](frustration.md).
- Readout work is defined in [readout-scoring.md](readout-scoring.md).
