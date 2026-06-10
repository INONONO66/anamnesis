use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::eval_common::real_bench::dataset::BenchDatasetName;
use crate::eval_common::real_bench::{BenchError, BenchResult};

const MAX_TOP_K: usize = 100;

#[derive(Debug, Clone)]
pub(crate) struct Args {
    pub(crate) dataset: BenchDatasetName,
    pub(crate) data_dir: PathBuf,
    pub(crate) output: PathBuf,
    pub(crate) samples: Option<usize>,
    pub(crate) warmup: usize,
    pub(crate) top_k: usize,
    pub(crate) seed_limit: Option<usize>,
    pub(crate) stratify: Option<usize>,
    pub(crate) skip_adversarial: bool,
    pub(crate) allow_download: bool,
    pub(crate) force: bool,
    pub(crate) embed_cache: Option<PathBuf>,
}

pub(crate) fn parse_args<I>(args: I) -> BenchResult<Option<Args>>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = ParsedArgs::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--dataset" => {
                parsed.saw_arg = true;
                parsed.dataset = Some(
                    BenchDatasetName::parse(&next_value(&mut iter, "--dataset")?)
                        .map_err(BenchError::InvalidInput)?,
                );
            }
            "--data-dir" => {
                parsed.saw_arg = true;
                parsed.data_dir = PathBuf::from(next_value(&mut iter, "--data-dir")?);
            }
            "--output" => {
                parsed.saw_arg = true;
                parsed.output = Some(PathBuf::from(next_value(&mut iter, "--output")?));
            }
            "--samples" => {
                parsed.saw_arg = true;
                parsed.samples = Some(parse_usize(
                    &next_value(&mut iter, "--samples")?,
                    "--samples",
                )?);
            }
            "--warmup" => {
                parsed.saw_arg = true;
                parsed.warmup = parse_usize(&next_value(&mut iter, "--warmup")?, "--warmup")?;
            }
            "--top-k" => {
                parsed.saw_arg = true;
                parsed.top_k = parse_usize(&next_value(&mut iter, "--top-k")?, "--top-k")?;
            }
            "--seed-limit" => {
                parsed.saw_arg = true;
                parsed.seed_limit =
                    Some(parse_usize(&next_value(&mut iter, "--seed-limit")?, "--seed-limit")?);
            }
            "--stratify" => {
                parsed.saw_arg = true;
                parsed.stratify =
                    Some(parse_usize(&next_value(&mut iter, "--stratify")?, "--stratify")?);
            }
            "--full" => {
                parsed.saw_arg = true;
                parsed.full = true;
            }
            "--skip-adversarial" => {
                parsed.saw_arg = true;
                parsed.skip_adversarial = true;
            }
            "--allow-download" => {
                parsed.saw_arg = true;
                parsed.allow_download = true;
            }
            "--force" => {
                parsed.saw_arg = true;
                parsed.force = true;
            }
            "--embed-cache" => {
                parsed.saw_arg = true;
                parsed.embed_cache = Some(PathBuf::from(next_value(&mut iter, "--embed-cache")?));
            }
            "--bench" => {}
            other => {
                return Err(BenchError::InvalidInput(format!(
                    "unknown argument: {other}"
                )));
            }
        }
    }

    if !parsed.saw_arg {
        return Ok(None);
    }
    let full = parsed.full;
    let args = parsed.into_args()?;
    validate_args(&args, full)?;
    Ok(Some(args))
}

#[derive(Debug, Clone)]
struct ParsedArgs {
    dataset: Option<BenchDatasetName>,
    data_dir: PathBuf,
    output: Option<PathBuf>,
    samples: Option<usize>,
    warmup: usize,
    top_k: usize,
    seed_limit: Option<usize>,
    stratify: Option<usize>,
    full: bool,
    skip_adversarial: bool,
    allow_download: bool,
    force: bool,
    embed_cache: Option<PathBuf>,
    saw_arg: bool,
}

impl Default for ParsedArgs {
    fn default() -> Self {
        Self {
            dataset: None,
            data_dir: PathBuf::from("benches/eval/data"),
            output: None,
            samples: None,
            warmup: 0,
            top_k: 20,
            seed_limit: None,
            stratify: None,
            full: false,
            skip_adversarial: false,
            allow_download: false,
            force: false,
            embed_cache: None,
            saw_arg: false,
        }
    }
}

impl ParsedArgs {
    fn into_args(self) -> BenchResult<Args> {
        let dataset = self
            .dataset
            .ok_or_else(|| BenchError::InvalidInput("missing --dataset".to_string()))?;
        Ok(Args {
            dataset,
            data_dir: self.data_dir,
            output: self.output.unwrap_or_else(|| default_output_path(dataset)),
            samples: self.samples,
            warmup: self.warmup,
            top_k: self.top_k,
            seed_limit: self.seed_limit,
            stratify: self.stratify,
            skip_adversarial: self.skip_adversarial,
            allow_download: self.allow_download,
            force: self.force,
            embed_cache: self.embed_cache,
        })
    }
}

fn validate_args(args: &Args, full: bool) -> BenchResult<()> {
    if args.top_k == 0 || args.top_k > MAX_TOP_K {
        return Err(BenchError::InvalidInput(format!(
            "--top-k must be in 1..={MAX_TOP_K}, got {}",
            args.top_k
        )));
    }
    if args.seed_limit == Some(0) {
        return Err(BenchError::InvalidInput(
            "--seed-limit must be at least 1".to_string(),
        ));
    }
    if args.stratify == Some(0) {
        return Err(BenchError::InvalidInput(
            "--stratify must be at least 1".to_string(),
        ));
    }
    if args.dataset == BenchDatasetName::LongMemEval
        && args.samples.is_none()
        && args.stratify.is_none()
        && !full
    {
        return Err(BenchError::InvalidInput(
            "LongMemEval-S is large; pass --samples <N>, --stratify <N>, or explicit --full"
                .to_string(),
        ));
    }
    Ok(())
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
        .parse::<usize>()
        .map_err(|err| BenchError::InvalidInput(format!("invalid {flag} value {value:?}: {err}")))
}

fn default_output_path(dataset: BenchDatasetName) -> PathBuf {
    PathBuf::from(format!(
        "benches/eval/results/real-memory-{}-{}.json",
        dataset.as_str(),
        timestamp_secs()
    ))
}

fn timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub(crate) fn print_usage() {
    eprintln!(
        "Usage: cargo bench --features embed --bench real_memory -- --dataset <locomo|longmemeval> [options]\n\n\
Options:\n\
  --dataset <name>      Dataset to run (required)\n\
  --data-dir <path>     Dataset directory (default: benches/eval/data)\n\
  --samples <N>         Limit evaluated questions after loading\n\
  --warmup <N>          Commit the first N selected questions before eval (default: 0)\n\
  --top-k <N>           Retrieval cutoff (default: 20)\n\
  --seed-limit <N>      RWR seed count (default: top-k)\n\
  --stratify <N>        Keep first N questions per question_type (LongMemEval; loads full file)\n\
  --skip-adversarial    Drop adversarial-category questions (LoCoMo protocol parity)\n\
  --output <path>       Report path (default: benches/eval/results/real-memory-*.json)\n\
  --allow-download      Allow FastEmbed model download/cache initialization\n\
  --full                Permit uncapped LongMemEval-S runs\n\
  --force               Overwrite an existing report file\n\
  --embed-cache <path>  SQLite embedding cache (model-keyed; speeds reruns)\n\
  --help                Show this usage"
    );
}
