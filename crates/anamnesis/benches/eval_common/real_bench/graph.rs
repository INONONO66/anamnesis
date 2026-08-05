use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anamnesis::Error;
use anamnesis::Memory;
use anamnesis::embedding::EmbeddingProvider;
use anamnesis::engine::SqliteStorage;
use anamnesis::graph::{KnowledgeType, NodeId, Timestamp};
use anamnesis::memory::AtomicFactInput;
use serde::{Deserialize, Serialize};

use super::dataset::{BenchTurn, FormationInput};
use super::error::{BenchError, BenchResult};

mod eval;

#[cfg(test)]
pub use eval::ranked_fragments_for_test;
pub use eval::{
    AnswerContext, AnswerEvidence, AnswerSourceAttribution, ConsumerSelectionPolicy, EvalOptions,
    QuestionEvaluation, ReadoutFeatureRow, RetrievedMemory, WarmupReport,
    evaluate_question_with_context, evaluate_questions, run_warmup,
};

pub struct BuiltMemoryGraph {
    pub memory: Memory<SqliteStorage>,
    pub provenance_by_node: HashMap<NodeId, NodeProvenance>,
    pub stats: GraphBuildStats,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphBuildStats {
    pub nodes_created: usize,
    pub temporal_edges_created: usize,
    pub extracted_edges_created: usize,
    #[serde(default)]
    pub derived_nodes_created: usize,
    #[serde(default)]
    pub atomic_facts_created: usize,
    #[serde(default)]
    pub derived_relations_validated: usize,
    #[serde(default)]
    pub reasoning_edges_created: usize,
    pub embedded_texts: usize,
}

/// Frozen, reference-blind consumer extraction artifact.
///
/// The artifact is produced from conversation turns only. Dataset fingerprints
/// and extractor identity are checked by the answer harness before ingest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedMemoryArtifact {
    pub schema_version: u32,
    pub dataset_fnv1a64: String,
    pub extractor_model: String,
    pub extractor_digest: String,
    pub prompt_version: String,
    pub records: Vec<DerivedMemoryRecord>,
    #[serde(default)]
    pub relations: Vec<DerivedMemoryRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedMemoryRecord {
    pub id: String,
    pub source_session_id: String,
    pub kind: String,
    pub content: String,
    pub source_turn_ids: Vec<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub evidence_object: Option<String>,
    #[serde(default)]
    pub evidence_span: Option<String>,
    #[serde(default)]
    pub evidence_source_turn_id: Option<String>,
    #[serde(default)]
    pub entity_tags: Vec<String>,
    pub valid_from_ms: Option<u64>,
    pub valid_until_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedMemoryRelation {
    pub from: String,
    pub to: String,
    pub kind: DerivedRelationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivedRelationKind {
    Reason,
    Causal,
    Contradicts,
    Supports,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProvenance {
    pub dataset: String,
    pub session_id: String,
    pub raw_session_id: String,
    pub raw_turn_id: Option<String>,
    pub turn_index: usize,
    pub speaker: String,
    pub content: String,
}

/// Dataset loaders keep their wire timestamps as epoch seconds. Convert them
/// at the graph boundary because the engine's `Timestamp` contract is epoch
/// milliseconds. Synthetic timestamps are already generated in milliseconds.
const MILLIS_PER_SECOND: u64 = 1_000;
const SESSION_GAP_MS: u64 = 86_400_000;
const TURN_GAP_MS: u64 = 60_000;

// ── build_memory_graph ───────────────────────────────────────────────────────

/// Build the in-memory graph from a loaded benchmark dataset.
///
/// `provider` must be a `CachingProvider` (or any `Arc<dyn EmbeddingProvider>`)
/// that handles both ingest-time embeddings (called per-text by `Memory::add`)
/// and query-time embeddings (called by `Memory::search_result_at_with`).
/// Build it with [`CachingProvider::new`] before calling this function.
pub fn build_memory_graph(
    input: FormationInput<'_>,
    provider: Arc<dyn EmbeddingProvider>,
) -> BenchResult<BuiltMemoryGraph> {
    build_memory_graph_with_derived(input, provider, &[], &[])
}

/// Build the product graph and add a frozen consumer extraction artifact.
///
/// Derived records enter the production atomic-fact sidecar and retain all
/// cited raw Episodic source IDs. They never enter the graph candidate pool,
/// node FTS corpus, or attraction dynamics.
pub fn build_memory_graph_with_derived(
    input: FormationInput<'_>,
    provider: Arc<dyn EmbeddingProvider>,
    derived: &[DerivedMemoryRecord],
    relations: &[DerivedMemoryRelation],
) -> BenchResult<BuiltMemoryGraph> {
    let session_turns: Vec<Vec<&BenchTurn>> = input
        .sessions
        .iter()
        .map(|session| {
            session
                .turns
                .iter()
                .filter(|turn| !turn.content.trim().is_empty())
                .collect()
        })
        .collect();

    let total_texts = session_turns
        .iter()
        .map(|turns| turns.len() * 2)
        .sum::<usize>();

    let mut memory = Memory::in_memory_with_provider(provider)
        .map_err(|err| BenchError::Engine(err.to_string()))?;

    let mut provenance_by_node: HashMap<NodeId, NodeProvenance> = HashMap::new();
    let mut stats = GraphBuildStats {
        embedded_texts: total_texts,
        ..GraphBuildStats::default()
    };

    let base_timestamp = ingest_base_timestamp(session_turns.len() as u64);

    // Per-session: keep the previous turn's provenance so we can insert the
    // semantic node's provenance when AddReceipt.finalized_semantic arrives
    // (one-turn lag: semantic of turn i-1 is returned when turn i is added).
    for (session_index, turns) in session_turns.iter().enumerate() {
        let session = &input.sessions[session_index];
        let session_id = &session.raw_session_id;
        let session_start = session.start_timestamp.map_or_else(
            || base_timestamp + session_index as u64 * SESSION_GAP_MS,
            |epoch_seconds| epoch_seconds.saturating_mul(MILLIS_PER_SECOND),
        );

        // Provenance of the previous turn — needed to assign the semantic node
        // id returned by add() (which is the semantic for that previous turn).
        let mut prev_provenance: Option<NodeProvenance> = None;

        for (turn_position, turn) in turns.iter().enumerate() {
            let timestamp = Timestamp(session_start + turn_position as u64 * TURN_GAP_MS);

            let receipt = memory
                .add(session_id, &turn.speaker, &turn.content, timestamp)
                .map_err(|err| BenchError::Engine(err.to_string()))?;

            // The episodic node belongs to the current turn.
            let epi_prov = node_provenance(input.dataset.as_str(), turn);
            provenance_by_node.insert(receipt.episodic, epi_prov.clone());
            stats.nodes_created += 1;

            // The semantic node (if returned) belongs to the PREVIOUS turn
            // (one-turn lag: it was finalized now that we have the +1 context).
            if let Some(sem_id) = receipt.finalized_semantic {
                if let Some(prev_prov) = prev_provenance.take() {
                    provenance_by_node.insert(sem_id, prev_prov);
                }
                stats.nodes_created += 1;
                stats.extracted_edges_created += 1;
            }

            // Temporal edge counter: wired by Memory for every turn after the first.
            if turn_position > 0 {
                stats.temporal_edges_created += 1;
            }

            prev_provenance = Some(epi_prov);
        }

        // Flush the session to finalize the last turn's semantic node.
        let last_sem_id = memory
            .flush_session(session_id)
            .map_err(|err| BenchError::Engine(err.to_string()))?;
        if let Some(sem_id) = last_sem_id {
            if let Some(prev_prov) = prev_provenance.take() {
                provenance_by_node.insert(sem_id, prev_prov);
            }
            stats.nodes_created += 1;
            stats.extracted_edges_created += 1;
        }
    }

    ingest_derived_memories(
        &mut memory,
        &mut provenance_by_node,
        &mut stats,
        input,
        derived,
        relations,
    )?;

    Ok(BuiltMemoryGraph {
        memory,
        provenance_by_node,
        stats,
    })
}

fn ingest_derived_memories(
    memory: &mut Memory<SqliteStorage>,
    provenance_by_node: &mut HashMap<NodeId, NodeProvenance>,
    stats: &mut GraphBuildStats,
    input: FormationInput<'_>,
    records: &[DerivedMemoryRecord],
    relations: &[DerivedMemoryRelation],
) -> BenchResult<()> {
    let sessions: HashMap<&str, &str> = input
        .sessions
        .iter()
        .map(|session| (session.session_id.as_str(), session.raw_session_id.as_str()))
        .collect();
    let mut episodic_by_turn: HashMap<String, NodeId> = HashMap::new();
    for (node_id, provenance) in provenance_by_node.iter() {
        let Some(turn_id) = provenance.raw_turn_id.as_deref() else {
            continue;
        };
        let node = memory
            .engine()
            .graph()
            .get_node(*node_id)
            .map_err(|error| BenchError::Engine(error.to_string()))?;
        if node.node_type == KnowledgeType::Episodic {
            episodic_by_turn.insert(turn_id.to_owned(), *node_id);
        }
    }

    let mut seen_ids = std::collections::HashSet::new();
    let mut materialized = std::collections::HashSet::new();
    for record in records {
        let Some(raw_session_id) = sessions.get(record.source_session_id.as_str()).copied() else {
            continue;
        };
        validate_derived_record(record, &mut seen_ids)?;
        let mut sources = Vec::with_capacity(record.source_turn_ids.len());
        let mut source_by_turn = HashMap::new();
        for turn_id in &record.source_turn_ids {
            let source = episodic_by_turn.get(turn_id.as_str()).ok_or_else(|| {
                BenchError::InvalidInput(format!(
                    "derived record {:?} cites unknown source turn {:?}",
                    record.id, turn_id
                ))
            })?;
            let provenance = provenance_by_node.get(source).ok_or_else(|| {
                BenchError::Parse(format!("source node {} lost provenance", source.0))
            })?;
            if provenance.session_id != record.source_session_id {
                return Err(BenchError::InvalidInput(format!(
                    "derived record {:?} crosses source sessions",
                    record.id
                )));
            }
            sources.push(*source);
            source_by_turn.insert(turn_id.as_str(), *source);
        }
        let evidence_reference = validate_derived_grounding(memory, record, &source_by_turn)?;

        let mut metadata = vec![
            ("anamnesis:derived-record-id".to_owned(), record.id.clone()),
            (
                "anamnesis:derived-record-kind".to_owned(),
                record.kind.trim().to_owned(),
            ),
            (
                "anamnesis:fact-kind".to_owned(),
                record.kind.trim().to_owned(),
            ),
            (
                "anamnesis:source-session".to_owned(),
                raw_session_id.to_owned(),
            ),
        ];
        for (key, value) in [
            ("anamnesis:ground-subject", record.subject.as_ref()),
            ("anamnesis:ground-relation", record.relation.as_ref()),
            ("anamnesis:ground-object", record.object.as_ref()),
            ("anamnesis:evidence-object", record.evidence_object.as_ref()),
        ] {
            if let Some(value) = value {
                metadata.push((key.to_owned(), value.clone()));
            }
        }
        if let Some((source_node_id, start, end)) = evidence_reference {
            metadata.push((
                "anamnesis:evidence-source-node-id".to_owned(),
                source_node_id.0.to_string(),
            ));
            metadata.push((
                "anamnesis:evidence-span-start".to_owned(),
                start.to_string(),
            ));
            metadata.push(("anamnesis:evidence-span-end".to_owned(), end.to_string()));
        }
        memory
            .add_atomic_fact(
                AtomicFactInput::new(record.content.trim(), sources)
                    .with_embedding_surface(derived_routing_surface(record))
                    .with_entity_tags(record.entity_tags.clone())
                    .with_validity(
                        record.valid_from_ms.map(Timestamp),
                        record.valid_until_ms.map(Timestamp),
                    )
                    .with_metadata(metadata),
            )
            .map_err(|error| BenchError::Engine(error.to_string()))?;
        stats.atomic_facts_created = stats.atomic_facts_created.saturating_add(1);
        stats.embedded_texts = stats.embedded_texts.saturating_add(1);
        materialized.insert(record.id.as_str());
    }
    for relation in relations {
        let from_present = materialized.contains(relation.from.as_str());
        let to_present = materialized.contains(relation.to.as_str());
        if !from_present || !to_present {
            if from_present || to_present {
                return Err(BenchError::InvalidInput(format!(
                    "derived relation {:?}->{:?} crosses benchmark samples",
                    relation.from, relation.to
                )));
            }
            continue;
        }
        // The artifact declares a relation, but it does not carry an explicit
        // review decision. Validate its endpoints here without granting it
        // runtime routing authority; reviewed relations enter through the same
        // typed admission API used by ordinary consumers.
        stats.derived_relations_validated = stats.derived_relations_validated.saturating_add(1);
    }
    Ok(())
}

fn validate_derived_grounding(
    memory: &Memory<SqliteStorage>,
    record: &DerivedMemoryRecord,
    source_by_turn: &HashMap<&str, NodeId>,
) -> BenchResult<Option<(NodeId, usize, usize)>> {
    let fields_present = [
        record.subject.is_some(),
        record.relation.is_some(),
        record.object.is_some(),
        record.evidence_span.is_some(),
        record.evidence_source_turn_id.is_some(),
    ];
    let grounded = fields_present.iter().all(|present| *present);
    if fields_present.iter().any(|present| *present) != grounded {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} has partial grounding",
            record.id
        )));
    }
    if !grounded {
        return Ok(None);
    }

    let subject = record.subject.as_deref().unwrap_or_default().trim();
    let relation = record.relation.as_deref().unwrap_or_default().trim();
    let object = record.object.as_deref().unwrap_or_default().trim();
    let evidence_object = record.evidence_object.as_deref().unwrap_or(object).trim();
    let evidence_span = record.evidence_span.as_deref().unwrap_or_default().trim();
    let evidence_turn = record
        .evidence_source_turn_id
        .as_deref()
        .unwrap_or_default();
    let expected_content = format!("{subject} {relation} {object}");
    if subject.is_empty()
        || relation.is_empty()
        || object.is_empty()
        || evidence_object.is_empty()
        || evidence_span.is_empty()
        || record.content.trim() != expected_content
        || record.evidence_object.as_ref().map_or_else(
            || !phrase_tokens_contain(evidence_span, evidence_object),
            |_| !evidence_span.contains(evidence_object),
        )
        || phrase_tokens(subject)
            .iter()
            .any(|token| is_unresolved_subject_token(token))
        || (record.evidence_object.is_some()
            && phrase_tokens(object)
                .iter()
                .any(|token| is_first_person_token(token)))
    {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} has invalid object grounding",
            record.id
        )));
    }
    let source_node_id = source_by_turn.get(evidence_turn).copied().ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "derived record {:?} cites an invalid evidence source",
            record.id
        ))
    })?;
    let source = memory
        .engine()
        .graph()
        .get_node(source_node_id)
        .map_err(|error| BenchError::Engine(error.to_string()))?;
    let Some(start) = source.content.find(evidence_span) else {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} evidence span is not verbatim",
            record.id
        )));
    };
    let end = start.checked_add(evidence_span.len()).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "derived record {:?} evidence span overflows",
            record.id
        ))
    })?;
    Ok(Some((source_node_id, start, end)))
}

fn derived_routing_surface(record: &DerivedMemoryRecord) -> String {
    match record
        .evidence_object
        .as_deref()
        .or(record.object.as_deref())
        .map(str::trim)
        .filter(|evidence_object| !evidence_object.is_empty())
    {
        Some(evidence_object) => {
            format!(
                "{}\nEvidence object: {}",
                record.content.trim(),
                evidence_object.trim(),
            )
        }
        None => record.content.trim().to_owned(),
    }
}

fn phrase_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn is_first_person_token(token: &str) -> bool {
    matches!(
        token,
        "i" | "me" | "my" | "mine" | "myself" | "we" | "us" | "our" | "ours" | "ourselves"
    )
}

fn is_unresolved_subject_token(token: &str) -> bool {
    is_first_person_token(token)
        || matches!(
            token,
            "you"
                | "your"
                | "yours"
                | "yourself"
                | "yourselves"
                | "he"
                | "him"
                | "his"
                | "himself"
                | "she"
                | "her"
                | "hers"
                | "herself"
                | "they"
                | "them"
                | "their"
                | "theirs"
                | "themselves"
                | "it"
                | "its"
                | "itself"
        )
}

fn phrase_tokens_contain(haystack: &str, needle: &str) -> bool {
    let haystack = phrase_tokens(haystack);
    let needle = phrase_tokens(needle);
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle.as_slice())
}

fn validate_derived_record<'a>(
    record: &'a DerivedMemoryRecord,
    seen_ids: &mut std::collections::HashSet<&'a str>,
) -> BenchResult<()> {
    if record.id.trim().is_empty()
        || record.id.trim().chars().count() > 64
        || record.source_session_id.trim().is_empty()
        || record.kind.trim().is_empty()
        || record.content.trim().is_empty()
        || record.content.trim().chars().count() > 500
        || record.source_turn_ids.is_empty()
    {
        return Err(BenchError::InvalidInput(
            "derived record requires id, source_session_id, kind, content, and sources".to_owned(),
        ));
    }
    const KINDS: [&str; 9] = [
        "fact",
        "entity",
        "event",
        "preference",
        "decision",
        "causal",
        "lesson",
        "convention",
        "gotcha",
    ];
    if !KINDS.contains(&record.kind.trim()) {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} has unsupported kind {:?}",
            record.id, record.kind
        )));
    }
    let unique_sources: std::collections::HashSet<_> =
        record.source_turn_ids.iter().map(String::as_str).collect();
    if unique_sources.len() != record.source_turn_ids.len()
        || record
            .source_turn_ids
            .iter()
            .any(|source| source.trim().is_empty())
    {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} has invalid or duplicate sources",
            record.id
        )));
    }
    let unique_tags: std::collections::HashSet<_> =
        record.entity_tags.iter().map(String::as_str).collect();
    if record.entity_tags.len() > 16
        || unique_tags.len() != record.entity_tags.len()
        || record
            .entity_tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.chars().count() > 64)
    {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} has invalid entity tags",
            record.id
        )));
    }
    if !seen_ids.insert(record.id.as_str()) {
        return Err(BenchError::InvalidInput(format!(
            "duplicate derived record id {:?}",
            record.id
        )));
    }
    if record
        .valid_from_ms
        .zip(record.valid_until_ms)
        .is_some_and(|(start, end)| start >= end)
    {
        return Err(BenchError::InvalidInput(format!(
            "derived record {:?} has an invalid validity window",
            record.id
        )));
    }
    Ok(())
}

// ── CachingProvider ───────────────────────────────────────────────────────────

/// An `EmbeddingProvider` that wraps an inner provider with an optional SQLite
/// embedding cache.
///
/// Every call to `embed` is resolved per-text: cache hits are served from the
/// SQLite store; misses are batched into the inner provider and written back.
/// This makes reruns cheap while allowing `Memory` to embed arbitrary texts
/// (including query strings) without pre-batching.
///
/// `EmbedCache` holds a `rusqlite::Connection` which is `Send` but not `Sync`;
/// we guard it with a `Mutex` so that `CachingProvider` is `Send + Sync`.
pub struct CachingProvider {
    inner: Arc<dyn EmbeddingProvider>,
    cache: Option<Mutex<super::embed_cache::EmbedCache>>,
}

impl CachingProvider {
    /// Create a new `CachingProvider`.
    ///
    /// Pass `cache: None` for an uncached provider (still wraps `inner`
    /// transparently).
    pub fn new(
        inner: Arc<dyn EmbeddingProvider>,
        cache: Option<super::embed_cache::EmbedCache>,
    ) -> Self {
        Self {
            inner,
            cache: cache.map(Mutex::new),
        }
    }

    fn cached_single<F>(&self, role: &str, text: &str, embed: F) -> Result<Vec<f32>, Error>
    where
        F: FnOnce() -> Result<Vec<f32>, Error>,
    {
        let Some(cache_mutex) = &self.cache else {
            return embed();
        };
        // Query and passage formatting can differ for asymmetric models. Keep
        // their cache entries distinct and do not reuse legacy untyped rows.
        let cache_key = format!("\u{1f}{role}:{text}");
        let cache = cache_mutex
            .lock()
            .map_err(|_| Error::InvalidInput("embed cache mutex poisoned".to_string()))?;
        if let Some(hit) = cache
            .get(&cache_key)
            .map_err(|err| Error::InvalidInput(err.to_string()))?
        {
            return Ok(hit.into_iter().map(|value| value as f32).collect());
        }
        let fresh = embed()?;
        let widened: Vec<f64> = fresh.iter().map(|&value| f64::from(value)).collect();
        cache
            .put(&cache_key, &widened)
            .map_err(|err| Error::InvalidInput(err.to_string()))?;
        Ok(fresh)
    }
}

impl EmbeddingProvider for CachingProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        let Some(cache_mutex) = &self.cache else {
            return self.inner.embed(texts);
        };
        let cache = cache_mutex
            .lock()
            .map_err(|_| Error::InvalidInput("embed cache mutex poisoned".to_string()))?;

        // Per-text cache lookup; collect indices of misses.
        let mut results: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        let mut miss_indices: Vec<usize> = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            let hit = cache
                .get(text)
                .map_err(|e| Error::InvalidInput(e.to_string()))?
                .map(|f64_vec| f64_vec.iter().map(|&x| x as f32).collect());
            if hit.is_none() {
                miss_indices.push(index);
            }
            results.push(hit);
        }

        if !miss_indices.is_empty() {
            let miss_texts: Vec<&str> = miss_indices.iter().map(|&i| texts[i]).collect();
            let fresh = self.inner.embed(&miss_texts)?;
            if fresh.len() != miss_indices.len() {
                return Err(Error::InvalidInput(format!(
                    "CachingProvider: inner provider returned {} embeddings for {} texts",
                    fresh.len(),
                    miss_indices.len()
                )));
            }
            for (&index, vec_f32) in miss_indices.iter().zip(fresh.iter()) {
                let vec_f64: Vec<f64> = vec_f32.iter().map(|&x| x as f64).collect();
                cache
                    .put(texts[index], &vec_f64)
                    .map_err(|e| Error::InvalidInput(e.to_string()))?;
                results[index] = Some(vec_f32.clone());
            }
        }

        Ok(results
            .into_iter()
            .map(|v| v.expect("all slots filled above"))
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.cached_single("query-v1", text, || self.inner.embed_query(text))
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let Some(cache_mutex) = &self.cache else {
            return self.inner.embed_queries(texts);
        };
        let cache = cache_mutex
            .lock()
            .map_err(|_| Error::InvalidInput("embed cache mutex poisoned".to_string()))?;
        let cache_keys: Vec<_> = texts
            .iter()
            .map(|text| format!("\u{1f}query-v1:{text}"))
            .collect();
        let mut results = Vec::with_capacity(texts.len());
        let mut miss_indices = Vec::new();
        for (index, cache_key) in cache_keys.iter().enumerate() {
            let hit = cache
                .get(cache_key)
                .map_err(|error| Error::InvalidInput(error.to_string()))?
                .map(|values| values.into_iter().map(|value| value as f32).collect());
            if hit.is_none() {
                miss_indices.push(index);
            }
            results.push(hit);
        }

        if !miss_indices.is_empty() {
            let misses: Vec<_> = miss_indices.iter().map(|&index| texts[index]).collect();
            let fresh = self.inner.embed_queries(&misses)?;
            if fresh.len() != miss_indices.len() {
                return Err(Error::InvalidInput(format!(
                    "CachingProvider: inner provider returned {} query embeddings for {} texts",
                    fresh.len(),
                    miss_indices.len()
                )));
            }
            for (&index, embedding) in miss_indices.iter().zip(fresh) {
                let widened: Vec<_> = embedding.iter().map(|&value| f64::from(value)).collect();
                cache
                    .put(&cache_keys[index], &widened)
                    .map_err(|error| Error::InvalidInput(error.to_string()))?;
                results[index] = Some(embedding);
            }
        }

        results
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                Error::InvalidInput("CachingProvider left a query embedding slot empty".to_string())
            })
    }

    fn embed_passage(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.cached_single("passage-v1", text, || self.inner.embed_passage(text))
    }
}

// ── Misc helpers ─────────────────────────────────────────────────────────────

fn ingest_base_timestamp(session_count: u64) -> u64 {
    let span = session_count.max(1) * SESSION_GAP_MS + SESSION_GAP_MS;
    Timestamp::now().0.saturating_sub(span)
}

fn node_provenance(dataset: &str, turn: &BenchTurn) -> NodeProvenance {
    NodeProvenance {
        dataset: dataset.to_string(),
        session_id: turn.session_id.clone(),
        raw_session_id: turn.raw_session_id.clone(),
        raw_turn_id: turn.raw_turn_id.clone(),
        turn_index: turn.turn_index,
        speaker: turn.speaker.clone(),
        content: turn.content.clone(),
    }
}
