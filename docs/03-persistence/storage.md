# Storage Design

Storage owns the durable graph state. The engine uses storage through a trait, and the default adapter is SQLite. Custom backends are valid if they satisfy the same contract.

## Storage Responsibilities

| Responsibility | Description |
|---|---|
| ID allocation | Allocate node and edge identifiers |
| CRUD | Store, fetch, mutate, and delete nodes and edges |
| adjacency | Return outgoing and incoming edge ids |
| hot fields | Read and write retained action, conductance, accessed time, checkpoints, and type |
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

    fn get_retained_action(&self, id: NodeId) -> Result<f64, Error>;
    fn set_retained_action(&mut self, id: NodeId, action: f64) -> Result<(), Error>;
    fn get_salience(&self, id: NodeId) -> Result<f64, Error>;
    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error>;
    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error>;
    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
    fn get_decay_checkpoint(&self, id: NodeId) -> Result<Timestamp, Error>;
    fn set_decay_checkpoint(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
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

`get_node_mut` and `get_edge_mut` are for metadata and non-hot fields. Hot-field updates use dedicated methods so maintenance and commit paths do not rewrite whole node or edge objects. Reservoir setters are storage-contract methods, not public semantic operations; public behavior changes physical quantities through interactions.

## SQLite Schema Overview

| Table | Purpose |
|---|---|
| `nodes` | Site identity, content, type, origin, scope, time, projections |
| `edges` | Directed relationships, type, projections, validity |
| `node_hot` | Cache-friendly retained action, salience, access time |
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
| valid interval | `fact_at` filtering |
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

- Every persisted node has a `decay_checkpoint`. It is initialized to the node's `created_at` on first commit, and the `v2 -> v3` migration backfills legacy nodes to `created_at` (or the later `accessed_at`) so no checkpoint precedes the node's own history. `get_decay_checkpoint` is therefore total for existing nodes and never fabricates a default; it errors only with `NodeNotFound`.
- Missing nodes or edges return typed errors.
- Storage implementations do not leak backend-specific errors directly across the trait boundary.
- `flush` failures propagate to callers.
- Backends with partial-write risk provide transactions or write batches.
- Numeric invalidity (`NaN`, infinities where disallowed) is rejected at the engine boundary.

## Performance Targets

| Operation | Target |
|---|---|
| hot retained-action update | Avoid serializing the whole node object |
| hot conductance update | Avoid serializing the whole edge object |
| adjacency traversal | Cost proportional to degree |
| full scan | Allowed only for maintenance and benchmarks |
| text search | Return top results under a limit |
| snapshot | Make clone cost explicit |
