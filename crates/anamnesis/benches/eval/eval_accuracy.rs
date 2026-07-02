#![allow(unused_imports)]

#[path = "../eval_common/mod.rs"]
mod eval_common;

use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::engine::{EngineConfig, SqliteStorage};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::{Fragment, SearchInput};

use eval_common::checkpoint::{self, Checkpoint, Phase, QuestionResult};
use eval_common::datasets::{
    ConvoMemLoader, Dataset, DatasetError, LoCoMoLoader, LongMemEvalLoader, UnifiedQuestion,
    UnifiedSession,
};
use eval_common::judge::{Judge, LlmJudge, MockJudge};
use eval_common::metrics::{compute_report, print_summary, write_json_report};
use eval_common::provider::{LlmProvider, OpenAiCompatibleProvider, ProviderConfig};

type DynError = Box<dyn Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatasetName {
    Locomo,
    LongMemEval,
    ConvoMem,
}

impl DatasetName {
    fn as_str(self) -> &'static str {
        match self {
            DatasetName::Locomo => "locomo",
            DatasetName::LongMemEval => "longmemeval",
            DatasetName::ConvoMem => "convomem",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JudgeMode {
    Mock,
    Llm,
}

#[derive(Debug, Clone)]
struct Args {
    dataset: DatasetName,
    samples: Option<usize>,
    judge: JudgeMode,
    llm_base_url: String,
    llm_model: String,
    output: PathBuf,
    dry_run: bool,
    resume: bool,
    data_dir: PathBuf,
    db_path: Option<PathBuf>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), DynError> {
    let args = match parse_args(env::args().skip(1)) {
        Ok(ParseOutcome::Run(args)) => args,
        Ok(ParseOutcome::Help) => {
            print_usage();
            return Ok(());
        }
        Err(message) => {
            eprintln!("{message}");
            print_usage();
            return Err(message.into());
        }
    };

    append_learning(&format!(
        "- [{}] eval_accuracy run configured for dataset `{}` with dry_run={} and output `{}`.\n",
        timestamp_secs(),
        args.dataset.as_str(),
        args.dry_run,
        args.output.display()
    ));

    eprintln!("LOAD");
    let (sessions, mut questions) = load_dataset(args.dataset, &args.data_dir)?;
    let sample_limit = args.samples.unwrap_or_else(|| {
        if args.dataset == DatasetName::ConvoMem {
            100
        } else {
            questions.len()
        }
    });
    if questions.len() > sample_limit {
        questions.truncate(sample_limit);
    }

    let checkpoint_path = checkpoint_path_for(&args.output);
    ensure_parent_dir(&checkpoint_path)?;
    let mut checkpoint = load_or_create_checkpoint(&args, &checkpoint_path)?;

    let engine: Engine<SqliteStorage> = if let Some(ref db_path) = args.db_path {
        eprintln!("LOAD DB: {}", db_path.display());
        let storage = SqliteStorage::open(db_path)
            .map_err(|e| format!("failed to open pre-ingested DB: {e}"))?;
        Engine::with_storage(EngineConfig::default(), storage)
    } else {
        if args.resume && checkpoint.ingest_completed {
            eprintln!("Resuming from checkpoint, re-ingesting sessions into fresh engine...");
        } else {
            eprintln!("INGEST");
        }
        let mut engine = Engine::new();
        ingest_sessions(&mut engine, &sessions)?;
        engine
    };
    checkpoint.ingest_completed = true;
    checkpoint.db_path = args.db_path.clone().or(Some(PathBuf::from("in-memory")));
    checkpoint.phase = Phase::Search;
    save_checkpoint(&checkpoint_path, &checkpoint)?;

    let provider_config = ProviderConfig {
        base_url: args.llm_base_url.clone(),
        model: args.llm_model.clone(),
        ..ProviderConfig::default()
    };

    let answer_provider: Option<Box<dyn LlmProvider>> = if args.dry_run {
        None
    } else {
        Some(Box::new(
            OpenAiCompatibleProvider::new(provider_config.clone())
                .map_err(|err| format!("failed to create LLM provider: {err}"))?,
        ))
    };
    let judge = make_judge(args.judge, args.dry_run, provider_config)?;

    for (idx, question) in questions.iter().enumerate() {
        let total = questions.len();
        let question_id = question.question_id.clone();
        let already_evaluated = checkpoint
            .results
            .get(&question_id)
            .is_some_and(|result| result.judge_result.is_some());
        if args.resume && already_evaluated {
            eprintln!("Skipping completed question {}/{}...", idx + 1, total);
            continue;
        }

        eprintln!("Searching question {}/{}...", idx + 1, total);
        let (fragments, search_latency_ms, context_tokens) = search_question(&engine, question)?;

        checkpoint.phase = Phase::Answer;
        let entry = checkpoint
            .results
            .entry(question_id.clone())
            .or_insert_with(empty_question_result);
        entry.search_latency_ms = Some(search_latency_ms);
        entry.context_tokens = Some(context_tokens);
        save_checkpoint(&checkpoint_path, &checkpoint)?;

        eprintln!("Answering question {}/{}...", idx + 1, total);
        let answer = answer_question(&args, answer_provider.as_deref(), question, &fragments);
        let entry = checkpoint
            .results
            .entry(question_id.clone())
            .or_insert_with(empty_question_result);
        entry.answer = Some(answer.clone());
        save_checkpoint(&checkpoint_path, &checkpoint)?;

        eprintln!("Evaluating question {}/{}...", idx + 1, total);
        checkpoint.phase = Phase::Evaluate;
        let judge_result = judge.evaluate(&question.question, &question.expected_answer, &answer);
        let entry = checkpoint
            .results
            .entry(question_id.clone())
            .or_insert_with(empty_question_result);
        entry.judge_result = Some(judge_result);
        checkpoint.completed_questions.insert(question_id);
        save_checkpoint(&checkpoint_path, &checkpoint)?;
    }

    eprintln!("REPORT");
    checkpoint.phase = Phase::Report;
    save_checkpoint(&checkpoint_path, &checkpoint)?;
    let report = compute_report(&checkpoint.results, &questions);
    print_summary(&report);
    write_json_report(&report, &args.output)?;

    append_learning(&format!(
        "- [{}] eval_accuracy wrote `{}` and checkpoint `{}` for {} questions.\n",
        timestamp_secs(),
        args.output.display(),
        checkpoint_path.display(),
        questions.len()
    ));

    Ok(())
}

enum ParseOutcome {
    Run(Args),
    Help,
}

fn parse_args<I>(args: I) -> Result<ParseOutcome, String>
where
    I: IntoIterator<Item = String>,
{
    let mut dataset = None;
    let mut samples = None;
    let mut judge = JudgeMode::Mock;
    let mut llm_base_url = "http://localhost:11434".to_string();
    let mut llm_model = "qwen2.5".to_string();
    let mut output = None;
    let mut dry_run = false;
    let mut resume = false;
    let mut data_dir = PathBuf::from("benches/eval/data");
    let mut db_path = None;
    let mut saw_actionable_arg = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(ParseOutcome::Help),
            "--dataset" => {
                saw_actionable_arg = true;
                dataset = Some(parse_dataset(&next_value(&mut iter, "--dataset")?)?)
            }
            "--samples" => {
                saw_actionable_arg = true;
                samples = Some(parse_usize(&next_value(&mut iter, "--samples")?)?)
            }
            "--judge" => {
                saw_actionable_arg = true;
                judge = parse_judge(&next_value(&mut iter, "--judge")?)?
            }
            "--llm-base-url" => {
                saw_actionable_arg = true;
                llm_base_url = next_value(&mut iter, "--llm-base-url")?
            }
            "--llm-model" => {
                saw_actionable_arg = true;
                llm_model = next_value(&mut iter, "--llm-model")?
            }
            "--output" => {
                saw_actionable_arg = true;
                output = Some(PathBuf::from(next_value(&mut iter, "--output")?))
            }
            "--dry-run" => dry_run = true,
            "--resume" => resume = true,
            "--data-dir" => {
                saw_actionable_arg = true;
                data_dir = PathBuf::from(next_value(&mut iter, "--data-dir")?)
            }
            "--db" => {
                saw_actionable_arg = true;
                db_path = Some(PathBuf::from(next_value(&mut iter, "--db")?))
            }
            "--bench" => {}
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if dataset.is_none() && !saw_actionable_arg {
        return Ok(ParseOutcome::Help);
    }

    let dataset = dataset.ok_or_else(|| "missing required --dataset".to_string())?;
    let output = match (output, resume) {
        (Some(path), _) => path,
        (None, true) => {
            latest_resume_output_path(dataset).unwrap_or_else(|| default_output_path(dataset))
        }
        (None, false) => default_output_path(dataset),
    };

    Ok(ParseOutcome::Run(Args {
        dataset,
        samples,
        judge,
        llm_base_url,
        llm_model,
        output,
        dry_run,
        resume,
        data_dir,
        db_path,
    }))
}

fn next_value<I>(iter: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    iter.next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn parse_dataset(value: &str) -> Result<DatasetName, String> {
    match value {
        "locomo" => Ok(DatasetName::Locomo),
        "longmemeval" => Ok(DatasetName::LongMemEval),
        "convomem" => Ok(DatasetName::ConvoMem),
        other => Err(format!(
            "invalid dataset {other:?}; expected locomo, longmemeval, or convomem"
        )),
    }
}

fn parse_judge(value: &str) -> Result<JudgeMode, String> {
    match value {
        "mock" => Ok(JudgeMode::Mock),
        "llm" => Ok(JudgeMode::Llm),
        other => Err(format!("invalid judge {other:?}; expected mock or llm")),
    }
}

fn parse_usize(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|err| format!("invalid --samples value {value:?}: {err}"))
}

fn print_usage() {
    eprintln!(
        "Usage: cargo bench --bench eval_accuracy -- --dataset <locomo|longmemeval|convomem> [options]\n\n\
Options:\n\
  --samples <N>            Limit questions (default: 100 for convomem, all for others)\n\
  --judge <mock|llm>       Judge mode (default: mock)\n\
  --llm-base-url <url>     LLM base URL (default: http://localhost:11434)\n\
  --llm-model <name>       LLM model name (default: qwen2.5)\n\
  --output <path>          Report path (default: benches/eval/results/{{dataset}}-{{timestamp}}.json)\n\
  --dry-run                Use MockJudge and top search hit answers; no LLM calls\n\
  --resume                 Resume from the checkpoint next to --output\n\
  --data-dir <path>        Dataset directory (default: benches/eval/data)\n\
  --db <path>              Pre-ingested SQLite file (skips ingest phase)\n\
  --help                   Show this usage"
    );
}

fn default_output_path(dataset: DatasetName) -> PathBuf {
    PathBuf::from(format!(
        "benches/eval/results/{}-{}.json",
        dataset.as_str(),
        timestamp_secs()
    ))
}

fn latest_resume_output_path(dataset: DatasetName) -> Option<PathBuf> {
    let dir = Path::new("benches/eval/results");
    let prefix = format!("{}-", dataset.as_str());
    let suffix = ".checkpoint.json";
    let entries = fs::read_dir(dir).ok()?;
    let mut newest: Option<(SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with(&prefix) || !file_name.ends_with(suffix) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(current, _)| modified > *current)
        {
            let stem = file_name.trim_end_matches(suffix);
            newest = Some((modified, dir.join(format!("{stem}.json"))));
        }
    }

    newest.map(|(_, path)| path)
}

fn timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn checkpoint_path_for(output: &Path) -> PathBuf {
    let mut checkpoint = output.to_path_buf();
    checkpoint.set_extension("checkpoint.json");
    checkpoint
}

fn ensure_parent_dir(path: &Path) -> Result<(), DynError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn load_dataset(
    dataset: DatasetName,
    data_dir: &Path,
) -> Result<(Vec<UnifiedSession>, Vec<UnifiedQuestion>), DynError> {
    let result = match dataset {
        DatasetName::Locomo => LoCoMoLoader.load(data_dir),
        DatasetName::LongMemEval => LongMemEvalLoader.load(data_dir),
        DatasetName::ConvoMem => ConvoMemLoader.load(data_dir),
    };

    match result {
        Ok(data) => Ok(data),
        Err(DatasetError::NotFound { path, hint }) => {
            eprintln!("Dataset not found at {path}. {hint}");
            eprintln!(
                "Dataset not found. Run: cargo bench --bench download_datasets -- --dataset {}",
                dataset.as_str()
            );
            process::exit(1);
        }
        Err(DatasetError::ParseError(message)) => {
            Err(format!("failed to parse {} dataset: {message}", dataset.as_str()).into())
        }
        Err(DatasetError::IoError(message)) => {
            Err(format!("failed to read {} dataset: {message}", dataset.as_str()).into())
        }
    }
}

fn load_or_create_checkpoint(args: &Args, path: &Path) -> Result<Checkpoint, DynError> {
    if args.resume
        && let Some(checkpoint) = checkpoint::load(path).map_err(format_checkpoint_error)?
    {
        if checkpoint.dataset == args.dataset.as_str() {
            eprintln!("Resuming checkpoint {}", path.display());
            return Ok(checkpoint);
        }
        eprintln!(
            "Ignoring checkpoint for dataset {}; current dataset is {}",
            checkpoint.dataset,
            args.dataset.as_str()
        );
    }

    Ok(Checkpoint {
        run_id: format!("{}-{}", args.dataset.as_str(), timestamp_secs()),
        dataset: args.dataset.as_str().to_string(),
        phase: Phase::Ingest,
        completed_questions: HashSet::new(),
        ingest_completed: false,
        db_path: None,
        results: HashMap::new(),
    })
}

fn save_checkpoint(path: &Path, checkpoint: &Checkpoint) -> Result<(), DynError> {
    checkpoint::save(path, checkpoint)
        .map_err(format_checkpoint_error)
        .map_err(Into::into)
}

fn format_checkpoint_error(err: checkpoint::CheckpointError) -> String {
    match err {
        checkpoint::CheckpointError::IoError(message) => format!("checkpoint I/O error: {message}"),
        checkpoint::CheckpointError::SerdeError(message) => {
            format!("checkpoint serialization error: {message}")
        }
    }
}

fn ingest_sessions(
    engine: &mut Engine<SqliteStorage>,
    sessions: &[UnifiedSession],
) -> Result<(), DynError> {
    for (idx, session) in sessions.iter().enumerate() {
        eprintln!("Ingesting session {}/{}...", idx + 1, sessions.len());
        for (turn_idx, turn) in session.turns.iter().enumerate() {
            if turn.content.trim().is_empty() {
                continue;
            }
            let observation = Observation {
                name: make_observation_name(&turn.content),
                summary: Some(format!("{} turn {}", turn.role, turn_idx + 1)),
                content: turn.content.clone(),
                embedding: None,
                confidence: 0.9,
                node_type: KnowledgeType::Episodic,
                entity_tags: vec![],
                origin: Origin {
                    peer_id: anamnesis::graph::types::PeerId(0),
                    source_kind: anamnesis::engine::SourceKind::AgentObservation,
                    session_id: session.session_id.clone(),
                    scope: ScopePath::universal(),
                    confidence: 0.9,
                },
                timestamp: Timestamp(0),
                valid_from: None,
                valid_until: None,
            };
            match engine.ingest(observation)? {
                IngestResult::Created(_) | IngestResult::Reinforced { .. } => {}
            }
        }
    }
    Ok(())
}

fn make_observation_name(content: &str) -> String {
    let mut name: String = content.chars().take(50).collect();
    if name.trim().is_empty() {
        name = "empty turn".to_string();
    }
    name
}

fn search_question(
    engine: &Engine<SqliteStorage>,
    question: &UnifiedQuestion,
) -> Result<(Vec<Fragment>, f64, usize), DynError> {
    let search_input = SearchInput {
        text: question.question.clone(),
        limit: 20,
        ..Default::default()
    };
    let start = Instant::now();
    let result = engine.search(search_input)?;
    let search_latency_ms = start.elapsed().as_secs_f64() * 1000.0;
    let context_tokens = result.package.token_usage.used;
    let fragments = collect_fragments(&result.package);
    Ok((fragments, search_latency_ms, context_tokens))
}

fn collect_fragments(package: &anamnesis::query::ContextPackage) -> Vec<Fragment> {
    package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .cloned()
        .collect()
}

fn answer_question(
    args: &Args,
    provider: Option<&dyn LlmProvider>,
    question: &UnifiedQuestion,
    fragments: &[Fragment],
) -> String {
    if args.dry_run {
        return top_fragment_answer(fragments).unwrap_or_default();
    }

    let Some(provider) = provider else {
        eprintln!("LLM provider unavailable; using empty answer");
        return String::new();
    };

    let prompt = build_answer_prompt(question, fragments);
    match provider.generate(&prompt) {
        Ok(answer) => answer,
        Err(err) => {
            eprintln!(
                "LLM answer generation failed for {}: {err}",
                question.question_id
            );
            String::new()
        }
    }
}

fn top_fragment_answer(fragments: &[Fragment]) -> Option<String> {
    fragments.first().map(fragment_text)
}

fn build_answer_prompt(question: &UnifiedQuestion, fragments: &[Fragment]) -> String {
    let mut prompt = String::from(
        "Answer the question using only the context below. If the answer is not present, reply with an empty string.\n\nContext:\n",
    );
    for (idx, fragment) in fragments.iter().enumerate() {
        prompt.push_str(&format!("[{}] {}\n", idx + 1, fragment_text(fragment)));
    }
    prompt.push_str("\nQuestion:\n");
    prompt.push_str(&question.question);
    prompt.push_str("\n\nAnswer:");
    prompt
}

fn fragment_text(fragment: &Fragment) -> String {
    fragment
        .content
        .as_ref()
        .or(fragment.summary.as_ref())
        .unwrap_or(&fragment.name)
        .clone()
}

fn make_judge(
    mode: JudgeMode,
    dry_run: bool,
    provider_config: ProviderConfig,
) -> Result<Box<dyn Judge>, DynError> {
    if dry_run || mode == JudgeMode::Mock {
        return Ok(Box::new(MockJudge));
    }

    Ok(Box::new(LlmJudge {
        provider: Box::new(
            OpenAiCompatibleProvider::new(provider_config)
                .map_err(|err| format!("failed to create judge provider: {err}"))?,
        ),
        system_prompt: String::new(),
    }))
}

fn empty_question_result() -> QuestionResult {
    QuestionResult {
        answer: None,
        judge_result: None,
        search_latency_ms: None,
        context_tokens: None,
    }
}

fn append_learning(line: &str) {
    let path = Path::new(".omo/notepads/bench-infra/learnings.md");
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(line.as_bytes()) {
                eprintln!("failed to append learning: {err}");
            }
        }
        Err(err) => eprintln!("failed to open learnings notepad: {err}"),
    }
}
