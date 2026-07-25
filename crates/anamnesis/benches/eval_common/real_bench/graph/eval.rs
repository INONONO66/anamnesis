use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anamnesis::engine::StorageAdapter;
use anamnesis::graph::Timestamp;
use anamnesis::memory::{RerankedCandidate, SearchTuning};
use anamnesis::query::{
    ContextPackage, Fragment, QueryConfig, ScoredNode, SearchResult, assemble_context_package,
};
use serde::{Deserialize, Serialize};

use super::super::dataset::BenchQuestion;
use super::super::error::{BenchError, BenchResult};
use super::super::metrics::{RankedRetrieval, RetrievalMetrics, first_hit_rank, retrieval_metrics};
use super::BuiltMemoryGraph;

/// Knobs for warmup/evaluation runs, bundled to keep call sites readable.
#[derive(Clone, Default)]
pub struct EvalOptions {
    pub top_k: usize,
    pub seed_limit: Option<usize>,
    pub dump_features: bool,
    /// Inject speaker entity-tag cues parsed from the question text.
    /// Default OFF: with a single speaker tag matching ~half a conversation,
    /// the entity channel floods seed fusion with arbitrary same-speaker
    /// turns (measured −21pp Recall@20 on LoCoMo).
    pub speaker_cues: bool,
    /// Benchmark-only shadow candidate: re-rank the live top-200 readout with
    /// reciprocal-rank fusion before packaging. This is deliberately not an
    /// engine policy; it exists to require answer-quality evidence before any
    /// product-level scoring change is proposed.
    pub shadow_rank_fusion: bool,
    /// Optional local cross-encoder used only on the benchmark's live readout
    /// surface. The engine remains model-agnostic and unchanged.
    pub shadow_cross_encoder: Option<Arc<fastembed::TextRerank>>,
    /// Number of cognitive readout candidates exposed to the shadow
    /// cross-encoder.
    pub shadow_cross_encoder_candidates: usize,
}

const SHADOW_RRF_DAMPING: f64 = 60.0;
const SHADOW_RRF_EMBEDDING_WEIGHT: f64 = 0.25;
const SHADOW_RRF_TEXT_WEIGHT: f64 = 1.0;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmupReport {
    pub questions: usize,
    pub sites_accessed: usize,
    pub paths_strengthened: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadoutFeatureRow {
    pub question_id: String,
    pub question_type: String,
    pub sample_index: usize,
    pub rank: usize,
    pub node_id: u64,
    /// Gold relevance of this node's provenance (independent per node — no
    /// novelty dedup, unlike the metric surface).
    pub label: bool,
    /// Raw gold units matched by this node's provenance (NO cross-row novelty
    /// dedup — the fit tool replays dedup in rank order, mirroring metrics.rs).
    pub matched_units: Vec<String>,
    /// Total relevant gold units for the question (denominator for recall/NDCG).
    pub total_relevant: usize,
    pub activation: f64,
    pub phi: f64,
    pub salience: f64,
    pub impedance: f64,
    pub scope_weight: f64,
    pub trust_weight: f64,
    pub stress: f64,
    /// Raw query/node embedding cosine from the readout trace.
    pub embedding_cosine: f64,
    /// Raw FTS/BM25 score for the exact benchmark question and node.
    pub text_score: f64,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<ReadoutFeatureRow>,
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

/// Product-shaped context emitted by one `Memory` search.
///
/// The answer benchmark persists this surface so an answer failure can be
/// separated into retrieval, reading, and judging failures after the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerContext {
    pub evidence: Vec<AnswerEvidence>,
    pub context_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerEvidence {
    pub rank: usize,
    pub node_id: u64,
    pub score: f64,
    pub text: String,
    pub session_id: Option<String>,
    pub raw_session_id: Option<String>,
    pub raw_turn_id: Option<String>,
    pub relevant: bool,
    pub matched_gold_units: Vec<String>,
}

pub fn run_warmup(
    graph: &mut BuiltMemoryGraph,
    questions: &[BenchQuestion],
    opts: &EvalOptions,
) -> BenchResult<WarmupReport> {
    let mut report = WarmupReport::default();
    for question in questions {
        let result = search_question(graph, question, opts)?;
        // Commit through the framework path so the warmup measures the shipped
        // reinforcement semantics (confidence default lives in Memory::used).
        let commit = graph
            .memory
            .used(anamnesis::memory::Recall {
                hits: Vec::new(),
                package: result.package,
            })
            .map_err(|err| BenchError::Engine(err.to_string()))?;
        report.questions += 1;
        report.sites_accessed += commit.sites_accessed;
        report.paths_strengthened += commit.paths_strengthened;
    }
    Ok(report)
}

pub fn evaluate_questions(
    graph: &mut BuiltMemoryGraph,
    questions: &[BenchQuestion],
    opts: &EvalOptions,
) -> BenchResult<Vec<QuestionEvaluation>> {
    questions
        .iter()
        .map(|question| {
            evaluate_question_with_context(graph, question, opts).map(|(evaluation, _)| evaluation)
        })
        .collect()
}

pub fn evaluate_question_with_context(
    graph: &mut BuiltMemoryGraph,
    question: &BenchQuestion,
    opts: &EvalOptions,
) -> BenchResult<(QuestionEvaluation, AnswerContext)> {
    let start = Instant::now();
    let result = search_question(graph, question, opts)?;
    let shadow = if let Some(reranker) = &opts.shadow_cross_encoder {
        Some(shadow_cross_encoder(
            &result,
            graph,
            question,
            reranker,
            opts.shadow_cross_encoder_candidates,
            opts.top_k,
        )?)
    } else {
        opts.shadow_rank_fusion
            .then(|| shadow_rank_fusion(&result, graph, question))
    };
    let search_latency_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Primary surface: pre-package readout candidates
    let retrievals = if let Some((ranked, _)) = &shadow {
        build_retrievals(ranked.iter().copied(), graph, question, opts.top_k)
    } else {
        readout_retrievals(&result.trace.readout, graph, question, opts.top_k)
    };
    let readout_ranked: Vec<_> = retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    // Package surface: packaged ContextPackage fragments
    let package = shadow
        .as_ref()
        .map_or(&result.package, |(_, package)| package);
    let package_retrievals = retrieved_memories(package, graph, question, opts.top_k);
    let package_ranked: Vec<_> = package_retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    let total_relevant = question.gold.total_relevant_units();
    let returned_fragments = package.total_fragments();

    let text_scores: HashMap<_, _> = if opts.dump_features {
        graph
            .memory
            .engine()
            .graph()
            .storage()
            .text_search(&question.question, 200)
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
    let features = if opts.dump_features {
        result
            .trace
            .readout
            .iter()
            .enumerate()
            .take(200)
            .filter_map(|(index, candidate)| {
                let provenance = graph.provenance_by_node.get(&candidate.node_id)?;
                let matched_units = question.gold.matched_units(
                    &provenance.raw_session_id,
                    provenance.raw_turn_id.as_deref(),
                    &provenance.content,
                );
                let label = !matched_units.is_empty();
                Some(ReadoutFeatureRow {
                    question_id: question.question_id.clone(),
                    question_type: question.question_type.clone(),
                    sample_index: question.sample_index,
                    rank: index + 1,
                    node_id: candidate.node_id.0,
                    label,
                    matched_units,
                    total_relevant,
                    activation: candidate.activation,
                    phi: candidate.phi,
                    salience: candidate.salience,
                    impedance: candidate.impedance,
                    scope_weight: candidate.scope_weight,
                    trust_weight: candidate.trust_weight,
                    stress: candidate.stress,
                    embedding_cosine: candidate.embedding_cosine,
                    text_score: text_scores.get(&candidate.node_id).copied().unwrap_or(0.0),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let evaluation = QuestionEvaluation {
        question_id: question.question_id.clone(),
        question_type: question.question_type.clone(),
        sample_index: question.sample_index,
        search_latency_ms,
        total_relevant,
        retrieval_metrics: retrieval_metrics(&readout_ranked, total_relevant, opts.top_k),
        package_metrics: retrieval_metrics(&package_ranked, total_relevant, opts.top_k),
        first_hit_rank: first_hit_rank(&readout_ranked),
        returned_fragments,
        retrievals,
        features,
    };
    let context = answer_context(package, graph, question, opts.top_k);
    Ok((evaluation, context))
}

fn shadow_rank_fusion(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
) -> (Vec<(anamnesis::graph::NodeId, f64)>, ContextPackage) {
    let candidates: Vec<_> = result.trace.readout.iter().take(200).collect();
    let mut embedding_order: Vec<usize> = (0..candidates.len()).collect();
    embedding_order.sort_by(|left, right| {
        candidates[*right]
            .embedding_cosine
            .total_cmp(&candidates[*left].embedding_cosine)
            .then_with(|| left.cmp(right))
    });
    let mut embedding_ranks = vec![0usize; candidates.len()];
    for (rank, index) in embedding_order.into_iter().enumerate() {
        embedding_ranks[index] = rank + 1;
    }

    let text_scores: HashMap<_, _> = graph
        .memory
        .engine()
        .graph()
        .storage()
        .text_search(&question.question, 200)
        .into_iter()
        .collect();
    let mut text_order: Vec<usize> = (0..candidates.len())
        .filter(|index| {
            text_scores
                .get(&candidates[*index].node_id)
                .is_some_and(|score| *score > 0.0)
        })
        .collect();
    text_order.sort_by(|left, right| {
        let left_score = text_scores
            .get(&candidates[*left].node_id)
            .copied()
            .unwrap_or(0.0);
        let right_score = text_scores
            .get(&candidates[*right].node_id)
            .copied()
            .unwrap_or(0.0);
        right_score
            .total_cmp(&left_score)
            .then_with(|| left.cmp(right))
    });
    let mut text_ranks = HashMap::new();
    for (rank, index) in text_order.into_iter().enumerate() {
        text_ranks.insert(index, rank + 1);
    }

    let mut ranked: Vec<_> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let cognitive = 1.0 / (SHADOW_RRF_DAMPING + (index + 1) as f64);
            let embedding = if candidate.embedding_cosine > 0.0 {
                SHADOW_RRF_EMBEDDING_WEIGHT / (SHADOW_RRF_DAMPING + embedding_ranks[index] as f64)
            } else {
                0.0
            };
            let text = text_ranks.get(&index).map_or(0.0, |rank| {
                SHADOW_RRF_TEXT_WEIGHT / (SHADOW_RRF_DAMPING + *rank as f64)
            });
            (candidate.node_id, cognitive + embedding + text, index)
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.2.cmp(&right.2))
    });

    let storage = graph.memory.engine().graph().storage();
    let scored_nodes = ranked
        .iter()
        .filter_map(|(node_id, score, _)| {
            let node = storage.get_node(*node_id).ok()?;
            Some(ScoredNode {
                node_id: *node_id,
                name: node.name.clone(),
                summary: node.summary.clone(),
                content: node.content.clone(),
                node_type: node.node_type.clone(),
                relevance: *score,
                origin: node.origin.clone(),
            })
        })
        .collect();
    let config = QueryConfig::default();
    let package = assemble_context_package(
        scored_nodes,
        &[],
        &[],
        config.token_budget,
        config.chars_per_token,
    );
    (
        ranked
            .into_iter()
            .map(|(node_id, score, _)| (node_id, score))
            .collect(),
        package,
    )
}

fn shadow_cross_encoder(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    reranker: &fastembed::TextRerank,
    candidate_limit: usize,
    final_limit: usize,
) -> BenchResult<(Vec<(anamnesis::graph::NodeId, f64)>, ContextPackage)> {
    let storage = graph.memory.engine().graph().storage();
    let candidates: Vec<_> = result
        .trace
        .readout
        .iter()
        .take(candidate_limit)
        .filter_map(|candidate| {
            storage
                .get_node(candidate.node_id)
                .ok()
                .map(|node| (candidate, node))
        })
        .collect();
    let documents: Vec<_> = candidates
        .iter()
        .map(|(_, node)| node.content.clone())
        .collect();
    let reranked = reranker
        .rerank(question.question.clone(), documents, false, Some(32))
        .map_err(|err| BenchError::Embedding(format!("cross-encoder rerank failed: {err}")))?;

    let ranked: Vec<_> = reranked
        .iter()
        .filter_map(|item| {
            let (candidate, _) = candidates.get(item.index)?;
            Some((candidate.node_id, f64::from(item.score)))
        })
        .collect();
    let consumer_ranking: Vec<_> = ranked
        .iter()
        .map(|(node_id, score)| RerankedCandidate {
            node_id: *node_id,
            score: *score,
        })
        .collect();
    let recall = graph
        .memory
        .repackage_reranked(result, &consumer_ranking, final_limit)
        .map_err(|err| BenchError::Engine(err.to_string()))?;
    Ok((ranked, recall.package))
}

fn search_question(
    graph: &mut BuiltMemoryGraph,
    question: &BenchQuestion,
    opts: &EvalOptions,
) -> BenchResult<SearchResult> {
    let now = question
        .question_date
        .map(Timestamp)
        .unwrap_or(Timestamp(0));
    let tuning = SearchTuning {
        seed_limit: opts.seed_limit,
        entity_tags: if opts.speaker_cues {
            super::speaker_cue_tags(&graph.speakers, &question.question)
        } else {
            vec![]
        },
    };
    graph
        .memory
        .search_result_at_with(&question.question, opts.top_k, now, &tuning)
        .map_err(|err| BenchError::Engine(err.to_string()))
}

fn readout_retrievals(
    readout: &[anamnesis::query::ReadoutCandidate],
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
) -> Vec<RetrievedMemory> {
    build_retrievals(
        readout.iter().map(|c| (c.node_id, c.score)),
        graph,
        question,
        top_k,
    )
}

fn retrieved_memories(
    package: &ContextPackage,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
) -> Vec<RetrievedMemory> {
    build_retrievals(
        ranked_fragments(package)
            .into_iter()
            .map(|f| (f.node_id, f.relevance)),
        graph,
        question,
        top_k,
    )
}

fn answer_context(
    package: &ContextPackage,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
) -> AnswerContext {
    let mut seen_units = HashSet::new();
    let evidence = ranked_fragments(package)
        .into_iter()
        .take(top_k)
        .enumerate()
        .map(|(index, fragment)| {
            let provenance = graph.provenance_by_node.get(&fragment.node_id);
            let matched_gold_units: Vec<_> = provenance
                .map(|provenance| {
                    question.gold.matched_units(
                        &provenance.raw_session_id,
                        provenance.raw_turn_id.as_deref(),
                        &provenance.content,
                    )
                })
                .unwrap_or_default()
                .into_iter()
                .filter(|unit| seen_units.insert(unit.clone()))
                .collect();
            AnswerEvidence {
                rank: index + 1,
                node_id: fragment.node_id.0,
                score: fragment.relevance,
                text: fragment_text(&fragment),
                session_id: provenance.map(|value| value.session_id.clone()),
                raw_session_id: provenance.map(|value| value.raw_session_id.clone()),
                raw_turn_id: provenance.and_then(|value| value.raw_turn_id.clone()),
                relevant: !matched_gold_units.is_empty(),
                matched_gold_units,
            }
        })
        .collect();
    AnswerContext {
        evidence,
        context_tokens: package.token_usage.used,
    }
}

fn build_retrievals(
    ranked: impl Iterator<Item = (anamnesis::graph::NodeId, f64)>,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
) -> Vec<RetrievedMemory> {
    let mut seen_units = HashSet::new();
    ranked
        .take(top_k)
        .enumerate()
        .filter_map(|(index, (node_id, score))| {
            let provenance = graph.provenance_by_node.get(&node_id)?;
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
                node_id: node_id.0,
                relevant,
                matched_gold_units,
                score,
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

fn fragment_text(fragment: &Fragment) -> String {
    fragment
        .content
        .as_ref()
        .or(fragment.summary.as_ref())
        .unwrap_or(&fragment.name)
        .clone()
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
