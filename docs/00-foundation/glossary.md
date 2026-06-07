# Glossary

This glossary defines the terms used across the Anamnesis technical specification. The core naming rule is simple: persistent state lives in reservoirs, public values are bounded projections, and query-time quantities are transient.

## Reservoirs And Projections

| Term | Meaning |
|---|---|
| `retained_action A_i` | Persistent memory strength for site `i`; composite `A_i = B_i + P_i` of base-level activation and an evidence prior; log need-odds |
| `base-level B_i` | Multi-trace ACT-R base-level activation over the node's access-trace history: `B_i = ln( Σ_j (now − t_j)^(−d_j) )` where each trace j stores (timestamp, per-trace decay rate `d_j`) computed at creation from current activation; owns forgetting and use-driven reinforcement; computed on demand from traces |
| `evidence prior P_i` | Separate persistent prior holding encoding surprise, feedback / social reinforcement, and peer trust; a decay-exempt evidence offset |
| `salience s_i` | Bounded public projection of the sum `B_i + P_i`; useful for ranking and packaging, not authoritative state |
| `conductance C_ij` | Persistent associative strength from cue `j` to target `i`; log likelihood ratio |
| `edge weight w_ij` | Bounded public projection of `C_ij`; storage/API-facing value |
| reservoir | The authoritative persistent quantity that dynamics update |
| projection | A clipped or transformed public view derived from a reservoir |

Core axiom:

```text
retained action A_i = B_i + P_i = log prior need-odds
conductance C_ij    = log likelihood ratio
total activation    = (B_i + P_i) + sum_j W_j * S_ji
                    = log posterior need-odds
```

`A_i` decomposes into two terms: the base-level `B_i = ln( Σ_j (now − t_j)^(−d_j) )` over the node's access traces, where each trace j stores (timestamp, per-trace decay rate `d_j`) computed from activation `m_j` at creation via `d_j = m_type · ( c · e^{m_j} + α )`, owning power-law forgetting and use-driven reinforcement; and the evidence prior `P_i` (encoding surprise, feedback, social reinforcement, peer trust). Dissipation acts on `B_i` only; `P_i` is a decay-exempt evidence offset.

## Core Terms

| Term | Definition |
|---|---|
| site | A node in the memory graph; stores a fragment, fact, identity, hypothesis, or event |
| fragment | Preserved source text, usually an episodic turn or extracted knowledge |
| cue | A seed signal from text, embedding, entity, scope, or explicit node id |
| query field | The potential field imposed by a query over candidate sites |
| activation flow | Query-local spreading response over the graph; read-only and transient |
| current `I_ij` | Activation flowing across an edge during a query; used for trace and commit |
| impedance `Z_i` | Effective difficulty of activating site `i` from the current query field |
| readout | Selecting lit sites for packaging into context |
| committed work | Evidence that a readout site or path was actually used by the caller |
| dissipation | Time-based aging of the base-level term `B_i` as its access traces age (power-law); does not act on the evidence prior `P_i` |
| frustration | Constraint stress when contradictory sites are active together |
| tension | A surfaced contradiction item in returned context |
| scope | Visibility and validity boundary such as session, project, or universal |
| origin | Provenance tuple identifying peer, session, source kind, scope, and confidence |
| crystallize | Create a synthesis site from selected source sites without overwriting them |

## Type Families

| Family | Examples | Role |
|---|---|---|
| Identity | `IdentityCore`, `IdentityLearned`, `IdentityState` | Stable or current agent traits |
| Knowledge | `Semantic`, `Procedural`, `Entity`, `Convention`, `Decision`, `Gotcha` | Reusable facts and operating knowledge |
| Debug | `DebugSession`, `Hypothesis`, `Evidence` | Structured debugging lifecycle |
| Memory | `Episodic`, `Event` | Raw or time-bound fragments |
| Custom | `Custom(String)` | Consumer-defined taxonomy |

## Symbols

| Symbol | Meaning |
|---|---|
| `A_i` | retained action for site `i`; the composite `A_i = B_i + P_i` |
| `s_i` | salience projection for site `i`; `s_i = logistic(B_i + P_i)` |
| `C_ij` | conductance from `j` to `i` |
| `w_ij` | projected edge weight |
| `a_i` | query-local activation response |
| `Q` | query field |
| `P` | conductance-normalized transition matrix |
| `alpha` | RWR restart rate |
| `eta` | learning-rate parameter derived from a behavioral specification |
| `lambda` | target reward or asymptote in Rescorla-Wagner-style updates |
| `Sigma` | uncertainty / precision structure for surprise or stress calculations |
| `Z_i` | impedance of site `i` |
| `B_i` | multi-trace ACT-R base-level activation of site `i`; `B_i = ln( Σ_j (now − t_j)^(−d_j) )` where `d_j = m_type · ( c · e^{m_j} + α )`; owns forgetting and use-driven reinforcement |
| `d_j` | per-trace decay rate for trace `j`, computed once at creation from current activation `m_j`, then stored immutably with the trace |
| `m_j` | activation from the existing traces evaluated at the moment trace `j` is created; `m_j = ln( Σ_{k existing} (t_j − t_k)^(−d_k) )` (empty history ⇒ `m_j = −∞`) |
| `m_type` | per-`node_type` decay multiplier; outer factor on `d_j` (a type with `m_type = 0` is permanent) |
| `α` | decay intercept `DECAY_INTERCEPT`; floor decay rate when activation is zero |
| `c` | decay scale `DECAY_SCALE`; sensitivity of the decay rate to current activation `m_j` |
| `P_i` | evidence prior for site `i`; encoding surprise, feedback / social reinforcement, and peer trust; decay-exempt (does not undergo base-level use-driven decay) |
| `W_j` | Attentional weight of cue `j` in the activation sum |
| `S_ji` | Associative strength from cue `j` to target `i`; the log-LR contribution, equal to `C_ij` |

## Common Distinctions

| Do Not Confuse | Distinction |
|---|---|
| salience vs retained action | Salience is a bounded display/ranking value; retained action is the persistent reservoir |
| edge weight vs conductance | Weight is the public projection; conductance is the associative log-LR reservoir |
| retrieval vs commit | Retrieval reads and computes transient activation; commit changes reservoirs using traces |
| contradiction vs deletion | Contradiction produces stress and tension; it does not erase either fact |
| scope vs trust | Scope controls visibility; trust controls how strongly origin evidence should count |
| vector similarity vs association | Similarity proposes seeds; conductance determines graph flow |

## Naming Rules

- Use `site` for the cognitive memory object and `node` only when discussing graph/storage mechanics.
- Use `conductance` for the persistent associative reservoir and `weight` for its bounded projection.
- Use `retained action` for persistent memory strength and `salience` for its projected public value.
- Use `activation` only for query-local transient response.
- Use `commit` only when a caller confirms that a readout was used.
