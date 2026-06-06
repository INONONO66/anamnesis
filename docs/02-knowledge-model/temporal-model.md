# Temporal Model

Anamnesis separates record time, fact time, and access time. A memory can be recorded now, describe a fact valid in the past, and be reactivated later.

## Time Axes

| Axis | Fields | Meaning |
|---|---|---|
| Record time | `created_at`, `updated_at` | When the engine stored or modified the site/edge |
| Access time | `accessed_at`, `access_history` | Last committed access plus the bounded 32-trace window that is the persistent substrate of base-level `B_i` |
| Fact time | `valid_from`, `valid_until` | When the content is true or applicable |
| Query time | `as_of` | Time filter requested by `fact_at` or temporal query |

Record time is not fact time. A decision recorded today can describe a policy that became valid last month.

## Valid Intervals

`valid_from` and `valid_until` define a half-open interval:

```text
valid_from <= as_of < valid_until
```

Missing bounds mean unbounded on that side. A fact with no validity interval is treated as generally valid unless scope policy says otherwise.

A bounded interval must be well-formed: `valid_from < valid_until`. Because the upper bound is exclusive, a zero-width interval (`valid_from == valid_until`) is empty and matches no `as_of`, so it is treated as malformed by validity-interval observability. Encode an instantaneous fact or event by leaving `valid_until` unbounded (it becomes true at `valid_from` and stays true), or, if it must expire, by giving it a minimal nonzero width (`valid_until = valid_from + 1` time unit). Never encode a point in time as a zero-width interval.

## Access History

Access is a committed interaction. It is not updated by read-only retrieval. The caller must commit that a site was actually used before `accessed_at` and the access history move.

`access_history` is a bounded 32-trace window — a creation trace plus each committed access — and it is the PERSISTENT substrate of base-level activation:

```text
B_i = ln( sum_j (now - t_j)^(-d*m_type) )
```

The `t_j` are the traces in this window; `m_type` is the `node_type` policy multiplier on the single decay prior `d`. `B_i` is computed on demand from the trace history; it is NOT maintained by incremental scalar decay. Because the sum ages every prior trace to `now` before adding any new trace, the multi-trace form reproduces power-law forgetting AND the testing and spacing effects.

The persistent access-history window drives:

- base-level forgetting and use-driven reinforcement (`B_i`),
- recency-aware packaging,
- stale-site observability.

The evidence prior `P_i` is a separate persistent prior (encoding surprise, feedback / social reinforcement, peer trust). It does NOT undergo base-level (use-driven) decay; it is a decay-exempt evidence offset. Public salience is the bounded logistic projection of the sum, `s_i = logistic(B_i + P_i)`.

## Tick And Access Interactions

`tick(now)` ages base-level activation by recomputing `B_i` from the access-history window at the new `now` (the evidence prior `P_i` is not use-decayed). `touch(node_id, now)` appends a fresh trace stamped at `now` into the window.

Decay-first ordering is INTRINSIC. Appending a now-stamped trace into a current-time-aged sum means `B_i` always ages prior traces to `now` before adding the new one — so there is no separate decay-then-reinforce scalar step and no `reinforce(A, work)` function:

```text
B_i = ln( sum_j (now - t_j)^(-d*m_type) )   // existing t_j already aged to now
        append a fresh trace t = now         // contributes (now - now + epsilon)^... 
```

This intrinsically prevents stale sites from receiving a full reinforcement boost without paying accumulated leakage: the old traces are evaluated at the current `now`, so their decayed contribution is already reflected before the new trace is added.

## Time Units

Core formulas use days for long-horizon memory decay unless a document explicitly states another unit. Query-local activation iterations are dimensionless and must not be mixed with between-query dissipation time.

## `fact_at` Queries

`fact_at(query, as_of)` returns context filtered to facts valid at `as_of`. It must not traverse edges invalid at the requested time. Tensions are also time-filtered: a contradiction is active only if both sides and the constraint edge are valid together.

## Design Invariants

- Query-time activation is fast transient state; base-level aging is slow between-query state.
- Record time, fact time, and access time remain distinct.
- Access timestamps and the access-history window update only through committed interactions.
- The access-history window (bounded to 32 traces) is the persistent substrate of `B_i`; `B_i` is computed from it, never maintained as an incremental scalar.
- Time filtering applies before readout packaging.
- Missing validity bounds are explicit unbounded intervals, not unknown errors.
