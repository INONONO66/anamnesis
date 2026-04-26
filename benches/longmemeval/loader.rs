//! LongMemEval dataset loader.
//!
//! Loads conversation sessions and evaluation questions from the LongMemEval dataset.
//! Dataset must be downloaded first using `download.sh`.

use std::path::Path;

/// A single conversation turn (speaker + content).
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub speaker: String,
    pub content: String,
}

/// A complete conversation session with metadata.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub turns: Vec<ConversationTurn>,
}

/// A single evaluation question with expected answer.
#[derive(Debug, Clone)]
pub struct EvalQuestion {
    pub question_id: String,
    pub session_id: String,
    pub question: String,
    pub expected_answer: String,
    pub question_type: String,
}

/// Attempt to load sessions from the data directory.
///
/// Returns Ok(sessions) if data files are present, Err(msg) if dataset not downloaded.
/// Checks for actual `.jsonl` files, not just the directory existing.
pub fn load_sessions(data_dir: &Path) -> Result<Vec<Session>, String> {
    let dataset_file = data_dir.join("longmemeval_test.jsonl");
    if !dataset_file.exists() {
        return Err(format!(
            "Dataset file not found: {}. Run download.sh first.",
            dataset_file.display()
        ));
    }
    // TODO: implement JSON parsing when dataset is downloaded
    // For now, return empty (dataset optional for CI)
    Ok(vec![])
}

pub fn load_questions(data_dir: &Path) -> Result<Vec<EvalQuestion>, String> {
    let dataset_file = data_dir.join("longmemeval_test.jsonl");
    if !dataset_file.exists() {
        return Err(format!(
            "Dataset file not found: {}. Run download.sh first.",
            dataset_file.display()
        ));
    }
    Ok(vec![])
}

fn main() {
    let data_dir = Path::new("benches/longmemeval/data");
    match load_sessions(data_dir) {
        Ok(sessions) => {
            println!("Loaded {} sessions", sessions.len());
            if sessions.is_empty() {
                println!("No data found — run benches/longmemeval/download.sh to download dataset");
            }
        }
        Err(e) => {
            println!("Dataset not available: {e}");
            println!("Run benches/longmemeval/download.sh to download dataset");
        }
    }
}
