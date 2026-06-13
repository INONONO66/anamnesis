use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anamnesis::Error;
use anamnesis::Memory;
use anamnesis::embedding::EmbeddingProvider;
use anamnesis::engine::SqliteStorage;
use anamnesis::graph::{NodeId, Timestamp};
use serde::{Deserialize, Serialize};

use super::dataset::{BenchTurn, LoadedBenchmark};
use super::error::{BenchError, BenchResult};

mod eval;

#[cfg(test)]
pub use eval::ranked_fragments_for_test;
pub use eval::{
    EvalOptions, QuestionEvaluation, ReadoutFeatureRow, RetrievedMemory, WarmupReport,
    evaluate_questions, run_warmup,
};

pub struct BuiltMemoryGraph {
    pub memory: Memory<SqliteStorage>,
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

// ── build_memory_graph ───────────────────────────────────────────────────────

/// Build the in-memory graph from a loaded benchmark dataset.
///
/// `provider` must be a `CachingProvider` (or any `Arc<dyn EmbeddingProvider>`)
/// that handles both ingest-time embeddings (called per-text by `Memory::add`)
/// and query-time embeddings (called by `Memory::search_result_at_with`).
/// Build it with [`CachingProvider::new`] before calling this function.
pub fn build_memory_graph(
    dataset: &LoadedBenchmark,
    provider: Arc<dyn EmbeddingProvider>,
) -> BenchResult<BuiltMemoryGraph> {
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
        let session = &dataset.sessions[session_index];
        let session_id = &session.raw_session_id;
        let session_start = session
            .start_timestamp
            .unwrap_or_else(|| base_timestamp + session_index as u64 * SESSION_GAP_SECS);

        // Provenance of the previous turn — needed to assign the semantic node
        // id returned by add() (which is the semantic for that previous turn).
        let mut prev_provenance: Option<NodeProvenance> = None;

        for (turn_position, turn) in turns.iter().enumerate() {
            let timestamp = Timestamp(session_start + turn_position as u64 * TURN_GAP_SECS);

            let receipt = memory
                .add(session_id, &turn.speaker, &turn.content, timestamp)
                .map_err(|err| BenchError::Engine(err.to_string()))?;

            // The episodic node belongs to the current turn.
            let epi_prov = node_provenance(dataset.dataset.as_str(), turn);
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

    let speakers: Vec<String> = session_turns
        .iter()
        .flat_map(|turns| turns.iter().map(|turn| turn.speaker.clone()))
        .filter(|s| !s.trim().is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(BuiltMemoryGraph {
        memory,
        provenance_by_node,
        stats,
        speakers,
    })
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
}

// ── Misc helpers ─────────────────────────────────────────────────────────────

fn ingest_base_timestamp(session_count: u64) -> u64 {
    let span = session_count.max(1) * SESSION_GAP_SECS + SESSION_GAP_SECS;
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

fn normalize_tag(value: &str) -> String {
    value.trim().to_lowercase().replace([' ', ':', '_'], "-")
}
