# Temporal Model

Anamnesis separates record time, fact time, and access time. A memory can be recorded now, describe a fact valid in the past, and be reactivated later.

## Time Axes

| Axis | Fields | Meaning |
|---|---|---|
| Record time | `created_at`, `updated_at` | When the engine stored or modified the site/edge |
| Access time | `accessed_at` | Last committed access used for dissipation and reinforcement |
| Fact time | `valid_from`, `valid_until` | When the content is true or applicable |
| Query time | `as_of` | Time filter requested by `fact_at` or temporal query |

Record time is not fact time. A decision recorded today can describe a policy that became valid last month.

## Valid Intervals

`valid_from` and `valid_until` define a half-open interval:

```text
valid_from <= as_of < valid_until
```

Missing bounds mean unbounded on that side. A fact with no validity interval is treated as generally valid unless scope policy says otherwise.

## Access History

Access is a committed interaction. It is not updated by read-only retrieval. The caller must commit that a site was actually used before `accessed_at` and retained action move.

Access history drives:

- lazy dissipation,
- reinforcement on use,
- recency-aware packaging,
- stale-site observability.

## Tick And Access Interactions

`tick(now)` applies batch dissipation. `touch(node_id, now)` applies lazy dissipation first, then access reinforcement. The ordering is invariant:

```text
decay first
then reinforce
```

This prevents stale sites from receiving a full reinforcement boost without paying accumulated leakage.

## Time Units

Core formulas use days for long-horizon memory decay unless a document explicitly states another unit. Query-local activation iterations are dimensionless and must not be mixed with between-query dissipation time.

## `fact_at` Queries

`fact_at(query, as_of)` returns context filtered to facts valid at `as_of`. It must not traverse edges invalid at the requested time. Tensions are also time-filtered: a contradiction is active only if both sides and the constraint edge are valid together.

## Design Invariants

- Query-time activation is fast transient state; dissipation is slow between-query state.
- Record time, fact time, and access time remain distinct.
- Access timestamps update only through committed interactions.
- Time filtering applies before readout packaging.
- Missing validity bounds are explicit unbounded intervals, not unknown errors.
