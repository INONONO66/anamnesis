//! Anamnesis — cognitive graph engine for LLM agents.
//!
//! Knowledge with spreading activation, conductance, perception, and forgetting.
//!
//! # Two Doors
//!
//! Anamnesis exposes two complementary API surfaces:
//!
//! | Surface | Type | When to use |
//! |:--------|:-----|:------------|
//! | **Framework API** | [`Memory`] | Default. Bench-proven ingest recipe out of the box. |
//! | **Kernel API** | [`Engine`] | Custom node/edge types, encoding strategy, or lifecycle control. |
//!
//! ## Framework API — `Memory` (front door)
//!
//! [`Memory`] ships the encoding recipe validated by the LoCoMo and LongMemEval
//! benchmarks: speaker-prefixed episodic turns, ±1-window semantic views,
//! `ExtractedFrom`/`Temporal` edges, session/speaker entity tags, and
//! ingest-everything engine config. Those benchmark numbers are what you get
//! out of the box.
//!
//! ```rust,no_run
//! # #[cfg(feature = "embed")]
//! # fn main() -> Result<(), anamnesis::Error> {
//! use anamnesis::{Memory, Engine};
//! use anamnesis::engine::Timestamp;
//!
//! // 1. Open a persistent Memory (requires feature = "embed")
//! let mut mem = Memory::open("my-memory.db")?;
//!
//! // 2. Add conversational turns
//! let now = Timestamp::now();
//! mem.add("session-1", "Alice", "I prefer dark mode", now)?;
//! mem.add("session-1", "Bob",   "Got it, dark mode it is", now)?;
//!
//! // 3. Search (auto-flushes pending buffers)
//! let recall = mem.search("display preferences", 5)?;
//! for hit in &recall.hits {
//!     println!("{:.3}  {}", hit.score, hit.text);
//! }
//!
//! // 4. Reinforce what was actually used (commit-gated)
//! mem.used(recall)?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "embed"))]
//! # fn main() {}
//! ```
//!
//! **Use `Memory`** unless you need custom node/edge types, your own ingest
//! representation, or custom packaging policy — then drop to **`Engine`** (the
//! kernel API). `Memory` is built
//! entirely on `Engine`'s public API: anything it does, you can do.
//!
//! ## Kernel API — `Engine`
//!
//! [`Engine`] is the raw substrate: spreading activation, conductance,
//! dissipation, frustration, identity, and debug lifecycle. Retrieval quality
//! depends on your encoding choices — the validated recipe is [`Memory`].
//! See [`docs/`](https://github.com/INONONO66/anamnesis/tree/main/docs) for the
//! full technical specification.
//!
//! ## Namespaces
//!
//! | Namespace | Purpose |
//! |:----------|:--------|
//! | [`anamnesis::memory`](crate::memory) | Framework API — `Memory`, `Hit`, `Recall`, `SearchTuning`, `AddReceipt` |
//! | [`anamnesis::engine`](crate::engine) | Kernel API — `Engine`, `EngineConfig`, graph types, storage, embeddings |
//!
//! ## Public API contract
//!
//! The documented public API consists of exactly three root symbols and two
//! namespaces:
//!
//! - **Root**: [`Memory`], [`Engine`], [`Error`]
//! - **Framework**: [`anamnesis::memory`](crate::memory) — `Memory`, `Hit`, `Recall`, `SearchTuning`, `AddReceipt`
//! - **Kernel**: [`anamnesis::engine`](crate::engine) — `Engine`, `EngineConfig`, graph types, query types,
//!   observability, storage, and embeddings
//!
//! The implementation modules (`api`, `graph`, `query`, `mechanics`, `snapshot`,
//! `storage`, `embedding`, `error`) are the crate's internal structure. They are
//! `pub` so the two namespaces above can re-export from them, but are hidden from
//! documentation: build against `anamnesis::engine::*` / `anamnesis::memory::*`.

// Internal implementation modules — the two documented namespaces (`engine`,
// `memory`) re-export from these. Hidden from docs; not part of the documented
// surface.
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod embedding;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod graph;
#[doc(hidden)]
pub mod mechanics;
#[doc(hidden)]
pub mod query;
#[doc(hidden)]
pub mod snapshot;
#[doc(hidden)]
pub mod storage;

/// Kernel API — full engine surface in one namespace.
pub mod engine;
/// Framework API — bench-proven ingest recipe with two-door entry.
pub mod memory;

// Root re-exports — exactly three symbols.
pub use api::Engine;
pub use error::Error;
pub use memory::Memory;
