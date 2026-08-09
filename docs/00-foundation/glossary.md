# Glossary

This glossary defines the terms used across the Anamnesis technical specification. The core naming rule is simple: persistent state lives in reservoirs, public values are bounded projections, and query-time quantities are transient.

## Reservoirs And Projections

| Term | Meaning |
|---|---|
| `retained_action A_i` | Persistent memory strength for site `i`; composite `A_i = B_i + P_i` of base-level activation and an evidence prior; log need-odds |
| `base-level B_i` | Multi-trace ACT-R base-level activation over the node's access-trace history: `B_i = ln( Σ_j (now − t_j)^(−d_j) )` where each trace j stores (timestamp, per-trace decay rate `d_j`) computed at creation from current activation; owns forgetting and use-driven reinforcement; computed on demand from traces |
| `evidence prior P_i` | Separate persistent prior holding encoding surprise and explicit feedback; a decay-exempt evidence offset |
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

`A_i` decomposes into two terms: the base-level `B_i = ln( Σ_j (now − t_j)^(−d_j) )` over the node's access traces, where each trace j stores (timestamp, per-trace decay rate `d_j`) computed from activation `m_j` at creation via `d_j = m_type · ( c · e^{m_j} + α )`, owning power-law forgetting and use-driven reinforcement; and the evidence prior `P_i` (encoding surprise and explicit feedback). Dissipation acts on `B_i` only; `P_i` is a decay-exempt evidence offset.

## Core Terms

| Term | Definition |
|---|---|
| site | A node in the cognitive memory graph |
| source fragment | Persisted text fragment that remains the source for any derived routing record |
| atomic fact | Source-bound routing record accepted by `AtomicFactInput` / `add_atomic_fact`; it has no engine-enforced review provenance and is never rendered as independent evidence |
| reviewed derivation | Explicitly reviewed, typed routing proposition accepted by `ReviewedDerivationInput` / `add_reviewed_derivation`; it records review provenance and still routes only to cited raw evidence |
| recall derivation | Query-time `RecallDerivation` policy: return evidence-stated values (`Extractive`) or permit only bounded inference from grounded personal/changing premises (`GroundedInference`) |
| reader contract | Provider-neutral `RecallReaderContract` compiled from a `RecallPlan`; it defines staged reading instructions, typed draft structure, citation-membership checks, and bounded recovery without performing model calls |
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
| scope | Opaque applicability label used by current retrieval ranking; it is not an authorization boundary |
| origin | Provenance tuple identifying peer, session, source kind, scope, and confidence |
| crystallize | Create a synthesis site from selected source sites without overwriting them |

## Knowledge Types

`KnowledgeType` is the current compact cognitive-node taxonomy. Current atomic
facts are isolated routing records rather than additional knowledge types.

| Variant | Role |
|---|---|
| `Episodic` | Raw or time-bound fragment — a specific event or conversation turn |
| `Semantic` | Reusable fact or generalization; the target of consolidation |
| `Identity` | Stable retrieval anchor / operating principle; routed to a dedicated context partition and used as a retrieval prior |
| `Custom(String)` | Consumer-defined taxonomy (renders by its bare label) |

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
| `P_i` | evidence prior for site `i`; encoding surprise and explicit feedback; decay-exempt (does not undergo base-level use-driven decay) |
| `W_j` | Attentional weight of cue `j` in the activation sum |
| `S_ji` | Associative strength from cue `j` to target `i`; the log-LR contribution, equal to `C_ij` |

## Common Distinctions

| Do Not Confuse | Distinction |
|---|---|
| salience vs retained action | Salience is a bounded display/ranking value; retained action is the persistent reservoir |
| edge weight vs conductance | Weight is the public projection; conductance is the associative log-LR reservoir |
| retrieval vs commit | Retrieval reads and computes transient activation; commit changes reservoirs using traces |
| contradiction vs deletion | Contradiction produces stress and tension; it does not erase either fact |
| scope vs authorization | `ScopePath` influences applicability and ranking; callers enforce access control before invoking the engine |
| vector similarity vs association | Similarity proposes seeds; conductance determines graph flow |
| routing fact vs evidence | A routing fact helps find its persisted raw source; it is not independently authoritative |
| record time vs occurred time | Storage time records when data entered the engine; occurred time records when an event happened |
| candidate vs delivered evidence | Candidate representations may guide ranking; the current product context contains the selected source-backed fragments |

## Proposed ADR-0015 Terms

The following terms belong to the additive design proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md).
They do not name current public types or storage tables.

| Term | Proposed meaning |
|---|---|
| evidence catalog | Source-grounded retrieval representation for entities, facts, relations, observations, and evidence references |
| grounded routing fact | Validated structured claim used only to route retrieval back to its source |
| reviewed claim | Source-cited fact or relation admitted by an explicit consumer review policy |
| observation record | Immutable versioned synthesis over cited facts or sources |
| evidence reference | Exact source node/span or media asset/region reference with content identity |
| evidence chain | Bounded sequence of typed, scope-eligible, time-compatible facts and source references covering requested query slots |
| `EvidenceBundle` | Structured ledger, cited raw evidence, tensions, uncovered slots, and trace returned by proposed evidence-complete recall |

## Naming Rules

- Use `site` for the cognitive memory object and `node` only when discussing graph/storage mechanics.
- Use `conductance` for the persistent associative reservoir and `weight` for its bounded projection.
- Use `retained action` for persistent memory strength and `salience` for its projected public value.
- Use `activation` only for query-local transient response.
- Use `commit` only when a caller confirms that a readout was used.
