use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anamnesis::engine::{AtomicFactId, RerankingProvider, SearchDiagnostics, StorageAdapter};
use anamnesis::graph::{NodeId, Timestamp};
use anamnesis::memory::{
    ContextRenderOptions, ContextRenderStyle, DeepRecallOptions, EvidenceSelection, Recall,
    RecallIntent, RecallPlan, RerankedCandidate, RerankedRecallOptions, SearchTuning,
};
use anamnesis::query::{ContextPackage, Fragment, SearchResult};
use serde::{Deserialize, Serialize};

use super::super::dataset::{BenchQuestion, RetrievalInput};
use super::super::error::{BenchError, BenchResult};
use super::super::metrics::{RankedRetrieval, RetrievalMetrics, first_hit_rank, retrieval_metrics};
use super::BuiltMemoryGraph;

type ConsumerRanking = Vec<(NodeId, f64)>;
type FrozenConsumerRankings = Arc<HashMap<String, ConsumerRanking>>;
// The boolean records that the package already passed canonical `Memory`
// deep selection. The raw ranking remains separate for reranker metrics and
// fixed-ranking diagnostic screens.
type ConsumerPackage = (ConsumerRanking, ContextPackage, bool);

/// Knobs for warmup/evaluation runs, bundled to keep call sites readable.
#[derive(Clone)]
pub struct EvalOptions {
    pub top_k: usize,
    pub seed_limit: Option<usize>,
    pub dump_features: bool,
    /// Optional local cross-encoder supplied to the canonical
    /// `Memory::rerank_search_result_at` product path.
    #[cfg(feature = "embed")]
    pub consumer_cross_encoder: Option<Arc<dyn RerankingProvider>>,
    /// Frozen deterministic-reranker scores keyed by question id. Replay still
    /// runs live core search, validates every node against that question's
    /// readout, and repackages through the product API.
    pub replayed_consumer_rankings: Option<FrozenConsumerRankings>,
    /// Number of cognitive readout candidates exposed to canonical local
    /// reranking and scored by candidate-pool metrics.
    pub consumer_candidate_k: usize,
    /// Additional final cutoffs to repackage from the exact same consumer
    /// ranking. Diagnostic-only: the primary product route still uses
    /// `top_k`.
    pub screen_top_k: Vec<usize>,
    /// Override only the number of diagnostic readout rows retained. Product
    /// behavior remains at the default trace bound when absent.
    pub diagnostic_readout_limit: Option<usize>,
    /// Product-API final evidence selection applied after consumer ranking.
    pub consumer_selection_policy: ConsumerSelectionPolicy,
    /// Product context presentation used for answer generation.
    pub context_render_style: ContextRenderStyle,
}

impl Default for EvalOptions {
    fn default() -> Self {
        Self {
            top_k: 20,
            seed_limit: None,
            dump_features: false,
            #[cfg(feature = "embed")]
            consumer_cross_encoder: None,
            replayed_consumer_rankings: None,
            consumer_candidate_k: 100,
            screen_top_k: Vec::new(),
            diagnostic_readout_limit: None,
            consumer_selection_policy: ConsumerSelectionPolicy::MemoryDeep,
            context_render_style: ContextRenderStyle::Detailed,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsumerSelectionPolicy {
    Relevance,
    /// Delegate deterministic intent detection and source-aware evidence
    /// compilation to the public `Memory` product API.
    #[default]
    MemoryDeep,
    /// Delegate exact canonical-source deduplication to `Memory`.
    MemoryDistinctSources,
    /// Delegate greedy canonical-source coverage to `Memory`.
    MemorySourceCoverage,
}

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
    /// Rank after the configured consumer reranker, when present.
    pub consumer_rank: Option<usize>,
    pub consumer_score: Option<f64>,
    pub node_id: u64,
    /// Gold relevance of this node's provenance (independent per node — no
    /// novelty dedup, unlike the metric surface).
    pub label: bool,
    /// Raw gold units matched by this node's provenance. Cross-row novelty
    /// deduplication is applied only when computing the ordered metric surface.
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
pub struct AtomicRouteFeatureRow {
    pub rank: usize,
    pub source_node_id: u64,
    pub kind_priority: usize,
    pub fact_id: u64,
    pub source_session_id: String,
    pub content: String,
    pub subject: Option<String>,
    pub relation: Option<String>,
    pub object: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionEvaluation {
    pub question_id: String,
    pub question_type: String,
    /// Sample (conversation/haystack) this question belongs to — needed for
    /// train/dev split comparisons (even = train, odd = dev).
    pub sample_index: usize,
    /// Query-to-packaged-evidence latency. This includes query embedding,
    /// retrieval, consumer reranking, selection, and `ContextPackage`
    /// assembly, but excludes final string rendering.
    pub search_latency_ms: f64,
    /// Time spent turning the packaged evidence into the exact product context
    /// string supplied to a reader.
    #[serde(default)]
    pub context_render_latency_ms: f64,
    /// Product memory latency through a reader-ready context string. This is
    /// `search_latency_ms + context_render_latency_ms`; it still excludes any
    /// consumer-owned prompt wrapper, tokenization, and model generation.
    #[serde(default)]
    pub context_ready_latency_ms: f64,
    pub total_relevant: usize,
    /// Gold coverage available anywhere in the first-stage cognitive candidate
    /// pool offered to the consumer reranker.
    pub candidate_metrics: RetrievalMetrics,
    /// Gold coverage after consumer ranking at the final selection cutoff.
    pub reranker_metrics: RetrievalMetrics,
    /// Gold coverage in the fragments actually delivered after packaging,
    /// validity filtering, mode-specific assembly, and result limiting.
    pub delivered_metrics: RetrievalMetrics,
    /// Exact gold-unit coverage visible in the final product-rendered string.
    /// This catches L1/name/summary resolution where provenance was selected
    /// but the source turn body was not actually exposed to the reader.
    pub rendered_recall: f64,
    pub rendered_hit: bool,
    pub rendered_matched_gold_units: Vec<String>,
    pub candidate_first_hit_rank: Option<usize>,
    pub reranker_first_hit_rank: Option<usize>,
    pub candidate_k: usize,
    pub reranker_k: usize,
    pub delivered_k: usize,
    pub candidate_returned: usize,
    pub reranker_returned: usize,
    pub delivered_fragments: usize,
    /// Final consumer-ranked selection surface. Candidate-pool diagnostics live
    /// in `features` when explicitly requested; delivered evidence lives in the
    /// paired [`AnswerContext`].
    pub reranker_retrievals: Vec<RetrievedMemory>,
    /// Fixed-ranking cutoff screens. Every entry reuses the exact candidate
    /// order produced for this question, varying only final selection and
    /// product packaging.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selection_variants: BTreeMap<String, SelectionVariantEvaluation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<ReadoutFeatureRow>,
    /// Query-ranked atomic facts that the production `Memory` path routed back
    /// to raw sources. Persisted only with `--dump-candidate-pool`; never used
    /// for selection or scoring.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub atomic_route_features: Vec<AtomicRouteFeatureRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionVariantEvaluation {
    pub selection_k: usize,
    pub selected_metrics: RetrievalMetrics,
    pub delivered_metrics: RetrievalMetrics,
    pub rendered_recall: f64,
    pub rendered_hit: bool,
    pub delivered_fragments: usize,
    pub context_tokens: usize,
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
    /// Exact product renderer output from [`Recall::as_context`].
    pub product_context: String,
    pub product_context_chars: usize,
    /// Canonical source nodes exposed by the production readout.
    ///
    /// These ids are reader input provenance, not relevance annotations.
    #[serde(default)]
    pub source_node_ids: Vec<u64>,
    /// Trusted visible source ownership from the same production readout.
    #[serde(default)]
    pub source_attributions: Vec<AnswerSourceAttribution>,
    pub evidence: Vec<AnswerEvidence>,
    pub context_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerSourceAttribution {
    pub source_node_id: u64,
    pub speaker: Option<String>,
    pub text: String,
    pub session_id: String,
    pub dialogue_block_node_id: u64,
    pub line_order: usize,
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
        let result = search_question(graph, question.retrieval_input(), opts)?;
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
    let retrieval = question.retrieval_input();
    let start = Instant::now();
    let result = search_question(graph, retrieval, opts)?;
    #[cfg(feature = "embed")]
    let needs_live_document_rerank = matches!(
        RecallPlan::infer(retrieval.question).recall_intent,
        RecallIntent::Enumeration | RecallIntent::Relational
    );
    #[cfg(feature = "embed")]
    let consumer_package = if let Some(rankings) = &opts.replayed_consumer_rankings
        && !needs_live_document_rerank
    {
        Some(replay_consumer_ranking(
            &result, graph, retrieval, rankings, opts.top_k,
        )?)
    } else if let Some(reranker) = &opts.consumer_cross_encoder {
        Some(consumer_cross_encoder_package(
            &result,
            graph,
            retrieval,
            reranker.as_ref(),
            opts.consumer_candidate_k,
            opts.top_k,
        )?)
    } else {
        None
    };
    #[cfg(not(feature = "embed"))]
    let consumer_package = if let Some(rankings) = &opts.replayed_consumer_rankings {
        Some(replay_consumer_ranking(
            &result, graph, retrieval, rankings, opts.top_k,
        )?)
    } else {
        None
    };
    let consumer_package =
        apply_consumer_selection_policy(&result, consumer_package, graph, retrieval, opts)?;
    let search_latency_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Candidate surface: cognitive readout actually exposed to the consumer
    // reranker. This measures whether later stages ever had a chance to recover
    // the annotated evidence.
    let candidate_cutoff = opts.consumer_candidate_k;
    let candidate_retrievals =
        readout_retrievals(&result.trace.readout, graph, question, candidate_cutoff);
    let candidate_ranked: Vec<_> = candidate_retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    // Reranker/selection surface: final ranking cutoff before package assembly.
    let reranker_retrievals = if let Some((ranked, _, _)) = &consumer_package {
        build_retrievals(ranked.iter().copied(), graph, question, opts.top_k)
    } else {
        readout_retrievals(&result.trace.readout, graph, question, opts.top_k)
    };
    let reranker_ranked: Vec<_> = reranker_retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    // Package surface: packaged ContextPackage fragments
    let package = consumer_package
        .as_ref()
        .map_or(&result.package, |(_, package, _)| package);
    let delivered_retrievals = retrieved_memories(package, graph, question, opts.top_k);
    let delivered_ranked: Vec<_> = delivered_retrievals
        .iter()
        .map(|item| RankedRetrieval {
            matched_gold_units: item.matched_gold_units.clone(),
            score: item.score,
        })
        .collect();

    let total_relevant = question.gold.total_relevant_units();
    let delivered_fragments = package.total_fragments();

    let text_scores: HashMap<_, _> = if opts.dump_features {
        graph
            .memory
            .engine()
            .graph()
            .storage()
            .text_search(retrieval.question, 200)
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
    let consumer_positions: HashMap<_, _> = consumer_package
        .as_ref()
        .map(|(ranking, _, _)| {
            ranking
                .iter()
                .enumerate()
                .map(|(index, (node_id, score))| (*node_id, (index + 1, *score)))
                .collect()
        })
        .unwrap_or_default();
    let features = if opts.dump_features {
        result
            .trace
            .readout
            .iter()
            .enumerate()
            .take(opts.diagnostic_readout_limit.unwrap_or(200))
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
                    consumer_rank: consumer_positions
                        .get(&candidate.node_id)
                        .map(|(rank, _)| *rank),
                    consumer_score: consumer_positions
                        .get(&candidate.node_id)
                        .map(|(_, score)| *score),
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
    let atomic_route_features = if opts.dump_features {
        atomic_route_features(&result, graph)?
    } else {
        Vec::new()
    };

    let context_render_start = Instant::now();
    let (product_context, source_node_ids, source_attributions) = render_product_context(
        package,
        graph,
        retrieval,
        opts.context_render_style,
        opts.consumer_selection_policy == ConsumerSelectionPolicy::MemoryDeep,
    )?;
    let context = answer_context(
        package,
        graph,
        question,
        opts.top_k,
        product_context,
        source_node_ids,
        source_attributions,
    );
    let context_render_latency_ms = context_render_start.elapsed().as_secs_f64() * 1000.0;
    let context_ready_latency_ms = search_latency_ms + context_render_latency_ms;
    let rendered_matched_gold_units =
        rendered_gold_units(&context.product_context, graph, question);
    let rendered_recall = if total_relevant == 0 {
        0.0
    } else {
        rendered_matched_gold_units.len() as f64 / total_relevant as f64
    };
    let selection_variants = fixed_ranking_selection_variants(
        &result,
        consumer_package
            .as_ref()
            .map(|(ranking, _, _)| ranking.as_slice()),
        graph,
        question,
        opts,
    )?;
    let evaluation = QuestionEvaluation {
        question_id: question.question_id.clone(),
        question_type: question.question_type.clone(),
        sample_index: question.sample_index,
        search_latency_ms,
        context_render_latency_ms,
        context_ready_latency_ms,
        total_relevant,
        candidate_metrics: retrieval_metrics(&candidate_ranked, total_relevant, candidate_cutoff),
        reranker_metrics: retrieval_metrics(&reranker_ranked, total_relevant, opts.top_k),
        delivered_metrics: retrieval_metrics(&delivered_ranked, total_relevant, opts.top_k),
        rendered_recall,
        rendered_hit: !rendered_matched_gold_units.is_empty(),
        rendered_matched_gold_units,
        candidate_first_hit_rank: first_hit_rank(&candidate_ranked),
        reranker_first_hit_rank: first_hit_rank(&reranker_ranked),
        candidate_k: candidate_cutoff,
        reranker_k: opts.top_k,
        delivered_k: opts.top_k,
        candidate_returned: candidate_retrievals.len(),
        reranker_returned: reranker_retrievals.len(),
        delivered_fragments,
        reranker_retrievals,
        selection_variants,
        features,
        atomic_route_features,
    };
    Ok((evaluation, context))
}

fn atomic_route_features(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
) -> BenchResult<Vec<AtomicRouteFeatureRow>> {
    let mut rows = Vec::new();
    for strategy in &result.trace.strategies_used {
        let Some(encoded) = strategy.strip_prefix("atomic_fact_sources:") else {
            continue;
        };
        if encoded.is_empty() {
            continue;
        }
        for marker in encoded.split(',') {
            let mut fields = marker.split('@');
            let source_node_id = fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| {
                    BenchError::Parse(format!("invalid atomic source marker {marker}"))
                })?;
            let kind_priority = fields
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| {
                    BenchError::Parse(format!("invalid atomic source marker {marker}"))
                })?;
            let fact_id = fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| {
                    BenchError::Parse(format!("invalid atomic source marker {marker}"))
                })?;
            if fields.next().is_some() {
                return Err(BenchError::Parse(format!(
                    "invalid atomic source marker {marker}"
                )));
            }
            let fact = graph
                .memory
                .engine()
                .graph()
                .storage()
                .get_atomic_fact(AtomicFactId(fact_id))
                .map_err(|error| BenchError::Engine(error.to_string()))?;
            rows.push(AtomicRouteFeatureRow {
                rank: rows.len() + 1,
                source_node_id,
                kind_priority,
                fact_id,
                source_session_id: fact.source_session_id.clone(),
                content: fact.content.clone(),
                subject: fact.metadata.get("anamnesis:ground-subject").cloned(),
                relation: fact.metadata.get("anamnesis:ground-relation").cloned(),
                object: fact.metadata.get("anamnesis:ground-object").cloned(),
            });
        }
    }
    Ok(rows)
}

fn replay_consumer_ranking(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    retrieval: RetrievalInput<'_>,
    rankings: &HashMap<String, ConsumerRanking>,
    top_k: usize,
) -> BenchResult<ConsumerPackage> {
    let ranking = rankings.get(retrieval.question_id).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "replayed ranking is missing question {:?}",
            retrieval.question_id
        ))
    })?;
    let live_nodes: HashSet<_> = result
        .trace
        .readout
        .iter()
        .map(|candidate| candidate.node_id)
        .collect();
    let mut seen = HashSet::new();
    if ranking.is_empty()
        || ranking.iter().any(|(node_id, score)| {
            !score.is_finite() || !live_nodes.contains(node_id) || !seen.insert(*node_id)
        })
    {
        return Err(BenchError::InvalidInput(format!(
            "replayed ranking for {:?} is not a unique finite subset of live readout",
            retrieval.question_id
        )));
    }
    let candidates: Vec<_> = ranking
        .iter()
        .map(|(node_id, score)| RerankedCandidate {
            node_id: *node_id,
            score: *score,
        })
        .collect();
    let recall = graph
        .memory
        .repackage_reranked_at(result, &candidates, top_k, question_time(retrieval))
        .map_err(|error| BenchError::Engine(error.to_string()))?;
    Ok((ranking.clone(), recall.package, false))
}

fn apply_consumer_selection_policy(
    result: &SearchResult,
    consumer_package: Option<ConsumerPackage>,
    graph: &BuiltMemoryGraph,
    retrieval: RetrievalInput<'_>,
    opts: &EvalOptions,
) -> BenchResult<Option<ConsumerPackage>> {
    let (ranking, package, memory_deep_applied) = match consumer_package {
        Some(values) => values,
        None if opts.consumer_selection_policy == ConsumerSelectionPolicy::Relevance => {
            return Ok(None);
        }
        None => (
            result
                .trace
                .readout
                .iter()
                .map(|candidate| (candidate.node_id, candidate.score))
                .collect(),
            result.package.clone(),
            false,
        ),
    };
    if opts.consumer_selection_policy == ConsumerSelectionPolicy::MemoryDeep && memory_deep_applied
    {
        return Ok(Some((ranking, package, true)));
    }
    match opts.consumer_selection_policy {
        ConsumerSelectionPolicy::Relevance => {
            if !memory_deep_applied {
                return Ok(Some((ranking, package, false)));
            }
            let candidates: Vec<_> = ranking
                .iter()
                .map(|(node_id, score)| RerankedCandidate {
                    node_id: *node_id,
                    score: *score,
                })
                .collect();
            let recall = graph
                .memory
                .repackage_reranked_at(result, &candidates, opts.top_k, question_time(retrieval))
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            Ok(Some((ranking, recall.package, false)))
        }
        ConsumerSelectionPolicy::MemoryDeep
        | ConsumerSelectionPolicy::MemoryDistinctSources
        | ConsumerSelectionPolicy::MemorySourceCoverage => {
            let selection = match opts.consumer_selection_policy {
                ConsumerSelectionPolicy::MemoryDeep => EvidenceSelection::Auto,
                ConsumerSelectionPolicy::MemoryDistinctSources => {
                    EvidenceSelection::DistinctSources
                }
                ConsumerSelectionPolicy::MemorySourceCoverage => EvidenceSelection::SourceCoverage,
                _ => unreachable!("matched memory-owned selection policies"),
            };
            let candidates: Vec<_> = ranking
                .iter()
                .map(|(node_id, score)| RerankedCandidate {
                    node_id: *node_id,
                    score: *score,
                })
                .collect();
            let recall = graph
                .memory
                .repackage_reranked_deep_at(
                    retrieval.question,
                    result,
                    &candidates,
                    DeepRecallOptions::new(opts.top_k).with_selection(selection),
                    question_time(retrieval),
                )
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            let compiled_ranking = recall
                .hits
                .iter()
                .map(|hit| (hit.node_id, hit.score))
                .collect();
            Ok(Some((compiled_ranking, recall.package, true)))
        }
    }
}

fn fixed_ranking_selection_variants(
    result: &SearchResult,
    consumer_ranking: Option<&[(anamnesis::graph::NodeId, f64)]>,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    opts: &EvalOptions,
) -> BenchResult<BTreeMap<String, SelectionVariantEvaluation>> {
    let retrieval = question.retrieval_input();
    let mut cutoffs = opts.screen_top_k.clone();
    cutoffs.sort_unstable();
    cutoffs.dedup();
    if cutoffs.is_empty() {
        return Ok(BTreeMap::new());
    }

    let ranking: Vec<_> = consumer_ranking.map_or_else(
        || {
            result
                .trace
                .readout
                .iter()
                .map(|candidate| (candidate.node_id, candidate.score))
                .collect()
        },
        |values| values.to_vec(),
    );
    let total_relevant = question.gold.total_relevant_units();
    let mut variants = BTreeMap::new();

    let reranked_candidates: Vec<_> = ranking
        .iter()
        .map(|(node_id, score)| RerankedCandidate {
            node_id: *node_id,
            score: *score,
        })
        .collect();
    for &selection_k in &cutoffs {
        let recall = graph
            .memory
            .repackage_reranked_at(
                result,
                &reranked_candidates,
                selection_k,
                question_time(retrieval),
            )
            .map_err(|err| BenchError::Engine(err.to_string()))?;
        let selected = build_retrievals(ranking.iter().copied(), graph, question, selection_k);
        let selected_ranked: Vec<_> = selected
            .iter()
            .map(|item| RankedRetrieval {
                matched_gold_units: item.matched_gold_units.clone(),
                score: item.score,
            })
            .collect();
        let delivered = retrieved_memories(&recall.package, graph, question, selection_k);
        let delivered_ranked: Vec<_> = delivered
            .iter()
            .map(|item| RankedRetrieval {
                matched_gold_units: item.matched_gold_units.clone(),
                score: item.score,
            })
            .collect();
        let (product_context, source_node_ids, source_attributions) = render_product_context(
            &recall.package,
            graph,
            retrieval,
            opts.context_render_style,
            opts.consumer_selection_policy == ConsumerSelectionPolicy::MemoryDeep,
        )?;
        let context = answer_context(
            &recall.package,
            graph,
            question,
            selection_k,
            product_context,
            source_node_ids,
            source_attributions,
        );
        let rendered_units = rendered_gold_units(&context.product_context, graph, question);
        let rendered_recall = if total_relevant == 0 {
            0.0
        } else {
            rendered_units.len() as f64 / total_relevant as f64
        };
        variants.insert(
            format!("top-{selection_k}"),
            SelectionVariantEvaluation {
                selection_k,
                selected_metrics: retrieval_metrics(&selected_ranked, total_relevant, selection_k),
                delivered_metrics: retrieval_metrics(
                    &delivered_ranked,
                    total_relevant,
                    selection_k,
                ),
                rendered_recall,
                rendered_hit: !rendered_units.is_empty(),
                delivered_fragments: recall.package.total_fragments(),
                context_tokens: context.context_tokens,
            },
        );
    }
    Ok(variants)
}

#[cfg(feature = "embed")]
fn consumer_cross_encoder_package(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    retrieval: RetrievalInput<'_>,
    reranker: &dyn RerankingProvider,
    candidate_limit: usize,
    final_limit: usize,
) -> BenchResult<ConsumerPackage> {
    let reranked = graph
        .memory
        .rerank_search_result_at(
            retrieval.question,
            result,
            reranker,
            RerankedRecallOptions::new(final_limit).with_candidate_limit(candidate_limit),
            question_time(retrieval),
        )
        .map_err(|err| BenchError::Engine(err.to_string()))?;
    let ranking = reranked
        .ranking
        .iter()
        .map(|candidate| (candidate.node_id, candidate.score))
        .collect();
    Ok((ranking, reranked.recall.package, true))
}

fn search_question(
    graph: &mut BuiltMemoryGraph,
    retrieval: RetrievalInput<'_>,
    opts: &EvalOptions,
) -> BenchResult<SearchResult> {
    let now = question_time(retrieval);
    let tuning = SearchTuning {
        seed_limit: opts.seed_limit,
        entity_tags: Vec::new(),
    };
    #[cfg(feature = "embed")]
    let readout_limit = opts.diagnostic_readout_limit.unwrap_or(200).max(
        opts.consumer_cross_encoder
            .as_ref()
            .map_or(200, |_| opts.consumer_candidate_k),
    );
    #[cfg(not(feature = "embed"))]
    let readout_limit = opts.diagnostic_readout_limit.unwrap_or(200);
    let search_limit = opts
        .top_k
        .max(anamnesis::memory::DEFAULT_RERANK_SEARCH_LIMIT);
    if readout_limit != 200 {
        graph
            .memory
            .search_result_at_with_diagnostics(
                retrieval.question,
                search_limit,
                now,
                &tuning,
                &SearchDiagnostics::with_readout_trace_limit(readout_limit),
            )
            .map_err(|err| BenchError::Engine(err.to_string()))
    } else {
        graph
            .memory
            .search_result_at_with(retrieval.question, search_limit, now, &tuning)
            .map_err(|err| BenchError::Engine(err.to_string()))
    }
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

fn render_product_context(
    package: &ContextPackage,
    graph: &BuiltMemoryGraph,
    retrieval: RetrievalInput<'_>,
    render_style: ContextRenderStyle,
    query_aware_context: bool,
) -> BenchResult<(String, Vec<u64>, Vec<AnswerSourceAttribution>)> {
    let recall = Recall {
        hits: Vec::new(),
        package: package.clone(),
    };
    let readout = graph
        .memory
        .readout_for(retrieval.question, &recall)
        .map_err(|err| BenchError::Engine(err.to_string()))?;
    let source_node_ids = readout
        .source_node_ids
        .into_iter()
        .map(|source_id| source_id.0)
        .collect();
    let source_attributions = readout
        .source_attributions
        .into_iter()
        .map(|source| AnswerSourceAttribution {
            source_node_id: source.source_node_id.0,
            speaker: source.speaker,
            text: source.text,
            session_id: source.session_id,
            dialogue_block_node_id: source.dialogue_block_node_id.0,
            line_order: source.line_order,
        })
        .collect();
    let render_options = ContextRenderOptions::with_style(render_style);
    let product_context = if query_aware_context {
        graph
            .memory
            .render_context_for_with(retrieval.question, &recall, render_options)
    } else {
        graph.memory.render_context_with(&recall, render_options)
    }
    .map_err(|err| BenchError::Engine(err.to_string()))?;
    Ok((product_context, source_node_ids, source_attributions))
}

fn answer_context(
    package: &ContextPackage,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
    product_context: String,
    source_node_ids: Vec<u64>,
    source_attributions: Vec<AnswerSourceAttribution>,
) -> AnswerContext {
    let product_context_chars = product_context.chars().count();
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
        product_context,
        product_context_chars,
        source_node_ids,
        source_attributions,
        evidence,
        context_tokens: package.token_usage.used,
    }
}

fn question_time(retrieval: RetrievalInput<'_>) -> Timestamp {
    retrieval
        .question_date
        .map(|epoch_seconds| Timestamp(epoch_seconds.saturating_mul(1_000)))
        .unwrap_or(Timestamp(0))
}

fn rendered_gold_units(
    product_context: &str,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
) -> Vec<String> {
    let match_surface = strip_product_provenance_for_match(product_context);
    let normalized_context =
        super::super::super::locomo_pipeline::normalize_for_match(&match_surface);
    if !question.gold.evidence_turn_ids.is_empty() {
        return question
            .gold
            .evidence_turn_ids
            .iter()
            .filter(|turn_id| {
                graph.provenance_by_node.values().any(|provenance| {
                    provenance.raw_turn_id.as_ref() == Some(*turn_id) && {
                        let normalized_content =
                            super::super::super::locomo_pipeline::normalize_for_match(
                                &provenance.content,
                            );
                        !normalized_content.is_empty()
                            && normalized_context.contains(&normalized_content)
                    }
                })
            })
            .map(|turn_id| format!("turn:{turn_id}"))
            .collect();
    }
    let session_ids = if !question.gold.answer_session_ids.is_empty() {
        &question.gold.answer_session_ids
    } else {
        &question.gold.evidence_session_ids
    };
    if !session_ids.is_empty() {
        return session_ids
            .iter()
            .filter(|session_id| product_context.contains(&format!("session \"{session_id}\"")))
            .map(|session_id| format!("session:{session_id}"))
            .collect();
    }
    question
        .gold
        .answer_needles
        .iter()
        .filter(|needle| normalized_context.contains(*needle))
        .map(|needle| format!("answer:{needle}"))
        .collect()
}

fn strip_product_provenance_for_match(product_context: &str) -> String {
    let mut stripped = String::with_capacity(product_context.len());
    for (index, line) in product_context.lines().enumerate() {
        if index > 0 {
            stripped.push('\n');
        }
        let Some((prefix, marker_suffix)) = line.split_once("[turn-source=node:") else {
            stripped.push_str(line);
            continue;
        };
        let digit_count = marker_suffix
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        let after_digits = &marker_suffix[digit_count..];
        let Some(content) = (digit_count > 0)
            .then(|| after_digits.strip_prefix("] "))
            .flatten()
        else {
            stripped.push_str(line);
            continue;
        };
        stripped.push_str(prefix);
        stripped.push_str(content);
    }
    stripped
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_match_surface_ignores_turn_source_annotations() {
        let raw_turn = "Alpha: We visited the sanctuary.\nAlpha shared a photo of a rescue dog.";
        let rendered = "    [turn-source=node:1347] Alpha: We visited the sanctuary.\n\
                        [turn-source=node:1347] Alpha shared a photo of a rescue dog.";
        let stripped = strip_product_provenance_for_match(rendered);

        assert!(
            super::super::super::super::locomo_pipeline::normalize_for_match(&stripped).contains(
                &super::super::super::super::locomo_pipeline::normalize_for_match(raw_turn)
            )
        );
    }
}
