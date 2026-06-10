use std::collections::HashMap;

use anamnesis::api::{IngestResult, Observation};
use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::peer::SourceKind;
use anamnesis::{Engine, EngineConfig, SqliteStorage};
use serde::{Deserialize, Serialize};

use super::dataset::{BenchSession, BenchTurn, LoadedBenchmark};
use super::error::{BenchError, BenchResult};

mod eval;

#[cfg(test)]
pub use eval::ranked_fragments_for_test;
pub use eval::{
    QuestionEvaluation, ReadoutFeatureRow, RetrievedMemory, WarmupReport, evaluate_questions,
    run_warmup,
};

pub struct BuiltMemoryGraph {
    pub engine: Engine<SqliteStorage>,
    pub provenance_by_node: HashMap<NodeId, NodeProvenance>,
    pub stats: GraphBuildStats,
    pub speakers: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphBuildStats {
    pub nodes_created: usize,
    pub temporal_edges_created: usize,
    pub extracted_edges_created: usize,
    pub embedded_texts: usize,
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

/// Dataset timestamps are used for ingest when present (preferred); otherwise
/// sessions are spaced a day apart anchored to the wall clock so
/// activation-dependent decay sees realistic synthetic trace ages.
const SESSION_GAP_SECS: u64 = 86_400;
const TURN_GAP_SECS: u64 = 60;

pub fn build_memory_graph(
    dataset: &LoadedBenchmark,
    provider: &dyn EmbeddingProvider,
    cache: Option<&super::embed_cache::EmbedCache>,
) -> BenchResult<BuiltMemoryGraph> {
    // Two views per turn: the speaker-prefixed turn (episodic) and the
    // speaker-prefixed +/-1-turn context window (semantic). Benchmark
    // questions reference participants by name and often depend on the
    // surrounding exchange, so both views carry signals the raw turn lacks.
    let session_turns: Vec<Vec<&BenchTurn>> = dataset
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
    let speaker_texts: Vec<String> = session_turns
        .iter()
        .flat_map(|turns| turns.iter().map(|turn| speaker_text(turn)))
        .collect();
    let window_texts: Vec<String> = session_turns
        .iter()
        .flat_map(|turns| (0..turns.len()).map(|index| window_text(turns, index)))
        .collect();
    let speaker_embeddings = embed_texts(provider, cache, &speaker_texts)?;
    let window_embeddings = embed_texts(provider, cache, &window_texts)?;

    let mut engine = Engine::with_config(benchmark_config());
    let mut provenance_by_node = HashMap::new();
    let mut stats = GraphBuildStats {
        embedded_texts: speaker_embeddings.len() + window_embeddings.len(),
        ..GraphBuildStats::default()
    };
    let mut embedding_index = 0usize;
    let base_timestamp = ingest_base_timestamp(session_turns.len() as u64);

    for (session_index, turns) in session_turns.iter().enumerate() {
        let mut previous = None;
        let session_start = dataset.sessions[session_index]
            .start_timestamp
            .unwrap_or_else(|| base_timestamp + session_index as u64 * SESSION_GAP_SECS);
        for (turn_position, turn) in turns.iter().enumerate() {
            let speaker_embedding = speaker_embeddings
                .get(embedding_index)
                .cloned()
                .ok_or_else(|| BenchError::Embedding("missing turn embedding".to_string()))?;
            let window_embedding = window_embeddings
                .get(embedding_index)
                .cloned()
                .ok_or_else(|| BenchError::Embedding("missing window embedding".to_string()))?;
            let timestamp = Timestamp(session_start + turn_position as u64 * TURN_GAP_SECS);
            embedding_index += 1;
            let raw_id = ingest_turn(
                &mut engine,
                dataset.dataset.as_str(),
                turn,
                speaker_text(turn),
                speaker_embedding,
                KnowledgeType::Episodic,
                timestamp,
            )?;
            let semantic_id = ingest_turn(
                &mut engine,
                dataset.dataset.as_str(),
                turn,
                window_text(turns, turn_position),
                window_embedding,
                KnowledgeType::Semantic,
                timestamp,
            )?;
            stats.nodes_created += 2;
            provenance_by_node.insert(raw_id, node_provenance(dataset.dataset.as_str(), turn));
            provenance_by_node.insert(semantic_id, node_provenance(dataset.dataset.as_str(), turn));
            engine
                .link(semantic_id, raw_id, EdgeType::ExtractedFrom)
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            stats.extracted_edges_created += 1;
            if let Some(previous_id) = previous {
                engine
                    .link(previous_id, raw_id, EdgeType::Temporal)
                    .map_err(|err| BenchError::Engine(err.to_string()))?;
                stats.temporal_edges_created += 1;
            }
            previous = Some(raw_id);
        }
    }

    let speakers: Vec<String> = session_turns
        .iter()
        .flat_map(|turns| turns.iter().map(|turn| turn.speaker.clone()))
        .filter(|s| !s.trim().is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(BuiltMemoryGraph {
        engine,
        provenance_by_node,
        stats,
        speakers,
    })
}

fn speaker_text(turn: &BenchTurn) -> String {
    format!("{}: {}", turn.speaker, turn.content)
}

fn window_text(turns: &[&BenchTurn], index: usize) -> String {
    let mut parts = Vec::new();
    if index > 0 {
        parts.push(speaker_text(turns[index - 1]));
    }
    parts.push(speaker_text(turns[index]));
    if index + 1 < turns.len() {
        parts.push(speaker_text(turns[index + 1]));
    }
    parts.join("\n")
}

fn ingest_base_timestamp(session_count: u64) -> u64 {
    let span = session_count.max(1) * SESSION_GAP_SECS + SESSION_GAP_SECS;
    Timestamp::now().0.saturating_sub(span)
}

fn benchmark_config() -> EngineConfig {
    let mut config = EngineConfig::default();
    config.dedup_enabled = false;
    config.novelty_threshold = 0.0;
    config.confidence_threshold = 0.0;
    config
}

pub(super) fn embed_texts(
    provider: &dyn EmbeddingProvider,
    cache: Option<&super::embed_cache::EmbedCache>,
    texts: &[String],
) -> BenchResult<Vec<Vec<f64>>> {
    let Some(cache) = cache else {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        return provider
            .embed_f64(&refs)
            .map_err(|err| BenchError::Embedding(err.to_string()));
    };

    let mut results: Vec<Option<Vec<f64>>> = Vec::with_capacity(texts.len());
    let mut missing: Vec<usize> = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let hit = cache.get(text)?;
        if hit.is_none() {
            missing.push(index);
        }
        results.push(hit);
    }
    if !missing.is_empty() {
        let refs: Vec<&str> = missing.iter().map(|&i| texts[i].as_str()).collect();
        let fresh = provider
            .embed_f64(&refs)
            .map_err(|err| BenchError::Embedding(err.to_string()))?;
        if fresh.len() != missing.len() {
            return Err(BenchError::Embedding(format!(
                "provider returned {} embeddings for {} texts",
                fresh.len(),
                missing.len()
            )));
        }
        for (&index, vec) in missing.iter().zip(fresh) {
            cache.put(&texts[index], &vec)?;
            results[index] = Some(vec);
        }
    }
    Ok(results
        .into_iter()
        .map(|v| v.expect("filled above"))
        .collect())
}

fn ingest_turn(
    engine: &mut Engine<SqliteStorage>,
    dataset: &str,
    turn: &BenchTurn,
    content: String,
    embedding: Vec<f64>,
    node_type: KnowledgeType,
    timestamp: Timestamp,
) -> BenchResult<NodeId> {
    let observation = Observation {
        name: make_name(&content),
        summary: Some(format!("{} turn {}", turn.speaker, turn.turn_index + 1)),
        content,
        embedding: Some(embedding),
        confidence: 0.95,
        node_type,
        entity_tags: entity_tags(dataset, turn),
        origin: Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: turn.session_id.clone(),
            scope: ScopePath::universal(),
            confidence: 0.95,
        },
        timestamp,
        valid_from: None,
        valid_until: None,
    };
    match engine
        .ingest(observation)
        .map_err(|err| BenchError::Engine(err.to_string()))?
    {
        IngestResult::Created(ids) => ids
            .first()
            .copied()
            .ok_or_else(|| BenchError::Engine("ingest created no node".to_string())),
        IngestResult::Reinforced { existing_id, .. } => Ok(existing_id),
    }
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

fn make_name(content: &str) -> String {
    let name: String = content.chars().take(50).collect();
    if name.trim().is_empty() {
        "empty turn".to_string()
    } else {
        name
    }
}

const GENERIC_ROLES: [&str; 6] = ["user", "assistant", "system", "human", "ai", "bot"];

/// Exact-match corpus speaker names in the question text and return their
/// entity tags. Sensory cue extraction only — no gold evidence involved.
pub fn speaker_cue_tags(speakers: &[String], question: &str) -> Vec<String> {
    let question_lower = question.to_lowercase();
    speakers
        .iter()
        .filter(|speaker| {
            let lower = speaker.to_lowercase();
            lower.len() >= 3
                && !GENERIC_ROLES.contains(&lower.as_str())
                && name_in_question(&lower, &question_lower)
        })
        .map(|speaker| format!("speaker-{}", normalize_tag(speaker)))
        .collect()
}

/// Whether a lowercased speaker name appears in the lowercased question.
/// Single-word names require a whole-token match so "Tim" never fires on
/// "times"; multi-word names ("mary jane") use phrase containment.
fn name_in_question(name_lower: &str, question_lower: &str) -> bool {
    if name_lower.contains(' ') {
        question_lower.contains(name_lower)
    } else {
        question_lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|token| token == name_lower)
    }
}

fn entity_tags(dataset: &str, turn: &BenchTurn) -> Vec<String> {
    let mut tags = vec![
        format!("dataset-{}", normalize_tag(dataset)),
        format!("session-{}", normalize_tag(&turn.raw_session_id)),
        format!("speaker-{}", normalize_tag(&turn.speaker)),
    ];
    if let Some(raw_turn_id) = &turn.raw_turn_id {
        tags.push(format!("turn-{}", normalize_tag(raw_turn_id)));
    }
    tags
}

fn normalize_tag(value: &str) -> String {
    value.trim().to_lowercase().replace([' ', ':', '_'], "-")
}
