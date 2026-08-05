use std::collections::BTreeSet;

use anamnesis::graph::NodeId;
use anamnesis::memory::{
    GroundedAnswerDraft, GroundedAnswerItem, RecallPlan, RecallReaderContract,
    RecallSourceAttribution,
};

const MAX_SOURCE_IDS_PER_CLAIM: usize = 8;

/// Route a query through the production contract's recommended read strategy.
pub fn complex_reflection_required(plan: &RecallPlan) -> bool {
    plan.reader_contract().reflection_recommended()
}

/// Parse one provider JSON object into the core's provider-neutral draft.
///
/// The parser accepts only typed `node:<u64>` citations. Dataset relevance,
/// reference answers, categories, and judge output are not inputs.
pub fn parse_grounded_draft(reflection: &str) -> Option<GroundedAnswerDraft> {
    let parsed = parse_reflection_json(reflection)?;
    let object = parsed.as_object()?;
    const REQUIRED_KEYS: [&str; 6] = [
        "required_slots",
        "evidence_findings",
        "reasoning_chain",
        "answer_items",
        "candidate_answer",
        "missing_or_ambiguous",
    ];
    if object.len() != REQUIRED_KEYS.len()
        || REQUIRED_KEYS.iter().any(|key| !object.contains_key(*key))
    {
        return None;
    }
    validate_short_string_array(parsed.get("required_slots")?)?;
    validate_short_string_array(parsed.get("reasoning_chain")?)?;
    let candidate_answer = answer_value(parsed.get("candidate_answer")?)?;
    let missing_or_ambiguous = parse_missing_or_ambiguous(parsed.get("missing_or_ambiguous")?)?;
    let mut cited_source_node_ids = BTreeSet::new();
    for finding in parsed.get("evidence_findings")?.as_array()? {
        answer_value(finding.get("fact")?)?;
        let source_node_ids = parse_source_ids(finding.get("source_ids")?)?;
        if source_node_ids.is_empty() {
            return None;
        }
        cited_source_node_ids.extend(source_node_ids);
    }
    let answer_items = parsed
        .get("answer_items")?
        .as_array()?
        .iter()
        .map(parse_grounded_item)
        .collect::<Option<Vec<_>>>()?;
    cited_source_node_ids.extend(
        answer_items
            .iter()
            .flat_map(|item| item.source_node_ids.iter().copied()),
    );
    Some(GroundedAnswerDraft::new(
        candidate_answer,
        answer_items,
        cited_source_node_ids.into_iter().collect(),
        missing_or_ambiguous,
    ))
}

fn parse_missing_or_ambiguous(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Null => Some(false),
        serde_json::Value::String(value) => Some(
            !value
                .trim()
                .trim_end_matches('.')
                .eq_ignore_ascii_case("none"),
        ),
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => None,
    }
}

/// Apply the core contract's bounded source-membership reconciliation.
pub fn reconcile_reflected_answer(
    contract: &RecallReaderContract,
    reflection: &str,
    final_answer: &str,
    allowed_source_node_ids: &[u64],
    source_attributions: &[RecallSourceAttribution],
) -> Option<String> {
    let draft = parse_grounded_draft(reflection)?;
    let allowed: Vec<_> = allowed_source_node_ids
        .iter()
        .copied()
        .map(NodeId)
        .collect();
    contract.reconcile_grounded_draft_with_attributions(
        &draft,
        final_answer,
        &allowed,
        source_attributions,
    )
}

fn parse_grounded_item(value: &serde_json::Value) -> Option<GroundedAnswerItem> {
    let value_text = answer_value(value.get("value")?)?;
    let source_node_ids = parse_source_ids(value.get("source_ids")?)?;
    if source_node_ids.is_empty() {
        return None;
    }
    Some(GroundedAnswerItem::new(value_text, source_node_ids))
}

fn parse_source_ids(value: &serde_json::Value) -> Option<Vec<NodeId>> {
    let source_ids: Vec<_> = value
        .as_array()?
        .iter()
        .map(|value| parse_node_source_id(value.as_str()?))
        .collect::<Option<_>>()?;
    (!source_ids.is_empty() && source_ids.len() <= MAX_SOURCE_IDS_PER_CLAIM).then_some(source_ids)
}

fn validate_short_string_array(value: &serde_json::Value) -> Option<()> {
    value
        .as_array()?
        .iter()
        .all(|item| item.as_str().is_some_and(|item| !item.trim().is_empty()))
        .then_some(())
}

fn parse_reflection_json(reflection: &str) -> Option<serde_json::Value> {
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

fn answer_value(value: &serde_json::Value) -> Option<String> {
    let answer = match value {
        serde_json::Value::String(value) => value.trim().to_owned(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(answer_value)
            .collect::<Option<Vec<_>>>()?
            .join(", "),
        serde_json::Value::Null | serde_json::Value::Object(_) => return None,
    };
    (!answer.is_empty()).then_some(answer)
}

fn parse_node_source_id(value: &str) -> Option<NodeId> {
    let digits = value.strip_prefix("node:")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok().map(NodeId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis::memory::RecallReaderStage;

    fn source_attribution(id: u64, speaker: &str, line_order: usize) -> RecallSourceAttribution {
        RecallSourceAttribution::new(
            NodeId(id),
            Some(speaker.to_owned()),
            format!("{speaker}: source evidence"),
            "session-a",
            NodeId(20),
            line_order,
        )
    }

    #[test]
    fn reflection_routing_delegates_to_the_core_contract() {
        let date_scoped = RecallPlan::infer("Which operation ran in January 2023?");
        assert!(complex_reflection_required(&date_scoped));
        assert!(!complex_reflection_required(&RecallPlan::infer(
            "What is the configured cache?"
        )));
    }

    #[test]
    fn parser_accepts_only_complete_json_with_typed_node_sources() {
        let reflection = serde_json::json!({
            "required_slots": ["completed targets"],
            "evidence_findings": [
                {"fact": "Two targets completed.", "source_ids": ["node:7", "node:9"]}
            ],
            "reasoning_chain": [],
            "answer_items": [
                {"value": "North region", "source_ids": ["node:7"]},
                {"value": "South region", "source_ids": ["node:9"]}
            ],
            "candidate_answer": "North region, South region",
            "missing_or_ambiguous": "None"
        })
        .to_string();
        let draft = parse_grounded_draft(&reflection).expect("typed draft");
        assert_eq!(draft.cited_source_node_ids, vec![NodeId(7), NodeId(9)]);
        assert_eq!(draft.answer_items.len(), 2);

        let canonical_null = reflection.replace("\"None\"", "null");
        let null_draft = parse_grounded_draft(&canonical_null).expect("canonical null draft");
        assert!(!null_draft.missing_or_ambiguous);

        let mut bounded_multi_source: serde_json::Value =
            serde_json::from_str(&reflection).expect("fixture JSON");
        bounded_multi_source["answer_items"][0]["source_ids"] =
            serde_json::json!(["node:1", "node:2", "node:3", "node:4"]);
        assert!(parse_grounded_draft(&bounded_multi_source.to_string()).is_some());
        bounded_multi_source["answer_items"][0]["source_ids"] = serde_json::json!([
            "node:1", "node:2", "node:3", "node:4", "node:5", "node:6", "node:7", "node:8",
            "node:9"
        ]);
        assert!(parse_grounded_draft(&bounded_multi_source.to_string()).is_none());

        let external_id = reflection.replace("node:7", "turn-7");
        assert!(parse_grounded_draft(&external_id).is_none());
        assert!(parse_grounded_draft("analysis before {\"answer_items\":[]}").is_none());
    }

    #[test]
    fn evaluator_reconciliation_uses_only_production_source_membership() {
        let contract =
            RecallPlan::infer("List every completed deployment target.").reader_contract();
        let reflection = serde_json::json!({
            "required_slots": ["completed targets"],
            "evidence_findings": [
                {"fact": "Two targets completed.", "source_ids": ["node:7", "node:9"]}
            ],
            "reasoning_chain": [],
            "answer_items": [
                {"value": "North region", "source_ids": ["node:7"]},
                {"value": "South region", "source_ids": ["node:9"]}
            ],
            "candidate_answer": "North region, South region",
            "missing_or_ambiguous": "None"
        })
        .to_string();
        let attributions = vec![
            source_attribution(7, "operator", 0),
            source_attribution(9, "operator", 1),
        ];
        assert_eq!(
            reconcile_reflected_answer(
                &contract,
                &reflection,
                "North region",
                &[7, 9],
                &attributions,
            ),
            Some("North region, South region".to_owned())
        );
        assert!(
            reconcile_reflected_answer(
                &contract,
                &reflection,
                "North region",
                &[7],
                &attributions,
            )
            .is_none()
        );
    }

    #[test]
    fn collection_reconciliation_declines_an_other_speaker_reply() {
        let contract =
            RecallPlan::infer("List every region the operator visited.").reader_contract();
        let attributions = vec![
            source_attribution(7, "operator", 0),
            source_attribution(9, "colleague", 1),
        ];
        let reflection = serde_json::json!({
            "required_slots": ["visited regions"],
            "evidence_findings": [
                {"fact": "The operator visited the north region.", "source_ids": ["node:7"]},
                {"fact": "The south region was visited.", "source_ids": ["node:9"]}
            ],
            "reasoning_chain": [],
            "answer_items": [
                {"value": "north region", "source_ids": ["node:7"]},
                {"value": "south region", "source_ids": ["node:9"]}
            ],
            "candidate_answer": "north region, south region",
            "missing_or_ambiguous": "None"
        })
        .to_string();
        assert!(
            reconcile_reflected_answer(
                &contract,
                &reflection,
                "north region",
                &[7, 9],
                &attributions,
            )
            .is_none()
        );
    }

    #[test]
    fn shared_contract_supplies_all_three_reader_stages() {
        let contract = RecallPlan::infer("Why are the two events related?").reader_contract();
        assert!(
            contract
                .instruction(RecallReaderStage::Answer)
                .contains("directed relationship")
        );
        assert!(
            contract
                .instruction(RecallReaderStage::Reflection)
                .contains("minimal reasoning chain")
        );
        assert!(
            contract
                .instruction(RecallReaderStage::Verification)
                .contains("shortest verified")
        );
    }
}
