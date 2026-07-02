//! Scope path representation for knowledge scoping.
//!
//! A scope path is an opaque, normalized string label attached to knowledge
//! (e.g. `"work/company-a/backend-platform"`). The empty path is the universal
//! scope, accessible across all scopes. Scopes are compared by identity only —
//! the former hierarchy (ancestor/descendant/sibling relations) was removed; two
//! distinct non-universal scopes are simply different, private labels.

use crate::error::Error;
use std::fmt;
use std::hash::{Hash, Hasher};

/// An opaque, normalized scope path label for organizing knowledge.
///
/// Slash characters may appear in the string (e.g. `"work/company-a/backend"`)
/// but carry no hierarchical meaning: scopes are compared by exact identity, with
/// the empty path as the universal scope accessible across all scopes.
///
/// Paths are normalized on construction: trailing slashes are trimmed,
/// consecutive slashes are rejected, and empty segments are rejected.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ScopePath(String);

impl ScopePath {
    /// Create a new scope path from a string.
    ///
    /// Normalizes the path by trimming trailing slashes and validating
    /// the format. Returns an error if the path is invalid.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` if:
    /// - The path is empty after trimming
    /// - The path contains consecutive slashes (`//`)
    /// - The path contains empty segments
    pub fn new(s: impl Into<String>) -> Result<Self, Error> {
        let mut path = s.into();

        // Reject consecutive slashes before normalization can hide malformed
        // trailing empty segments (for example, `a//` must not become `a`).
        if path.contains("//") {
            return Err(Error::InvalidInput(
                "scope path cannot contain consecutive slashes".to_string(),
            ));
        }

        // Trim trailing slashes
        path = path.trim_end_matches('/').to_string();

        // Reject empty path (only universal() can create empty)
        if path.is_empty() {
            return Err(Error::InvalidInput(
                "scope path cannot be empty; use ScopePath::universal() for universal scope"
                    .to_string(),
            ));
        }

        // Reject empty segments (e.g., "/foo" or "foo/")
        for segment in path.split('/') {
            if segment.is_empty() {
                return Err(Error::InvalidInput(
                    "scope path cannot contain empty segments".to_string(),
                ));
            }
        }

        Ok(ScopePath(path))
    }

    /// Create a universal scope path (empty path).
    ///
    /// Universal scope paths apply across all domains and projects.
    pub fn universal() -> Self {
        ScopePath(String::new())
    }

    /// Get the scope path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check if this is a universal scope path.
    pub fn is_universal(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ScopePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            write!(f, "universal")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

impl Hash for ScopePath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Path Shape Tests =====

    #[test]
    fn path_empty_string_rejected() {
        let result = ScopePath::new("");
        assert!(result.is_err());
        match result {
            Err(Error::InvalidInput(msg)) => {
                assert!(msg.contains("cannot be empty"));
            }
            _ => panic!("expected InvalidInput error"),
        }
    }

    #[test]
    fn path_slash_only_rejected() {
        let result = ScopePath::new("/");
        assert!(result.is_err());
    }

    #[test]
    fn path_consecutive_slashes_rejected() {
        let result = ScopePath::new("a//b");
        assert!(result.is_err());
        match result {
            Err(Error::InvalidInput(msg)) => {
                assert!(msg.contains("consecutive slashes"));
            }
            _ => panic!("expected InvalidInput error"),
        }
    }

    #[test]
    fn path_consecutive_trailing_slashes_rejected() {
        let result = ScopePath::new("a//");
        assert!(result.is_err());
        match result {
            Err(Error::InvalidInput(msg)) => {
                assert!(msg.contains("consecutive slashes"));
            }
            _ => panic!("expected InvalidInput error"),
        }
    }

    #[test]
    fn path_trailing_slash_trimmed() {
        let path = ScopePath::new("a/b/").unwrap();
        assert_eq!(path.as_str(), "a/b");
    }

    #[test]
    fn path_simple_segment() {
        let path = ScopePath::new("a").unwrap();
        assert_eq!(path.as_str(), "a");
    }

    #[test]
    fn path_deeply_nested_allowed() {
        let path = ScopePath::new("a/b/c/d/e/f/g/h/i/j").unwrap();
        assert_eq!(path.as_str(), "a/b/c/d/e/f/g/h/i/j");
    }
}
