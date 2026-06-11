use std::error::Error;
use std::fmt;
use std::path::PathBuf;

pub type BenchResult<T> = Result<T, BenchError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchError {
    DatasetNotFound { path: PathBuf, hint: String },
    Parse(String),
    Embedding(String),
    Engine(String),
    InvalidInput(String),
}

impl fmt::Display for BenchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BenchError::DatasetNotFound { path, hint } => {
                write!(f, "Dataset not found at {}. {hint}", path.display())
            }
            BenchError::Parse(message) => write!(f, "parse error: {message}"),
            BenchError::Embedding(message) => write!(f, "embedding error: {message}"),
            BenchError::Engine(message) => write!(f, "engine error: {message}"),
            BenchError::InvalidInput(message) => write!(f, "invalid input: {message}"),
        }
    }
}

impl Error for BenchError {}
