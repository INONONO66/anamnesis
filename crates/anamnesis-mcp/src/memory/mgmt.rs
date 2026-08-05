//! Single-node management primitives: `update`/`forget`/`supersede`/`list`/
//! `get`/`remember`/`relate`, plus their wire-format parsers (`parse_relation`/
//! `build_note_options`/`parse_metadata_filter`). Split out of `memory.rs`
//! verbatim (behavior-preserving move only) — every function body below is
//! byte-identical to its prior form in the single-file module.

use std::collections::HashMap;

use anamnesis::graph::{NodeId, Origin, PeerId, ScopePath, SourceKind, Timestamp};
use anamnesis::memory::{
    ListFilter, MemoryView, NoteOptions, Relation, SourceFragmentInput, Subgraph,
};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};
use sha2::{Digest, Sha256};

use super::MemoryRegistry;

/// Map an agent-facing relation label to the curated [`Relation`] vocabulary.
///
/// Accepts the canonical names (case-insensitive, `-`/`_`/space-insensitive).
/// An unrecognized label is **not** silently coerced to `Custom`: it returns a
/// clear error listing the accepted relations, so an agent typo surfaces instead
/// of quietly authoring a `Custom("typo")` edge. Use an explicit `custom:<label>`
/// prefix to author a consumer-defined relation on purpose.
pub fn parse_relation(label: &str) -> Result<Relation, Error> {
    let norm = label.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    if let Some(custom) = norm.strip_prefix("custom:") {
        let custom = custom.trim();
        if custom.is_empty() {
            return Err(Error::InvalidInput(
                "relation \"custom:\" requires a non-empty label (e.g. \"custom:blocks\")"
                    .to_string(),
            ));
        }
        // Preserve the caller's original (untrimmed-of-case) custom label after
        // the prefix, rather than the normalized form, so labels are faithful.
        let original = label.trim();
        let original_custom = original[original.find(':').map(|i| i + 1).unwrap_or(0)..].trim();
        return Ok(Relation::Custom(original_custom.to_string()));
    }
    let relation = match norm.as_str() {
        "causes" | "causal" => Relation::Causes,
        "contradicts" => Relation::Contradicts,
        "supports" => Relation::Supports,
        "refutes" => Relation::Refutes,
        "reason" => Relation::Reason,
        "rejected-alternative" | "rejectedalternative" => Relation::RejectedAlternative,
        "belongs-to" | "belongsto" => Relation::BelongsTo,
        "related" | "semantic" => Relation::Related,
        "supersedes" | "supersede" => Relation::Supersedes,
        _ => {
            return Err(Error::InvalidInput(format!(
                "unknown relation {label:?}; expected one of: causes, contradicts, supports, \
                 refutes, reason, rejected-alternative, belongs-to, related, supersedes (or \
                 \"custom:<label>\")"
            )));
        }
    };
    Ok(relation)
}

/// Build [`NoteOptions`] for `remember` from the wire-level tags/metadata/scope.
///
/// Empty-string tags are dropped rather than stored (adversarial input, not a
/// caller error). An invalid `scope` string (e.g. empty) surfaces
/// [`ScopePath::new`]'s error so `dispatch` can map it to `invalid_params`.
pub(crate) fn build_note_options(
    tags: Option<Vec<String>>,
    metadata: Option<HashMap<String, String>>,
    scope: Option<String>,
) -> Result<NoteOptions, Error> {
    let tags = tags
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    let metadata = metadata.unwrap_or_default().into_iter().collect();
    let scope = scope.map(ScopePath::new).transpose()?;
    Ok(NoteOptions {
        scope,
        tags,
        metadata,
    })
}

/// Consumer-materialized attachment transcript and its explicit provenance.
///
/// This is an MCP-layer input, not a processor configuration. Constructing it
/// performs no file, network, embedding, OCR, or vision operation.
pub(crate) struct AttachmentTranscriptInput {
    pub(crate) transcript: String,
    pub(crate) session: String,
    pub(crate) attachment_id: String,
    pub(crate) attachment_sha256: String,
    pub(crate) processor_provider: String,
    pub(crate) processor_model: String,
    pub(crate) processor_profile: String,
    pub(crate) processor_schema: String,
    pub(crate) confidence: f64,
    pub(crate) observed_at: Timestamp,
    pub(crate) scope: Option<String>,
    pub(crate) tags: Option<Vec<String>>,
}

const META_ATTACHMENT_ADMISSION_SHA256: &str = "attachment:admission-sha256";

fn canonical_provenance_value(label: &str, value: String) -> Result<String, Error> {
    if value.trim().is_empty() {
        return Err(Error::InvalidInput(format!(
            "attachment transcript {label} must not be empty"
        )));
    }
    if value.trim() != value {
        return Err(Error::InvalidInput(format!(
            "attachment transcript {label} must not have surrounding whitespace"
        )));
    }
    Ok(value)
}

fn update_length_prefixed(digest: &mut Sha256, label: &str, value: &str) -> Result<(), Error> {
    let length = u64::try_from(value.len()).map_err(|_| {
        Error::InvalidInput(format!(
            "attachment transcript {label} is too large to fingerprint"
        ))
    })?;
    digest.update(length.to_be_bytes());
    digest.update(value.as_bytes());
    Ok(())
}

/// Stable retry identity for the exact producer output within one namespace.
/// Length prefixes make adjacent fields unambiguous without relying on JSON or
/// platform-specific serialization details.
fn attachment_admission_sha256(fields: &[(&str, &str)]) -> Result<String, Error> {
    let mut digest = Sha256::new();
    digest.update(b"anamnesis:attachment-transcript-admission:v1");
    for &(label, value) in fields {
        update_length_prefixed(&mut digest, label, value)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Validate the MCP-layer contract and map it to the engine's generic raw
/// source admission type.
pub(crate) fn build_attachment_source_fragment(
    input: AttachmentTranscriptInput,
) -> Result<SourceFragmentInput, Error> {
    let session = canonical_provenance_value("session", input.session)?;
    let attachment_id = canonical_provenance_value("attachment_id", input.attachment_id)?;
    let attachment_sha256 =
        canonical_provenance_value("attachment_sha256", input.attachment_sha256)?;
    if attachment_sha256.len() != 64
        || !attachment_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::InvalidInput(
            "attachment transcript attachment_sha256 must be a 64-digit hexadecimal SHA-256"
                .to_owned(),
        ));
    }
    let processor_provider =
        canonical_provenance_value("processor_provider", input.processor_provider)?;
    let processor_model = canonical_provenance_value("processor_model", input.processor_model)?;
    let processor_profile =
        canonical_provenance_value("processor_profile", input.processor_profile)?;
    let processor_schema = canonical_provenance_value("processor_schema", input.processor_schema)?;
    let attachment_sha256 = attachment_sha256.to_ascii_lowercase();
    let admission_sha256 = attachment_admission_sha256(&[
        ("session", &session),
        ("attachment_id", &attachment_id),
        ("attachment_sha256", &attachment_sha256),
        ("processor_provider", &processor_provider),
        ("processor_model", &processor_model),
        ("processor_profile", &processor_profile),
        ("processor_schema", &processor_schema),
        ("transcript", input.transcript.as_str()),
    ])?;
    let scope = input.scope.map(ScopePath::new).transpose()?;
    let origin = Origin {
        peer_id: PeerId(0),
        source_kind: SourceKind::DocumentExtract,
        session_id: session,
        scope: scope.unwrap_or_else(ScopePath::universal),
        confidence: input.confidence,
    };
    let tags = input.tags.unwrap_or_default();

    Ok(
        SourceFragmentInput::new(input.transcript, origin, input.observed_at)
            .with_entity_tags(tags)
            .with_metadata(vec![
                ("attachment:id".to_owned(), attachment_id),
                ("attachment:sha256".to_owned(), attachment_sha256),
                (
                    META_ATTACHMENT_ADMISSION_SHA256.to_owned(),
                    admission_sha256,
                ),
                ("processor:provider".to_owned(), processor_provider),
                ("processor:model".to_owned(), processor_model),
                ("processor:profile".to_owned(), processor_profile),
                ("processor:schema".to_owned(), processor_schema),
            ]),
    )
}

/// Parse a `list` metadata filter's `"key=value"` wire format into a
/// `(key, value)` pair. Splits on the first `=`; a missing `=` or an empty
/// key is a caller error.
pub(crate) fn parse_metadata_filter(raw: &str) -> Result<(String, String), Error> {
    let (key, value) = raw.split_once('=').ok_or_else(|| {
        Error::InvalidInput(format!(
            "malformed metadata filter {raw:?}; expected \"key=value\""
        ))
    })?;
    if key.is_empty() {
        return Err(Error::InvalidInput(format!(
            "malformed metadata filter {raw:?}; key must not be empty"
        )));
    }
    Ok((key.to_string(), value.to_string()))
}

impl MemoryRegistry {
    /// Author a typed reasoning-chain edge between two existing nodes.
    ///
    /// `relation` is parsed via [`parse_relation`] (unknown labels error clearly).
    /// The node ids typically come from a prior `recall`. Returns the new edge id.
    pub fn relate(
        &mut self,
        from_id: u64,
        to_id: u64,
        relation: &str,
        ns: Option<&str>,
    ) -> Result<u64, Error> {
        self.ops.relates += 1;
        let relation = parse_relation(relation)?;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_relate(&mut mem, from_id, to_id, relation)
    }

    /// Store one distilled insight (`add_note`). Returns the episodic node id.
    pub fn remember(&mut self, text: &str, ns: Option<&str>) -> Result<u64, Error> {
        self.ops.remembers += 1;
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_remember(&mut mem, text)
    }

    /// Replace a node's content and re-embed it. `pub(crate)` [`mem_update`]
    /// does the namespace-locked work; `crate::dispatch` calls it directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn update(&mut self, id: u64, new_content: &str, ns: Option<&str>) -> Result<(), Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_update(&mut mem, NodeId(id), new_content)
    }

    /// Soft- (`hard = false`) or hard-delete (`hard = true`, irreversible) a node.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn forget(
        &mut self,
        id: u64,
        reason: &str,
        hard: bool,
        ns: Option<&str>,
    ) -> Result<(), Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        if hard {
            mem_forget_hard(&mut mem, NodeId(id))
        } else {
            mem_forget(&mut mem, NodeId(id), reason)
        }
    }

    /// Mark `new_id` as superseding `old_id`. Returns the new edge id.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn supersede(&mut self, new_id: u64, old_id: u64, ns: Option<&str>) -> Result<u64, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_supersede(&mut mem, NodeId(new_id), NodeId(old_id))
    }

    /// List nodes matching `filter`, ordered by salience (highest first).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn list(
        &mut self,
        filter: &ListFilter,
        ns: Option<&str>,
    ) -> Result<Vec<MemoryView>, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_list(&mut mem, filter)
    }

    /// Read a single node as a [`MemoryView`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get(&mut self, id: u64, ns: Option<&str>) -> Result<MemoryView, Error> {
        let handle = self.namespace_handle(ns)?;
        let mut mem = handle.lock().unwrap_or_else(|p| p.into_inner());
        mem_get(&mut mem, NodeId(id))
    }
}

// ── Namespace-locked primitives (phase-2 work) ───────────────────────────────
//
// Each function below operates on an already-resolved `&mut Memory` — no
// registry access, no global lock. `crate::dispatch` calls these directly
// between acquiring and releasing a namespace's `Mutex`, and the
// `MemoryRegistry` convenience methods above call the SAME functions after
// locking their own resolved handle, so the two call paths can never diverge.

/// Namespace-locked body of [`MemoryRegistry::relate`].
pub(crate) fn mem_relate(
    mem: &mut Memory<SqliteStorage>,
    from_id: u64,
    to_id: u64,
    relation: Relation,
) -> Result<u64, Error> {
    // Flush so a just-added turn (its semantic still buffered) is a valid
    // endpoint, mirroring `search`.
    mem.flush_all()?;
    let edge = mem.relate(NodeId(from_id), NodeId(to_id), relation)?;
    mem.flush_all()?;
    Ok(edge.0)
}

/// Namespace-locked body of [`MemoryRegistry::remember`].
pub(crate) fn mem_remember(mem: &mut Memory<SqliteStorage>, text: &str) -> Result<u64, Error> {
    mem_remember_with(mem, text, NoteOptions::default())
}

/// Like [`mem_remember`], with scope/tags/metadata routed through
/// [`Memory::add_note_with`].
pub(crate) fn mem_remember_with(
    mem: &mut Memory<SqliteStorage>,
    text: &str,
    opts: NoteOptions,
) -> Result<u64, Error> {
    let receipt = mem.add_note_with(text, Timestamp::now(), opts)?;
    mem.flush_all()?;
    Ok(receipt.episodic.0)
}

/// Namespace-locked admission of one consumer-materialized source fragment.
///
/// Exact retries reuse a live Episodic source carrying the same stable
/// admission fingerprint. A retracted or superseded source is not reused.
/// No embedding is supplied, so this path cannot invoke the configured
/// embedding provider. The exact transcript remains available to text search.
pub(crate) fn mem_add_source_fragment(
    mem: &mut Memory<SqliteStorage>,
    input: SourceFragmentInput,
) -> Result<u64, Error> {
    let admission_sha256 = input
        .metadata
        .iter()
        .find_map(|(key, value)| (key == META_ATTACHMENT_ADMISSION_SHA256).then_some(value.clone()))
        .ok_or_else(|| {
            Error::InvalidInput(
                "attachment transcript is missing its admission fingerprint".to_owned(),
            )
        })?;
    let existing = mem.list(&ListFilter {
        min_salience: 0.0,
        limit: usize::MAX,
        node_type: Some(anamnesis::graph::KnowledgeType::Episodic),
        tag: None,
        scope: None,
        metadata: Some((
            META_ATTACHMENT_ADMISSION_SHA256.to_owned(),
            admission_sha256,
        )),
    })?;
    if let Some(source) = existing
        .into_iter()
        .find(|source| !source.retracted && source.valid_until.is_none())
    {
        return Ok(source.node_id.0);
    }

    let node_id = mem.add_source_fragment(input)?;
    mem.flush_all()?;
    Ok(node_id.0)
}

/// Namespace-locked body of [`MemoryRegistry::update`].
pub(crate) fn mem_update(
    mem: &mut Memory<SqliteStorage>,
    id: NodeId,
    new_content: &str,
) -> Result<(), Error> {
    // Flush so a just-added node (its semantic still buffered) is a valid
    // target, mirroring `mem_relate`.
    mem.flush_all()?;
    mem.update_content(id, new_content, Timestamp::now())?;
    mem.flush_all()?;
    Ok(())
}

/// Namespace-locked body of [`MemoryRegistry::get`].
pub(crate) fn mem_get(mem: &mut Memory<SqliteStorage>, id: NodeId) -> Result<MemoryView, Error> {
    mem.flush_all()?;
    mem.get(id)
}

/// Namespace-locked body of [`MemoryRegistry::list`].
pub(crate) fn mem_list(
    mem: &mut Memory<SqliteStorage>,
    filter: &ListFilter,
) -> Result<Vec<MemoryView>, Error> {
    mem.flush_all()?;
    mem.list(filter)
}

/// Namespace-locked soft-delete body of [`MemoryRegistry::forget`].
pub(crate) fn mem_forget(
    mem: &mut Memory<SqliteStorage>,
    id: NodeId,
    reason: &str,
) -> Result<(), Error> {
    mem.flush_all()?;
    mem.forget(id, reason, Timestamp::now())?;
    mem.flush_all()?;
    Ok(())
}

/// Namespace-locked hard-delete body of [`MemoryRegistry::forget`] (`hard = true`).
pub(crate) fn mem_forget_hard(mem: &mut Memory<SqliteStorage>, id: NodeId) -> Result<(), Error> {
    mem.flush_all()?;
    mem.delete_hard(id)?;
    mem.flush_all()?;
    Ok(())
}

/// Namespace-locked body of [`MemoryRegistry::supersede`]. Returns the new
/// `Supersedes` edge id.
pub(crate) fn mem_supersede(
    mem: &mut Memory<SqliteStorage>,
    new_id: NodeId,
    old_id: NodeId,
) -> Result<u64, Error> {
    mem.flush_all()?;
    let edge = mem.supersede(new_id, old_id)?;
    mem.flush_all()?;
    Ok(edge.0)
}

/// Namespace-locked body of `dispatch`'s `Request::Graph` seed path: a
/// bounded k-hop subgraph export rooted at `seeds`. Flushes first (like
/// `mem_list`/`mem_get`) so a just-remembered node's still-buffered semantic
/// is a valid seed/neighbor.
pub(crate) fn mem_graph(
    mem: &mut Memory<SqliteStorage>,
    seeds: &[u64],
    depth: usize,
    budget: usize,
) -> Result<Subgraph, Error> {
    mem.flush_all()?;
    let seed_ids: Vec<NodeId> = seeds.iter().copied().map(NodeId).collect();
    mem.subgraph(&seed_ids, depth, budget)
}

/// Resolve `Request::Graph`'s `query` path into seed node ids: the same
/// [`Memory::search`] the `recall` path calls (see
/// [`crate::memory::mem_recall_packaged_gated`]), but a pure read — no
/// reinforcement, no tick — since rendering a graph view is not a "use" of
/// the returned memories. Returns ids in ranked (highest-score-first) order;
/// no matches yields an empty vec (never an error), so a query with no hits
/// renders an empty subgraph rather than failing the request.
pub(crate) fn resolve_seeds_from_query(
    mem: &mut Memory<SqliteStorage>,
    query: &str,
    k: usize,
) -> Result<Vec<u64>, Error> {
    let recall = mem.search(query, k)?;
    Ok(recall.hits.iter().map(|h| h.node_id.0).collect())
}
