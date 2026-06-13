use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde_json::Value;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const DEFAULT_OUTPUT_DIR: &str = "benches/eval/data";

// The original HuggingFace repository (snap-llm-workshop/locomo) is no longer
// available. We download from the canonical GitHub repository instead and
// transform the data structure to match our loader expectations.
const LOCOMO_GITHUB_URL: &str =
    "https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json";

const LONGMEMEVAL_REPO: &str = "xiaowu0162/LongMemEval";
const LONGMEMEVAL_REVISION: &str = "2ec2a557f339b6c0369619b1ed5793734cc87533";
const LONGMEMEVAL_FILE: &str = "longmemeval_s";

const CONVOMEM_REPO: &str = "Salesforce/ConvoMem";
const CONVOMEM_REVISION: &str = "e3e9b39115b02346824c70d349350de738f8be41";

#[derive(Clone, Copy)]
enum Dataset {
    Locomo,
    LongMemEval,
    ConvoMem,
}

impl Dataset {
    fn name(self) -> &'static str {
        match self {
            Self::Locomo => "locomo",
            Self::LongMemEval => "longmemeval",
            Self::ConvoMem => "convomem",
        }
    }
}

enum DatasetArg {
    One(Dataset),
    All,
}

struct Config {
    dataset: DatasetArg,
    output_dir: PathBuf,
    force: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let config = match parse_args(env::args().skip(1))? {
        Some(config) => config,
        None => return Ok(()),
    };

    fs::create_dir_all(&config.output_dir)?;

    let token = env::var("HF_TOKEN").ok().filter(|token| !token.is_empty());
    let client = Client::builder()
        .user_agent("anamnesis-benchmark-dataset-downloader/0.1")
        .build()?;

    match config.dataset {
        DatasetArg::One(dataset) => download_dataset(&client, token.as_deref(), dataset, &config)?,
        DatasetArg::All => {
            for dataset in [Dataset::Locomo, Dataset::LongMemEval, Dataset::ConvoMem] {
                download_dataset(&client, token.as_deref(), dataset, &config)?;
            }
        }
    }

    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Config>> {
    let mut dataset = DatasetArg::All;
    let mut output_dir = PathBuf::from(DEFAULT_OUTPUT_DIR);
    let mut force = false;
    let mut args = args.peekable();

    if args.peek().is_none() {
        return Ok(None);
    }

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage();
                return Ok(None);
            }
            "--dataset" => {
                let value = args
                    .next()
                    .ok_or_else(|| invalid_input("--dataset requires a value"))?;
                dataset = parse_dataset(&value)?;
            }
            "--output-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| invalid_input("--output-dir requires a value"))?;
                output_dir = PathBuf::from(value);
            }
            "--force" => force = true,
            "--bench" => {}
            other => return Err(invalid_input(format!("unknown argument: {other}"))),
        }
    }

    Ok(Some(Config {
        dataset,
        output_dir,
        force,
    }))
}

fn parse_dataset(value: &str) -> Result<DatasetArg> {
    match value {
        "locomo" => Ok(DatasetArg::One(Dataset::Locomo)),
        "longmemeval" => Ok(DatasetArg::One(Dataset::LongMemEval)),
        "convomem" => Ok(DatasetArg::One(Dataset::ConvoMem)),
        "all" => Ok(DatasetArg::All),
        other => Err(invalid_input(format!(
            "unknown dataset: {other}; expected locomo, longmemeval, convomem, or all"
        ))),
    }
}

fn print_usage() {
    println!(
        "Usage: download_datasets [OPTIONS]\n\n\
Options:\n\
  --dataset <locomo|longmemeval|convomem|all>  Dataset to download (default: all)\n\
  --output-dir <path>                          Output directory (default: benches/eval/data)\n\
  --force                                      Re-download existing files\n\
  -h, --help                                   Show this help\n\n\
Datasets:\n\
  locomo       LoCoMo locomo10.json\n\
  longmemeval  LongMemEval short split\n\
  convomem     First ConvoMem category evidence batch"
    );
}

fn download_dataset(
    client: &Client,
    token: Option<&str>,
    dataset: Dataset,
    config: &Config,
) -> Result<()> {
    match dataset {
        Dataset::Locomo => download_locomo(
            client,
            &config.output_dir.join("locomo").join("locomo10.json"),
            config.force,
        ),
        Dataset::LongMemEval => download_hf_file(
            client,
            token,
            dataset,
            LONGMEMEVAL_REPO,
            LONGMEMEVAL_REVISION,
            LONGMEMEVAL_FILE,
            &config
                .output_dir
                .join("longmemeval")
                .join("longmemeval_s.json"),
            config.force,
        ),
        Dataset::ConvoMem => download_convomem(client, token, config),
    }
}

fn download_locomo(client: &Client, output_path: &Path, force: bool) -> Result<()> {
    if output_path.exists() && !force {
        eprintln!("Skipping locomo: {} already exists", output_path.display());
        let count = verify_json(output_path)?;
        eprintln!("Verified locomo: {count} entries");
        return Ok(());
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let response = client.get(LOCOMO_GITHUB_URL).send()?.error_for_status()?;
    save_response(Dataset::Locomo, response, output_path)?;

    transform_locomo(output_path)?;

    let count = verify_json(output_path)?;
    eprintln!("Verified locomo: {count} entries");
    Ok(())
}

fn transform_locomo(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let samples: Vec<Value> = serde_json::from_reader(file)?;

    let transformed: Vec<Value> = samples
        .into_iter()
        .map(|sample| {
            let mut new_sample = serde_json::Map::new();
            if let Some(qa) = sample.get("qa") {
                new_sample.insert("qa".to_string(), qa.clone());
            }
            if let Some(conv) = sample.get("conversation").and_then(Value::as_object) {
                for (key, value) in conv {
                    if key.starts_with("session_") && !key.ends_with("_date_time") {
                        new_sample.insert(key.clone(), value.clone());
                    }
                }
            }
            Value::Object(new_sample)
        })
        .collect();

    let mut file = File::create(path)?;
    serde_json::to_writer(&mut file, &transformed)?;
    file.flush()?;
    Ok(())
}

fn download_convomem(client: &Client, token: Option<&str>, config: &Config) -> Result<()> {
    let (category, remote_path) = first_convomem_evidence_path(client, token)?;
    let output_path = config
        .output_dir
        .join("convomem")
        .join(&category)
        .join("1_evidence")
        .join("batched_000.json");

    download_hf_file(
        client,
        token,
        Dataset::ConvoMem,
        CONVOMEM_REPO,
        CONVOMEM_REVISION,
        &remote_path,
        &output_path,
        config.force,
    )
}

fn first_convomem_evidence_path(client: &Client, token: Option<&str>) -> Result<(String, String)> {
    let evidence_root = "core_benchmark/evidence_questions";
    let url = format!(
        "https://huggingface.co/api/datasets/{CONVOMEM_REPO}/tree/{CONVOMEM_REVISION}/{evidence_root}"
    );
    let response = request_with_auth(client.get(url), token)
        .send()?
        .error_for_status()?;
    let tree: Value = response.json()?;
    let entries = tree
        .as_array()
        .ok_or_else(|| invalid_data("ConvoMem tree API did not return a JSON array"))?;

    let mut categories: Vec<String> = entries
        .iter()
        .filter_map(|entry| {
            let entry_type = entry.get("type").and_then(Value::as_str)?;
            let path = entry.get("path").and_then(Value::as_str)?;
            if matches!(entry_type, "directory" | "folder" | "dir") {
                Some(path.rsplit('/').next().unwrap_or(path).to_owned())
            } else {
                None
            }
        })
        .collect();
    categories.sort();

    let category = categories
        .into_iter()
        .next()
        .ok_or_else(|| invalid_data("ConvoMem tree API did not list any category directories"))?;

    let evidence_dir = format!("{evidence_root}/{category}/1_evidence");
    let url = format!(
        "https://huggingface.co/api/datasets/{CONVOMEM_REPO}/tree/{CONVOMEM_REVISION}/{evidence_dir}"
    );
    let response = request_with_auth(client.get(url), token)
        .send()?
        .error_for_status()?;
    let files: Value = response.json()?;
    let first_file = files
        .as_array()
        .and_then(|arr| {
            arr.iter().find_map(|entry| {
                let path = entry.get("path").and_then(Value::as_str)?;
                if path.ends_with(".json") {
                    Some(path.to_owned())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| invalid_data("ConvoMem: no JSON files in 1_evidence directory"))?;

    Ok((category, first_file))
}

#[allow(clippy::too_many_arguments)]
fn download_hf_file(
    client: &Client,
    token: Option<&str>,
    dataset: Dataset,
    repo: &str,
    revision: &str,
    remote_path: &str,
    output_path: &Path,
    force: bool,
) -> Result<()> {
    if output_path.exists() && !force {
        eprintln!(
            "Skipping {}: {} already exists",
            dataset.name(),
            output_path.display()
        );
        let count = verify_json(output_path)?;
        eprintln!("Verified {}: {count} entries", dataset.name());
        return Ok(());
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let url = format!("https://huggingface.co/datasets/{repo}/resolve/{revision}/{remote_path}");
    let response = request_with_auth(client.get(url), token)
        .send()?
        .error_for_status()?;
    save_response(dataset, response, output_path)?;

    let count = verify_json(output_path)?;
    eprintln!("Verified {}: {count} entries", dataset.name());

    Ok(())
}

fn save_response(dataset: Dataset, mut response: Response, output_path: &Path) -> Result<()> {
    let total = response.content_length();
    let temp_path = output_path.with_extension("json.tmp");
    let mut file = File::create(&temp_path)?;
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];

    print_progress(dataset, downloaded, total);

    loop {
        let bytes_read = response.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;
        print_progress(dataset, downloaded, total);
    }

    file.flush()?;
    fs::rename(temp_path, output_path)?;

    Ok(())
}

fn print_progress(dataset: Dataset, downloaded: u64, total: Option<u64>) {
    match total {
        Some(total) => eprintln!("Downloading {}... {downloaded}/{total}", dataset.name()),
        None => eprintln!("Downloading {}... {downloaded}/unknown", dataset.name()),
    }
}

fn verify_json(path: &Path) -> Result<usize> {
    let file = File::open(path)?;
    let value: Value = serde_json::from_reader(file)?;

    let count = match value {
        Value::Array(entries) => entries.len(),
        Value::Object(entries) => entries.len(),
        _ => 1,
    };

    Ok(count)
}

fn request_with_auth(request: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    let request = request.header(USER_AGENT, "anamnesis-benchmark-dataset-downloader/0.1");
    match token {
        Some(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
        None => request,
    }
}

fn invalid_input(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

fn invalid_data(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}
