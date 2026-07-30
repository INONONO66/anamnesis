//! Deterministic evidence readout planning for the [`Memory`](super::Memory) facade.
//!
//! The kernel ranks graph nodes. This module turns that node ranking into a
//! source-aware evidence ranking without calling a generative model. Raw
//! Episodic fragments remain the canonical evidence units; Semantic windows and
//! reviewed derived knowledge are representations attached to those units.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::Error;
use crate::graph::{EdgeType, KnowledgeType, NodeId, ScopePath};
use crate::mechanics::attraction::cosine_similarity;
use crate::storage::{AtomicFactId, StorageAdapter};

use super::{RerankedCandidate, parse_entity_tags};

/// Canonical latency-sensitive candidate width for production reranked recall.
///
/// This is intentionally independent of the final evidence limit. The local
/// reranker sees a broader evidence surface, while callers retain control over
/// how many selected memories enter their context.
pub const DEFAULT_RERANK_CANDIDATE_LIMIT: usize = 50;

/// Canonical cognitive-search width for production reranked recall.
///
/// Keep this independent of the final context limit so a small delivered
/// context does not silently narrow the evidence surface before reranking.
pub const DEFAULT_RERANK_SEARCH_LIMIT: usize = 20;

/// Canonical final evidence width for quality-oriented product recall.
///
/// A caller can still request a smaller package explicitly. The shared
/// default preserves the multi-hop evidence that local-reader screening lost
/// at final widths of eight and twelve.
pub const DEFAULT_RERANK_FINAL_LIMIT: usize = 20;

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
    /// Direct queries preserve reranker order so post-package product filters
    /// can retain alternate knowledge representations. Inference and date
    /// queries remove candidates that contribute no new canonical raw evidence.
    /// Enumeration, relationship, and frequency queries additionally preserve
    /// source-session diversity.
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
    /// few final hits. The broader reranker candidate surface is retained in
    /// diagnostics independently of this package width.
    pub search_limit: usize,
    /// Final source-aware selection and package options.
    pub deep: DeepRecallOptions,
    /// Optional graph scope applied during cognitive search, before reranking.
    pub scope: Option<ScopePath>,
}

impl RerankedRecallOptions {
    /// Build the latency-sensitive profile used by the MCP product path and benchmarks.
    ///
    /// Cognitive search uses at least 20 seeds/results and exposes up to
    /// [`DEFAULT_RERANK_CANDIDATE_LIMIT`] evidence documents to the local
    /// reranker; the final package is capped at `limit`.
    pub fn new(limit: usize) -> Self {
        Self {
            candidate_limit: DEFAULT_RERANK_CANDIDATE_LIMIT,
            search_limit: limit.max(DEFAULT_RERANK_SEARCH_LIMIT),
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
            RecallIntent::Direct => EvidenceSelection::Relevance,
            RecallIntent::Temporal => EvidenceSelection::SourceCoverage,
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
    // Preserve room for at least one other source session at small final
    // widths, while allowing a larger evidence chain from one session when the
    // caller has explicitly budgeted a broader context.
    let max_primary_per_session = limit.saturating_mul(4).div_ceil(5).clamp(2, 10);

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
            session_counts.get(session).copied().unwrap_or_default() < max_primary_per_session
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

#[derive(Debug)]
struct PreselectionCandidate {
    ranking_index: usize,
    node_type: KnowledgeType,
    source_node_ids: Vec<NodeId>,
    source_sessions: Vec<String>,
    query_facets: HashSet<String>,
    embedding_cosine: f64,
    atomic_bridge: Option<TemporalBridgeSignal>,
}

fn normalize_facet_term(term: &str) -> String {
    let term = term.trim_matches('\'');
    match term {
        "authors" => "author".to_owned(),
        "books" => "book".to_owned(),
        "cities" => "city".to_owned(),
        "developed" | "developing" => "develop".to_owned(),
        "games" => "game".to_owned(),
        "moved" | "moving" => "move".to_owned(),
        "planned" | "planning" => "plan".to_owned(),
        "states" => "state".to_owned(),
        "visited" | "visiting" => "visit".to_owned(),
        _ if term.len() > 5 && term.ends_with("ies") => {
            format!("{}y", &term[..term.len() - 3])
        }
        _ if term.len() > 5 && term.ends_with("ing") => term[..term.len() - 3].to_owned(),
        _ if term.len() > 4 && term.ends_with("ed") => term[..term.len() - 2].to_owned(),
        _ if term.len() > 4 && term.ends_with('s') => term[..term.len() - 1].to_owned(),
        _ => term.to_owned(),
    }
}

fn facet_terms(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|term| term.len() > 2)
        .map(str::to_lowercase)
        .filter(|term| {
            !matches!(
                term.as_str(),
                "about"
                    | "all"
                    | "also"
                    | "and"
                    | "are"
                    | "based"
                    | "been"
                    | "between"
                    | "can"
                    | "connection"
                    | "could"
                    | "did"
                    | "does"
                    | "done"
                    | "for"
                    | "from"
                    | "had"
                    | "has"
                    | "have"
                    | "how"
                    | "infer"
                    | "kind"
                    | "likely"
                    | "list"
                    | "might"
                    | "please"
                    | "relationship"
                    | "remember"
                    | "should"
                    | "suggest"
                    | "tell"
                    | "that"
                    | "the"
                    | "their"
                    | "them"
                    | "then"
                    | "there"
                    | "these"
                    | "they"
                    | "this"
                    | "those"
                    | "was"
                    | "were"
                    | "what"
                    | "when"
                    | "where"
                    | "which"
                    | "who"
                    | "why"
                    | "will"
                    | "with"
                    | "would"
                    | "you"
            )
        })
        .map(|term| normalize_facet_term(&term))
        .filter(|term| term.len() > 2)
        .collect()
}

fn source_sessions<S: StorageAdapter>(
    storage: &S,
    source_node_ids: &[NodeId],
) -> Result<Vec<String>, Error> {
    let mut sessions = Vec::new();
    for source_node_id in source_node_ids {
        let session = storage.get_node(*source_node_id)?.origin.session_id.clone();
        if !sessions.contains(&session) {
            sessions.push(session);
        }
    }
    Ok(sessions)
}

fn candidate_facet_terms<S: StorageAdapter>(
    storage: &S,
    node_id: NodeId,
    source_node_ids: &[NodeId],
    query_facets: &HashSet<String>,
) -> Result<HashSet<String>, Error> {
    let mut terms = facet_terms(&storage.get_node(node_id)?.content);
    for source_node_id in source_node_ids {
        terms.extend(facet_terms(&storage.get_node(*source_node_id)?.content));
    }
    Ok(query_facets.intersection(&terms).cloned().collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TemporalBridgeSignal {
    kind_priority: usize,
    seed_rank: usize,
    distance: usize,
    backward_hops: usize,
}

fn bridge_signal_is_better(candidate: TemporalBridgeSignal, current: TemporalBridgeSignal) -> bool {
    candidate.kind_priority > current.kind_priority
        || (candidate.kind_priority == current.kind_priority
            && (candidate.distance < current.distance
                || (candidate.distance == current.distance
                    && (candidate.backward_hops < current.backward_hops
                        || (candidate.backward_hops == current.backward_hops
                            && candidate.seed_rank < current.seed_rank)))))
}

fn temporal_bridge_signals<S: StorageAdapter>(
    storage: &S,
    routed_atomic_sources: &[(NodeId, usize)],
    max_hops: usize,
) -> Result<HashMap<NodeId, TemporalBridgeSignal>, Error> {
    let mut signals = HashMap::new();
    let mut queue = VecDeque::new();
    let mut seen_seeds = HashSet::new();
    for (seed_rank, &(source_node_id, kind_priority)) in routed_atomic_sources.iter().enumerate() {
        if !seen_seeds.insert(source_node_id) {
            continue;
        }
        let source = storage.get_node(source_node_id)?;
        if source.node_type != KnowledgeType::Episodic {
            continue;
        }
        let signal = TemporalBridgeSignal {
            // Only recurring conventions warrant directional expansion ahead
            // of ordinary semantic relevance. Other typed facts still route
            // their exact raw sources through the atomic lane.
            kind_priority: usize::from(kind_priority == 3),
            seed_rank,
            distance: 0,
            backward_hops: 0,
        };
        signals.insert(source_node_id, signal);
        queue.push_back((source_node_id, source.origin.session_id.clone(), signal));
    }

    while let Some((node_id, source_session, signal)) = queue.pop_front() {
        if signals.get(&node_id) != Some(&signal) || signal.distance >= max_hops {
            continue;
        }

        let mut neighbors = Vec::new();
        for &edge_id in storage.edges_from(node_id) {
            let edge = storage.get_edge(edge_id)?;
            if edge.edge_type == EdgeType::Temporal {
                neighbors.push((edge.target, false));
            }
        }
        for &edge_id in storage.edges_to(node_id) {
            let edge = storage.get_edge(edge_id)?;
            if edge.edge_type == EdgeType::Temporal {
                neighbors.push((edge.source, true));
            }
        }
        neighbors.sort_unstable();
        neighbors.dedup();

        for (neighbor_id, traversed_backward) in neighbors {
            let neighbor = storage.get_node(neighbor_id)?;
            if neighbor.node_type != KnowledgeType::Episodic
                || neighbor.origin.session_id != source_session
            {
                continue;
            }
            let next_signal = TemporalBridgeSignal {
                distance: signal.distance + 1,
                backward_hops: signal.backward_hops + usize::from(traversed_backward),
                ..signal
            };
            let should_update = signals
                .get(&neighbor_id)
                .is_none_or(|current| bridge_signal_is_better(next_signal, *current));
            if should_update {
                signals.insert(neighbor_id, next_signal);
                queue.push_back((neighbor_id, source_session.clone(), next_signal));
            }
        }
    }

    Ok(signals)
}

#[derive(Debug)]
struct AtomicFactCandidate {
    fact_id: AtomicFactId,
    dense_score: f64,
    lexical_overlap: usize,
    entity_matches: usize,
    kind_priority: usize,
}

pub(super) struct RoutedAtomicSource {
    pub candidate: crate::query::ReadoutCandidate,
    pub kind_priority: usize,
}

fn normalized_phrase(value: &str) -> String {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn selective_entity_matches(query: &str, entity_tags: &[String]) -> usize {
    let normalized_query = normalized_phrase(query);
    entity_tags
        .iter()
        .filter(|tag| {
            !tag.starts_with("anamnesis:")
                && !tag.starts_with("session-")
                && !tag.starts_with("speaker-")
        })
        .filter(|tag| {
            let normalized_tag = normalized_phrase(tag);
            normalized_tag.len() > 2 && normalized_query.contains(&normalized_tag)
        })
        .count()
}

fn inference_fact_kind_priority(plan: &RecallPlan, metadata: &HashMap<String, String>) -> usize {
    if plan.answer_shape != AnswerShape::Inference {
        return 0;
    }
    let kind = metadata
        .get("anamnesis:fact-kind")
        .or_else(|| metadata.get("anamnesis:benchmark-derived-kind"))
        .map(|value| value.trim().to_lowercase());
    match kind.as_deref() {
        Some("convention") => 3,
        Some("preference") => 2,
        Some("causal" | "decision" | "lesson") => 1,
        _ => 0,
    }
}

fn uses_complex_expansion(plan: &RecallPlan) -> bool {
    if !matches!(
        plan.answer_shape,
        AnswerShape::Collection | AnswerShape::Relationship | AnswerShape::Inference
    ) {
        return false;
    }

    let normalized_query = plan.query.trim().to_lowercase();
    let padded_query = format!(" {normalized_query} ");
    let requests_causal_explanation = normalized_query
        .split(|character: char| !character.is_alphanumeric())
        .find(|term| !term.is_empty())
        .is_some_and(|term| term == "why")
        || normalized_query.contains("왜");
    // Gift/device questions are intentionally conservative. The relevant
    // memory usually describes a need rather than the external-world product
    // that satisfies it, so broad fact expansion can displace the exact need
    // evidence before a reader supplies the short commonsense bridge. Advice
    // and other inference questions still benefit from the deeper lane.
    let requests_gift_recommendation =
        plan.answer_shape == AnswerShape::Inference && padded_query.contains(" gift ");

    !requests_causal_explanation && !requests_gift_recommendation
}

fn add_atomic_rrf_scores(
    ranked: impl IntoIterator<Item = AtomicFactId>,
    scores: &mut HashMap<AtomicFactId, f64>,
) {
    const RRF_K: f64 = 60.0;
    for (rank, fact_id) in ranked.into_iter().enumerate() {
        *scores.entry(fact_id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
}

pub(super) fn route_atomic_fact_sources<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    query_embedding: &[f64],
    now: crate::graph::Timestamp,
    scope: &ScopePath,
) -> Result<Vec<RoutedAtomicSource>, Error> {
    if !uses_complex_expansion(plan) {
        return Ok(Vec::new());
    }
    let fact_limit = match plan.answer_shape {
        AnswerShape::Collection => 16,
        AnswerShape::Relationship => 12,
        AnswerShape::Inference => 16,
        _ => return Ok(Vec::new()),
    };
    let atomic_fact_ids = storage.all_atomic_fact_ids();
    if atomic_fact_ids.is_empty() {
        return Ok(Vec::new());
    }

    let query_terms = facet_terms(&plan.query);
    let mut facts = Vec::with_capacity(atomic_fact_ids.len());
    for fact_id in atomic_fact_ids {
        let fact = storage.get_atomic_fact(fact_id)?;
        if fact
            .metadata
            .get("retracted")
            .is_some_and(|value| value == "true")
            || !crate::graph::valid_at(fact.valid_from, fact.valid_until, now)
            || crate::query::scoring::scope_weight(scope, &fact.scope) <= 0.0
        {
            continue;
        }
        let dense_score = cosine_similarity(query_embedding, &fact.embedding);
        let lexical_overlap = query_terms
            .intersection(&facet_terms(&fact.content))
            .count();
        let entity_matches = selective_entity_matches(&plan.query, &fact.entity_tags);
        let kind_priority = inference_fact_kind_priority(plan, &fact.metadata);
        if dense_score > 0.0 || lexical_overlap > 0 || entity_matches > 0 {
            facts.push(AtomicFactCandidate {
                fact_id,
                dense_score,
                lexical_overlap,
                entity_matches,
                kind_priority,
            });
        }
    }

    const LANE_DEPTH: usize = 64;
    let mut dense: Vec<_> = facts.iter().collect();
    dense.sort_by(|left, right| {
        right
            .dense_score
            .total_cmp(&left.dense_score)
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut lexical: Vec<_> = facts
        .iter()
        .filter(|fact| fact.lexical_overlap > 0)
        .collect();
    lexical.sort_by(|left, right| {
        right
            .lexical_overlap
            .cmp(&left.lexical_overlap)
            .then_with(|| right.dense_score.total_cmp(&left.dense_score))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut entities: Vec<_> = facts
        .iter()
        .filter(|fact| fact.entity_matches > 0)
        .collect();
    entities.sort_by(|left, right| {
        right
            .entity_matches
            .cmp(&left.entity_matches)
            .then_with(|| right.dense_score.total_cmp(&left.dense_score))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut inference_kinds: Vec<_> = facts
        .iter()
        .filter(|fact| {
            fact.kind_priority > 0 && (fact.entity_matches > 0 || fact.lexical_overlap > 0)
        })
        .collect();
    inference_kinds.sort_by(|left, right| {
        right
            .kind_priority
            .cmp(&left.kind_priority)
            .then_with(|| right.entity_matches.cmp(&left.entity_matches))
            .then_with(|| right.dense_score.total_cmp(&left.dense_score))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });

    let mut fused = HashMap::new();
    add_atomic_rrf_scores(
        dense.iter().take(LANE_DEPTH).map(|fact| fact.fact_id),
        &mut fused,
    );
    add_atomic_rrf_scores(
        lexical.iter().take(LANE_DEPTH).map(|fact| fact.fact_id),
        &mut fused,
    );
    add_atomic_rrf_scores(
        entities.iter().take(LANE_DEPTH).map(|fact| fact.fact_id),
        &mut fused,
    );
    add_atomic_rrf_scores(
        inference_kinds
            .iter()
            .take(LANE_DEPTH)
            .map(|fact| fact.fact_id),
        &mut fused,
    );
    let dense_by_id: HashMap<_, _> = facts
        .iter()
        .map(|fact| (fact.fact_id, fact.dense_score))
        .collect();
    let kind_priority_by_id: HashMap<_, _> = facts
        .iter()
        .map(|fact| (fact.fact_id, fact.kind_priority))
        .collect();
    // Hypothetical/preference questions need stable behavioral evidence more
    // than another semantically similar event. Reserve a small typed lane
    // before session balancing so recurring conventions (and then explicit
    // preferences) are not discarded merely because the same conversation
    // also yielded a slightly denser generic fact.
    const INFERENCE_KIND_QUOTA: usize = 4;
    let inference_kind_rank: HashMap<_, _> = inference_kinds
        .iter()
        .take(INFERENCE_KIND_QUOTA)
        .enumerate()
        .map(|(rank, fact)| (fact.fact_id, rank))
        .collect();
    let mut ranked_facts: Vec<_> = fused.into_iter().collect();
    ranked_facts.sort_by(|(left_id, left_score), (right_id, right_score)| {
        match (
            inference_kind_rank.get(left_id),
            inference_kind_rank.get(right_id),
        ) {
            (Some(left_rank), Some(right_rank)) => left_rank.cmp(right_rank),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => right_score.total_cmp(left_score),
        }
        .then_with(|| {
            dense_by_id
                .get(right_id)
                .copied()
                .unwrap_or_default()
                .total_cmp(&dense_by_id.get(left_id).copied().unwrap_or_default())
        })
        .then_with(|| left_id.cmp(right_id))
    });
    // A single conversation can yield many near-duplicate facts about the same
    // person or theme. Keep the fact lane session-diverse before backfilling so
    // collection/multi-hop queries bridge events and open-domain inference can
    // recover a useful premise from a less lexically obvious conversation.
    let per_session_limit = match plan.answer_shape {
        AnswerShape::Inference => 2,
        AnswerShape::Collection | AnswerShape::Relationship => 2,
        _ => fact_limit,
    };
    let mut session_counts = HashMap::new();
    let mut session_diverse = Vec::with_capacity(fact_limit);
    let mut deferred = Vec::new();
    for ranked_fact in ranked_facts {
        let session = storage
            .get_atomic_fact(ranked_fact.0)?
            .source_session_id
            .clone();
        let count = session_counts.entry(session).or_insert(0usize);
        if *count < per_session_limit && session_diverse.len() < fact_limit {
            *count += 1;
            session_diverse.push(ranked_fact);
        } else {
            deferred.push(ranked_fact);
        }
    }
    if session_diverse.len() < fact_limit {
        session_diverse.extend(
            deferred
                .into_iter()
                .take(fact_limit - session_diverse.len()),
        );
    }
    let ranked_facts = session_diverse;
    let max_fused = ranked_facts
        .first()
        .map(|(_, score)| *score)
        .unwrap_or(1.0)
        .max(f64::EPSILON);

    let mut seen_sources = HashSet::new();
    let mut routed = Vec::new();
    let live_node_ids: HashSet<_> = storage.all_node_ids().into_iter().collect();
    // The trace can retain multiple raw provenance rows per selected fact, but
    // the caller controls how many are promoted into the latency-sensitive
    // document head. Keep the auxiliary lane bounded by the 20-row production
    // tail even when a fact cites several turns.
    let source_limit = fact_limit.saturating_mul(2).min(20);
    for (fact_id, fused_score) in ranked_facts {
        let fact = storage.get_atomic_fact(fact_id)?;
        for source_id in fact.source_node_ids.iter().copied() {
            if routed.len() >= source_limit {
                return Ok(routed);
            }
            if !seen_sources.insert(source_id) {
                continue;
            }
            // Source deletion can race or outlive a reviewed sidecar record.
            // Treat that fact as stale provenance instead of failing the whole
            // product recall; no sidecar text is ever returned on its own.
            if !live_node_ids.contains(&source_id) {
                continue;
            }
            let source = storage.get_node(source_id)?;
            if source
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
                || !crate::graph::valid_at(source.valid_from, source.valid_until, now)
                || crate::query::scoring::scope_weight(scope, &source.origin.scope) <= 0.0
            {
                continue;
            }
            let embedding_cosine = source.embedding.as_ref().map_or(0.0, |embedding| {
                cosine_similarity(query_embedding, embedding)
            });
            let activation = (fused_score / max_fused).clamp(f64::EPSILON, 1.0);
            routed.push(RoutedAtomicSource {
                candidate: crate::query::ReadoutCandidate {
                    node_id: source_id,
                    score: fused_score,
                    activation,
                    phi: embedding_cosine,
                    embedding_cosine,
                    salience: storage.get_salience(source_id)?,
                    impedance: (-activation.ln()).max(0.0),
                    scope_weight: crate::query::scoring::scope_weight(scope, &source.origin.scope),
                    trust_weight: 1.0,
                    stress: 0.0,
                },
                kind_priority: kind_priority_by_id
                    .get(&fact_id)
                    .copied()
                    .unwrap_or_default(),
            });
        }
    }
    Ok(routed)
}

fn coverage_preselected_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[crate::query::ReadoutCandidate],
    limit: usize,
    routed_atomic_sources: &[(NodeId, usize)],
) -> Result<Vec<crate::query::ReadoutCandidate>, Error> {
    let inspected = ranking.len().min(limit);
    if !uses_complex_expansion(plan) || ranking.len() <= inspected || inspected < 2 {
        return Ok(ranking.iter().take(inspected).cloned().collect());
    }

    // At the production width of 50 this preserves the first 30 rows exactly.
    // Smaller explicit widths retain the same 3:2 head/deep ratio.
    let head_limit = inspected.saturating_mul(3).div_ceil(5).clamp(1, inspected);
    let query_facets = facet_terms(&plan.query);
    let bridge_signals = temporal_bridge_signals(storage, routed_atomic_sources, 4)?;
    let mut chosen_indices: HashSet<_> = (0..inspected).collect();
    let mut head_sessions = HashSet::new();
    let mut head_sources = HashSet::new();
    for candidate in ranking.iter().take(head_limit) {
        let sources = canonical_sources(storage, candidate.node_id)?;
        head_sources.extend(sources.iter().copied());
        head_sessions.extend(source_sessions(storage, &sources)?);
    }
    let mut tail = Vec::with_capacity(ranking.len().saturating_sub(head_limit));
    for (ranking_index, candidate) in ranking.iter().enumerate().skip(head_limit) {
        let source_node_ids = canonical_sources(storage, candidate.node_id)?;
        let atomic_bridge = source_node_ids
            .iter()
            .filter_map(|source_node_id| bridge_signals.get(source_node_id).copied())
            .filter(|signal| signal.distance > 0)
            .min_by(|left, right| {
                if bridge_signal_is_better(*left, *right) {
                    std::cmp::Ordering::Less
                } else if bridge_signal_is_better(*right, *left) {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            });
        tail.push(PreselectionCandidate {
            ranking_index,
            node_type: storage.get_node(candidate.node_id)?.node_type.clone(),
            source_sessions: source_sessions(storage, &source_node_ids)?,
            query_facets: candidate_facet_terms(
                storage,
                candidate.node_id,
                &source_node_ids,
                &query_facets,
            )?,
            source_node_ids,
            embedding_cosine: candidate.embedding_cosine,
            atomic_bridge,
        });
    }

    // Estimate query-facet rarity over the complete tail. This demotes
    // ubiquitous speaker names while preserving discriminating objects,
    // activities, places, and relations.
    let mut facet_frequency: HashMap<String, usize> = HashMap::new();
    for candidate in &tail {
        for facet in &candidate.query_facets {
            *facet_frequency.entry(facet.clone()).or_default() += 1;
        }
    }
    let rare_threshold = tail.len().div_ceil(4).max(1);
    let rare_facet_count = |candidate: &PreselectionCandidate| {
        candidate
            .query_facets
            .iter()
            .filter(|facet| {
                facet_frequency.get(*facet).copied().unwrap_or_default() <= rare_threshold
            })
            .count()
    };

    // A routed fact often identifies the correct conversation but cites a
    // premise or compressed extraction rather than the exact nearby response.
    // Admit at most two raw/session-window neighbors from the deeper trace.
    // The top 30 remains immutable, and only a weaker Semantic tail view can
    // be displaced, so direct evidence and ordinary ranked Episodic rows stay
    // protected.
    let mut bridge_candidates: Vec<_> = tail
        .iter()
        .filter(|candidate| {
            candidate.ranking_index >= inspected && candidate.atomic_bridge.is_some()
        })
        .collect();
    bridge_candidates.sort_by(|left, right| {
        right
            .atomic_bridge
            .map(|signal| signal.kind_priority)
            .unwrap_or_default()
            .cmp(
                &left
                    .atomic_bridge
                    .map(|signal| signal.kind_priority)
                    .unwrap_or_default(),
            )
            .then_with(|| {
                left.atomic_bridge
                    .map(|signal| {
                        if signal.kind_priority > 0 {
                            signal.backward_hops
                        } else {
                            0
                        }
                    })
                    .cmp(&right.atomic_bridge.map(|signal| {
                        if signal.kind_priority > 0 {
                            signal.backward_hops
                        } else {
                            0
                        }
                    }))
            })
            .then_with(|| rare_facet_count(right).cmp(&rare_facet_count(left)))
            .then_with(|| right.query_facets.len().cmp(&left.query_facets.len()))
            .then_with(|| right.embedding_cosine.total_cmp(&left.embedding_cosine))
            .then_with(|| {
                left.atomic_bridge
                    .map(|signal| signal.distance)
                    .cmp(&right.atomic_bridge.map(|signal| signal.distance))
            })
            .then_with(|| {
                left.atomic_bridge
                    .map(|signal| signal.seed_rank)
                    .cmp(&right.atomic_bridge.map(|signal| signal.seed_rank))
            })
            .then_with(|| left.ranking_index.cmp(&right.ranking_index))
    });

    let mut bridge_replacements = 0usize;
    for candidate in bridge_candidates {
        if bridge_replacements >= 2 {
            break;
        }
        let mut covered_sources = head_sources.clone();
        for selected in tail.iter().filter(|selected| {
            chosen_indices.contains(&selected.ranking_index)
                && selected.ranking_index != candidate.ranking_index
        }) {
            covered_sources.extend(selected.source_node_ids.iter().copied());
        }
        if !candidate
            .source_node_ids
            .iter()
            .any(|source| !covered_sources.contains(source))
        {
            continue;
        }

        let mut victims: Vec<_> = tail
            .iter()
            .filter(|victim| {
                victim.ranking_index < inspected
                    && chosen_indices.contains(&victim.ranking_index)
                    && victim.node_type == KnowledgeType::Semantic
                    && victim.atomic_bridge.is_none()
            })
            .collect();
        victims.sort_by(|left, right| {
            rare_facet_count(left)
                .cmp(&rare_facet_count(right))
                .then_with(|| left.query_facets.len().cmp(&right.query_facets.len()))
                .then_with(|| left.embedding_cosine.total_cmp(&right.embedding_cosine))
                .then_with(|| right.ranking_index.cmp(&left.ranking_index))
        });
        let Some(victim) = victims.first() else {
            break;
        };
        chosen_indices.remove(&victim.ranking_index);
        chosen_indices.insert(candidate.ranking_index);
        bridge_replacements += 1;
    }

    // Begin with the proven prefix and admit a deeper candidate only when it
    // has materially stronger query-facet evidence, or when it replaces a
    // canonically redundant tail view. This gives the deeper trace an
    // opportunity to recover missing facts without making diversity alone a
    // reason to discard a relevant rank-31..50 document.
    let mut deeper: Vec<_> = tail
        .iter()
        .filter(|candidate| candidate.ranking_index >= inspected)
        .collect();
    deeper.sort_by(|left, right| {
        rare_facet_count(right)
            .cmp(&rare_facet_count(left))
            .then_with(|| right.query_facets.len().cmp(&left.query_facets.len()))
            .then_with(|| {
                let left_bridge = left
                    .source_sessions
                    .iter()
                    .any(|session| head_sessions.contains(session));
                let right_bridge = right
                    .source_sessions
                    .iter()
                    .any(|session| head_sessions.contains(session));
                right_bridge.cmp(&left_bridge)
            })
            .then_with(|| right.source_node_ids.len().cmp(&left.source_node_ids.len()))
            .then_with(|| left.ranking_index.cmp(&right.ranking_index))
    });

    for candidate in deeper {
        if candidate.query_facets.is_empty() {
            continue;
        }
        let candidate_rare = rare_facet_count(candidate);
        let candidate_bridge = candidate
            .source_sessions
            .iter()
            .any(|session| head_sessions.contains(session));
        let mut selected_tail: Vec<_> = tail
            .iter()
            .filter(|selected| {
                selected.ranking_index < inspected
                    && selected.ranking_index >= head_limit
                    && chosen_indices.contains(&selected.ranking_index)
            })
            .collect();
        selected_tail.sort_by_key(|selected| std::cmp::Reverse(selected.ranking_index));

        let mut victim_index = None;
        for victim in selected_tail {
            // Raw Episodic rows are not interchangeable with an overlapping
            // Semantic window: local rerankers can strongly prefer the focused
            // turn even when both representations resolve to the same source.
            if victim.node_type == KnowledgeType::Episodic {
                continue;
            }
            let mut covered_without_victim = head_sources.clone();
            for selected in tail.iter().filter(|selected| {
                chosen_indices.contains(&selected.ranking_index)
                    && selected.ranking_index != victim.ranking_index
            }) {
                covered_without_victim.extend(selected.source_node_ids.iter().copied());
            }

            let candidate_adds_source = candidate
                .source_node_ids
                .iter()
                .any(|source| !covered_without_victim.contains(source));
            if !candidate_adds_source {
                continue;
            }

            let victim_rare = rare_facet_count(victim);
            let materially_stronger = candidate_rare > victim_rare
                || (candidate_rare == victim_rare
                    && candidate.query_facets.len() >= victim.query_facets.len() + 2)
                || (candidate_bridge && candidate.query_facets.len() > victim.query_facets.len());
            if materially_stronger {
                victim_index = Some(victim.ranking_index);
                break;
            }
        }
        if let Some(victim_index) = victim_index {
            chosen_indices.remove(&victim_index);
            chosen_indices.insert(candidate.ranking_index);
        }
    }

    Ok(ranking
        .iter()
        .enumerate()
        .filter(|(index, _)| chosen_indices.contains(index))
        .map(|(_, candidate)| candidate.clone())
        .collect())
}

pub(crate) fn compile_rerank_documents<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[crate::query::ReadoutCandidate],
    limit: usize,
    routed_atomic_sources: &[(NodeId, usize)],
) -> Result<Vec<EvidenceDocument>, Error> {
    let ranking =
        coverage_preselected_ranking(storage, plan, ranking, limit, routed_atomic_sources)?;
    if plan.answer_shape == AnswerShape::Inference {
        return compile_inference_documents(storage, &ranking, limit);
    }
    if plan.answer_shape == AnswerShape::Frequency {
        return compile_evidence_documents(storage, &ranking, limit);
    }
    if matches!(
        plan.recall_intent,
        RecallIntent::Enumeration | RecallIntent::Relational
    ) {
        return compile_evidence_documents(storage, &ranking, limit);
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
    use crate::graph::edge::EdgeSource;
    use crate::graph::node::Origin;
    use crate::graph::{Edge, MemoryTier, Node, PeerId, Timestamp};
    use crate::query::ReadoutCandidate;
    use crate::storage::SqliteStorage;
    use std::collections::VecDeque;

    fn fixture_node(
        id: NodeId,
        node_type: KnowledgeType,
        content: String,
        session_id: String,
    ) -> Node {
        Node {
            id,
            node_type,
            name: content.clone(),
            summary: None,
            content,
            embedding: None,
            created_at: Timestamp(1),
            updated_at: Timestamp(1),
            accessed_at: Timestamp(1),
            valid_from: None,
            valid_until: None,
            salience: 0.5,
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: PeerId(0),
                source_kind: crate::graph::SourceKind::AgentObservation,
                session_id,
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            entity_tags: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    fn ranked_fixture() -> (SqliteStorage, Vec<ReadoutCandidate>, NodeId) {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut ranking = Vec::new();
        let mut rare_node_id = None;
        for index in 0..80 {
            let id = storage.next_node_id();
            let content = if index == 70 {
                rare_node_id = Some(id);
                "Alice connected the rarecomet project to Bob".to_owned()
            } else {
                format!("ordinary evidence fragment {index}")
            };
            let node_type = if (30..50).contains(&index) {
                KnowledgeType::Semantic
            } else {
                KnowledgeType::Episodic
            };
            storage
                .set_node(fixture_node(
                    id,
                    node_type,
                    content,
                    format!("session-{}", index % 4),
                ))
                .expect("fixture node");
            ranking.push(ReadoutCandidate {
                node_id: id,
                score: 100.0 - index as f64,
                activation: 1.0,
                phi: 0.0,
                embedding_cosine: 0.0,
                salience: 0.5,
                impedance: 1.0,
                scope_weight: 1.0,
                trust_weight: 1.0,
                stress: 0.0,
            });
        }
        (
            storage,
            ranking,
            rare_node_id.expect("rare fixture node exists"),
        )
    }

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

    #[test]
    fn complex_preselection_keeps_head_and_routes_a_deep_query_facet() {
        let (storage, ranking, rare_node_id) = ranked_fixture();
        let plan =
            RecallPlan::infer("What is the relationship between the rarecomet project and Alice?");

        let selected =
            coverage_preselected_ranking(&storage, &plan, &ranking, 50, &[]).expect("preselection");

        assert_eq!(selected.len(), 50);
        assert_eq!(
            selected[..30],
            ranking[..30],
            "the authoritative head must stay byte-for-byte unchanged"
        );
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == rare_node_id),
            "a rare query facet from the deeper trace must reach the document surface"
        );
        assert!(
            selected
                .windows(2)
                .all(|window| window[0].score > window[1].score),
            "selected rows retain original cognitive rank"
        );
    }

    #[test]
    fn conservative_question_shapes_preserve_the_prefix() {
        let (storage, ranking, rare_node_id) = ranked_fixture();
        for query in [
            "Where is the rarecomet project?",
            "When did the rarecomet project start?",
            "Why did Alice choose the rarecomet project?",
            "What rarecomet device could Alice gift Bob?",
        ] {
            let plan = RecallPlan::infer(query);
            let selected = coverage_preselected_ranking(&storage, &plan, &ranking, 50, &[])
                .expect("preselection");
            assert_eq!(selected, ranking[..50]);
            assert!(
                selected
                    .iter()
                    .all(|candidate| candidate.node_id != rare_node_id)
            );
        }
    }

    #[test]
    fn advice_inference_can_use_the_deeper_trace() {
        let (storage, ranking, rare_node_id) = ranked_fixture();
        let plan = RecallPlan::infer("What advice might Alice give Bob about rarecomet?");
        let selected =
            coverage_preselected_ranking(&storage, &plan, &ranking, 50, &[]).expect("preselection");

        assert_eq!(selected[..30], ranking[..30]);
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == rare_node_id)
        );
    }

    #[test]
    fn atomic_source_can_route_a_bounded_temporal_neighbor_from_the_deep_trace() {
        let (mut storage, ranking, _) = ranked_fixture();
        let routed_source = ranking[68].node_id;
        let nearby_evidence = ranking[72].node_id;
        let edge_id = storage.next_edge_id();
        storage
            .set_edge(Edge::seeded(
                edge_id,
                routed_source,
                nearby_evidence,
                EdgeType::Temporal,
                1.0,
                EdgeSource::Manual,
                Timestamp(1),
                Timestamp(1),
                HashMap::new(),
            ))
            .expect("temporal edge");

        let plan = RecallPlan::infer("Would Alice enjoy a mountain retreat?");
        let selected =
            coverage_preselected_ranking(&storage, &plan, &ranking, 50, &[(routed_source, 0)])
                .expect("preselection");

        assert_eq!(selected.len(), 50);
        assert_eq!(selected[..30], ranking[..30]);
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == nearby_evidence),
            "a routed source can recover an adjacent premise without opening the whole session"
        );
        assert!(
            selected
                .iter()
                .all(|candidate| candidate.node_id != routed_source),
            "the seed itself does not bypass the ordinary ranking"
        );
    }

    #[test]
    fn selective_entity_matching_ignores_recipe_speaker_and_session_tags() {
        let tags = vec![
            "speaker-alice".to_owned(),
            "session-1".to_owned(),
            "anamnesis:derived".to_owned(),
            "Alice".to_owned(),
            "LGBTQ support group".to_owned(),
        ];
        assert_eq!(
            selective_entity_matches("What did Alice learn from the LGBTQ support group?", &tags),
            2
        );
        assert_eq!(
            selective_entity_matches("What did Bob learn from pottery?", &tags),
            0
        );
    }
}
