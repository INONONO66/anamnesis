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

    // Context-setting clauses before a punctuation boundary are useful lexical
    // surfaces in their own right. The locale rules below recognise a small,
    // general set of leading markers rather than one question template.
    if let Some(premise) = leading_context_clause(q) {
        results.push(premise);
    }

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

/// A latency-bounded dense-query plan for complex `Memory` recall.
pub(crate) struct ComplexDenseQueryPlan {
    /// All surfaces embedded together in one provider batch.
    pub variants: Vec<String>,
    /// Surfaces participating in graph seed collection.
    pub engine_variant_indices: Vec<usize>,
    /// Surfaces participating in isolated atomic-fact routing.
    pub atomic_variant_indices: Vec<usize>,
}

/// Build the latency-bounded dense lanes used only by complex `Memory` recall.
///
/// The original query stays first. Deterministic relation-bearing surfaces
/// come next and are shared with atomic-fact routing. A final entity-only lane
/// may seed graph recall, but never routes atomic facts by itself. The complete
/// surface remains capped at four embeddings and one provider batch.
pub(crate) fn complex_dense_query_plan(query: &str) -> ComplexDenseQueryPlan {
    const MAX_DENSE_VARIANTS: usize = 4;

    let original = query.trim().to_owned();
    let mut variants = Vec::with_capacity(MAX_DENSE_VARIANTS);
    variants.push(original);
    let entity_anchors = proper_noun_anchors_with_inference(query, true);
    let semantic_variant_limit =
        MAX_DENSE_VARIANTS.saturating_sub(usize::from(!entity_anchors.is_empty()));
    for candidate in relation_clause_dense_surfaces(query)
        .into_iter()
        .chain(decompose_query(query))
        .chain(relation_bearing_dense_surface(query))
        .chain(predicate_dense_surface(query))
    {
        if variants.len() >= semantic_variant_limit {
            break;
        }
        if !variants
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&candidate))
        {
            variants.push(candidate);
        }
    }
    let atomic_variant_count = variants.len();
    for candidate in entity_anchors {
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
    ComplexDenseQueryPlan {
        engine_variant_indices: (0..variants.len()).collect(),
        atomic_variant_indices: (0..atomic_variant_count).collect(),
        variants,
    }
}

/// Build short relation-bearing facets without asking another model to
/// rewrite the query. Compound questions often describe independent
/// constraints on the same operation. Embedding the clauses separately lets
/// either constraint recover its own raw source while the complete query
/// remains the primary lane.
fn relation_clause_dense_surfaces(query: &str) -> Vec<String> {
    let mut surfaces: Vec<String> = Vec::new();

    let anchors = proper_noun_anchors_with_inference(query, true);
    let carried_anchor = (anchors.len() == 1).then(|| anchors[0].as_str());
    for (index, raw_clause) in clause_facets(query).into_iter().enumerate() {
        let mut clause = relation_bearing_dense_surface(&raw_clause)
            .unwrap_or_else(|| raw_clause.trim().to_owned());
        if index > 0
            && let Some(anchor) = carried_anchor
            && !contains_normalized_phrase(&clause, anchor)
        {
            clause = format!("{anchor} {clause}");
        }
        if !surfaces
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&clause))
        {
            surfaces.push(clause);
        }
    }
    surfaces
}

/// Rules for deterministic clause faceting in one locale.
///
/// Strong punctuation always forms a potential boundary. Connector splitting
/// intentionally excludes ambiguous nominal coordinators such as `and` and
/// `or`; those commonly join entity names or list items rather than clauses.
struct ClauseFacetRules {
    punctuation: &'static [char],
    connectors: &'static [&'static str],
    leading_markers: &'static [&'static str],
}

const ENGLISH_CLAUSE_FACET_RULES: ClauseFacetRules = ClauseFacetRules {
    punctuation: &[',', ';', ':'],
    connectors: &[
        "although", "because", "but", "if", "though", "unless", "when", "whereas", "while", "yet",
    ],
    leading_markers: &[
        "according to",
        "although",
        "based on",
        "because",
        "considering",
        "given",
        "if",
        "in light of",
        "though",
        "unless",
        "when",
        "whereas",
        "while",
    ],
};

/// Split a query into bounded, query-derived clause facets.
///
/// This is deliberately a syntax-light fallback: it recognises only explicit
/// punctuation and a conservative set of English clause connectors. A split
/// is emitted only when at least two substantive facets survive, so ordinary
/// noun phrases and comma-separated labels remain intact.
fn clause_facets(query: &str) -> Vec<String> {
    const MAX_CLAUSE_FACETS: usize = 4;

    let mut raw_facets = Vec::new();
    let mut saw_boundary = false;
    for punctuation_part in query
        .trim()
        .split(|character| ENGLISH_CLAUSE_FACET_RULES.punctuation.contains(&character))
    {
        let connector_parts = split_clause_connectors(punctuation_part);
        saw_boundary |= connector_parts.len() > 1;
        raw_facets.extend(connector_parts);
    }
    saw_boundary |= raw_facets.len() > 1;
    if !saw_boundary {
        return Vec::new();
    }

    let mut facets = Vec::with_capacity(MAX_CLAUSE_FACETS);
    for raw_facet in raw_facets {
        let (facet, _) = strip_leading_clause_marker(&raw_facet);
        let facet = trim_question(facet);
        if substantive_term_count(facet) < 2
            || facets
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(facet))
        {
            continue;
        }
        facets.push(facet.to_owned());
        if facets.len() >= MAX_CLAUSE_FACETS {
            break;
        }
    }

    if facets.len() > 1 { facets } else { Vec::new() }
}

fn split_clause_connectors(clause: &str) -> Vec<String> {
    let tokens: Vec<_> = clause.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut parts = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        let normalized = normalized_dense_words(token);
        let is_connector = ENGLISH_CLAUSE_FACET_RULES
            .connectors
            .iter()
            .any(|connector| normalized.eq_ignore_ascii_case(connector));
        if is_connector && index > start && index + 1 < tokens.len() {
            parts.push(tokens[start..index].join(" "));
            start = index + 1;
        }
    }
    parts.push(tokens[start..].join(" "));
    parts
}

fn leading_context_clause(query: &str) -> Option<String> {
    let boundary = query.char_indices().find_map(|(index, character)| {
        ENGLISH_CLAUSE_FACET_RULES
            .punctuation
            .contains(&character)
            .then_some(index)
    })?;
    let (clause, stripped_marker) = strip_leading_clause_marker(&query[..boundary]);
    let clause = trim_question(clause);
    (stripped_marker && substantive_term_count(clause) >= 2).then(|| clause.to_owned())
}

fn strip_leading_clause_marker(value: &str) -> (&str, bool) {
    let value = value.trim();
    matching_leading_clause_marker(value)
        .and_then(|marker| strip_prefix_ci(value, marker).map(|rest| (rest.trim_start(), true)))
        .unwrap_or((value, false))
}

fn matching_leading_clause_marker(value: &str) -> Option<&'static str> {
    let value = value.trim();
    ENGLISH_CLAUSE_FACET_RULES
        .leading_markers
        .iter()
        .find_map(|marker| {
            strip_prefix_ci(value, marker).and_then(|rest| {
                rest.chars()
                    .next()
                    .is_none_or(|character| !character.is_alphanumeric())
                    .then_some(*marker)
            })
        })
}

fn substantive_term_count(value: &str) -> usize {
    value
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|term| !term.is_empty())
        .take(2)
        .count()
}

fn contains_normalized_phrase(value: &str, phrase: &str) -> bool {
    normalized_dense_words(value).contains(&normalized_dense_words(phrase))
}

/// Remove named anchors and low-information possessives from one auxiliary
/// surface. The primary embedding still contains the complete question; this
/// facet prevents a person's frequently repeated name from overwhelming the
/// requested action or object in a dense atomic-fact lane.
fn predicate_dense_surface(query: &str) -> Option<String> {
    const LOW_INFORMATION_TERMS: &[&str] = &[
        "a", "an", "he", "her", "hers", "his", "its", "my", "our", "ours", "she", "that", "the",
        "their", "theirs", "they", "your", "yours",
    ];

    let relation_surface = relation_bearing_dense_surface(query)?;
    let anchors = proper_noun_anchors_with_inference(query, true);
    if anchors.len() != 1 {
        return None;
    }
    let anchor_terms: std::collections::HashSet<_> = anchors
        .into_iter()
        .flat_map(|anchor| {
            anchor
                .split_whitespace()
                .map(normalized_dense_words)
                .collect::<Vec<_>>()
        })
        .collect();
    let terms: Vec<_> = relation_surface
        .split_whitespace()
        .filter(|term| {
            let normalized = normalized_dense_words(term);
            !normalized.is_empty()
                && !anchor_terms.contains(&normalized)
                && !LOW_INFORMATION_TERMS.contains(&normalized.as_str())
        })
        .collect();
    if terms.len() < 2 {
        return None;
    }
    let surface = terms.join(" ");
    (!surface.eq_ignore_ascii_case(&relation_surface)).then_some(surface)
}

fn normalized_dense_words(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Preserve bounded entity seeding for completeness-sensitive collections.
/// Entity-only vectors can seed graph recall, while atomic facts
/// remain routed by the complete query and bounded relation/predicate surfaces.
pub(crate) fn conservative_complex_dense_query_plan(query: &str) -> ComplexDenseQueryPlan {
    const MAX_DENSE_VARIANTS: usize = 5;

    let original = query.trim().to_owned();
    let mut variants = Vec::with_capacity(MAX_DENSE_VARIANTS);
    variants.push(original);
    let mut atomic_surfaces = Vec::new();
    for surface in relation_bearing_dense_surface(query)
        .into_iter()
        .chain(predicate_dense_surface(query))
    {
        if !atomic_surfaces
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&surface))
        {
            atomic_surfaces.push(surface);
        }
    }
    let graph_variant_limit = MAX_DENSE_VARIANTS.saturating_sub(atomic_surfaces.len());
    for candidate in proper_noun_anchors_with_inference(query, true)
        .into_iter()
        .chain(decompose_query(query))
    {
        if variants.len() >= graph_variant_limit {
            break;
        }
        if !variants
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&candidate))
        {
            variants.push(candidate);
        }
    }
    let engine_variant_indices: Vec<_> = (0..variants.len()).collect();
    let mut atomic_variant_indices = vec![0];
    for surface in atomic_surfaces {
        let index = variants
            .iter()
            .position(|existing| existing.eq_ignore_ascii_case(&surface))
            .unwrap_or_else(|| {
                variants.push(surface);
                variants.len() - 1
            });
        if index != 0 && !atomic_variant_indices.contains(&index) {
            atomic_variant_indices.push(index);
        }
    }
    ComplexDenseQueryPlan {
        variants,
        engine_variant_indices,
        atomic_variant_indices,
    }
}

#[cfg(test)]
fn complex_dense_query_variants(query: &str) -> Vec<String> {
    complex_dense_query_plan(query).variants
}

fn relation_bearing_dense_surface(query: &str) -> Option<String> {
    const QUERY_WRAPPERS: &[&str] = &[
        "am", "are", "can", "could", "did", "do", "does", "has", "have", "had", "how", "is", "may",
        "might", "should", "was", "were", "what", "when", "where", "which", "who", "why", "will",
        "would",
    ];

    let terms: Vec<_> = query
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|term| !term.is_empty())
        .filter(|term| {
            !QUERY_WRAPPERS
                .iter()
                .any(|wrapper| term.eq_ignore_ascii_case(wrapper))
        })
        .collect();
    if terms.len() < 2 {
        return None;
    }
    let surface = terms.join(" ");
    (!surface.eq_ignore_ascii_case(query.trim())).then_some(surface)
}

fn proper_noun_anchors(query: &str) -> Vec<String> {
    proper_noun_anchors_with_inference(query, false)
}

fn proper_noun_anchors_with_inference(query: &str, include_inference: bool) -> Vec<String> {
    const QUESTION_WORDS: &[&str] = &[
        "Am",
        "Are",
        "Based",
        "Can",
        "Considering",
        "Could",
        "Did",
        "Do",
        "Does",
        "Has",
        "Have",
        "How",
        "Is",
        "List",
        "May",
        "Might",
        "Should",
        "Was",
        "Were",
        "What",
        "When",
        "Where",
        "Which",
        "Who",
        "Why",
        "Will",
        "Would",
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
    }) || strip_leading_clause_marker(query).1;
    if !include_inference && (starts_inference || has_inference_cue) {
        return Vec::new();
    }

    let mut anchors = Vec::new();
    let mut phrase = Vec::new();
    let leading_marker_term_count = matching_leading_clause_marker(query)
        .map(|marker| marker.split_whitespace().count())
        .unwrap_or(0);
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

    for (token_index, raw_token) in query.split_whitespace().enumerate() {
        let token = raw_token
            .trim_matches(|character: char| !character.is_alphanumeric() && character != '\'');
        let token = token.strip_suffix("'s").unwrap_or(token);
        let is_anchor = token_index >= leading_marker_term_count
            && token.chars().next().is_some_and(char::is_uppercase)
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
    fn comma_context_and_consequence_become_query_derived_facets() {
        let query =
            "Based on Nimbus deployment logs, which retry policy would reduce queue failures?";

        assert_eq!(decompose_query(query), vec!["Nimbus deployment logs"]);
        assert_eq!(
            clause_facets(query),
            vec![
                "Nimbus deployment logs",
                "which retry policy would reduce queue failures"
            ]
        );
        assert_eq!(
            complex_dense_query_variants(query),
            vec![
                query,
                "Nimbus deployment logs",
                "Nimbus retry policy reduce queue failures",
                "Nimbus"
            ]
        );
    }

    #[test]
    fn no_match_returns_original() {
        let r = decompose_query("foo bar baz");
        assert_eq!(r, vec!["foo bar baz"]);
    }

    #[test]
    fn query_variants_keep_original_before_auxiliary_decompositions() {
        let variants = query_variants("How many times has Nimbus retried its deployment?");
        assert_eq!(
            variants,
            vec![
                "How many times has Nimbus retried its deployment?",
                "Nimbus retried its deployment",
                "Nimbus"
            ]
        );
        assert_eq!(query_variants("foo bar baz"), vec!["foo bar baz"]);
    }

    #[test]
    fn query_variants_add_entity_anchors_for_bridge_recall() {
        assert_eq!(
            query_variants("Which regions did Nimbus deploy with Atlas?"),
            vec![
                "Which regions did Nimbus deploy with Atlas?",
                "Nimbus",
                "Atlas"
            ]
        );
        assert_eq!(
            query_variants("What deployment advice could Nimbus and Atlas share?"),
            vec!["What deployment advice could Nimbus and Atlas share?"]
        );
        assert_eq!(
            query_variants("How did Nimbus publish its release candidate?"),
            vec![
                "How did Nimbus publish its release candidate?",
                "Nimbus publish its release candidate",
                "Nimbus"
            ]
        );
        assert_eq!(
            query_variants("How often does Nimbus run health checks?"),
            vec![
                "How often does Nimbus run health checks?",
                "Nimbus run health checks",
                "Nimbus"
            ]
        );
    }

    #[test]
    fn complex_dense_variants_recover_inference_entity_lanes() {
        assert_eq!(
            complex_dense_query_variants("What deployment advice could Nimbus and Atlas share?"),
            vec![
                "What deployment advice could Nimbus and Atlas share?",
                "deployment advice Nimbus and Atlas share",
                "Nimbus",
                "Atlas"
            ]
        );
        assert_eq!(
            complex_dense_query_variants("Which regions did Nimbus deploy with Atlas?"),
            vec![
                "Which regions did Nimbus deploy with Atlas?",
                "regions Nimbus deploy with Atlas",
                "Nimbus",
                "Atlas"
            ]
        );
    }

    #[test]
    fn atomic_dense_prefix_excludes_entity_only_fallbacks() {
        let plan = complex_dense_query_plan("What skills has Nimbus helped teams learn?");

        assert_eq!(
            plan.variants,
            vec![
                "What skills has Nimbus helped teams learn?",
                "skills Nimbus helped teams learn",
                "skills helped teams learn",
                "Nimbus"
            ]
        );
        assert_eq!(plan.engine_variant_indices, vec![0, 1, 2, 3]);
        assert_eq!(plan.atomic_variant_indices, vec![0, 1, 2]);
    }

    #[test]
    fn connective_clauses_carry_the_single_anchor_into_an_elided_clause() {
        let query = "Which rollout should Nimbus pause while error rates are elevated?";
        let plan = complex_dense_query_plan(query);

        assert_eq!(
            relation_clause_dense_surfaces(query),
            vec!["rollout Nimbus pause", "Nimbus error rates elevated"]
        );
        assert_eq!(
            plan.variants,
            vec![
                query,
                "rollout Nimbus pause",
                "Nimbus error rates elevated",
                "Nimbus"
            ]
        );
        assert_eq!(plan.engine_variant_indices, vec![0, 1, 2, 3]);
        assert_eq!(plan.atomic_variant_indices, vec![0, 1, 2]);
    }

    #[test]
    fn clause_facets_do_not_split_a_simple_product_query() {
        let query = "Which retry policy should Nimbus use?";

        assert!(clause_facets(query).is_empty());
        assert!(relation_clause_dense_surfaces(query).is_empty());
    }

    #[test]
    fn clause_facets_are_stably_deduplicated_and_capped() {
        let query = "Given Nimbus logs, Nimbus pauses because queues stall; Nimbus pauses because queues stall, operators retry safely?";
        let first = clause_facets(query);
        let second = clause_facets(query);

        assert_eq!(first, second);
        assert!(first.len() <= 4);
        assert_eq!(first.first().map(String::as_str), Some("Nimbus logs"));
        for (index, facet) in first.iter().enumerate() {
            assert!(
                first[..index]
                    .iter()
                    .all(|existing| !existing.eq_ignore_ascii_case(facet)),
                "clause facets must be deduplicated stably"
            );
        }

        let variants = complex_dense_query_variants(query);
        assert_eq!(variants.first().map(String::as_str), Some(query));
        assert!(variants.len() <= 4, "dense variants must remain capped");
    }

    #[test]
    fn conservative_collection_plan_keeps_entity_seeds_out_of_atomic_routing() {
        let plan = conservative_complex_dense_query_plan(
            "What operational incidents did Nimbus experience?",
        );

        assert_eq!(
            plan.variants,
            vec![
                "What operational incidents did Nimbus experience?",
                "Nimbus",
                "operational incidents Nimbus experience",
                "operational incidents experience"
            ]
        );
        assert_eq!(plan.engine_variant_indices, vec![0, 1]);
        assert_eq!(plan.atomic_variant_indices, vec![0, 2, 3]);
    }

    #[test]
    fn conservative_plan_uses_only_query_derived_surfaces() {
        let query = "What kind of release channels does Nimbus maintain?";
        let plan = conservative_complex_dense_query_plan(query);

        assert_eq!(
            plan.variants,
            vec![
                query,
                "Nimbus",
                "release channels does Nimbus maintain",
                "kind of release channels Nimbus maintain",
                "kind of release channels maintain",
            ]
        );
        assert_eq!(plan.engine_variant_indices, vec![0, 1, 2]);
        assert_eq!(plan.atomic_variant_indices, vec![0, 3, 4]);
    }

    #[test]
    fn conservative_plan_preserves_two_entity_seeds_and_a_bounded_atomic_lane() {
        let plan =
            conservative_complex_dense_query_plan("Which regions did Nimbus deploy with Atlas?");

        assert_eq!(
            plan.variants,
            vec![
                "Which regions did Nimbus deploy with Atlas?",
                "Nimbus",
                "Atlas",
                "regions Nimbus deploy with Atlas"
            ]
        );
        assert_eq!(plan.engine_variant_indices, vec![0, 1, 2]);
        assert_eq!(plan.atomic_variant_indices, vec![0, 3]);
        assert_eq!(plan.variants.len(), 4);
    }

    #[test]
    fn complex_dense_variants_are_bounded_and_deterministic() {
        let query = "How did Alice and Bob compare Carol with Dana?";
        let first = complex_dense_query_variants(query);
        let second = complex_dense_query_variants(query);

        assert_eq!(first, second);
        assert_eq!(first.first().map(String::as_str), Some(query));
        assert_eq!(
            first.get(1).map(String::as_str),
            Some("Alice and Bob compare Carol with Dana")
        );
        assert_eq!(first.get(2).map(String::as_str), Some("Alice"));
        assert!(
            first.len() <= 4,
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
