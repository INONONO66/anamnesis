# Storage Design

Storage owns the durable graph state. The engine uses storage through a trait, and the default adapter is SQLite. Custom backends are valid if they satisfy the same contract.

## Storage Responsibilities

| Responsibility | Description |
|---|---|
| ID allocation | Allocate node and edge identifiers |
| CRUD | Store, fetch, mutate, and delete nodes and edges |
| adjacency | Return outgoing and incoming edge ids |
| hot fields | Read and write access history, evidence prior, cached retained action `A_i = B_i + P_i`, its salience projection, conductance, accessed time, and type (`B_i` is recomputed from access history and is not stored independently; the cached `A_i` scalar is write-behind) |
| atomic sidecar | Persist source-bound routing facts, explicitly reviewed routing derivations, typed routing relations, and cited raw-source ids outside graph topology |
| iteration | Enumerate all node and edge ids |
| search helpers | Provide type, scope, peer, entity, and text scans |
| flush | Commit pending writes for write-behind backends |

## Default Adapter

`SqliteStorage` is the default adapter. `Engine::new` starts with in-memory SQLite. File-backed adapters keep the same schema on disk. The core dependency is bundled SQLite; other backends are added by implementing the trait.

## Trait Contract

The abridged shape below shows the current graph, hot-field, and atomic-sidecar
capabilities; rustdoc is authoritative for the complete trait.

```rust
pub trait StorageAdapter: Send + Sync {
    fn next_atomic_fact_id(&mut self) -> Result<AtomicFactId, Error>;
    fn set_atomic_fact(&mut self, fact: AtomicFact) -> Result<(), Error>;
    fn get_atomic_fact(&self, id: AtomicFactId) -> Result<&AtomicFact, Error>;
    fn delete_atomic_fact(&mut self, id: AtomicFactId) -> Result<(), Error>;
    fn all_atomic_fact_ids(&self) -> Vec<AtomicFactId>;
    fn atomic_fact_by_metadata(&self, key: &str, value: &str) -> Result<Option<&AtomicFact>, Error>;
    fn atomic_source_incarnation(&self, source: &Node) -> Result<String, Error>;
    fn atomic_fact_source_is_current(&self, fact: &AtomicFact, source: &Node) -> Result<bool, Error>;
    fn next_atomic_fact_relation_id(&mut self) -> Result<AtomicFactRelationId, Error>;
    fn set_atomic_fact_relation(&mut self, relation: AtomicFactRelation) -> Result<(), Error>;
    fn get_atomic_fact_relation(&self, id: AtomicFactRelationId) -> Result<&AtomicFactRelation, Error>;
    fn delete_atomic_fact_relation(&mut self, id: AtomicFactRelationId) -> Result<(), Error>;
    fn all_atomic_fact_relation_ids(&self) -> Vec<AtomicFactRelationId>;
    fn atomic_fact_relations_from(&self, id: AtomicFactId) -> &[AtomicFactRelationId];
    fn atomic_fact_relations_to(&self, id: AtomicFactId) -> &[AtomicFactRelationId];

    fn next_node_id(&mut self) -> NodeId;
    fn next_edge_id(&mut self) -> EdgeId;

    fn set_node(&mut self, node: Node) -> Result<(), Error>;
    fn get_node(&self, id: NodeId) -> Result<&Node, Error>;
    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error>;
    fn delete_node(&mut self, id: NodeId) -> Result<(), Error>;

    fn set_edge(&mut self, edge: Edge) -> Result<(), Error>;
    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error>;
    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error>;
    fn delete_edge(&mut self, id: EdgeId) -> Result<(), Error>;

    fn edges_from(&self, id: NodeId) -> &[EdgeId];
    fn edges_to(&self, id: NodeId) -> &[EdgeId];

    // Persistent substrate of base-level B_i: the bounded 32-trace access window.
    // Each AccessTrace stores its timestamp and per-trace decay rate d_j.
    // B_i = ln( sum_j (now - t_j)^(-d_j) ) is computed on demand from these
    // traces (elapsed time is floored to a minimum positive delta, 1 ms, so a
    // freshly stamped trace does not diverge); it is not a stored scalar. The per-trace decay rate is computed
    // ONCE at creation from the activation m_j of the traces that already exist
    // (d_j = m_type * ( c * e^{m_j} + α )) and then stored immutably with the
    // trace. A committed access:
    // 1. computes d_now from the current activation m_now of the existing traces;
    // 2. appends a (now-stamped, d_now) trace, evicting the oldest beyond the
    //    32-trace window.
    fn get_access_history(&self, id: NodeId) -> Result<&VecDeque<AccessTrace>, Error>;
    fn append_access_trace(&mut self, id: NodeId, trace: AccessTrace) -> Result<(), Error>;
    // Persistent decay-exempt evidence prior P_i (encoding surprise and
    // explicit feedback). It does not undergo base-level decay.
    fn get_evidence_prior(&self, id: NodeId) -> Result<f64, Error>;
    fn set_evidence_prior(&mut self, id: NodeId, prior: f64) -> Result<(), Error>;
    fn get_retained_action(&self, id: NodeId) -> Result<f64, Error>;
    fn set_retained_action(&mut self, id: NodeId, value: f64) -> Result<(), Error>;
    fn get_salience(&self, id: NodeId) -> Result<f64, Error>;
    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error>;
    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error>;
    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
    fn get_node_type(&self, id: NodeId) -> Result<&KnowledgeType, Error>;

    fn get_conductance(&self, id: EdgeId) -> Result<f64, Error>;
    fn set_conductance(&mut self, id: EdgeId, conductance: f64) -> Result<(), Error>;
    fn get_edge_accessed_at(&self, id: EdgeId) -> Result<Timestamp, Error>;
    fn set_edge_accessed_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error>;

    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn all_node_ids(&self) -> Vec<NodeId>;
    fn all_edge_ids(&self) -> Vec<EdgeId>;
}
```

`get_node_mut` and `get_edge_mut` are for metadata and non-hot fields. Hot-field updates use dedicated methods so maintenance and commit paths do not rewrite whole node or edge objects. The access-history and evidence-prior accessors are storage-contract methods, not public semantic operations; public behavior changes the persistent substrate through interactions (a committed access appends a trace; feedback and encoding surprise move `P_i`). The base-level term `B_i` is never a stored field, so storage exposes no `B_i` setter — it is recomputed from `access_history` whenever salience is projected.

## SQLite Schema Overview

| Table | Purpose |
|---|---|
| `nodes` | Site identity, content, type, origin, scope, time, validity, access count, access history (bounded 32-trace window; each trace stores its timestamp plus the per-trace decay rate `d_j`), evidence prior `P_i`, tier, and consumer metadata. SQLite also persists a reserved per-allocation node generation in the metadata column but removes it from the public `Node::metadata` map; the retained-action base-level `B_i` is computed from access history, not stored |
| `edges` | Directed relationships, type, conductance reservoir `C_ij`, weight projection, validity, access/leak checkpoints, source |
| `salience` | SoA hot field: per-node logistic projection (write-behind, committed by `flush`) |
| `retained_action` | SoA hot field: per-node cached composite `A_i = B_i + P_i` (write-behind) |
| `accessed_at` | SoA hot field: per-node last committed access (write-behind) |
| `decay_checkpoint` | SoA hot field: retained for snapshot/back-compat; no longer load-bearing under recompute-from-history (write-behind) |
| `atomic_facts` | Isolated source-bound routing facts and explicitly reviewed routing derivations, embeddings, cited raw source ids, source session/scope, validity, and metadata; never graph nodes or reader evidence by themselves |
| `atomic_fact_relations` | Reviewed typed adjacency between atomic facts, including reviewer/profile/time, idempotency, scope, validity, and audit metadata; never graph edges or reader evidence by itself |
| `entity_tags` | Tag rows used for node candidate generation |
| `free_ids` | Deleted node/edge ids available for reuse; consumed atomically with allocation |
| `graph_metadata` | Key-value store for embedding model, embedding-migration checkpoints (`embedding.migration.*`), durable sidecar ID high-water marks, and the monotonic node-incarnation high-water mark |
| `node_fts` | Full-text search over name, summary, content, tags |

Hot fields may be folded into base tables for simple adapters, but
implementations should make their update cost explicit. The removed peer
registry and peer-alias tables are not part of the current schema; producer ids
remain fields on `Origin`.

## Proposed: Evidence Catalog Extension (ADR-0015)

[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
proposes additive catalog storage beside the graph and current atomic-fact
sidecar. The following logical tables and indexes are not implemented:

| Logical table | Purpose |
|---|---|
| `catalog_source_revisions` | Stable source identity, immutable revision bytes/hash, origin, record time, and lifecycle lineage |
| `catalog_active_versions` | Mutable active-revision pointers and explicit retraction/deletion state |
| `catalog_entities` | Stable scope-neutral entity identity, canonical label, and lifecycle |
| `catalog_aliases` / `catalog_mentions` | Provenance-bearing names and exact source mentions, each with independent visibility |
| `catalog_facts` | Subject/predicate/object, modality, admission class, scope, and time |
| `catalog_fact_relations` | Typed directed fact adjacency with relation-specific evidence, admission, scope, time, and formation provenance |
| `catalog_evidence_refs` | Exact source-revision spans or media regions with hashes and validation receipts |
| `catalog_observations` | Immutable derivation versions, proposition-level support maps, and freshness state |
| `catalog_formation_runs` | Producer/profile identity, validation, review, and audit result |
| `catalog_generations` | Graph/catalog/index generation identities used for cache and snapshot consistency |

Names are logical contracts; a storage adapter may normalize them differently.
Under the proposal, immutable source revisions are canonical evidence; mutable
raw graph nodes remain compatibility views of their active revisions. Foreign
keys or equivalent adapter checks would prevent a catalog record from remaining
eligible after its cited revision is retracted or deleted.

| Proposed index | Purpose |
|---|---|
| catalog entity alias | Canonical entity lookup without a full tag scan |
| catalog subject/predicate/object | Selective fact routing |
| catalog lexical projection / embedding space | Bounded FTS/vector lookup over versioned fact projections without changing fact identity |
| catalog source | Invalidation and raw-evidence hydration |
| catalog scope/time/admission | Eligibility filtering before scoring |
| fact-relation adjacency | Bounded evidence-chain traversal |

## Indexes

| Index | Purpose |
|---|---|
| node type | Type-filtered query |
| entity tag | Candidate generation |
| scope | Applicability and ranking; not authorization |
| peer / origin | Provenance introspection |
| valid interval | temporal validity filtering (search path) |
| salience projection | List and packaging |
| adjacency | Activation traversal |
| atomic-fact session / scope | Schema-level session/scope lookup; current hot routing scans the live sidecar and validates eligibility in memory |
| atomic-fact metadata | In-memory exact key/value index for retry-safe promotion and other keyed control-plane lookups; duplicate matches resolve by ascending fact id |
| atomic-relation endpoints / idempotency | Directed sidecar traversal and retry-safe reviewed promotion |

## Snapshot

Snapshots store a clone of the storage state under a label and timestamp. Restore replaces the engine's graph storage with the cloned snapshot storage.

Snapshot is intentionally clone-based. This keeps the core simple and makes the cost visible. Backends may later provide copy-on-write or database-native snapshots behind the same API.

Under ADR-0015, snapshot, clone, restore, and rollback would cover source
revisions, catalog/admission/freshness state, active-version pointers,
graph/catalog/index generations, and graph projections together. A graph-only
snapshot could otherwise restore a state whose citations and eligibility no
longer agree.

## SnapshotStore

| Field | Meaning |
|---|---|
| snapshot id | Stable identifier returned by `snapshot` |
| label | Human-readable label |
| captured_at | Record time |
| storage clone | Restorable graph state |

## Error Policy

- `decay_checkpoint` is obsolete under recompute-from-history. Base-level `B_i` is computed directly from `access_history` by aging every trace to `now` using that trace's own stored per-trace decay rate `d_j`, so there is no scalar reservoir carrying an "as-of" timestamp that a checkpoint must guard. The earliest trace is the creation trace, so the access-history window is self-dating and no separate checkpoint is needed to keep `B_i` total. Adapters that still carry a `decay_checkpoint` column (e.g. from the `v2 -> v3` migration) may retain it for telemetry, but it is no longer load-bearing for memory strength; the persistent substrate is the access-history window (timestamp + `d_j` pairs) plus `P_i`.
- Missing nodes or edges return typed errors.
- Storage implementations do not leak backend-specific errors directly across the trait boundary.
- `flush` failures propagate to callers.
- Backends with partial-write risk provide transactions or write batches.
- Numeric invalidity (`NaN`, infinities where disallowed) is rejected at the engine boundary.
- Product admission through `Memory::add_atomic_fact` validates that cited
  sources exist as Episodic nodes in one session and scope, then binds each
  citation to an engine-owned source-incarnation value. It does not establish a
  review decision. `Memory::add_reviewed_derivation` additionally requires
  explicit review provenance and rejects a source that was not current,
  unretracted, and valid at the declared review time. The lower-level storage
  setter can restore legacy or fixture rows, but a row without a current
  binding remains ineligible.
  SQLite combines a monotonic, durable allocation generation with a fingerprint
  of authority-bearing source fields. Retrieval, retry validation, and relation
  admission require an exact current match, so neither byte-identical reuse of a
  deleted numeric node id nor an in-place evidence/provenance change inherits the
  prior citation. The generation high-water mark survives deletion, reopen, and
  storage clone. Its reserved metadata key is backend-owned and hidden from
  callers. A failed write does not mutate the sources.
- The `StorageAdapter` default incarnation method remains content/provenance
  based for source compatibility. An adapter that reuses node ids must override
  it with an equivalent durable per-allocation generation. An atomic fact with a
  missing or obsolete binding is retained for audit but is ineligible; opening a
  database never manufactures authority from whichever node currently occupies
  the cited id.
- Atomic-fact relation writes validate both endpoints and typed review fields.
  Deleting a fact removes its incident relation rows in the same transaction.
- Atomic-fact and relation ID high-water marks are persisted when allocated.
  Deletion and process restart therefore never reassign an ID that may still be
  referenced by an audit record.

## Proposed: Catalog Migration Contract

If ADR-0015 is accepted, catalog support must be additive. Existing node/edge
ids, raw content, origins, scope, validity, and access history would be
preserved. Existing atomic rows may be projected into canonical records only
when their source provenance validates. Relation-shaped metadata is never
promoted automatically because it does not carry the review and relation-level
provenance required for admission; an explicit reviewed write must create a
typed relation record.

Catalog writes would be transactional with their evidence references and
formation audit record. A source-revision write or live-source update,
retraction, deletion, or visibility change would commit atomically with all
dependent stale/revoked state, graph projection changes, index generations, and
mutation events. Dependencies would be emitted before dependents, and rollback
would emit no event. Open-time migration would remain transactional and
rollback safe.

Catalog support would use a separate additive extension trait, such as
`EvidenceCatalogStorage: StorageAdapter`. Existing `StorageAdapter`
implementations would gain no required methods and would continue to compile
unchanged; catalog-only APIs would be available only when the adapter implements
the extension trait. The current narrow atomic-relation methods are defaulted on
`StorageAdapter` so generic `Memory<S>` recall remains source-compatible; they
do not constitute the proposed catalog surface.

## Performance Targets

| Operation | Target |
|---|---|
| hot access-trace append / evidence-prior update | Avoid serializing the whole node object; the access-history window is bounded to 32 traces |
| hot conductance update | Avoid serializing the whole edge object |
| adjacency traversal | Cost proportional to degree |
| full scan | Allowed only for maintenance and benchmarks |
| text search | Return top results under a limit |
| atomic-fact lookup | Use the isolated sidecar and return only cited raw sources to the reader-facing lane |
| snapshot | Make clone cost explicit |

The proposed catalog adds separate targets: indexed catalog lookup without a
hot-path full scan, versioned lexical/vector fact lookup within a declared
embedding space, chain expansion bounded by declared depth and visited-record
count, and source hydration only after evidence selection. Those are ADR-0015
implementation promotion requirements, not current storage behavior.
