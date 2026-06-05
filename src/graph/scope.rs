//! Hierarchical scope path representation for knowledge scoping.
//!
//! Scope paths are slash-delimited hierarchies that organize knowledge
//! into domains, projects, and features. They enable fine-grained access
//! control and relevance weighting in multi-domain knowledge graphs.

use crate::error::Error;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A hierarchical scope path for organizing knowledge.
///
/// Scope paths are slash-delimited strings representing a hierarchy
/// (e.g., `"work/company-a/backend-platform"`). The empty path represents
/// universal knowledge accessible across all scopes.
///
/// Paths are normalized on construction: trailing slashes are trimmed,
/// consecutive slashes are rejected, and empty segments are rejected.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ScopePath(String);

/// Relationship between two scope paths in the hierarchy.
///
/// Used to determine relevance weighting and access control
/// when querying knowledge across different scopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ScopeRelation {
    /// Paths are identical.
    Equal,
    /// Self is an ancestor of other (self is a prefix of other).
    Ancestor,
    /// Self is a descendant of other (other is a prefix of self).
    Descendant,
    /// Self and other share a common ancestor but diverge.
    Sibling,
    /// One path is universal (empty).
    Universal,
    /// Paths have no hierarchical relationship.
    Disjoint,
}

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

    /// Determine the relationship between this path and another.
    ///
    /// Returns the hierarchical relationship (Equal, Ancestor, Descendant,
    /// Sibling, Universal, or Disjoint) between the two paths.
    pub fn relation_to(&self, other: &Self) -> ScopeRelation {
        // Both empty → Equal
        if self.0.is_empty() && other.0.is_empty() {
            return ScopeRelation::Equal;
        }

        // One empty → Universal
        if self.0.is_empty() || other.0.is_empty() {
            return ScopeRelation::Universal;
        }

        // Exact match
        if self.0 == other.0 {
            return ScopeRelation::Equal;
        }

        // Check ancestor: self is prefix of other
        if other.0.starts_with(&format!("{}/", self.0)) {
            return ScopeRelation::Ancestor;
        }

        // Check descendant: other is prefix of self
        if self.0.starts_with(&format!("{}/", other.0)) {
            return ScopeRelation::Descendant;
        }

        // Check sibling: same parent, different last segment
        let self_parts: Vec<&str> = self.0.split('/').collect();
        let other_parts: Vec<&str> = other.0.split('/').collect();

        if self_parts.len() == other_parts.len() && self_parts.len() > 1 {
            // Compare all but last segment
            if self_parts[..self_parts.len() - 1] == other_parts[..other_parts.len() - 1] {
                return ScopeRelation::Sibling;
            }
        }

        // No relationship
        ScopeRelation::Disjoint
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

    // ===== Relation Tests (6 cases) =====

    #[test]
    fn relation_equal_same_path() {
        let a = ScopePath::new("personal/foo").unwrap();
        let b = ScopePath::new("personal/foo").unwrap();
        assert_eq!(a.relation_to(&b), ScopeRelation::Equal);
    }

    #[test]
    fn relation_ancestor() {
        let parent = ScopePath::new("personal").unwrap();
        let child = ScopePath::new("personal/foo").unwrap();
        assert_eq!(parent.relation_to(&child), ScopeRelation::Ancestor);
    }

    #[test]
    fn relation_descendant() {
        let child = ScopePath::new("personal/foo").unwrap();
        let parent = ScopePath::new("personal").unwrap();
        assert_eq!(child.relation_to(&parent), ScopeRelation::Descendant);
    }

    #[test]
    fn relation_sibling() {
        let a = ScopePath::new("personal/foo").unwrap();
        let b = ScopePath::new("personal/bar").unwrap();
        assert_eq!(a.relation_to(&b), ScopeRelation::Sibling);
    }

    #[test]
    fn relation_universal() {
        let universal = ScopePath::universal();
        let specific = ScopePath::new("personal").unwrap();
        assert_eq!(universal.relation_to(&specific), ScopeRelation::Universal);
        assert_eq!(specific.relation_to(&universal), ScopeRelation::Universal);
    }

    #[test]
    fn relation_disjoint() {
        let a = ScopePath::new("work").unwrap();
        let b = ScopePath::new("personal").unwrap();
        assert_eq!(a.relation_to(&b), ScopeRelation::Disjoint);
    }

    // ===== Path Shape Tests (6 cases) =====

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
