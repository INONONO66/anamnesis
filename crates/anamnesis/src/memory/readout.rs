//! Deterministic evidence readout planning for the [`Memory`](super::Memory) facade.
//!
//! The kernel ranks graph nodes. This module turns that node ranking into a
//! source-aware evidence ranking without calling a generative model. Raw
//! Episodic fragments remain the canonical evidence units; Semantic windows and
//! reviewed derived knowledge are representations attached to those units.

use std::collections::HashSet;

use crate::error::Error;
use crate::graph::{EdgeType, KnowledgeType, NodeId};
use crate::storage::StorageAdapter;

use super::{RerankedCandidate, parse_entity_tags};

/// Deterministic question shape used by deep memory readout.
///
/// This is deliberately a retrieval intent, not an answer taxonomy. It only
/// controls whether the readout should preserve pure relevance order or prefer
/// candidates that add distinct raw evidence sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecallIntent {
    /// One fact or a general semantic lookup.
    Direct,
    /// A list, count, or set-membership question that needs distinct evidence.
    Enumeration,
    /// A question anchored in time. Relevance order remains authoritative.
    Temporal,
    /// A question explicitly relating multiple entities, events, or causes.
    Relational,
}

/// Source-aware selection applied before normal package validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceSelection {
    /// Choose the policy from the deterministic [`RecallIntent`].
    ///
    /// Enumeration and relational questions use [`SourceCoverage`](Self::SourceCoverage);
    /// direct and temporal questions preserve relevance order.
    #[default]
    Auto,
    /// Preserve the supplied ranking byte-for-byte.
    Relevance,
    /// Keep only the highest-ranked representation of an identical raw-source set.
    DistinctSources,
    /// Keep a candidate only when it contributes at least one raw source that
    /// higher-ranked candidates have not covered, then backfill from later candidates.
    SourceCoverage,
}

/// Options for model-free deep recall through [`Memory`](super::Memory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeepRecallOptions {
    /// Maximum number of ranked evidence representations in the final package.
    pub limit: usize,
    /// Source-aware selection policy.
    pub selection: EvidenceSelection,
}

impl DeepRecallOptions {
    /// Build the default automatic deep-readout profile.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            selection: EvidenceSelection::Auto,
        }
    }

    /// Override the deterministic evidence-selection policy.
    pub fn with_selection(mut self, selection: EvidenceSelection) -> Self {
        self.selection = selection;
        self
    }
}

pub(crate) fn classify_intent(query: &str) -> RecallIntent {
    let normalized = query.trim().to_lowercase();
    let words: Vec<_> = normalized
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty())
        .collect();

    let begins_with_any =
        |prefixes: &[&str]| prefixes.iter().any(|prefix| normalized.starts_with(prefix));
    let contains_word = |needle: &str| words.contains(&needle);

    if !crate::query::temporal::parse_time_cues(&normalized, 1_700_000_000_000).is_empty()
        || begins_with_any(&[
            "when ",
            "what date ",
            "what day ",
            "how long ",
            "언제 ",
            "언제였",
            "몇 년",
            "몇 달",
            "몇 주",
            "며칠",
        ])
        || contains_word("before")
        || contains_word("after")
        || contains_word("ago")
        || normalized.contains("지난주")
        || normalized.contains("지난달")
        || normalized.contains("작년")
    {
        return RecallIntent::Temporal;
    }

    if begins_with_any(&[
        "how many ",
        "list ",
        "list all ",
        "what are ",
        "which are ",
        "몇 번 ",
        "몇 개 ",
        "무엇들이 ",
        "어떤 것들이 ",
    ]) || normalized.contains(" all ")
    {
        return RecallIntent::Enumeration;
    }

    if normalized.contains(" relationship between ")
        || normalized.contains(" connection between ")
        || normalized.contains(" in common")
        || normalized.contains(" both ")
        || normalized.contains(" compare ")
        || normalized.contains(" causes ")
        || normalized.contains(" reasons ")
        || normalized.contains(" 관계")
        || normalized.contains(" 공통")
        || normalized.contains(" 원인")
    {
        return RecallIntent::Relational;
    }

    RecallIntent::Direct
}

pub(crate) fn asks_for_time_answer(query: &str) -> bool {
    let normalized = query.trim().to_lowercase();
    [
        "when ",
        "what date ",
        "what day ",
        "what month ",
        "what week ",
        "what year ",
        "which date ",
        "which day ",
        "which month ",
        "which week ",
        "which year ",
        "언제 ",
        "언제였",
        "몇 년",
        "몇 월",
        "몇 주",
        "며칠",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}

pub(crate) fn temporal_evidence_matches(query: &str, evidence: &str) -> bool {
    fn terms(value: &str) -> HashSet<String> {
        value
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| term.len() > 2)
            .map(str::to_lowercase)
            .filter(|term| {
                !matches!(
                    term.as_str(),
                    "after"
                        | "ago"
                        | "before"
                        | "date"
                        | "day"
                        | "did"
                        | "does"
                        | "during"
                        | "for"
                        | "from"
                        | "get"
                        | "got"
                        | "had"
                        | "has"
                        | "have"
                        | "her"
                        | "him"
                        | "his"
                        | "last"
                        | "month"
                        | "next"
                        | "the"
                        | "their"
                        | "this"
                        | "was"
                        | "week"
                        | "were"
                        | "what"
                        | "when"
                        | "which"
                        | "year"
                )
            })
            .map(|term| {
                match term.as_str() {
                    "adopted" => "adopt",
                    "applied" => "apply",
                    "gifted" => "gift",
                    "interviewed" => "interview",
                    "jamming" => "jam",
                    "made" => "make",
                    "married" | "marry" | "wedding" => "marriage",
                    "met" => "meet",
                    "planned" => "plan",
                    "resumed" => "resume",
                    "signed" => "sign",
                    "started" => "start",
                    "attended" | "go" | "took" | "went" => "attend",
                    "returned" | "returning" => "return",
                    "visited" => "visit",
                    "won" => "win",
                    _ => term.as_str(),
                }
                .to_owned()
            })
            .collect()
    }

    let query_terms = terms(query);
    if query_terms.is_empty() {
        return false;
    }
    let evidence_terms = terms(evidence);
    let overlap = query_terms.intersection(&evidence_terms).count();
    overlap >= query_terms.len().min(4)
}

pub(crate) fn compile_ranking<S: StorageAdapter>(
    storage: &S,
    query: &str,
    ranking: &[RerankedCandidate],
    selection: EvidenceSelection,
) -> Result<Vec<RerankedCandidate>, Error> {
    let resolved_selection = match selection {
        EvidenceSelection::Auto => match classify_intent(query) {
            RecallIntent::Enumeration | RecallIntent::Relational => {
                EvidenceSelection::SourceCoverage
            }
            RecallIntent::Direct | RecallIntent::Temporal => EvidenceSelection::Relevance,
        },
        explicit => explicit,
    };

    match resolved_selection {
        EvidenceSelection::Auto | EvidenceSelection::Relevance => Ok(ranking.to_vec()),
        EvidenceSelection::DistinctSources => distinct_source_ranking(storage, ranking),
        EvidenceSelection::SourceCoverage => source_coverage_ranking(storage, ranking),
    }
}

fn distinct_source_ranking<S: StorageAdapter>(
    storage: &S,
    ranking: &[RerankedCandidate],
) -> Result<Vec<RerankedCandidate>, Error> {
    let mut seen = HashSet::new();
    let mut selected = Vec::with_capacity(ranking.len());
    for candidate in ranking {
        let sources = canonical_sources(storage, candidate.node_id)?;
        if seen.insert(sources) {
            selected.push(*candidate);
        }
    }
    Ok(selected)
}

fn source_coverage_ranking<S: StorageAdapter>(
    storage: &S,
    ranking: &[RerankedCandidate],
) -> Result<Vec<RerankedCandidate>, Error> {
    let mut covered = HashSet::new();
    let mut selected = Vec::with_capacity(ranking.len());
    for candidate in ranking {
        let sources = canonical_sources(storage, candidate.node_id)?;
        if sources.iter().any(|source| !covered.contains(source)) {
            covered.extend(sources);
            selected.push(*candidate);
        }
    }
    Ok(selected)
}

pub(super) fn canonical_sources<S: StorageAdapter>(
    storage: &S,
    node_id: NodeId,
) -> Result<Vec<NodeId>, Error> {
    let node = storage.get_node(node_id)?;
    if node.node_type == KnowledgeType::Episodic {
        return Ok(vec![node_id]);
    }

    let mut sources = extracted_episodic_sources(storage, node_id)?;
    if sources.len() == 1
        && node.node_type == KnowledgeType::Semantic
        && !node
            .entity_tags
            .iter()
            .any(|tag| tag == "anamnesis:derived")
    {
        extend_window_sources(storage, node.content.as_str(), sources[0], &mut sources)?;
    }
    if sources.is_empty() {
        sources.push(node_id);
    }
    sources.sort_unstable();
    sources.dedup();
    Ok(sources)
}

fn extracted_episodic_sources<S: StorageAdapter>(
    storage: &S,
    node_id: NodeId,
) -> Result<Vec<NodeId>, Error> {
    let mut sources = Vec::new();
    for &edge_id in storage.edges_from(node_id) {
        let edge = storage.get_edge(edge_id)?;
        if edge.edge_type == EdgeType::ExtractedFrom
            && storage.get_node(edge.target)?.node_type == KnowledgeType::Episodic
        {
            sources.push(edge.target);
        }
    }
    for &edge_id in storage.edges_to(node_id) {
        let edge = storage.get_edge(edge_id)?;
        if edge.edge_type == EdgeType::ExtractedFrom
            && storage.get_node(edge.source)?.node_type == KnowledgeType::Episodic
        {
            sources.push(edge.source);
        }
    }
    sources.sort_unstable();
    sources.dedup();
    Ok(sources)
}

fn extend_window_sources<S: StorageAdapter>(
    storage: &S,
    window_content: &str,
    center: NodeId,
    sources: &mut Vec<NodeId>,
) -> Result<(), Error> {
    let mut neighbors = Vec::new();
    for &edge_id in storage.edges_from(center) {
        let edge = storage.get_edge(edge_id)?;
        if edge.edge_type == EdgeType::Temporal {
            neighbors.push(edge.target);
        }
    }
    for &edge_id in storage.edges_to(center) {
        let edge = storage.get_edge(edge_id)?;
        if edge.edge_type == EdgeType::Temporal {
            neighbors.push(edge.source);
        }
    }
    neighbors.sort_unstable();
    neighbors.dedup();

    for candidate_id in neighbors {
        let candidate = storage.get_node(candidate_id)?;
        if candidate.node_type != KnowledgeType::Episodic {
            continue;
        }
        let (speaker, _) = parse_entity_tags(&candidate.entity_tags);
        let rendered = speaker.map_or_else(
            || candidate.content.clone(),
            |speaker| format!("{speaker}: {}", candidate.content),
        );
        if window_content.lines().any(|line| line.trim() == rendered) {
            sources.push(candidate_id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_retrieval_intents_without_a_model() {
        assert_eq!(
            classify_intent("How many times did Alice move?"),
            RecallIntent::Enumeration
        );
        assert_eq!(
            classify_intent("When did Alice move?"),
            RecallIntent::Temporal
        );
        assert_eq!(
            classify_intent("What is the relationship between Alice and Bob?"),
            RecallIntent::Relational
        );
        assert_eq!(
            classify_intent("Where does Alice live?"),
            RecallIntent::Direct
        );
    }

    #[test]
    fn temporal_precedence_prevents_coverage_reordering() {
        assert_eq!(
            classify_intent("When did Alice and Bob meet?"),
            RecallIntent::Temporal
        );
    }

    #[test]
    fn separates_temporal_retrieval_from_time_answer_rendering() {
        assert!(asks_for_time_answer("When did Alice move?"));
        assert!(asks_for_time_answer("Which week did Alice move?"));
        assert!(!asks_for_time_answer(
            "Where did Alice move four years ago?"
        ));
        assert!(!asks_for_time_answer(
            "Which activity did Alice pursue on 5 June 2023?"
        ));
    }

    #[test]
    fn temporal_evidence_requires_query_subject_overlap() {
        let query = "When did John get married at a greenhouse?";
        assert!(temporal_evidence_matches(
            query,
            "John had a wedding ceremony in a greenhouse last week."
        ));
        assert!(!temporal_evidence_matches(
            query,
            "John won an intense basketball game last week."
        ));
    }
}
