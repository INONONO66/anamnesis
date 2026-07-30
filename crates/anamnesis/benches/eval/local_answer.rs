#[path = "../eval_common/mod.rs"]
mod eval_common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anamnesis::engine::EmbeddingProvider;
use anamnesis::memory::{AnswerShape, ContextRenderStyle, RecallIntent, RecallPlan};
use serde::{Deserialize, Serialize};

use eval_common::answer_metrics;
use eval_common::provider::{LlmProvider, OpenAiCompatibleProvider, ProviderConfig, ProviderError};
use eval_common::real_bench::dataset::{
    BenchDatasetName, BenchQuestion, BenchSession, GoldEvidence, LoadedBenchmark,
    load_benchmark_dataset, restrict_to_questions, split_by_sample,
};
use eval_common::real_bench::graph::{
    AnswerContext, AnswerEvidence, CachingProvider, ConsumerSelectionPolicy, DerivedMemoryArtifact,
    EvalOptions, QuestionEvaluation, build_memory_graph, build_memory_graph_with_derived,
    evaluate_question_with_context,
};
use eval_common::real_bench::{BenchError, BenchResult};

#[cfg(not(feature = "embed"))]
compile_error!("local_answer requires: cargo bench --features embed --bench local_answer");

const SCHEMA_VERSION: u32 = 37;
const DATASET_LOADER_VERSION: &str = "locomo-caption-v2+longmemeval-cleaned-v1";
const ANSWER_PROMPT_VERSION: &str = "official-format-v11-grounded-inference-contract";
const REFLECT_PROMPT_VERSION: &str = "evidence-reflect-v11-source-completeness-compact";
const JUDGE_PROMPT_VERSION: &str = "semantic-answer-equivalence-v3";
const PROVIDER_READER_MAX_OUTPUT_TOKENS: u64 = 700;
const ENGINE_PACKAGE_POLICY_VERSION: &str = "timestamped-final-reassembly-v2";
const SHADOW_RRF_POLICY_VERSION: &str = "shadow-rrf-cognitive1-embedding0.25-text1-k60-v1";
const ROUTE_FULL_CONTEXT: &str = "0-full-context";
const ROUTE_ORACLE_BASELINE: &str = "1-oracle-baseline";
const ROUTE_RETRIEVAL_BASELINE: &str = "2-retrieval-baseline";
const ROUTE_RETRIEVAL_STRONG: &str = "3-retrieval-strong";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ContextSurface {
    /// Headline lane: exact `Recall::as_context()` output.
    ProductWire,
    /// Analysis-only lane: per-fragment labels enriched by the dataset adapter.
    DiagnosticFragments,
}

fn default_local_reader_backend() -> String {
    "ollama".to_owned()
}

fn default_local_judge_backend() -> String {
    "ollama".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RunConfig {
    dataset: BenchDatasetName,
    samples: Option<usize>,
    stratify: Option<usize>,
    question_type: Option<String>,
    sample_seed: u64,
    skip_adversarial: bool,
    run_strong_reader: bool,
    #[serde(default)]
    strong_reader_reflect: bool,
    #[serde(default)]
    strong_reader_reflect_complex_only: bool,
    #[serde(default = "default_local_reader_backend")]
    strong_reader_backend: String,
    run_full_context: bool,
    run_local_judge: bool,
    #[serde(default = "default_local_judge_backend")]
    judge_backend: String,
    run_oracle_baseline: bool,
    run_retrieval_baseline: bool,
    predict_only: bool,
    context_surface: ContextSurface,
    #[serde(default)]
    context_render_style: String,
    #[serde(default)]
    derived_memory_artifact_fnv1a64: Option<String>,
    #[serde(default)]
    derived_memory_extractor: Option<String>,
    #[serde(default)]
    derived_memory_extractor_digest: Option<String>,
    #[serde(default)]
    derived_memory_prompt_version: Option<String>,
    #[serde(default)]
    external_memory_artifact_fnv1a64: Option<String>,
    #[serde(default)]
    external_memory_system: Option<String>,
    #[serde(default)]
    external_memory_version: Option<String>,
    #[serde(default)]
    external_memory_config_digest: Option<String>,
    compact_retrieval_context: bool,
    hydrate_episodic_context: bool,
    shadow_rank_fusion: bool,
    consumer_cross_encoder: Option<String>,
    #[serde(default)]
    consumer_ranking_report_fnv1a64: Option<String>,
    /// Fingerprint of a prior answer report used only to reuse results whose
    /// question, rendered context, reader prompt, model, and generation
    /// settings are byte-for-byte identical.
    #[serde(default)]
    paired_answer_report_fnv1a64: Option<String>,
    #[serde(default)]
    consumer_prefilter_cross_encoder: Option<String>,
    #[serde(default)]
    consumer_prefilter_k: Option<usize>,
    #[serde(default)]
    consumer_prefilter_query_fusion: bool,
    #[serde(default)]
    consumer_evidence_documents: bool,
    consumer_candidate_k: usize,
    first_stage_seed_limit: Option<usize>,
    dump_candidate_pool: bool,
    screen_top_k: Vec<usize>,
    screen_source_dedup: bool,
    diagnostic_readout_limit: Option<usize>,
    consumer_selection_policy: ConsumerSelectionPolicy,
    top_k: usize,
    answer_prompt_version: String,
    #[serde(default)]
    reflect_prompt_version: Option<String>,
    #[serde(default)]
    judge_prompt_version: String,
    baseline_reader_model: String,
    strong_reader_model: String,
    judge_model: String,
    embedding_model: String,
    dataset_loader_version: String,
    engine_package_policy_version: String,
    reader_generation: GenerationOptions,
    judge_generation: GenerationOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GenerationOptions {
    think: bool,
    temperature: f64,
    top_p: f64,
    top_k: u64,
    presence_penalty: f64,
    seed: u64,
    num_ctx: u64,
    num_predict: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunReport {
    schema_version: u32,
    run_id: String,
    created_at_unix: u64,
    completed_at_unix: Option<u64>,
    local_only: bool,
    ollama_base_url: String,
    ollama_version: String,
    model_digests: BTreeMap<String, String>,
    dataset_path: String,
    dataset_bytes: u64,
    dataset_fnv1a64: String,
    config: RunConfig,
    questions: Vec<QuestionRecord>,
    summary: Option<RunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuestionRecord {
    question_id: String,
    question: String,
    expected_answer: String,
    question_type: String,
    sample_index: usize,
    question_date: Option<String>,
    oracle_context: Vec<OracleEvidence>,
    retrieval_context: Option<AnswerContext>,
    retrieval_evaluation: Option<QuestionEvaluation>,
    routes: BTreeMap<String, RouteResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OracleEvidence {
    session_id: String,
    raw_session_id: String,
    raw_turn_id: Option<String>,
    turn_index: usize,
    speaker: String,
    date: Option<String>,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RouteResult {
    reader_model: String,
    answer: String,
    answer_latency_ms: f64,
    context_items: usize,
    context_chars: usize,
    thinking_chars: usize,
    done_reason: Option<String>,
    prompt_eval_tokens: Option<u64>,
    output_eval_tokens: Option<u64>,
    /// Official deterministic LoCoMo score. LongMemEval uses its judge metric instead.
    locomo_official_f1: Option<f64>,
    /// Reference-blind output-canonicalization diagnostic. Never promoted as
    /// the official memory-quality score.
    locomo_reader_surface_f1: Option<f64>,
    canonicalized_answer: Option<String>,
    /// Reference-blind analysis emitted by the optional two-pass reflect
    /// reader. Benchmark gold and judge feedback are never part of this text.
    #[serde(default)]
    reflection: Option<String>,
    #[serde(default)]
    reflection_latency_ms: Option<f64>,
    judge: Option<JudgeDecision>,
    /// True when this exact-input result was copied from the paired answer
    /// report instead of asking the local model to sample the same prompt
    /// again.
    #[serde(default)]
    reused_from_paired_report: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JudgeDecision {
    judge_model: String,
    correct: Option<bool>,
    confidence: Option<f64>,
    reason: String,
    raw_response: String,
    parse_error: Option<String>,
    latency_ms: f64,
    done_reason: Option<String>,
    prompt_eval_tokens: Option<u64>,
    output_eval_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunSummary {
    total_questions: usize,
    routes: BTreeMap<String, RouteSummary>,
    #[serde(default)]
    retrieval: RetrievalSummary,
    #[serde(default)]
    selection_variants: BTreeMap<String, SelectionVariantSummary>,
    retrieval_bottleneck_cases: usize,
    strong_reader_recoveries: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RetrievalSummary {
    evaluated: usize,
    candidate_k: usize,
    reranker_k: usize,
    delivered_k: usize,
    mean_candidate_recall_at_k: f64,
    mean_reranker_recall_at_k: f64,
    mean_delivered_recall_at_k: f64,
    mean_rendered_recall: f64,
    candidate_hit_at_k: f64,
    reranker_hit_at_k: f64,
    delivered_hit_at_k: f64,
    rendered_hit: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SelectionVariantSummary {
    evaluated: usize,
    selection_k: usize,
    mean_selected_recall: f64,
    mean_delivered_recall: f64,
    mean_rendered_recall: f64,
    selected_hit: f64,
    rendered_hit: f64,
    mean_delivered_fragments: f64,
    mean_context_tokens: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RouteSummary {
    judged: usize,
    correct: usize,
    unparsed: usize,
    accuracy: f64,
    accuracy_ci95_low: f64,
    accuracy_ci95_high: f64,
    macro_accuracy: f64,
    accuracy_by_type: BTreeMap<String, f64>,
    locomo_official_scored: usize,
    locomo_official_f1: Option<f64>,
    locomo_official_f1_ci95_low: Option<f64>,
    locomo_official_f1_ci95_high: Option<f64>,
    locomo_official_f1_by_type: BTreeMap<String, f64>,
    locomo_reader_surface_f1: Option<f64>,
    locomo_reader_surface_f1_by_type: BTreeMap<String, f64>,
    mean_answer_latency_ms: f64,
    mean_judge_latency_ms: f64,
}

#[derive(Debug, Clone)]
struct Args {
    dataset: BenchDatasetName,
    data_dir: PathBuf,
    output: PathBuf,
    samples: Option<usize>,
    stratify: Option<usize>,
    question_type: Option<String>,
    sample_seed: u64,
    skip_adversarial: bool,
    run_strong_reader: bool,
    strong_reader_reflect: bool,
    strong_reader_reflect_complex_only: bool,
    strong_reader_remote: bool,
    frontier_judge: bool,
    frontier_base_url: Option<String>,
    frontier_max_cost_usd: Option<f64>,
    run_full_context: bool,
    run_local_judge: bool,
    run_oracle_baseline: bool,
    run_retrieval_baseline: bool,
    predict_only: bool,
    context_surface: ContextSurface,
    evidence_context: bool,
    derived_memory_artifact: Option<PathBuf>,
    external_memory_artifact: Option<PathBuf>,
    answer_report: Option<PathBuf>,
    paired_answer_report: Option<PathBuf>,
    judge_report: Option<PathBuf>,
    compact_retrieval_context: bool,
    hydrate_episodic_context: bool,
    shadow_rank_fusion: bool,
    consumer_cross_encoder: Option<String>,
    consumer_ranking_report: Option<PathBuf>,
    consumer_prefilter_cross_encoder: Option<String>,
    consumer_prefilter_k: Option<usize>,
    consumer_prefilter_query_fusion: bool,
    consumer_evidence_documents: bool,
    consumer_candidate_k: usize,
    first_stage_seed_limit: Option<usize>,
    dump_candidate_pool: bool,
    screen_top_k: Vec<usize>,
    screen_source_dedup: bool,
    diagnostic_readout_limit: Option<usize>,
    consumer_selection_policy: ConsumerSelectionPolicy,
    top_k: usize,
    baseline_reader_model: String,
    strong_reader_model: String,
    judge_model: String,
    embedding_model: String,
    ollama_base_url: String,
    timeout_secs: u64,
    embed_cache: Option<PathBuf>,
    allow_download: bool,
    resume: bool,
    force: bool,
    reader_generation: GenerationOptions,
    judge_generation: GenerationOptions,
}

struct ConsumerRankingReplay {
    rankings: Arc<HashMap<String, Vec<(anamnesis::graph::NodeId, f64)>>>,
    source_config: RunConfig,
    report_fnv1a64: String,
}

struct OllamaClient {
    http: reqwest::blocking::Client,
    base_url: String,
    timeout_secs: u64,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Deserialize)]
struct OllamaMessage {
    content: String,
    #[serde(default)]
    thinking: String,
}

struct GeneratedText {
    content: String,
    thinking_chars: usize,
    done_reason: Option<String>,
    prompt_eval_tokens: Option<u64>,
    output_eval_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OllamaVersionResponse {
    version: String,
}

#[derive(Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaTag>,
}

#[derive(Deserialize)]
struct OllamaTag {
    name: String,
    #[serde(default)]
    model: String,
    digest: String,
}

#[derive(Deserialize)]
struct JudgeJson {
    verdict: String,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalMemoryArtifact {
    schema_version: u32,
    dataset_fnv1a64: String,
    system_name: String,
    system_version: String,
    system_config_digest: String,
    records: Vec<ExternalMemoryRecord>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalMemoryRecord {
    question_id: String,
    context: String,
    #[serde(default)]
    evidence: Vec<ExternalMemoryEvidence>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalMemoryEvidence {
    text: String,
    raw_session_id: Option<String>,
    raw_turn_id: Option<String>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> BenchResult<()> {
    let Some(args) = parse_args(std::env::args().skip(1))? else {
        print_usage();
        return Ok(());
    };
    validate_local_url(&args.ollama_base_url)?;
    if let Some(path) = args.answer_report.as_deref() {
        return run_answer_report(&args, path);
    }
    if let Some(path) = args.judge_report.as_deref() {
        return run_judge_report(&args, path);
    }

    let dataset_path = dataset_path(args.dataset, &args.data_dir);
    let (dataset_bytes, dataset_fnv1a64) = fingerprint(&dataset_path)?;
    let loader_limit = (args.dataset == BenchDatasetName::LongMemEval && args.stratify.is_none())
        .then_some(args.samples)
        .flatten();
    let mut loaded = load_benchmark_dataset(args.dataset, &args.data_dir, loader_limit)?;
    if args.skip_adversarial {
        loaded
            .questions
            .retain(|question| question.question_type != "adversarial");
    }
    if let Some(question_type) = args.question_type.as_deref() {
        loaded
            .questions
            .retain(|question| question.question_type == question_type);
    }
    if let Some(per_type) = args.stratify {
        stratify_questions_seeded(&mut loaded.questions, per_type, args.sample_seed);
    }
    let loaded = restrict_to_questions(loaded, args.samples);
    if loaded.questions.is_empty() {
        return Err(BenchError::InvalidInput(
            "selected dataset contains no questions".to_string(),
        ));
    }
    if let Some(path) = args.external_memory_artifact.as_deref() {
        return run_external_memory_artifact(
            &args,
            &loaded,
            dataset_path,
            dataset_bytes,
            &dataset_fnv1a64,
            path,
        );
    }
    let (derived_artifact, derived_artifact_fnv1a64) = args
        .derived_memory_artifact
        .as_deref()
        .map(|path| {
            if args.dataset != BenchDatasetName::Locomo {
                return Err(BenchError::InvalidInput(
                    "derived-memory artifact currently requires LoCoMo stable turn ids".to_owned(),
                ));
            }
            load_derived_memory_artifact(path, &dataset_fnv1a64)
        })
        .transpose()?
        .map_or((None, None), |(artifact, digest)| {
            (Some(artifact), Some(digest))
        });
    let ranking_replay = args
        .consumer_ranking_report
        .as_deref()
        .map(|path| {
            load_consumer_ranking_replay(
                path,
                &loaded,
                &dataset_fnv1a64,
                &args,
                derived_artifact_fnv1a64.as_deref(),
            )
        })
        .transpose()?;

    let (ollama, ollama_version, model_digests) = if args.predict_only {
        (None, "not-used-predict-only".to_string(), BTreeMap::new())
    } else {
        let client = OllamaClient::new(&args.ollama_base_url, args.timeout_secs)?;
        let version = client.version()?;
        let mut requested_models = vec![args.baseline_reader_model.as_str()];
        if args.run_local_judge && !args.frontier_judge {
            requested_models.push(args.judge_model.as_str());
        }
        if args.run_strong_reader && !args.strong_reader_remote {
            requested_models.push(args.strong_reader_model.as_str());
        }
        let digests = client.require_models(&requested_models)?;
        eprintln!("LOCAL ollama={} models={:?}", version, requested_models);
        (Some(client), version, digests)
    };
    let frontier_reader = args
        .strong_reader_remote
        .then(|| {
            let base_url = args.frontier_base_url.as_deref().ok_or_else(|| {
                BenchError::InvalidInput(
                    "--frontier-reader requires --frontier-base-url or LLM_BASE_URL".to_owned(),
                )
            })?;
            OpenAiCompatibleProvider::new(ProviderConfig {
                base_url: base_url.to_owned(),
                model: args.strong_reader_model.clone(),
                timeout_secs: args.timeout_secs,
                max_retries: 3,
                max_output_tokens: Some(PROVIDER_READER_MAX_OUTPUT_TOKENS),
                chat_template_enable_thinking: qwen_chat_template_thinking(
                    &args.strong_reader_model,
                    args.reader_generation.think,
                ),
            })
            .map_err(provider_error)
        })
        .transpose()?;
    eprintln!(
        "LOAD dataset={} questions={} fingerprint={}",
        args.dataset.as_str(),
        loaded.questions.len(),
        dataset_fnv1a64
    );

    if !args.allow_download {
        return Err(BenchError::InvalidInput(
            "FastEmbed may initialize/download model weights; pass --allow-download".to_string(),
        ));
    }
    let embedding_model =
        anamnesis::embedding::fastembed::embed_model_from_name(&args.embedding_model)
            .map_err(|err| BenchError::Embedding(format!("embedding model: {err}")))?;
    let inner = Arc::new(
        anamnesis::engine::FastEmbedProvider::with_model(embedding_model)
            .map_err(|err| BenchError::Embedding(format!("FastEmbed init failed: {err}")))?,
    );
    let embedding_model = inner.model_name().to_string();
    let cache = args
        .embed_cache
        .as_deref()
        .map(|path| {
            eval_common::real_bench::embed_cache::EmbedCache::open(path, inner.model_name())
        })
        .transpose()?;
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(CachingProvider::new(inner.clone(), cache));
    if let Some(replay) = &ranking_replay
        && replay.source_config.embedding_model != embedding_model
    {
        return Err(BenchError::InvalidInput(
            "consumer ranking report embedding model differs".to_owned(),
        ));
    }
    let consumer_cross_encoder = if ranking_replay.is_some() && !args.consumer_evidence_documents {
        None
    } else {
        args.consumer_cross_encoder
            .as_deref()
            .map(|model_name| {
                anamnesis::embedding::fastembed::FastEmbedReranker::with_model_name(model_name)
                    .map(|reranker| {
                        Arc::new(reranker) as Arc<dyn anamnesis::embedding::RerankingProvider>
                    })
                    .map_err(|err| {
                        BenchError::Embedding(format!("cross-encoder init failed: {err}"))
                    })
            })
            .transpose()?
    };
    let consumer_prefilter_cross_encoder = args
        .consumer_prefilter_cross_encoder
        .as_deref()
        .map(|model_name| {
            let model = model_name
                .parse::<fastembed::RerankerModel>()
                .map_err(|err| {
                    BenchError::InvalidInput(format!(
                        "unknown prefilter cross-encoder model: {err}"
                    ))
                })?;
            fastembed::TextRerank::try_new(
                fastembed::RerankInitOptions::new(model)
                    .with_cache_dir(PathBuf::from(".fastembed_cache")),
            )
            .map(Arc::new)
            .map_err(|err| {
                BenchError::Embedding(format!("prefilter cross-encoder init failed: {err}"))
            })
        })
        .transpose()?;

    let config = RunConfig {
        dataset: args.dataset,
        samples: args.samples,
        stratify: args.stratify,
        question_type: args.question_type.clone(),
        sample_seed: args.sample_seed,
        skip_adversarial: args.skip_adversarial,
        run_strong_reader: args.run_strong_reader,
        strong_reader_reflect: args.strong_reader_reflect,
        strong_reader_reflect_complex_only: args.strong_reader_reflect_complex_only,
        strong_reader_backend: if args.strong_reader_remote {
            "openai-compatible".to_owned()
        } else {
            default_local_reader_backend()
        },
        run_full_context: args.run_full_context,
        run_local_judge: args.run_local_judge,
        judge_backend: if args.frontier_judge {
            "openai-compatible".to_owned()
        } else {
            default_local_judge_backend()
        },
        run_oracle_baseline: args.run_oracle_baseline,
        run_retrieval_baseline: args.run_retrieval_baseline,
        predict_only: args.predict_only,
        context_surface: args.context_surface,
        context_render_style: if args.evidence_context {
            "evidence".to_owned()
        } else {
            "detailed".to_owned()
        },
        derived_memory_artifact_fnv1a64: derived_artifact_fnv1a64,
        derived_memory_extractor: derived_artifact
            .as_ref()
            .map(|artifact| artifact.extractor_model.clone()),
        derived_memory_extractor_digest: derived_artifact
            .as_ref()
            .map(|artifact| artifact.extractor_digest.clone()),
        derived_memory_prompt_version: derived_artifact
            .as_ref()
            .map(|artifact| artifact.prompt_version.clone()),
        external_memory_artifact_fnv1a64: None,
        external_memory_system: None,
        external_memory_version: None,
        external_memory_config_digest: None,
        compact_retrieval_context: args.compact_retrieval_context,
        hydrate_episodic_context: args.hydrate_episodic_context,
        shadow_rank_fusion: args.shadow_rank_fusion,
        consumer_cross_encoder: args.consumer_cross_encoder.clone(),
        consumer_ranking_report_fnv1a64: ranking_replay
            .as_ref()
            .map(|replay| replay.report_fnv1a64.clone()),
        paired_answer_report_fnv1a64: None,
        consumer_prefilter_cross_encoder: args.consumer_prefilter_cross_encoder.clone(),
        consumer_prefilter_k: args.consumer_prefilter_k,
        consumer_prefilter_query_fusion: args.consumer_prefilter_query_fusion,
        consumer_evidence_documents: args.consumer_evidence_documents,
        consumer_candidate_k: args.consumer_candidate_k,
        first_stage_seed_limit: args.first_stage_seed_limit,
        dump_candidate_pool: args.dump_candidate_pool,
        screen_top_k: args.screen_top_k.clone(),
        screen_source_dedup: args.screen_source_dedup,
        diagnostic_readout_limit: args.diagnostic_readout_limit,
        consumer_selection_policy: args.consumer_selection_policy,
        top_k: args.top_k,
        answer_prompt_version: ANSWER_PROMPT_VERSION.to_string(),
        reflect_prompt_version: args
            .strong_reader_reflect
            .then(|| REFLECT_PROMPT_VERSION.to_owned()),
        judge_prompt_version: JUDGE_PROMPT_VERSION.to_string(),
        baseline_reader_model: args.baseline_reader_model.clone(),
        strong_reader_model: args.strong_reader_model.clone(),
        judge_model: args.judge_model.clone(),
        embedding_model,
        dataset_loader_version: DATASET_LOADER_VERSION.to_string(),
        engine_package_policy_version: if let Some(replay) = &ranking_replay {
            format!(
                "consumer-ranking-replay-{}-top{}-product-path-v1",
                replay.report_fnv1a64, args.consumer_candidate_k
            )
        } else if let Some(model) = &args.consumer_cross_encoder {
            match (
                args.consumer_prefilter_cross_encoder.as_deref(),
                args.consumer_prefilter_k,
                args.consumer_prefilter_query_fusion,
            ) {
                (Some(prefilter), Some(prefilter_k), query_fusion) => format!(
                    "consumer-cascade-top{}-{prefilter}-top{prefilter_k}-{model}-query-fusion-{query_fusion}-evidence-documents-{}-product-path-v3",
                    args.consumer_candidate_k, args.consumer_evidence_documents
                ),
                _ => format!(
                    "consumer-cross-encoder-top{}-{model}-evidence-documents-{}-product-path-v3",
                    args.consumer_candidate_k, args.consumer_evidence_documents
                ),
            }
        } else if args.shadow_rank_fusion {
            SHADOW_RRF_POLICY_VERSION.to_string()
        } else {
            ENGINE_PACKAGE_POLICY_VERSION.to_string()
        },
        reader_generation: args.reader_generation.clone(),
        judge_generation: args.judge_generation.clone(),
    };
    let mut report = load_or_create_report(
        &args,
        &loaded,
        config,
        dataset_path,
        dataset_bytes,
        dataset_fnv1a64,
        ollama_version,
        model_digests,
    )?;
    write_report(&args.output, &report)?;

    let options = EvalOptions {
        top_k: args.top_k,
        seed_limit: args.first_stage_seed_limit,
        dump_features: args.dump_candidate_pool,
        speaker_cues: false,
        shadow_rank_fusion: args.shadow_rank_fusion,
        consumer_cross_encoder,
        replayed_consumer_rankings: ranking_replay.map(|replay| replay.rankings),
        consumer_prefilter_cross_encoder,
        consumer_prefilter_k: args.consumer_prefilter_k,
        consumer_prefilter_query_fusion: args.consumer_prefilter_query_fusion,
        consumer_evidence_documents: args.consumer_evidence_documents,
        consumer_candidate_k: args.consumer_candidate_k,
        screen_top_k: args.screen_top_k.clone(),
        screen_source_dedup: args.screen_source_dedup,
        diagnostic_readout_limit: args.diagnostic_readout_limit,
        consumer_selection_policy: args.consumer_selection_policy,
        context_render_style: if args.evidence_context {
            ContextRenderStyle::Evidence
        } else {
            ContextRenderStyle::Detailed
        },
    };
    let groups = split_by_sample(loaded);
    for (group_index, group) in groups.iter().enumerate() {
        let sample_index = group.questions[0].sample_index;
        eprintln!(
            "GRAPH {}/{} sample={} sessions={} questions={}",
            group_index + 1,
            groups.len(),
            sample_index,
            group.sessions.len(),
            group.questions.len()
        );
        let mut graph = match &derived_artifact {
            Some(artifact) => build_memory_graph_with_derived(
                group,
                provider.clone(),
                &artifact.records,
                &artifact.relations,
            )?,
            None => build_memory_graph(group, provider.clone())?,
        };
        for question in &group.questions {
            let record_index = report
                .questions
                .iter()
                .position(|record| record.question_id == question.question_id)
                .ok_or_else(|| {
                    BenchError::Parse(format!(
                        "question {} missing from report",
                        question.question_id
                    ))
                })?;

            let (retrieval_evaluation, retrieval_context) =
                evaluate_question_with_context(&mut graph, question, &options)?;
            let oracle_context = oracle_context(group, question);
            {
                let record = &mut report.questions[record_index];
                record.oracle_context = oracle_context.clone();
                record.retrieval_context = Some(retrieval_context.clone());
                record.retrieval_evaluation = Some(retrieval_evaluation);
            }
            write_report(&args.output, &report)?;

            let oracle_prompt_context = oracle_prompt_context(&oracle_context);
            let mut retrieval_prompt_context = match args.context_surface {
                ContextSurface::ProductWire => product_wire_prompt_context(&retrieval_context),
                ContextSurface::DiagnosticFragments => {
                    diagnostic_retrieval_prompt_context(group, &retrieval_context)
                }
            };
            if args.compact_retrieval_context {
                retrieval_prompt_context = compact_prompt_context(retrieval_prompt_context);
            }
            if args.hydrate_episodic_context {
                hydrate_episodic_context(group, &mut retrieval_prompt_context);
            }
            if args.run_full_context {
                let ollama = require_ollama(&ollama)?;
                let full_context = full_prompt_context(group);
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_FULL_CONTEXT,
                    &args.baseline_reader_model,
                    &full_context,
                    ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
            if args.run_oracle_baseline {
                let ollama = require_ollama(&ollama)?;
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_ORACLE_BASELINE,
                    &args.baseline_reader_model,
                    &oracle_prompt_context,
                    ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
            if args.run_retrieval_baseline {
                let ollama = require_ollama(&ollama)?;
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_RETRIEVAL_BASELINE,
                    &args.baseline_reader_model,
                    &retrieval_prompt_context,
                    ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
            if args.run_strong_reader {
                if let Some(provider) = frontier_reader.as_ref() {
                    if should_reflect_question(&args, &report.questions[record_index].question) {
                        run_provider_reflect_answer(
                            &mut report,
                            record_index,
                            ROUTE_RETRIEVAL_STRONG,
                            provider,
                            &retrieval_prompt_context,
                        )?;
                    } else {
                        run_provider_answer(
                            &mut report,
                            record_index,
                            ROUTE_RETRIEVAL_STRONG,
                            provider,
                            &retrieval_prompt_context,
                        )?;
                    }
                } else {
                    let ollama = require_ollama(&ollama)?;
                    if should_reflect_question(&args, &report.questions[record_index].question) {
                        run_reflect_answer(
                            &mut report,
                            record_index,
                            ROUTE_RETRIEVAL_STRONG,
                            &args.strong_reader_model,
                            &retrieval_prompt_context,
                            ollama,
                            &args.reader_generation,
                        )?;
                    } else {
                        run_answer(
                            &mut report,
                            record_index,
                            ROUTE_RETRIEVAL_STRONG,
                            &args.strong_reader_model,
                            &retrieval_prompt_context,
                            ollama,
                            &args.reader_generation,
                        )?;
                    }
                }
                write_report(&args.output, &report)?;
            }
        }
    }

    if args.run_local_judge {
        let ollama = require_ollama(&ollama)?;
        eprintln!("JUDGE PHASE questions={}", report.questions.len());
        let mut routes = Vec::new();
        if args.run_full_context {
            routes.push(ROUTE_FULL_CONTEXT);
        }
        if args.run_oracle_baseline {
            routes.push(ROUTE_ORACLE_BASELINE);
        }
        if args.run_retrieval_baseline {
            routes.push(ROUTE_RETRIEVAL_BASELINE);
        }
        if args.run_strong_reader {
            routes.push(ROUTE_RETRIEVAL_STRONG);
        }
        for record_index in 0..report.questions.len() {
            for route in &routes {
                run_judge(&mut report, record_index, route, ollama, &args)?;
                write_report(&args.output, &report)?;
            }
        }
    }

    report.summary = Some(build_summary(&report.questions));
    report.completed_at_unix = Some(timestamp_secs());
    write_report(&args.output, &report)?;
    print_summary(report.summary.as_ref());
    eprintln!("REPORT {}", args.output.display());
    Ok(())
}

fn run_external_memory_artifact(
    args: &Args,
    loaded: &LoadedBenchmark,
    dataset_path: PathBuf,
    dataset_bytes: u64,
    dataset_fnv1a64: &str,
    artifact_path: &Path,
) -> BenchResult<()> {
    if args.run_oracle_baseline
        || args.run_full_context
        || args.run_strong_reader
        || args.derived_memory_artifact.is_some()
        || args.evidence_context
        || args.consumer_cross_encoder.is_some()
        || args.consumer_prefilter_cross_encoder.is_some()
        || args.shadow_rank_fusion
    {
        return Err(BenchError::InvalidInput(
            "--external-memory-artifact is one frozen retrieval-context lane; use \
             --retrieval-only and omit oracle/full/strong/derived/evidence/reranker flags"
                .to_owned(),
        ));
    }
    let (_, artifact_fnv1a64) = fingerprint(artifact_path)?;
    let bytes = std::fs::read(artifact_path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to read external-memory artifact {}: {error}",
            artifact_path.display()
        ))
    })?;
    let artifact: ExternalMemoryArtifact = serde_json::from_slice(&bytes).map_err(|error| {
        BenchError::Parse(format!(
            "failed to parse external-memory artifact {}: {error}",
            artifact_path.display()
        ))
    })?;
    if artifact.schema_version != 1 || artifact.dataset_fnv1a64 != dataset_fnv1a64 {
        return Err(BenchError::InvalidInput(
            "external-memory artifact schema or dataset fingerprint differs".to_owned(),
        ));
    }
    for value in [
        artifact.system_name.as_str(),
        artifact.system_version.as_str(),
        artifact.system_config_digest.as_str(),
    ] {
        if value.trim().is_empty() || value.len() > 256 {
            return Err(BenchError::InvalidInput(
                "external-memory artifact system identity is empty or too long".to_owned(),
            ));
        }
    }
    let selected_ids: BTreeSet<_> = loaded
        .questions
        .iter()
        .map(|question| question.question_id.as_str())
        .collect();
    let mut records = BTreeMap::new();
    for record in artifact.records {
        if record.context.trim().is_empty()
            || record.context.len() > 1_000_000
            || record.evidence.len() > 512
            || record
                .evidence
                .iter()
                .any(|evidence| evidence.text.len() > 100_000)
        {
            return Err(BenchError::InvalidInput(format!(
                "external context {:?} violates size bounds",
                record.question_id
            )));
        }
        let question_id = record.question_id.clone();
        if records.insert(question_id.clone(), record).is_some() {
            return Err(BenchError::InvalidInput(format!(
                "duplicate external context question id {question_id:?}"
            )));
        }
    }
    let artifact_ids: BTreeSet<_> = records.keys().map(String::as_str).collect();
    if artifact_ids != selected_ids {
        return Err(BenchError::InvalidInput(
            "external-memory artifact question set differs from the selected benchmark set"
                .to_owned(),
        ));
    }

    let (ollama, ollama_version, model_digests) = if args.predict_only {
        (None, "not-used-predict-only".to_owned(), BTreeMap::new())
    } else {
        let client = OllamaClient::new(&args.ollama_base_url, args.timeout_secs)?;
        let version = client.version()?;
        let mut requested_models = vec![args.baseline_reader_model.as_str()];
        if args.run_local_judge && !args.frontier_judge {
            requested_models.push(args.judge_model.as_str());
        }
        let digests = client.require_models(&requested_models)?;
        (Some(client), version, digests)
    };
    let config = RunConfig {
        dataset: args.dataset,
        samples: args.samples,
        stratify: args.stratify,
        question_type: args.question_type.clone(),
        sample_seed: args.sample_seed,
        skip_adversarial: args.skip_adversarial,
        run_strong_reader: false,
        strong_reader_reflect: false,
        strong_reader_reflect_complex_only: false,
        strong_reader_backend: default_local_reader_backend(),
        run_full_context: false,
        run_local_judge: args.run_local_judge,
        judge_backend: default_local_judge_backend(),
        run_oracle_baseline: false,
        run_retrieval_baseline: true,
        predict_only: args.predict_only,
        context_surface: ContextSurface::ProductWire,
        context_render_style: "external-system-wire".to_owned(),
        derived_memory_artifact_fnv1a64: None,
        derived_memory_extractor: None,
        derived_memory_extractor_digest: None,
        derived_memory_prompt_version: None,
        external_memory_artifact_fnv1a64: Some(artifact_fnv1a64),
        external_memory_system: Some(artifact.system_name.clone()),
        external_memory_version: Some(artifact.system_version.clone()),
        external_memory_config_digest: Some(artifact.system_config_digest.clone()),
        compact_retrieval_context: false,
        hydrate_episodic_context: false,
        shadow_rank_fusion: false,
        consumer_cross_encoder: None,
        consumer_ranking_report_fnv1a64: None,
        paired_answer_report_fnv1a64: None,
        consumer_prefilter_cross_encoder: None,
        consumer_prefilter_k: None,
        consumer_prefilter_query_fusion: false,
        consumer_evidence_documents: false,
        consumer_candidate_k: 0,
        first_stage_seed_limit: None,
        dump_candidate_pool: false,
        screen_top_k: Vec::new(),
        screen_source_dedup: false,
        diagnostic_readout_limit: None,
        consumer_selection_policy: ConsumerSelectionPolicy::Relevance,
        top_k: 0,
        answer_prompt_version: ANSWER_PROMPT_VERSION.to_owned(),
        reflect_prompt_version: None,
        judge_prompt_version: JUDGE_PROMPT_VERSION.to_owned(),
        baseline_reader_model: args.baseline_reader_model.clone(),
        strong_reader_model: args.strong_reader_model.clone(),
        judge_model: args.judge_model.clone(),
        embedding_model: "external-system-owned".to_owned(),
        dataset_loader_version: DATASET_LOADER_VERSION.to_owned(),
        engine_package_policy_version: format!(
            "external-context:{}:{}:{}",
            artifact.system_name, artifact.system_version, artifact.system_config_digest
        ),
        reader_generation: args.reader_generation.clone(),
        judge_generation: args.judge_generation.clone(),
    };
    let mut report = load_or_create_report(
        args,
        loaded,
        config,
        dataset_path,
        dataset_bytes,
        dataset_fnv1a64.to_owned(),
        ollama_version,
        model_digests,
    )?;

    for record in &mut report.questions {
        let external = records.get(&record.question_id).ok_or_else(|| {
            BenchError::Parse(format!(
                "external context disappeared for {:?}",
                record.question_id
            ))
        })?;
        let context_tokens = external.context.chars().count().div_ceil(4);
        record.retrieval_context = Some(AnswerContext {
            product_context: external.context.clone(),
            product_context_chars: external.context.len(),
            evidence: external
                .evidence
                .iter()
                .enumerate()
                .map(|(index, evidence)| AnswerEvidence {
                    rank: index + 1,
                    node_id: u64::try_from(index + 1).unwrap_or(u64::MAX),
                    score: 0.0,
                    text: evidence.text.clone(),
                    session_id: None,
                    raw_session_id: evidence.raw_session_id.clone(),
                    raw_turn_id: evidence.raw_turn_id.clone(),
                    relevant: false,
                    matched_gold_units: Vec::new(),
                })
                .collect(),
            context_tokens,
        });
        record.retrieval_evaluation = None;
        record.routes.clear();
    }
    write_report(&args.output, &report)?;

    if !args.predict_only {
        let ollama = require_ollama(&ollama)?;
        for record_index in 0..report.questions.len() {
            let context = report.questions[record_index]
                .retrieval_context
                .as_ref()
                .map(product_wire_prompt_context)
                .ok_or_else(|| BenchError::Parse("external context disappeared".to_owned()))?;
            run_answer(
                &mut report,
                record_index,
                ROUTE_RETRIEVAL_BASELINE,
                &args.baseline_reader_model,
                &context,
                ollama,
                &args.reader_generation,
            )?;
            if args.run_local_judge {
                run_judge(
                    &mut report,
                    record_index,
                    ROUTE_RETRIEVAL_BASELINE,
                    ollama,
                    args,
                )?;
            }
            write_report(&args.output, &report)?;
        }
    }
    report.summary = Some(build_summary(&report.questions));
    report.completed_at_unix = Some(timestamp_secs());
    write_report(&args.output, &report)?;
    print_summary(report.summary.as_ref());
    eprintln!("REPORT {}", args.output.display());
    Ok(())
}

fn load_consumer_ranking_replay(
    path: &Path,
    loaded: &LoadedBenchmark,
    dataset_fnv1a64: &str,
    args: &Args,
    derived_artifact_fnv1a64: Option<&str>,
) -> BenchResult<ConsumerRankingReplay> {
    let (_, report_fnv1a64) = fingerprint(path)?;
    let text = std::fs::read_to_string(path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to read consumer ranking report {}: {error}",
            path.display()
        ))
    })?;
    let report: RunReport =
        serde_json::from_str(&text).map_err(|error| BenchError::Parse(error.to_string()))?;
    let config = &report.config;
    if report.dataset_fnv1a64 != dataset_fnv1a64
        || config.dataset != args.dataset
        || config.samples != args.samples
        || config.stratify != args.stratify
        || config.question_type != args.question_type
        || config.sample_seed != args.sample_seed
        || config.skip_adversarial != args.skip_adversarial
        || config.consumer_cross_encoder != args.consumer_cross_encoder
        || config.consumer_candidate_k != args.consumer_candidate_k
        || (config.consumer_evidence_documents != args.consumer_evidence_documents
            && !args.consumer_evidence_documents)
        || config.first_stage_seed_limit != args.first_stage_seed_limit
        || config.derived_memory_artifact_fnv1a64.as_deref() != derived_artifact_fnv1a64
        || config.consumer_selection_policy != ConsumerSelectionPolicy::Relevance
    {
        return Err(BenchError::InvalidInput(
            "consumer ranking report retrieval controls differ or its ranking is not raw relevance"
                .to_owned(),
        ));
    }
    let selected_ids: BTreeSet<_> = loaded
        .questions
        .iter()
        .map(|question| question.question_id.as_str())
        .collect();
    let report_ids: BTreeSet<_> = report
        .questions
        .iter()
        .map(|question| question.question_id.as_str())
        .collect();
    if selected_ids != report_ids {
        return Err(BenchError::InvalidInput(
            "consumer ranking report question set differs".to_owned(),
        ));
    }

    let mut rankings = HashMap::new();
    for question in &report.questions {
        let evaluation = question.retrieval_evaluation.as_ref().ok_or_else(|| {
            BenchError::InvalidInput(format!(
                "consumer ranking report is incomplete for {:?}",
                question.question_id
            ))
        })?;
        let mut seen = BTreeSet::new();
        let ranking: Vec<_> = evaluation
            .reranker_retrievals
            .iter()
            .map(|retrieval| {
                if !retrieval.score.is_finite() || !seen.insert(retrieval.node_id) {
                    return Err(BenchError::InvalidInput(format!(
                        "consumer ranking report has invalid rows for {:?}",
                        question.question_id
                    )));
                }
                Ok((anamnesis::graph::NodeId(retrieval.node_id), retrieval.score))
            })
            .collect::<BenchResult<_>>()?;
        if ranking.len() < args.top_k {
            return Err(BenchError::InvalidInput(format!(
                "consumer ranking report has {} rows for {:?}, fewer than requested top-k {}",
                ranking.len(),
                question.question_id,
                args.top_k
            )));
        }
        rankings.insert(question.question_id.clone(), ranking);
    }
    Ok(ConsumerRankingReplay {
        rankings: Arc::new(rankings),
        source_config: report.config,
        report_fnv1a64,
    })
}

fn run_answer_report(args: &Args, source_path: &Path) -> BenchResult<()> {
    if args.predict_only
        || args.run_oracle_baseline
        || args.run_full_context
        || args.evidence_context
        || args.derived_memory_artifact.is_some()
    {
        return Err(BenchError::InvalidInput(
            "--answer-report accepts one stored product retrieval context lane only; use \
             --retrieval-only and omit predict/oracle/full/evidence/derived flags"
                .to_owned(),
        ));
    }
    if source_path == args.output {
        return Err(BenchError::InvalidInput(
            "--answer-report output must differ from its source report".to_owned(),
        ));
    }
    let resume_existing = args.resume && args.output.exists();
    if args.resume && !resume_existing {
        return Err(BenchError::InvalidInput(format!(
            "cannot resume missing answer report {}",
            args.output.display()
        )));
    }
    if args.output.exists() && !args.force && !resume_existing {
        return Err(BenchError::InvalidInput(format!(
            "{} already exists; pass --force or --resume",
            args.output.display()
        )));
    }
    let input_path = if resume_existing {
        args.output.as_path()
    } else {
        source_path
    };
    let text = std::fs::read_to_string(input_path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to read answer source report {}: {error}",
            input_path.display()
        ))
    })?;
    let mut report: RunReport =
        serde_json::from_str(&text).map_err(|error| BenchError::Parse(error.to_string()))?;
    if report.config.dataset != args.dataset || (!report.local_only && !resume_existing) {
        return Err(BenchError::InvalidInput(
            "answer source report dataset/locality differs".to_owned(),
        ));
    }
    if report
        .questions
        .iter()
        .any(|record| record.retrieval_context.is_none())
    {
        return Err(BenchError::InvalidInput(
            "answer source report has incomplete retrieval contexts".to_owned(),
        ));
    }
    if resume_existing {
        let expected_reflect_version = args
            .strong_reader_reflect
            .then(|| REFLECT_PROMPT_VERSION.to_owned());
        let exact_config = report.config.run_strong_reader == args.run_strong_reader
            && report.config.strong_reader_reflect == args.strong_reader_reflect
            && report.config.strong_reader_reflect_complex_only
                == args.strong_reader_reflect_complex_only
            && report.config.run_local_judge == args.run_local_judge
            && report.config.judge_backend
                == if args.frontier_judge {
                    "openai-compatible"
                } else {
                    "ollama"
                }
            && report.config.answer_prompt_version == ANSWER_PROMPT_VERSION
            && report.config.reflect_prompt_version == expected_reflect_version
            && report.config.judge_prompt_version == JUDGE_PROMPT_VERSION
            && report.config.baseline_reader_model == args.baseline_reader_model
            && report.config.strong_reader_model == args.strong_reader_model
            && report.config.judge_model == args.judge_model
            && report.config.reader_generation == args.reader_generation
            && report.config.judge_generation == args.judge_generation;
        if !exact_config {
            return Err(BenchError::InvalidInput(
                "answer report resume requires identical route, prompt, model, judge, and \
                 generation settings"
                    .to_owned(),
            ));
        }
    }

    let needs_ollama = !args.strong_reader_remote || (args.run_local_judge && !args.frontier_judge);
    let (ollama, ollama_version, model_digests) = if needs_ollama {
        let client = OllamaClient::new(&args.ollama_base_url, args.timeout_secs)?;
        let version = client.version()?;
        let mut requested_models = Vec::new();
        if !args.strong_reader_remote {
            requested_models.push(if args.run_strong_reader {
                args.strong_reader_model.as_str()
            } else {
                args.baseline_reader_model.as_str()
            });
        }
        if args.run_local_judge && !args.frontier_judge {
            requested_models.push(args.judge_model.as_str());
        }
        let digests = client.require_models(&requested_models)?;
        (Some(client), version, digests)
    } else {
        (
            None,
            "not-used-frontier-answer-report".to_owned(),
            BTreeMap::new(),
        )
    };
    let frontier_reader = args
        .strong_reader_remote
        .then(|| {
            let base_url = args.frontier_base_url.as_deref().ok_or_else(|| {
                BenchError::InvalidInput(
                    "--frontier-reader requires --frontier-base-url or LLM_BASE_URL".to_owned(),
                )
            })?;
            OpenAiCompatibleProvider::new(ProviderConfig {
                base_url: base_url.to_owned(),
                model: args.strong_reader_model.clone(),
                timeout_secs: args.timeout_secs,
                max_retries: 3,
                max_output_tokens: Some(PROVIDER_READER_MAX_OUTPUT_TOKENS),
                chat_template_enable_thinking: qwen_chat_template_thinking(
                    &args.strong_reader_model,
                    args.reader_generation.think,
                ),
            })
            .map_err(provider_error)
        })
        .transpose()?;
    let frontier_judge = args
        .frontier_judge
        .then(|| {
            let base_url = args.frontier_base_url.as_deref().ok_or_else(|| {
                BenchError::InvalidInput(
                    "--frontier-judge requires --frontier-base-url or LLM_BASE_URL".to_owned(),
                )
            })?;
            OpenAiCompatibleProvider::new(ProviderConfig {
                base_url: base_url.to_owned(),
                model: args.judge_model.clone(),
                timeout_secs: args.timeout_secs,
                max_retries: 3,
                max_output_tokens: Some(256),
                chat_template_enable_thinking: qwen_chat_template_thinking(
                    &args.judge_model,
                    args.judge_generation.think,
                ),
            })
            .map_err(provider_error)
        })
        .transpose()?;
    let paired_answer_report = if args.run_strong_reader {
        None
    } else {
        args.paired_answer_report
            .as_deref()
            .map(|path| load_paired_answer_report(path, &report, args, &model_digests))
            .transpose()?
    };
    let mut combined_model_digests = report.model_digests.clone();
    combined_model_digests.extend(model_digests);

    report.schema_version = SCHEMA_VERSION;
    if !resume_existing {
        report.run_id = format!(
            "local-answer-{}-answer-report-{}",
            report.config.dataset.as_str(),
            timestamp_secs()
        );
        report.created_at_unix = timestamp_secs();
    }
    report.completed_at_unix = None;
    report.ollama_base_url = args.ollama_base_url.clone();
    report.ollama_version = ollama_version;
    report.model_digests = combined_model_digests;
    report.local_only = frontier_lanes_are_local(args);
    report.config.run_strong_reader = args.run_strong_reader;
    report.config.strong_reader_reflect = args.strong_reader_reflect;
    report.config.strong_reader_reflect_complex_only = args.strong_reader_reflect_complex_only;
    report.config.strong_reader_backend = if args.strong_reader_remote {
        "openai-compatible".to_owned()
    } else {
        default_local_reader_backend()
    };
    report.config.run_full_context = false;
    report.config.run_local_judge = args.run_local_judge;
    report.config.judge_backend = if args.frontier_judge {
        "openai-compatible".to_owned()
    } else {
        default_local_judge_backend()
    };
    report.config.run_oracle_baseline = false;
    report.config.run_retrieval_baseline = true;
    report.config.predict_only = false;
    if report.config.context_render_style.is_empty() {
        report.config.context_render_style = "detailed".to_owned();
    }
    report.config.answer_prompt_version = ANSWER_PROMPT_VERSION.to_owned();
    report.config.reflect_prompt_version = args
        .strong_reader_reflect
        .then(|| REFLECT_PROMPT_VERSION.to_owned());
    report.config.judge_prompt_version = JUDGE_PROMPT_VERSION.to_owned();
    report.config.baseline_reader_model = args.baseline_reader_model.clone();
    report.config.strong_reader_model = args.strong_reader_model.clone();
    report.config.judge_model = args.judge_model.clone();
    report.config.reader_generation = args.reader_generation.clone();
    report.config.judge_generation = args.judge_generation.clone();
    report.config.paired_answer_report_fnv1a64 = paired_answer_report
        .as_ref()
        .map(|(_, fingerprint)| fingerprint.clone());
    if !resume_existing {
        for record in &mut report.questions {
            if args.run_strong_reader {
                record
                    .routes
                    .retain(|route, _| route == ROUTE_RETRIEVAL_BASELINE);
            } else {
                record.routes.clear();
            }
        }
    }
    let reused = paired_answer_report
        .as_ref()
        .map(|(paired, _)| reuse_identical_answers(&mut report, paired, args))
        .transpose()?
        .unwrap_or(0);
    if paired_answer_report.is_some() {
        eprintln!(
            "REUSE paired answers={reused} generated={} total={}",
            report.questions.len().saturating_sub(reused),
            report.questions.len()
        );
    }
    report.summary = None;
    write_report(&args.output, &report)?;

    let target_route = if args.run_strong_reader {
        ROUTE_RETRIEVAL_STRONG
    } else {
        ROUTE_RETRIEVAL_BASELINE
    };
    for record_index in 0..report.questions.len() {
        if report.questions[record_index]
            .routes
            .contains_key(target_route)
        {
            continue;
        }
        let context = report.questions[record_index]
            .retrieval_context
            .as_ref()
            .map(product_wire_prompt_context)
            .ok_or_else(|| BenchError::Parse("retrieval context disappeared".to_owned()))?;
        if let Some(provider) = frontier_reader.as_ref() {
            ensure_frontier_budget(&report, args)?;
            if should_reflect_question(args, &report.questions[record_index].question) {
                run_provider_reflect_answer(
                    &mut report,
                    record_index,
                    ROUTE_RETRIEVAL_STRONG,
                    provider,
                    &context,
                )?;
            } else {
                run_provider_answer(
                    &mut report,
                    record_index,
                    ROUTE_RETRIEVAL_STRONG,
                    provider,
                    &context,
                )?;
            }
        } else {
            let ollama = require_ollama(&ollama)?;
            let (route, reader_model) = if args.run_strong_reader {
                (ROUTE_RETRIEVAL_STRONG, args.strong_reader_model.as_str())
            } else {
                (
                    ROUTE_RETRIEVAL_BASELINE,
                    args.baseline_reader_model.as_str(),
                )
            };
            if should_reflect_question(args, &report.questions[record_index].question) {
                run_reflect_answer(
                    &mut report,
                    record_index,
                    route,
                    reader_model,
                    &context,
                    ollama,
                    &args.reader_generation,
                )?;
            } else {
                run_answer(
                    &mut report,
                    record_index,
                    route,
                    reader_model,
                    &context,
                    ollama,
                    &args.reader_generation,
                )?;
            }
        }
        write_report(&args.output, &report)?;
    }
    if args.run_local_judge {
        eprintln!("JUDGE PHASE questions={}", report.questions.len());
        for record_index in 0..report.questions.len() {
            let needs_judge = report.questions[record_index]
                .routes
                .get(target_route)
                .is_some_and(|route| route.judge.is_none());
            if needs_judge {
                ensure_frontier_budget(&report, args)?;
                if let Some(provider) = frontier_judge.as_ref() {
                    run_provider_judge(&mut report, record_index, target_route, provider)?;
                } else {
                    let ollama = require_ollama(&ollama)?;
                    run_judge(&mut report, record_index, target_route, ollama, args)?;
                }
                write_report(&args.output, &report)?;
            }
        }
    }
    report.summary = Some(build_summary(&report.questions));
    report.completed_at_unix = Some(timestamp_secs());
    write_report(&args.output, &report)?;
    print_summary(report.summary.as_ref());
    let (input_tokens, output_tokens, cost_usd) = frontier_usage(&report);
    if input_tokens > 0 || output_tokens > 0 {
        eprintln!(
            "FRONTIER USAGE input={} output={} gpt-4o-cost=${:.6}",
            input_tokens, output_tokens, cost_usd
        );
    }
    eprintln!("REPORT {}", args.output.display());
    Ok(())
}

fn load_paired_answer_report(
    path: &Path,
    source: &RunReport,
    args: &Args,
    current_model_digests: &BTreeMap<String, String>,
) -> BenchResult<(RunReport, String)> {
    let (_, fingerprint) = fingerprint(path)?;
    let text = std::fs::read_to_string(path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to read paired answer report {}: {error}",
            path.display()
        ))
    })?;
    let paired: RunReport =
        serde_json::from_str(&text).map_err(|error| BenchError::Parse(error.to_string()))?;
    if !paired.local_only
        || paired.config.dataset != source.config.dataset
        || paired.dataset_fnv1a64 != source.dataset_fnv1a64
        || paired.config.context_surface != source.config.context_surface
        || paired.config.answer_prompt_version != ANSWER_PROMPT_VERSION
        || paired.config.baseline_reader_model != args.baseline_reader_model
        || paired.config.reader_generation != args.reader_generation
    {
        return Err(BenchError::InvalidInput(
            "paired answer report dataset, context surface, prompt, reader, generation, or \
             fingerprint differs"
                .to_owned(),
        ));
    }
    let current_reader_digest = current_model_digests
        .get(&args.baseline_reader_model)
        .ok_or_else(|| BenchError::InvalidInput("current reader digest is missing".to_owned()))?;
    if paired.model_digests.get(&args.baseline_reader_model) != Some(current_reader_digest) {
        return Err(BenchError::InvalidInput(
            "paired answer report reader model digest differs from the current local model"
                .to_owned(),
        ));
    }
    Ok((paired, fingerprint))
}

fn reuse_identical_answers(
    report: &mut RunReport,
    paired: &RunReport,
    args: &Args,
) -> BenchResult<usize> {
    let mut paired_by_id = HashMap::with_capacity(paired.questions.len());
    for record in &paired.questions {
        if paired_by_id
            .insert(record.question_id.as_str(), record)
            .is_some()
        {
            return Err(BenchError::InvalidInput(
                "paired answer report contains duplicate question ids".to_owned(),
            ));
        }
    }
    let judge_compatible = args.run_local_judge
        && paired.config.run_local_judge
        && paired.config.judge_prompt_version == JUDGE_PROMPT_VERSION
        && paired.config.judge_model == args.judge_model
        && paired.config.judge_generation == args.judge_generation
        && paired.model_digests.get(&args.judge_model)
            == report.model_digests.get(&args.judge_model);
    let mut reused = 0usize;
    for record in &mut report.questions {
        let Some(previous) = paired_by_id.get(record.question_id.as_str()) else {
            continue;
        };
        if record.question != previous.question
            || record.expected_answer != previous.expected_answer
            || record.question_type != previous.question_type
            || record.question_date != previous.question_date
        {
            return Err(BenchError::InvalidInput(format!(
                "paired answer report question metadata differs for {}",
                record.question_id
            )));
        }
        let Some(current_context) = record
            .retrieval_context
            .as_ref()
            .map(product_wire_prompt_context)
        else {
            continue;
        };
        let Some(previous_context) = previous
            .retrieval_context
            .as_ref()
            .map(product_wire_prompt_context)
        else {
            continue;
        };
        if answer_prompt(record, &current_context) != answer_prompt(previous, &previous_context) {
            continue;
        }
        let Some(previous_route) = previous.routes.get(ROUTE_RETRIEVAL_BASELINE) else {
            continue;
        };
        if previous_route.reader_model != args.baseline_reader_model {
            return Err(BenchError::InvalidInput(format!(
                "paired answer report reader differs for {}",
                record.question_id
            )));
        }
        let mut route = previous_route.clone();
        let canonicalized = answer_metrics::canonicalize_standalone_iso_date(&route.answer);
        route.locomo_official_f1 = locomo_official_score(
            report.config.dataset,
            &record.question_type,
            &record.expected_answer,
            &route.answer,
        );
        route.locomo_reader_surface_f1 = locomo_official_score(
            report.config.dataset,
            &record.question_type,
            &record.expected_answer,
            &canonicalized,
        );
        route.canonicalized_answer = (canonicalized != route.answer).then_some(canonicalized);
        if !judge_compatible {
            route.judge = None;
        }
        route.reused_from_paired_report = true;
        record
            .routes
            .insert(ROUTE_RETRIEVAL_BASELINE.to_owned(), route);
        reused += 1;
    }
    Ok(reused)
}

fn run_judge_report(args: &Args, source_path: &Path) -> BenchResult<()> {
    if !args.run_local_judge
        || args.predict_only
        || args.run_full_context
        || args.run_strong_reader
        || args.evidence_context
        || args.derived_memory_artifact.is_some()
    {
        return Err(BenchError::InvalidInput(
            "--judge-report rejudges existing answers only; omit predict/full/strong/evidence/\
             derived flags and do not pass --skip-local-judge"
                .to_owned(),
        ));
    }
    if source_path == args.output {
        return Err(BenchError::InvalidInput(
            "--judge-report output must differ from its source report".to_owned(),
        ));
    }
    if args.output.exists() && !args.force {
        return Err(BenchError::InvalidInput(format!(
            "{} already exists; pass --force",
            args.output.display()
        )));
    }
    let text = std::fs::read_to_string(source_path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to read judge source report {}: {error}",
            source_path.display()
        ))
    })?;
    let mut report: RunReport =
        serde_json::from_str(&text).map_err(|error| BenchError::Parse(error.to_string()))?;
    if report.config.dataset != args.dataset || !report.local_only {
        return Err(BenchError::InvalidInput(
            "judge source report dataset/locality differs".to_owned(),
        ));
    }
    if report
        .questions
        .iter()
        .any(|record| record.routes.is_empty())
    {
        return Err(BenchError::InvalidInput(
            "judge source report contains unanswered questions".to_owned(),
        ));
    }

    let ollama = OllamaClient::new(&args.ollama_base_url, args.timeout_secs)?;
    let ollama_version = ollama.version()?;
    let judge_digest = ollama.require_models(&[args.judge_model.as_str()])?;

    report.schema_version = SCHEMA_VERSION;
    report.run_id = format!(
        "local-answer-{}-judge-report-{}",
        report.config.dataset.as_str(),
        timestamp_secs()
    );
    report.created_at_unix = timestamp_secs();
    report.completed_at_unix = None;
    report.ollama_base_url = args.ollama_base_url.clone();
    report.ollama_version = ollama_version;
    report.model_digests.extend(judge_digest);
    report.config.run_local_judge = true;
    report.config.judge_prompt_version = JUDGE_PROMPT_VERSION.to_owned();
    report.config.judge_model = args.judge_model.clone();
    report.config.judge_generation = args.judge_generation.clone();
    for record in &mut report.questions {
        for route in record.routes.values_mut() {
            route.judge = None;
        }
    }
    report.summary = None;
    write_report(&args.output, &report)?;

    for record_index in 0..report.questions.len() {
        let routes: Vec<_> = report.questions[record_index]
            .routes
            .keys()
            .cloned()
            .collect();
        for route in routes {
            run_judge(&mut report, record_index, &route, &ollama, args)?;
            write_report(&args.output, &report)?;
        }
    }
    report.summary = Some(build_summary(&report.questions));
    report.completed_at_unix = Some(timestamp_secs());
    write_report(&args.output, &report)?;
    print_summary(report.summary.as_ref());
    eprintln!("REPORT {}", args.output.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn load_or_create_report(
    args: &Args,
    loaded: &LoadedBenchmark,
    config: RunConfig,
    dataset_path: PathBuf,
    dataset_bytes: u64,
    dataset_fnv1a64: String,
    ollama_version: String,
    model_digests: BTreeMap<String, String>,
) -> BenchResult<RunReport> {
    if args.resume {
        let text = std::fs::read_to_string(&args.output).map_err(|err| {
            BenchError::InvalidInput(format!("failed to resume {}: {err}", args.output.display()))
        })?;
        let report: RunReport =
            serde_json::from_str(&text).map_err(|err| BenchError::Parse(err.to_string()))?;
        if report.schema_version != SCHEMA_VERSION
            || report.config != config
            || report.dataset_fnv1a64 != dataset_fnv1a64
        {
            return Err(BenchError::InvalidInput(
                "resume report configuration or dataset fingerprint differs".to_string(),
            ));
        }
        let selected: BTreeSet<_> = loaded
            .questions
            .iter()
            .map(|question| question.question_id.as_str())
            .collect();
        let recorded: BTreeSet<_> = report
            .questions
            .iter()
            .map(|question| question.question_id.as_str())
            .collect();
        if selected != recorded {
            return Err(BenchError::InvalidInput(
                "resume report question set differs".to_string(),
            ));
        }
        return Ok(report);
    }
    if args.output.exists() && !args.force {
        return Err(BenchError::InvalidInput(format!(
            "{} already exists; pass --force or --resume",
            args.output.display()
        )));
    }

    Ok(RunReport {
        schema_version: SCHEMA_VERSION,
        run_id: format!(
            "local-answer-{}-{}",
            config.dataset.as_str(),
            timestamp_secs()
        ),
        created_at_unix: timestamp_secs(),
        completed_at_unix: None,
        local_only: frontier_lanes_are_local(args),
        ollama_base_url: args.ollama_base_url.clone(),
        ollama_version,
        model_digests,
        dataset_path: dataset_path.display().to_string(),
        dataset_bytes,
        dataset_fnv1a64,
        config,
        questions: loaded
            .questions
            .iter()
            .map(|question| QuestionRecord {
                question_id: question.question_id.clone(),
                question: question.question.clone(),
                expected_answer: question.expected_answer.clone(),
                question_type: question.question_type.clone(),
                sample_index: question.sample_index,
                question_date: question.question_date.map(format_epoch_date),
                oracle_context: Vec::new(),
                retrieval_context: None,
                retrieval_evaluation: None,
                routes: BTreeMap::new(),
            })
            .collect(),
        summary: None,
    })
}

fn run_answer(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    reader_model: &str,
    context: &[PromptEvidence],
    ollama: &OllamaClient,
    generation: &GenerationOptions,
) -> BenchResult<()> {
    if !report.questions[record_index].routes.contains_key(route) {
        let record = &report.questions[record_index];
        eprintln!(
            "ANSWER {} {} model={} context={}",
            record.question_id,
            route,
            reader_model,
            context.len()
        );
        let prompt = answer_prompt(record, context);
        let start = Instant::now();
        let generated = ollama.generate(reader_model, &prompt, false, generation)?;
        if generated.content.is_empty() {
            return Err(BenchError::Parse(format!(
                "reader {reader_model} returned an empty final answer (done_reason={:?}, \
                 output_tokens={:?}, thinking_chars={}); raise --reader-num-predict or disable \
                 thinking",
                generated.done_reason, generated.output_eval_tokens, generated.thinking_chars
            )));
        }
        let answer_latency_ms = start.elapsed().as_secs_f64() * 1000.0;
        let locomo_official_f1 = locomo_official_score(
            report.config.dataset,
            &record.question_type,
            &record.expected_answer,
            &generated.content,
        );
        let canonicalized = answer_metrics::canonicalize_standalone_iso_date(&generated.content);
        let locomo_reader_surface_f1 = locomo_official_score(
            report.config.dataset,
            &record.question_type,
            &record.expected_answer,
            &canonicalized,
        );
        let canonicalized_answer = (canonicalized != generated.content).then_some(canonicalized);
        report.questions[record_index].routes.insert(
            route.to_string(),
            RouteResult {
                reader_model: reader_model.to_string(),
                answer: generated.content,
                answer_latency_ms,
                context_items: context.len(),
                context_chars: context.iter().map(|item| item.text.chars().count()).sum(),
                thinking_chars: generated.thinking_chars,
                done_reason: generated.done_reason,
                prompt_eval_tokens: generated.prompt_eval_tokens,
                output_eval_tokens: generated.output_eval_tokens,
                locomo_official_f1,
                locomo_reader_surface_f1,
                canonicalized_answer,
                reflection: None,
                reflection_latency_ms: None,
                judge: None,
                reused_from_paired_report: false,
            },
        );
    }
    Ok(())
}

fn inference_candidate_answer(query: &str, reflection: &str) -> Option<String> {
    if RecallPlan::infer(query).answer_shape != AnswerShape::Inference {
        return None;
    }
    reflection_candidate_answer(reflection)
}

fn reflection_candidate_answer(reflection: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(reflection).ok()?;
    reflection_answer_value(parsed.get("candidate_answer")?)
}

fn reflection_answer_value(value: &serde_json::Value) -> Option<String> {
    let answer = match value {
        serde_json::Value::String(value) => value.trim().to_owned(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(reflection_answer_value)
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null | serde_json::Value::Object(_) => return None,
    };
    (!answer.is_empty()).then_some(answer)
}

fn source_validated_collection_items(
    reflection: &str,
    context: &[PromptEvidence],
) -> Option<Vec<String>> {
    let allowed_source_ids: BTreeSet<_> = context
        .iter()
        .flat_map(|item| item.source_ids.iter().cloned())
        .collect();
    eval_common::reader_contract::validated_collection_items(reflection, &allowed_source_ids)
}

fn run_reflect_answer(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    reader_model: &str,
    context: &[PromptEvidence],
    ollama: &OllamaClient,
    generation: &GenerationOptions,
) -> BenchResult<()> {
    if report.questions[record_index].routes.contains_key(route) {
        return Ok(());
    }
    let record = &report.questions[record_index];
    eprintln!(
        "REFLECT {} {} model={} context={}",
        record.question_id,
        route,
        reader_model,
        context.len()
    );
    let reflection_start = Instant::now();
    let reflection = ollama.generate(
        reader_model,
        &reflection_prompt(record, context),
        true,
        generation,
    )?;
    let reflection_latency_ms = reflection_start.elapsed().as_secs_f64() * 1000.0;
    if reflection.content.is_empty() {
        return Err(BenchError::Parse(format!(
            "reflect reader {reader_model} returned an empty evidence analysis"
        )));
    }

    let (mut generated, answer_latency_ms) = if let Some(candidate) =
        inference_candidate_answer(&record.question, &reflection.content)
    {
        (
            GeneratedText {
                content: candidate,
                thinking_chars: 0,
                done_reason: Some("reflection-candidate".to_owned()),
                prompt_eval_tokens: None,
                output_eval_tokens: None,
            },
            0.0,
        )
    } else {
        let answer_start = Instant::now();
        let generated = ollama.generate(
            reader_model,
            &reflected_answer_prompt(record, context, &reflection.content),
            false,
            generation,
        )?;
        (generated, answer_start.elapsed().as_secs_f64() * 1000.0)
    };
    if RecallPlan::infer(&record.question).answer_shape == AnswerShape::Collection
        && let Some(items) = source_validated_collection_items(&reflection.content, context)
        && eval_common::reader_contract::collection_answer_misses_item(&generated.content, &items)
    {
        generated.content = items.join(", ");
        generated.done_reason = Some("reflection-source-completeness-backfill".to_owned());
    }
    if generated.content.is_empty() {
        return Err(BenchError::Parse(format!(
            "reflect reader {reader_model} returned an empty final answer"
        )));
    }

    let locomo_official_f1 = locomo_official_score(
        report.config.dataset,
        &record.question_type,
        &record.expected_answer,
        &generated.content,
    );
    let canonicalized = answer_metrics::canonicalize_standalone_iso_date(&generated.content);
    let locomo_reader_surface_f1 = locomo_official_score(
        report.config.dataset,
        &record.question_type,
        &record.expected_answer,
        &canonicalized,
    );
    let canonicalized_answer = (canonicalized != generated.content).then_some(canonicalized);
    report.questions[record_index].routes.insert(
        route.to_owned(),
        RouteResult {
            reader_model: reader_model.to_owned(),
            answer: generated.content,
            answer_latency_ms: answer_latency_ms + reflection_latency_ms,
            context_items: context.len(),
            context_chars: context.iter().map(|item| item.text.chars().count()).sum(),
            thinking_chars: reflection.thinking_chars + generated.thinking_chars,
            done_reason: generated.done_reason,
            prompt_eval_tokens: sum_optional_counts(
                reflection.prompt_eval_tokens,
                generated.prompt_eval_tokens,
            ),
            output_eval_tokens: sum_optional_counts(
                reflection.output_eval_tokens,
                generated.output_eval_tokens,
            ),
            locomo_official_f1,
            locomo_reader_surface_f1,
            canonicalized_answer,
            reflection: Some(reflection.content),
            reflection_latency_ms: Some(reflection_latency_ms),
            judge: None,
            reused_from_paired_report: false,
        },
    );
    Ok(())
}

fn sum_optional_counts(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn run_provider_answer(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    provider: &dyn LlmProvider,
    context: &[PromptEvidence],
) -> BenchResult<()> {
    if report.questions[record_index].routes.contains_key(route) {
        return Ok(());
    }
    let record = &report.questions[record_index];
    eprintln!(
        "FRONTIER ANSWER {} {} model={} context={}",
        record.question_id,
        route,
        provider.name(),
        context.len()
    );
    let start = Instant::now();
    let generation = provider
        .generate_with_usage(&answer_prompt(record, context))
        .map_err(provider_error)?;
    let answer = generation.content.trim().to_owned();
    if answer.is_empty() {
        return Err(BenchError::Parse(format!(
            "frontier reader {} returned an empty final answer",
            provider.name()
        )));
    }
    let answer_latency_ms = start.elapsed().as_secs_f64() * 1000.0;
    insert_provider_route(
        report,
        record_index,
        route,
        provider.name(),
        context,
        answer,
        answer_latency_ms,
        None,
        None,
        generation.prompt_tokens,
        generation.completion_tokens,
    );
    Ok(())
}

fn run_provider_reflect_answer(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    provider: &dyn LlmProvider,
    context: &[PromptEvidence],
) -> BenchResult<()> {
    if report.questions[record_index].routes.contains_key(route) {
        return Ok(());
    }
    let record = &report.questions[record_index];
    eprintln!(
        "FRONTIER REFLECT {} {} model={} context={}",
        record.question_id,
        route,
        provider.name(),
        context.len()
    );
    let reflection_start = Instant::now();
    let reflection_generation = provider
        .generate_with_usage(&reflection_prompt(record, context))
        .map_err(provider_error)?;
    let reflection = reflection_generation.content.trim().to_owned();
    let reflection_latency_ms = reflection_start.elapsed().as_secs_f64() * 1000.0;
    if reflection.is_empty() {
        return Err(BenchError::Parse(format!(
            "frontier reflect reader {} returned an empty evidence analysis",
            provider.name()
        )));
    }
    let (mut answer, answer_latency_ms, answer_prompt_tokens, answer_output_tokens) =
        if let Some(candidate) = inference_candidate_answer(&record.question, &reflection) {
            (candidate, 0.0, None, None)
        } else {
            let answer_start = Instant::now();
            let generation = provider
                .generate_with_usage(&reflected_answer_prompt(record, context, &reflection))
                .map_err(provider_error)?;
            (
                generation.content.trim().to_owned(),
                answer_start.elapsed().as_secs_f64() * 1000.0,
                generation.prompt_tokens,
                generation.completion_tokens,
            )
        };
    if RecallPlan::infer(&record.question).answer_shape == AnswerShape::Collection
        && let Some(items) = source_validated_collection_items(&reflection, context)
        && eval_common::reader_contract::collection_answer_misses_item(&answer, &items)
    {
        answer = items.join(", ");
    }
    if answer.is_empty() {
        return Err(BenchError::Parse(format!(
            "frontier reflect reader {} returned an empty final answer",
            provider.name()
        )));
    }
    insert_provider_route(
        report,
        record_index,
        route,
        provider.name(),
        context,
        answer,
        answer_latency_ms + reflection_latency_ms,
        Some(reflection),
        Some(reflection_latency_ms),
        sum_optional_counts(reflection_generation.prompt_tokens, answer_prompt_tokens),
        sum_optional_counts(
            reflection_generation.completion_tokens,
            answer_output_tokens,
        ),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_provider_route(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    reader_model: &str,
    context: &[PromptEvidence],
    answer: String,
    answer_latency_ms: f64,
    reflection: Option<String>,
    reflection_latency_ms: Option<f64>,
    prompt_eval_tokens: Option<u64>,
    output_eval_tokens: Option<u64>,
) {
    let record = &report.questions[record_index];
    let locomo_official_f1 = locomo_official_score(
        report.config.dataset,
        &record.question_type,
        &record.expected_answer,
        &answer,
    );
    let canonicalized = answer_metrics::canonicalize_standalone_iso_date(&answer);
    let locomo_reader_surface_f1 = locomo_official_score(
        report.config.dataset,
        &record.question_type,
        &record.expected_answer,
        &canonicalized,
    );
    let canonicalized_answer = (canonicalized != answer).then_some(canonicalized);
    report.questions[record_index].routes.insert(
        route.to_owned(),
        RouteResult {
            reader_model: reader_model.to_owned(),
            answer,
            answer_latency_ms,
            context_items: context.len(),
            context_chars: context.iter().map(|item| item.text.chars().count()).sum(),
            thinking_chars: 0,
            done_reason: None,
            prompt_eval_tokens,
            output_eval_tokens,
            locomo_official_f1,
            locomo_reader_surface_f1,
            canonicalized_answer,
            reflection,
            reflection_latency_ms,
            judge: None,
            reused_from_paired_report: false,
        },
    );
}

fn provider_error(error: ProviderError) -> BenchError {
    BenchError::InvalidInput(format!("frontier provider request failed: {error}"))
}

fn qwen_chat_template_thinking(model: &str, enabled: bool) -> Option<bool> {
    model
        .to_ascii_lowercase()
        .contains("qwen")
        .then_some(enabled)
}

fn frontier_lanes_are_local(args: &Args) -> bool {
    let loopback = args
        .frontier_base_url
        .as_deref()
        .is_some_and(is_loopback_url);
    (!args.strong_reader_remote || loopback) && (!args.frontier_judge || loopback)
}

fn is_loopback_url(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

fn run_provider_judge(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    provider: &dyn LlmProvider,
) -> BenchResult<()> {
    let needs_judge = report.questions[record_index]
        .routes
        .get(route)
        .is_some_and(|result| result.judge.is_none());
    if !needs_judge {
        return Ok(());
    }
    let record = &report.questions[record_index];
    let answer = record
        .routes
        .get(route)
        .map(|result| result.answer.clone())
        .ok_or_else(|| BenchError::Parse(format!("route {route} disappeared")))?;
    eprintln!(
        "FRONTIER JUDGE {} {} model={}",
        record.question_id,
        route,
        provider.name()
    );
    let prompt = judge_prompt(record, &answer);
    let start = Instant::now();
    let generation = provider
        .generate_with_usage(&prompt)
        .map_err(provider_error)?;
    if generation.content.trim().is_empty() {
        return Err(BenchError::Parse(format!(
            "frontier judge {} returned an empty response",
            provider.name()
        )));
    }
    let generated = GeneratedText {
        content: generation.content,
        thinking_chars: 0,
        done_reason: None,
        prompt_eval_tokens: generation.prompt_tokens,
        output_eval_tokens: generation.completion_tokens,
    };
    let decision = parse_judge(
        provider.name(),
        generated,
        start.elapsed().as_secs_f64() * 1000.0,
    );
    if let Some(result) = report.questions[record_index].routes.get_mut(route) {
        result.judge = Some(decision);
    }
    Ok(())
}

fn frontier_usage(report: &RunReport) -> (u64, u64, f64) {
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    for record in &report.questions {
        for route in record.routes.values() {
            if route.reader_model == "gpt-4o" {
                input_tokens =
                    input_tokens.saturating_add(route.prompt_eval_tokens.unwrap_or_default());
                output_tokens =
                    output_tokens.saturating_add(route.output_eval_tokens.unwrap_or_default());
            }
            if let Some(judge) = &route.judge
                && judge.judge_model == "gpt-4o"
            {
                input_tokens =
                    input_tokens.saturating_add(judge.prompt_eval_tokens.unwrap_or_default());
                output_tokens =
                    output_tokens.saturating_add(judge.output_eval_tokens.unwrap_or_default());
            }
        }
    }
    let cost_usd =
        input_tokens as f64 * 2.50 / 1_000_000.0 + output_tokens as f64 * 10.0 / 1_000_000.0;
    (input_tokens, output_tokens, cost_usd)
}

fn ensure_frontier_budget(report: &RunReport, args: &Args) -> BenchResult<()> {
    let Some(max_cost_usd) = args.frontier_max_cost_usd else {
        return Ok(());
    };
    let (input_tokens, output_tokens, cost_usd) = frontier_usage(report);
    if cost_usd >= max_cost_usd {
        return Err(BenchError::InvalidInput(format!(
            "frontier cost stop reached: ${cost_usd:.6} >= ${max_cost_usd:.6} \
             ({input_tokens} input, {output_tokens} output tokens)"
        )));
    }
    Ok(())
}

fn run_judge(
    report: &mut RunReport,
    record_index: usize,
    route: &str,
    ollama: &OllamaClient,
    args: &Args,
) -> BenchResult<()> {
    let needs_judge = report.questions[record_index]
        .routes
        .get(route)
        .is_some_and(|result| result.judge.is_none());
    if needs_judge {
        write_report(&args.output, report)?;
        let record = &report.questions[record_index];
        let answer = record
            .routes
            .get(route)
            .map(|result| result.answer.clone())
            .ok_or_else(|| BenchError::Parse(format!("route {route} disappeared")))?;
        eprintln!(
            "JUDGE {} {} model={}",
            record.question_id, route, args.judge_model
        );
        let prompt = judge_prompt(record, &answer);
        let start = Instant::now();
        let generated =
            ollama.generate(&args.judge_model, &prompt, true, &args.judge_generation)?;
        if generated.content.is_empty() {
            return Err(BenchError::Parse(format!(
                "judge {} returned an empty response (done_reason={:?})",
                args.judge_model, generated.done_reason
            )));
        }
        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
        let decision = parse_judge(&args.judge_model, generated, latency_ms);
        if let Some(result) = report.questions[record_index].routes.get_mut(route) {
            result.judge = Some(decision);
        }
    }
    Ok(())
}

#[derive(Clone)]
struct PromptEvidence {
    label: String,
    text: String,
    raw_turn_id: Option<String>,
    source_ids: Vec<String>,
}

fn oracle_prompt_context(evidence: &[OracleEvidence]) -> Vec<PromptEvidence> {
    evidence
        .iter()
        .enumerate()
        .map(|(index, item)| PromptEvidence {
            label: format!(
                "gold-{} session={} date={} speaker={} turn={}",
                index + 1,
                item.raw_session_id,
                item.date.as_deref().unwrap_or("unknown"),
                item.speaker,
                item.raw_turn_id.as_deref().unwrap_or("unknown")
            ),
            text: item.content.clone(),
            raw_turn_id: item.raw_turn_id.clone(),
            source_ids: item.raw_turn_id.iter().cloned().collect(),
        })
        .collect()
}

fn product_wire_prompt_context(context: &AnswerContext) -> Vec<PromptEvidence> {
    let mut source_ids = BTreeSet::new();
    for item in &context.evidence {
        source_ids.insert(format!("node:{}", item.node_id));
        if let Some(raw_turn_id) = &item.raw_turn_id {
            source_ids.insert(raw_turn_id.clone());
        }
    }
    vec![PromptEvidence {
        label: "anamnesis-product-context".to_string(),
        text: context.product_context.clone(),
        raw_turn_id: None,
        source_ids: source_ids.into_iter().collect(),
    }]
}

fn diagnostic_retrieval_prompt_context(
    group: &LoadedBenchmark,
    context: &AnswerContext,
) -> Vec<PromptEvidence> {
    context
        .evidence
        .iter()
        .map(|item| {
            let date = item
                .raw_session_id
                .as_deref()
                .and_then(|raw_id| {
                    group
                        .sessions
                        .iter()
                        .find(|session| session.raw_session_id == raw_id)
                })
                .and_then(|session| session.start_timestamp)
                .map(format_epoch_date);
            PromptEvidence {
                label: format!(
                    "retrieved-{} score={:.6} session={} date={} turn={}",
                    item.rank,
                    item.score,
                    item.raw_session_id.as_deref().unwrap_or("unknown"),
                    date.as_deref().unwrap_or("unknown"),
                    item.raw_turn_id.as_deref().unwrap_or("unknown")
                ),
                text: item.text.clone(),
                raw_turn_id: item.raw_turn_id.clone(),
                source_ids: item
                    .raw_turn_id
                    .iter()
                    .cloned()
                    .chain(std::iter::once(format!("node:{}", item.node_id)))
                    .collect(),
            }
        })
        .collect()
}

fn compact_prompt_context(context: Vec<PromptEvidence>) -> Vec<PromptEvidence> {
    let mut best_by_turn: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (index, item) in context.iter().enumerate() {
        let Some(raw_turn_id) = item.raw_turn_id.as_ref() else {
            continue;
        };
        let text_len = item.text.chars().count();
        best_by_turn
            .entry(raw_turn_id.clone())
            .and_modify(|best| {
                if text_len > best.1 {
                    *best = (index, text_len);
                }
            })
            .or_insert((index, text_len));
    }
    context
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let keep = item.raw_turn_id.as_ref().is_none_or(|raw_turn_id| {
                best_by_turn
                    .get(raw_turn_id)
                    .is_none_or(|(best_index, _)| *best_index == index)
            });
            keep.then_some(item)
        })
        .collect()
}

fn hydrate_episodic_context(group: &LoadedBenchmark, context: &mut [PromptEvidence]) {
    for item in context
        .iter_mut()
        .filter(|item| is_synthetic_turn_label(&item.text))
    {
        let Some(raw_turn_id) = item.raw_turn_id.as_deref() else {
            continue;
        };
        let source = group
            .sessions
            .iter()
            .flat_map(|session| session.turns.iter())
            .find(|turn| turn.raw_turn_id.as_deref() == Some(raw_turn_id));
        if let Some(source) = source {
            item.text = source.content.clone();
        }
    }
}

fn is_synthetic_turn_label(text: &str) -> bool {
    text.rsplit_once(" turn ").is_some_and(|(speaker, index)| {
        !speaker.is_empty() && !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn full_prompt_context(group: &LoadedBenchmark) -> Vec<PromptEvidence> {
    group
        .sessions
        .iter()
        .flat_map(|session| {
            let date = session.start_timestamp.map(format_epoch_date);
            session.turns.iter().map(move |turn| PromptEvidence {
                label: format!(
                    "history session={} date={} speaker={} turn={}",
                    session.raw_session_id,
                    date.as_deref().unwrap_or("unknown"),
                    turn.speaker,
                    turn.raw_turn_id.as_deref().unwrap_or("unknown")
                ),
                text: turn.content.clone(),
                raw_turn_id: turn.raw_turn_id.clone(),
                source_ids: turn.raw_turn_id.iter().cloned().collect(),
            })
        })
        .collect()
}

fn answer_prompt(record: &QuestionRecord, context: &[PromptEvidence]) -> String {
    let mut prompt = String::from(
        "You are a memory question-answering reader. Use only the supplied evidence. \
         Think briefly and reserve enough output budget for the final answer. \
         Combine multiple evidence items and reason about dates when needed. \
         When the query-derived plan requests inference and the evidence supplies a relevant \
         personal premise, make the shortest stable ordinary-world bridge to the requested \
         conclusion; do not abstain merely because that conclusion is implicit. Never invent a \
         new personal event or preference. \
         Give only the shortest direct answer that fully answers the question. \
         Do not mention the evidence or explain your reasoning. \
         If the evidence is insufficient, answer exactly No information available.\n\n",
    );
    prompt.push_str(&query_plan_instruction(&record.question));
    prompt.push('\n');
    if let Some(date) = &record.question_date {
        prompt.push_str(&format!("Question date: {date}\n"));
    }
    append_prompt_evidence(&mut prompt, context);
    prompt.push_str("\nQuestion: ");
    prompt.push_str(&record.question);
    prompt.push_str("\nAnswer:");
    prompt
}

fn reflection_prompt(record: &QuestionRecord, context: &[PromptEvidence]) -> String {
    let mut prompt = String::from(
        "You are the evidence-analysis stage of a memory system. Use only the supplied evidence. \
         Do not invent personal facts absent from the evidence. You may use stable, ordinary \
         world knowledge only to connect an explicit evidence fact to the question, such as a \
         city's state or a commonplace implication; identify that bridge in reasoning_chain. \
         Inspect every item internally, but write only evidence that directly helps answer the \
         question. Return one complete JSON object of at most 450 tokens with these keys: \
         required_slots, evidence_findings, reasoning_chain, answer_items, candidate_answer, \
         missing_or_ambiguous. For collection questions, answer_items must contain one object per \
         distinct final item with keys value and source_ids; source_ids must be a non-empty array \
         of at most three ids copied exactly from the supplied evidence labels. Use an empty \
         answer_items array for other answer shapes. Keep each required slot under eight words. \
         Include at most six distinct supporting facts in evidence_findings, with one short \
         sentence per fact; preserve names, negation, quantities, source dates, and who said or \
         did each thing. Use at most four short \
         reasoning_chain steps to connect those findings without inventing a personal event. Do \
         not summarize irrelevant evidence and do not repeat a fact in multiple fields. \
         For counts and lists, enumerate distinct completed evidence-backed items before forming \
         candidate_answer; exclude plans, intentions, hypothetical events, and duplicate mentions \
         of the same event unless the question explicitly asks about them. For relative dates \
         inside evidence, resolve them against that evidence item's date, not the question date. \
         If an item says an activity has continued for a duration, subtract that duration from \
         the item's date to infer its start. If a duration is requested across multiple items, \
         identify the earliest start and latest completion for the same event and calculate the \
         elapsed time. When the question asks what is likely, might, could, or whether something \
         is true, record the best-supported plausible implication rather than requiring explicit \
         confirmation. Prefer a premise that the evidence explicitly links as a reason, goal, \
         preference, or consequence over an unrelated co-occurring skill or event. Preserve all \
         equally plausible ordinary-world alternatives when the evidence does not distinguish \
         them. For yes/no conclusions, include the shortest supporting entity or fact in \
         candidate_answer. \
         Treat paraphrases and references such as home country, partner, or that event as slots \
         to resolve to a specific value whenever the evidence permits. No benchmark annotation, \
         reference answer, or judge feedback is available. Finish the JSON before the output \
         limit; brevity is mandatory.\n\n",
    );
    prompt.push_str(&query_plan_instruction(&record.question));
    prompt.push('\n');
    if let Some(date) = &record.question_date {
        prompt.push_str(&format!("Question date: {date}\n"));
    }
    append_prompt_evidence(&mut prompt, context);
    prompt.push_str("\nQuestion: ");
    prompt.push_str(&record.question);
    prompt.push_str("\nEvidence analysis JSON:");
    prompt
}

fn reflected_answer_prompt(
    record: &QuestionRecord,
    context: &[PromptEvidence],
    reflection: &str,
) -> String {
    let mut prompt = String::from(
        "You are the final verification stage of a memory system. Use only the supplied evidence. \
         The draft analysis is untrusted: check every claim against the evidence and correct it. \
         Make sure the final answer fills every slot requested by the question. For lists and \
         counts, include every distinct completed supported item, excluding mere plans and \
         duplicate mentions. For temporal questions, resolve relative expressions and elapsed \
         durations against the dated evidence items and preserve the requested granularity. \
         Resolve descriptive references to their specific names, places, \
         events, or values when the evidence supplies them. Match the semantic type requested by \
         the question; for example, normalize an evidenced city to its containing country when \
         the question asks for a country. A concise commonsense implication is \
         allowed when the query-derived plan requests inference; do not abstain merely because the \
         conclusion is implicit when the draft gives a complete evidence-grounded chain. Stable \
         ordinary world knowledge may bridge an explicit evidence fact to that conclusion, but \
         must not supply a new personal fact. Answer No information available only when neither \
         the evidence nor a valid grounded implication bears on the requested answer. Prefer the \
         exact wording and specificity of the evidence over a vague paraphrase. Give only the \
         shortest direct final answer, with no explanation and no mention of the analysis or \
         evidence.\n\n",
    );
    prompt.push_str(&query_plan_instruction(&record.question));
    prompt.push('\n');
    if let Some(date) = &record.question_date {
        prompt.push_str(&format!("Question date: {date}\n"));
    }
    append_prompt_evidence(&mut prompt, context);
    prompt.push_str("\nDraft evidence analysis:\n");
    prompt.push_str(reflection);
    prompt.push_str("\n\nQuestion: ");
    prompt.push_str(&record.question);
    prompt.push_str("\nFinal answer:");
    prompt
}

fn append_prompt_evidence(prompt: &mut String, context: &[PromptEvidence]) {
    prompt.push_str("Evidence:\n");
    for item in context {
        let source_ids = if item.source_ids.is_empty() {
            "unlabeled".to_owned()
        } else {
            item.source_ids.join(",")
        };
        prompt.push_str(&format!(
            "[{} source_ids={}]\n{}\n",
            item.label, source_ids, item.text
        ));
    }
}

fn query_plan_instruction(query: &str) -> String {
    let plan = RecallPlan::infer(query);
    let answer_instruction = match plan.answer_shape {
        AnswerShape::Temporal => {
            "For this temporal question, resolve a relative expression inside an evidence item \
             against that same evidence item's date. Do not resolve evidence text against the \
             Question date. Use the Question date only for a relative expression in the question \
             itself. Subtract stated elapsed durations to infer starts, or compare the dated start \
             and completion of the same event when the question asks how long it took. For a \
             frequency question, order repeated instances of the same event by their evidence \
             dates and state the supported cadence rather than merely counting the instances. \
             Preserve the answer's requested granularity. State the resulting date, date range, \
             duration, or frequency directly in natural language; do not copy a value from these \
             instructions."
        }
        AnswerShape::Frequency => {
            "For this frequency question, identify repeated instances of the same requested event, \
             order them by their evidence dates, and infer the supported cadence from consecutive \
             intervals. Do not substitute a raw event count for the frequency, and do not mix in \
             similarly named events about another person."
        }
        AnswerShape::Count => {
            "Enumerate the distinct evidence-backed events or entities internally, deduplicate \
             repeated descriptions of the same item, exclude plans or hypothetical future events, \
             and answer with the resulting count."
        }
        AnswerShape::Collection => {
            "Return every distinct evidence-backed item requested by the question. Deduplicate \
             paraphrases of the same item and separate final items with commas."
        }
        AnswerShape::Relationship => {
            "Combine the evidence needed to state the requested relationship, comparison, reason, \
             or causal connection. Do not stop at an intermediate fact."
        }
        AnswerShape::Inference => {
            "Derive the shortest reasonable conclusion whose premises are all present in the \
             evidence. The question asks for a plausible implication, not explicit confirmation. \
             Prefer evidence explicitly linked as a reason, goal, preference, or consequence over \
             unrelated co-occurring facts. Stable ordinary world knowledge may connect an \
             explicit evidence fact to the conclusion, and equally plausible alternatives should \
             all be preserved when the evidence cannot distinguish them. State the requested \
             conclusion, not only its intermediate premises, and do not abstain solely because \
             the conclusion was implicit. For yes/no conclusions, include the shortest supporting \
             entity or fact."
        }
        AnswerShape::Fact => {
            "Extract the exact requested fact and preserve names, numbers, units, specificity, \
             and negation. Match the semantic type requested by the question: if the evidence \
             supplies a city but the question asks for its country, return the containing country."
        }
        _ => "Answer in the shape requested by the question using only supplied evidence.",
    };
    let evidence_instruction = match plan.recall_intent {
        RecallIntent::Enumeration => {
            "Combine distinct relevant evidence items before answering; repeated source windows \
             are not additional facts."
        }
        RecallIntent::Relational => {
            "Trace the shortest complete chain between the named entities, events, or causes and \
             verify each link against the evidence."
        }
        RecallIntent::Temporal => {
            "Keep event time, observation time, and relative temporal expressions separate."
        }
        RecallIntent::Direct => {
            "Prefer the most specific directly supported value; use multiple items only when they \
             disambiguate or resolve a reference."
        }
        _ => "Use the smallest complete set of relevant evidence.",
    };
    format!(
        "Query-derived reader plan: answer_shape={:?}, recall_intent={:?}. {answer_instruction} \
         {evidence_instruction}",
        plan.answer_shape, plan.recall_intent
    )
}

fn should_reflect_question(args: &Args, query: &str) -> bool {
    if !args.strong_reader_reflect {
        return false;
    }
    if !args.strong_reader_reflect_complex_only {
        return true;
    }
    let plan = RecallPlan::infer(query);
    matches!(
        plan.answer_shape,
        AnswerShape::Count
            | AnswerShape::Collection
            | AnswerShape::Frequency
            | AnswerShape::Inference
            | AnswerShape::Relationship
    )
}

fn judge_prompt(record: &QuestionRecord, answer: &str) -> String {
    let criterion = if record.question_id.contains("_abs") {
        "The question is unanswerable. Mark correct if the candidate identifies that the \
         requested information is unavailable or incomplete, even if it also mentions related \
         information."
    } else {
        match record.question_type.as_str() {
            "temporal-reasoning" => {
                "Mark correct if the candidate contains the correct answer or an equivalent \
                 answer. Do not penalize an off-by-one error when the requested result is a \
                 number of days, weeks, months, or similar duration. Missing required answer \
                 information is incorrect."
            }
            "knowledge-update" => {
                "Mark correct if the candidate contains the required updated answer. Older \
                 information may also appear and must not cause rejection when the updated \
                 answer is present."
            }
            "preference" | "single-session-preference" => {
                "The reference is a personalization rubric. Mark correct when the candidate \
                 correctly recalls and uses the user's personal information; it need not cover \
                 every point in the rubric."
            }
            _ => {
                "Mark correct only if the candidate directly states the correct answer or a \
                 semantically equivalent answer. Intermediate facts without the requested \
                 conclusion, and responses containing only a subset of required information, \
                 are incorrect."
            }
        }
    };
    format!(
        "You are an impartial benchmark judge. {criterion} Treat the reference as an answer key, \
         not as required wording. When a yes/no or either/or candidate directly gives the same \
         conclusion as the reference, do not require an explanatory clause that appears only \
         after the reference conclusion. Questions that explicitly ask why or how still require \
         the requested reason or method, and list or count questions still require all essential \
         items. Return one JSON object only, with \
         keys verdict (\"correct\" or \"incorrect\"), confidence (0 to 1), and reason (one short \
         sentence).\n\nQuestion: {}\nReference answer: {}\nCandidate answer: {}",
        record.question, record.expected_answer, answer
    )
}

fn parse_judge(model: &str, generated: GeneratedText, latency_ms: f64) -> JudgeDecision {
    let raw_response = generated.content;
    let parsed = extract_json_object(&raw_response)
        .and_then(|json| serde_json::from_str::<JudgeJson>(json).map_err(|err| err.to_string()));
    match parsed {
        Ok(value) => {
            let verdict = value.verdict.trim().to_lowercase();
            let correct = match verdict.as_str() {
                "correct" => Some(true),
                "incorrect" => Some(false),
                _ => None,
            };
            JudgeDecision {
                judge_model: model.to_string(),
                correct,
                confidence: value.confidence.map(|value| value.clamp(0.0, 1.0)),
                reason: value.reason,
                raw_response,
                parse_error: correct
                    .is_none()
                    .then(|| format!("unknown verdict {:?}", value.verdict)),
                latency_ms,
                done_reason: generated.done_reason,
                prompt_eval_tokens: generated.prompt_eval_tokens,
                output_eval_tokens: generated.output_eval_tokens,
            }
        }
        Err(error) => JudgeDecision {
            judge_model: model.to_string(),
            correct: None,
            confidence: None,
            reason: String::new(),
            raw_response,
            parse_error: Some(error),
            latency_ms,
            done_reason: generated.done_reason,
            prompt_eval_tokens: generated.prompt_eval_tokens,
            output_eval_tokens: generated.output_eval_tokens,
        },
    }
}

fn extract_json_object(value: &str) -> Result<&str, String> {
    let start = value
        .find('{')
        .ok_or_else(|| "judge response contains no JSON object".to_string())?;
    let end = value
        .rfind('}')
        .ok_or_else(|| "judge response contains no closing brace".to_string())?;
    if end < start {
        return Err("judge response JSON braces are reversed".to_string());
    }
    Ok(&value[start..=end])
}

fn oracle_context(dataset: &LoadedBenchmark, question: &BenchQuestion) -> Vec<OracleEvidence> {
    dataset
        .sessions
        .iter()
        .flat_map(|session| {
            session
                .turns
                .iter()
                .filter(move |turn| {
                    oracle_turn_matches(
                        &question.gold,
                        session,
                        turn.raw_turn_id.as_deref(),
                        &turn.content,
                    )
                })
                .map(move |turn| OracleEvidence {
                    session_id: session.session_id.clone(),
                    raw_session_id: session.raw_session_id.clone(),
                    raw_turn_id: turn.raw_turn_id.clone(),
                    turn_index: turn.turn_index,
                    speaker: turn.speaker.clone(),
                    date: session.start_timestamp.map(format_epoch_date),
                    content: turn.content.clone(),
                })
        })
        .collect()
}

fn oracle_turn_matches(
    gold: &GoldEvidence,
    session: &BenchSession,
    raw_turn_id: Option<&str>,
    content: &str,
) -> bool {
    if !gold.evidence_turn_ids.is_empty() {
        return raw_turn_id.is_some_and(|turn_id| {
            gold.evidence_turn_ids
                .iter()
                .any(|evidence| evidence == turn_id)
        });
    }
    if !gold.answer_session_ids.is_empty() {
        return gold
            .answer_session_ids
            .iter()
            .any(|evidence| evidence == &session.raw_session_id);
    }
    if !gold.evidence_session_ids.is_empty() {
        return gold
            .evidence_session_ids
            .iter()
            .any(|evidence| evidence == &session.raw_session_id);
    }
    let normalized = content.to_lowercase();
    gold.answer_needles
        .iter()
        .any(|needle| normalized.contains(needle))
}

fn locomo_official_score(
    dataset: BenchDatasetName,
    question_type: &str,
    reference: &str,
    prediction: &str,
) -> Option<f64> {
    (dataset == BenchDatasetName::Locomo)
        .then(|| answer_metrics::locomo_official_score(question_type, reference, prediction))
        .flatten()
}

fn require_ollama(client: &Option<OllamaClient>) -> BenchResult<&OllamaClient> {
    client.as_ref().ok_or_else(|| {
        BenchError::InvalidInput(
            "answer or judge route requested while --predict-only is active".to_string(),
        )
    })
}

impl OllamaClient {
    fn new(base_url: &str, timeout_secs: u64) -> BenchResult<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|err| BenchError::InvalidInput(err.to_string()))?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            timeout_secs,
        })
    }

    fn version(&self) -> BenchResult<String> {
        let response = self
            .http
            .get(format!("{}/api/version", self.base_url))
            .send()
            .map_err(|err| {
                BenchError::InvalidInput(format!("local Ollama is not reachable: {err}"))
            })?
            .error_for_status()
            .map_err(|err| BenchError::InvalidInput(format!("Ollama version failed: {err}")))?
            .json::<OllamaVersionResponse>()
            .map_err(|err| BenchError::Parse(format!("Ollama version response: {err}")))?;
        Ok(response.version)
    }

    fn require_models(&self, requested: &[&str]) -> BenchResult<BTreeMap<String, String>> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .map_err(|err| BenchError::InvalidInput(format!("Ollama tags failed: {err}")))?
            .error_for_status()
            .map_err(|err| BenchError::InvalidInput(format!("Ollama tags failed: {err}")))?
            .json::<OllamaTagsResponse>()
            .map_err(|err| BenchError::Parse(format!("Ollama tags response: {err}")))?;
        let mut digests = BTreeMap::new();
        for requested_name in requested {
            let requested_normalized = normalize_model_name(requested_name);
            let Some(found) = response.models.iter().find(|tag| {
                normalize_model_name(&tag.name) == requested_normalized
                    || normalize_model_name(&tag.model) == requested_normalized
            }) else {
                return Err(BenchError::InvalidInput(format!(
                    "local model {requested_name:?} is not installed; run `ollama pull {requested_name}`"
                )));
            };
            digests.insert((*requested_name).to_string(), found.digest.clone());
        }
        Ok(digests)
    }

    fn generate(
        &self,
        model: &str,
        prompt: &str,
        json: bool,
        generation: &GenerationOptions,
    ) -> BenchResult<GeneratedText> {
        let mut body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": false,
            "think": generation.think,
            "keep_alive": "10m",
            "options": {
                "temperature": generation.temperature,
                "top_p": generation.top_p,
                "top_k": generation.top_k,
                "presence_penalty": generation.presence_penalty,
                "seed": generation.seed,
                "num_ctx": generation.num_ctx,
                "num_predict": generation.num_predict
            }
        });
        if json {
            body["format"] = serde_json::Value::String("json".to_string());
        }
        let response = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .map_err(|err| {
                BenchError::InvalidInput(format!(
                    "Ollama generation failed for {model} after {}s timeout: {err}",
                    self.timeout_secs
                ))
            })?
            .error_for_status()
            .map_err(|err| {
                BenchError::InvalidInput(format!("Ollama generation failed for {model}: {err}"))
            })?
            .json::<OllamaChatResponse>()
            .map_err(|err| BenchError::Parse(format!("Ollama chat response for {model}: {err}")))?;
        Ok(GeneratedText {
            content: response.message.content.trim().to_string(),
            thinking_chars: response.message.thinking.chars().count(),
            done_reason: response.done_reason,
            prompt_eval_tokens: response.prompt_eval_count,
            output_eval_tokens: response.eval_count,
        })
    }
}

fn normalize_model_name(value: &str) -> String {
    if value.contains(':') {
        value.to_string()
    } else {
        format!("{value}:latest")
    }
}

fn build_summary(questions: &[QuestionRecord]) -> RunSummary {
    let routes = [
        ROUTE_FULL_CONTEXT,
        ROUTE_ORACLE_BASELINE,
        ROUTE_RETRIEVAL_BASELINE,
        ROUTE_RETRIEVAL_STRONG,
    ]
    .into_iter()
    .filter(|route| {
        questions
            .iter()
            .any(|question| question.routes.contains_key(*route))
    })
    .map(|route| (route.to_string(), summarize_route(questions, route)))
    .collect();
    let retrieval_bottleneck_cases = questions
        .iter()
        .filter(|question| {
            route_correct(question, ROUTE_ORACLE_BASELINE) == Some(true)
                && route_correct(question, ROUTE_RETRIEVAL_BASELINE) == Some(false)
        })
        .count();
    let strong_reader_recoveries = questions
        .iter()
        .filter(|question| {
            route_correct(question, ROUTE_RETRIEVAL_BASELINE) == Some(false)
                && route_correct(question, ROUTE_RETRIEVAL_STRONG) == Some(true)
        })
        .count();
    RunSummary {
        total_questions: questions.len(),
        routes,
        retrieval: summarize_retrieval(questions),
        selection_variants: summarize_selection_variants(questions),
        retrieval_bottleneck_cases,
        strong_reader_recoveries,
    }
}

fn summarize_retrieval(questions: &[QuestionRecord]) -> RetrievalSummary {
    let evaluated: Vec<_> = questions
        .iter()
        .filter_map(|question| {
            Some((
                question.retrieval_evaluation.as_ref()?,
                question.retrieval_context.as_ref()?,
            ))
        })
        .collect();
    if evaluated.is_empty() {
        return RetrievalSummary::default();
    }
    let count = evaluated.len();
    let candidate_recall = evaluated
        .iter()
        .map(|(evaluation, _)| evaluation.candidate_metrics.recall_at_k)
        .sum::<f64>();
    let reranker_recall = evaluated
        .iter()
        .map(|(evaluation, _)| evaluation.reranker_metrics.recall_at_k)
        .sum::<f64>();
    let delivered_recall = evaluated
        .iter()
        .map(|(evaluation, _)| evaluation.delivered_metrics.recall_at_k)
        .sum::<f64>();
    let rendered_recall = evaluated
        .iter()
        .map(|(evaluation, _)| evaluation.rendered_recall)
        .sum::<f64>();
    let candidate_hits = evaluated
        .iter()
        .filter(|(evaluation, _)| evaluation.candidate_first_hit_rank.is_some())
        .count();
    let reranker_hits = evaluated
        .iter()
        .filter(|(evaluation, _)| evaluation.reranker_first_hit_rank.is_some())
        .count();
    let delivered_hits = evaluated
        .iter()
        .filter(|(_, context)| context.evidence.iter().any(|item| item.relevant))
        .count();
    let rendered_hits = evaluated
        .iter()
        .filter(|(evaluation, _)| evaluation.rendered_hit)
        .count();
    let first = evaluated[0].0;
    RetrievalSummary {
        evaluated: count,
        candidate_k: first.candidate_k,
        reranker_k: first.reranker_k,
        delivered_k: first.delivered_k,
        mean_candidate_recall_at_k: candidate_recall / count as f64,
        mean_reranker_recall_at_k: reranker_recall / count as f64,
        mean_delivered_recall_at_k: delivered_recall / count as f64,
        mean_rendered_recall: rendered_recall / count as f64,
        candidate_hit_at_k: ratio(candidate_hits, count),
        reranker_hit_at_k: ratio(reranker_hits, count),
        delivered_hit_at_k: ratio(delivered_hits, count),
        rendered_hit: ratio(rendered_hits, count),
    }
}

fn summarize_selection_variants(
    questions: &[QuestionRecord],
) -> BTreeMap<String, SelectionVariantSummary> {
    #[derive(Default)]
    struct Accumulator {
        count: usize,
        selection_k: usize,
        selected_recall: f64,
        delivered_recall: f64,
        rendered_recall: f64,
        selected_hits: usize,
        rendered_hits: usize,
        delivered_fragments: usize,
        context_tokens: usize,
    }

    let mut values: BTreeMap<String, Accumulator> = BTreeMap::new();
    for evaluation in questions
        .iter()
        .filter_map(|question| question.retrieval_evaluation.as_ref())
    {
        for (name, variant) in &evaluation.selection_variants {
            let accumulator = values.entry(name.clone()).or_default();
            accumulator.count += 1;
            accumulator.selection_k = variant.selection_k;
            accumulator.selected_recall += variant.selected_metrics.recall_at_k;
            accumulator.delivered_recall += variant.delivered_metrics.recall_at_k;
            accumulator.rendered_recall += variant.rendered_recall;
            accumulator.selected_hits += usize::from(variant.selected_metrics.recall_at_k > 0.0);
            accumulator.rendered_hits += usize::from(variant.rendered_hit);
            accumulator.delivered_fragments += variant.delivered_fragments;
            accumulator.context_tokens += variant.context_tokens;
        }
    }

    values
        .into_iter()
        .filter_map(|(name, value)| {
            (value.count > 0).then(|| {
                let count = value.count as f64;
                (
                    name,
                    SelectionVariantSummary {
                        evaluated: value.count,
                        selection_k: value.selection_k,
                        mean_selected_recall: value.selected_recall / count,
                        mean_delivered_recall: value.delivered_recall / count,
                        mean_rendered_recall: value.rendered_recall / count,
                        selected_hit: ratio(value.selected_hits, value.count),
                        rendered_hit: ratio(value.rendered_hits, value.count),
                        mean_delivered_fragments: value.delivered_fragments as f64 / count,
                        mean_context_tokens: value.context_tokens as f64 / count,
                    },
                )
            })
        })
        .collect()
}

fn summarize_route(questions: &[QuestionRecord], route: &str) -> RouteSummary {
    let mut judged = 0usize;
    let mut correct = 0usize;
    let mut unparsed = 0usize;
    let mut answer_latency = 0.0;
    let mut judge_latency = 0.0;
    let mut type_counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut official_total = 0.0;
    let mut official_scored = 0usize;
    let mut official_scores = Vec::new();
    let mut official_type_scores: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    let mut reader_surface_total = 0.0;
    let mut reader_surface_scored = 0usize;
    let mut reader_surface_type_scores: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for question in questions {
        let Some(result) = question.routes.get(route) else {
            continue;
        };
        answer_latency += result.answer_latency_ms;
        if let Some(score) = result.locomo_official_f1 {
            official_total += score;
            official_scored += 1;
            official_scores.push(score);
            let values = official_type_scores
                .entry(question.question_type.clone())
                .or_insert((0, 0.0));
            values.0 += 1;
            values.1 += score;
        }
        if let Some(score) = result.locomo_reader_surface_f1 {
            reader_surface_total += score;
            reader_surface_scored += 1;
            let values = reader_surface_type_scores
                .entry(question.question_type.clone())
                .or_insert((0, 0.0));
            values.0 += 1;
            values.1 += score;
        }
        if let Some(judge) = &result.judge {
            judge_latency += judge.latency_ms;
            match judge.correct {
                Some(is_correct) => {
                    judged += 1;
                    correct += usize::from(is_correct);
                    let counts = type_counts
                        .entry(question.question_type.clone())
                        .or_insert((0, 0));
                    counts.0 += 1;
                    counts.1 += usize::from(is_correct);
                }
                None => unparsed += 1,
            }
        }
    }
    let completed = questions
        .iter()
        .filter(|question| question.routes.contains_key(route))
        .count();
    let accuracy_by_type: BTreeMap<_, _> = type_counts
        .into_iter()
        .map(|(kind, (total, correct))| (kind, ratio(correct, total)))
        .collect();
    let macro_accuracy = if accuracy_by_type.is_empty() {
        0.0
    } else {
        accuracy_by_type.values().sum::<f64>() / accuracy_by_type.len() as f64
    };
    let locomo_official_f1_by_type = official_type_scores
        .into_iter()
        .map(|(kind, (count, total))| (kind, total / count as f64))
        .collect();
    let locomo_reader_surface_f1_by_type = reader_surface_type_scores
        .into_iter()
        .map(|(kind, (count, total))| (kind, total / count as f64))
        .collect();
    let (accuracy_ci95_low, accuracy_ci95_high) = wilson_interval(correct, judged);
    let official_ci95 = bootstrap_mean_ci(&official_scores);
    RouteSummary {
        judged,
        correct,
        unparsed,
        accuracy: ratio(correct, judged),
        accuracy_ci95_low,
        accuracy_ci95_high,
        macro_accuracy,
        accuracy_by_type,
        locomo_official_scored: official_scored,
        locomo_official_f1: (official_scored > 0)
            .then_some(official_total / official_scored as f64),
        locomo_official_f1_ci95_low: official_ci95.map(|interval| interval.0),
        locomo_official_f1_ci95_high: official_ci95.map(|interval| interval.1),
        locomo_official_f1_by_type,
        locomo_reader_surface_f1: (reader_surface_scored > 0)
            .then_some(reader_surface_total / reader_surface_scored as f64),
        locomo_reader_surface_f1_by_type,
        mean_answer_latency_ms: if completed == 0 {
            0.0
        } else {
            answer_latency / completed as f64
        },
        mean_judge_latency_ms: if judged + unparsed == 0 {
            0.0
        } else {
            judge_latency / (judged + unparsed) as f64
        },
    }
}

fn bootstrap_mean_ci(values: &[f64]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    if values.len() == 1 {
        return Some((values[0], values[0]));
    }

    const RESAMPLES: usize = 10_000;
    let mut state = 0x6a09_e667_f3bc_c909_u64 ^ values.len() as u64;
    let mut means = Vec::with_capacity(RESAMPLES);
    for _ in 0..RESAMPLES {
        let mut total = 0.0;
        for _ in values {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            total += values[state as usize % values.len()];
        }
        means.push(total / values.len() as f64);
    }
    means.sort_by(f64::total_cmp);
    let low_index = (RESAMPLES - 1) * 25 / 1_000;
    let high_index = (RESAMPLES - 1) * 975 / 1_000;
    Some((means[low_index], means[high_index]))
}

fn wilson_interval(successes: usize, total: usize) -> (f64, f64) {
    if total == 0 {
        return (0.0, 0.0);
    }
    let z = 1.959_963_984_540_054_f64;
    let n = total as f64;
    let proportion = successes as f64 / n;
    let denominator = 1.0 + z * z / n;
    let center = (proportion + z * z / (2.0 * n)) / denominator;
    let margin =
        z * ((proportion * (1.0 - proportion) / n + z * z / (4.0 * n * n)).sqrt()) / denominator;
    ((center - margin).max(0.0), (center + margin).min(1.0))
}

fn route_correct(question: &QuestionRecord, route: &str) -> Option<bool> {
    question
        .routes
        .get(route)
        .and_then(|result| result.judge.as_ref())
        .and_then(|judge| judge.correct)
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn print_summary(summary: Option<&RunSummary>) {
    let Some(summary) = summary else {
        return;
    };
    eprintln!();
    eprintln!(
        "{:<28} {:>8} {:>10} {:>18} {:>10} {:>10} {:>12}",
        "Route", "Judged", "Judge", "95% CI", "Macro", "Raw F1", "Surface F1"
    );
    eprintln!(
        "{:-<28} {:-<8} {:-<10} {:-<18} {:-<10} {:-<10} {:-<12}",
        "", "", "", "", "", "", ""
    );
    for (route, values) in &summary.routes {
        let official = values.locomo_official_f1.map_or_else(
            || "n/a".to_string(),
            |score| format!("{:.1}%", score * 100.0),
        );
        let surface = values.locomo_reader_surface_f1.map_or_else(
            || "n/a".to_string(),
            |score| format!("{:.1}%", score * 100.0),
        );
        eprintln!(
            "{:<28} {:>8} {:>9.1}% {:>7.1}%..{:>6.1}% {:>9.1}% {:>10} {:>12}",
            route,
            values.judged,
            values.accuracy * 100.0,
            values.accuracy_ci95_low * 100.0,
            values.accuracy_ci95_high * 100.0,
            values.macro_accuracy * 100.0,
            official,
            surface
        );
    }
    if summary.retrieval.evaluated > 0 {
        eprintln!(
            "retrieval candidate@{} recall={:.3} hit={:.3}; reranker@{} recall={:.3} hit={:.3}; \
             delivered@{} recall={:.3} hit={:.3}; rendered recall={:.3} hit={:.3}",
            summary.retrieval.candidate_k,
            summary.retrieval.mean_candidate_recall_at_k,
            summary.retrieval.candidate_hit_at_k,
            summary.retrieval.reranker_k,
            summary.retrieval.mean_reranker_recall_at_k,
            summary.retrieval.reranker_hit_at_k,
            summary.retrieval.delivered_k,
            summary.retrieval.mean_delivered_recall_at_k,
            summary.retrieval.delivered_hit_at_k,
            summary.retrieval.mean_rendered_recall,
            summary.retrieval.rendered_hit,
        );
    }
    eprintln!(
        "retrieval bottlenecks={} strong-reader recoveries={}",
        summary.retrieval_bottleneck_cases, summary.strong_reader_recoveries
    );
    for (name, variant) in &summary.selection_variants {
        eprintln!(
            "selection {name}: selected_recall={:.3} delivered_recall={:.3} \
             rendered_recall={:.3} hit={:.3} fragments={:.1} tokens={:.0}",
            variant.mean_selected_recall,
            variant.mean_delivered_recall,
            variant.mean_rendered_recall,
            variant.rendered_hit,
            variant.mean_delivered_fragments,
            variant.mean_context_tokens,
        );
    }
}

fn stratify_questions_seeded(questions: &mut Vec<BenchQuestion>, per_type: usize, seed: u64) {
    let mut candidates: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for question in questions.iter() {
        let score = stable_sample_score(seed, &question.question_id);
        candidates
            .entry(question.question_type.clone())
            .or_default()
            .push((score, question.question_id.clone()));
    }

    let mut selected = BTreeSet::new();
    for values in candidates.values_mut() {
        values.sort_unstable();
        selected.extend(
            values
                .iter()
                .take(per_type)
                .map(|(_, question_id)| question_id.clone()),
        );
    }
    questions.retain(|question| selected.contains(&question.question_id));
}

fn stable_sample_score(seed: u64, question_id: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in seed.to_le_bytes().iter().chain(question_id.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58476d1ce4e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d049bb133111eb);
    hash ^ (hash >> 31)
}

fn write_report(path: &Path, report: &RunReport) -> BenchResult<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|err| BenchError::InvalidInput(err.to_string()))?;
    }
    let bytes =
        serde_json::to_vec_pretty(report).map_err(|err| BenchError::Parse(err.to_string()))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes).map_err(|err| BenchError::InvalidInput(err.to_string()))?;
    std::fs::rename(&temp, path).map_err(|err| {
        BenchError::InvalidInput(format!(
            "failed to commit report {} -> {}: {err}",
            temp.display(),
            path.display()
        ))
    })
}

fn fingerprint(path: &Path) -> BenchResult<(u64, String)> {
    let bytes = std::fs::read(path).map_err(|err| {
        BenchError::InvalidInput(format!("failed to read {}: {err}", path.display()))
    })?;
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in &bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok((bytes.len() as u64, format!("{hash:016x}")))
}

fn load_derived_memory_artifact(
    path: &Path,
    dataset_fnv1a64: &str,
) -> BenchResult<(DerivedMemoryArtifact, String)> {
    let (_, artifact_fnv1a64) = fingerprint(path)?;
    let bytes = std::fs::read(path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to read derived-memory artifact {}: {error}",
            path.display()
        ))
    })?;
    let artifact: DerivedMemoryArtifact = serde_json::from_slice(&bytes).map_err(|error| {
        BenchError::Parse(format!(
            "failed to parse derived-memory artifact {}: {error}",
            path.display()
        ))
    })?;
    if artifact.schema_version != 1 {
        return Err(BenchError::InvalidInput(format!(
            "unsupported derived-memory artifact schema {}",
            artifact.schema_version
        )));
    }
    if artifact.dataset_fnv1a64 != dataset_fnv1a64 {
        return Err(BenchError::InvalidInput(
            "derived-memory artifact dataset fingerprint differs".to_owned(),
        ));
    }
    if !artifact
        .extractor_model
        .trim()
        .to_ascii_lowercase()
        .starts_with("qwen3.6")
    {
        return Err(BenchError::InvalidInput(
            "derived-memory artifact must use the frozen qwen3.6 extractor lane".to_owned(),
        ));
    }
    if artifact.extractor_digest.trim().is_empty() || artifact.prompt_version.trim().is_empty() {
        return Err(BenchError::InvalidInput(
            "derived-memory artifact requires extractor digest and prompt version".to_owned(),
        ));
    }
    let mut ids = BTreeSet::new();
    for record in &artifact.records {
        if !ids.insert(record.id.as_str()) {
            return Err(BenchError::InvalidInput(format!(
                "duplicate derived-memory artifact id {:?}",
                record.id
            )));
        }
    }
    let mut relation_keys = BTreeSet::new();
    for relation in &artifact.relations {
        if relation.from == relation.to
            || !ids.contains(relation.from.as_str())
            || !ids.contains(relation.to.as_str())
        {
            return Err(BenchError::InvalidInput(format!(
                "derived-memory relation {:?}->{:?} has invalid endpoints",
                relation.from, relation.to
            )));
        }
        let key = format!("{}→{}→{:?}", relation.from, relation.to, relation.kind);
        if !relation_keys.insert(key) {
            return Err(BenchError::InvalidInput(
                "derived-memory artifact contains a duplicate relation".to_owned(),
            ));
        }
    }
    Ok((artifact, artifact_fnv1a64))
}

fn dataset_path(dataset: BenchDatasetName, data_dir: &Path) -> PathBuf {
    match dataset {
        BenchDatasetName::Locomo => data_dir.join("locomo").join("locomo10.json"),
        BenchDatasetName::LongMemEval => data_dir.join("longmemeval").join("longmemeval_s.json"),
    }
}

fn validate_local_url(value: &str) -> BenchResult<()> {
    let normalized = value.trim_end_matches('/');
    let local = normalized.starts_with("http://localhost:")
        || normalized == "http://localhost"
        || normalized.starts_with("http://127.0.0.1:")
        || normalized == "http://127.0.0.1"
        || normalized.starts_with("http://[::1]:")
        || normalized == "http://[::1]";
    if local {
        Ok(())
    } else {
        Err(BenchError::InvalidInput(
            "--ollama-base-url must be a loopback HTTP address; this benchmark is local-only"
                .to_string(),
        ))
    }
}

fn format_epoch_date(timestamp: u64) -> String {
    let days = (timestamp / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn parse_args<I>(args: I) -> BenchResult<Option<Args>>
where
    I: IntoIterator<Item = String>,
{
    let mut dataset = None;
    let mut data_dir = PathBuf::from("benches/eval/data");
    let mut output = None;
    let mut samples = None;
    let mut stratify = None;
    let mut question_type = None;
    let mut sample_seed = 42u64;
    let mut skip_adversarial = false;
    let mut run_strong_reader = false;
    let mut strong_reader_reflect = false;
    let mut strong_reader_reflect_complex_only = false;
    let mut strong_reader_remote = false;
    let mut frontier_judge = false;
    let mut frontier_base_url = std::env::var("LLM_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut frontier_max_cost_usd = None;
    let mut run_full_context = false;
    let mut run_local_judge = true;
    let mut run_oracle_baseline = true;
    let mut run_retrieval_baseline = true;
    let mut predict_only = false;
    let mut context_surface = ContextSurface::ProductWire;
    let mut evidence_context = false;
    let mut derived_memory_artifact = None;
    let mut external_memory_artifact = None;
    let mut answer_report = None;
    let mut paired_answer_report = None;
    let mut judge_report = None;
    let mut compact_retrieval_context = false;
    let mut hydrate_episodic_context = false;
    let mut shadow_rank_fusion = false;
    let mut consumer_cross_encoder =
        Some(anamnesis::embedding::fastembed::DEFAULT_RERANKER_MODEL.to_owned());
    let mut consumer_ranking_report = None;
    let mut consumer_prefilter_cross_encoder = None;
    let mut consumer_prefilter_k = None;
    let mut consumer_prefilter_query_fusion = false;
    let mut consumer_evidence_documents = true;
    let mut consumer_candidate_k = anamnesis::memory::DEFAULT_RERANK_CANDIDATE_LIMIT;
    let mut first_stage_seed_limit = None;
    let mut dump_candidate_pool = false;
    let mut screen_top_k = Vec::new();
    let mut screen_source_dedup = false;
    let mut diagnostic_readout_limit = None;
    let mut consumer_selection_policy = ConsumerSelectionPolicy::MemoryDeep;
    let mut top_k = anamnesis::memory::DEFAULT_RERANK_FINAL_LIMIT;
    let mut baseline_reader_model = "qwen3.6:35b-a3b".to_string();
    let mut strong_reader_model = "qwen3.6:35b-a3b".to_string();
    let mut judge_model = "qwen3.6:35b-a3b".to_string();
    let mut embedding_model = "bge-base-en-v1.5".to_string();
    let mut ollama_base_url = "http://127.0.0.1:11434".to_string();
    let mut timeout_secs = 600u64;
    let mut embed_cache = None;
    let mut allow_download = false;
    let mut resume = false;
    let mut force = false;
    let mut reader_generation = GenerationOptions {
        think: true,
        temperature: 1.0,
        top_p: 0.95,
        top_k: 20,
        presence_penalty: 1.5,
        seed: 42,
        num_ctx: 32_768,
        num_predict: 8_192,
    };
    let judge_generation = GenerationOptions {
        think: false,
        temperature: 0.0,
        top_p: 1.0,
        top_k: 40,
        presence_penalty: 0.0,
        seed: 42,
        num_ctx: 32_768,
        num_predict: 256,
    };
    let mut saw_arg = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--dataset" => {
                saw_arg = true;
                dataset = Some(
                    BenchDatasetName::parse(&next_value(&mut iter, "--dataset")?)
                        .map_err(BenchError::InvalidInput)?,
                );
            }
            "--data-dir" => data_dir = PathBuf::from(next_value(&mut iter, "--data-dir")?),
            "--output" => output = Some(PathBuf::from(next_value(&mut iter, "--output")?)),
            "--samples" => {
                samples = Some(parse_usize(
                    &next_value(&mut iter, "--samples")?,
                    "--samples",
                )?)
            }
            "--stratify" => {
                stratify = Some(parse_usize(
                    &next_value(&mut iter, "--stratify")?,
                    "--stratify",
                )?)
            }
            "--question-type" => question_type = Some(next_value(&mut iter, "--question-type")?),
            "--sample-seed" => {
                sample_seed = parse_u64(&next_value(&mut iter, "--sample-seed")?, "--sample-seed")?
            }
            "--skip-adversarial" => skip_adversarial = true,
            "--baseline-only" => {
                run_strong_reader = false;
                strong_reader_reflect = false;
                strong_reader_reflect_complex_only = false;
                strong_reader_remote = false;
            }
            "--run-strong-reader" => run_strong_reader = true,
            "--run-reflect-reader" => {
                run_strong_reader = true;
                strong_reader_reflect = true;
            }
            "--reflect-complex-only" => {
                run_strong_reader = true;
                strong_reader_reflect = true;
                strong_reader_reflect_complex_only = true;
            }
            "--frontier-reader" => {
                run_strong_reader = true;
                strong_reader_remote = true;
            }
            "--frontier-judge" => frontier_judge = true,
            "--frontier-base-url" => {
                frontier_base_url = Some(next_value(&mut iter, "--frontier-base-url")?)
            }
            "--frontier-max-cost-usd" => {
                frontier_max_cost_usd = Some(parse_f64(
                    &next_value(&mut iter, "--frontier-max-cost-usd")?,
                    "--frontier-max-cost-usd",
                )?)
            }
            "--full-context" => run_full_context = true,
            "--skip-local-judge" => run_local_judge = false,
            "--predict-only" => predict_only = true,
            "--retrieval-only" => run_oracle_baseline = false,
            "--oracle-only" => run_retrieval_baseline = false,
            "--diagnostic-fragment-context" => {
                context_surface = ContextSurface::DiagnosticFragments
            }
            "--evidence-context" => evidence_context = true,
            "--derived-memory-artifact" => {
                derived_memory_artifact = Some(PathBuf::from(next_value(
                    &mut iter,
                    "--derived-memory-artifact",
                )?))
            }
            "--external-memory-artifact" => {
                external_memory_artifact = Some(PathBuf::from(next_value(
                    &mut iter,
                    "--external-memory-artifact",
                )?))
            }
            "--answer-report" => {
                answer_report = Some(PathBuf::from(next_value(&mut iter, "--answer-report")?))
            }
            "--paired-answer-report" => {
                paired_answer_report = Some(PathBuf::from(next_value(
                    &mut iter,
                    "--paired-answer-report",
                )?))
            }
            "--judge-report" => {
                judge_report = Some(PathBuf::from(next_value(&mut iter, "--judge-report")?))
            }
            "--compact-retrieval-context" => compact_retrieval_context = true,
            "--hydrate-episodic-context" => hydrate_episodic_context = true,
            "--shadow-rank-fusion" => shadow_rank_fusion = true,
            "--consumer-cross-encoder" | "--shadow-cross-encoder" => {
                consumer_cross_encoder = Some(next_value(&mut iter, &arg)?)
            }
            "--no-product-reranker" => {
                consumer_cross_encoder = None;
                consumer_evidence_documents = false;
                consumer_selection_policy = ConsumerSelectionPolicy::Relevance;
            }
            "--consumer-ranking-report" => {
                consumer_ranking_report = Some(PathBuf::from(next_value(&mut iter, &arg)?))
            }
            "--consumer-prefilter-cross-encoder" => {
                consumer_prefilter_cross_encoder = Some(next_value(&mut iter, &arg)?)
            }
            "--consumer-prefilter-k" => {
                consumer_prefilter_k = Some(parse_usize(
                    &next_value(&mut iter, &arg)?,
                    "--consumer-prefilter-k",
                )?)
            }
            "--consumer-prefilter-query-fusion" => consumer_prefilter_query_fusion = true,
            "--consumer-evidence-documents" => consumer_evidence_documents = true,
            "--consumer-candidate-k" | "--shadow-cross-encoder-candidates" => {
                consumer_candidate_k =
                    parse_usize(&next_value(&mut iter, &arg)?, "--consumer-candidate-k")?
            }
            "--first-stage-seed-limit" => {
                first_stage_seed_limit = Some(parse_usize(
                    &next_value(&mut iter, "--first-stage-seed-limit")?,
                    "--first-stage-seed-limit",
                )?)
            }
            "--dump-candidate-pool" => dump_candidate_pool = true,
            "--screen-top-k" => {
                screen_top_k =
                    parse_usize_list(&next_value(&mut iter, "--screen-top-k")?, "--screen-top-k")?
            }
            "--top-k" => top_k = parse_usize(&next_value(&mut iter, "--top-k")?, "--top-k")?,
            "--baseline-reader-model" => {
                baseline_reader_model = next_value(&mut iter, "--baseline-reader-model")?
            }
            "--strong-reader-model" => {
                strong_reader_model = next_value(&mut iter, "--strong-reader-model")?
            }
            "--judge-model" => judge_model = next_value(&mut iter, "--judge-model")?,
            "--embedding-model" => embedding_model = next_value(&mut iter, "--embedding-model")?,
            "--ollama-base-url" => ollama_base_url = next_value(&mut iter, "--ollama-base-url")?,
            "--timeout-secs" => {
                timeout_secs =
                    parse_u64(&next_value(&mut iter, "--timeout-secs")?, "--timeout-secs")?
            }
            "--reader-think" => reader_generation.think = true,
            "--reader-no-think" => reader_generation.think = false,
            "--reader-temperature" => {
                reader_generation.temperature = parse_f64(
                    &next_value(&mut iter, "--reader-temperature")?,
                    "--reader-temperature",
                )?
            }
            "--screen-source-dedup" => screen_source_dedup = true,
            "--diagnostic-readout-limit" => {
                diagnostic_readout_limit = Some(parse_usize(
                    &next_value(&mut iter, "--diagnostic-readout-limit")?,
                    "--diagnostic-readout-limit",
                )?)
            }
            "--consumer-selection" => {
                consumer_selection_policy =
                    parse_consumer_selection(&next_value(&mut iter, "--consumer-selection")?)?
            }
            "--reader-top-p" => {
                reader_generation.top_p =
                    parse_f64(&next_value(&mut iter, "--reader-top-p")?, "--reader-top-p")?
            }
            "--reader-top-k" => {
                reader_generation.top_k =
                    parse_u64(&next_value(&mut iter, "--reader-top-k")?, "--reader-top-k")?
            }
            "--reader-presence-penalty" => {
                reader_generation.presence_penalty = parse_f64(
                    &next_value(&mut iter, "--reader-presence-penalty")?,
                    "--reader-presence-penalty",
                )?
            }
            "--generation-seed" => {
                reader_generation.seed = parse_u64(
                    &next_value(&mut iter, "--generation-seed")?,
                    "--generation-seed",
                )?
            }
            "--reader-num-ctx" => {
                reader_generation.num_ctx = parse_u64(
                    &next_value(&mut iter, "--reader-num-ctx")?,
                    "--reader-num-ctx",
                )?
            }
            "--reader-num-predict" => {
                reader_generation.num_predict = parse_i64(
                    &next_value(&mut iter, "--reader-num-predict")?,
                    "--reader-num-predict",
                )?
            }
            "--embed-cache" => {
                embed_cache = Some(PathBuf::from(next_value(&mut iter, "--embed-cache")?))
            }
            "--allow-download" => allow_download = true,
            "--resume" => resume = true,
            "--force" => force = true,
            "--bench" => {}
            other => {
                return Err(BenchError::InvalidInput(format!(
                    "unknown argument: {other}"
                )));
            }
        }
    }
    if !saw_arg {
        return Ok(None);
    }
    let dataset =
        dataset.ok_or_else(|| BenchError::InvalidInput("missing --dataset".to_string()))?;
    if top_k == 0 || top_k > 100 {
        return Err(BenchError::InvalidInput(
            "--top-k must be in 1..=100".to_string(),
        ));
    }
    if stratify == Some(0) || samples == Some(0) {
        return Err(BenchError::InvalidInput(
            "--samples and --stratify must be at least 1".to_string(),
        ));
    }
    if timeout_secs == 0 {
        return Err(BenchError::InvalidInput(
            "--timeout-secs must be at least 1".to_string(),
        ));
    }
    if frontier_max_cost_usd.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err(BenchError::InvalidInput(
            "--frontier-max-cost-usd must be a positive finite number".to_owned(),
        ));
    }
    if frontier_judge && answer_report.is_none() {
        return Err(BenchError::InvalidInput(
            "--frontier-judge currently requires --answer-report".to_owned(),
        ));
    }
    if frontier_judge && !run_local_judge {
        return Err(BenchError::InvalidInput(
            "--frontier-judge cannot be combined with --skip-local-judge".to_owned(),
        ));
    }
    if !(0.0..=2.0).contains(&reader_generation.temperature)
        || !(0.0..=1.0).contains(&reader_generation.top_p)
        || reader_generation.top_k == 0
        || reader_generation.num_ctx < 4_096
        || reader_generation.num_predict == 0
    {
        return Err(BenchError::InvalidInput(
            "reader generation values are outside supported ranges".to_string(),
        ));
    }
    if dataset == BenchDatasetName::LongMemEval
        && run_full_context
        && reader_generation.num_ctx < 131_072
    {
        return Err(BenchError::InvalidInput(
            "LongMemEval full context requires --reader-num-ctx at least 131072".to_string(),
        ));
    }
    if resume && force {
        return Err(BenchError::InvalidInput(
            "--resume and --force are mutually exclusive".to_string(),
        ));
    }
    if usize::from(answer_report.is_some())
        + usize::from(judge_report.is_some())
        + usize::from(external_memory_artifact.is_some())
        > 1
    {
        return Err(BenchError::InvalidInput(
            "--answer-report, --judge-report, and --external-memory-artifact are mutually \
             exclusive"
                .to_owned(),
        ));
    }
    if paired_answer_report.is_some() && answer_report.is_none() {
        return Err(BenchError::InvalidInput(
            "--paired-answer-report requires --answer-report".to_owned(),
        ));
    }
    if derived_memory_artifact.is_some() && external_memory_artifact.is_some() {
        return Err(BenchError::InvalidInput(
            "--derived-memory-artifact and --external-memory-artifact are mutually exclusive"
                .to_owned(),
        ));
    }
    if shadow_rank_fusion && consumer_cross_encoder.is_some() {
        return Err(BenchError::InvalidInput(
            "--shadow-rank-fusion and --consumer-cross-encoder are mutually exclusive".to_string(),
        ));
    }
    if consumer_evidence_documents && consumer_cross_encoder.is_none() {
        return Err(BenchError::InvalidInput(
            "--consumer-evidence-documents requires --consumer-cross-encoder".to_owned(),
        ));
    }
    if consumer_ranking_report.is_some()
        && (consumer_cross_encoder.is_none()
            || shadow_rank_fusion
            || consumer_prefilter_cross_encoder.is_some()
            || consumer_prefilter_k.is_some()
            || consumer_prefilter_query_fusion
            || answer_report.is_some()
            || judge_report.is_some()
            || external_memory_artifact.is_some())
    {
        return Err(BenchError::InvalidInput(
            "--consumer-ranking-report requires the source --consumer-cross-encoder identity and \
             cannot be combined with rank fusion, a cascade, stored-answer/judge mode, or an \
             external artifact"
                .to_owned(),
        ));
    }
    if context_surface == ContextSurface::ProductWire
        && (compact_retrieval_context || hydrate_episodic_context || shadow_rank_fusion)
    {
        return Err(BenchError::InvalidInput(
            "benchmark-only fragment compaction, hydration, and rank fusion require \
             --diagnostic-fragment-context"
                .to_string(),
        ));
    }
    if !(1..=512).contains(&consumer_candidate_k) {
        return Err(BenchError::InvalidInput(
            "--consumer-candidate-k must be in 1..=512".to_string(),
        ));
    }
    if consumer_prefilter_cross_encoder.is_some() != consumer_prefilter_k.is_some()
        || consumer_prefilter_cross_encoder.is_some() && consumer_cross_encoder.is_none()
        || consumer_prefilter_query_fusion && consumer_prefilter_cross_encoder.is_none()
        || consumer_prefilter_k.is_some_and(|value| value == 0 || value > consumer_candidate_k)
    {
        return Err(BenchError::InvalidInput(
            "prefilter cascade requires both --consumer-prefilter-cross-encoder and \
             --consumer-prefilter-k in 1..=consumer-candidate-k plus a final \
             --consumer-cross-encoder; query fusion additionally requires that cascade"
                .to_owned(),
        ));
    }
    if first_stage_seed_limit == Some(0) || first_stage_seed_limit.is_some_and(|value| value > 200)
    {
        return Err(BenchError::InvalidInput(
            "--first-stage-seed-limit must be in 1..=200".to_string(),
        ));
    }
    if screen_top_k.iter().any(|value| !(1..=100).contains(value)) {
        return Err(BenchError::InvalidInput(
            "--screen-top-k values must be in 1..=100".to_string(),
        ));
    }
    if diagnostic_readout_limit == Some(0)
        || diagnostic_readout_limit.is_some_and(|value| value > 4_096)
    {
        return Err(BenchError::InvalidInput(
            "--diagnostic-readout-limit must be in 1..=4096".to_string(),
        ));
    }
    if diagnostic_readout_limit.is_some() {
        dump_candidate_pool = true;
    }
    for (flag, model) in [("--baseline-reader-model", baseline_reader_model.as_str())] {
        if !model.trim().to_ascii_lowercase().starts_with("qwen3.6") {
            return Err(BenchError::InvalidInput(format!(
                "{flag} is frozen to the qwen3.6 lane"
            )));
        }
    }
    if !frontier_judge
        && !judge_model
            .trim()
            .to_ascii_lowercase()
            .starts_with("qwen3.6")
    {
        return Err(BenchError::InvalidInput(
            "--judge-model is frozen to qwen3.6 for the local lane".to_owned(),
        ));
    }
    if frontier_max_cost_usd.is_some()
        && ((strong_reader_remote && strong_reader_model != "gpt-4o")
            || (frontier_judge && judge_model != "gpt-4o"))
    {
        return Err(BenchError::InvalidInput(
            "--frontier-max-cost-usd currently uses GPT-4o pricing and requires remote models \
             to be exactly gpt-4o"
                .to_owned(),
        ));
    }
    if !strong_reader_remote
        && !strong_reader_model
            .trim()
            .to_ascii_lowercase()
            .starts_with("qwen3.6")
    {
        return Err(BenchError::InvalidInput(
            "--strong-reader-model is frozen to qwen3.6 for the local lane".to_owned(),
        ));
    }
    if predict_only {
        run_strong_reader = false;
        strong_reader_reflect = false;
        strong_reader_reflect_complex_only = false;
        strong_reader_remote = false;
        run_full_context = false;
        run_local_judge = false;
        run_oracle_baseline = false;
        run_retrieval_baseline = false;
    }
    if !run_oracle_baseline
        && !run_retrieval_baseline
        && !run_full_context
        && !run_strong_reader
        && !predict_only
    {
        return Err(BenchError::InvalidInput(
            "at least one answer route must be enabled".to_string(),
        ));
    }
    if dataset == BenchDatasetName::LongMemEval && samples.is_none() && stratify.is_none() {
        return Err(BenchError::InvalidInput(
            "LongMemEval-S is large; pass --samples or --stratify".to_string(),
        ));
    }
    let output = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "benches/eval/results/local-answer-{}-{}.json",
            dataset.as_str(),
            timestamp_secs()
        ))
    });
    Ok(Some(Args {
        dataset,
        data_dir,
        output,
        samples,
        stratify,
        question_type,
        sample_seed,
        skip_adversarial,
        run_strong_reader,
        strong_reader_reflect,
        strong_reader_reflect_complex_only,
        strong_reader_remote,
        frontier_judge,
        frontier_base_url,
        frontier_max_cost_usd,
        run_full_context,
        run_local_judge,
        run_oracle_baseline,
        run_retrieval_baseline,
        predict_only,
        context_surface,
        evidence_context,
        derived_memory_artifact,
        external_memory_artifact,
        answer_report,
        paired_answer_report,
        judge_report,
        compact_retrieval_context,
        hydrate_episodic_context,
        shadow_rank_fusion,
        consumer_cross_encoder,
        consumer_ranking_report,
        consumer_prefilter_cross_encoder,
        consumer_prefilter_k,
        consumer_prefilter_query_fusion,
        consumer_evidence_documents,
        consumer_candidate_k,
        first_stage_seed_limit,
        dump_candidate_pool,
        screen_top_k,
        screen_source_dedup,
        diagnostic_readout_limit,
        consumer_selection_policy,
        top_k,
        baseline_reader_model,
        strong_reader_model,
        judge_model,
        embedding_model,
        ollama_base_url,
        timeout_secs,
        embed_cache,
        allow_download,
        resume,
        force,
        reader_generation,
        judge_generation,
    }))
}

fn next_value<I>(iter: &mut I, flag: &str) -> BenchResult<String>
where
    I: Iterator<Item = String>,
{
    iter.next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| BenchError::InvalidInput(format!("missing value for {flag}")))
}

fn parse_usize(value: &str, flag: &str) -> BenchResult<usize> {
    value
        .parse()
        .map_err(|err| BenchError::InvalidInput(format!("invalid {flag} value {value:?}: {err}")))
}

fn parse_usize_list(value: &str, flag: &str) -> BenchResult<Vec<usize>> {
    let values: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| parse_usize(item, flag))
        .collect::<BenchResult<_>>()?;
    if values.is_empty() {
        return Err(BenchError::InvalidInput(format!(
            "{flag} requires at least one comma-separated value"
        )));
    }
    Ok(values)
}

fn parse_consumer_selection(value: &str) -> BenchResult<ConsumerSelectionPolicy> {
    match value {
        "relevance" => Ok(ConsumerSelectionPolicy::Relevance),
        "memory-deep" => Ok(ConsumerSelectionPolicy::MemoryDeep),
        "memory-distinct-sources" => Ok(ConsumerSelectionPolicy::MemoryDistinctSources),
        "memory-source-coverage" => Ok(ConsumerSelectionPolicy::MemorySourceCoverage),
        "source-dedup" => Ok(ConsumerSelectionPolicy::SourceDedup),
        "source-coverage" => Ok(ConsumerSelectionPolicy::SourceCoverage),
        "provenance-guardrail" => Ok(ConsumerSelectionPolicy::ProvenanceGuardrail),
        _ => Err(BenchError::InvalidInput(
            "--consumer-selection must be relevance, memory-deep, \
             memory-distinct-sources, memory-source-coverage, source-dedup, \
             source-coverage, or provenance-guardrail"
                .to_string(),
        )),
    }
}

fn parse_u64(value: &str, flag: &str) -> BenchResult<u64> {
    value
        .parse()
        .map_err(|err| BenchError::InvalidInput(format!("invalid {flag} value {value:?}: {err}")))
}

fn parse_i64(value: &str, flag: &str) -> BenchResult<i64> {
    value
        .parse()
        .map_err(|err| BenchError::InvalidInput(format!("invalid {flag} value {value:?}: {err}")))
}

fn parse_f64(value: &str, flag: &str) -> BenchResult<f64> {
    value
        .parse()
        .map_err(|err| BenchError::InvalidInput(format!("invalid {flag} value {value:?}: {err}")))
}

fn timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn print_usage() {
    eprintln!(
        "Usage: cargo bench --features embed --bench local_answer -- --dataset <locomo|longmemeval> [options]\n\n\
Options:\n\
  --dataset <name>                 Dataset to run (required)\n\
  --samples <N>                    Limit selected questions\n\
  --stratify <N>                   Keep seeded N questions per type\n\
  --question-type <TYPE>           Keep only one normalized benchmark question type\n\
  --sample-seed <N>                Stable stratified-sample seed (default: 42)\n\
  --skip-adversarial               Drop LoCoMo adversarial questions\n\
  --full-context                   Add the no-retrieval full-history upper bound\n\
  --skip-local-judge               Omit the secondary judge (LoCoMo official F1 remains)\n\
  --predict-only                   Run ingest/search/retrieval metrics without Ollama answers\n\
  --retrieval-only                 Skip dataset-annotated evidence answers\n\
  --oracle-only                    Skip retrieval answers; run only annotated-evidence answers\n\
  --diagnostic-fragment-context    Use adapter-enriched fragments instead of product renderer\n\
  --evidence-context               Use product session/time-grouped evidence rendering\n\
  --derived-memory-artifact <JSON> Add a frozen reference-blind qwen3.6 extraction artifact\n\
--external-memory-artifact <JSON> Evaluate one fingerprint-bound external system context lane\n\
--answer-report <JSON>           Answer stored product contexts without rerunning retrieval\n\
--paired-answer-report <JSON>    Reuse exact-input answers/judges; generate changed contexts only\n\
--judge-report <JSON>            Judge existing answers without rerunning retrieval or reader\n\
  --compact-retrieval-context      Keep the richest retrieved item per source turn\n\
  --hydrate-episodic-context       Replace packaged turn labels with source fragments\n\
  --shadow-rank-fusion             Benchmark-only top-200 cognitive/embed/text RRF candidate\n\
  --consumer-cross-encoder <model> Override the canonical product reranker\n\
  --no-product-reranker            Ablation: disable local reranking and deep selection\n\
  --consumer-ranking-report <path> Replay frozen scores from a compatible report\n\
  --consumer-prefilter-cross-encoder <model> Fast first-stage model for a reranker cascade\n\
  --consumer-prefilter-k <N>       Documents passed from the prefilter to the quality reranker\n\
  --consumer-prefilter-query-fusion Fuse core query variants at the fast prefilter\n\
  --consumer-evidence-documents    Compatibility flag; canonical evidence documents are the default\n\
  --consumer-candidate-k <N>       Cognitive candidate/metric cutoff (default: production profile)\n\
  --first-stage-seed-limit <N>     RWR seed cutoff, independent of final top-k\n\
  --dump-candidate-pool            Persist top-200 readout feature diagnostics\n\
  --screen-top-k <A,B,...>         Repackage one fixed ranking at extra final cutoffs\n\
  --screen-source-dedup            Also screen source-turn deduplication with backfill\n\
  --diagnostic-readout-limit <N>   Retain up to N trace rows without changing retrieval\n\
  --consumer-selection <POLICY>    memory-deep (default), relevance, memory-distinct-sources,\n\
                                   memory-source-coverage, source-dedup, source-coverage, or provenance-guardrail\n\
--run-strong-reader              Add route 3 with --strong-reader-model\n\
--run-reflect-reader             Add route 3 with two-pass evidence reflection\n\
--reflect-complex-only           Reflect Count, Collection, Frequency, Inference, and Relationship plans\n\
--frontier-reader                Run route 3 through an OpenAI-compatible API\n\
  --frontier-base-url <url>        API base URL (or set LLM_BASE_URL)\n\
  --baseline-only                  Compatibility alias: omit route 3\n\
  --top-k <N>                      Product retrieval cutoff (default: production profile)\n\
  --baseline-reader-model <name>   Reader for routes 0, 1, and 2 (default: qwen3.6:35b-a3b)\n\
  --strong-reader-model <name>     Reader for route 3 (default: qwen3.6:35b-a3b)\n\
  --judge-model <name>             Separate local judge (default: qwen3.6:35b-a3b)\n\
  --embedding-model <name>         FastEmbed model (default: bge-base-en-v1.5)\n\
  --reader-think                   Enable reader thinking (default)\n\
  --reader-no-think                Disable reader thinking\n\
  --reader-temperature <F>         Reader sampling temperature (default: 1.0)\n\
  --reader-top-p <F>               Reader nucleus sampling cutoff (default: 0.95)\n\
  --reader-top-k <N>               Reader top-k sampling cutoff (default: 20)\n\
  --reader-presence-penalty <F>    Reader presence penalty (default: 1.5)\n\
  --generation-seed <N>            Reader generation seed (default: 42)\n\
  --reader-num-ctx <N>             Reader context window (default: 32768)\n\
  --reader-num-predict <N>         Reader output budget (default: 8192)\n\
  --ollama-base-url <url>          Loopback Ollama URL (default: http://127.0.0.1:11434)\n\
  --timeout-secs <N>               Per-generation timeout (default: 600)\n\
  --embed-cache <path>             SQLite embedding cache\n\
  --allow-download                 Allow FastEmbed initialization/download\n\
  --output <path>                  Incremental JSON report path\n\
  --resume                         Resume the exact report/configuration\n\
  --force                          Overwrite an existing report\n\
  --data-dir <path>                Dataset directory (default: benches/eval/data)\n\
  --help                           Show this usage\n\n\
Routes:\n\
  0-full-context         Complete history + baseline local reader\n\
  1-oracle-baseline      Gold evidence + baseline local reader\n\
  2-retrieval-baseline   Memory+BGE evidence + same reader\n\
  3-retrieval-strong     Same retrieval + stronger local or frontier reader\n\
Route 0 is added with --full-context and route 3 with --run-strong-reader, \
--run-reflect-reader, or --reflect-complex-only. Add --frontier-reader to move route 3 to an \
OpenAI-compatible API; the bearer token is read only from LLM_API_KEY. \
LoCoMo routes receive the official deterministic F1; every route also receives \
an explicitly secondary local-judge score."
    );
}
