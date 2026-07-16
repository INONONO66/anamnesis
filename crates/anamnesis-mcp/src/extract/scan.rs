use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::extract::{
    profile::profile_id,
    types::{ExtractionScanResult, ExtractionSource, ExtractorProfileComponents},
};

/// Returns the lowercase SHA-256 digest of the source's exact UTF-8 bytes.
pub(crate) fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Selects the oldest eligible, unprocessed session-and-scope capture batch.
pub(crate) fn scan(
    sources: Vec<ExtractionSource>,
    processed_turn_keys: &HashSet<String>,
    profile: &ExtractorProfileComponents,
    min_turns: u32,
    max_turns: u32,
) -> Result<ExtractionScanResult> {
    let min_turns = usize::try_from(min_turns)?;
    let max_turns = usize::try_from(max_turns)?;
    let mut groups = BTreeMap::<(String, String), Vec<ExtractionSource>>::new();

    for mut source in sources {
        if processed_turn_keys.contains(&source.turn_key) {
            continue;
        }
        source.content_hash = content_hash(&source.content);
        groups
            .entry((source.session_id.clone(), source.scope.clone()))
            .or_default()
            .push(source);
    }

    let mut eligible = groups
        .into_iter()
        .filter_map(|(group_key, mut sources)| {
            sources.sort_by(|left, right| {
                (left.at_ms, left.turn_key.as_str()).cmp(&(right.at_ms, right.turn_key.as_str()))
            });
            (sources.len() >= min_turns).then_some((group_key, sources))
        })
        .collect::<Vec<_>>();

    eligible.sort_by(|(left_key, left_sources), (right_key, right_sources)| {
        let left_first = &left_sources[0];
        let right_first = &right_sources[0];
        (left_first.at_ms, left_first.turn_key.as_str(), left_key).cmp(&(
            right_first.at_ms,
            right_first.turn_key.as_str(),
            right_key,
        ))
    });

    let sources = match eligible.into_iter().next() {
        Some((_, sources)) => sources.into_iter().take(max_turns).collect(),
        None => Vec::new(),
    };

    Ok(ExtractionScanResult {
        profile_id: profile_id(profile)?,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn profile() -> ExtractorProfileComponents {
        ExtractorProfileComponents {
            provider_id: "extractor".into(),
            model_id: "model".into(),
            prompt_version: 1,
            schema_version: 1,
            normalization_version: 1,
            relation_policy_version: 1,
            command_hash: "command".into(),
        }
    }

    fn source(session_id: &str, scope: &str, at_ms: u64, turn_key: &str) -> ExtractionSource {
        ExtractionSource {
            node_id: at_ms,
            turn_key: turn_key.into(),
            session_id: session_id.into(),
            scope: scope.into(),
            content: format!("{session_id}/{scope}/{turn_key}"),
            content_hash: "stale".into(),
            at_ms,
        }
    }

    #[test]
    fn selects_the_oldest_eligible_session_scope_group_and_sorts_it() {
        let mut sources = (0..9)
            .map(|index| source("shared", "scope-a", 100 + index, &format!("a-{index}")))
            .collect::<Vec<_>>();
        sources.extend(
            (0..10)
                .rev()
                .map(|index| source("shared", "scope-b", 200 + index, &format!("b-{index:02}"))),
        );
        sources.extend(
            (0..21).map(|index| source("later", "scope-a", 300 + index, &format!("c-{index}"))),
        );

        let result = scan(sources, &HashSet::new(), &profile(), 10, 20).expect("scan");

        assert_eq!(result.sources.len(), 10);
        assert!(
            result
                .sources
                .iter()
                .all(|source| source.session_id == "shared" && source.scope == "scope-b")
        );
        assert_eq!(
            result
                .sources
                .iter()
                .map(|source| source.turn_key.as_str())
                .collect::<Vec<_>>(),
            (0..10)
                .map(|index| format!("b-{index:02}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn excludes_processed_turns_before_applying_the_minimum() {
        let sources = (0..10)
            .map(|index| source("session", "scope", index, &format!("turn-{index}")))
            .collect();
        let processed = HashSet::from(["turn-0".to_string()]);

        let result = scan(sources, &processed, &profile(), 10, 20).expect("scan");

        assert!(result.sources.is_empty());
    }

    #[test]
    fn caps_the_selected_group_at_max_turns_and_hashes_exact_utf8_content() {
        let content = "café 🦀\ncombining: e\u{301}";
        let sources = (0..21)
            .map(|index| ExtractionSource {
                content: content.into(),
                ..source("session", "scope", index, &format!("turn-{index:02}"))
            })
            .collect();

        let result = scan(sources, &HashSet::new(), &profile(), 10, 20).expect("scan");

        assert_eq!(result.sources.len(), 20);
        assert!(
            result
                .sources
                .iter()
                .all(|source| source.content_hash == content_hash(content))
        );
    }
}
