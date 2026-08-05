# Ingestion Layering

Anamnesis separates source persistence, consumer-owned extraction, and
deterministic admission. The first and third layers are model-free product
mechanisms. The middle layer is policy owned by a plugin or another consumer.

## Current Three-Layer Contract

| Layer | Owner | Current responsibility | Model call? |
|---|---|---|---|
| **Raw persistence** | `Memory`, engine, daemon, and write clients | Validate an ingest request and persist the supplied turn, note, or edge with origin and time metadata | No |
| **Consumer extraction** | Plugin or another caller | Decide whether and how to derive a compact candidate from persisted sources | Consumer choice |
| **Deterministic admission** | Shared product APIs | Validate the narrow record type accepted by the current engine and keep it subordinate to cited source nodes | No |

The layers have different failure boundaries. Hook capture is best-effort before
persistence: an unavailable daemon, an unsupported host event, or a filtered
transcript entry can prevent a turn from reaching storage. Once a source has
been persisted successfully, later extraction or admission failure does not
delete or replace it.

## Raw Persistence

`Memory::add`, `add_note`, and the daemon write paths persist the input supplied
by their caller. The engine applies its documented validation and storage
transaction rules; it does not decide whether the text is a good fact or the
right extraction granularity.

The shipped plugin uses lifecycle hooks to submit text-bearing transcript
windows as raw `Episodic` sources. Overlapping successful submissions are
idempotently deduplicated by the capture path. This is a plugin policy, not a
guarantee that every host event or transcript item reaches the daemon.

## Consumer Extraction

Extraction may be immediate, deferred, local-model based, agent driven, or
absent. Provider choice, prompts, retry policy, and review policy remain outside
the engine crate. Extraction output is an untrusted candidate until it crosses a
supported admission surface.

The current plugin workflow described by
[ADR-0013](../adr/0013-reasoning-capture-pipeline.md) preserves successfully
captured raw sources independently of its deferred extraction queue. Another
consumer can use a different extraction policy without changing graph
mechanics.

## Current Deterministic Admission

The current shared derived-record surface is deliberately narrow:
`Memory::add_atomic_fact` admits a reviewed atomic routing fact into an isolated
sidecar. `Memory::add_atomic_fact_relation` admits a reviewed typed routing
relation between two such facts. Fact admission rejects empty claims, missing
or non-Episodic sources, sources from different sessions or scopes, malformed
validity intervals, and invalid embeddings. Relation admission rejects missing
or self endpoints, empty review identity/profile/idempotency fields, disjoint
concrete scopes, and validity intervals with no endpoint-time intersection. The
extraction adapter can additionally validate byte-exact grounding metadata
before invoking these APIs.

Admitted atomic facts and relations remain outside graph topology, attraction,
forgetting, normal node FTS, and graph budgets. They can route a complex query
through bounded typed adjacency to cited raw sources, but only those raw sources
enter the reader-facing evidence lane. This is the shared deterministic
admission behavior exercised by direct `Memory` callers and product clients; it
is not a general entity/fact catalog or a reviewed-claim lifecycle.

## Proposed: General Formation Admission (ADR-0015)

[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
proposes extending the narrow atomic-fact surface into source, grounded-routing,
reviewed-claim, and observation classes backed by one catalog transaction. That
transaction, its review states, catalog types, invalidation rules, and
cross-entry-point parity are implementation promotion criteria, not current
behavior.

## Contributor Rules

- Describe capture separately from persistence. Only a successfully persisted
  source is guaranteed to survive optional formation failure.
- Keep provider selection and inference outside the engine crate.
- Route current derived retrieval through the public atomic-fact admission
  surface; do not present sidecar text as independent evidence.
- Label the general catalog, canonical relation-evidence, observation, and
  generalized admission behavior as proposed until ADR-0015 is accepted; keep
  the implemented narrow atomic routing-relation contract distinct.
- Keep all deterministic admission and persistence errors observable and
  fallible.

See [ADR-0012](../adr/0012-daemon-core-mcp-plugin-clients.md),
[ADR-0013](../adr/0013-reasoning-capture-pipeline.md), and
[framework-layer](framework-layer.md).
