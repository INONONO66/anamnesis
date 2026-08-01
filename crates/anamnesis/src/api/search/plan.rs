//! Search plan derivation stage.

use crate::api::EngineConfig;
use crate::error::Error;
use crate::query::SearchInput;
use crate::query::types::SearchPlan;

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
    let time_cues = crate::query::temporal::parse_time_cues(&text, input.now.0);

    Ok(SearchPlan {
        text,
        use_text,
        use_vector,
        use_entity,
        use_graph: true,
        use_persona_bias,
        seed_limit,
        time_cues,
    })
}

/// Decompose a natural language query into sub-queries.
///
/// Applies pattern matching to extract meaningful sub-queries:
/// - English: "who is X", "what is X", "how many times did X",
///   "where did X", "what kind of X", "X of Y"
/// - Korean: "X의 Y", "X이/가 누구"
///
/// Returns the original query as a single-element vec if no pattern matches.
pub(crate) fn decompose_query(query: &str) -> Vec<String> {
    let q = query.trim();
    if q.is_empty() {
        return vec![q.to_string()];
    }

    let mut results = Vec::new();

    // English copular questions: "who is/was X" → extract X.
    if let Some(rest) = strip_any_prefix_ci(q, &["who is ", "who was ", "who were "]) {
        let subject = trim_question(rest);
        if !subject.is_empty() {
            results.push(subject.to_string());
        }
    }

    // English copular questions: "what is/was X" → extract X.
    if let Some(rest) = strip_any_prefix_ci(q, &["what is ", "what was ", "what were "]) {
        let subject = trim_question(rest);
        if !subject.is_empty() {
            results.push(subject.to_string());
        }
    }

    // Event/count/location questions carry low-value interrogative and
    // auxiliary tokens. Strip only the leading wrapper; retain the full
    // subject+predicate phrase so lexical search can match the event itself.
    if results.is_empty()
        && let Some(rest) = strip_any_prefix_ci(
            q,
            &[
                "how many times has ",
                "how many times have ",
                "how many times did ",
                "how often did ",
                "how often does ",
                "how often has ",
                "how often have ",
                "how did ",
                "how has ",
                "how have ",
                "where did ",
                "where does ",
                "where has ",
                "where have ",
                "what has ",
                "what have ",
                "what kind of ",
            ],
        )
    {
        let event = trim_question(rest);
        if !event.is_empty() {
            results.push(event.to_string());
        }
    }

    // English: "how does X work" → extract X
    if results.is_empty()
        && let Some(rest) = strip_prefix_ci(q, "how does ")
    {
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

    {
        let targets: Vec<&str> = if results.is_empty() {
            vec![q]
        } else {
            results.iter().map(String::as_str).collect()
        };
        let mut of_candidates: Vec<String> = Vec::new();
        for target in &targets {
            if let Some(idx) = find_word_boundary(target, " of ") {
                let x = target[..idx].trim();
                let y = target[idx + 4..].trim().trim_end_matches('?').trim();
                if !x.is_empty() {
                    of_candidates.push(x.to_string());
                }
                if !y.is_empty() && y != x {
                    of_candidates.push(y.to_string());
                }
            }
        }
        if results.is_empty() {
            results = of_candidates;
        } else {
            for c in of_candidates {
                if !results.contains(&c) {
                    results.push(c);
                }
            }
        }
    }

    // Korean: "X의 Y" → extract X and Y
    if results.is_empty()
        && let Some(idx) = q.find("의 ")
    {
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

/// Preserve the user's complete wording and add deterministic decompositions
/// only as auxiliary lexical-recall channels.
pub(crate) fn query_variants(query: &str) -> Vec<String> {
    let original = query.trim().to_string();
    let decomposed = decompose_query(query);
    let entity_anchors = proper_noun_anchors(query);
    let mut variants = Vec::with_capacity(
        decomposed
            .len()
            .saturating_add(entity_anchors.len())
            .saturating_add(1),
    );
    variants.push(original);
    for candidate in decomposed.into_iter().chain(entity_anchors) {
        if !variants
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&candidate))
        {
            variants.push(candidate);
        }
    }
    variants
}

/// Build the latency-bounded dense lanes used only by complex `Memory` recall.
///
/// Unlike ordinary lexical variants, these retain entity-only lanes for
/// inference questions. Those lanes recover premises about each participant
/// before the local reranker sees the union. The original query stays first
/// and the complete surface is capped at three embeddings.
pub(crate) fn complex_dense_query_variants(query: &str) -> Vec<String> {
    const MAX_DENSE_VARIANTS: usize = 3;

    let original = query.trim().to_owned();
    let mut variants = Vec::with_capacity(MAX_DENSE_VARIANTS);
    variants.push(original);
    for candidate in proper_noun_anchors_with_inference(query, true)
        .into_iter()
        .chain(decompose_query(query))
    {
        if variants.len() >= MAX_DENSE_VARIANTS {
            break;
        }
        if !variants
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&candidate))
        {
            variants.push(candidate);
        }
    }
    variants
}

fn proper_noun_anchors(query: &str) -> Vec<String> {
    proper_noun_anchors_with_inference(query, false)
}

fn proper_noun_anchors_with_inference(query: &str, include_inference: bool) -> Vec<String> {
    const QUESTION_WORDS: &[&str] = &[
        "Am", "Are", "Can", "Could", "Did", "Do", "Does", "Has", "Have", "How", "Is", "List",
        "May", "Might", "Should", "Was", "Were", "What", "When", "Where", "Which", "Who", "Why",
        "Will", "Would",
    ];
    const MAX_ANCHORS: usize = 4;

    let normalized = query.trim().to_lowercase();
    let words: Vec<_> = normalized
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty())
        .collect();
    let starts_inference = words.first().is_some_and(|word| {
        matches!(
            *word,
            "am" | "are"
                | "can"
                | "could"
                | "did"
                | "do"
                | "does"
                | "has"
                | "have"
                | "is"
                | "might"
                | "should"
                | "was"
                | "were"
                | "will"
                | "would"
        )
    });
    let has_inference_cue = words.iter().any(|word| {
        matches!(
            *word,
            "could" | "infer" | "imply" | "likely" | "might" | "suggest" | "would"
        )
    }) || normalized.starts_with("based on ")
        || normalized.starts_with("considering ");
    if !include_inference && (starts_inference || has_inference_cue) {
        return Vec::new();
    }

    let mut anchors = Vec::new();
    let mut phrase = Vec::new();
    let flush_phrase = |phrase: &mut Vec<String>, anchors: &mut Vec<String>| {
        if phrase.is_empty() || anchors.len() >= MAX_ANCHORS {
            phrase.clear();
            return;
        }
        let candidate = phrase.join(" ");
        if !anchors
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&candidate))
        {
            anchors.push(candidate);
        }
        phrase.clear();
    };

    for raw_token in query.split_whitespace() {
        let token = raw_token
            .trim_matches(|character: char| !character.is_alphanumeric() && character != '\'');
        let token = token.strip_suffix("'s").unwrap_or(token);
        let is_anchor = token.chars().next().is_some_and(char::is_uppercase)
            && token.chars().any(char::is_alphabetic)
            && !QUESTION_WORDS
                .iter()
                .any(|word| word.eq_ignore_ascii_case(token));
        if is_anchor {
            phrase.push(token.to_owned());
        } else {
            flush_phrase(&mut phrase, &mut anchors);
        }
    }
    flush_phrase(&mut phrase, &mut anchors);
    anchors
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

fn strip_any_prefix_ci<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes
        .iter()
        .find_map(|prefix| strip_prefix_ci(s, prefix))
}

fn trim_question(value: &str) -> &str {
    value.trim_end_matches(['?', '.', '!']).trim()
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
    fn past_copular_question_extracts_subject() {
        let r = decompose_query("What was Alice's first job?");
        assert_eq!(r, vec!["Alice's first job"]);
    }

    #[test]
    fn count_question_extracts_event_phrase() {
        let r = decompose_query("How many times has John injured his ankle?");
        assert_eq!(r, vec!["John injured his ankle"]);
    }

    #[test]
    fn location_question_extracts_event_phrase() {
        let r = decompose_query("Where did Dana buy her camera?");
        assert_eq!(r, vec!["Dana buy her camera"]);
    }

    #[test]
    fn what_kind_question_extracts_subject_phrase() {
        let r = decompose_query("What kind of pets does Alice have?");
        assert_eq!(r, vec!["pets does Alice have"]);
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
    fn query_variants_keep_original_before_auxiliary_decompositions() {
        let variants = query_variants("How many times has John injured his ankle?");
        assert_eq!(
            variants,
            vec![
                "How many times has John injured his ankle?",
                "John injured his ankle",
                "John"
            ]
        );
        assert_eq!(query_variants("foo bar baz"), vec!["foo bar baz"]);
    }

    #[test]
    fn query_variants_add_entity_anchors_for_bridge_recall() {
        assert_eq!(
            query_variants("Which countries did Aria visit with Blake?"),
            vec![
                "Which countries did Aria visit with Blake?",
                "Aria",
                "Blake"
            ]
        );
        assert_eq!(
            query_variants("What advice could Rowan and Taylor share?"),
            vec!["What advice could Rowan and Taylor share?"]
        );
        assert_eq!(
            query_variants("How did Nora promote her clothes store?"),
            vec![
                "How did Nora promote her clothes store?",
                "Nora promote her clothes store",
                "Nora"
            ]
        );
        assert_eq!(
            query_variants("How often does Quinn get health checkups?"),
            vec![
                "How often does Quinn get health checkups?",
                "Quinn get health checkups",
                "Quinn"
            ]
        );
    }

    #[test]
    fn complex_dense_variants_recover_inference_entity_lanes() {
        assert_eq!(
            complex_dense_query_variants("What advice could Rowan and Taylor share?"),
            vec![
                "What advice could Rowan and Taylor share?",
                "Rowan",
                "Taylor"
            ]
        );
        assert_eq!(
            complex_dense_query_variants("Which countries did Aria visit with Blake?"),
            vec![
                "Which countries did Aria visit with Blake?",
                "Aria",
                "Blake"
            ]
        );
    }

    #[test]
    fn complex_dense_variants_are_bounded_and_deterministic() {
        let query = "How did Alice and Bob compare Carol with Dana?";
        let first = complex_dense_query_variants(query);
        let second = complex_dense_query_variants(query);

        assert_eq!(first, second);
        assert_eq!(first.first().map(String::as_str), Some(query));
        assert_eq!(first.get(1).map(String::as_str), Some("Alice"));
        assert_eq!(first.get(2).map(String::as_str), Some("Bob"));
        assert!(
            first.len() <= 3,
            "dense expansion must stay latency-bounded"
        );
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
