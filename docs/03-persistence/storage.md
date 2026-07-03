# Storage Design

Storage owns the durable graph state. The engine uses storage through a trait, and the default adapter is SQLite. Custom backends are valid if they satisfy the same contract.

## Storage Responsibilities

| Responsibility | Description |
|---|---|
| ID allocation | Allocate node and edge identifiers |
| CRUD | Store, fetch, mutate, and delete nodes and edges |
| adjacency | Return outgoing and incoming edge ids |
| hot fields | Read and write access history, evidence prior, conductance, accessed time, and type (`B_i` is computed from access history; retained action `A_i = B_i + P_i` is not stored as a scalar) |
| iteration | Enumerate all node and edge ids |
| search helpers | Provide type, scope, peer, entity, and text scans |
| flush | Commit pending writes for write-behind backends |

## Default Adapter

`SqliteStorage` is the default adapter. `Engine::new` starts with in-memory SQLite. File-backed adapters keep the same schema on disk. The core dependency is bundled SQLite; other backends are added by implementing the trait.

## Trait Contract

```rust
pub trait StorageAdapter: Send + Sync {
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
    // Each trace is a pair (Timestamp, per-trace decay rate d_j).
    // B_i = ln( sum_j (now - t_j)^(-d_j) ) is computed on demand from these
    // traces (elapsed time is floored to a minimum positive delta, 1 ms, so a
    // freshly stamped trace does not diverge); it is not a stored scalar. The per-trace decay rate is computed
    // ONCE at creation from the activation m_j of the traces that already exist
    // (d_j = m_type * ( c * e^{m_j} + α )) and then stored immutably with the
    // trace. A committed access:
    // 1. computes d_now from the current activation m_now of the existing traces;
    // 2. appends a (now-stamped, d_now) trace, evicting the oldest beyond the
    //    32-trace window.
    fn get_access_history(&self, id: NodeId) -> Result<&[(Timestamp, DecayRate)], Error>;
    fn append_access_trace(&mut self, id: NodeId, t: Timestamp, d: DecayRate) -> Result<(), Error>;
    // Persistent decay-exempt evidence prior P_i (encoding surprise, feedback /
    // social reinforcement, peer trust). It does not undergo base-level decay.
    fn get_evidence_prior(&self, id: NodeId) -> Result<f64, Error>;
    fn set_evidence_prior(&mut self, id: NodeId, prior: f64) -> Result<(), Error>;
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
| `nodes` | Site identity, content, type, origin, scope, time, projections |
| `edges` | Directed relationships, type, projections, validity |
| `node_hot` | Cache-friendly access history (bounded 32-trace window; each trace row stores its timestamp plus the per-trace decay rate `d_j`), evidence prior `P_i`, salience, access time; the retained-action base-level `B_i` is computed from access history, not stored |
| `edge_hot` | Cache-friendly conductance, weight, access time |
| `adjacency_from` | Outgoing edge index |
| `adjacency_to` | Incoming edge index |
| `fts_nodes` | Full-text search over name, summary, content, tags |

Hot fields may be folded into base tables for simple adapters, but implementations should make their update cost explicit.

## Indexes

| Index | Purpose |
|---|---|
| node type | Type-filtered query |
| entity tag | Candidate generation and reflection |
| scope | Visibility and ranking |
| peer / origin | Provenance introspection |
| valid interval | temporal validity filtering (search path) |
| salience projection | List and packaging |
| adjacency | Activation traversal |

## Snapshot

Snapshots store a clone of the storage state under a label and timestamp. Restore replaces the engine's graph storage with the cloned snapshot storage.

Snapshot is intentionally clone-based. This keeps the core simple and makes the cost visible. Backends may later provide copy-on-write or database-native snapshots behind the same API.

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

## Performance Targets

| Operation | Target |
|---|---|
| hot access-trace append / evidence-prior update | Avoid serializing the whole node object; the access-history window is bounded to 32 traces |
| hot conductance update | Avoid serializing the whole edge object |
| adjacency traversal | Cost proportional to degree |
| full scan | Allowed only for maintenance and benchmarks |
| text search | Return top results under a limit |
| snapshot | Make clone cost explicit |
