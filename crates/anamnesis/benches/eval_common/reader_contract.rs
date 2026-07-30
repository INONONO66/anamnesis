use std::collections::BTreeSet;

pub fn validated_collection_items(
    reflection: &str,
    allowed_source_ids: &BTreeSet<String>,
) -> Option<Vec<String>> {
    let parsed: serde_json::Value = serde_json::from_str(reflection).ok()?;
    let answer_items = parsed.get("answer_items")?.as_array()?;
    if allowed_source_ids.is_empty() {
        return None;
    }

    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
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
        let normalized = normalize_collection_item(&value);
        if normalized.is_empty() {
            return None;
        }
        if seen.insert(normalized) {
            items.push(value);
        }
    }
    (!items.is_empty()).then_some(items)
}

pub fn collection_answer_misses_item(answer: &str, items: &[String]) -> bool {
    let normalized_answer = normalize_collection_item(answer);
    items.iter().any(|item| {
        let normalized_item = normalize_collection_item(item);
        !normalized_item.is_empty() && !normalized_answer.contains(&normalized_item)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_validated_items_detect_final_answer_omissions() {
        let allowed = ["D1:1".to_owned(), "node:7".to_owned()]
            .into_iter()
            .collect();
        let reflection = serde_json::json!({
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
            "answer_items": [{"value": "Lisbon", "source_ids": ["D9:9"]}]
        })
        .to_string();
        let missing = serde_json::json!({
            "answer_items": [{"value": "Lisbon", "source_ids": []}]
        })
        .to_string();
        assert!(validated_collection_items(&unsupported, &allowed).is_none());
        assert!(validated_collection_items(&missing, &allowed).is_none());
    }
}
