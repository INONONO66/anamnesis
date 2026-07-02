# Peer Identity

> **Roadmap / not shipped (as of v0.10.0).** The multi-peer registry, trust levels, trust profiles, trust-weighted readout, and `reflect_batch` cross-agent linking described here were **removed in the [v0.10.0 shrink](../adr/0014-shrink-to-product.md)** — they had no consumer (production always ran with a single `PeerId(0)` and the readout trust term is now a neutral `1.0`). What **survives** is the storage-level provenance: `Origin` still carries `peer_id` and `source_kind`. This document is retained as the **design intent** for a future multi-peer/provenance layer (a re-add condition and the ADR-0015 schema-redesign track in ADR-0014); read it as future direction, not current behavior.

Peer identity records who produced a fragment and how that source should affect retrieval. It is a provenance model, not an authorization system.

## Peer Model

| Field | Meaning |
|---|---|
| `peer_id` | Stable peer identifier |
| `display_name` | Human-readable label |
| `trust_level` | Ranked authority level (`Untrusted` … `Owner`); source classification is `source_kind` on `Origin` |
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
- provenance packaging.

Peer identity also informs promotion eligibility: corroboration and contradiction signals feed the promotion conditions in [scoping-promotion.md](scoping-promotion.md). Promotion itself is a write operation (it adds a broader-scope synthesis and `ConsolidatedFrom` edges); read-only retrieval never performs it.

Peer identity must not override scope visibility. A trusted private fragment still cannot leak into an unauthorized query scope.

## Session Summary

`SessionSummary` carried `peer_id`, `session_id`, and node ids produced in a session; `reflect_batch` used this metadata to add entity links across agents (metadata only — it did not merge nodes or call an LLM). Both were removed in v0.10.0 (see the banner above); this section records the intended shape for a future multi-peer layer.

## Related Documents

- Origin fields are defined in [graph-model.md](graph-model.md).
- Scope rules are defined in [scoping-promotion.md](scoping-promotion.md).
- Social updates are described in [social.md](../04-cognitive-dynamics/social.md).
