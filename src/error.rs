//! Error types for the Anamnesis engine.

use crate::graph::types::{EdgeId, NodeId, PeerId};
use std::fmt;

/// All errors that can occur in the Anamnesis engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// A node with the given ID was not found.
    NodeNotFound(NodeId),
    /// An edge with the given ID was not found.
    EdgeNotFound(EdgeId),
    /// An error from the storage backend.
    StorageError(String),
    /// An observation was rejected by the perception gate.
    Rejected(String),
    /// Invalid configuration value.
    InvalidConfig(String),
    /// Invalid input value.
    InvalidInput(String),
    /// Query budget exhausted before completion.
    BudgetExhausted,
    /// A peer with the given ID was not found in the registry.
    PeerNotFound(PeerId),
    /// An alias or identifier is already registered to another peer.
    DuplicateAlias(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NodeNotFound(id) => write!(f, "node not found: {}", id.0),
            Error::EdgeNotFound(id) => write!(f, "edge not found: {}", id.0),
            Error::StorageError(msg) => write!(f, "storage error: {}", msg),
            Error::Rejected(reason) => write!(f, "observation rejected: {}", reason),
            Error::InvalidConfig(msg) => write!(f, "invalid config: {}", msg),
            Error::InvalidInput(msg) => write!(f, "invalid input: {}", msg),
            Error::BudgetExhausted => write!(f, "query budget exhausted"),
            Error::PeerNotFound(id) => write!(f, "peer not found: {}", id.0),
            Error::DuplicateAlias(alias) => write!(f, "duplicate alias: {}", alias),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_error_variants_constructable() {
        let errors = [
            Error::NodeNotFound(NodeId(1)),
            Error::EdgeNotFound(EdgeId(2)),
            Error::StorageError("disk full".to_string()),
            Error::Rejected("low novelty".to_string()),
            Error::InvalidConfig("max_nodes must be > 0".to_string()),
            Error::InvalidInput("search requires text or query_embedding".to_string()),
            Error::BudgetExhausted,
        ];
        assert_eq!(errors.len(), 7);
    }

    #[test]
    fn error_display() {
        let e = Error::NodeNotFound(NodeId(42));
        assert_eq!(e.to_string(), "node not found: 42");

        let e = Error::BudgetExhausted;
        assert_eq!(e.to_string(), "query budget exhausted");
    }

    #[test]
    fn error_is_std_error() {
        let e: Box<dyn std::error::Error> = Box::new(Error::BudgetExhausted);
        assert_eq!(e.to_string(), "query budget exhausted");
    }
}
