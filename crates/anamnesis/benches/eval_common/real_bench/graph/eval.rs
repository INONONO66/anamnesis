use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anamnesis::engine::{RerankingProvider, SearchDiagnostics, StorageAdapter};
use anamnesis::graph::{EdgeType, NodeId, Timestamp};
use anamnesis::memory::{
    ContextRenderOptions, ContextRenderStyle, DeepRecallOptions, Direction, EvidenceSelection,
    Recall, RecallIntent, RecallPlan, RerankedCandidate, RerankedRecallOptions, SearchTuning,
};
use anamnesis::query::{
    ContextPackage, Fragment, QueryConfig, ScoredNode, SearchResult, assemble_context_package,
};
use serde::{Deserialize, Serialize};

use super::super::dataset::BenchQuestion;
use super::super::error::{BenchError, BenchResult};
use super::super::metrics::{RankedRetrieval, RetrievalMetrics, first_hit_rank, retrieval_metrics};
use super::BuiltMemoryGraph;

type ConsumerRanking = Vec<(NodeId, f64)>;
type FrozenConsumerRankings = Arc<HashMap<String, ConsumerRanking>>;
type ConsumerPackage = (ConsumerRanking, ContextPackage);

/// Knobs for warmup/evaluation runs, bundled to keep call sites readable.
#[derive(Clone)]
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
    /// Optional local cross-encoder supplied to the canonical
    /// `Memory::rerank_search_result_at` product path.
    #[cfg(feature = "embed")]
    pub consumer_cross_encoder: Option<Arc<dyn RerankingProvider>>,
    /// Frozen consumer scores keyed by question id. This benchmark-only replay
    /// path still runs live core search and validates every node against that
    /// question's readout before product repackaging; it only avoids repeating
    /// an expensive deterministic cross-encoder.
    pub replayed_consumer_rankings: Option<FrozenConsumerRankings>,
    /// Optional fast first-stage cross-encoder. When present, it ranks the
    /// broad cognitive pool and only its top `consumer_prefilter_k` documents
    /// reach the quality reranker.
    #[cfg(feature = "embed")]
    pub consumer_prefilter_cross_encoder: Option<Arc<fastembed::TextRerank>>,
    /// Cascade width after the optional fast prefilter.
    pub consumer_prefilter_k: Option<usize>,
    /// Fuse the exact deterministic query variants used by core lexical
    /// retrieval at the optional fast prefilter. The quality reranker still
    /// sees the original complete question.
    pub consumer_prefilter_query_fusion: bool,
    /// Number of cognitive readout candidates exposed to canonical local
    /// reranking and scored by candidate-pool metrics.
    pub consumer_candidate_k: usize,
    /// Compile overlapping graph representations into canonical raw-evidence
    /// documents through `Memory` before the local cross-encoder scores them.
    /// This is the canonical product route; disabling it is an ablation.
    pub consumer_evidence_documents: bool,
    /// Additional final cutoffs to repackage from the exact same consumer
    /// ranking. Diagnostic-only: the primary product route still uses
    /// `top_k`.
    pub screen_top_k: Vec<usize>,
    /// Add a consumer-side screen that keeps the highest-ranked representation
    /// of each raw source turn and backfills from later candidates.
    pub screen_source_dedup: bool,
    /// Override only the number of diagnostic readout rows retained. Product
    /// behavior remains at the default trace bound when absent.
    pub diagnostic_readout_limit: Option<usize>,
    /// Consumer-owned final ordering policy applied after the model or shadow
    /// ranking and before core validation/packaging.
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
            speaker_cues: false,
            shadow_rank_fusion: false,
            #[cfg(feature = "embed")]
            consumer_cross_encoder: None,
            replayed_consumer_rankings: None,
            #[cfg(feature = "embed")]
            consumer_prefilter_cross_encoder: None,
            consumer_prefilter_k: None,
            consumer_prefilter_query_fusion: false,
            consumer_candidate_k: 100,
            consumer_evidence_documents: false,
            screen_top_k: Vec::new(),
            screen_source_dedup: false,
            diagnostic_readout_limit: None,
            consumer_selection_policy: ConsumerSelectionPolicy::Relevance,
            context_render_style: ContextRenderStyle::Detailed,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsumerSelectionPolicy {
    #[default]
    Relevance,
    /// Delegate deterministic intent detection and source-aware evidence
    /// compilation to the public `Memory` product API.
    MemoryDeep,
    /// Delegate exact canonical-source deduplication to `Memory`.
    MemoryDistinctSources,
    /// Delegate greedy canonical-source coverage to `Memory`.
    MemorySourceCoverage,
    SourceDedup,
    /// Preserve reranker order while skipping candidates whose entire raw
    /// source set has already been covered. Later candidates backfill the
    /// vacated slots. This is presentation/consumer policy only: core search,
    /// graph activation, validity, and package validation remain unchanged.
    SourceCoverage,
    /// Preserve raw-fragment dominance when reviewed derived knowledge is
    /// present. Derived candidates retain their relative order but are
    /// deferred so they occupy at most one slot in every four selected
    /// candidates; raw candidates backfill the vacated positions.
    ///
    /// This is a consumer-side extraction guardrail, not a core scoring term.
    /// With no `anamnesis:derived` nodes the ranking is byte-for-byte
    /// unchanged.
    ProvenanceGuardrail,
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
    /// Rank after the configured consumer reranker, when present.
    pub consumer_rank: Option<usize>,
    pub consumer_score: Option<f64>,
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
    #[cfg(feature = "embed")]
    let needs_live_document_rerank = opts.consumer_evidence_documents
        && matches!(
            RecallPlan::infer(&question.question).recall_intent,
            RecallIntent::Enumeration | RecallIntent::Relational
        );
    #[cfg(feature = "embed")]
    let shadow = if let Some(rankings) = &opts.replayed_consumer_rankings
        && !needs_live_document_rerank
    {
        Some(replay_consumer_ranking(
            &result, graph, question, rankings, opts.top_k,
        )?)
    } else if let Some(reranker) = &opts.consumer_cross_encoder {
        Some(consumer_cross_encoder_package(
            &result,
            graph,
            question,
            reranker.as_ref(),
            opts.consumer_prefilter_cross_encoder.as_deref(),
            opts.consumer_candidate_k,
            opts.consumer_prefilter_k,
            opts.consumer_prefilter_query_fusion,
            opts.consumer_evidence_documents,
            opts.top_k,
        )?)
    } else {
        opts.shadow_rank_fusion
            .then(|| shadow_rank_fusion(&result, graph, question))
    };
    #[cfg(not(feature = "embed"))]
    let shadow = if let Some(rankings) = &opts.replayed_consumer_rankings {
        Some(replay_consumer_ranking(
            &result, graph, question, rankings, opts.top_k,
        )?)
    } else {
        opts.shadow_rank_fusion
            .then(|| shadow_rank_fusion(&result, graph, question))
    };
    let shadow = apply_consumer_selection_policy(&result, shadow, graph, question, opts)?;
    let search_latency_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Candidate surface: cognitive readout actually exposed to the consumer
    // reranker. This measures whether later stages ever had a chance to recover
    // the annotated evidence.
    let candidate_cutoff = if opts.shadow_rank_fusion {
        200
    } else {
        opts.consumer_candidate_k
    };
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
    let reranker_retrievals = if let Some((ranked, _)) = &shadow {
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
    let package = shadow
        .as_ref()
        .map_or(&result.package, |(_, package)| package);
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
            .text_search(&question.question, 200)
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
    let consumer_positions: HashMap<_, _> = shadow
        .as_ref()
        .map(|(ranking, _)| {
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

    let context = answer_context(
        package,
        graph,
        question,
        opts.top_k,
        opts.context_render_style,
        opts.consumer_selection_policy == ConsumerSelectionPolicy::MemoryDeep,
    )?;
    let rendered_matched_gold_units =
        rendered_gold_units(&context.product_context, graph, question);
    let rendered_recall = if total_relevant == 0 {
        0.0
    } else {
        rendered_matched_gold_units.len() as f64 / total_relevant as f64
    };
    let selection_variants = fixed_ranking_selection_variants(
        &result,
        shadow.as_ref().map(|(ranking, _)| ranking.as_slice()),
        graph,
        question,
        opts,
    )?;
    let evaluation = QuestionEvaluation {
        question_id: question.question_id.clone(),
        question_type: question.question_type.clone(),
        sample_index: question.sample_index,
        search_latency_ms,
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
    };
    Ok((evaluation, context))
}

fn replay_consumer_ranking(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    rankings: &HashMap<String, ConsumerRanking>,
    top_k: usize,
) -> BenchResult<ConsumerPackage> {
    let ranking = rankings.get(&question.question_id).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "replayed ranking is missing question {:?}",
            question.question_id
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
            question.question_id
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
        .repackage_reranked_at(result, &candidates, top_k, question_time(question))
        .map_err(|error| BenchError::Engine(error.to_string()))?;
    Ok((ranking.clone(), recall.package))
}

fn apply_consumer_selection_policy(
    result: &SearchResult,
    shadow: Option<ConsumerPackage>,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    opts: &EvalOptions,
) -> BenchResult<Option<ConsumerPackage>> {
    let (ranking, package) = match shadow {
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
        ),
    };
    match opts.consumer_selection_policy {
        ConsumerSelectionPolicy::Relevance => Ok(Some((ranking, package))),
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
                    &question.question,
                    result,
                    &candidates,
                    DeepRecallOptions::new(opts.top_k).with_selection(selection),
                    question_time(question),
                )
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            let compiled_ranking = recall
                .hits
                .iter()
                .map(|hit| (hit.node_id, hit.score))
                .collect();
            Ok(Some((compiled_ranking, recall.package)))
        }
        ConsumerSelectionPolicy::SourceDedup => {
            let ranking = source_dedup_ranking(&ranking, graph)?;
            let candidates: Vec<_> = ranking
                .iter()
                .map(|(node_id, score)| RerankedCandidate {
                    node_id: *node_id,
                    score: *score,
                })
                .collect();
            let recall = graph
                .memory
                .repackage_reranked_at(result, &candidates, opts.top_k, question_time(question))
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            Ok(Some((ranking, recall.package)))
        }
        ConsumerSelectionPolicy::SourceCoverage => {
            let ranking = source_coverage_ranking(&ranking, graph)?;
            let candidates: Vec<_> = ranking
                .iter()
                .map(|(node_id, score)| RerankedCandidate {
                    node_id: *node_id,
                    score: *score,
                })
                .collect();
            let recall = graph
                .memory
                .repackage_reranked_at(result, &candidates, opts.top_k, question_time(question))
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            Ok(Some((ranking, recall.package)))
        }
        ConsumerSelectionPolicy::ProvenanceGuardrail => {
            let ranking = provenance_guardrail_ranking(&ranking, graph)?;
            let candidates: Vec<_> = ranking
                .iter()
                .map(|(node_id, score)| RerankedCandidate {
                    node_id: *node_id,
                    score: *score,
                })
                .collect();
            let recall = graph
                .memory
                .repackage_reranked_at(result, &candidates, opts.top_k, question_time(question))
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            Ok(Some((ranking, recall.package)))
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

    let mut policies = vec![("".to_string(), ranking.clone())];
    if opts.screen_source_dedup {
        policies.push((
            "source-dedup-".to_string(),
            source_dedup_ranking(&ranking, graph)?,
        ));
    }

    for (name_prefix, policy_ranking) in policies {
        let reranked_candidates: Vec<_> = policy_ranking
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
                    question_time(question),
                )
                .map_err(|err| BenchError::Engine(err.to_string()))?;
            let selected =
                build_retrievals(policy_ranking.iter().copied(), graph, question, selection_k);
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
            let context = answer_context(
                &recall.package,
                graph,
                question,
                selection_k,
                opts.context_render_style,
                opts.consumer_selection_policy == ConsumerSelectionPolicy::MemoryDeep,
            )?;
            let rendered_units = rendered_gold_units(&context.product_context, graph, question);
            let rendered_recall = if total_relevant == 0 {
                0.0
            } else {
                rendered_units.len() as f64 / total_relevant as f64
            };
            variants.insert(
                format!("{name_prefix}top-{selection_k}"),
                SelectionVariantEvaluation {
                    selection_k,
                    selected_metrics: retrieval_metrics(
                        &selected_ranked,
                        total_relevant,
                        selection_k,
                    ),
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
    }
    Ok(variants)
}

fn source_dedup_ranking(
    ranking: &[(NodeId, f64)],
    graph: &BuiltMemoryGraph,
) -> BenchResult<ConsumerRanking> {
    let mut seen_sources = HashSet::new();
    let mut selected = Vec::with_capacity(ranking.len());
    for &(node_id, score) in ranking {
        let mut sources: Vec<_> = graph
            .memory
            .neighbors(node_id)
            .map_err(|err| BenchError::Engine(err.to_string()))?
            .into_iter()
            .filter(|neighbor| {
                neighbor.direction == Direction::Outgoing
                    && neighbor.edge_type == EdgeType::ExtractedFrom
            })
            .map(|neighbor| neighbor.node)
            .collect();
        if sources.is_empty() {
            sources.push(node_id);
        } else {
            sources.sort_unstable();
            sources.dedup();
        }
        if seen_sources.insert(sources) {
            selected.push((node_id, score));
        }
    }
    Ok(selected)
}

fn source_coverage_ranking(
    ranking: &[(NodeId, f64)],
    graph: &BuiltMemoryGraph,
) -> BenchResult<ConsumerRanking> {
    let mut covered_sources = HashSet::new();
    let mut selected = Vec::with_capacity(ranking.len());
    for &(node_id, score) in ranking {
        let mut sources: Vec<_> = graph
            .memory
            .neighbors(node_id)
            .map_err(|err| BenchError::Engine(err.to_string()))?
            .into_iter()
            .filter(|neighbor| {
                neighbor.direction == Direction::Outgoing
                    && neighbor.edge_type == EdgeType::ExtractedFrom
            })
            .map(|neighbor| neighbor.node)
            .collect();
        if sources.is_empty() {
            sources.push(node_id);
        } else {
            sources.sort_unstable();
            sources.dedup();
        }
        if sources
            .iter()
            .any(|source| !covered_sources.contains(source))
        {
            covered_sources.extend(sources);
            selected.push((node_id, score));
        }
    }
    Ok(selected)
}

fn provenance_guardrail_ranking(
    ranking: &[(NodeId, f64)],
    graph: &BuiltMemoryGraph,
) -> BenchResult<ConsumerRanking> {
    let mut derived_ids = HashSet::new();
    for &(node_id, _) in ranking {
        let node = graph
            .memory
            .get(node_id)
            .map_err(|err| BenchError::Engine(err.to_string()))?;
        if node
            .entity_tags
            .iter()
            .any(|tag| tag == "anamnesis:derived")
        {
            derived_ids.insert(node_id);
        }
    }
    if derived_ids.is_empty() {
        return Ok(ranking.to_vec());
    }
    let reordered = provenance_guardrail_order(ranking, |node_id| derived_ids.contains(&node_id));
    let denominator = reordered.len().saturating_add(1) as f64;
    Ok(reordered
        .into_iter()
        .enumerate()
        .map(|(index, (node_id, _))| {
            let score = reordered_score(index, denominator);
            (node_id, score)
        })
        .collect())
}

fn reordered_score(index: usize, denominator: f64) -> f64 {
    (denominator - index.saturating_add(1) as f64) / denominator
}

fn provenance_guardrail_order(
    ranking: &[(NodeId, f64)],
    is_derived: impl Fn(NodeId) -> bool,
) -> ConsumerRanking {
    const DERIVED_STRIDE: usize = 4;

    let mut selected = Vec::with_capacity(ranking.len());
    let mut pending_derived = std::collections::VecDeque::new();
    let mut derived_selected = 0usize;

    for &(node_id, score) in ranking {
        if is_derived(node_id) {
            pending_derived.push_back((node_id, score));
        } else {
            selected.push((node_id, score));
        }

        while derived_selected.saturating_mul(DERIVED_STRIDE) <= selected.len() {
            let Some(candidate) = pending_derived.pop_front() else {
                break;
            };
            selected.push(candidate);
            derived_selected = derived_selected.saturating_add(1);
        }
    }
    selected.extend(pending_derived);
    selected
}

fn shadow_rank_fusion(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
) -> ConsumerPackage {
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

#[cfg(feature = "embed")]
#[allow(clippy::too_many_arguments)]
fn consumer_cross_encoder_package(
    result: &SearchResult,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    reranker: &dyn RerankingProvider,
    prefilter: Option<&fastembed::TextRerank>,
    candidate_limit: usize,
    prefilter_limit: Option<usize>,
    prefilter_query_fusion: bool,
    evidence_documents: bool,
    final_limit: usize,
) -> BenchResult<ConsumerPackage> {
    if evidence_documents && prefilter.is_none() {
        let reranked = graph
            .memory
            .rerank_search_result_at(
                &question.question,
                result,
                reranker,
                RerankedRecallOptions::new(final_limit).with_candidate_limit(candidate_limit),
                question_time(question),
            )
            .map_err(|err| BenchError::Engine(err.to_string()))?;
        let ranking = reranked
            .ranking
            .iter()
            .map(|candidate| (candidate.node_id, candidate.score))
            .collect();
        return Ok((ranking, reranked.recall.package));
    }

    let broad_candidates: Vec<_> = if evidence_documents {
        graph
            .memory
            .rerank_documents(&question.question, result, candidate_limit)
            .map_err(|err| BenchError::Engine(err.to_string()))?
            .into_iter()
            .map(|document| (document.node_id, document.text))
            .collect()
    } else {
        result
            .trace
            .readout
            .iter()
            .take(candidate_limit)
            .map(|candidate| {
                graph
                    .memory
                    .get(candidate.node_id)
                    .map(|node| (candidate.node_id, node.content.clone()))
                    .map_err(|err| BenchError::Engine(err.to_string()))
            })
            .collect::<BenchResult<_>>()?
    };
    let candidates = match (prefilter, prefilter_limit) {
        (Some(prefilter), Some(prefilter_limit)) => {
            let documents: Vec<_> = broad_candidates
                .iter()
                .map(|(_, content)| content.clone())
                .collect();
            let prefiltered_indices = if prefilter_query_fusion {
                let query_variants = anamnesis::query::search_query_variants(&question.question);
                rerank_query_variants(prefilter, &query_variants, &question.question, &documents)?
            } else {
                prefilter
                    .rerank(question.question.clone(), documents, false, Some(32))
                    .map_err(|err| {
                        BenchError::Embedding(format!("prefilter cross-encoder failed: {err}"))
                    })?
                    .into_iter()
                    .map(|item| item.index)
                    .collect()
            };
            prefiltered_indices
                .into_iter()
                .take(prefilter_limit)
                .filter_map(|index| broad_candidates.get(index).cloned())
                .collect::<Vec<_>>()
        }
        _ => broad_candidates,
    };
    let documents: Vec<_> = candidates
        .iter()
        .map(|(_, content)| content.clone())
        .collect();
    let reranked = reranker
        .rerank(&question.question, &documents)
        .map_err(|err| BenchError::Embedding(format!("cross-encoder rerank failed: {err}")))?;

    let ranked: Vec<_> = reranked
        .iter()
        .filter_map(|item| {
            let (node_id, _) = candidates.get(item.index)?;
            Some((*node_id, item.score))
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
        .repackage_reranked_at(
            result,
            &consumer_ranking,
            final_limit,
            question_time(question),
        )
        .map_err(|err| BenchError::Engine(err.to_string()))?;
    Ok((ranked, recall.package))
}

#[cfg(feature = "embed")]
fn rerank_query_variants(
    reranker: &fastembed::TextRerank,
    query_variants: &[String],
    original_question: &str,
    documents: &[String],
) -> BenchResult<Vec<usize>> {
    const RRF_DAMPING: f64 = 60.0;

    let fallback;
    let variants = if query_variants.is_empty() {
        fallback = vec![original_question.to_owned()];
        fallback.as_slice()
    } else {
        query_variants
    };
    let mut scores = vec![0.0_f64; documents.len()];
    let mut best_rank = vec![usize::MAX; documents.len()];
    for query in variants {
        let ranked = reranker
            .rerank(query.clone(), documents.to_vec(), false, Some(32))
            .map_err(|err| BenchError::Embedding(format!("query-fusion rerank failed: {err}")))?;
        for (rank, item) in ranked.into_iter().enumerate() {
            if item.index < scores.len() {
                scores[item.index] += 1.0 / (RRF_DAMPING + (rank + 1) as f64);
                best_rank[item.index] = best_rank[item.index].min(rank);
            }
        }
    }
    let mut indices: Vec<_> = (0..documents.len()).collect();
    indices.sort_by(|left, right| {
        scores[*right]
            .total_cmp(&scores[*left])
            .then_with(|| best_rank[*left].cmp(&best_rank[*right]))
            .then_with(|| left.cmp(right))
    });
    Ok(indices)
}

fn search_question(
    graph: &mut BuiltMemoryGraph,
    question: &BenchQuestion,
    opts: &EvalOptions,
) -> BenchResult<SearchResult> {
    let now = question_time(question);
    let tuning = SearchTuning {
        seed_limit: opts.seed_limit,
        entity_tags: if opts.speaker_cues {
            super::speaker_cue_tags(&graph.speakers, &question.question)
        } else {
            vec![]
        },
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
                &question.question,
                search_limit,
                now,
                &tuning,
                &SearchDiagnostics::with_readout_trace_limit(readout_limit),
            )
            .map_err(|err| BenchError::Engine(err.to_string()))
    } else {
        graph
            .memory
            .search_result_at_with(&question.question, search_limit, now, &tuning)
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

fn answer_context(
    package: &ContextPackage,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
    top_k: usize,
    render_style: ContextRenderStyle,
    query_aware_context: bool,
) -> BenchResult<AnswerContext> {
    let recall = Recall {
        hits: Vec::new(),
        package: package.clone(),
    };
    let render_options = ContextRenderOptions::with_style(render_style);
    let product_context = if query_aware_context {
        graph
            .memory
            .render_context_for_with(&question.question, &recall, render_options)
    } else {
        graph.memory.render_context_with(&recall, render_options)
    }
    .map_err(|err| BenchError::Engine(err.to_string()))?;
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
    Ok(AnswerContext {
        product_context,
        product_context_chars,
        evidence,
        context_tokens: package.token_usage.used,
    })
}

fn question_time(question: &BenchQuestion) -> Timestamp {
    question
        .question_date
        .map(|epoch_seconds| Timestamp(epoch_seconds.saturating_mul(1_000)))
        .unwrap_or(Timestamp(0))
}

fn rendered_gold_units(
    product_context: &str,
    graph: &BuiltMemoryGraph,
    question: &BenchQuestion,
) -> Vec<String> {
    let normalized_context =
        super::super::super::locomo_pipeline::normalize_for_match(product_context);
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
    fn provenance_guardrail_preserves_raw_order_and_bounds_derived_prefixes() {
        let ranking: Vec<_> = (1..=12)
            .map(|value| (NodeId(value), (13 - value) as f64))
            .collect();
        let derived: HashSet<_> = [NodeId(1), NodeId(2), NodeId(3), NodeId(4), NodeId(8)]
            .into_iter()
            .collect();
        let reordered = provenance_guardrail_order(&ranking, |node_id| derived.contains(&node_id));

        let raw_before: Vec<_> = ranking
            .iter()
            .map(|(node_id, _)| *node_id)
            .filter(|node_id| !derived.contains(node_id))
            .collect();
        let raw_after: Vec<_> = reordered
            .iter()
            .map(|(node_id, _)| *node_id)
            .filter(|node_id| !derived.contains(node_id))
            .collect();
        assert_eq!(raw_after, raw_before);
        assert_eq!(reordered.len(), ranking.len());

        // The bound applies while raw backfill remains available. Excess
        // derived candidates are retained only at the tail so the ranking
        // remains a lossless permutation.
        for prefix_len in 1..=raw_before.len() {
            let derived_count = reordered[..prefix_len]
                .iter()
                .filter(|(node_id, _)| derived.contains(node_id))
                .count();
            assert!(derived_count <= prefix_len.div_ceil(4));
        }
    }

    #[test]
    fn provenance_guardrail_is_identity_without_derived_nodes() {
        let ranking = vec![(NodeId(3), 0.9), (NodeId(1), 0.8), (NodeId(2), 0.7)];
        assert_eq!(provenance_guardrail_order(&ranking, |_| false), ranking);
    }

    #[test]
    fn reordered_scores_are_finite_bounded_and_strictly_descending() {
        let denominator = 5.0;
        let scores: Vec<_> = (0..4)
            .map(|index| reordered_score(index, denominator))
            .collect();
        assert!(scores.iter().all(|score| score.is_finite()));
        assert!(scores.iter().all(|score| *score > 0.0 && *score < 1.0));
        assert!(scores.windows(2).all(|pair| pair[0] > pair[1]));
    }
}
