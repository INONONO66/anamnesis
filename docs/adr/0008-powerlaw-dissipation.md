# 0008. Forgetting Is Power-Law Base-Level Dissipation

- Status: Accepted
- Date: 2026-06-05
- Related: [dissipation](../04-cognitive-dynamics/dissipation.md), [temporal-model](../02-knowledge-model/temporal-model.md)

## Context

Memory should fade naturally without deleting source fragments. Human and ACT-R memory literature favors power-law decay over arbitrary linear fading.

## Decision

Model forgetting as power-law dissipation of retained action:

```text
B_i = ln(sum_j t_j^-d)
A_i' = decay(A_i, delta_days, node_type, d)
s_i' = project_salience(A_i')
```

Apply decay before reinforcement on committed access:

```text
A_after_decay = decay(A_before, now - accessed_at)
A_next        = reinforce(A_after_decay, work)
```

Low salience hides a site from broad retrieval but does not delete it.

## Consequences

Benefits:

- Matches ACT-R base-level memory shape.
- Preserves source fragments for reactivation.
- Makes stale knowledge less likely without erasing provenance.
- Keeps forgetting and reinforcement on one reservoir.

Tradeoffs:

- Full tick can be expensive on large graphs.
- Lazy decay requires correct checkpoint handling.
- Decay constants must be calibrated.

## Alternatives Considered

### Linear decay

Rejected. It lacks cognitive basis and behaves poorly across long horizons.

### Delete on threshold

Rejected. It destroys recoverability and provenance.

### Separate archive tiers only

Rejected. Tiering alone is a policy label, not a dynamics model.

### Reinforce before decay

Rejected. It lets stale sites avoid accumulated leakage.
