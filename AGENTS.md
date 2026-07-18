# PROJECT KNOWLEDGE BASE

**Project:** Anamnesis
**Language:** Rust (2024 edition, workspace, resolver = "3")
**Purpose:** Cognitive dynamics engine for LLMs — graph-structured knowledge with attraction, gravity, perception, and forgetting

> This file is deliberately limited to durable commands and invariants. API
> truth lives in rustdoc (`cargo doc --open`) and the `docs/` tree
> (`docs/00-foundation` … `docs/07-quality-gates`, `docs/adr/`). If a statement
> here ever disagrees with the code, the code is right — fix this file.

## OVERVIEW

Anamnesis models knowledge as a graph whose dynamics mimic cognitive processes:

- **Attraction**: similar/related nodes cluster (embedding similarity + co-occurrence)
- **Gravity**: important nodes (high centrality) attract new knowledge
- **Perception**: input gating (novelty, confidence, budget)
- **Forgetting**: ACT-R base-level decay — each node recomputes
  `B_i = ln(Σ_j (now − t_j)^(−d_j))` on demand from its bounded access-trace
  window (each trace stores its own decay `d_j`); `salience = logistic(B_i + P_i)`
  with `P_i` the stored evidence prior (ADR-0008)

## WORKSPACE LAYOUT

```
crates/
├── anamnesis/          # the engine library (package name: anamnesis-engine)
│   └── src/
│       ├── graph/      # Node, Edge, Graph, Origin, ScopePath, KnowledgeType, EdgeType
│       ├── mechanics/  # priors, attraction, perception, forgetting, topology, observability
│       ├── query/      # spreading activation (RWR), scoring, assembly, packaging, temporal
│       ├── storage/    # StorageAdapter trait + SqliteStorage (rusqlite bundled, FTS5)
│       ├── embedding/  # EmbeddingProvider trait + optional FastEmbedProvider (feature "embed")
│       ├── snapshot/   # clone-based SnapshotStore
│       ├── memory/     # Memory: higher-level facade over Engine
│       └── api/        # Engine (public API), api/search/ submodule
└── anamnesis-mcp/      # MCP server + CLI + daemon + hooks (binary crate)
    └── src/
        ├── memory/     # MemoryRegistry (namespace-aware wrapper over anamnesis::Memory)
        ├── dispatch/   # MCP request dispatch (daemon/embedded byte-parity contract)
        ├── extract/    # shadow extraction pipeline
        └── …           # hook, daemon, launcher, capture, proto (wire shapes), dashboard
```

## COMMANDS

```bash
cargo build                          # engine (default features, no FastEmbed)
cargo build --features embed         # with the optional FastEmbed provider
cargo test --workspace               # full suite
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo bench                          # benchmarks (benches/eval downloads datasets separately)
plugin/tests/verify_versions.sh X.Y.Z   # release lockstep check
plugin/tests/verify_release_tag.sh vX.Y.Z
```

Package names for `-p`: `anamnesis-engine`, `anamnesis-mcp`.

## CONVENTIONS

- **rusqlite (bundled SQLite) is the sole core dependency**; `feature = "embed"` adds optional FastEmbed
- **Trait-based storage** — `StorageAdapter`; `Engine<S: StorageAdapter + Clone>`
- **Pure mechanics** — scoring/decay functions are side-effect free
- **No async in core** — the engine is synchronous; async lives in anamnesis-mcp (tokio)
- **Origin on every node** — provenance (`peer_id`, `source_kind`, `session_id`, `scope`, `confidence`)
- **Fragments over summaries** — conversation turns are preserved as nodes; summaries are emergent
- **GraphEvents are observable** — mutations buffer events drained via `drain_events()`;
  their semantic order (dependencies before dependents, none on rollback) is a contract
- **Multi-process safety is layered** — the fs4 advisory lock lives in anamnesis-mcp;
  opening a live database file directly with `SqliteStorage::open` from a second
  process is NOT guarded (write-behind hot fields can clobber each other's `flush`)

## INVARIANTS (hard rules for any change)

1. **No panics on library paths that return `Result`** — no `unwrap`/`expect`/`panic!`
   in library code. Snapshot/clone paths go through `StorageAdapter::try_clone`
   (fallible), never `Clone::clone` directly; no `catch_unwind`.
2. **No swallowed errors** — no `let _ =` on fallible calls. Either propagate or
   handle with a named fallback that preserves the invariant (see
   `next_node_id`'s free-id consumption for the pattern).
3. **Write-behind means flush** — base rows are written eagerly in one
   `BEGIN IMMEDIATE`; later hot-field mutations are dirty-tracked and durable
   only after `flush()`. Multi-statement mutations are transactional.
4. **No `println!` in the engine library** — CLI output belongs to anamnesis-mcp.
5. **No global state** — all state lives in Graph/Engine instances.
6. **Public API is additive-only** — 0.x minor releases never remove or change
   existing public signatures (the Semver check job enforces this).
7. **No `pub(crate)` widening to make tests compile** — relocate the test or
   test through the public surface.

## BOUNDARY (what Anamnesis does NOT do)

- No LLM calls (embedding generation/extraction are consumer-side)
- No session management, no network/HTTP server (anamnesis-mcp is the MCP/CLI/daemon layer)
- No serialization-format opinions beyond the storage adapter
