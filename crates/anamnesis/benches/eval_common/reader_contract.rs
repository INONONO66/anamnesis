use std::collections::{BTreeMap, BTreeSet};

use anamnesis::memory::{AnswerShape, RecallIntent, RecallPlan};

pub fn complex_reflection_required(plan: &RecallPlan) -> bool {
    plan.recall_intent == RecallIntent::Temporal
        || matches!(
            plan.answer_shape,
            AnswerShape::Count
                | AnswerShape::Collection
                | AnswerShape::Frequency
                | AnswerShape::Inference
                | AnswerShape::Relationship
        )
}

pub fn query_presents_explicit_alternatives(query: &str) -> bool {
    format!(" {} ", query.trim().to_lowercase()).contains(" or ")
}

pub fn visible_product_source_ids(context: &str) -> BTreeSet<String> {
    let mut allowed = BTreeSet::new();
    let mut block_header = None;
    let mut turn_sources = BTreeSet::new();

    for line in context.lines() {
        if line.starts_with("- [") {
            commit_source_block(&mut allowed, block_header.take(), &mut turn_sources);
            block_header = parse_node_marker(line, "source=node:");
            continue;
        }
        if let Some(source_id) = parse_node_marker(line, "turn-source=node:") {
            turn_sources.insert(source_id);
        }
    }
    commit_source_block(&mut allowed, block_header, &mut turn_sources);
    allowed
}

fn commit_source_block(
    allowed: &mut BTreeSet<String>,
    block_header: Option<String>,
    turn_sources: &mut BTreeSet<String>,
) {
    if turn_sources.is_empty() {
        if let Some(source_id) = block_header {
            allowed.insert(source_id);
        }
    } else {
        allowed.append(turn_sources);
    }
    turn_sources.clear();
}

fn parse_node_marker(line: &str, marker: &str) -> Option<String> {
    let suffix = line.split_once(marker)?.1;
    let digits: String = suffix.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| format!("node:{digits}"))
}

pub fn parse_reflection_json(reflection: &str) -> Option<serde_json::Value> {
    let trimmed = reflection.trim();
    let json = if let Some(fenced) = trimmed.strip_prefix("```") {
        let fenced = fenced.strip_suffix("```")?;
        let newline = fenced.find('\n')?;
        let language = fenced[..newline].trim();
        if !language.is_empty() && language != "json" {
            return None;
        }
        fenced[newline + 1..].trim()
    } else {
        trimmed
    };
    let parsed: serde_json::Value = serde_json::from_str(json).ok()?;
    parsed.is_object().then_some(parsed)
}

pub fn reconcile_reflected_answer(
    query: &str,
    reflection: &str,
    final_answer: &str,
    allowed_source_ids: &BTreeSet<String>,
) -> Option<String> {
    let parsed = parse_reflection_json(reflection)?;
    if !reflection_is_unambiguous(&parsed)
        || !reflection_sources_are_visible(&parsed, allowed_source_ids)
    {
        return None;
    }
    let candidate = answer_value(parsed.get("candidate_answer")?)?;
    let candidate_polarity = answer_polarity(&candidate);
    let final_polarity = answer_polarity(final_answer);

    if query_presents_explicit_alternatives(query)
        && final_polarity.is_some()
        && candidate_polarity.is_none()
    {
        return Some(candidate);
    }
    if query_starts_with_binary_auxiliary(query)
        && candidate_polarity.is_some()
        && final_polarity.is_some()
        && candidate_polarity != final_polarity
    {
        return Some(candidate);
    }
    None
}

pub fn query_starts_with_binary_auxiliary(query: &str) -> bool {
    let first = query
        .trim_start()
        .split(|character: char| !character.is_alphabetic())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "am" | "are"
            | "can"
            | "could"
            | "did"
            | "do"
            | "does"
            | "had"
            | "has"
            | "have"
            | "is"
            | "may"
            | "might"
            | "should"
            | "was"
            | "were"
            | "will"
            | "would"
    )
}

pub fn query_requests_plural_public_branches(query: &str) -> bool {
    let normalized = format!(" {} ", query.trim().to_ascii_lowercase());
    let requests_plural_location = [" states ", " countries ", " cities ", " locations "]
        .iter()
        .any(|needle| normalized.contains(needle));
    requests_plural_location
        && (normalized.contains(" branches ")
            || normalized.contains(" located ")
            || normalized.contains(" locations of ")
            || (normalized.contains(" based on ") && normalized.contains(" visiting ")))
}

fn reflection_is_unambiguous(parsed: &serde_json::Value) -> bool {
    parsed
        .get("missing_or_ambiguous")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| {
            value
                .trim()
                .trim_end_matches('.')
                .eq_ignore_ascii_case("none")
        })
}

fn reflection_sources_are_visible(
    parsed: &serde_json::Value,
    allowed_source_ids: &BTreeSet<String>,
) -> bool {
    if allowed_source_ids.is_empty() {
        return false;
    }
    let mut cited = BTreeSet::new();
    collect_node_source_ids(parsed, &mut cited);
    !cited.is_empty() && cited.is_subset(allowed_source_ids)
}

fn collect_node_source_ids(value: &serde_json::Value, cited: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(value) => collect_node_source_ids_from_text(value, cited),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_node_source_ids(value, cited);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_node_source_ids(value, cited);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn collect_node_source_ids_from_text(value: &str, cited: &mut BTreeSet<String>) {
    for (start, _) in value.match_indices("node:") {
        let suffix = &value[start + "node:".len()..];
        let digits: String = suffix.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            cited.insert(format!("node:{digits}"));
        }
    }
}

fn answer_polarity(answer: &str) -> Option<bool> {
    let normalized = answer.trim_start().to_ascii_lowercase();
    let mut words = normalized
        .split(|character: char| !character.is_alphabetic())
        .filter(|word| !word.is_empty());
    let first = words.next()?;
    let second = words.next();
    match (first, second) {
        // Abstentions are not negative answers and must not trigger polarity
        // reconciliation merely because they begin with the word "no".
        ("no", Some("information" | "evidence" | "answer")) => None,
        ("yes", _) => Some(true),
        ("no", _) | ("unlikely", _) => Some(false),
        ("likely", Some("yes")) => Some(true),
        ("likely", Some("no")) => Some(false),
        _ => None,
    }
}

pub fn validated_collection_items(
    reflection: &str,
    allowed_source_ids: &BTreeSet<String>,
) -> Option<Vec<String>> {
    validate_collection_items(reflection, allowed_source_ids, &BTreeSet::new())
        .map(|(items, _)| items)
}

pub fn validated_collection_items_for_query(
    query: &str,
    reflection: &str,
    allowed_source_ids: &BTreeSet<String>,
    product_context: &str,
) -> Option<(Vec<String>, bool)> {
    let excluded_source_ids = clearly_other_speaker_response_sources(query, product_context);
    validate_collection_items(reflection, allowed_source_ids, &excluded_source_ids)
}

fn validate_collection_items(
    reflection: &str,
    allowed_source_ids: &BTreeSet<String>,
    excluded_source_ids: &BTreeSet<String>,
) -> Option<(Vec<String>, bool)> {
    let parsed = parse_reflection_json(reflection)?;
    if !reflection_is_unambiguous(&parsed) {
        return None;
    }
    let answer_items = parsed.get("answer_items")?.as_array()?;
    if allowed_source_ids.is_empty() {
        return None;
    }

    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let mut excluded_an_item = false;
    for item in answer_items {
        let value = answer_value(item.get("value")?)?;
        let source_ids = item.get("source_ids")?.as_array()?;
        if source_ids.is_empty()
            || source_ids.iter().any(|source_id| {
                source_id
                    .as_str()
                    .is_none_or(|source_id| !allowed_source_ids.contains(source_id))
            })
        {
            return None;
        }
        if source_ids.iter().all(|source_id| {
            source_id
                .as_str()
                .is_some_and(|source_id| excluded_source_ids.contains(source_id))
        }) {
            excluded_an_item = true;
            continue;
        }
        let normalized = normalize_collection_item(&value);
        if normalized.is_empty() {
            return None;
        }
        if seen.insert(normalized) {
            items.push(value);
        }
    }
    (!items.is_empty()).then_some((items, excluded_an_item))
}

#[derive(Clone)]
struct BoundDialogueTurn {
    source_id: String,
    speaker: String,
    text: String,
}

fn clearly_other_speaker_response_sources(query: &str, context: &str) -> BTreeSet<String> {
    let turns: Vec<_> = context
        .lines()
        .filter_map(parse_bound_dialogue_turn)
        .collect();
    let speakers: BTreeSet<_> = turns.iter().map(|turn| turn.speaker.as_str()).collect();
    let mut matching_speakers = speakers
        .into_iter()
        .filter(|speaker| contains_normalized_phrase(query, speaker));
    let Some(target) = matching_speakers.next() else {
        return BTreeSet::new();
    };
    if matching_speakers.next().is_some() {
        return BTreeSet::new();
    }

    let mut excluded = BTreeSet::new();
    let mut previous: Option<BoundDialogueTurn> = None;
    for line in context.lines() {
        if line.trim_start().starts_with("- [") {
            previous = None;
        }
        let Some(turn) = parse_bound_dialogue_turn(line) else {
            continue;
        };
        if let Some(prior) = previous.as_ref()
            && prior.speaker.eq_ignore_ascii_case(target)
            && !turn.speaker.eq_ignore_ascii_case(target)
            && asks_about_addressee(&prior.text)
            && !explicitly_attributes_to_target(&turn.text, target)
        {
            excluded.insert(turn.source_id.clone());
        }
        previous = Some(turn);
    }
    excluded
}

fn parse_bound_dialogue_turn(line: &str) -> Option<BoundDialogueTurn> {
    let line = line.trim_start();
    let source_id = parse_node_marker(line, "turn-source=node:")?;
    let (_, body) = line.split_once("] ")?;
    let (speaker, text) = body.split_once(':')?;
    let speaker = speaker.trim();
    let text = text.trim();
    if speaker.is_empty() || text.is_empty() {
        return None;
    }
    Some(BoundDialogueTurn {
        source_id,
        speaker: speaker.to_owned(),
        text: text.to_owned(),
    })
}

fn contains_normalized_phrase(text: &str, phrase: &str) -> bool {
    let text = normalized_words(text);
    let phrase = normalized_words(phrase);
    !phrase.is_empty() && format!(" {text} ").contains(&format!(" {phrase} "))
}

fn normalized_words(value: &str) -> String {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn asks_about_addressee(text: &str) -> bool {
    text.contains('?')
        && ["you", "your", "yours", "yourself"]
            .iter()
            .any(|pronoun| contains_normalized_phrase(text, pronoun))
}

fn explicitly_attributes_to_target(text: &str, target: &str) -> bool {
    contains_normalized_phrase(text, target)
        || ["you", "your", "yours", "yourself"]
            .iter()
            .any(|pronoun| contains_normalized_phrase(text, pronoun))
}

pub fn collection_answer_misses_item(answer: &str, items: &[String]) -> bool {
    let answer_tokens = collection_token_counts(answer);
    items.iter().any(|item| {
        let item_tokens = collection_token_counts(item);
        !item_tokens.is_empty()
            && item_tokens.iter().any(|(token, count)| {
                answer_tokens.get(token).copied().unwrap_or_default() < *count
            })
    })
}

fn answer_value(value: &serde_json::Value) -> Option<String> {
    let answer = match value {
        serde_json::Value::String(value) => value.trim().to_owned(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(answer_value)
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null | serde_json::Value::Object(_) => return None,
    };
    (!answer.is_empty()).then_some(answer)
}

fn normalize_collection_item(value: &str) -> String {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn collection_token_counts(value: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for token in value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .filter(|token| {
            !matches!(
                token.as_str(),
                "a" | "an"
                    | "and"
                    | "at"
                    | "by"
                    | "for"
                    | "from"
                    | "in"
                    | "of"
                    | "on"
                    | "the"
                    | "to"
                    | "with"
            )
        })
        .map(|token| {
            if token.len() > 5 && token.ends_with("ies") {
                format!("{}y", &token[..token.len() - 3])
            } else if token.len() > 4 && token.ends_with('s') && !token.ends_with("ss") {
                token[..token.len() - 1].to_owned()
            } else {
                token
            }
        })
    {
        *counts.entry(token).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_validated_items_detect_final_answer_omissions() {
        let allowed = ["D1:1".to_owned(), "node:7".to_owned()]
            .into_iter()
            .collect();
        let reflection = serde_json::json!({
            "missing_or_ambiguous": "None",
            "answer_items": [
                {"value": "California", "source_ids": ["D1:1"]},
                {"value": "Florida", "source_ids": ["D1:1"]},
                {"value": "Lisbon", "source_ids": ["node:7"]}
            ]
        })
        .to_string();
        let items = validated_collection_items(&reflection, &allowed).expect("validated items");
        assert!(collection_answer_misses_item(
            "California and Florida",
            &items
        ));
        assert!(!collection_answer_misses_item(
            "California, Florida, Lisbon",
            &items
        ));
    }

    #[test]
    fn unsupported_or_missing_source_ids_reject_the_backfill() {
        let allowed = ["D1:1".to_owned()].into_iter().collect();
        let unsupported = serde_json::json!({
            "missing_or_ambiguous": "None",
            "answer_items": [{"value": "Lisbon", "source_ids": ["D9:9"]}]
        })
        .to_string();
        let missing = serde_json::json!({
            "missing_or_ambiguous": "None",
            "answer_items": [{"value": "Lisbon", "source_ids": []}]
        })
        .to_string();
        assert!(validated_collection_items(&unsupported, &allowed).is_none());
        assert!(validated_collection_items(&missing, &allowed).is_none());

        let ambiguous = serde_json::json!({
            "missing_or_ambiguous": "A second location may be missing",
            "answer_items": [{"value": "Lisbon", "source_ids": ["D1:1"]}]
        })
        .to_string();
        assert!(validated_collection_items(&ambiguous, &allowed).is_none());
    }

    #[test]
    fn strict_reflection_parser_accepts_raw_or_single_fenced_json_only() {
        let raw = r#"{"candidate_answer":"California"}"#;
        let fenced = "```json\n{\"candidate_answer\":\"California\"}\n```";
        assert_eq!(parse_reflection_json(raw), parse_reflection_json(fenced));
        assert!(
            parse_reflection_json(
                "Evidence follows:\n```json\n{\"candidate_answer\":\"California\"}\n```"
            )
            .is_none()
        );
        assert!(parse_reflection_json("```text\n{}\n```").is_none());
        assert!(parse_reflection_json("[\"not\", \"an\", \"object\"]").is_none());
    }

    #[test]
    fn collection_completeness_is_order_insensitive_and_handles_possessives_and_plurals() {
        let items = vec![
            "Evan's California collaborations".to_owned(),
            "Influencers in Florida".to_owned(),
        ];
        assert!(!collection_answer_misses_item(
            "Florida influencers; collaborations by Evan in California",
            &items
        ));
        assert!(collection_answer_misses_item(
            "Collaborations by Evan in California",
            &items
        ));
    }

    #[test]
    fn complex_reflection_includes_date_scoped_fact_queries() {
        let date_scoped = RecallPlan::infer("Which book did Jolene read in January 2023?");
        assert_eq!(date_scoped.answer_shape, AnswerShape::Fact);
        assert_eq!(date_scoped.recall_intent, RecallIntent::Temporal);
        assert!(complex_reflection_required(&date_scoped));

        assert!(!complex_reflection_required(&RecallPlan::infer(
            "What is Jolene's favorite book?"
        )));
    }

    #[test]
    fn detects_explicit_answer_alternatives_without_treating_other_questions_as_choices() {
        assert!(query_presents_explicit_alternatives(
            "Would Tim enjoy books by C. S. Lewis or John Green?"
        ));
        assert!(query_presents_explicit_alternatives(
            "Did Alice arrive before or after Bob?"
        ));
        assert!(!query_presents_explicit_alternatives(
            "Which composer wrote the film theme?"
        ));
    }

    #[test]
    fn visible_product_sources_prefer_turn_bindings_over_semantic_window_headers() {
        let context = "\
## KNOWLEDGE
- [Semantic source=node:274] travel window
    [turn-source=node:269] James: I visited Italy.
    [turn-source=node:271] John: I visited Japan.
- [Episodic source=node:769] James visited Nuuk.
";
        assert_eq!(
            visible_product_source_ids(context),
            ["node:269", "node:271", "node:769"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }

    #[test]
    fn reconciles_only_source_grounded_shape_or_polarity_regressions() {
        let allowed = ["node:7".to_owned()].into_iter().collect();
        let supported_yes = serde_json::json!({
            "evidence_findings": ["The shelter is in Stamford (node:7)."],
            "candidate_answer": "Yes; Stamford shelter",
            "missing_or_ambiguous": "None"
        })
        .to_string();
        assert_eq!(
            reconcile_reflected_answer(
                "Does James live in Connecticut?",
                &supported_yes,
                "No; unrelated venue",
                &allowed,
            ),
            Some("Yes; Stamford shelter".to_owned())
        );

        let supported_choice = serde_json::json!({
            "evidence_findings": ["Tim enjoys fantasy books (node:7)."],
            "candidate_answer": "C. S. Lewis",
            "missing_or_ambiguous": "None"
        })
        .to_string();
        assert_eq!(
            reconcile_reflected_answer(
                "Would Tim prefer C. S. Lewis or John Green?",
                &supported_choice,
                "Yes; fantasy books",
                &allowed,
            ),
            Some("C. S. Lewis".to_owned())
        );

        let unsupported = supported_yes.replace("node:7", "node:9");
        assert!(
            reconcile_reflected_answer(
                "Does James live in Connecticut?",
                &unsupported,
                "No; unrelated venue",
                &allowed,
            )
            .is_none()
        );
        assert!(
            reconcile_reflected_answer(
                "Does James live in Connecticut?",
                &supported_yes,
                "Yes; nearby shelter",
                &allowed,
            )
            .is_none()
        );
    }

    #[test]
    fn polarity_requires_whole_words_and_ignores_abstentions() {
        assert_eq!(answer_polarity("Yes; supported"), Some(true));
        assert_eq!(
            answer_polarity("Likely no, based on the evidence"),
            Some(false)
        );
        assert_eq!(answer_polarity("No; contradicted"), Some(false));
        assert_eq!(answer_polarity("No information available"), None);
        assert_eq!(answer_polarity("No evidence was found"), None);
        assert_eq!(answer_polarity("none"), None);
        assert_eq!(answer_polarity("nothing relevant"), None);
        assert_eq!(answer_polarity("north of the venue"), None);
        assert_eq!(answer_polarity("not enough evidence"), None);
    }

    #[test]
    fn detects_binary_auxiliary_without_misclassifying_wh_questions() {
        assert!(query_starts_with_binary_auxiliary(
            "Would Melanie enjoy classical music?"
        ));
        assert!(query_starts_with_binary_auxiliary(
            "Does James live in Connecticut?"
        ));
        assert!(!query_starts_with_binary_auxiliary(
            "Which company signed John?"
        ));
    }

    #[test]
    fn plural_public_branches_exclude_personal_travel_collections() {
        assert!(query_requests_plural_public_branches(
            "Which US states might Tim be in based on his plans of visiting a theme park?"
        ));
        assert!(query_requests_plural_public_branches(
            "In which cities are the organization's branches located?"
        ));
        assert!(!query_requests_plural_public_branches(
            "Which countries has James visited?"
        ));
    }

    #[test]
    fn collection_ownership_rejects_an_interlocutors_direct_reply() {
        let context = "\
## KNOWLEDGE
- [Semantic source=node:274] travel window
    [turn-source=node:269] James: I visited Italy. What was the last country you visited?
    [turn-source=node:271] John: This was Japan.
    [turn-source=node:713] John: You will visit Canada next.
- [Episodic source=node:769] James visited Nuuk.
";
        let allowed = visible_product_source_ids(context);
        let reflection = serde_json::json!({
            "missing_or_ambiguous": "None",
            "answer_items": [
                {"value": "Italy", "source_ids": ["node:269"]},
                {"value": "Japan", "source_ids": ["node:271"]},
                {"value": "Canada", "source_ids": ["node:713"]},
                {"value": "Greenland", "source_ids": ["node:769"]}
            ]
        })
        .to_string();
        assert_eq!(
            validated_collection_items_for_query(
                "Which countries has James visited?",
                &reflection,
                &allowed,
                context,
            ),
            Some((
                vec![
                    "Italy".to_owned(),
                    "Canada".to_owned(),
                    "Greenland".to_owned()
                ],
                true,
            ))
        );
    }
}
