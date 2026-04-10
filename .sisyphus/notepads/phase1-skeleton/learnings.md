# Phase 1 Skeleton — Learnings

## Clean-Slate Rebuild (Commit: 52c7471)

### What Was Done
- Deleted all .rs files in src/ except lib.rs (14 files removed)
- Deleted all .rs files in tests/ (3 files removed)
- Replaced src/lib.rs with minimal placeholder: `// Phase 1 skeleton — modules will be declared as they are built`
- Fixed Cargo.toml: changed crate-type from `["lib", "cdylib"]` to `["lib"]`
- Verified `cargo build` passes with 0 errors

### Files Deleted
**src/**
- src/api/mod.rs
- src/graph/edge.rs, node.rs, mod.rs
- src/mechanics/attraction.rs, forgetting.rs, gravity.rs, perception.rs, mod.rs
- src/query/mod.rs, spread.rs
- src/storage/memory.rs, mod.rs

**tests/**
- tests/graph_test.rs
- tests/integration_test.rs
- tests/mechanics_test.rs

### Key Decisions
1. **Minimal lib.rs**: Single-line comment as placeholder. No module declarations yet — they will be added as modules are built.
2. **Cargo.toml cleanup**: Removed `cdylib` crate-type. Core library is `lib` only; FFI/bindings can be added later if needed.
3. **Tests directory preserved**: Empty tests/ directory remains for future integration tests.

### Build Status
✅ `cargo build` passes cleanly. No compilation errors or warnings.

### Next Steps
- Phase 1 will rebuild modules incrementally: graph → mechanics → query → storage → api
- Each module will be added with tests as it's built
- Commit strategy: atomic commits per module or feature

## Core Type Primitives (Commit: d15f33f)

### What Was Done
- Created `src/graph/types.rs` with 3 newtypes (NodeId, EdgeId, Timestamp) and 2 enums (KnowledgeType, EdgeType)
- Created `src/error.rs` with Error enum (6 variants) + Display + std::error::Error impls
- Created `src/graph/mod.rs` with module declarations and re-exports
- Updated `src/lib.rs` to declare modules and re-export core types
- All 8 unit tests pass; `cargo clippy -- -D warnings` passes

### Type System Design
**Newtypes (Copy, Eq, Hash, Ord):**
- `NodeId(u64)` — unique node identifier
- `EdgeId(u64)` — unique edge identifier
- `Timestamp(u64)` — unix milliseconds; `Timestamp::now()` returns 0 in Phase 1

**KnowledgeType (12 variants):**
- Identity tier (3): IdentityCore, IdentityLearned, IdentityState
- Knowledge tier (6): Semantic, Procedural, Entity, Convention, Decision, Gotcha
- Memory tier (2): Episodic, Event
- Custom(String) for extensibility

**EdgeType (12 variants):**
- Supportive (10): Semantic, Causal, Temporal, Reason, ReinforcedBy, ConsolidatedFrom, ExtractedFrom, Entity, Supersedes, RejectedAlternative
- Inhibitory (1): Contradicts
- Custom(String) for extensibility

**Error (6 variants):**
- NodeNotFound(NodeId), EdgeNotFound(EdgeId)
- StorageError(String), Rejected(String), InvalidConfig(String)
- BudgetExhausted

### Key Decisions
1. **No methods on enums**: KnowledgeType and EdgeType are data-only. Decay rates, kappa values, mass priors live in doc comments only (not code).
2. **No Default impl**: Enums don't derive Default. Consumers must explicitly choose a type.
3. **No serde**: Serialization is consumer's responsibility.
4. **Docstrings for physics**: Each variant documents its role in the physics model (decay class, kappa value, mass prior).
5. **Unused import cleanup**: Removed `use std::fmt` from types.rs (not needed; Display not implemented on types).

### Build Status
✅ All 8 tests pass
✅ `cargo clippy -- -D warnings` passes
✅ Commit created: `feat(graph): define core type primitives`

### Next Steps
- Phase 2: Node and Edge structures (content, salience, timestamps, metadata)
- Phase 3: Graph CRUD operations
- Phase 4: Mechanics (attraction, gravity, perception, forgetting)
