use std::collections::HashSet;
use std::time::Instant;

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::query::{ContextPackage, Fragment, ReadoutCandidate, SearchInput, SearchResult};
use anamnesis::{ConfidenceLevel, Engine, SqliteStorage};
use serde::{Deserialize, Serialize};

use super::super::dataset::BenchQuestion;
use super::super::error::{BenchError, BenchResult};
use super::super::metrics::{RankedRetrieval, RetrievalMetrics, first_hit_rank, retrieval_metrics};
use super::{BuiltMemoryGraph, embed_texts};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmupReport {
    pub questions: usize,
    pub sites_accessed: usize,
    pub paths_strengthened: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionEvaluation {
    pub question_id: String,
    pub question_type: String,
    /// Sample (conversation/haystack) this question belongs to — needed for
    /// train/dev split comparisons (even = train, odd = dev).
    pub sample_index: usize,
    pub search_latency_ms: f64,
    pub total_relevant: usize,
    /// Pre-package readout surface (primary retrieval metric).
    pub retrieval_metrics: RetrievalMetrics,
    /// Packaged ContextPackage surface (context-shape metric).
    pub package_metrics: RetrievalMetrics,
    pub first_hit_rank: Option<usize>,
    pub returned_fragments: usize,
    pub retrievals: Vec<RetrievedMemory>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievedMemory {
    pub rank: usize,
    pub node_id: u64,
    pub relevant: bool,
    pub matched_gold_units: Vec<String>,
    pub score: f64,
    pub session_id: String,
    pub raw_session_id: String,
    pub raw_turn_id: Option<String>,
    pub content_chars: usize,
}

pub fn run_warmup(
    graph: &mut BuiltMemoryGraph,
    questions: &[BenchQuestion],
    provider: &dyn EmbeddingProvider,
    top_k: usize,
) -> BenchResult<WarmupReport> {
    let mut report = WarmupReport::default();
    for question in questions {
        let result = search_question(&graph.engine, question, provider, top_k)?;
        let (_, commit) = graph
            .engine
            .commit(result.package, Some(ConfidenceLevel::Medium))
            .map_err(|err| BenchError::Engine(err.to_string()))?;
        report.questions += 1;
        report.sites_accessed += commit.sites_accessed;
        report.paths_strengthened += commit.paths_strengthened;
    }
    Ok(report)
}

pub fn evaluate_questions(
    graph: &BuiltMemoryGraph,
    questions: &[BenchQuestion],
    provider: &dyn EmbeddingProvider,
    top_k: usize,
) -> BenchResult<Vec<QuestionEvaluation>> {
    questions
        .iter()
        .map(|question| evaluate_question(graph, question, provider, top_k))
        .collect()
}

fn evaluate_question(
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    provider: &dyn EmbeddingProvider,
    top_k: usize,
) -> BenchResult<QuestionEvaluation> {
    let start = Instant::now();
    let result = search_question(&graph.engine, question, provider, top_k)?;
    let search_latency_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Primary surface: pre-package readout candidates
    let retrievals = readout_retrievals(&result.trace.readout, graph, question, top_k);
    let readout_ranked: Vec<_> = retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    // Package surface: packaged ContextPackage fragments
    let package_retrievals = retrieved_memories(&result.package, graph, question, top_k);
    let package_ranked: Vec<_> = package_retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    let total_relevant = question.gold.total_relevant_units();
    let returned_fragments = result.package.total_fragments();

    Ok(QuestionEvaluation {
        question_id: question.question_id.clone(),
        question_type: question.question_type.clone(),
        sample_index: question.sample_index,
        search_latency_ms,
        total_relevant,
        retrieval_metrics: retrieval_metrics(&readout_ranked, total_relevant, top_k),
        package_metrics: retrieval_metrics(&package_ranked, total_relevant, top_k),
        first_hit_rank: first_hit_rank(&readout_ranked),
        returned_fragments,
        retrievals,
    })
}

fn search_question(
    engine: &Engine<SqliteStorage>,
    question: &BenchQuestion,
    provider: &dyn EmbeddingProvider,
    top_k: usize,
) -> BenchResult<SearchResult> {
    let embedding = embed_texts(provider, std::slice::from_ref(&question.question))?
        .into_iter()
        .next()
        .ok_or_else(|| BenchError::Embedding("provider returned no query embedding".to_string()))?;
    let result = engine
        .search(SearchInput {
            text: question.question.clone(),
            query_embedding: Some(embedding),
            limit: top_k,
            seed_limit: Some(top_k.max(1)),
            ..SearchInput::default()
        })
        .map_err(|err| BenchError::Engine(err.to_string()))?;
    Ok(result)
}

fn readout_retrievals(
    readout: &[ReadoutCandidate],
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
) -> Vec<RetrievedMemory> {
    let mut seen_units = HashSet::new();
    readout
        .iter()
        .take(top_k)
        .enumerate()
        .filter_map(|(index, candidate)| {
            let provenance = graph.provenance_by_node.get(&candidate.node_id)?;
            let matched_gold_units: Vec<_> = question
                .gold
                .matched_units(
                    &provenance.raw_session_id,
                    provenance.raw_turn_id.as_deref(),
                    &provenance.content,
                )
                .into_iter()
                .filter(|unit| seen_units.insert(unit.clone()))
                .collect();
            let relevant = !matched_gold_units.is_empty();
            Some(RetrievedMemory {
                rank: index + 1,
                node_id: candidate.node_id.0,
                relevant,
                matched_gold_units,
                score: candidate.score,
                session_id: provenance.session_id.clone(),
                raw_session_id: provenance.raw_session_id.clone(),
                raw_turn_id: provenance.raw_turn_id.clone(),
                content_chars: provenance.content.chars().count(),
            })
        })
        .collect()
}

fn retrieved_memories(
    package: &ContextPackage,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
) -> Vec<RetrievedMemory> {
    let mut seen_units = HashSet::new();
    ranked_fragments(package)
        .into_iter()
        .take(top_k)
        .enumerate()
        .filter_map(|(index, fragment)| {
            let provenance = graph.provenance_by_node.get(&fragment.node_id)?;
            let matched_gold_units: Vec<_> = question
                .gold
                .matched_units(
                    &provenance.raw_session_id,
                    provenance.raw_turn_id.as_deref(),
                    &provenance.content,
                )
                .into_iter()
                .filter(|unit| seen_units.insert(unit.clone()))
                .collect();
            let relevant = !matched_gold_units.is_empty();
            Some(RetrievedMemory {
                rank: index + 1,
                node_id: fragment.node_id.0,
                relevant,
                matched_gold_units,
                score: fragment.relevance,
                session_id: provenance.session_id.clone(),
                raw_session_id: provenance.raw_session_id.clone(),
                raw_turn_id: provenance.raw_turn_id.clone(),
                content_chars: provenance.content.chars().count(),
            })
        })
        .collect()
}

fn collect_fragments(package: &ContextPackage) -> Vec<Fragment> {
    package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .cloned()
        .collect()
}

fn ranked_fragments(package: &ContextPackage) -> Vec<Fragment> {
    let mut fragments = collect_fragments(package);
    fragments.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| left.node_id.0.cmp(&right.node_id.0))
    });
    fragments
}

#[cfg(test)]
pub fn ranked_fragments_for_test(package: &ContextPackage) -> Vec<Fragment> {
    ranked_fragments(package)
}
