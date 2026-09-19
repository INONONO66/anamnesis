# Prospective Episode lineage admission

Implemented compatibility contract (G004 recovery): `remember` without any of
`origin_role`, `lineage_mode`, `parent_recall_ids` retains the existing import
API. Engine.remember/put, journal replay, source adapters and original readers
remain supported. These paths do not authenticate source role or independence:
they create canonical **pre-lineage** Episodes, without a lineage row. Their
semantic provenance is unknown/ineligible. They have not become version 2.

The explicit authenticated path supplies all three fields on `remember`:

```json
{"origin_role":"assistant","lineage_mode":"receipts","parent_recall_ids":["01900000-0000-7000-8000-000000000001"]}
```

`origin_role` is user/assistant/tool/document/operator. Direct input requires
no parents; receipt input requires 1..4 distinct parent IDs. The adapter is
responsible for truthful role and whether it delivered context. Neither source
prose nor actor labels establish these facts. The server owns all roots,
depths, context digests and the version discriminator; callers cannot supply
these fields. A partial/invalid explicit request fails with `invalid_params`
after stored-version selection, without Episode/CAS/outbox/topology changes.

## Custody and bounds

Successful hello creates a server-random connection binding, independent of the
caller-provided client label. Receipts issued on that connection retain it and
an ordered selection of at most 64 server-computed lineage snapshots. New
receipt-linked admission requires the same authenticated connection. A new
connection, even using the same client label/token, cannot reuse a prior
connection's receipt for a new revision. A future reconnectable capability
would require a separate wire contract; client labels are not credentials.
Installation-bound receipt feedback remains compatible with its existing API.

Direct lineage has the Episode's own ID as its only root and depth zero.
Receipt lineage copies each parent's retained selection_digest, pairs/sorts the
parent IDs and digests, unions/sorts roots and adds one hop to maximum depth.
Over 16 roots keeps the first 16; over depth 8 stores 8. Either overflow, an
empty selection, or unknown/incomplete input yields complete=false. An unknown
legacy selection has no invented roots. No ancestry traversal or Fact write
occurs. Current policy checks both delivered sources and snapshot roots before
admission; unknown/missing, foreign, denied or corrupt parents reject atomically.

Parent creation must precede ingestion. A logical one-ms admission tick permits
same-wall-clock-ms receipt/remember ordering without sleeps. Receipt TTL controls
feedback only: a retained expired parent can establish lineage, but a deleted
parent cannot establish new lineage. Children never depend on later retention.
The row, v2 digest, Episode, revision CAS, existing Outbox and session topology
commit together under the writer fence. Lineage rows are not receipt-TTL data.

New explicit lineage admission requires the database; it is not acknowledged
through the legacy spool without parent custody. Metadata-free spool behavior
is unchanged. On database-outcome uncertainty, the immutable stored revision
is checked before any new parent lookup on retry.

## Frozen digests and retries

- Absent episode_digest_version and absent digest_format: frozen insertion-order
  JSON.stringify pre-lineage body.
- Absent episode_digest_version and digest_format=rfc8785-v1: canonical
  pre-lineage body. This existing format is **not** version 2.
- episode_digest_version=2 and digest_format=episode-rfc8785-v2: RFC-8785 body
  including episode_digest_version, origin_role and lineage_digest, alongside
  schema/content/properties/time/payload_hash/previous_revision_key.
- Other stored versions or mismatched discriminators fail closed.

The existing row selects the verifier before new metadata or parent validation.
Exact legacy retries ignore newly supplied lineage fields and never change any
stored field, row, eligibility or delivery binding. V2 retries compare role,
mode and normalized parent IDs to the immutable retained lineage body; they
verify the stored lineage and Episode digests without looking up parents. This
holds for current and historical revisions, expired/deleted parents and daemon
restart. Changed meaning or lineage under the same revision is a conflict.
No data migration or in-place upgrade is provided; a new source revision may
opt into v2.

## Semantic adapter

Store.semanticEpisode is a fenced installation-only retained-source adapter.
It reads and checks immutable Episode/lineage digests and policy, never accepts
caller source text, role, ancestry or replacement time. It translates the old
TimePoint explicitly: second -> semantic instant, aligned day/month/year ->
the same precision. Legacy minute has no semantic ABI equivalent and rejects;
misaligned coarse times reject rather than being rounded or cast. Language
and generation/entity-resolution custody remain separate semantic-stage inputs.
The semantic validator rejects unknown/incomplete ancestry; original readers
remain original readers, not semantic extraction eligibility claims. The older
claim/judge audit lifecycle and its text/code modality are unchanged. This work
creates no Facts, semantic links, Hits, extraction serving readiness or ranking
boost from lineage.
