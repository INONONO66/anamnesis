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
/// Returns Ok(sessions) if data is present, Err(msg) if dataset not downloaded.
pub fn load_sessions(data_dir: &Path) -> Result<Vec<Session>, String> {
    if !data_dir.exists() {
        return Err(format!(
            "Data directory not found: {}. Run download.sh first.",
            data_dir.display()
        ));
    }
    // TODO: implement JSON parsing when dataset is downloaded
    // For now, return empty (dataset optional for CI)
    Ok(vec![])
}

/// Attempt to load evaluation questions from the data directory.
pub fn load_questions(data_dir: &Path) -> Result<Vec<EvalQuestion>, String> {
    if !data_dir.exists() {
        return Err(format!(
            "Data directory not found: {}. Run download.sh first.",
            data_dir.display()
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
