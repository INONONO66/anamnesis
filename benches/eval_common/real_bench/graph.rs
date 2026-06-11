use std::collections::HashMap;
use std::sync::Arc;

use anamnesis::Error;
use anamnesis::Memory;
use anamnesis::SqliteStorage;
use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::{NodeId, Timestamp};
use serde::{Deserialize, Serialize};

use super::dataset::{BenchSession, BenchTurn, LoadedBenchmark};
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

pub fn build_memory_graph(
    dataset: &LoadedBenchmark,
    provider: &dyn EmbeddingProvider,
    cache: Option<&super::embed_cache::EmbedCache>,
) -> BenchResult<BuiltMemoryGraph> {
    // Wrap provider + cache into an Arc so Memory can hold it.
    // SAFETY: CachingProvider borrows from the outer scope, but we only use it
    // inside this function before returning, so the lifetimes are correct.
    // We use Arc<CachingProvider> with a transmuted lifetime to satisfy the
    // 'static bound — instead, we wrap with a newtype that erases the lifetime
    // by holding the provider/cache pointers as raw references.
    //
    // Simpler: build an owned Vec<Vec<f64>> for all texts upfront via
    // embed_texts (preserves batch caching), then hand a StaticProvider to
    // Memory that serves pre-computed embeddings. But that reintroduces the
    // batch pre-compute. Instead, use the Arc trick: create a
    // CachingProviderArc that boxes the inner ref behind Arc<Mutex<..>> — but
    // that requires 'static inner.
    //
    // Cleanest solution that doesn't require 'static: Pre-embed all texts
    // upfront (preserving the cache/batch path) then give Memory a
    // PrecomputedProvider backed by those vecs. This is equivalent to the old
    // recipe for embedding VALUES, different only in how the Arc is handed in.

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

    // Pre-compute all embeddings upfront (same as before: batched + cached).
    // This ensures embedding VALUES are identical to the old recipe.
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

    let total_texts = speaker_embeddings.len() + window_embeddings.len();

    // Build a PrecomputedProvider that serves embeddings by text lookup.
    // We index them in insertion order (same as the old recipe).
    let mut embed_map: HashMap<String, Vec<f64>> = HashMap::new();
    for (text, embedding) in speaker_texts.iter().zip(speaker_embeddings.iter()) {
        embed_map.insert(text.clone(), embedding.clone());
    }
    for (text, embedding) in window_texts.iter().zip(window_embeddings.iter()) {
        embed_map
            .entry(text.clone())
            .or_insert_with(|| embedding.clone());
    }
    let precomputed = Arc::new(PrecomputedProvider(embed_map));

    let mut memory = Memory::in_memory_with_provider(precomputed as Arc<dyn EmbeddingProvider>)
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

// ── PrecomputedProvider ───────────────────────────────────────────────────────

/// An `EmbeddingProvider` backed by a pre-computed lookup table.
///
/// Used by `build_memory_graph` to hand pre-batched embeddings to `Memory`
/// without re-computing them. Text lookup is exact-match; unknown texts are
/// an error (should never happen in normal bench flow).
struct PrecomputedProvider(HashMap<String, Vec<f64>>);

impl EmbeddingProvider for PrecomputedProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        texts
            .iter()
            .map(|text| {
                self.0
                    .get(*text)
                    .map(|v| v.iter().map(|&x| x as f32).collect())
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "PrecomputedProvider: no embedding for {:?}",
                            text
                        ))
                    })
            })
            .collect()
    }

    fn dimensions(&self) -> usize {
        self.0.values().next().map(|v| v.len()).unwrap_or(0)
    }

    fn model_name(&self) -> &str {
        "precomputed"
    }
}

// ── Misc helpers ─────────────────────────────────────────────────────────────

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
