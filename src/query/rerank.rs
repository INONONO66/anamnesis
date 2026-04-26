//! Goal-weighted reranking for query results.
//!
//! Boosts nodes whose name or content match context keywords,
//! then re-sorts by relevance descending.

use crate::query::assembly::ScoredNode;

/// Rerank scored nodes by boosting those matching context keywords.
///
/// For each node, checks if any whitespace-separated context keyword appears in:
/// - `name` (substring, case-insensitive)
/// - `content` (substring, case-insensitive)
///
/// Matching nodes get +0.2 relevance boost (clamped to 1.0).
/// Results are re-sorted by relevance descending.
///
/// Returns the input unchanged when `context` is empty or whitespace-only.
pub fn rerank_with_context(mut scored_nodes: Vec<ScoredNode>, context: &str) -> Vec<ScoredNode> {
    let keywords: Vec<String> = context
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    if keywords.is_empty() {
        return scored_nodes;
    }

    for node in &mut scored_nodes {
        let name_lower = node.name.to_lowercase();
        let content_lower = node.content.to_lowercase();

        let matches = keywords
            .iter()
            .any(|kw| name_lower.contains(kw.as_str()) || content_lower.contains(kw.as_str()));

        if matches {
            node.relevance = (node.relevance + 0.2).min(1.0);
        }
    }

    scored_nodes.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scored_nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Origin;
    use crate::graph::{KnowledgeType, NodeId};

    fn make_origin() -> Origin {
        Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: None,
            confidence: 0.9,
        }
    }

    fn make_scored(id: u64, name: &str, content: &str, relevance: f64) -> ScoredNode {
        ScoredNode {
            node_id: NodeId(id),
            name: name.to_string(),
            summary: None,
            content: content.to_string(),
            node_type: KnowledgeType::Semantic,
            relevance,
            origin: make_origin(),
        }
    }

    #[test]
    fn empty_context_returns_unchanged() {
        let nodes = vec![
            make_scored(0, "alpha", "content a", 0.5),
            make_scored(1, "beta", "content b", 0.6),
        ];
        let result = rerank_with_context(nodes, "");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].node_id, NodeId(0));
    }

    #[test]
    fn whitespace_only_context_returns_unchanged() {
        let nodes = vec![make_scored(0, "alpha", "content", 0.5)];
        let result = rerank_with_context(nodes, "   ");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].relevance, 0.5);
    }

    #[test]
    fn matching_name_boosts_relevance() {
        let nodes = vec![
            make_scored(0, "auth_handler", "handles requests", 0.5),
            make_scored(1, "db_pool", "manages connections", 0.5),
        ];
        let result = rerank_with_context(nodes, "auth");
        assert_eq!(result[0].node_id, NodeId(0));
        assert!((result[0].relevance - 0.7).abs() < 1e-10);
        assert!((result[1].relevance - 0.5).abs() < 1e-10);
    }

    #[test]
    fn matching_content_boosts_relevance() {
        let nodes = vec![
            make_scored(0, "module_a", "database migration tool", 0.4),
            make_scored(1, "module_b", "http server", 0.4),
        ];
        let result = rerank_with_context(nodes, "database");
        assert_eq!(result[0].node_id, NodeId(0));
        assert!((result[0].relevance - 0.6).abs() < 1e-10);
    }

    #[test]
    fn case_insensitive_matching() {
        let nodes = vec![make_scored(0, "AuthModule", "content", 0.3)];
        let result = rerank_with_context(nodes, "auth");
        assert!((result[0].relevance - 0.5).abs() < 1e-10);
    }

    #[test]
    fn boost_clamped_to_one() {
        let nodes = vec![make_scored(0, "auth", "content", 0.9)];
        let result = rerank_with_context(nodes, "auth");
        assert!((result[0].relevance - 1.0).abs() < 1e-10);
    }

    #[test]
    fn multiple_keywords_any_match_boosts() {
        let nodes = vec![
            make_scored(0, "db_pool", "connection manager", 0.4),
            make_scored(1, "auth", "login handler", 0.4),
            make_scored(2, "logger", "writes logs", 0.4),
        ];
        let result = rerank_with_context(nodes, "auth db");
        // Both auth and db_pool should be boosted, logger should not
        assert!((result[0].relevance - 0.6).abs() < 1e-10);
        assert!((result[1].relevance - 0.6).abs() < 1e-10);
        assert!((result[2].relevance - 0.4).abs() < 1e-10);
    }

    #[test]
    fn rerank_re_sorts_by_relevance_descending() {
        let nodes = vec![
            make_scored(0, "low_node", "irrelevant", 0.3),
            make_scored(1, "auth_node", "handles auth", 0.2),
        ];
        let result = rerank_with_context(nodes, "auth");
        // auth_node gets 0.2 + 0.2 = 0.4, low_node stays 0.3
        assert_eq!(result[0].node_id, NodeId(1));
        assert_eq!(result[1].node_id, NodeId(0));
    }
}
