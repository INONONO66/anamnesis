//! Deterministic evidence readout planning for the [`Memory`](super::Memory) facade.
//!
//! The kernel ranks graph nodes. This module turns that node ranking into a
//! source-aware evidence ranking without calling a generative model. Raw
//! Episodic fragments remain the canonical evidence units; Semantic windows and
//! reviewed derived knowledge are representations attached to those units.

use std::collections::{HashMap, HashSet};

use crate::error::Error;
use crate::graph::{EdgeType, KnowledgeType, NodeId, ScopePath};
use crate::storage::StorageAdapter;

use super::{RerankedCandidate, parse_entity_tags};

/// One canonical raw-evidence document for an external reranker.
///
/// The document keeps one live cognitive readout representation, so its score
/// can be passed directly to
/// [`Memory::repackage_reranked`](super::Memory::repackage_reranked). Its text
/// is assembled only from raw Episodic sources not already represented by an
/// earlier document. This prevents overlapping Semantic windows from spending
/// most of a reranker's candidate budget on repeated turns.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EvidenceDocument {
    /// Live readout node that represents this evidence document.
    pub node_id: NodeId,
    /// Canonical raw source nodes represented in `text`.
    pub source_node_ids: Vec<NodeId>,
    /// Speaker-qualified direct source evidence presented to the reranker.
    pub text: String,
}

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

/// Shape of the answer requested by a memory query.
///
/// Unlike [`RecallIntent`], this describes the requested output rather than
/// the retrieval strategy. In particular, a query can be temporally scoped
/// while still requesting a factual answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnswerShape {
    /// One fact, entity, place, or other direct answer.
    Fact,
    /// A calendar date, day, week, month, year, or time range.
    Temporal,
    /// A recurrence cadence inferred from repeated dated events.
    Frequency,
    /// A numeric cardinality.
    Count,
    /// A list or set of answers.
    Collection,
    /// A relationship, comparison, reason, or causal connection.
    Relationship,
    /// A concise implication or likely conclusion grounded in retrieved evidence.
    Inference,
}

/// Deterministic plan shared by deep retrieval and context rendering.
///
/// `Memory` derives this plan from the complete query with a locale-aware,
/// model-free parser. Consumers normally need only pass the query. A consumer
/// with structured intent from its own UI or protocol may override the answer
/// shape without replacing the memory-owned retrieval logic.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecallPlan {
    /// Original query used to derive the plan.
    pub query: String,
    /// Evidence-selection intent.
    pub recall_intent: RecallIntent,
    /// Requested answer shape.
    pub answer_shape: AnswerShape,
}

impl RecallPlan {
    /// Infer a deterministic plan from a natural-language query.
    pub fn infer(query: &str) -> Self {
        infer_plan(query, None)
    }

    /// Infer a plan while honoring a typed answer-shape hint.
    ///
    /// The hint changes answer presentation intent only. Temporal constraints
    /// present in the query still participate in retrieval planning.
    pub fn infer_with_answer_shape(query: &str, answer_shape: AnswerShape) -> Self {
        infer_plan(query, Some(answer_shape))
    }
}

/// Source-aware selection applied before normal package validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceSelection {
    /// Choose the policy from the deterministic [`RecallIntent`].
    ///
    /// Enumeration and explicit relationship questions use
    /// [`SourceSessionCoverage`](Self::SourceSessionCoverage), grounded
    /// inference uses [`SourceCoverage`](Self::SourceCoverage), frequency
    /// questions use source-session coverage, and direct/date questions
    /// preserve relevance order.
    #[default]
    Auto,
    /// Preserve the supplied ranking byte-for-byte.
    Relevance,
    /// Keep only the highest-ranked representation of an identical raw-source set.
    DistinctSources,
    /// Keep a candidate only when it contributes at least one raw source that
    /// higher-ranked candidates have not covered, then backfill from later candidates.
    SourceCoverage,
    /// Preserve a bounded burst of evidence from each source session before
    /// backfilling additional candidates from already saturated sessions.
    ///
    /// This protects multi-event and multi-hop questions from spending their
    /// entire evidence budget on overlapping turns from one conversation
    /// without discarding a small same-session evidence chain.
    SourceSessionCoverage,
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

/// Options for the canonical production reranked-recall pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RerankedRecallOptions {
    /// Number of cognitive readout candidates compiled into evidence documents.
    pub candidate_limit: usize,
    /// Cognitive search/package width before local reranking.
    ///
    /// This remains at least 20 by default even when a caller requests only a
    /// few final hits, keeping candidate generation identical to the evaluated
    /// product profile.
    pub search_limit: usize,
    /// Final source-aware selection and package options.
    pub deep: DeepRecallOptions,
    /// Optional graph scope applied during cognitive search, before reranking.
    pub scope: Option<ScopePath>,
}

impl RerankedRecallOptions {
    /// Build the latency-sensitive profile used by the MCP product path and benchmarks.
    ///
    /// Cognitive search uses at least 20 seeds/results and exposes up to 20
    /// evidence documents to the local reranker; the final package is capped at
    /// `limit`.
    pub fn new(limit: usize) -> Self {
        Self {
            candidate_limit: 20,
            search_limit: limit.max(20),
            deep: DeepRecallOptions::new(limit),
            scope: None,
        }
    }

    /// Override the reranker candidate pool width.
    pub fn with_candidate_limit(mut self, candidate_limit: usize) -> Self {
        self.candidate_limit = candidate_limit;
        self
    }

    /// Override the cognitive search width independently of the final hit cap.
    pub fn with_search_limit(mut self, search_limit: usize) -> Self {
        self.search_limit = search_limit;
        self
    }

    /// Override the source-aware evidence-selection policy.
    pub fn with_selection(mut self, selection: EvidenceSelection) -> Self {
        self.deep.selection = selection;
        self
    }

    /// Restrict cognitive search and all downstream reranking to `scope`.
    pub fn with_scope(mut self, scope: ScopePath) -> Self {
        self.scope = Some(scope);
        self
    }
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

fn infer_plan(query: &str, answer_shape_hint: Option<AnswerShape>) -> RecallPlan {
    let normalized = query.trim().to_lowercase();
    let words: Vec<_> = normalized
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty())
        .collect();

    let has_word = |needle: &str| words.contains(&needle);
    let has_sequence = |needles: &[&str]| {
        !needles.is_empty() && words.windows(needles.len()).any(|window| window == needles)
    };
    let has_any_sequence =
        |sequences: &[&[&str]]| sequences.iter().any(|sequence| has_sequence(sequence));

    // These are locale rule packs rather than sentence prefixes: interrogative
    // phrases may occur anywhere in a polite wrapper or inverted question.
    const EN_TEMPORAL_TARGETS: &[&[&str]] = &[
        &["how", "long"],
        &["how", "often"],
        &["what", "date"],
        &["what", "day"],
        &["what", "month"],
        &["what", "week"],
        &["what", "year"],
        &["which", "date"],
        &["which", "day"],
        &["which", "month"],
        &["which", "week"],
        &["which", "year"],
    ];
    const EN_COUNT_TARGETS: &[&[&str]] = &[&["how", "many"], &["number", "of"]];
    const EN_COLLECTION_TARGETS: &[&[&str]] = &[
        &["list"],
        &["list", "all"],
        &["what", "are"],
        &["which", "are"],
    ];

    let requests_temporal_answer = has_word("when")
        || has_any_sequence(EN_TEMPORAL_TARGETS)
        || normalized.contains("언제")
        || normalized.contains("몇 년")
        || normalized.contains("몇년")
        || normalized.contains("몇 월")
        || normalized.contains("몇월")
        || normalized.contains("몇 주")
        || normalized.contains("몇주")
        || normalized.contains("며칠");
    let requests_count = has_any_sequence(EN_COUNT_TARGETS)
        || normalized.contains("몇 번")
        || normalized.contains("몇번")
        || normalized.contains("몇 개")
        || normalized.contains("몇개");
    let requests_plural_object = words
        .iter()
        .position(|word| matches!(*word, "what" | "which"))
        .is_some_and(|start| {
            let object_phrase: Vec<_> = words[start.saturating_add(1)..]
                .iter()
                .take_while(|word| {
                    !matches!(
                        **word,
                        "are"
                            | "can"
                            | "could"
                            | "did"
                            | "do"
                            | "does"
                            | "had"
                            | "has"
                            | "have"
                            | "is"
                            | "might"
                            | "should"
                            | "was"
                            | "will"
                            | "would"
                    )
                })
                .copied()
                .collect();
            !object_phrase.iter().any(|word| word.ends_with("'s"))
                && object_phrase.iter().any(|word| {
                    word.len() > 3 && word.ends_with('s') && !matches!(*word, "this" | "thus")
                })
        });
    let requests_collection = has_any_sequence(EN_COLLECTION_TARGETS)
        || has_word("all")
        || (matches!(words.as_slice(), ["what", "has" | "have", ..]) && has_word("done"))
        || requests_plural_object
        || normalized.contains("무엇들이")
        || normalized.contains("어떤 것들이");
    let requests_relationship =
        has_any_sequence(&[
            &["relationship", "between"],
            &["connection", "between"],
            &["in", "common"],
        ]) || matches!(words.as_slice(), ["how", "did" | "has" | "have", ..])
            || (has_word("where") && has_word("from") && (has_word("move") || has_word("moved")))
            || has_word("why")
            || has_word("both")
            || has_word("compare")
            || has_word("causes")
            || has_word("reasons")
            || normalized.contains("관계")
            || normalized.contains("공통")
            || normalized.contains("원인");
    let starts_yes_no_question = words.first().is_some_and(|word| {
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
    let requests_inference = has_word("likely")
        || has_word("could")
        || has_word("might")
        || has_word("would")
        || has_word("infer")
        || has_word("imply")
        || has_word("suggest")
        || has_any_sequence(&[
            &["what", "could"],
            &["how", "might"],
            &["how", "would"],
            &["what", "kind", "of", "person"],
        ])
        || starts_yes_no_question
        || normalized.contains("것 같")
        || normalized.contains("가능성이");

    let requests_frequency_answer = has_sequence(&["how", "often"]);
    let inferred_answer_shape = if requests_frequency_answer {
        AnswerShape::Frequency
    } else if requests_temporal_answer {
        AnswerShape::Temporal
    } else if requests_count {
        AnswerShape::Count
    } else if requests_collection {
        AnswerShape::Collection
    } else if requests_relationship {
        AnswerShape::Relationship
    } else if requests_inference {
        AnswerShape::Inference
    } else {
        AnswerShape::Fact
    };
    let answer_shape = answer_shape_hint.unwrap_or(inferred_answer_shape);

    let has_temporal_constraint =
        !crate::query::temporal::parse_time_cues(&normalized, 1_700_000_000_000).is_empty()
            || has_word("before")
            || has_word("after")
            || has_word("ago")
            || normalized.contains("지난주")
            || normalized.contains("지난달")
            || normalized.contains("작년");
    let recall_intent = if matches!(answer_shape, AnswerShape::Temporal | AnswerShape::Frequency)
        || has_temporal_constraint
    {
        RecallIntent::Temporal
    } else if matches!(answer_shape, AnswerShape::Count | AnswerShape::Collection) {
        RecallIntent::Enumeration
    } else if matches!(
        answer_shape,
        AnswerShape::Relationship | AnswerShape::Inference
    ) {
        RecallIntent::Relational
    } else {
        RecallIntent::Direct
    };

    RecallPlan {
        query: query.trim().to_owned(),
        recall_intent,
        answer_shape,
    }
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
                    "about"
                        | "after"
                        | "ago"
                        | "before"
                        | "could"
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
                        | "please"
                        | "remember"
                        | "tell"
                        | "the"
                        | "their"
                        | "this"
                        | "was"
                        | "week"
                        | "were"
                        | "what"
                        | "when"
                        | "which"
                        | "would"
                        | "year"
                        | "you"
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
    overlap >= query_terms.len().min(3)
}

pub(crate) fn compile_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    selection: EvidenceSelection,
    limit: usize,
) -> Result<Vec<RerankedCandidate>, Error> {
    let resolved_selection = match selection {
        EvidenceSelection::Auto => match plan.recall_intent {
            RecallIntent::Enumeration => EvidenceSelection::SourceSessionCoverage,
            RecallIntent::Relational if plan.answer_shape == AnswerShape::Inference => {
                EvidenceSelection::SourceCoverage
            }
            RecallIntent::Relational => EvidenceSelection::SourceSessionCoverage,
            RecallIntent::Temporal if plan.answer_shape == AnswerShape::Frequency => {
                EvidenceSelection::SourceSessionCoverage
            }
            RecallIntent::Direct | RecallIntent::Temporal => EvidenceSelection::Relevance,
        },
        explicit => explicit,
    };

    match resolved_selection {
        EvidenceSelection::Auto | EvidenceSelection::Relevance => Ok(ranking.to_vec()),
        EvidenceSelection::DistinctSources => distinct_source_ranking(storage, ranking),
        EvidenceSelection::SourceCoverage => source_coverage_ranking(storage, ranking),
        EvidenceSelection::SourceSessionCoverage => {
            source_session_coverage_ranking(storage, ranking, limit)
        }
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

fn temporal_successor<S: StorageAdapter>(
    storage: &S,
    source: NodeId,
) -> Result<Option<NodeId>, Error> {
    let source_session = &storage.get_node(source)?.origin.session_id;
    let mut successors = Vec::new();
    for edge_id in storage.edges_from(source) {
        let edge = storage.get_edge(*edge_id)?;
        if edge.edge_type == EdgeType::Temporal
            && storage.get_node(edge.target)?.node_type == KnowledgeType::Episodic
            && storage.get_node(edge.target)?.origin.session_id == *source_session
        {
            successors.push(edge.target);
        }
    }
    successors.sort_unstable();
    Ok(successors.into_iter().next())
}

fn source_session_coverage_ranking<S: StorageAdapter>(
    storage: &S,
    ranking: &[RerankedCandidate],
    limit: usize,
) -> Result<Vec<RerankedCandidate>, Error> {
    const MAX_PRIMARY_PER_SESSION: usize = 4;

    let mut covered_sources = HashSet::new();
    let mut session_counts = HashMap::new();
    let mut primary = Vec::with_capacity(ranking.len());
    let mut deferred = Vec::new();

    for candidate in ranking {
        let sources = canonical_sources(storage, candidate.node_id)?;
        let new_sources: Vec<_> = sources
            .into_iter()
            .filter(|source| !covered_sources.contains(source))
            .collect();
        if new_sources.is_empty() {
            continue;
        }
        covered_sources.extend(new_sources.iter().copied());

        let mut sessions = Vec::new();
        for source_id in &new_sources {
            let session_id = storage.get_node(*source_id)?.origin.session_id.clone();
            if !sessions.contains(&session_id) {
                sessions.push(session_id);
            }
        }
        if sessions.iter().any(|session| {
            session_counts.get(session).copied().unwrap_or_default() < MAX_PRIMARY_PER_SESSION
        }) {
            for session in sessions {
                *session_counts.entry(session).or_insert(0usize) += 1;
            }
            primary.push(*candidate);
        } else {
            deferred.push(*candidate);
        }
    }

    if primary.len() < limit {
        primary.extend(deferred.into_iter().take(limit - primary.len()));
    }
    primary.truncate(limit);
    Ok(primary)
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

pub(crate) fn compile_evidence_documents<S: StorageAdapter>(
    storage: &S,
    ranking: &[crate::query::ReadoutCandidate],
    limit: usize,
) -> Result<Vec<EvidenceDocument>, Error> {
    let candidate_surface: HashSet<_> = ranking
        .iter()
        .take(limit)
        .map(|candidate| candidate.node_id)
        .collect();
    let mut covered_sources = HashSet::new();
    let mut documents = Vec::new();
    let mut document_by_node = HashMap::new();

    for candidate in ranking.iter().take(limit) {
        let candidate_sources = canonical_sources(storage, candidate.node_id)?;
        let new_sources: Vec<_> = candidate_sources
            .into_iter()
            .filter(|source| covered_sources.insert(*source))
            .collect();
        if new_sources.is_empty() {
            continue;
        }

        let mut fallback_sources = Vec::new();
        for source_id in new_sources {
            if candidate_surface.contains(&source_id) {
                let text = render_source(storage, source_id)?;
                let index = documents.len();
                documents.push(EvidenceDocument {
                    node_id: source_id,
                    source_node_ids: vec![source_id],
                    text,
                });
                document_by_node.insert(source_id, index);
            } else {
                fallback_sources.push(source_id);
            }
        }
        if fallback_sources.is_empty() {
            continue;
        }
        let fallback_text = fallback_sources
            .iter()
            .map(|source_id| render_source(storage, *source_id))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        if let Some(index) = document_by_node.get(&candidate.node_id).copied() {
            let document = &mut documents[index];
            document.source_node_ids.extend(fallback_sources);
            if !fallback_text.is_empty() {
                if !document.text.is_empty() {
                    document.text.push('\n');
                }
                document.text.push_str(&fallback_text);
            }
        } else {
            let text = if fallback_text.trim().is_empty() {
                storage.get_node(candidate.node_id)?.content.clone()
            } else {
                fallback_text
            };
            let index = documents.len();
            documents.push(EvidenceDocument {
                node_id: candidate.node_id,
                source_node_ids: fallback_sources,
                text,
            });
            document_by_node.insert(candidate.node_id, index);
        }
    }

    Ok(documents)
}

fn compile_inference_documents<S: StorageAdapter>(
    storage: &S,
    ranking: &[crate::query::ReadoutCandidate],
    limit: usize,
) -> Result<Vec<EvidenceDocument>, Error> {
    let candidates = &ranking[..ranking.len().min(limit)];
    let candidate_surface: HashSet<_> = candidates
        .iter()
        .map(|candidate| candidate.node_id)
        .collect();
    let mut semantically_represented_sources = HashSet::new();
    for candidate in candidates {
        let node = storage.get_node(candidate.node_id)?;
        if node.node_type == KnowledgeType::Semantic {
            semantically_represented_sources.extend(canonical_sources(storage, candidate.node_id)?);
        }
    }

    let mut seen_source_sets = HashSet::new();
    let mut represented_nodes = HashSet::new();
    let mut documents = Vec::new();
    for candidate in candidates {
        let node = storage.get_node(candidate.node_id)?;
        if node.node_type == KnowledgeType::Episodic
            && semantically_represented_sources.contains(&candidate.node_id)
        {
            continue;
        }

        let mut representative = candidate.node_id;
        let mut source_node_ids = canonical_sources(storage, candidate.node_id)?;
        if node.node_type == KnowledgeType::Semantic
            && let Some(last_source) = source_node_ids.last().copied()
            && storage
                .get_node(last_source)?
                .content
                .trim_end()
                .ends_with('?')
            && let Some(next_source) = temporal_successor(storage, last_source)?
            && candidate_surface.contains(&next_source)
        {
            representative = next_source;
            source_node_ids.push(next_source);
            source_node_ids.sort_unstable();
            source_node_ids.dedup();
        }
        if !seen_source_sets.insert(source_node_ids.clone()) {
            continue;
        }
        if !represented_nodes.insert(representative) {
            continue;
        }
        let text = source_node_ids
            .iter()
            .map(|source_id| render_source(storage, *source_id))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        documents.push(EvidenceDocument {
            node_id: representative,
            source_node_ids,
            text,
        });
    }
    Ok(documents)
}

pub(crate) fn compile_rerank_documents<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[crate::query::ReadoutCandidate],
    limit: usize,
) -> Result<Vec<EvidenceDocument>, Error> {
    if plan.answer_shape == AnswerShape::Inference {
        return compile_inference_documents(storage, ranking, limit);
    }
    if plan.answer_shape == AnswerShape::Frequency {
        return compile_evidence_documents(storage, ranking, limit);
    }
    if matches!(
        plan.recall_intent,
        RecallIntent::Enumeration | RecallIntent::Relational
    ) {
        return compile_evidence_documents(storage, ranking, limit);
    }

    ranking
        .iter()
        .take(limit)
        .map(|candidate| {
            let node = storage.get_node(candidate.node_id)?;
            Ok(EvidenceDocument {
                node_id: candidate.node_id,
                source_node_ids: canonical_sources(storage, candidate.node_id)?,
                text: node.content.clone(),
            })
        })
        .collect()
}

fn render_source<S: StorageAdapter>(storage: &S, source_id: NodeId) -> Result<String, Error> {
    let source = storage.get_node(source_id)?;
    let (speaker, _) = parse_entity_tags(&source.entity_tags);
    Ok(speaker.map_or_else(
        || source.content.clone(),
        |speaker| format!("{speaker}: {}", source.content),
    ))
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
            RecallPlan::infer("How many times did Alice move?").recall_intent,
            RecallIntent::Enumeration
        );
        assert_eq!(
            RecallPlan::infer("When did Alice move?").recall_intent,
            RecallIntent::Temporal
        );
        assert_eq!(
            RecallPlan::infer("What is the relationship between Alice and Bob?").recall_intent,
            RecallIntent::Relational
        );
        assert_eq!(
            RecallPlan::infer("Where does Alice live?").recall_intent,
            RecallIntent::Direct
        );
    }

    #[test]
    fn temporal_precedence_prevents_coverage_reordering() {
        assert_eq!(
            RecallPlan::infer("When did Alice and Bob meet?").recall_intent,
            RecallIntent::Temporal
        );
    }

    #[test]
    fn separates_temporal_retrieval_from_answer_shape() {
        assert_eq!(
            RecallPlan::infer("When did Alice move?").answer_shape,
            AnswerShape::Temporal
        );
        assert_eq!(
            RecallPlan::infer("Could you tell me which week Alice moved?").answer_shape,
            AnswerShape::Temporal
        );
        let constrained = RecallPlan::infer("Where did Alice move four years ago?");
        assert_eq!(constrained.recall_intent, RecallIntent::Temporal);
        assert_eq!(constrained.answer_shape, AnswerShape::Fact);
        let dated = RecallPlan::infer("Which activity did Alice pursue on 5 June 2023?");
        assert_eq!(dated.recall_intent, RecallIntent::Temporal);
        assert_eq!(dated.answer_shape, AnswerShape::Fact);
    }

    #[test]
    fn detects_answer_shapes_beyond_sentence_prefixes() {
        assert_eq!(
            RecallPlan::infer("Do you remember when Alice moved?").answer_shape,
            AnswerShape::Temporal
        );
        assert_eq!(
            RecallPlan::infer("Alice moved on which date?").answer_shape,
            AnswerShape::Temporal
        );
        assert_eq!(
            RecallPlan::infer("John은 몇번 이사했어?").answer_shape,
            AnswerShape::Count
        );
        assert_eq!(
            RecallPlan::infer("Please list every city Alice visited.").answer_shape,
            AnswerShape::Collection
        );
        assert_eq!(
            RecallPlan::infer("What kind of games has James developed?").answer_shape,
            AnswerShape::Collection
        );
        assert_eq!(
            RecallPlan::infer("What personal health incidents does Evan face?").answer_shape,
            AnswerShape::Collection
        );
        assert_eq!(
            RecallPlan::infer("What kind of car does Evan drive?").answer_shape,
            AnswerShape::Fact
        );
        assert_eq!(
            RecallPlan::infer("Which popular music composer's tunes does Tim enjoy?").answer_shape,
            AnswerShape::Fact
        );
        assert_eq!(
            RecallPlan::infer("Would Dana want to move home soon?").answer_shape,
            AnswerShape::Inference
        );
        assert_eq!(
            RecallPlan::infer("What might Alice do next?").answer_shape,
            AnswerShape::Inference
        );
        assert_eq!(
            RecallPlan::infer("Could you tell me when Alice moved?").answer_shape,
            AnswerShape::Temporal
        );
        assert_eq!(
            RecallPlan::infer("How long did the restoration take?").answer_shape,
            AnswerShape::Temporal
        );
        let frequency = RecallPlan::infer("How often does Quinn get health checkups?");
        assert_eq!(frequency.answer_shape, AnswerShape::Frequency);
        assert_eq!(frequency.recall_intent, RecallIntent::Temporal);
        assert_eq!(
            RecallPlan::infer("Why did Alice move?").answer_shape,
            AnswerShape::Relationship
        );
        let manner = RecallPlan::infer("How did Nora promote her clothes store?");
        assert_eq!(manner.answer_shape, AnswerShape::Relationship);
        assert_eq!(manner.recall_intent, RecallIntent::Relational);
        let origin = RecallPlan::infer("Where did Dana move from 4 years ago?");
        assert_eq!(origin.answer_shape, AnswerShape::Relationship);
        assert_eq!(origin.recall_intent, RecallIntent::Temporal);
        let completed = RecallPlan::infer("What has Andrew done with his dogs?");
        assert_eq!(completed.answer_shape, AnswerShape::Collection);
        assert_eq!(completed.recall_intent, RecallIntent::Enumeration);
        let yes_no = RecallPlan::infer("Does James live in Connecticut?");
        assert_eq!(yes_no.answer_shape, AnswerShape::Inference);
        assert_eq!(yes_no.recall_intent, RecallIntent::Relational);
        let gift = RecallPlan::infer(
            "What electronic device could Rowan gift Quinn to help with his fitness goals?",
        );
        assert_eq!(gift.answer_shape, AnswerShape::Inference);
        assert_eq!(gift.recall_intent, RecallIntent::Relational);
        assert_eq!(
            RecallPlan::infer(
                "Which US states might Tim visit based on his Universal Studios plans?"
            )
            .answer_shape,
            AnswerShape::Collection
        );
    }

    #[test]
    fn typed_answer_shape_hint_keeps_temporal_query_constraints() {
        let plan = RecallPlan::infer_with_answer_shape(
            "What happened last week?",
            AnswerShape::Collection,
        );
        assert_eq!(plan.answer_shape, AnswerShape::Collection);
        assert_eq!(plan.recall_intent, RecallIntent::Temporal);
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
