//! Search plan derivation stage.

use crate::api::EngineConfig;
use crate::error::Error;
use crate::query::types::SearchPlan;
use crate::query::{PackagingMode, SearchInput};

/// Derive a `SearchPlan` from a `SearchInput`, normalising the query text and
/// rejecting inputs that have neither a non-empty trimmed text nor an embedding.
///
/// The default seed limit is 3 when `SearchInput.seed_limit` is `None`.
pub(crate) fn derive_search_plan(
    input: &SearchInput,
    _config: &EngineConfig,
) -> Result<SearchPlan, Error> {
    let text = input.text.trim().to_string();

    if text.is_empty() && input.query_embedding.is_none() {
        return Err(Error::InvalidInput(
            "search input requires non-empty text or query_embedding".to_string(),
        ));
    }

    let use_text = !text.is_empty();
    let use_vector = input.query_embedding.is_some();
    let use_entity = !input.entity_tags.is_empty();
    let use_persona_bias = input.agent_id.is_some();
    let seed_limit = input.seed_limit.unwrap_or(3);

    Ok(SearchPlan {
        text,
        use_text,
        use_vector,
        use_entity,
        use_graph: true,
        use_persona_bias,
        seed_limit,
        packaging_mode: PackagingMode::KnowledgeOnly,
    })
}

/// Decompose a natural language query into sub-queries.
///
/// Applies pattern matching to extract meaningful sub-queries:
/// - English: "who is X", "what is X", "X of Y"
/// - Korean: "X의 Y", "X이/가 누구"
///
/// Returns the original query as a single-element vec if no pattern matches.
pub(crate) fn decompose_query(query: &str) -> Vec<String> {
    let q = query.trim();
    if q.is_empty() {
        return vec![q.to_string()];
    }

    let mut results = Vec::new();

    // English: "who is X" → extract X
    if let Some(rest) = strip_prefix_ci(q, "who is ") {
        let subject = rest.trim_end_matches('?').trim();
        if !subject.is_empty() {
            results.push(subject.to_string());
        }
    }

    // English: "what is X" → extract X
    if let Some(rest) = strip_prefix_ci(q, "what is ") {
        let subject = rest.trim_end_matches('?').trim();
        if !subject.is_empty() {
            results.push(subject.to_string());
        }
    }

    // English: "how does X work" → extract X
    if results.is_empty() {
        if let Some(rest) = strip_prefix_ci(q, "how does ") {
            let subject = rest
                .trim_end_matches('?')
                .trim()
                .strip_suffix(" work")
                .unwrap_or(rest.trim_end_matches('?').trim())
                .trim();
            if !subject.is_empty() {
                results.push(subject.to_string());
            }
        }
    }

    // English: "X of Y" → extract X and Y
    if results.is_empty() {
        if let Some(idx) = find_word_boundary(q, " of ") {
            let x = q[..idx].trim();
            let y = q[idx + 4..].trim().trim_end_matches('?').trim();
            if !x.is_empty() {
                results.push(x.to_string());
            }
            if !y.is_empty() && y != x {
                results.push(y.to_string());
            }
        }
    }

    // Korean: "X의 Y" → extract X and Y
    if results.is_empty() {
        if let Some(idx) = q.find("의 ") {
            let x = q[..idx].trim();
            let rest = &q[idx + "의 ".len()..];
            let y = strip_korean_suffixes(rest.trim_end_matches('?').trim());
            if !x.is_empty() {
                results.push(x.to_string());
            }
            if !y.is_empty() && y != x {
                results.push(y.to_string());
            }
        }
    }

    // Korean: "X이/가/은/는 누구" → extract X
    if results.is_empty() {
        let who_patterns = ["이 누구", "가 누구", "은 누구", "는 누구"];
        for pat in &who_patterns {
            if let Some(idx) = q.find(pat) {
                let subject = q[..idx].trim();
                if !subject.is_empty() {
                    results.push(subject.to_string());
                    break;
                }
            }
        }
    }

    // Korean: "X이/가/은/는 뭐" → extract X
    if results.is_empty() {
        let what_patterns = ["이 뭐", "가 뭐", "은 뭐", "는 뭐"];
        for pat in &what_patterns {
            if let Some(idx) = q.find(pat) {
                let subject = q[..idx].trim();
                if !subject.is_empty() {
                    results.push(subject.to_string());
                    break;
                }
            }
        }
    }

    if results.is_empty() {
        vec![q.to_string()]
    } else {
        results
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let sb = s.as_bytes();
    let pb = prefix.as_bytes();
    if sb.len() >= pb.len() && sb[..pb.len()].eq_ignore_ascii_case(pb) {
        Some(&s[pb.len()..])
    } else {
        None
    }
}

fn find_word_boundary(s: &str, pat: &str) -> Option<usize> {
    s.find(pat)
}

fn strip_korean_suffixes(s: &str) -> &str {
    let suffixes = ["는", "은", "가", "이", "를", "을"];
    for suffix in &suffixes {
        if let Some(stripped) = s.strip_suffix(suffix) {
            return stripped.trim();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_is_extracts_subject() {
        let r = decompose_query("who is Alice?");
        assert_eq!(r, vec!["Alice"]);
    }

    #[test]
    fn what_is_extracts_subject() {
        let r = decompose_query("what is the factory pattern?");
        assert_eq!(r, vec!["the factory pattern"]);
    }

    #[test]
    fn of_pattern_extracts_both() {
        let r = decompose_query("CEO of Hashed");
        assert_eq!(r, vec!["CEO", "Hashed"]);
    }

    #[test]
    fn korean_eui_extracts_both() {
        let r = decompose_query("Hashed의 CEO는");
        assert_eq!(r, vec!["Hashed", "CEO"]);
    }

    #[test]
    fn korean_nugu_extracts_subject() {
        let r = decompose_query("Alice가 누구야?");
        assert_eq!(r, vec!["Alice"]);
    }

    #[test]
    fn korean_mwo_extracts_subject() {
        let r = decompose_query("팩토리 패턴은 뭐야?");
        assert_eq!(r, vec!["팩토리 패턴"]);
    }

    #[test]
    fn how_does_extracts_subject() {
        let r = decompose_query("how does spreading activation work?");
        assert_eq!(r, vec!["spreading activation"]);
    }

    #[test]
    fn no_match_returns_original() {
        let r = decompose_query("foo bar baz");
        assert_eq!(r, vec!["foo bar baz"]);
    }

    #[test]
    fn empty_input() {
        let r = decompose_query("");
        assert_eq!(r, vec![""]);
    }

    #[test]
    fn whitespace_trimmed() {
        let r = decompose_query("  who is Bob?  ");
        assert_eq!(r, vec!["Bob"]);
    }
}
