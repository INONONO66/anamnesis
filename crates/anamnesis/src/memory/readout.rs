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

/// Default evidence cap for one-fact product queries.
///
/// Complex and completeness-sensitive shapes retain the caller's requested
/// width. This cap only reduces redundant simple-query context after the full
/// candidate and reranker stages have run.
pub const DEFAULT_SIMPLE_DELIVERY_LIMIT: usize = 12;

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
    /// Direct queries preserve the reranker's first eight rows, then prefer
    /// tail rows that add canonical raw evidence before backfilling redundant
    /// representations. Inference and date queries remove candidates that
    /// contribute no new canonical raw evidence. Enumeration, relationship,
    /// and frequency queries additionally preserve source-session diversity.
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
    /// Whether the production path may shrink one-fact delivery below the
    /// caller's maximum while retaining temporal and complex-query widths.
    pub adaptive_delivery: bool,
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
            adaptive_delivery: true,
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

    /// Enable or disable question-shape-aware final evidence caps.
    ///
    /// Disabling this preserves the exact caller-supplied final maximum. It
    /// does not change cognitive search or reranker candidate widths.
    pub fn with_adaptive_delivery(mut self, adaptive_delivery: bool) -> Self {
        self.adaptive_delivery = adaptive_delivery;
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
    let requests_creator_attribution = words.iter().any(|word| {
        let word = word.strip_suffix("'s").unwrap_or(word);
        matches!(
            word,
            "artist" | "author" | "composer" | "creator" | "director" | "writer"
        )
    }) && words.iter().any(|word| {
        matches!(
            *word,
            "book"
                | "books"
                | "film"
                | "films"
                | "movie"
                | "movies"
                | "music"
                | "novel"
                | "novels"
                | "song"
                | "songs"
                | "theme"
                | "themes"
                | "tune"
                | "tunes"
        )
    });
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
            || requests_creator_attribution
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
    let requests_comparative_preference = has_word("prefer")
        && (has_sequence(&["more", "than"]) || has_sequence(&["rather", "than"]));
    let requests_inference = has_word("likely")
        || has_word("possibly")
        || has_word("potentially")
        || has_word("probably")
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
        || requests_comparative_preference
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
            || has_sequence(&["over", "time"])
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

pub(super) fn adaptive_delivery_limit(plan: &RecallPlan, requested_limit: usize) -> usize {
    if plan.recall_intent == RecallIntent::Direct && plan.answer_shape == AnswerShape::Fact {
        requested_limit.min(DEFAULT_SIMPLE_DELIVERY_LIMIT)
    } else {
        requested_limit
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
    overlap >= query_terms.len().min(2)
}

pub(crate) fn compile_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    selection: EvidenceSelection,
    limit: usize,
    routed_atomic_sources: &[AtomicSourceMarker],
) -> Result<Vec<RerankedCandidate>, Error> {
    let automatic = selection == EvidenceSelection::Auto;
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

    let baseline = match resolved_selection {
        EvidenceSelection::Auto | EvidenceSelection::Relevance => Ok(ranking.to_vec()),
        EvidenceSelection::DistinctSources => distinct_source_ranking(storage, ranking),
        EvidenceSelection::SourceCoverage => source_coverage_ranking(storage, ranking),
        EvidenceSelection::SourceSessionCoverage => {
            source_session_coverage_ranking(storage, ranking, limit)
        }
    }?;
    if !automatic {
        return Ok(baseline);
    }
    let baseline = if plan.recall_intent == RecallIntent::Direct {
        head_preserving_source_coverage_ranking(storage, &baseline, limit)?
    } else {
        baseline
    };
    claim_slot_coverage_ranking(
        storage,
        plan,
        ranking,
        &baseline,
        limit,
        routed_atomic_sources,
    )
}

fn head_preserving_source_coverage_ranking<S: StorageAdapter>(
    storage: &S,
    ranking: &[RerankedCandidate],
    limit: usize,
) -> Result<Vec<RerankedCandidate>, Error> {
    let head_limit = ranking.len().min(limit).min(8);
    let mut covered_sources = HashSet::new();
    let mut selected = Vec::with_capacity(ranking.len());
    for candidate in ranking.iter().take(head_limit) {
        covered_sources.extend(canonical_sources(storage, candidate.node_id)?);
        selected.push(*candidate);
    }

    let mut deferred = Vec::new();
    for candidate in ranking.iter().skip(head_limit) {
        let sources = canonical_sources(storage, candidate.node_id)?;
        if sources
            .iter()
            .any(|source| !covered_sources.contains(source))
        {
            covered_sources.extend(sources);
            selected.push(*candidate);
        } else {
            deferred.push(*candidate);
        }
    }
    selected.extend(deferred);
    Ok(selected)
}

fn claim_slot_coverage_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    baseline: &[RerankedCandidate],
    limit: usize,
    routed_atomic_sources: &[AtomicSourceMarker],
) -> Result<Vec<RerankedCandidate>, Error> {
    if !uses_atomic_fact_expansion(plan) || limit == 0 {
        return Ok(baseline.to_vec());
    }

    let mut claim_sources: HashMap<AtomicFactId, HashSet<NodeId>> = HashMap::new();
    for marker in routed_atomic_sources {
        let Some(fact_id) = marker.fact_id else {
            continue;
        };
        claim_sources
            .entry(fact_id)
            .or_default()
            .insert(marker.source_node_id);
    }
    if claim_sources.is_empty() {
        return Ok(baseline.to_vec());
    }

    enum ClaimCoverage {
        Legacy,
        Grounded {
            evidence_source: NodeId,
            evidence_span: String,
        },
        Invalid,
    }
    let mut claim_coverage = HashMap::new();
    let live_node_ids: HashSet<_> = storage.all_node_ids().into_iter().collect();
    for fact_id in claim_sources.keys().copied() {
        let fact = storage.get_atomic_fact(fact_id)?;
        let evidence_source = fact
            .metadata
            .get("anamnesis:evidence-source-node-id")
            .and_then(|value| value.parse::<u64>().ok())
            .map(NodeId);
        let evidence_start = fact
            .metadata
            .get("anamnesis:evidence-span-start")
            .and_then(|value| value.parse::<usize>().ok());
        let evidence_end = fact
            .metadata
            .get("anamnesis:evidence-span-end")
            .and_then(|value| value.parse::<usize>().ok());
        let ground_object = fact.metadata.get("anamnesis:ground-object");
        let requires_exact_object = fact.metadata.contains_key("anamnesis:evidence-object");
        let evidence_object = fact
            .metadata
            .get("anamnesis:evidence-object")
            .or(ground_object);
        let has_grounding_metadata = evidence_source.is_some()
            || evidence_start.is_some()
            || evidence_end.is_some()
            || ground_object.is_some()
            || evidence_object.is_some();
        let coverage = match (
            evidence_source,
            evidence_start,
            evidence_end,
            evidence_object,
        ) {
            (Some(evidence_source), Some(start), Some(end), Some(object))
                if live_node_ids.contains(&evidence_source)
                    && claim_sources
                        .get(&fact_id)
                        .is_some_and(|sources| sources.contains(&evidence_source)) =>
            {
                let source = storage.get_node(evidence_source)?;
                match source.content.get(start..end) {
                    Some(evidence_span)
                        if if requires_exact_object {
                            evidence_span.contains(object)
                        } else {
                            normalized_phrase(evidence_span).contains(&normalized_phrase(object))
                        } =>
                    {
                        ClaimCoverage::Grounded {
                            evidence_source,
                            evidence_span: evidence_span.to_owned(),
                        }
                    }
                    _ => ClaimCoverage::Invalid,
                }
            }
            _ if !has_grounding_metadata => ClaimCoverage::Legacy,
            _ => ClaimCoverage::Invalid,
        };
        claim_coverage.insert(fact_id, coverage);
    }

    let mut source_cache = HashMap::new();
    let mut covered_claim_cache = HashMap::new();
    for candidate in ranking.iter().chain(baseline) {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            source_cache.entry(candidate.node_id)
        {
            let candidate_sources = canonical_sources(storage, candidate.node_id)?
                .into_iter()
                .collect::<HashSet<_>>();
            let candidate_content = &storage.get_node(candidate.node_id)?.content;
            let covered = claim_sources
                .iter()
                .filter_map(|(fact_id, sources)| {
                    let covers = match claim_coverage.get(fact_id) {
                        Some(ClaimCoverage::Grounded {
                            evidence_source,
                            evidence_span,
                        }) => {
                            candidate_sources.contains(evidence_source)
                                && candidate_content.contains(evidence_span)
                        }
                        Some(ClaimCoverage::Legacy) => sources
                            .iter()
                            .any(|source| candidate_sources.contains(source)),
                        Some(ClaimCoverage::Invalid) | None => false,
                    };
                    covers.then_some(*fact_id)
                })
                .collect::<HashSet<_>>();
            entry.insert(candidate_sources);
            covered_claim_cache.insert(candidate.node_id, covered);
        }
    }
    let covered_claims = |node_id: NodeId| -> HashSet<AtomicFactId> {
        covered_claim_cache
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
    };

    let mut selected: Vec<_> = baseline.iter().take(limit).copied().collect();
    let mut coverage_counts = HashMap::new();
    for candidate in &selected {
        for fact_id in covered_claims(candidate.node_id) {
            *coverage_counts.entry(fact_id).or_insert(0usize) += 1;
        }
    }
    let mut missing: HashSet<_> = claim_sources
        .keys()
        .filter(|fact_id| !coverage_counts.contains_key(fact_id))
        .copied()
        .collect();
    if missing.is_empty() {
        return Ok(baseline.to_vec());
    }

    // The reranker's authoritative head is never removed. At the default
    // twenty-fragment width this freezes the first twelve rows and lets at
    // most four tail rows change, only when all of a victim's canonical raw
    // evidence remains represented.
    let head_limit = selected
        .len()
        .min(limit.saturating_mul(3).div_ceil(5).max(1));
    let mut replacements = 0usize;
    const MAX_CLAIM_REPLACEMENTS: usize = 4;

    while !missing.is_empty() && replacements < MAX_CLAIM_REPLACEMENTS {
        let selected_ids: HashSet<_> = selected.iter().map(|candidate| candidate.node_id).collect();
        let best_candidate = ranking
            .iter()
            .enumerate()
            .filter(|(_, candidate)| !selected_ids.contains(&candidate.node_id))
            .filter_map(|(rank, candidate)| {
                let claims = covered_claims(candidate.node_id);
                let gain = claims
                    .iter()
                    .filter(|fact_id| missing.contains(fact_id))
                    .count();
                (gain > 0).then_some((rank, *candidate, claims, gain))
            })
            .max_by(|left, right| left.3.cmp(&right.3).then_with(|| right.0.cmp(&left.0)));
        let Some((_, candidate, candidate_claims, _)) = best_candidate else {
            break;
        };

        if selected.len() < limit {
            selected.push(candidate);
        } else {
            let victim = (head_limit..selected.len()).rev().find(|index| {
                let victim_node_id = selected[*index].node_id;
                let victim_sources = source_cache
                    .get(&victim_node_id)
                    .cloned()
                    .unwrap_or_default();
                let mut sources_without_victim = source_cache
                    .get(&candidate.node_id)
                    .cloned()
                    .unwrap_or_default();
                for retained in selected
                    .iter()
                    .filter(|retained| retained.node_id != victim_node_id)
                {
                    if let Some(sources) = source_cache.get(&retained.node_id) {
                        sources_without_victim.extend(sources);
                    }
                }
                let preserves_raw_evidence = victim_sources
                    .iter()
                    .all(|source| sources_without_victim.contains(source));
                preserves_raw_evidence
                    && covered_claims(victim_node_id).into_iter().all(|fact_id| {
                        coverage_counts.get(&fact_id).copied().unwrap_or_default()
                            + usize::from(candidate_claims.contains(&fact_id))
                            > 1
                    })
            });
            let Some(victim) = victim else {
                break;
            };
            selected[victim] = candidate;
        }

        replacements += 1;
        coverage_counts.clear();
        for selected_candidate in &selected {
            for fact_id in covered_claims(selected_candidate.node_id) {
                *coverage_counts.entry(fact_id).or_insert(0usize) += 1;
            }
        }
        missing.retain(|fact_id| !coverage_counts.contains_key(fact_id));
    }

    if replacements == 0 {
        Ok(baseline.to_vec())
    } else {
        Ok(selected)
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
    lexical_idf_score: f64,
    matched_terms: HashSet<String>,
    entity_matches: usize,
    kind_priority: usize,
    source_session_id: String,
    source_node_ids: Vec<NodeId>,
}

pub(super) struct RoutedAtomicSource {
    pub candidate: crate::query::ReadoutCandidate,
    pub kind_priority: usize,
    pub fact_ids: Vec<AtomicFactId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct AtomicSourceMarker {
    pub source_node_id: NodeId,
    pub kind_priority: usize,
    pub fact_id: Option<AtomicFactId>,
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

fn atomic_entity_matches(
    query: &str,
    entity_tags: &[String],
    metadata: &HashMap<String, String>,
) -> usize {
    let mut matches = selective_entity_matches(query, entity_tags);
    let Some(subject) = metadata
        .get("anamnesis:ground-subject")
        .map(|subject| subject.trim())
        .filter(|subject| !subject.is_empty())
    else {
        return matches;
    };
    let normalized_subject = normalized_phrase(subject);
    let subject_is_tagged = entity_tags
        .iter()
        .any(|tag| normalized_phrase(tag) == normalized_subject);
    if !subject_is_tagged {
        let normalized_query = normalized_phrase(query);
        matches += usize::from(
            normalized_subject.len() > 2 && normalized_query.contains(&normalized_subject),
        );
    }
    matches
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

pub(super) fn uses_dense_query_expansion(plan: &RecallPlan) -> bool {
    plan.recall_intent != RecallIntent::Temporal && uses_complex_expansion(plan)
}

fn uses_atomic_fact_expansion(plan: &RecallPlan) -> bool {
    uses_complex_expansion(plan)
        || matches!(
            plan.answer_shape,
            AnswerShape::Count | AnswerShape::Frequency
        )
}

fn requests_creator_attribution_window(plan: &RecallPlan) -> bool {
    if plan.answer_shape != AnswerShape::Relationship {
        return false;
    }
    let words: HashSet<_> = plan
        .query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let requests_creator = words.iter().any(|word| {
        matches!(
            word.as_str(),
            "artist" | "author" | "composer" | "creator" | "director" | "writer"
        )
    });
    let names_work = words.iter().any(|word| {
        matches!(
            word.as_str(),
            "book"
                | "film"
                | "movie"
                | "music"
                | "novel"
                | "song"
                | "theme"
                | "tune"
                | "tunes"
                | "work"
        )
    });
    requests_creator && names_work
}

fn uses_idf_atomic_lane(plan: &RecallPlan) -> bool {
    matches!(
        plan.answer_shape,
        AnswerShape::Count | AnswerShape::Frequency
    )
}

fn uses_strict_atomic_admission(plan: &RecallPlan) -> bool {
    matches!(
        plan.answer_shape,
        AnswerShape::Count | AnswerShape::Frequency
    )
}

pub(super) fn parse_atomic_source_markers(strategies: &[String]) -> Vec<AtomicSourceMarker> {
    let mut markers = Vec::new();
    for strategy in strategies {
        let Some(encoded_sources) = strategy.strip_prefix("atomic_fact_sources:") else {
            continue;
        };
        for encoded_source in encoded_sources.split(',') {
            let mut parts = encoded_source.split('@');
            let Some(encoded_id) = parts.next() else {
                continue;
            };
            let Some(encoded_priority) = parts.next() else {
                continue;
            };
            let Ok(source_id) = encoded_id.parse::<u64>() else {
                continue;
            };
            let Ok(kind_priority) = encoded_priority.parse::<usize>() else {
                continue;
            };
            let fact_id = match parts.next() {
                Some(encoded_fact_id) => {
                    let Ok(fact_id) = encoded_fact_id.parse::<u64>() else {
                        continue;
                    };
                    Some(AtomicFactId(fact_id))
                }
                None => None,
            };
            if parts.next().is_some() {
                continue;
            }
            let marker = AtomicSourceMarker {
                source_node_id: NodeId(source_id),
                kind_priority,
                fact_id,
            };
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
    }
    markers
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

fn source_diverse_atomic_ranking(
    ranked_facts: Vec<(AtomicFactId, f64)>,
    fact_limit: usize,
    per_session_limit: usize,
    source_sessions: &HashMap<AtomicFactId, String>,
    source_nodes: &HashMap<AtomicFactId, Vec<NodeId>>,
) -> Vec<(AtomicFactId, f64)> {
    let mut session_counts = HashMap::new();
    let mut covered_sources = HashSet::new();
    let mut selected = Vec::with_capacity(fact_limit);
    let mut deferred = Vec::new();

    // First preserve both session breadth and raw-evidence breadth. Multiple
    // atomic claims can cite different spans in one raw turn, but returning
    // that turn once already exposes every span to the reader.
    for ranked_fact in ranked_facts {
        let Some(session) = source_sessions.get(&ranked_fact.0) else {
            deferred.push(ranked_fact);
            continue;
        };
        let adds_source = source_nodes.get(&ranked_fact.0).is_some_and(|sources| {
            sources
                .iter()
                .any(|source| !covered_sources.contains(source))
        });
        let count = session_counts.entry(session.clone()).or_insert(0usize);
        if selected.len() < fact_limit && *count < per_session_limit && adds_source {
            *count += 1;
            if let Some(sources) = source_nodes.get(&ranked_fact.0) {
                covered_sources.extend(sources.iter().copied());
            }
            selected.push(ranked_fact);
        } else {
            deferred.push(ranked_fact);
        }
    }

    // If the session quota left capacity, relax it only for facts that expose
    // another raw source. Exact-source duplicates remain the final backfill.
    let mut duplicate_sources = Vec::new();
    for ranked_fact in deferred {
        let adds_source = source_nodes.get(&ranked_fact.0).is_some_and(|sources| {
            sources
                .iter()
                .any(|source| !covered_sources.contains(source))
        });
        if selected.len() < fact_limit && adds_source {
            if let Some(sources) = source_nodes.get(&ranked_fact.0) {
                covered_sources.extend(sources.iter().copied());
            }
            selected.push(ranked_fact);
        } else {
            duplicate_sources.push(ranked_fact);
        }
    }
    if selected.len() < fact_limit {
        selected.extend(
            duplicate_sources
                .into_iter()
                .take(fact_limit - selected.len()),
        );
    }
    selected
}

pub(super) fn route_atomic_fact_sources<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    query_embedding: &[f64],
    now: crate::graph::Timestamp,
    scope: &ScopePath,
) -> Result<Vec<RoutedAtomicSource>, Error> {
    if !uses_atomic_fact_expansion(plan) {
        return Ok(Vec::new());
    }
    let fact_limit = match plan.answer_shape {
        AnswerShape::Collection => 16,
        AnswerShape::Relationship => 12,
        AnswerShape::Inference => 16,
        AnswerShape::Count | AnswerShape::Frequency => 16,
        _ => return Ok(Vec::new()),
    };
    let atomic_fact_ids = storage.all_atomic_fact_ids();
    if atomic_fact_ids.is_empty() {
        return Ok(Vec::new());
    }

    let query_terms = facet_terms(&plan.query);
    let mut facts = Vec::with_capacity(atomic_fact_ids.len());
    let mut lexical_document_frequency = HashMap::new();
    let mut eligible_fact_count = 0usize;
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
        eligible_fact_count += 1;
        let dense_score = cosine_similarity(query_embedding, &fact.embedding);
        let fact_terms = facet_terms(&fact.content);
        let matched_terms: HashSet<_> = query_terms.intersection(&fact_terms).cloned().collect();
        for term in &matched_terms {
            *lexical_document_frequency
                .entry(term.clone())
                .or_insert(0usize) += 1;
        }
        let lexical_overlap = matched_terms.len();
        let entity_matches = atomic_entity_matches(&plan.query, &fact.entity_tags, &fact.metadata);
        let kind_priority = inference_fact_kind_priority(plan, &fact.metadata);
        if uses_strict_atomic_admission(plan) && entity_matches == 0 && lexical_overlap < 2 {
            continue;
        }
        if dense_score > 0.0 || lexical_overlap > 0 || entity_matches > 0 {
            facts.push(AtomicFactCandidate {
                fact_id,
                dense_score,
                lexical_overlap,
                lexical_idf_score: 0.0,
                matched_terms,
                entity_matches,
                kind_priority,
                source_session_id: fact.source_session_id.clone(),
                source_node_ids: fact.source_node_ids.clone(),
            });
        }
    }
    for fact in &mut facts {
        fact.lexical_idf_score = fact
            .matched_terms
            .iter()
            .map(|term| {
                let document_frequency = lexical_document_frequency
                    .get(term)
                    .copied()
                    .unwrap_or_default();
                ((eligible_fact_count as f64 + 1.0) / (document_frequency as f64 + 1.0)).ln() + 1.0
            })
            .sum();
    }

    const LANE_DEPTH: usize = 64;
    let mut dense: Vec<_> = facts.iter().collect();
    dense.sort_by(|left, right| {
        right
            .dense_score
            .total_cmp(&left.dense_score)
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut idf_lexical: Vec<_> = facts
        .iter()
        .filter(|fact| fact.lexical_idf_score > 0.0)
        .collect();
    idf_lexical.sort_by(|left, right| {
        right
            .lexical_idf_score
            .total_cmp(&left.lexical_idf_score)
            .then_with(|| right.lexical_overlap.cmp(&left.lexical_overlap))
            .then_with(|| right.dense_score.total_cmp(&left.dense_score))
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
    if uses_idf_atomic_lane(plan) {
        add_atomic_rrf_scores(
            idf_lexical.iter().take(LANE_DEPTH).map(|fact| fact.fact_id),
            &mut fused,
        );
    }
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
    let source_sessions_by_id: HashMap<_, _> = facts
        .iter()
        .map(|fact| (fact.fact_id, fact.source_session_id.clone()))
        .collect();
    let source_nodes_by_id: HashMap<_, _> = facts
        .iter()
        .map(|fact| (fact.fact_id, fact.source_node_ids.clone()))
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
        AnswerShape::Count | AnswerShape::Frequency => 4,
        _ => fact_limit,
    };
    let ranked_facts = source_diverse_atomic_ranking(
        ranked_facts,
        fact_limit,
        per_session_limit,
        &source_sessions_by_id,
        &source_nodes_by_id,
    );
    let max_fused = ranked_facts
        .first()
        .map(|(_, score)| *score)
        .unwrap_or(1.0)
        .max(f64::EPSILON);

    let mut routed_position_by_source: HashMap<NodeId, usize> = HashMap::new();
    let mut routed: Vec<RoutedAtomicSource> = Vec::new();
    let live_node_ids: HashSet<_> = storage.all_node_ids().into_iter().collect();
    // The trace can retain multiple raw provenance rows per selected fact, but
    // the caller controls how many are promoted into the latency-sensitive
    // document head. Keep the auxiliary lane bounded by the 20-row production
    // tail even when a fact cites several turns.
    let source_limit = fact_limit.saturating_mul(2).min(20);
    for (fact_id, fused_score) in ranked_facts {
        let fact = storage.get_atomic_fact(fact_id)?;
        let evidence_source = fact
            .metadata
            .get("anamnesis:evidence-source-node-id")
            .and_then(|value| value.parse::<u64>().ok())
            .map(NodeId);
        let mut ordered_sources = fact.source_node_ids.clone();
        ordered_sources.sort_by_key(|source_id| usize::from(Some(*source_id) != evidence_source));
        for source_id in ordered_sources {
            if let Some(position) = routed_position_by_source.get(&source_id).copied() {
                let routed_source = &mut routed[position];
                if !routed_source.fact_ids.contains(&fact_id) {
                    routed_source.fact_ids.push(fact_id);
                }
                routed_source.kind_priority = routed_source.kind_priority.max(
                    kind_priority_by_id
                        .get(&fact_id)
                        .copied()
                        .unwrap_or_default(),
                );
                continue;
            }
            if routed.len() >= source_limit {
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
                fact_ids: vec![fact_id],
            });
            routed_position_by_source.insert(source_id, routed.len() - 1);
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
    if !uses_atomic_fact_expansion(plan) || ranking.len() <= inspected || inspected < 2 {
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
    if requests_creator_attribution_window(plan) {
        return ranking
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
            .collect();
    }
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
    use crate::storage::{AtomicFact, SqliteStorage};
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

    fn seed_legacy_atomic_fact(
        storage: &mut SqliteStorage,
        fact_id: AtomicFactId,
        source_node_id: NodeId,
    ) {
        let (content, source_session_id, scope, observed_at) = {
            let source = storage
                .get_node(source_node_id)
                .expect("atomic fact source");
            (
                source.content.clone(),
                source.origin.session_id.clone(),
                source.origin.scope.clone(),
                source.created_at,
            )
        };
        storage
            .set_atomic_fact(AtomicFact {
                id: fact_id,
                content,
                embedding: vec![1.0],
                source_node_ids: vec![source_node_id],
                entity_tags: Vec::new(),
                source_session_id,
                scope,
                observed_at,
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            })
            .expect("legacy atomic fact");
    }

    fn seed_grounded_atomic_fact(
        storage: &mut SqliteStorage,
        fact_id: AtomicFactId,
        source_node_id: NodeId,
        evidence_span: &str,
        object: &str,
    ) {
        let (source_session_id, scope, observed_at, start) = {
            let source = storage
                .get_node(source_node_id)
                .expect("grounded atomic fact source");
            (
                source.origin.session_id.clone(),
                source.origin.scope.clone(),
                source.created_at,
                source
                    .content
                    .find(evidence_span)
                    .expect("grounded evidence span"),
            )
        };
        let metadata = [
            (
                "anamnesis:evidence-source-node-id".to_owned(),
                source_node_id.0.to_string(),
            ),
            (
                "anamnesis:evidence-span-start".to_owned(),
                start.to_string(),
            ),
            (
                "anamnesis:evidence-span-end".to_owned(),
                (start + evidence_span.len()).to_string(),
            ),
            ("anamnesis:ground-object".to_owned(), object.to_owned()),
        ]
        .into_iter()
        .collect();
        storage
            .set_atomic_fact(AtomicFact {
                id: fact_id,
                content: format!("Alice completed {object}"),
                embedding: vec![1.0],
                source_node_ids: vec![source_node_id],
                entity_tags: vec!["Alice".to_owned()],
                source_session_id,
                scope,
                observed_at,
                valid_from: None,
                valid_until: None,
                metadata,
            })
            .expect("grounded atomic fact");
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
    fn over_time_is_a_temporal_retrieval_constraint() {
        let plan = RecallPlan::infer("What diet change did Sam adopt over time?");

        assert_eq!(plan.answer_shape, AnswerShape::Fact);
        assert_eq!(plan.recall_intent, RecallIntent::Temporal);
        assert_eq!(adaptive_delivery_limit(&plan, 20), 20);
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
            AnswerShape::Relationship
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
            RecallPlan::infer("Which meat does Audrey prefer eating more than others?")
                .answer_shape,
            AnswerShape::Inference
        );
        assert_eq!(
            RecallPlan::infer("Which state do Alice and Bob potentially live in?").answer_shape,
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
        assert!(temporal_evidence_matches(
            "Which book did Jolene read in January 2023?",
            "Jolene: Two weeks ago I read Avalanche by Neal Stephenson."
        ));
    }

    #[test]
    fn adaptive_delivery_keeps_completeness_queries_wide() {
        for query in [
            "What projects did Alice complete?",
            "When did Alice move?",
            "Where did Alice move in June 2023?",
            "How many projects did Alice complete?",
            "How often does Alice get a health checkup?",
            "Why did Alice move?",
            "Would Alice enjoy a mountain retreat?",
        ] {
            let plan = RecallPlan::infer(query);
            assert_eq!(adaptive_delivery_limit(&plan, 20), 20, "{query:?}");
        }
    }

    #[test]
    fn adaptive_delivery_caps_fact_context_without_exceeding_the_request() {
        let plan = RecallPlan::infer("Where does Alice live?");
        assert_eq!(adaptive_delivery_limit(&plan, 20), 12);
        assert_eq!(adaptive_delivery_limit(&plan, 8), 8);
    }

    #[test]
    fn direct_auto_selection_freezes_the_head_and_defers_redundant_tail_views() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut ranking = Vec::new();
        for index in 0..13 {
            let id = storage.next_node_id();
            storage
                .set_node(fixture_node(
                    id,
                    KnowledgeType::Episodic,
                    format!("direct evidence {index}"),
                    "direct-session".to_owned(),
                ))
                .expect("direct evidence");
            ranking.push(RerankedCandidate {
                node_id: id,
                score: 100.0 - index as f64,
            });
        }

        let redundant_tail = storage.next_node_id();
        storage
            .set_node(fixture_node(
                redundant_tail,
                KnowledgeType::Semantic,
                "redundant tail view".to_owned(),
                "direct-session".to_owned(),
            ))
            .expect("redundant tail view");
        let edge_id = storage.next_edge_id();
        storage
            .set_edge(Edge::seeded(
                edge_id,
                redundant_tail,
                ranking[0].node_id,
                EdgeType::ExtractedFrom,
                1.0,
                EdgeSource::Manual,
                Timestamp(1),
                Timestamp(1),
                HashMap::new(),
            ))
            .expect("redundant provenance");
        ranking.insert(
            8,
            RerankedCandidate {
                node_id: redundant_tail,
                score: 92.5,
            },
        );

        let selected = compile_ranking(
            &storage,
            &RecallPlan::infer("Where does Alice live?"),
            &ranking,
            EvidenceSelection::Auto,
            12,
            &[],
        )
        .expect("direct auto selection");

        assert_eq!(selected[..8], ranking[..8]);
        assert_eq!(selected[8], ranking[9]);
        assert!(
            selected[..12]
                .iter()
                .all(|candidate| candidate.node_id != redundant_tail)
        );
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == redundant_tail),
            "deferred representations remain available as a last-resort backfill"
        );
    }

    #[test]
    fn explicit_relevance_selection_preserves_direct_reranker_order() {
        let (storage, readout, _) = ranked_fixture();
        let ranking: Vec<_> = readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect();

        let selected = compile_ranking(
            &storage,
            &RecallPlan::infer("Where does Alice live?"),
            &ranking,
            EvidenceSelection::Relevance,
            12,
            &[],
        )
        .expect("explicit relevance");

        assert_eq!(selected, ranking);
    }

    #[test]
    fn atomic_expansion_is_gated_to_complex_count_and_frequency_queries() {
        for query in [
            "What projects did Alice complete?",
            "How many times did Alice move?",
            "How often does Alice get a health checkup?",
        ] {
            assert!(
                uses_atomic_fact_expansion(&RecallPlan::infer(query)),
                "{query:?} should use the isolated atomic lane"
            );
        }
        for query in [
            "Where does Alice live?",
            "When did Alice move?",
            "Which activity did Alice pursue on 5 June 2023?",
            "Why did Alice move?",
            "What device could Alice gift Bob?",
        ] {
            assert!(
                !uses_atomic_fact_expansion(&RecallPlan::infer(query)),
                "{query:?} must preserve the conservative production path"
            );
        }
    }

    #[test]
    fn claim_slot_selection_preserves_the_head_and_recovers_missing_fact_provenance() {
        let (mut storage, readout, _) = ranked_fixture();
        let mut ranking: Vec<_> = readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect();
        let claim_source = storage.next_node_id();
        storage
            .set_node(fixture_node(
                claim_source,
                KnowledgeType::Episodic,
                "Alice completed the missing cobalt project".to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("claim source");
        let bridge_candidate = storage.next_node_id();
        storage
            .set_node(fixture_node(
                bridge_candidate,
                KnowledgeType::Semantic,
                "Alice project evidence bridge".to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("bridge candidate");
        for source in [ranking[19].node_id, claim_source] {
            let edge_id = storage.next_edge_id();
            storage
                .set_edge(Edge::seeded(
                    edge_id,
                    bridge_candidate,
                    source,
                    EdgeType::ExtractedFrom,
                    1.0,
                    EdgeSource::Manual,
                    Timestamp(1),
                    Timestamp(1),
                    HashMap::new(),
                ))
                .expect("bridge provenance");
        }
        ranking.push(RerankedCandidate {
            node_id: bridge_candidate,
            score: -1.0,
        });
        seed_legacy_atomic_fact(&mut storage, AtomicFactId(1), ranking[0].node_id);
        seed_legacy_atomic_fact(&mut storage, AtomicFactId(2), claim_source);
        let markers = [
            AtomicSourceMarker {
                source_node_id: ranking[0].node_id,
                kind_priority: 0,
                fact_id: Some(AtomicFactId(1)),
            },
            AtomicSourceMarker {
                source_node_id: claim_source,
                kind_priority: 0,
                fact_id: Some(AtomicFactId(2)),
            },
        ];

        let selected = compile_ranking(
            &storage,
            &RecallPlan::infer("How many projects did Alice complete?"),
            &ranking,
            EvidenceSelection::Auto,
            20,
            &markers,
        )
        .expect("claim-slot selection");

        assert_eq!(selected.len(), 20);
        assert_eq!(
            selected[..12],
            ranking[..12],
            "the authoritative reranker head must not be removed"
        );
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == bridge_candidate),
            "a candidate that adds a missing claim while preserving the victim's raw source may replace a redundant tail row"
        );
    }

    #[test]
    fn grounded_claim_slot_requires_the_answer_bearing_span_not_only_the_same_turn() {
        let (mut storage, readout, _) = ranked_fixture();
        let mut ranking: Vec<_> = readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect();
        let evidence = "Alice completed the missing cobalt project";
        let claim_source = storage.next_node_id();
        storage
            .set_node(fixture_node(
                claim_source,
                KnowledgeType::Episodic,
                evidence.to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("claim source");
        let topic_only = storage.next_node_id();
        storage
            .set_node(fixture_node(
                topic_only,
                KnowledgeType::Semantic,
                "Alice discussed a project".to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("topic-only summary");
        let topic_edge = storage.next_edge_id();
        storage
            .set_edge(Edge::seeded(
                topic_edge,
                topic_only,
                claim_source,
                EdgeType::ExtractedFrom,
                1.0,
                EdgeSource::Manual,
                Timestamp(1),
                Timestamp(1),
                HashMap::new(),
            ))
            .expect("topic provenance");
        ranking[18] = RerankedCandidate {
            node_id: topic_only,
            score: ranking[18].score,
        };

        let grounded_bridge = storage.next_node_id();
        storage
            .set_node(fixture_node(
                grounded_bridge,
                KnowledgeType::Semantic,
                evidence.to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("grounded bridge");
        for source in [ranking[19].node_id, claim_source] {
            let edge_id = storage.next_edge_id();
            storage
                .set_edge(Edge::seeded(
                    edge_id,
                    grounded_bridge,
                    source,
                    EdgeType::ExtractedFrom,
                    1.0,
                    EdgeSource::Manual,
                    Timestamp(1),
                    Timestamp(1),
                    HashMap::new(),
                ))
                .expect("grounded bridge provenance");
        }
        ranking.push(RerankedCandidate {
            node_id: grounded_bridge,
            score: -1.0,
        });
        seed_grounded_atomic_fact(
            &mut storage,
            AtomicFactId(1),
            claim_source,
            evidence,
            "cobalt project",
        );

        let selected = compile_ranking(
            &storage,
            &RecallPlan::infer("How many projects did Alice complete?"),
            &ranking,
            EvidenceSelection::Auto,
            20,
            &[AtomicSourceMarker {
                source_node_id: claim_source,
                kind_priority: 0,
                fact_id: Some(AtomicFactId(1)),
            }],
        )
        .expect("grounded claim-slot selection");

        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == grounded_bridge),
            "a topic-only summary sharing the same raw turn must not satisfy the claim"
        );
    }

    #[test]
    fn claim_slot_selection_is_byte_stable_when_the_baseline_covers_every_claim() {
        let (mut storage, readout, _) = ranked_fixture();
        let ranking: Vec<_> = readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect();
        let marker = AtomicSourceMarker {
            source_node_id: ranking[0].node_id,
            kind_priority: 0,
            fact_id: Some(AtomicFactId(1)),
        };
        seed_legacy_atomic_fact(&mut storage, AtomicFactId(1), ranking[0].node_id);

        let selected = compile_ranking(
            &storage,
            &RecallPlan::infer("How many projects did Alice complete?"),
            &ranking,
            EvidenceSelection::Auto,
            20,
            &[marker],
        )
        .expect("claim-slot selection");

        assert_eq!(selected, ranking[..20]);
    }

    #[test]
    fn atomic_source_marker_parser_accepts_new_claim_ids_and_legacy_markers() {
        let strategies = vec![
            "cognitive".to_owned(),
            "atomic_fact_sources:7@3@11,9@0".to_owned(),
        ];
        assert_eq!(
            parse_atomic_source_markers(&strategies),
            vec![
                AtomicSourceMarker {
                    source_node_id: NodeId(7),
                    kind_priority: 3,
                    fact_id: Some(AtomicFactId(11)),
                },
                AtomicSourceMarker {
                    source_node_id: NodeId(9),
                    kind_priority: 0,
                    fact_id: None,
                },
            ]
        );
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
    fn creator_attribution_reranker_preserves_the_semantic_window() {
        let (mut storage, ranking, _) = ranked_fixture();
        let raw_source = ranking[0].node_id;
        let semantic_window = ranking[6].node_id;
        let window_text = "Tim: My favorite piano tune is a movie theme.\n\
                           John: Which movie?\n\
                           Tim: Harry Potter and the Philosopher's Stone.";
        let mut window_node = storage
            .get_node(semantic_window)
            .expect("semantic window")
            .clone();
        window_node.node_type = KnowledgeType::Semantic;
        window_node.content = window_text.to_owned();
        storage.set_node(window_node).expect("semantic window");
        let edge_id = storage.next_edge_id();
        storage
            .set_edge(Edge::seeded(
                edge_id,
                semantic_window,
                raw_source,
                EdgeType::ExtractedFrom,
                1.0,
                EdgeSource::Manual,
                Timestamp(1),
                Timestamp(1),
                HashMap::new(),
            ))
            .expect("window provenance");

        let plan =
            RecallPlan::infer("Which popular music composer's tunes does Tim enjoy playing?");
        let documents = compile_rerank_documents(&storage, &plan, &ranking, 50, &[])
            .expect("creator documents");
        let document = documents
            .iter()
            .find(|document| document.node_id == semantic_window)
            .expect("semantic window remains independently rerankable");
        assert_eq!(document.text, window_text);
        assert_eq!(document.source_node_ids, vec![raw_source]);

        let ordinary = RecallPlan::infer("What is Tim's relationship with John?");
        assert!(!requests_creator_attribution_window(&ordinary));
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

    #[test]
    fn atomic_entity_matching_uses_canonical_subject_without_double_counting() {
        let query = "Which countries has Deborah traveled to?";
        let mut metadata = HashMap::new();
        metadata.insert("anamnesis:ground-subject".to_owned(), "Deborah".to_owned());

        assert_eq!(
            atomic_entity_matches(query, &["Rio de Janeiro".to_owned()], &metadata),
            1,
            "the canonical subject remains routable when the extractor omits it from entity tags"
        );
        assert_eq!(
            atomic_entity_matches(
                query,
                &["Deborah".to_owned(), "Rio de Janeiro".to_owned()],
                &metadata,
            ),
            1,
            "the canonical subject must not be counted twice"
        );
    }

    #[test]
    fn atomic_fact_ranking_prefers_new_raw_sources_before_duplicate_claims() {
        let ranked = vec![
            (AtomicFactId(1), 4.0),
            (AtomicFactId(2), 3.0),
            (AtomicFactId(3), 2.0),
            (AtomicFactId(4), 1.0),
        ];
        let source_sessions = [
            (AtomicFactId(1), "session-a".to_owned()),
            (AtomicFactId(2), "session-a".to_owned()),
            (AtomicFactId(3), "session-b".to_owned()),
            (AtomicFactId(4), "session-a".to_owned()),
        ]
        .into_iter()
        .collect();
        let source_nodes = [
            (AtomicFactId(1), vec![NodeId(10)]),
            (AtomicFactId(2), vec![NodeId(10)]),
            (AtomicFactId(3), vec![NodeId(20)]),
            (AtomicFactId(4), vec![NodeId(30)]),
        ]
        .into_iter()
        .collect();

        let selected = source_diverse_atomic_ranking(ranked, 3, 2, &source_sessions, &source_nodes);
        assert_eq!(
            selected
                .into_iter()
                .map(|(fact_id, _)| fact_id)
                .collect::<Vec<_>>(),
            [AtomicFactId(1), AtomicFactId(3), AtomicFactId(4)],
            "a second claim from one raw turn must not consume the slot of another source"
        );
    }
}
