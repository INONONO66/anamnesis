//! Storage abstraction for the Anamnesis graph engine.
//!
//! The `StorageAdapter` trait defines the interface for all storage backends.
//! `SqliteStorage` is the default implementation, using an in-memory SQLite
//! database with write-behind dirty tracking for hot fields and FTS5 text search.

pub mod sqlite;

pub use sqlite::SqliteStorage;

use crate::error::Error;
use crate::graph::{
    AccessTrace, Edge, EdgeId, KnowledgeType, Node, NodeId, PeerId, ScopePath, Timestamp,
};
use std::collections::{HashMap, VecDeque};

const ATOMIC_SOURCE_INCARNATION_PREFIX: &str = "anamnesis:source-incarnation:";
pub(crate) const NODE_INCARNATION_METADATA_KEY: &str = "anamnesis:node-incarnation";

pub(crate) fn atomic_source_incarnation_key(id: NodeId) -> String {
    format!("{ATOMIC_SOURCE_INCARNATION_PREFIX}{}", id.0)
}

pub(crate) fn is_atomic_source_incarnation_key(key: &str) -> bool {
    key.starts_with(ATOMIC_SOURCE_INCARNATION_PREFIX)
}

/// Fingerprint of the provenance and evidence carried by a raw source node.
///
/// `generation` is a backend-owned, durable allocation generation when the
/// adapter supports one. The default adapter implementation passes `None` and
/// therefore cannot distinguish byte-identical reuse of a numeric node ID.
pub(crate) fn atomic_source_incarnation(node: &Node, generation: Option<u64>) -> String {
    const OFFSET_ONE: u64 = 0xcbf2_9ce4_8422_2325;
    const OFFSET_TWO: u64 = 0x8422_2325_cbf2_9ce4;
    const PRIME_ONE: u64 = 0x0000_0100_0000_01b3;
    const PRIME_TWO: u64 = 0x0000_0100_0000_01e7;

    fn add_field(states: &mut [u64; 2], field: &[u8]) {
        let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
        for byte in length.to_le_bytes().iter().chain(field) {
            states[0] ^= u64::from(*byte);
            states[0] = states[0].wrapping_mul(PRIME_ONE);
            states[1] ^= u64::from(*byte);
            states[1] = states[1].wrapping_mul(PRIME_TWO);
        }
    }

    fn source_kind_label(kind: &crate::graph::SourceKind) -> &'static [u8] {
        use crate::graph::SourceKind;
        match kind {
            SourceKind::AgentObservation => b"agent-observation",
            SourceKind::HumanInput => b"human-input",
            SourceKind::DocumentExtract => b"document-extract",
            SourceKind::SystemEvent => b"system-event",
            SourceKind::Inferred => b"inferred",
            SourceKind::External => b"external",
        }
    }

    let mut states = [OFFSET_ONE, OFFSET_TWO];
    match generation {
        Some(value) => {
            add_field(&mut states, b"generation");
            add_field(&mut states, &value.to_le_bytes());
        }
        None => add_field(&mut states, b"adapter-unversioned"),
    }
    add_field(&mut states, &node.id.0.to_le_bytes());
    add_field(&mut states, node.name.as_bytes());
    match &node.summary {
        Some(summary) => {
            add_field(&mut states, b"summary-present");
            add_field(&mut states, summary.as_bytes());
        }
        None => add_field(&mut states, b"summary-absent"),
    }
    add_field(&mut states, node.content.as_bytes());
    add_field(&mut states, &node.created_at.0.to_le_bytes());
    add_field(&mut states, &node.updated_at.0.to_le_bytes());
    match node.valid_from {
        Some(timestamp) => {
            add_field(&mut states, b"valid-from");
            add_field(&mut states, &timestamp.0.to_le_bytes());
        }
        None => add_field(&mut states, b"no-valid-from"),
    }
    match node.valid_until {
        Some(timestamp) => {
            add_field(&mut states, b"valid-until");
            add_field(&mut states, &timestamp.0.to_le_bytes());
        }
        None => add_field(&mut states, b"no-valid-until"),
    }
    add_field(&mut states, &node.origin.peer_id.0.to_le_bytes());
    add_field(&mut states, source_kind_label(&node.origin.source_kind));
    add_field(&mut states, node.origin.session_id.as_bytes());
    add_field(&mut states, node.origin.scope.as_str().as_bytes());
    add_field(&mut states, &node.origin.confidence.to_bits().to_le_bytes());
    match &node.node_type {
        KnowledgeType::Identity => add_field(&mut states, b"identity"),
        KnowledgeType::Semantic => add_field(&mut states, b"semantic"),
        KnowledgeType::Episodic => add_field(&mut states, b"episodic"),
        KnowledgeType::Custom(label) => {
            add_field(&mut states, b"custom");
            add_field(&mut states, label.as_bytes());
        }
    }
    let mut tags = node
        .entity_tags
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    tags.sort_unstable();
    tags.dedup();
    for tag in tags {
        add_field(&mut states, tag.as_bytes());
    }
    let mut metadata = node
        .metadata
        .iter()
        .filter(|(key, _)| key.as_str() != NODE_INCARNATION_METADATA_KEY)
        .collect::<Vec<_>>();
    metadata.sort_by_key(|(key, _)| *key);
    for (key, value) in metadata {
        add_field(&mut states, key.as_bytes());
        add_field(&mut states, value.as_bytes());
    }
    format!("v2:{:016x}{:016x}", states[0], states[1])
}

/// Stable identifier in the isolated atomic-fact sidecar.
///
/// Atomic facts are retrieval representations, not graph nodes. Their IDs
/// therefore live in a separate namespace and never consume or collide with
/// [`NodeId`] allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AtomicFactId(pub u64);

/// One persisted record in the isolated atomic-fact sidecar.
///
/// The record deliberately stores only extracted text plus raw Episodic source
/// provenance. It is absent from graph topology, node budgets, attraction,
/// forgetting, and the normal node FTS/vector candidate pool.
#[derive(Debug, Clone, PartialEq)]
pub struct AtomicFact {
    /// Sidecar-local identifier.
    pub id: AtomicFactId,
    /// Standalone atomic claim used only to route back to raw evidence.
    pub content: String,
    /// Passage embedding in the same vector space as the owning [`Memory`](crate::Memory).
    pub embedding: Vec<f64>,
    /// Authoritative raw Episodic source nodes.
    pub source_node_ids: Vec<NodeId>,
    /// Selective entity tags. Broad speaker/session tags do not belong here.
    pub entity_tags: Vec<String>,
    /// Source conversation session retained for provenance validation.
    pub source_session_id: String,
    /// Visibility scope inherited from the cited sources.
    pub scope: ScopePath,
    /// Latest observation time among the cited sources.
    pub observed_at: Timestamp,
    /// Optional fact-validity start.
    pub valid_from: Option<Timestamp>,
    /// Optional fact-validity end.
    pub valid_until: Option<Timestamp>,
    /// Consumer metadata such as extractor version or stable external id.
    pub metadata: HashMap<String, String>,
}

/// Stable identifier for one reviewed relation between two atomic facts.
///
/// Relation IDs live in their own sidecar namespace and never consume graph
/// [`EdgeId`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AtomicFactRelationId(pub u64);

/// Semantic kind of a reviewed relation between two atomic facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AtomicFactRelationKind {
    /// The source fact provides a reason for the target fact.
    Reason,
    /// The source fact causally contributes to the target fact.
    Causal,
    /// The source fact supports the target fact.
    Supports,
    /// The source fact contradicts the target fact.
    Contradicts,
}

/// One reviewed, typed relation in the isolated atomic-fact sidecar.
///
/// These relations connect retrieval representations only. They do not enter
/// graph topology, spreading activation, attraction, or forgetting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AtomicFactRelation {
    /// Sidecar-local relation identifier.
    pub id: AtomicFactRelationId,
    /// Directed source endpoint.
    pub from_fact_id: AtomicFactId,
    /// Directed target endpoint.
    pub to_fact_id: AtomicFactId,
    /// Reviewed semantic relation type.
    pub kind: AtomicFactRelationKind,
    /// Stable identity of the reviewer or review process.
    pub reviewed_by: String,
    /// Review-policy or model profile applied to this decision.
    pub review_profile: String,
    /// Time at which the relation was reviewed.
    pub reviewed_at: Timestamp,
    /// Stable consumer key used to make repeated promotion idempotent.
    pub idempotency_key: String,
    /// Visibility scope for this relation.
    pub scope: ScopePath,
    /// Optional relation-validity start.
    pub valid_from: Option<Timestamp>,
    /// Optional relation-validity end.
    pub valid_until: Option<Timestamp>,
    /// Consumer metadata such as review version or provenance identifiers.
    pub metadata: HashMap<String, String>,
}

impl AtomicFactRelation {
    /// Reconstruct a reviewed relation for a storage adapter.
    ///
    /// This constructor exposes every required persisted field while defaulting
    /// optional validity and metadata to empty values. It does not perform the
    /// evidence, scope, or endpoint admission enforced by
    /// [`Memory::add_atomic_fact_relation`](crate::Memory::add_atomic_fact_relation);
    /// consumers admitting a new relation should use that higher-level API.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: AtomicFactRelationId,
        from_fact_id: AtomicFactId,
        to_fact_id: AtomicFactId,
        kind: AtomicFactRelationKind,
        reviewed_by: impl Into<String>,
        review_profile: impl Into<String>,
        reviewed_at: Timestamp,
        idempotency_key: impl Into<String>,
        scope: ScopePath,
    ) -> Self {
        Self {
            id,
            from_fact_id,
            to_fact_id,
            kind,
            reviewed_by: reviewed_by.into(),
            review_profile: review_profile.into(),
            reviewed_at,
            idempotency_key: idempotency_key.into(),
            scope,
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        }
    }

    /// Attach a half-open validity interval reconstructed by a storage adapter.
    pub fn with_validity(
        mut self,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Self {
        self.valid_from = valid_from;
        self.valid_until = valid_until;
        self
    }

    /// Attach persisted consumer metadata reconstructed by a storage adapter.
    pub fn with_metadata(mut self, metadata: HashMap<String, String>) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Storage backend interface for the Anamnesis graph engine.
///
/// Implementations must provide O(1) amortized node/edge access.
/// The `SqliteStorage` implementation uses an in-memory SQLite database
/// with cached graph objects and SoA hot fields for fast spreading activation.
///
/// # Node strength substrate
///
/// Persistent node strength is `A_i = B_i + P_i` (ADR-0008). The base level `B_i`
/// is the multi-trace ACT-R activation `ln(Σ_j (now − at_j)^(−d_j))` over the
/// node's bounded 32-trace `access_history`, where each trace carries its own
/// activation-dependent decay `d_j` (Pavlik & Anderson 2005); it is computed on
/// demand and is NOT a stored field, so the trait exposes no `B_i` setter. The
/// persistent substrate is the access-history window (a committed access appends a
/// now-stamped [`AccessTrace`] via
/// [`append_access_trace`](StorageAdapter::append_access_trace)) plus the
/// decay-EXEMPT evidence prior `P_i`
/// ([`get_evidence_prior`](StorageAdapter::get_evidence_prior) /
/// [`set_evidence_prior`](StorageAdapter::set_evidence_prior)). `retained_action`
/// and `salience` are CACHED projections of the composite, refreshed only by
/// commit/touch/tick; read-only access returns the cache unchanged.
///
/// # Decay checkpoint (obsolete)
///
/// `decay_checkpoint` is retained only for snapshot/back-compat (the `v2 -> v3`
/// migration introduced it). Under recompute-from-history it is no longer
/// load-bearing for memory strength: `B_i` ages every trace to `now` directly, so
/// no "as-of" baseline is needed. Engine maintenance no longer reads or advances it.
pub trait StorageAdapter: Send + Sync {
    // Isolated atomic-fact sidecar
    //
    // Default methods preserve source compatibility for third-party storage
    // adapters. Unsupported adapters simply expose an empty read lane and return
    // an explicit error if a consumer attempts to write an atomic fact.

    /// Allocate an ID in the isolated atomic-fact namespace.
    fn next_atomic_fact_id(&mut self) -> Result<AtomicFactId, Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic facts".to_string(),
        ))
    }

    /// Persist one complete atomic-fact record.
    fn set_atomic_fact(&mut self, _fact: AtomicFact) -> Result<(), Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic facts".to_string(),
        ))
    }

    /// Retrieve one atomic fact.
    fn get_atomic_fact(&self, _id: AtomicFactId) -> Result<&AtomicFact, Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic facts".to_string(),
        ))
    }

    /// Delete one atomic fact.
    fn delete_atomic_fact(&mut self, _id: AtomicFactId) -> Result<(), Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic facts".to_string(),
        ))
    }

    /// Return all live atomic-fact IDs in ascending order.
    fn all_atomic_fact_ids(&self) -> Vec<AtomicFactId> {
        Vec::new()
    }

    /// Return every live atomic-fact ID whose metadata contains an exact
    /// key-value pair.
    ///
    /// Results follow [`all_atomic_fact_ids`](Self::all_atomic_fact_ids)
    /// ordering. The default implementation keeps third-party adapters source
    /// compatible; adapters may override it with an indexed lookup.
    fn atomic_fact_ids_by_metadata(
        &self,
        key: &str,
        value: &str,
    ) -> Result<Vec<AtomicFactId>, Error> {
        let mut matches = Vec::new();
        for fact_id in self.all_atomic_fact_ids() {
            let fact = self.get_atomic_fact(fact_id)?;
            if fact
                .metadata
                .get(key)
                .is_some_and(|candidate| candidate == value)
            {
                matches.push(fact_id);
            }
        }
        Ok(matches)
    }

    /// Find the first live atomic fact whose metadata contains an exact
    /// key-value pair.
    ///
    /// Results follow [`all_atomic_fact_ids`](Self::all_atomic_fact_ids)
    /// ordering. The default implementation keeps third-party adapters source
    /// compatible; adapters may override it with an indexed lookup.
    fn atomic_fact_by_metadata(
        &self,
        key: &str,
        value: &str,
    ) -> Result<Option<&AtomicFact>, Error> {
        for fact_id in self.all_atomic_fact_ids() {
            let fact = self.get_atomic_fact(fact_id)?;
            if fact
                .metadata
                .get(key)
                .is_some_and(|candidate| candidate == value)
            {
                return Ok(Some(fact));
            }
        }
        Ok(None)
    }

    /// Return the current authority fingerprint for one raw source node.
    ///
    /// The default is content/provenance based. Adapters that can reuse a
    /// numeric [`NodeId`] must override this method and incorporate a durable,
    /// monotonically allocated generation so byte-identical replacements do
    /// not inherit facts attached to an earlier allocation.
    fn atomic_source_incarnation(&self, source: &Node) -> Result<String, Error> {
        Ok(crate::storage::atomic_source_incarnation(source, None))
    }

    /// Whether an atomic fact was bound to this exact source allocation and
    /// the source's current authority-bearing fields.
    ///
    /// Missing stamps are deliberately not inferred from a live node: legacy
    /// or partially written facts remain ineligible until they are reviewed
    /// and written again by the consumer.
    fn atomic_fact_source_is_current(
        &self,
        fact: &AtomicFact,
        source: &Node,
    ) -> Result<bool, Error> {
        let Some(expected) = fact.metadata.get(&atomic_source_incarnation_key(source.id)) else {
            return Ok(false);
        };
        Ok(expected == &self.atomic_source_incarnation(source)?)
    }

    // Reviewed atomic-fact relation sidecar. Defaults preserve source
    // compatibility for third-party adapters just as the atomic-fact methods do.

    /// Allocate an ID in the reviewed atomic-fact relation namespace.
    fn next_atomic_fact_relation_id(&mut self) -> Result<AtomicFactRelationId, Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic fact relations".to_string(),
        ))
    }

    /// Persist one complete reviewed atomic-fact relation.
    fn set_atomic_fact_relation(&mut self, _relation: AtomicFactRelation) -> Result<(), Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic fact relations".to_string(),
        ))
    }

    /// Retrieve one reviewed atomic-fact relation.
    fn get_atomic_fact_relation(
        &self,
        _id: AtomicFactRelationId,
    ) -> Result<&AtomicFactRelation, Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic fact relations".to_string(),
        ))
    }

    /// Delete one reviewed atomic-fact relation.
    fn delete_atomic_fact_relation(&mut self, _id: AtomicFactRelationId) -> Result<(), Error> {
        Err(Error::StorageError(
            "storage adapter does not support atomic fact relations".to_string(),
        ))
    }

    /// Return all live reviewed relation IDs in ascending order.
    fn all_atomic_fact_relation_ids(&self) -> Vec<AtomicFactRelationId> {
        Vec::new()
    }

    /// Return reviewed relation IDs whose directed source is `id`, in ascending
    /// relation-ID order.
    fn atomic_fact_relations_from(&self, _id: AtomicFactId) -> &[AtomicFactRelationId] {
        &[]
    }

    /// Return reviewed relation IDs whose directed target is `id`, in ascending
    /// relation-ID order.
    fn atomic_fact_relations_to(&self, _id: AtomicFactId) -> &[AtomicFactRelationId] {
        &[]
    }

    /// Find one reviewed relation by its stable idempotency key.
    ///
    /// The default preserves compatibility for external adapters. Backends
    /// with a keyed cache or index should override it.
    fn atomic_fact_relation_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<&AtomicFactRelation>, Error> {
        for relation_id in self.all_atomic_fact_relation_ids() {
            let relation = self.get_atomic_fact_relation(relation_id)?;
            if relation.idempotency_key == idempotency_key {
                return Ok(Some(relation));
            }
        }
        Ok(None)
    }

    // ID allocation
    /// Allocate the next available NodeId (reuses freed IDs when available).
    fn next_node_id(&mut self) -> NodeId;
    /// Allocate the next available EdgeId (reuses freed IDs when available).
    fn next_edge_id(&mut self) -> EdgeId;

    // Node CRUD
    /// Store a node. The node's id must have been allocated via next_node_id().
    fn set_node(&mut self, node: Node) -> Result<(), Error>;
    /// Retrieve a node by ID.
    fn get_node(&self, id: NodeId) -> Result<&Node, Error>;
    /// Retrieve a mutable reference to a node.
    ///
    /// # SoA Invariant
    /// Mutations to `salience`, `accessed_at`, or `node_type` through this reference
    /// will NOT be reflected in the SoA hot-field arrays. Use `set_salience()`,
    /// `set_accessed_at()` instead for those fields.
    ///
    /// # Index Invariant
    /// Mutations to `entity_tags`, `node_type`, `origin.agent_id`, or
    /// `origin.scope` will NOT update secondary indexes. To change these
    /// fields, call `set_node()` with the modified node so indexes are rebuilt.
    ///
    /// Safe to mutate: `name`, `summary`, `content`, `embedding`, `access_count`,
    /// `access_history`, `metadata`, `valid_from`, `valid_until`.
    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error>;
    /// Delete a node. Frees the ID for reuse. Caller must remove edges first.
    fn delete_node(&mut self, id: NodeId) -> Result<(), Error>;

    // Edge CRUD
    /// Store an edge. The edge's id must have been allocated via next_edge_id().
    fn set_edge(&mut self, edge: Edge) -> Result<(), Error>;
    /// Retrieve an edge by ID.
    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error>;
    /// Retrieve a mutable reference to an edge.
    ///
    /// # Adjacency Invariant
    /// Mutations to `source` or `target` through this reference will NOT update
    /// the adjacency index. Only mutate `weight`, `metadata`, or `edge_type`.
    /// To change source/target, delete the edge and create a new one.
    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error>;
    /// Delete an edge. Frees the ID for reuse. Updates adjacency index.
    fn delete_edge(&mut self, id: EdgeId) -> Result<(), Error>;

    // Adjacency (O(degree) — backed by adjacency index)
    /// Return all outgoing edge IDs from a node.
    fn edges_from(&self, id: NodeId) -> &[EdgeId];
    /// Return all incoming edge IDs to a node.
    fn edges_to(&self, id: NodeId) -> &[EdgeId];

    // Hot field access (SoA — cache-friendly for physics iteration)
    /// Get salience for a node. O(1) direct array access.
    fn get_salience(&self, id: NodeId) -> Result<f64, Error>;
    /// Set salience for a node. Keeps SoA in sync with Node.salience.
    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error>;
    /// Get accessed_at for a node. O(1) direct array access.
    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error>;
    /// Set accessed_at for a node. Keeps SoA in sync with Node.accessed_at.
    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
    /// Get the decay checkpoint timestamp for a node. O(1) direct array access.
    ///
    /// OBSOLETE under the base-level model: retained for snapshot/back-compat only
    /// (see the trait-level "Decay checkpoint (obsolete)" docs). It is no longer a
    /// load-bearing input to memory strength — `B_i` ages traces to `now` directly.
    fn get_decay_checkpoint(&self, id: NodeId) -> Result<Timestamp, Error>;
    /// Set the decay checkpoint timestamp for a node.
    ///
    /// OBSOLETE: kept for snapshot/back-compat parity. Engine maintenance no longer
    /// advances it.
    fn set_decay_checkpoint(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;

    // ── Base-level substrate: access-trace history (B_i) ──────────────────────
    //
    // B_i = ln(Σ_j (now − at_j)^(−d_j)) is computed on demand from these traces
    // ([`crate::mechanics::forgetting::compute_base_level`]); it is not a stored
    // scalar. Each [`AccessTrace`] carries its own activation-dependent decay `d_j`
    // (Pavlik & Anderson 2005). A committed access appends a now-stamped trace whose
    // `d_j` was computed from the existing history
    // ([`crate::mechanics::forgetting::compute_trace_decay`]), evicting the oldest
    // beyond the bounded 32-trace window, raising B_i.

    /// Get the node's bounded access-trace history (the substrate of `B_i`).
    fn get_access_history(&self, id: NodeId) -> Result<&VecDeque<AccessTrace>, Error>;

    /// Append an access trace, maintaining the bounded 32-trace window, and durably
    /// persist it. Called only from commit/touch (a committed access). The trace's
    /// `decay` must already be computed from the pre-append history
    /// ([`crate::mechanics::forgetting::compute_trace_decay`]).
    fn append_access_trace(&mut self, id: NodeId, trace: AccessTrace) -> Result<(), Error>;

    // ── Persistent reservoirs (decay-exempt evidence prior P_i, conductance) ──
    //
    // `P_i` (`evidence_prior`) is the persistent, decay-exempt log-odds offset
    // holding encoding surprise and explicit consumer feedback (ADR-0008 as
    // narrowed by ADR-0014). `conductance` `C_ij` is the edge associative reservoir; `weight`
    // is its bounded projection. The setters recompute the projection inside the
    // setter (the ADR "commit recomputes projections" step).

    /// Get the evidence prior `P_i` (decay-exempt log-odds offset) for a node.
    fn get_evidence_prior(&self, id: NodeId) -> Result<f64, Error>;

    /// Set the evidence prior `P_i` for a node. Called only from
    /// ingest/feedback/commit; the engine refreshes the `salience`/`retained_action`
    /// cache from `B_i(now) + P_i` afterwards.
    fn set_evidence_prior(&mut self, id: NodeId, prior: f64) -> Result<(), Error>;

    /// Get the cached composite retained action `A_i = B_i + P_i` for a node.
    ///
    /// This returns the CACHED snapshot last written by commit/touch/tick (it is not
    /// recomputed on read), so read-only query/search return a stable value.
    fn get_retained_action(&self, id: NodeId) -> Result<f64, Error>;

    /// Refresh the cached composite retained action `A_i` for a node and recompute
    /// the `salience` projection (`salience = project_salience(value)`). Called only
    /// from commit/touch/tick with the freshly recomputed `B_i(now) + P_i`.
    fn set_retained_action(&mut self, id: NodeId, value: f64) -> Result<(), Error>;

    /// Get the conductance `C_ij` (log-likelihood-ratio reservoir) for an edge.
    fn get_conductance(&self, id: EdgeId) -> Result<f64, Error>;

    /// Set the conductance `C_ij` for an edge and recompute the `weight`
    /// projection (`weight = project_weight(value)`). Called only from
    /// commit/tick.
    fn set_conductance(&mut self, id: EdgeId, value: f64) -> Result<(), Error>;

    /// Get the last-accessed timestamp for an edge. O(1) direct array access.
    fn get_edge_accessed_at(&self, id: EdgeId) -> Result<Timestamp, Error>;

    /// Set the last-accessed timestamp for an edge.
    ///
    /// A committed use is, by definition, not idle: implementations also reset
    /// the edge's `leaked_at` checkpoint to the same instant, clearing any
    /// outstanding idle-leak debt (the two fields stay distinct; this keeps
    /// them synchronized on every "use" event, per interactions.md).
    fn set_edge_accessed_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error>;

    /// Get the per-edge leak checkpoint — the last `now` idle-edge leakage was
    /// actually charged from (see [`Edge::leaked_at`]). O(1) direct array access.
    fn get_edge_leaked_at(&self, id: EdgeId) -> Result<Timestamp, Error>;

    /// Set the leak checkpoint for an edge. Called by `Engine::tick` after a
    /// successful leak, so a fixed idle window is charged once regardless of
    /// how many times `tick` runs at the same `now`.
    fn set_edge_leaked_at(&mut self, id: EdgeId, ts: Timestamp) -> Result<(), Error>;

    /// Fallible clone of the full storage state for snapshot paths.
    ///
    /// The default wraps `Clone::clone` (infallible). Backends whose clone
    /// performs I/O or locking should override to surface failures as `Err`
    /// instead of panicking. Engine snapshot/restore paths call this, never
    /// `Clone::clone` directly.
    fn try_clone(&self) -> Result<Self, Error>
    where
        Self: Sized + Clone,
    {
        Ok(self.clone())
    }

    /// Persist any buffered hot-field writes.
    ///
    /// Storage backends that write hot fields immediately can use this default no-op.
    /// Write-behind backends should override it and preserve dirty state on failure.
    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
    /// Get node type for a node. O(1) direct array access.
    fn get_node_type(&self, id: NodeId) -> Result<&KnowledgeType, Error>;

    // Counts and iteration
    /// Number of live nodes (excludes deleted slots).
    fn node_count(&self) -> usize;
    /// Number of live edges (excludes deleted slots).
    fn edge_count(&self) -> usize;
    /// All live node IDs.
    fn all_node_ids(&self) -> Vec<NodeId>;
    /// All live edge IDs.
    fn all_edge_ids(&self) -> Vec<EdgeId>;

    /// Return all node IDs that have the given entity tag.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_entity_tag(&self, tag: &str) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| {
                self.get_node(id)
                    .ok()
                    .is_some_and(|n| n.entity_tags.iter().any(|t| t == tag))
            })
            .collect()
    }

    /// Return all node IDs of the given knowledge type.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_type(&self, kt: &KnowledgeType) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| self.get_node_type(id).ok().is_some_and(|t| t == kt))
            .collect()
    }

    /// Return all node IDs created by the given peer.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_peer(&self, peer_id: PeerId) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| {
                self.get_node(id)
                    .ok()
                    .is_some_and(|n| n.origin.peer_id == peer_id)
            })
            .collect()
    }

    /// Return all node IDs whose origin scope equals the given scope path.
    ///
    /// Default implementation scans all nodes: O(N). Override for O(1) index lookup.
    fn nodes_by_scope(&self, scope: &ScopePath) -> Vec<NodeId> {
        self.all_node_ids()
            .into_iter()
            .filter(|&id| {
                self.get_node(id)
                    .ok()
                    .is_some_and(|n| n.origin.scope == *scope)
            })
            .collect()
    }

    /// Return all live node IDs sorted by ID descending (most recently allocated first).
    ///
    /// Default implementation sorts the result of all_node_ids(): O(N log N). Override for O(1).
    fn node_ids_descending(&self) -> Vec<NodeId> {
        let mut ids = self.all_node_ids();
        ids.sort_by_key(|a| std::cmp::Reverse(a.0));
        ids
    }

    /// Return up to `limit` live node IDs sorted by ID descending.
    ///
    /// Default delegates to `node_ids_descending()` + truncate.
    /// Override for O(limit) instead of O(N log N) when only a small
    /// prefix of the descending list is needed (e.g. ingest trigger pool).
    fn node_ids_descending_limit(&self, limit: usize) -> Vec<NodeId> {
        let mut ids = self.node_ids_descending();
        ids.truncate(limit);
        ids
    }

    /// Search nodes by text query (case-insensitive substring match on name and content).
    ///
    /// Returns up to `limit` node IDs with their match scores.
    /// Default implementation scans all nodes: O(N). Override for full-text search index.
    ///
    /// # Arguments
    /// * `query` - Search string (case-insensitive)
    /// * `limit` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of (NodeId, score) tuples. Score is 1.0 for default impl (simple match).
    fn text_search(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)> {
        let query_lower = query.to_lowercase();
        self.all_node_ids()
            .into_iter()
            .filter_map(|id| {
                self.get_node(id).ok().and_then(|node| {
                    let name_match = node.name.to_lowercase().contains(&query_lower);
                    let content_match = node.content.to_lowercase().contains(&query_lower);
                    if name_match || content_match {
                        Some((id, 1.0))
                    } else {
                        None
                    }
                })
            })
            .take(limit)
            .collect()
    }
}
