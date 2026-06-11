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
//! use anamnesis::{Memory, Timestamp};
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
//! representation, custom packaging policy, peer/trust control, or the debug
//! lifecycle — then drop to **`Engine`** (the kernel API). `Memory` is built
//! entirely on `Engine`'s public API: anything it does, you can do.
//!
//! ## Kernel API — `Engine`
//!
//! [`Engine`] is the raw substrate: spreading activation, conductance,
//! dissipation, frustration, identity, and debug lifecycle. Retrieval quality
//! depends on your encoding choices — the validated recipe is [`Memory`].
//! See [`docs/`](https://github.com/INONONO66/anamnesis/tree/main/docs) for the
//! full technical specification.

pub mod api;
pub mod embedding;
pub mod error;
pub mod graph;
pub mod mechanics;
pub mod memory;
pub mod peer;
pub mod query;
pub mod snapshot;
pub mod storage;

// Framework API — the validated consumer layer.
pub use memory::AddReceipt;
pub use memory::Hit;
pub use memory::Memory;
pub use memory::Recall;
pub use memory::SearchTuning;

// Core re-exports
pub use api::{
    CrystallizeRequest, CrystallizeResult, DebugOutcome, Engine, EngineConfig, EvidenceResult,
    IngestResult, Observation, ObservedRef, PerspectiveKey, ReflectReport, SessionSummary,
    TickReport,
};
pub use embedding::EmbeddingProvider;
#[cfg(feature = "embed")]
pub use embedding::fastembed::FastEmbedProvider;
pub use error::Error;
pub use graph::{Edge, Node, Origin};
pub use graph::{EdgeId, EdgeType, KnowledgeType, NodeId, PeerId, Timestamp};
pub use mechanics::energy::{
    EnergyTerms, SiteBond, SiteEnergy, dirichlet_energy, energy as readout_energy,
};
pub use mechanics::health::GraphHealth;
pub use mechanics::observability::{
    InvariantCheck, InvariantReport, InvariantResult, OperationalWarning,
};
pub use mechanics::social::{ConfidenceLevel, FeedbackSignal};
pub use peer::{PeerProfile, PeerRegistry, SourceKind, TrustLevel};
pub use query::{
    AccessedSite, ActivatedTension, CoReadoutPair, CommitTrace, ContextPackage, Fragment,
    PackagingMode, PathUsedEdge, Query, QueryConfig, SearchInput, SearchResult, SearchTrace,
    Tension, TokenBudget,
};
pub use snapshot::{SnapshotBackend, SnapshotEntry, SnapshotId, SnapshotStore};
pub use storage::{SqliteStorage, StorageAdapter};
