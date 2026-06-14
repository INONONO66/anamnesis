use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::judge::JudgeResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Phase {
    Ingest,
    Search,
    Answer,
    Evaluate,
    Report,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionResult {
    pub answer: Option<String>,
    pub judge_result: Option<JudgeResult>,
    pub search_latency_ms: Option<f64>,
    pub context_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub run_id: String,
    pub dataset: String,
    pub phase: Phase,
    pub completed_questions: HashSet<String>,
    pub ingest_completed: bool,
    pub db_path: Option<PathBuf>,
    pub results: HashMap<String, QuestionResult>,
}

#[derive(Debug)]
pub enum CheckpointError {
    IoError(String),
    SerdeError(String),
}

pub fn save(path: &Path, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
    let bytes = serde_json::to_vec_pretty(checkpoint)
        .map_err(|err| CheckpointError::SerdeError(err.to_string()))?;
    let mut temp_path = path.to_path_buf();
    temp_path.set_extension("tmp");

    fs::write(&temp_path, bytes).map_err(|err| CheckpointError::IoError(err.to_string()))?;
    fs::rename(&temp_path, path).map_err(|err| {
        let _ = fs::remove_file(&temp_path);
        CheckpointError::IoError(err.to_string())
    })?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Option<Checkpoint>, CheckpointError> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content)
            .map(Some)
            .map_err(|err| CheckpointError::SerdeError(err.to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(CheckpointError::IoError(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_common_save_load_roundtrip() -> Result<(), String> {
        let path = std::env::temp_dir().join(format!(
            "anamnesis-eval-common-checkpoint-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        let mut completed_questions = HashSet::new();
        completed_questions.insert("q1".to_string());

        let mut results = HashMap::new();
        results.insert(
            "q1".to_string(),
            QuestionResult {
                answer: Some("answer".to_string()),
                judge_result: Some(JudgeResult::correct("ok")),
                search_latency_ms: Some(12.5),
                context_tokens: Some(42),
            },
        );

        let checkpoint = Checkpoint {
            run_id: "run-1".to_string(),
            dataset: "dataset".to_string(),
            phase: Phase::Evaluate,
            completed_questions,
            ingest_completed: true,
            db_path: Some(PathBuf::from("db.sqlite")),
            results,
        };

        save(&path, &checkpoint).map_err(|err| format!("save failed: {err:?}"))?;
        let loaded = load(&path).map_err(|err| format!("load failed: {err:?}"))?;
        let _ = fs::remove_file(&path);

        match loaded {
            Some(loaded_checkpoint) => {
                assert_eq!(loaded_checkpoint.run_id, checkpoint.run_id);
                assert_eq!(loaded_checkpoint.dataset, checkpoint.dataset);
                assert!(loaded_checkpoint.ingest_completed);
                assert!(loaded_checkpoint.completed_questions.contains("q1"));
                assert!(loaded_checkpoint.results.contains_key("q1"));
                Ok(())
            }
            None => Err("checkpoint was not loaded".to_string()),
        }
    }

    #[test]
    fn eval_common_missing_checkpoint_returns_none() {
        let result = load(Path::new("/nonexistent/checkpoint.json"));
        assert!(matches!(result, Ok(None)));
    }
}
