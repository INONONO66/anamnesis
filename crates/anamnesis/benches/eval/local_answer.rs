#[path = "../eval_common/mod.rs"]
mod eval_common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anamnesis::engine::EmbeddingProvider;
use serde::{Deserialize, Serialize};

use eval_common::answer_metrics;
use eval_common::real_bench::dataset::{
    BenchDatasetName, BenchQuestion, BenchSession, GoldEvidence, LoadedBenchmark,
    load_benchmark_dataset, restrict_to_questions, split_by_sample,
};
use eval_common::real_bench::graph::{
    AnswerContext, CachingProvider, EvalOptions, QuestionEvaluation, build_memory_graph,
    evaluate_question_with_context,
};
use eval_common::real_bench::{BenchError, BenchResult};

#[cfg(not(feature = "embed"))]
compile_error!("local_answer requires: cargo bench --features embed --bench local_answer");

const SCHEMA_VERSION: u32 = 15;
const DATASET_LOADER_VERSION: &str = "locomo-caption-v2+longmemeval-cleaned-v1";
const ANSWER_PROMPT_VERSION: &str = "official-format-v6-temporal-anchor";
const ENGINE_PACKAGE_POLICY_VERSION: &str = "baseline-package-v0";
const SHADOW_RRF_POLICY_VERSION: &str = "shadow-rrf-cognitive1-embedding0.25-text1-k60-v1";
const ROUTE_FULL_CONTEXT: &str = "0-full-context";
const ROUTE_ORACLE_BASELINE: &str = "1-oracle-baseline";
const ROUTE_RETRIEVAL_BASELINE: &str = "2-retrieval-baseline";
const ROUTE_RETRIEVAL_STRONG: &str = "3-retrieval-strong";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RunConfig {
    dataset: BenchDatasetName,
    samples: Option<usize>,
    stratify: Option<usize>,
    question_type: Option<String>,
    sample_seed: u64,
    skip_adversarial: bool,
    run_strong_reader: bool,
    run_full_context: bool,
    run_local_judge: bool,
    run_oracle_baseline: bool,
    run_retrieval_baseline: bool,
    compact_retrieval_context: bool,
    hydrate_episodic_context: bool,
    shadow_rank_fusion: bool,
    shadow_cross_encoder: Option<String>,
    shadow_cross_encoder_candidates: usize,
    top_k: usize,
    answer_prompt_version: String,
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
    judge: Option<JudgeDecision>,
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
    retrieval_bottleneck_cases: usize,
    strong_reader_recoveries: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RetrievalSummary {
    evaluated: usize,
    mean_readout_recall_at_k: f64,
    mean_package_recall_at_k: f64,
    readout_hit_at_k: f64,
    package_hit_at_k: f64,
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
    run_full_context: bool,
    run_local_judge: bool,
    run_oracle_baseline: bool,
    run_retrieval_baseline: bool,
    compact_retrieval_context: bool,
    hydrate_episodic_context: bool,
    shadow_rank_fusion: bool,
    shadow_cross_encoder: Option<String>,
    shadow_cross_encoder_candidates: usize,
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

    let ollama = OllamaClient::new(&args.ollama_base_url, args.timeout_secs)?;
    let ollama_version = ollama.version()?;
    let mut requested_models = vec![args.baseline_reader_model.as_str()];
    if args.run_local_judge {
        requested_models.push(args.judge_model.as_str());
    }
    if args.run_strong_reader {
        requested_models.push(args.strong_reader_model.as_str());
    }
    let model_digests = ollama.require_models(&requested_models)?;

    eprintln!(
        "LOCAL ollama={} models={:?}",
        ollama_version, requested_models
    );
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
    let shadow_cross_encoder = args
        .shadow_cross_encoder
        .as_deref()
        .map(|model_name| {
            let model = model_name
                .parse::<fastembed::RerankerModel>()
                .map_err(|err| {
                    BenchError::InvalidInput(format!("unknown cross-encoder model: {err}"))
                })?;
            fastembed::TextRerank::try_new(
                fastembed::RerankInitOptions::new(model)
                    .with_cache_dir(PathBuf::from(".fastembed_cache")),
            )
            .map(Arc::new)
            .map_err(|err| BenchError::Embedding(format!("cross-encoder init failed: {err}")))
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
        run_full_context: args.run_full_context,
        run_local_judge: args.run_local_judge,
        run_oracle_baseline: args.run_oracle_baseline,
        run_retrieval_baseline: args.run_retrieval_baseline,
        compact_retrieval_context: args.compact_retrieval_context,
        hydrate_episodic_context: args.hydrate_episodic_context,
        shadow_rank_fusion: args.shadow_rank_fusion,
        shadow_cross_encoder: args.shadow_cross_encoder.clone(),
        shadow_cross_encoder_candidates: args.shadow_cross_encoder_candidates,
        top_k: args.top_k,
        answer_prompt_version: ANSWER_PROMPT_VERSION.to_string(),
        baseline_reader_model: args.baseline_reader_model.clone(),
        strong_reader_model: args.strong_reader_model.clone(),
        judge_model: args.judge_model.clone(),
        embedding_model,
        dataset_loader_version: DATASET_LOADER_VERSION.to_string(),
        engine_package_policy_version: if let Some(model) = &args.shadow_cross_encoder {
            format!(
                "shadow-cross-encoder-top{}-{model}-v1",
                args.shadow_cross_encoder_candidates
            )
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
        seed_limit: None,
        dump_features: false,
        speaker_cues: false,
        shadow_rank_fusion: args.shadow_rank_fusion,
        shadow_cross_encoder,
        shadow_cross_encoder_candidates: args.shadow_cross_encoder_candidates,
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
        let mut graph = build_memory_graph(group, provider.clone())?;
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
            let mut retrieval_prompt_context = retrieval_prompt_context(group, &retrieval_context);
            if args.compact_retrieval_context {
                retrieval_prompt_context = compact_prompt_context(retrieval_prompt_context);
            }
            if args.hydrate_episodic_context {
                hydrate_episodic_context(group, &mut retrieval_prompt_context);
            }
            if args.run_full_context {
                let full_context = full_prompt_context(group);
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_FULL_CONTEXT,
                    &args.baseline_reader_model,
                    &full_context,
                    &ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
            if args.run_oracle_baseline {
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_ORACLE_BASELINE,
                    &args.baseline_reader_model,
                    &oracle_prompt_context,
                    &ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
            if args.run_retrieval_baseline {
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_RETRIEVAL_BASELINE,
                    &args.baseline_reader_model,
                    &retrieval_prompt_context,
                    &ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
            if args.run_strong_reader {
                run_answer(
                    &mut report,
                    record_index,
                    ROUTE_RETRIEVAL_STRONG,
                    &args.strong_reader_model,
                    &retrieval_prompt_context,
                    &ollama,
                    &args.reader_generation,
                )?;
                write_report(&args.output, &report)?;
            }
        }
    }

    if args.run_local_judge {
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
                run_judge(&mut report, record_index, route, &ollama, &args)?;
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
        local_only: true,
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
                judge: None,
            },
        );
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
        })
        .collect()
}

fn retrieval_prompt_context(
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
            })
        })
        .collect()
}

fn answer_prompt(record: &QuestionRecord, context: &[PromptEvidence]) -> String {
    let mut prompt = String::from(
        "You are a memory question-answering reader. Use only the supplied evidence. \
         Think briefly and reserve enough output budget for the final answer. \
         Combine multiple evidence items and reason about dates when needed. \
         Give only the shortest direct answer that fully answers the question. \
         Do not mention the evidence or explain your reasoning. \
         If the evidence is insufficient, answer exactly No information available.\n\n",
    );
    prompt.push_str(question_type_instruction(&record.question_type));
    prompt.push('\n');
    if let Some(date) = &record.question_date {
        prompt.push_str(&format!("Question date: {date}\n"));
    }
    prompt.push_str("Evidence:\n");
    for item in context {
        prompt.push_str(&format!("[{}]\n{}\n", item.label, item.text));
    }
    prompt.push_str("\nQuestion: ");
    prompt.push_str(&record.question);
    prompt.push_str("\nAnswer:");
    prompt
}

fn question_type_instruction(question_type: &str) -> &'static str {
    match question_type {
        "temporal" | "temporal-reasoning" => {
            "For this temporal question, resolve a relative expression inside an evidence item \
             against that same evidence item's date. Do not resolve evidence text against the \
             Question date. Use the Question date only for a relative expression in the question \
             itself. Preserve the answer's requested granularity and use a natural-language date \
             such as 'the Friday before 15 July 2023' rather than an ISO date."
        }
        "multi-hop" | "multi-session" => {
            "For this multi-evidence question, combine all relevant items before answering. \
             Count distinct events or entities explicitly when the question asks for a count. \
             Separate distinct answer items with commas rather than joining them with 'and'."
        }
        "knowledge-update" => {
            "For this update question, prefer the latest applicable fact and do not mix it with \
             an older superseded value."
        }
        "preference" => {
            "For this preference question, infer only preferences directly grounded in the \
             supplied evidence; a concise grounded recommendation is allowed."
        }
        "open-domain" => {
            "For this open-domain question, make the shortest reasonable inference from the \
             supplied evidence and ordinary commonsense. Do not answer UNKNOWN merely because \
             the conclusion is implicit rather than stated word for word."
        }
        "adversarial" => {
            "For this adversarial question, answer exactly No information available when the \
             requested claim is not supported by the supplied evidence."
        }
        _ => {
            "Extract the exact requested fact from the supplied evidence and preserve names, \
             numbers, units, and negation."
        }
    }
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
                "Mark correct if the candidate contains the correct answer, an equivalent \
                 answer, or all intermediate facts needed to derive it. A response containing \
                 only a subset of required information is incorrect."
            }
        }
    };
    format!(
        "You are an impartial benchmark judge. {criterion} Return one JSON object only, with \
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
    let readout_recall = evaluated
        .iter()
        .map(|(evaluation, _)| evaluation.retrieval_metrics.recall_at_k)
        .sum::<f64>();
    let package_recall = evaluated
        .iter()
        .map(|(evaluation, _)| evaluation.package_metrics.recall_at_k)
        .sum::<f64>();
    let readout_hits = evaluated
        .iter()
        .filter(|(evaluation, _)| evaluation.first_hit_rank.is_some())
        .count();
    let package_hits = evaluated
        .iter()
        .filter(|(_, context)| context.evidence.iter().any(|item| item.relevant))
        .count();
    RetrievalSummary {
        evaluated: count,
        mean_readout_recall_at_k: readout_recall / count as f64,
        mean_package_recall_at_k: package_recall / count as f64,
        readout_hit_at_k: ratio(readout_hits, count),
        package_hit_at_k: ratio(package_hits, count),
    }
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
        "{:<28} {:>8} {:>10} {:>18} {:>10} {:>12}",
        "Route", "Judged", "Judge", "95% CI", "Macro", "LoCoMo F1"
    );
    eprintln!(
        "{:-<28} {:-<8} {:-<10} {:-<18} {:-<10} {:-<12}",
        "", "", "", "", "", ""
    );
    for (route, values) in &summary.routes {
        let official = values.locomo_official_f1.map_or_else(
            || "n/a".to_string(),
            |score| format!("{:.1}%", score * 100.0),
        );
        eprintln!(
            "{:<28} {:>8} {:>9.1}% {:>7.1}%..{:>6.1}% {:>9.1}% {:>12}",
            route,
            values.judged,
            values.accuracy * 100.0,
            values.accuracy_ci95_low * 100.0,
            values.accuracy_ci95_high * 100.0,
            values.macro_accuracy * 100.0,
            official
        );
    }
    eprintln!(
        "retrieval package recall={:.3} hit={:.3}",
        summary.retrieval.mean_package_recall_at_k, summary.retrieval.package_hit_at_k
    );
    eprintln!(
        "retrieval bottlenecks={} strong-reader recoveries={}",
        summary.retrieval_bottleneck_cases, summary.strong_reader_recoveries
    );
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
    let mut run_full_context = false;
    let mut run_local_judge = true;
    let mut run_oracle_baseline = true;
    let mut run_retrieval_baseline = true;
    let mut compact_retrieval_context = false;
    let mut hydrate_episodic_context = false;
    let mut shadow_rank_fusion = false;
    let mut shadow_cross_encoder = None;
    let mut shadow_cross_encoder_candidates = 100usize;
    let mut top_k = 20usize;
    let mut baseline_reader_model = "qwen3.5:35b-a3b".to_string();
    let mut strong_reader_model = "qwen3.5:35b-a3b".to_string();
    let mut judge_model = "gemma3:12b".to_string();
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
            "--baseline-only" => run_strong_reader = false,
            "--run-strong-reader" => run_strong_reader = true,
            "--full-context" => run_full_context = true,
            "--skip-local-judge" => run_local_judge = false,
            "--retrieval-only" => run_oracle_baseline = false,
            "--oracle-only" => run_retrieval_baseline = false,
            "--compact-retrieval-context" => compact_retrieval_context = true,
            "--hydrate-episodic-context" => hydrate_episodic_context = true,
            "--shadow-rank-fusion" => shadow_rank_fusion = true,
            "--shadow-cross-encoder" => {
                shadow_cross_encoder = Some(next_value(&mut iter, "--shadow-cross-encoder")?)
            }
            "--shadow-cross-encoder-candidates" => {
                shadow_cross_encoder_candidates = parse_usize(
                    &next_value(&mut iter, "--shadow-cross-encoder-candidates")?,
                    "--shadow-cross-encoder-candidates",
                )?
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
    if shadow_rank_fusion && shadow_cross_encoder.is_some() {
        return Err(BenchError::InvalidInput(
            "--shadow-rank-fusion and --shadow-cross-encoder are mutually exclusive".to_string(),
        ));
    }
    if !(1..=200).contains(&shadow_cross_encoder_candidates) {
        return Err(BenchError::InvalidInput(
            "--shadow-cross-encoder-candidates must be in 1..=200".to_string(),
        ));
    }
    if !run_oracle_baseline && !run_retrieval_baseline && !run_full_context && !run_strong_reader {
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
        run_full_context,
        run_local_judge,
        run_oracle_baseline,
        run_retrieval_baseline,
        compact_retrieval_context,
        hydrate_episodic_context,
        shadow_rank_fusion,
        shadow_cross_encoder,
        shadow_cross_encoder_candidates,
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
  --retrieval-only                 Skip dataset-annotated evidence answers\n\
  --oracle-only                    Skip retrieval answers; run only annotated-evidence answers\n\
  --compact-retrieval-context      Keep the richest retrieved item per source turn\n\
  --hydrate-episodic-context       Replace packaged turn labels with source fragments\n\
  --shadow-rank-fusion             Benchmark-only top-200 cognitive/embed/text RRF candidate\n\
  --shadow-cross-encoder <model>   Benchmark-only local reranker over the live top-100\n\
  --shadow-cross-encoder-candidates <N>\n\
                                   Cognitive candidate cutoff (default: 100)\n\
  --run-strong-reader              Add route 3 with --strong-reader-model\n\
  --baseline-only                  Compatibility alias: omit route 3\n\
  --top-k <N>                      Product retrieval cutoff (default: 20)\n\
  --baseline-reader-model <name>   Reader for routes 0, 1, and 2 (default: qwen3.5:35b-a3b)\n\
  --strong-reader-model <name>     Reader for route 3 (default: qwen3.5:35b-a3b)\n\
  --judge-model <name>             Separate local judge (default: gemma3:12b)\n\
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
  3-retrieval-strong     Same retrieval + stronger local reader\n\
Route 0 is added with --full-context and route 3 with --run-strong-reader. \
LoCoMo routes receive the official deterministic F1; every route also receives \
an explicitly secondary local-judge score."
    );
}
