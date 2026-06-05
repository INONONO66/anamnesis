# Peer Identity

Peer identity records who produced a fragment and how that source should affect retrieval. It is a provenance model, not an authorization system.

## Peer Model

| Field | Meaning |
|---|---|
| `peer_id` | Stable peer identifier |
| `display_name` | Human-readable label |
| `trust_level` | Coarse source class such as human, agent, tool, or system |
| `trust_profile` | Calibrated reliability signals |
| `created_at` | Registration time |

Peers can be humans, agents, tools, importers, or system processes. The engine stores their identity so later retrieval can explain provenance and combine corroborating evidence.

## Origin And Peer

Every site origin references a peer:

```text
Origin {
    peer_id,
    source_kind,
    session_id,
    scope,
    confidence,
}
```

Origin confidence is a source-side estimate. Trust profile is learned or configured peer reliability. They are separate signals.

## Trust Profile

| Signal | Meaning |
|---|---|
| prior reliability | Initial belief about this peer |
| corroboration count | How often other peers support this peer's claims |
| contradiction count | How often this peer conflicts with later accepted context |
| feedback history | Consumer feedback tied to this peer's fragments |
| scope expertise | Scope-specific reliability |

Trust changes must be evidence-based. The engine must not silently promote or demote a peer without a traceable interaction.

## Retrieval Effects

Peer identity can affect:

- ranking through trust-weighted readout,
- tension surfacing when trusted peers disagree,
- cross-agent entity linking,
- provenance packaging,
- promotion from narrow scopes to broader scopes.

Peer identity must not override scope visibility. A trusted private fragment still cannot leak into an unauthorized query scope.

## Session Summary

`SessionSummary` carries `agent_id`, `session_id`, and node ids produced in a session. `reflect_batch` uses this metadata to add entity links across agents. It does not merge nodes or call an LLM.

## Related Documents

- Origin fields are defined in [graph-model.md](graph-model.md).
- Scope rules are defined in [scoping-promotion.md](scoping-promotion.md).
- Social updates are described in [social.md](../04-cognitive-dynamics/social.md).
