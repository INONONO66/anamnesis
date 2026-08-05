# Producer Identity And Origin

Producer identity records who or what supplied a source. It is provenance, not
authentication, authorization, or an automatic truth score.

## Current contract

Every graph node carries:

```text
Origin {
    peer_id,
    source_kind,
    session_id,
    scope,
    confidence,
}
```

| Field | Meaning |
|---|---|
| `peer_id` | Stable opaque producer id assigned by the consumer |
| `source_kind` | Human, agent, tool, import, system, or other source category |
| `session_id` | Consumer-provided session provenance |
| `scope` | Visibility/applicability path enforced during retrieval |
| `confidence` | Source-side admission signal in `[0, 1]` |

The engine does not maintain a peer registry, resolve aliases, authenticate a
producer, learn peer reputation, or derive a ranking signal from `PeerId`. A
consumer may map external identities to `PeerId`, but the mapping and any
authorization decision remain outside the core.

## Retrieval and packaging

- Origin is retained through source-aware reranking, packaging, rendering, and
  commit traces.
- Scope eligibility is evaluated independently from producer identity.
- A high-confidence or familiar producer cannot make a hidden source visible.
- Contradictory sources keep their separate origins and are surfaced together
  when both are visible.
- Read-only retrieval never changes origin or confidence.

## Derived evidence

The evidence catalog proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
records producer and formation-profile identity for every derived record. That
identity does not replace exact source references and does not determine
admission by itself. Derived visibility remains no broader than the cited
sources.

Canonical entity aliases in the evidence catalog describe entities mentioned
by sources; they are distinct from the producer identity that wrote the source.

## Invariants

- Origin is present on every source node.
- `PeerId` is opaque and stable within the consumer's identity domain.
- Producer identity never overrides scope or temporal validity.
- Confidence is calibrated input, not an authorization or truth verdict.
- Derived records retain exact source references, through which every
  contributing origin remains auditable.

## Related documents

- Origin storage is defined in [graph-model.md](graph-model.md).
- Evidence references are defined in [evidence-model.md](evidence-model.md).
- Scope rules are defined in [scoping-promotion.md](scoping-promotion.md).
- Feedback effects are defined in
  [social.md](../04-cognitive-dynamics/social.md).
