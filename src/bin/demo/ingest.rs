use std::path::Path;

use anamnesis::embedding::{EmbeddingProvider, widen};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::storage::SqliteStorage;
use anamnesis::{Engine, FastEmbedProvider, IngestResult, Observation};

pub struct IngestConfig {
    pub chunk_separator: String,
    pub min_chunk_chars: usize,
    pub max_chunk_chars: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            chunk_separator: "\n\n".to_string(),
            min_chunk_chars: 50,
            max_chunk_chars: 2000,
        }
    }
}

pub struct Chunk {
    pub content: String,
    pub source_file: String,
    pub chunk_index: usize,
    pub total_chunks: usize,
}

pub struct IngestReport {
    pub files_processed: usize,
    pub chunks_total: usize,
    pub nodes_created: usize,
    pub nodes_reinforced: usize,
    pub errors: Vec<String>,
}

const SUPPORTED_EXTENSIONS: &[&str] = &["md", "txt", "text", "json"];

pub fn read_and_chunk(path: &Path, config: &IngestConfig) -> Result<Vec<Chunk>, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let source_file = path.display().to_string();

    match ext.as_str() {
        "md" | "txt" | "text" => chunk_text_file(path, &source_file, config),
        "json" => chunk_json_file(path, &source_file, config),
        _ => Err(format!(
            "unsupported file extension '.{ext}'; supported formats: {}",
            SUPPORTED_EXTENSIONS.join(", ")
        )),
    }
}

fn chunk_text_file(
    path: &Path,
    source_file: &str,
    config: &IngestConfig,
) -> Result<Vec<Chunk>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {source_file}: {e}"))?;

    let raw_chunks: Vec<&str> = content.split(&config.chunk_separator).collect();
    let filtered: Vec<String> = raw_chunks
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() >= config.min_chunk_chars)
        .map(|s| truncate_chunk(s, config.max_chunk_chars))
        .collect();

    let total = filtered.len();
    Ok(filtered
        .into_iter()
        .enumerate()
        .map(|(i, content)| Chunk {
            content,
            source_file: source_file.to_string(),
            chunk_index: i,
            total_chunks: total,
        })
        .collect())
}

fn chunk_json_file(
    path: &Path,
    source_file: &str,
    config: &IngestConfig,
) -> Result<Vec<Chunk>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {source_file}: {e}"))?;

    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse JSON in {source_file}: {e}"))?;

    let raw_strings: Vec<String> = match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        other => vec![other.to_string()],
    };

    let filtered: Vec<String> = raw_strings
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() >= config.min_chunk_chars)
        .map(|s| truncate_chunk(s, config.max_chunk_chars))
        .collect();

    let total = filtered.len();
    Ok(filtered
        .into_iter()
        .enumerate()
        .map(|(i, content)| Chunk {
            content,
            source_file: source_file.to_string(),
            chunk_index: i,
            total_chunks: total,
        })
        .collect())
}

fn truncate_chunk(s: String, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s;
    }
    s.chars().take(max_chars).collect()
}

pub fn ingest_file(
    engine: &mut Engine<SqliteStorage>,
    embedder: &FastEmbedProvider,
    path: &Path,
    scope: &str,
    config: &IngestConfig,
) -> Result<IngestReport, String> {
    let chunks = read_and_chunk(path, config)?;

    let mut report = IngestReport {
        files_processed: 1,
        chunks_total: chunks.len(),
        nodes_created: 0,
        nodes_reinforced: 0,
        errors: Vec::new(),
    };

    let filename_tag = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let scope_path = ScopePath::new(scope).map_err(|e| format!("invalid scope '{scope}': {e}"))?;

    let now = Timestamp::now();

    for chunk in &chunks {
        let embedding = match embedder.embed_single(&chunk.content) {
            Ok(vec_f32) => Some(widen(&vec_f32)),
            Err(e) => {
                report.errors.push(format!(
                    "embedding failed for chunk {} of {}: {e}",
                    chunk.chunk_index, chunk.source_file
                ));
                None
            }
        };

        let name = build_chunk_name(&filename_tag, chunk.chunk_index, &chunk.content);

        let observation = Observation {
            name,
            summary: None,
            content: chunk.content.clone(),
            embedding,
            confidence: 0.8,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![filename_tag.clone()],
            origin: Origin {
                agent_id: "demo-agent".to_string(),
                session_id: format!("ingest-{filename_tag}"),
                scope: scope_path.clone(),
                confidence: 0.8,
            },
            timestamp: now,
        };

        match engine.ingest(observation) {
            Ok(IngestResult::Created(_)) => report.nodes_created += 1,
            Ok(IngestResult::Reinforced { .. }) => report.nodes_reinforced += 1,
            Err(e) => report.errors.push(format!(
                "ingest failed for chunk {} of {}: {e}",
                chunk.chunk_index, chunk.source_file
            )),
        }
    }

    Ok(report)
}

pub fn ingest_directory(
    engine: &mut Engine<SqliteStorage>,
    embedder: &FastEmbedProvider,
    dir_path: &Path,
    scope: &str,
    config: &IngestConfig,
) -> Result<IngestReport, String> {
    if !dir_path.is_dir() {
        return Err(format!("{} is not a directory", dir_path.display()));
    }

    let entries = std::fs::read_dir(dir_path)
        .map_err(|e| format!("failed to read directory {}: {e}", dir_path.display()))?;

    let mut report = IngestReport {
        files_processed: 0,
        chunks_total: 0,
        nodes_created: 0,
        nodes_reinforced: 0,
        errors: Vec::new(),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                report.errors.push(format!("failed to read entry: {e}"));
                continue;
            }
        };

        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        match ingest_file(engine, embedder, &path, scope, config) {
            Ok(file_report) => {
                report.files_processed += file_report.files_processed;
                report.chunks_total += file_report.chunks_total;
                report.nodes_created += file_report.nodes_created;
                report.nodes_reinforced += file_report.nodes_reinforced;
                report.errors.extend(file_report.errors);
            }
            Err(e) => {
                report.errors.push(format!("{}: {e}", path.display()));
            }
        }
    }

    Ok(report)
}

fn build_chunk_name(filename_tag: &str, chunk_index: usize, content: &str) -> String {
    let preview: String = content.chars().take(60).collect();
    let preview = preview.lines().next().unwrap_or(&preview);
    format!("{filename_tag}[{chunk_index}]: {preview}")
}
