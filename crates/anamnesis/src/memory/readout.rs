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
use crate::storage::{
    AtomicFact, AtomicFactId, AtomicFactRelationId, AtomicFactRelationKind, StorageAdapter,
};

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
/// default preserves multi-source evidence chains under bounded delivery.
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
    /// Speaker-qualified authoritative raw source evidence for display and
    /// answer generation.
    pub text: String,
    /// Query-routed scoring surface used only by the reranker.
    ///
    /// This normally equals [`Self::text`]. For complex enumeration,
    /// relationship, and inference queries, when a reviewed atomic fact with
    /// valid byte-exact grounding routed one of the raw sources, it also
    /// includes a bounded canonical cue and verbatim evidence span. A
    /// relational query may likewise add a validated immediately preceding
    /// same-session question around a native answer candidate. These cues are
    /// never emitted by readout packaging; [`Self::text`] and the cited source
    /// nodes remain the authoritative evidence returned to a reader.
    rerank_text: String,
}

impl EvidenceDocument {
    fn from_raw(node_id: NodeId, source_node_ids: Vec<NodeId>, text: String) -> Self {
        Self {
            node_id,
            source_node_ids,
            rerank_text: text.clone(),
            text,
        }
    }

    /// Text that a reranking consumer should score for this document.
    ///
    /// The returned surface can contain bounded, source-grounded routing cues.
    /// Display and answer-generation consumers should continue to use
    /// [`Self::text`] or the final packaged raw fragments instead.
    pub fn rerank_text(&self) -> &str {
        &self.rerank_text
    }
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

    /// Compile the model-independent reader contract for this recall plan.
    ///
    /// The contract does not call a model or parse a provider-specific wire
    /// format. Consumers can use its stage instructions directly, or translate
    /// a provider response into [`GroundedAnswerDraft`] for deterministic
    /// source-membership validation.
    pub fn reader_contract(&self) -> RecallReaderContract {
        RecallReaderContract::from_plan(self)
    }
}

/// Stage of a source-grounded memory readout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecallReaderStage {
    /// Produce an answer directly from the delivered evidence.
    Answer,
    /// Inspect and organize evidence before drafting an answer.
    Reflection,
    /// Verify a draft against the delivered evidence before returning it.
    Verification,
}

/// Whether a separate evidence-analysis pass is useful for a recall plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReflectionRecommendation {
    /// A direct read is normally sufficient.
    Optional,
    /// Multiple facts, dates, or reasoning steps should be checked separately.
    Recommended,
}

/// Output form requested by the query independently of its semantic shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReaderAnswerForm {
    /// A normal factual, temporal, list, relationship, or inferred answer.
    Direct,
    /// A yes/no conclusion followed by its shortest concrete support.
    Binary,
    /// One or more explicitly named alternatives must be compared by name.
    Alternatives,
}

/// Provider-neutral instructions for reading one [`RecallPlan`].
///
/// This contract is deliberately text-only and dependency-free. It describes
/// evidence discipline and answer shape; transport adapters remain responsible
/// for choosing a model, wire schema, and generation settings.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecallReaderContract {
    /// Plan from which this contract was compiled.
    pub plan: RecallPlan,
    /// Whether a distinct analysis pass is recommended.
    pub reflection: ReflectionRecommendation,
    /// Query-level output form.
    pub answer_form: ReaderAnswerForm,
}

/// Trusted ownership metadata for one visibly source-bound evidence line.
///
/// A source may appear in more than one overlapping context window. The block
/// and line order preserve that presentation without changing the canonical
/// source identity used for citation validation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecallSourceAttribution {
    /// Canonical source node cited by this visible line.
    pub source_node_id: NodeId,
    /// Canonical speaker from the source node's provenance tags, when present.
    pub speaker: Option<String>,
    /// Exact visible evidence line.
    pub text: String,
    /// Source session retained from the delivered fragment.
    pub session_id: String,
    /// Delivered fragment that presented this line.
    pub dialogue_block_node_id: NodeId,
    /// Zero-based line order within the delivered fragment.
    pub line_order: usize,
}

impl RecallSourceAttribution {
    /// Create one trusted source-line attribution.
    pub fn new(
        source_node_id: NodeId,
        speaker: Option<String>,
        text: impl Into<String>,
        session_id: impl Into<String>,
        dialogue_block_node_id: NodeId,
        line_order: usize,
    ) -> Self {
        Self {
            source_node_id,
            speaker,
            text: text.into(),
            session_id: session_id.into(),
            dialogue_block_node_id,
            line_order,
        }
    }
}

impl RecallReaderContract {
    /// Compile a reader contract from a deterministic recall plan.
    pub fn from_plan(plan: &RecallPlan) -> Self {
        let reflection = if plan.recall_intent == RecallIntent::Temporal
            || matches!(
                plan.answer_shape,
                AnswerShape::Count
                    | AnswerShape::Collection
                    | AnswerShape::Frequency
                    | AnswerShape::Inference
                    | AnswerShape::Relationship
            ) {
            ReflectionRecommendation::Recommended
        } else {
            ReflectionRecommendation::Optional
        };
        let answer_form = if query_presents_explicit_alternatives(&plan.query) {
            ReaderAnswerForm::Alternatives
        } else if query_starts_with_binary_auxiliary(&plan.query) {
            ReaderAnswerForm::Binary
        } else {
            ReaderAnswerForm::Direct
        };
        Self {
            plan: plan.clone(),
            reflection,
            answer_form,
        }
    }

    /// Return whether a distinct evidence-analysis pass is recommended.
    pub fn reflection_recommended(&self) -> bool {
        self.reflection == ReflectionRecommendation::Recommended
    }

    /// Compile the complete generic instruction for one reader stage.
    ///
    /// The returned text never contains evidence or an answer. Consumers
    /// should append their query, current date when relevant, and the exact
    /// context returned by [`Memory::render_context_for`](super::Memory::render_context_for).
    pub fn instruction(&self, stage: RecallReaderStage) -> String {
        let requests_duration = query_requests_elapsed_duration(&self.plan.query);
        let mut rules = Vec::new();
        rules.push(match stage {
            RecallReaderStage::Answer => {
                "Use the delivered evidence as authoritative for every personal, session-specific, or changing fact. Combine separate passages when the requested answer requires more than one premise."
            }
            RecallReaderStage::Reflection => {
                "Identify every slot required by the question, then inspect the delivered evidence for each slot before drafting a conclusion. Keep every supporting claim tied to its exact source id."
            }
            RecallReaderStage::Verification => {
                "Treat the draft as untrusted. Verify every personal, session-specific, or changing claim against the delivered evidence and its exact source id; correct unsupported or incomplete claims."
            }
        });
        rules.push(
            "In multi-speaker material, a statement belongs only to the speaker named on that exact source line. A neighboring turn, enclosing summary, or section title does not transfer ownership.",
        );
        rules.push(
            "Keep entities, polarity, modality, quantities, units, descriptive wording, and requested granularity exact. Prefer an explicit source phrase over a nearby effect or broader paraphrase.",
        );
        rules.push(
            "A single stable and unambiguous public relation may bridge an explicit evidence anchor to a requested public value, such as containment, creator attribution, or category membership. Public knowledge must not create a personal event, preference, or uncertain attribute; abstain when that bridge is ambiguous.",
        );

        rules.push(match self.plan.answer_shape {
            AnswerShape::Fact if self.plan.recall_intent == RecallIntent::Temporal => {
                "Use the source observation and resolved event times to select the passage covering the requested date or interval, then return the requested attribute rather than the time anchor."
            }
            AnswerShape::Fact => {
                "Return the most explicit supported value with the semantic type and specificity requested by the question; do not substitute a nearby detail."
            }
            AnswerShape::Temporal if requests_duration => match stage {
                RecallReaderStage::Answer => {
                    "Build the answer from one source-grounded chronological event chain. Establish the target entity identity, a start or first active observation, any projection and intervening progress, and a completion or end observation. Resolve an alias, pronoun, or generic reference only when ownership remains with the same speaker and either same-session linkage or compatible cross-session event continuity establishes the same event; lexical similarity alone is insufficient. A projection is a forecast rather than an observed end. Use an explicit source-stated duration when available; otherwise compute elapsed time only from grounded start and completion or end timestamps, preserve approximate wording, and never use retrieval time."
                }
                RecallReaderStage::Reflection => {
                    "Build a source-cited chronological event chain before drafting. Create and inspect required slots for entity identity, start or projection, intervening progress, completion or end, and elapsed duration; fill every available slot from exact source ids, and mark an inspected-but-absent progress observation instead of inventing one. Resolve an alias, pronoun, or generic reference only when ownership remains with the same speaker and either same-session linkage or compatible cross-session event continuity establishes the same event; lexical similarity alone is insufficient. Treat a projection as a forecast, not an observed completion. Use an explicit source-stated duration when available; otherwise derive the duration only from grounded start and completion or end timestamps, preserving approximate wording. Do not declare the endpoints missing until the whole delivered evidence has been checked for that grounded event chain."
                }
                RecallReaderStage::Verification => {
                    "Rebuild the source-cited chronological event chain before accepting the draft or an abstention. Verify the entity identity, start or projection, intervening progress, completion or end, and elapsed duration slots against the whole delivered evidence. Merge aliases or generic references only under the same-speaker ownership and same-session linkage or compatible cross-session event-continuity rules; reject a merge based only on lexical similarity. Treat a projection as a forecast rather than an observed completion. Preserve an explicit grounded duration, or recompute elapsed time only from grounded start and completion or end timestamps, and correct any value based on retrieval time."
                }
            },
            AnswerShape::Temporal => {
                "Resolve a relative expression in the question against the supplied question time, and one in evidence against that source's observation time, never retrieval time. If a source says an activity has continued for a duration, subtract that duration from its event time to infer the start. Otherwise identify the grounded start and end of the same event and compute their elapsed interval, preserving approximate wording when appropriate."
            }
            AnswerShape::Frequency => {
                "Identify repeated instances of the same requested event, order them by source-resolved event time, and state the supported cadence. Distinguish an explicit schedule from observed recurrence and do not substitute a raw count."
            }
            AnswerShape::Count => match stage {
                RecallReaderStage::Answer => {
                    "Scan the whole delivered evidence, enumerate distinct eligible supported events or items, and only then count. Merge continuation, photo, and retelling passages that describe the same speaker, session, time, and activity unless the sources establish separate occurrences. Exclude plans, hypotheticals, and unobserved instances unless the question requests them."
                }
                RecallReaderStage::Reflection => {
                    "Build a complete source-cited event ledger from the whole delivered evidence before counting. For every candidate occurrence, distinguish an eligible event from a plan or hypothetical and from another representation of the same speaker, session, time, and activity. Keep only distinct eligible occurrences in the final items, but continue scanning for omitted events before deriving the count."
                }
                RecallReaderStage::Verification => {
                    "Recompute the count from the source-cited event items instead of trusting the draft number. Merge continuation, photo, and retelling passages that describe one occurrence; remove plans, hypotheticals, and unsupported instances unless the question requests them; and rescan the whole delivered evidence for an omitted eligible event before returning the corrected count."
                }
            },
            AnswerShape::Collection => {
                "Return every distinct supported item requested by the question. Check the whole delivered evidence, preserve ownership, deduplicate paraphrases, and exclude plans or merely plausible additions. If one grounded public anchor has multiple canonical one-hop values of the requested plural type, preserve all supported alternatives."
            }
            AnswerShape::Relationship => {
                "Combine all passages required to state the directed relationship, comparison, reason, or causal connection. Preserve attribution, modality, and temporal order, and do not stop at an intermediate fact."
            }
            AnswerShape::Inference => {
                "Derive the shortest conventional conclusion whose personal premises are grounded in evidence. For likely, might, could, or whether questions, return the best-supported plausible conclusion without requiring the source to state the prediction verbatim. A person's explicit evidence may support a strongly diagnostic implication, but public knowledge alone must not create a personal fact. Prefer an explicitly linked reason, goal, preference, or consequence over unrelated co-occurrence; preserve equally plausible alternatives when the evidence cannot distinguish them."
            }
        });

        match self.answer_form {
            ReaderAnswerForm::Direct => {}
            ReaderAnswerForm::Binary => rules.push(match stage {
                RecallReaderStage::Reflection => {
                    "Draft an explicit yes/no polarity with one short concrete supporting phrase; preserve likely or uncertain modality when the question or evidence calls for it. An abstention is not a negative answer."
                }
                RecallReaderStage::Answer | RecallReaderStage::Verification => {
                    "Return an explicit yes/no polarity followed by the shortest concrete supporting phrase; preserve likely or uncertain modality when the question or evidence calls for it. An abstention is not a negative answer."
                }
            }),
            ReaderAnswerForm::Alternatives => rules.push(match stage {
                RecallReaderStage::Reflection => {
                    "Compare every named alternative separately and draft the supported alternative by name. Preserve multiple alternatives only when the evidence does not distinguish them."
                }
                RecallReaderStage::Answer | RecallReaderStage::Verification => {
                    "Compare every named alternative separately and answer with the supported alternative name, not a bare yes/no. Preserve multiple alternatives only when the evidence does not distinguish them."
                }
            }),
        }

        rules.push(match stage {
            RecallReaderStage::Answer => {
                "Return only the shortest complete answer. Abstain only when a required grounded premise is absent or materially ambiguous."
            }
            RecallReaderStage::Reflection => {
                "Keep the analysis bounded: retain only facts that fill a required slot, the minimal reasoning chain, source-cited final values or events, a candidate answer, and any genuinely missing or ambiguous premise. A citation grounds the premise used by a temporal calculation, stable public one-hop relation, or strongly diagnostic implication; the derived answer value need not appear verbatim in the source. Resolve references such as home country, partner, or that event to a specific value whenever the evidence permits."
            }
            RecallReaderStage::Verification => {
                "Treat citations as grounding for the draft's premises, not as a requirement that every derived answer word appear verbatim in a source. Preserve a source-cited candidate when it has the requested semantic type and no concrete contradiction or required missing premise is identified. In particular, do not replace it with an abstention merely because it contains verified temporal arithmetic, one stable public relation, or a strongly diagnostic implication. Rescan for omitted required slots, compound answer parts, list items, count events, temporal endpoints, and contradictions. Abstain only when neither the delivered evidence nor a valid grounded derivation bears on a required answer slot. Return only the shortest verified complete answer, without source ids or reasoning."
            }
        });
        rules.join(" ")
    }

    /// Concise instruction embedded in query-aware rendered context.
    pub fn context_guidance(&self) -> String {
        product_reader_guidance_for_plan(&self.plan)
    }

    /// Reconcile a typed draft with trusted visible source ownership.
    ///
    /// Only collection omission is eligible for deterministic repair. Plain id
    /// membership does not verify the meaning of a candidate conclusion, so
    /// polarity and alternative choices remain the final reader's decision.
    pub fn reconcile_grounded_draft_with_attributions(
        &self,
        draft: &GroundedAnswerDraft,
        final_answer: &str,
        allowed_source_node_ids: &[NodeId],
        source_attributions: &[RecallSourceAttribution],
    ) -> Option<String> {
        if self.plan.answer_shape != AnswerShape::Collection
            || draft.missing_or_ambiguous
            || draft.candidate_answer.trim().is_empty()
        {
            return None;
        }
        let allowed: HashSet<_> = allowed_source_node_ids.iter().copied().collect();
        if allowed.is_empty()
            || draft.cited_source_node_ids.is_empty()
            || draft
                .cited_source_node_ids
                .iter()
                .any(|source_id| !allowed.contains(source_id))
        {
            return None;
        }

        if !collection_ownership_is_verified(&self.plan.query, draft, source_attributions) {
            return None;
        }
        let items = validated_collection_items(draft, &allowed)?;
        if collection_answer_misses_item(final_answer, &items) {
            return Some(items.join(", "));
        }
        None
    }
}

fn collection_ownership_is_verified(
    query: &str,
    draft: &GroundedAnswerDraft,
    source_attributions: &[RecallSourceAttribution],
) -> bool {
    let speakers: HashSet<_> = source_attributions
        .iter()
        .filter_map(|source| source.speaker.as_deref())
        .collect();
    if speakers.is_empty() {
        return false;
    }
    let normalized_query = normalized_phrase(query);
    let mut matching_speakers = speakers.iter().copied().filter(|speaker| {
        let normalized_speaker = normalized_phrase(speaker);
        !normalized_speaker.is_empty()
            && normalized_contains_phrase(&normalized_query, &normalized_speaker)
    });
    let target = matching_speakers.next();
    if matching_speakers.next().is_some() {
        return false;
    }
    let target = match target {
        Some(target) => Some(target),
        None if speakers.len() == 1 => speakers.iter().copied().next(),
        None => None,
    };
    let Some(target) = target else {
        return false;
    };

    draft.answer_items.iter().all(|item| {
        item.source_node_ids.iter().all(|source_node_id| {
            let owners: Vec<_> = source_attributions
                .iter()
                .filter(|source| source.source_node_id == *source_node_id)
                .filter_map(|source| source.speaker.as_deref())
                .collect();
            !owners.is_empty()
                && owners
                    .iter()
                    .all(|speaker| speaker.eq_ignore_ascii_case(target))
        })
    })
}

/// One answer item cited by a provider-neutral evidence-analysis draft.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroundedAnswerItem {
    /// Final value for this distinct item or counted event.
    pub value: String,
    /// Exact source nodes cited for this item.
    pub source_node_ids: Vec<NodeId>,
}

impl GroundedAnswerItem {
    /// Create one source-cited draft item.
    pub fn new(value: impl Into<String>, source_node_ids: Vec<NodeId>) -> Self {
        Self {
            value: value.into(),
            source_node_ids,
        }
    }
}

/// Typed provider output accepted by deterministic reader reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroundedAnswerDraft {
    /// Proposed shortest complete answer.
    pub candidate_answer: String,
    /// Source-cited final values, or distinct list items and counted events.
    pub answer_items: Vec<GroundedAnswerItem>,
    /// Every source node cited anywhere in the analysis.
    pub cited_source_node_ids: Vec<NodeId>,
    /// Whether the analysis found a required missing or ambiguous premise.
    pub missing_or_ambiguous: bool,
}

impl GroundedAnswerDraft {
    /// Create a typed draft parsed by a consumer adapter.
    pub fn new(
        candidate_answer: impl Into<String>,
        answer_items: Vec<GroundedAnswerItem>,
        cited_source_node_ids: Vec<NodeId>,
        missing_or_ambiguous: bool,
    ) -> Self {
        Self {
            candidate_answer: candidate_answer.into(),
            answer_items,
            cited_source_node_ids,
            missing_or_ambiguous,
        }
    }
}

fn validated_collection_items(
    draft: &GroundedAnswerDraft,
    allowed: &HashSet<NodeId>,
) -> Option<Vec<String>> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for item in &draft.answer_items {
        let value = item.value.trim();
        if value.is_empty()
            || item.source_node_ids.is_empty()
            || item
                .source_node_ids
                .iter()
                .any(|source_id| !allowed.contains(source_id))
        {
            return None;
        }
        let normalized = normalize_collection_item(value);
        if normalized.is_empty() {
            return None;
        }
        if seen.insert(normalized) {
            items.push(value.to_owned());
        }
    }
    (!items.is_empty()).then_some(items)
}

fn collection_answer_misses_item(answer: &str, items: &[String]) -> bool {
    let answer_tokens = collection_token_counts(answer);
    items.iter().any(|item| {
        let item_tokens = collection_token_counts(item);
        !item_tokens.is_empty()
            && item_tokens.iter().any(|(token, count)| {
                answer_tokens.get(token).copied().unwrap_or_default() < *count
            })
    })
}

fn normalize_collection_item(value: &str) -> String {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn collection_token_counts(value: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
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

fn query_presents_explicit_alternatives(query: &str) -> bool {
    format!(" {} ", query.trim().to_lowercase()).contains(" or ")
}

fn query_starts_with_binary_auxiliary(query: &str) -> bool {
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

#[cfg(test)]
fn product_reader_guidance(plan: &RecallPlan) -> String {
    plan.reader_contract().context_guidance()
}

fn query_requests_elapsed_duration(query: &str) -> bool {
    let normalized_query = normalized_phrase(query);
    normalized_contains_phrase(&normalized_query, "how long")
        || normalized_query.contains("duration")
        || normalized_query.contains("elapsed")
        || normalized_query.contains("얼마나 오래")
        || normalized_query.contains("얼마 동안")
}

fn product_reader_guidance_for_plan(plan: &RecallPlan) -> String {
    let requests_duration = query_requests_elapsed_duration(&plan.query);
    let temporal_fact = plan.recall_intent == RecallIntent::Temporal
        && !matches!(
            plan.answer_shape,
            AnswerShape::Temporal | AnswerShape::Frequency
        );
    let inference_collection =
        plan.answer_shape == AnswerShape::Collection && query_has_inference_modal(&plan.query);
    let guidance = match plan.answer_shape {
        AnswerShape::Fact if temporal_fact => {
            "Use the source and resolved event times to choose the evidence that covers the \
             requested date or interval, then answer the requested attribute exactly. Observation \
             time is only an anchor for resolving relative expressions such as yesterday, last \
             week, or for a month; do not substitute a nearby event."
        }
        AnswerShape::Fact => {
            "Prefer the most explicit source passage. Answer the requested attribute and \
             granularity exactly; when asked how a source describes something, preserve its \
             explicit descriptive word or phrase rather than a nearby effect or paraphrase. Do \
             not substitute a nearby detail, and do not abstain when an explicit answer is \
             present."
        }
        AnswerShape::Temporal if requests_duration => {
            "Build one source-grounded chronological event chain: establish entity identity, a \
             start or projection, intervening progress, and completion or end. Resolve aliases \
             and generic references only under same-speaker ownership plus same-session linkage \
             or compatible cross-session event continuity; lexical similarity alone is not \
             enough. A projection is a forecast, not an observed end. Use an explicit grounded \
             duration when available; otherwise compute elapsed time only from grounded start and \
             completion or end timestamps. Preserve approximate wording and never use retrieval \
             time."
        }
        AnswerShape::Temporal => {
            "Answer from source and resolved event-time annotations. Resolve relative expressions \
             against their source observation, and preserve the event's time rather than \
             substituting retrieval time."
        }
        AnswerShape::Frequency => {
            "Infer cadence only from repeated dated observations, and distinguish an explicit \
             schedule from an observed recurrence."
        }
        AnswerShape::Count => {
            "Count distinct supported events or items once. Do not infer unobserved instances or \
             count duplicate representations of the same source."
        }
        AnswerShape::Collection if inference_collection => {
            "Return every distinct plausible item requested by the question. Ground all personal \
             or changing premises in the evidence, then use stable, widely known background \
             knowledge only to bridge those premises; preserve alternatives instead of choosing \
             one arbitrarily."
        }
        AnswerShape::Collection => {
            "Return every distinct item explicitly supported by the evidence. Preserve subject \
             and source ownership, and do not add merely plausible items."
        }
        AnswerShape::Relationship if plan.recall_intent == RecallIntent::Temporal => {
            "Combine the source-grounded time or event anchor with the evidence that resolves its \
             referenced entity, place, or relationship. State the resulting directed relation \
             concisely while preserving attribution, modality, and temporal order."
        }
        AnswerShape::Relationship => {
            "Combine every source passage required by the relationship, then state only the \
             directed relationship, comparison, or reason they support. Preserve attribution, \
             modality, and temporal order."
        }
        AnswerShape::Inference => {
            "Ground every personal, session-specific, or changing premise in the source \
             evidence. Stable, widely known background knowledge may bridge those grounded \
             premises to the most conventional value or one concise conclusion; never invent \
             personal facts or merely restate the clue. For a yes/no or likely question, give the \
             supported conclusion even when the source does not state a prediction verbatim. \
             Preserve modality and time, and treat the evidence as insufficient only when a \
             required source-grounded premise is absent or materially ambiguous."
        }
    };
    guidance.to_owned()
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
    /// This protects multi-event and multi-source queries from spending their
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
    /// Build the latency-sensitive profile shared by product consumers.
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
        || has_word("duration")
        || has_word("elapsed")
        || normalized.contains("언제")
        || normalized.contains("얼마나 오래")
        || normalized.contains("얼마 동안")
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
            || has_word("meet")
            || has_word("met")
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
    if plan.recall_intent == RecallIntent::Direct {
        requested_limit.min(DEFAULT_SIMPLE_DELIVERY_LIMIT)
    } else {
        requested_limit
    }
}

pub(crate) fn temporal_evidence_matches(query: &str, evidence: &str) -> bool {
    fn normalized_term(mut term: String) -> String {
        if term.len() > 5 && (term.ends_with("ies") || term.ends_with("ied")) {
            term.truncate(term.len() - 3);
            term.push('y');
        } else if term.len() > 5 && term.ends_with("ing") {
            term.truncate(term.len() - 3);
            let bytes = term.as_bytes();
            let doubled_ending =
                bytes.len() >= 2 && bytes[bytes.len() - 1] == bytes[bytes.len() - 2];
            if doubled_ending {
                term.pop();
            }
        } else if term.len() > 4 && term.ends_with("ed") {
            term.truncate(term.len() - 2);
            let bytes = term.as_bytes();
            let doubled_ending =
                bytes.len() >= 2 && bytes[bytes.len() - 1] == bytes[bytes.len() - 2];
            if doubled_ending {
                term.pop();
            }
        } else if term.len() > 4 && term.ends_with("es") {
            term.truncate(term.len() - 2);
        } else if term.len() > 3 && term.ends_with('s') && !term.ends_with("ss") {
            term.pop();
        }
        term
    }

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
            .map(normalized_term)
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

#[cfg(test)]
pub(crate) fn compile_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    selection: EvidenceSelection,
    limit: usize,
    routed_atomic_sources: &[AtomicSourceMarker],
) -> Result<Vec<RerankedCandidate>, Error> {
    compile_ranking_with_atomic_chains(
        storage,
        plan,
        ranking,
        selection,
        limit,
        routed_atomic_sources,
        &[],
    )
}

pub(crate) fn compile_ranking_with_atomic_chains<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    selection: EvidenceSelection,
    limit: usize,
    routed_atomic_sources: &[AtomicSourceMarker],
    atomic_relation_paths: &[AtomicRelationPath],
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
    let claim_ranking = claim_slot_coverage_ranking(
        storage,
        plan,
        ranking,
        &baseline,
        limit,
        routed_atomic_sources,
    )?;
    atomic_chain_group_ranking(
        storage,
        plan,
        ranking,
        &claim_ranking,
        limit,
        atomic_relation_paths,
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
    // Marker order is the atomic router's query-relative ranking. Preserve it
    // through final evidence selection instead of letting HashMap iteration or
    // a second-stage document rank silently choose a different claim.
    let mut claim_priority = HashMap::new();
    for (priority, marker) in routed_atomic_sources.iter().enumerate() {
        let Some(fact_id) = marker.fact_id else {
            continue;
        };
        claim_priority.entry(fact_id).or_insert(priority);
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
                if claim_sources
                    .get(&fact_id)
                    .is_some_and(|sources| sources.contains(&evidence_source)) =>
            {
                match storage.get_node(evidence_source) {
                    Ok(source) => match source.content.get(start..end) {
                        Some(evidence_span)
                            if if requires_exact_object {
                                evidence_span.contains(object)
                            } else {
                                normalized_phrase(evidence_span)
                                    .contains(&normalized_phrase(object))
                            } =>
                        {
                            ClaimCoverage::Grounded {
                                evidence_source,
                                evidence_span: evidence_span.to_owned(),
                            }
                        }
                        _ => ClaimCoverage::Invalid,
                    },
                    Err(Error::NodeNotFound(_)) => ClaimCoverage::Invalid,
                    Err(error) => return Err(error),
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
    let grounded_reserve_limit = match plan.answer_shape {
        AnswerShape::Collection if query_has_inference_modal(&plan.query) => 2,
        AnswerShape::Collection => 1,
        AnswerShape::Inference => 1,
        _ => 0,
    };
    let mut grounded_reserves_used = 0usize;
    const MAX_CLAIM_REPLACEMENTS: usize = 4;

    while !missing.is_empty() && replacements < MAX_CLAIM_REPLACEMENTS {
        let selected_ids: HashSet<_> = selected.iter().map(|candidate| candidate.node_id).collect();
        let best_candidate = ranking
            .iter()
            .enumerate()
            .filter(|(_, candidate)| !selected_ids.contains(&candidate.node_id))
            .filter_map(|(rank, candidate)| {
                let claims = covered_claims(candidate.node_id);
                let missing_claims = claims
                    .iter()
                    .filter(|fact_id| missing.contains(fact_id))
                    .copied()
                    .collect::<Vec<_>>();
                let gain = missing_claims.len();
                let grounded_gain = missing_claims
                    .iter()
                    .filter(|fact_id| {
                        matches!(
                            claim_coverage.get(fact_id),
                            Some(ClaimCoverage::Grounded { .. })
                        )
                    })
                    .count();
                let best_claim_priority = missing_claims
                    .iter()
                    .filter_map(|fact_id| claim_priority.get(fact_id))
                    .copied()
                    .min()
                    .unwrap_or(usize::MAX);
                (gain > 0).then_some((
                    rank,
                    *candidate,
                    claims,
                    gain,
                    grounded_gain,
                    best_claim_priority,
                ))
            })
            .max_by(|left, right| {
                left.4
                    .cmp(&right.4)
                    .then_with(|| left.3.cmp(&right.3))
                    .then_with(|| right.5.cmp(&left.5))
                    .then_with(|| right.0.cmp(&left.0))
            });
        let Some((_, candidate, candidate_claims, _, _, _)) = best_candidate else {
            break;
        };
        let candidate_has_missing_grounded_claim = candidate_claims.iter().any(|fact_id| {
            missing.contains(fact_id)
                && matches!(
                    claim_coverage.get(fact_id),
                    Some(ClaimCoverage::Grounded { .. })
                )
        });

        if selected.len() < limit {
            selected.push(candidate);
        } else {
            let victim = (head_limit..selected.len()).rev().find_map(|index| {
                let victim_node_id = selected[index].node_id;
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
                let victim_claims = covered_claims(victim_node_id);
                let preserves_claim_coverage = victim_claims.iter().all(|fact_id| {
                    coverage_counts.get(fact_id).copied().unwrap_or_default()
                        + usize::from(candidate_claims.contains(fact_id))
                        > 1
                });
                let uses_grounded_complex_reserve = grounded_reserves_used < grounded_reserve_limit
                    && candidate_has_missing_grounded_claim
                    && victim_claims.is_empty();
                ((preserves_raw_evidence || uses_grounded_complex_reserve)
                    && preserves_claim_coverage)
                    .then_some((
                        index,
                        uses_grounded_complex_reserve && !preserves_raw_evidence,
                    ))
            });
            let Some((victim, used_grounded_reserve)) = victim else {
                break;
            };
            selected[victim] = candidate;
            grounded_reserves_used += usize::from(used_grounded_reserve);
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

fn atomic_chain_group_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    baseline: &[RerankedCandidate],
    limit: usize,
    paths: &[AtomicRelationPath],
) -> Result<Vec<RerankedCandidate>, Error> {
    const MAX_CHAIN_GROUPS: usize = 2;
    const MAX_CHAIN_ADDITIONS: usize = 4;

    if limit == 0
        || paths.is_empty()
        || plan.recall_intent == RecallIntent::Temporal
        || !matches!(
            plan.answer_shape,
            AnswerShape::Relationship | AnswerShape::Inference
        )
    {
        return Ok(baseline.to_vec());
    }

    let mut source_cache: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
    for candidate in ranking.iter().chain(baseline) {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            source_cache.entry(candidate.node_id)
        {
            entry.insert(
                canonical_sources(storage, candidate.node_id)?
                    .into_iter()
                    .collect(),
            );
        }
    }

    let covers = |candidates: &[RerankedCandidate], required: &HashSet<NodeId>| {
        let mut covered = HashSet::new();
        for candidate in candidates {
            if let Some(sources) = source_cache.get(&candidate.node_id) {
                covered.extend(sources.iter().copied());
            }
        }
        required.iter().all(|source| covered.contains(source))
    };

    let mut selected: Vec<_> = baseline.iter().take(limit).copied().collect();
    let head_limit = selected
        .len()
        .min(limit.saturating_mul(3).div_ceil(5).max(1));
    let mut protected_sources = HashSet::new();
    let mut accepted_groups = 0usize;
    let mut additions_used = 0usize;
    let mut changed = false;

    for path in paths {
        if accepted_groups >= MAX_CHAIN_GROUPS || additions_used >= MAX_CHAIN_ADDITIONS {
            break;
        }
        let required: HashSet<_> = path.source_groups.iter().flatten().copied().collect();
        if required.is_empty() {
            continue;
        }
        if covers(&selected, &required) {
            protected_sources.extend(required);
            continue;
        }

        let representable: HashSet<_> = ranking
            .iter()
            .filter_map(|candidate| source_cache.get(&candidate.node_id))
            .flat_map(|sources| sources.iter().copied())
            .collect();
        if !required.iter().all(|source| representable.contains(source)) {
            continue;
        }

        let selected_ids: HashSet<_> = selected.iter().map(|candidate| candidate.node_id).collect();
        let mut additions = Vec::new();
        let mut missing = required
            .iter()
            .filter(|source| {
                !selected.iter().any(|candidate| {
                    source_cache
                        .get(&candidate.node_id)
                        .is_some_and(|sources| sources.contains(source))
                })
            })
            .copied()
            .collect::<HashSet<_>>();
        while !missing.is_empty() && additions_used + additions.len() < MAX_CHAIN_ADDITIONS {
            let addition_ids: HashSet<_> = additions
                .iter()
                .map(|candidate: &RerankedCandidate| candidate.node_id)
                .collect();
            let best = ranking
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    !selected_ids.contains(&candidate.node_id)
                        && !addition_ids.contains(&candidate.node_id)
                })
                .filter_map(|(rank, candidate)| {
                    let gain = source_cache
                        .get(&candidate.node_id)
                        .map(|sources| sources.intersection(&missing).count())
                        .unwrap_or_default();
                    (gain > 0).then_some((gain, rank, *candidate))
                })
                .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));
            let Some((_, _, addition)) = best else {
                break;
            };
            if let Some(sources) = source_cache.get(&addition.node_id) {
                missing.retain(|source| !sources.contains(source));
            }
            additions.push(addition);
        }
        if !missing.is_empty() || additions.is_empty() {
            continue;
        }

        let spare = limit.saturating_sub(selected.len());
        let replacements_needed = additions.len().saturating_sub(spare);
        let mut required_after_change = protected_sources.clone();
        required_after_change.extend(required.iter().copied());
        let mut victims = Vec::new();
        for index in (head_limit..selected.len()).rev() {
            if victims.len() >= replacements_needed {
                break;
            }
            let mut trial = selected
                .iter()
                .enumerate()
                .filter(|(candidate_index, _)| {
                    *candidate_index != index && !victims.contains(candidate_index)
                })
                .map(|(_, candidate)| *candidate)
                .collect::<Vec<_>>();
            trial.extend(additions.iter().copied());
            if covers(&trial, &required_after_change) {
                victims.push(index);
            }
        }
        if victims.len() != replacements_needed {
            continue;
        }
        victims.sort_unstable();
        let mut next = selected
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, _)| victims.binary_search(index).is_err())
            .map(|(_, candidate)| candidate)
            .collect::<Vec<_>>();
        next.extend(additions.iter().copied());
        if next.len() > limit || !covers(&next, &required_after_change) {
            // No partial chain is admitted when the complete bounded source
            // group cannot survive the final evidence width.
            continue;
        }
        selected = next;
        protected_sources = required_after_change;
        accepted_groups += 1;
        additions_used += additions.len();
        changed = true;
    }

    if changed {
        Ok(selected)
    } else {
        Ok(baseline.to_vec())
    }
}

pub(super) fn compile_atomic_chain_source_ranking<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    ranking: &[RerankedCandidate],
    limit: usize,
    paths: &[AtomicRelationPath],
) -> Result<Vec<RerankedCandidate>, Error> {
    atomic_chain_group_ranking(storage, plan, ranking, ranking, limit, paths)
}

fn query_has_inference_modal(query: &str) -> bool {
    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .any(|term| {
            matches!(
                term.to_ascii_lowercase().as_str(),
                "could" | "likely" | "might" | "possibly" | "potentially" | "probably" | "would"
            )
        })
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

fn temporal_predecessor<S: StorageAdapter>(
    storage: &S,
    source: NodeId,
) -> Result<Option<NodeId>, Error> {
    let answer = storage.get_node(source)?;
    let mut predecessors = Vec::new();
    for edge_id in storage.edges_to(source) {
        let edge = storage.get_edge(*edge_id)?;
        if edge.edge_type != EdgeType::Temporal {
            continue;
        }
        let predecessor = storage.get_node(edge.source)?;
        if predecessor.node_type == KnowledgeType::Episodic
            && predecessor.origin.session_id == answer.origin.session_id
            && predecessor.origin.scope == answer.origin.scope
            && predecessor.created_at <= answer.created_at
            && predecessor
                .valid_from
                .is_none_or(|valid_from| valid_from <= answer.created_at)
            && predecessor.valid_until.is_none()
            && !predecessor
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
        {
            predecessors.push((predecessor.created_at, predecessor.id));
        }
    }
    predecessors.sort_unstable();
    Ok(predecessors.pop().map(|(_, node_id)| node_id))
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
                documents.push(EvidenceDocument::from_raw(source_id, vec![source_id], text));
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
                document.rerank_text = document.text.clone();
            }
        } else {
            let text = if fallback_text.trim().is_empty() {
                storage.get_node(candidate.node_id)?.content.clone()
            } else {
                fallback_text
            };
            let index = documents.len();
            documents.push(EvidenceDocument::from_raw(
                candidate.node_id,
                fallback_sources,
                text,
            ));
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
        documents.push(EvidenceDocument::from_raw(
            representative,
            source_node_ids,
            text,
        ));
    }
    Ok(documents)
}

const MAX_SAME_SESSION_REPLY_BRIDGES: usize = 2;
const MAX_REPLY_QUESTION_CHARS: usize = 512;

#[derive(Debug, Clone, Copy)]
struct SameSessionReplyBridge {
    answer_document_index: usize,
    question_source: NodeId,
    query_facet_overlap: usize,
}

/// Add bounded dialogue context to the reranker surface without changing the
/// authoritative evidence documents.
///
/// A raw answer can score poorly after a Semantic dialogue window is split
/// into canonical source documents because the question that gives the answer
/// its meaning lives in the preceding turn. Reattach that question only for
/// scoring when the answer is already a native candidate, the predecessor is
/// a live same-scope turn connected by an immediate same-session temporal edge,
/// and the pair matches the query. The document count, representative ids, raw
/// source ids, and emitted text remain byte-for-byte unchanged.
fn apply_bounded_same_session_reply_context<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    documents: &mut [EvidenceDocument],
) -> Result<(), Error> {
    if plan.recall_intent != RecallIntent::Relational || documents.len() < 2 {
        return Ok(());
    }
    let query_facets = facet_terms(&plan.query);
    if query_facets.is_empty() {
        return Ok(());
    }
    let required_overlap = query_facets.len().min(2);

    let mut source_documents = HashMap::new();
    for (document_index, document) in documents.iter().enumerate() {
        for source_id in &document.source_node_ids {
            source_documents.entry(*source_id).or_insert(document_index);
        }
    }
    let mut native_answers: Vec<_> = source_documents.keys().copied().collect();
    native_answers.sort_unstable();

    let mut bridge_by_answer = HashMap::new();
    for answer_source in native_answers {
        let Some(question_source) = temporal_predecessor(storage, answer_source)? else {
            continue;
        };
        let question = storage.get_node(question_source)?;
        if question.node_type != KnowledgeType::Episodic
            || !question.content.trim_end().ends_with('?')
        {
            continue;
        }
        let Some(&answer_document_index) = source_documents.get(&answer_source) else {
            continue;
        };
        if documents[answer_document_index]
            .source_node_ids
            .contains(&question_source)
        {
            continue;
        }

        let answer = storage.get_node(answer_source)?;
        let pair_facets = facet_terms(&format!("{} {}", question.content, answer.content));
        let query_facet_overlap = query_facets.intersection(&pair_facets).count();
        if query_facet_overlap < required_overlap {
            continue;
        }
        let candidate = SameSessionReplyBridge {
            answer_document_index,
            question_source,
            query_facet_overlap,
        };
        bridge_by_answer
            .entry(answer_document_index)
            .and_modify(|current: &mut SameSessionReplyBridge| {
                if candidate.query_facet_overlap > current.query_facet_overlap
                    || (candidate.query_facet_overlap == current.query_facet_overlap
                        && candidate.question_source < current.question_source)
                {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    }

    let mut bridges: Vec<_> = bridge_by_answer.into_values().collect();
    bridges.sort_by(|left, right| {
        right
            .query_facet_overlap
            .cmp(&left.query_facet_overlap)
            .then_with(|| left.answer_document_index.cmp(&right.answer_document_index))
            .then_with(|| left.question_source.cmp(&right.question_source))
    });
    bridges.truncate(MAX_SAME_SESSION_REPLY_BRIDGES);

    for bridge in bridges {
        let question = render_source(storage, bridge.question_source)?;
        let mut chars = question.chars();
        let bounded_question: String = chars.by_ref().take(MAX_REPLY_QUESTION_CHARS).collect();
        let bounded_question = if chars.next().is_some() {
            format!("{bounded_question}…")
        } else {
            bounded_question
        };
        let document = &mut documents[bridge.answer_document_index];
        let answer_surface = std::mem::take(&mut document.rerank_text);
        document.rerank_text = format!(
            "Immediate same-session question:\n{bounded_question}\nResponse evidence:\n{answer_surface}"
        );
    }

    Ok(())
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
        "favorite" | "favorites" | "favourite" | "favourites" | "preferred" | "prefers" => {
            "prefer".to_owned()
        }
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

pub(super) fn facet_terms(value: &str) -> HashSet<String> {
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
    subject_matches: usize,
    kind_priority: usize,
    source_session_id: String,
    source_node_ids: Vec<NodeId>,
}

const MAX_ATOMIC_ROUTING_COMPONENT_BYTES: usize = 1_024;
const MAX_ATOMIC_ROUTING_SURFACE_BYTES: usize = 4_096;
const MAX_ATOMIC_ROUTING_SPAN_BYTES: usize = 2_048;

struct AtomicRoutingMetadata<'a> {
    subject: &'a str,
    relation: &'a str,
    object: &'a str,
    evidence_object: &'a str,
    evidence_source: NodeId,
    evidence_start: usize,
    evidence_end: usize,
    requires_exact_object: bool,
}

fn bounded_atomic_routing_component(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= MAX_ATOMIC_ROUTING_COMPONENT_BYTES
        && !value.chars().any(char::is_control))
    .then_some(value)
}

fn atomic_routing_metadata(fact: &AtomicFact) -> Option<AtomicRoutingMetadata<'_>> {
    let subject = bounded_atomic_routing_component(fact.metadata.get("anamnesis:ground-subject")?)?;
    let relation =
        bounded_atomic_routing_component(fact.metadata.get("anamnesis:ground-relation")?)?;
    let object = bounded_atomic_routing_component(fact.metadata.get("anamnesis:ground-object")?)?;
    let requires_exact_object = fact.metadata.contains_key("anamnesis:evidence-object");
    let evidence_object = bounded_atomic_routing_component(
        fact.metadata
            .get("anamnesis:evidence-object")
            .map(String::as_str)
            .unwrap_or(object),
    )?;
    let surface_bytes = subject
        .len()
        .saturating_add(relation.len())
        .saturating_add(object.len())
        .saturating_add(evidence_object.len());
    if surface_bytes > MAX_ATOMIC_ROUTING_SURFACE_BYTES {
        return None;
    }

    let evidence_source = fact
        .metadata
        .get("anamnesis:evidence-source-node-id")?
        .parse::<u64>()
        .ok()
        .map(NodeId)?;
    if !fact.source_node_ids.contains(&evidence_source) {
        return None;
    }
    let evidence_start = fact
        .metadata
        .get("anamnesis:evidence-span-start")?
        .parse::<usize>()
        .ok()?;
    let evidence_end = fact
        .metadata
        .get("anamnesis:evidence-span-end")?
        .parse::<usize>()
        .ok()?;
    if evidence_end <= evidence_start
        || evidence_end.saturating_sub(evidence_start) > MAX_ATOMIC_ROUTING_SPAN_BYTES
    {
        return None;
    }

    Some(AtomicRoutingMetadata {
        subject,
        relation,
        object,
        evidence_object,
        evidence_source,
        evidence_start,
        evidence_end,
        requires_exact_object,
    })
}

fn atomic_routing_metadata_terms<S: StorageAdapter>(
    storage: &S,
    fact: &AtomicFact,
    query_terms: &HashSet<String>,
    now: crate::graph::Timestamp,
    scope: &ScopePath,
) -> Result<HashSet<String>, Error> {
    let Some(metadata) = atomic_routing_metadata(fact) else {
        return Ok(HashSet::new());
    };
    let mut terms = HashSet::new();
    for value in [
        metadata.subject,
        metadata.relation,
        metadata.object,
        metadata.evidence_object,
    ] {
        terms.extend(facet_terms(value));
    }
    // Hydrate and validate the cited source only when this bounded metadata
    // projection could affect the current lexical route. The full fact sidecar
    // remains scan-based today, so avoiding one raw-node lookup per unrelated
    // fact materially limits the cost of this lane.
    if terms.is_disjoint(query_terms) {
        return Ok(HashSet::new());
    }

    let source = match storage.get_node(metadata.evidence_source) {
        Ok(source) => source,
        Err(Error::NodeNotFound(_)) => return Ok(HashSet::new()),
        Err(error) => return Err(error),
    };
    if source.node_type != KnowledgeType::Episodic
        || source.origin.session_id != fact.source_session_id
        || source.origin.scope != fact.scope
        || !storage.atomic_fact_source_is_current(fact, source)?
        || source.created_at > now
        || source
            .metadata
            .get("retracted")
            .is_some_and(|value| value == "true")
        || !crate::graph::valid_at(source.valid_from, source.valid_until, now)
        || !atomic_scope_is_visible(scope, &source.origin.scope)
    {
        return Ok(HashSet::new());
    }
    let Some(evidence_span) = source
        .content
        .get(metadata.evidence_start..metadata.evidence_end)
    else {
        return Ok(HashSet::new());
    };
    let object_is_grounded = if metadata.requires_exact_object {
        evidence_span.contains(metadata.evidence_object)
    } else {
        normalized_phrase(evidence_span).contains(&normalized_phrase(metadata.evidence_object))
    };
    if !object_is_grounded {
        return Ok(HashSet::new());
    }
    Ok(terms)
}

fn max_query_cosine(query_embeddings: &[&[f64]], candidate_embedding: &[f64]) -> f64 {
    query_embeddings
        .iter()
        .map(|query_embedding| cosine_similarity(query_embedding, candidate_embedding))
        .max_by(f64::total_cmp)
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AtomicRouteOrigin {
    Direct,
    AuxiliaryQuery,
    Chain { depth: usize },
}

#[derive(Debug)]
pub(super) struct RoutedAtomicSource {
    pub candidate: crate::query::ReadoutCandidate,
    pub kind_priority: usize,
    pub fact_ids: Vec<AtomicFactId>,
    pub origin: AtomicRouteOrigin,
}

/// Canonically oriented reviewed relation retained while a chain is traversed.
///
/// `from_fact_id` and `to_fact_id` always preserve the stored relation
/// orientation. They are deliberately independent of the order in which the
/// breadth-first traversal reached the two endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AtomicRelationHop {
    pub(super) relation_id: AtomicFactRelationId,
    pub(super) from_fact_id: AtomicFactId,
    pub(super) to_fact_id: AtomicFactId,
    pub(super) kind: AtomicFactRelationKind,
}

/// One bounded traversal path and the raw evidence needed to render it.
///
/// `fact_ids` follows traversal order. `source_groups` has the same length and
/// contains the complete bounded raw-source group retained for each fact.
/// Selection treats the flattened source set as an indivisible unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AtomicRelationPath {
    pub(super) fact_ids: Vec<AtomicFactId>,
    pub(super) hops: Vec<AtomicRelationHop>,
    pub(super) source_groups: Vec<Vec<NodeId>>,
}

const ATOMIC_CHAIN_MAX_PATHS: usize = 8;
const ATOMIC_CHAIN_MAX_FACTS_PER_PATH: usize = 3;
const ATOMIC_CHAIN_MAX_SOURCES_PER_FACT: usize = 2;
const ATOMIC_CHAIN_MAX_TRACE_BYTES: usize = 4_096;

#[derive(Debug, Default)]
pub(super) struct AtomicChainExpansion {
    pub sources: Vec<RoutedAtomicSource>,
    pub paths: Vec<AtomicRelationPath>,
    pub diagnostics: AtomicChainDiagnostics,
}

#[derive(Debug, Default)]
pub(super) struct AtomicChainDiagnostics {
    pub visited_relations: usize,
    pub expanded_facts: usize,
    pub routed_sources: usize,
    pub contradictions_excluded: usize,
    pub truncated: bool,
}

#[derive(Debug)]
struct SubjectRawCandidate {
    candidate: crate::query::ReadoutCandidate,
    speaker: String,
    session_id: String,
    lexical_overlap: usize,
    lexical_idf_score: f64,
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

fn normalized_contains_phrase(normalized_value: &str, normalized_phrase: &str) -> bool {
    normalized_value == normalized_phrase
        || normalized_value
            .strip_prefix(normalized_phrase)
            .is_some_and(|suffix| suffix.starts_with(' '))
        || normalized_value
            .strip_suffix(normalized_phrase)
            .is_some_and(|prefix| prefix.ends_with(' '))
        || normalized_value.contains(&format!(" {normalized_phrase} "))
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

fn atomic_subject_matches(query: &str, metadata: &HashMap<String, String>) -> usize {
    let Some(subject) = metadata
        .get("anamnesis:ground-subject")
        .map(|subject| normalized_phrase(subject))
        .filter(|subject| subject.len() > 2)
    else {
        return 0;
    };
    usize::from(normalized_contains_phrase(
        &normalized_phrase(query),
        &subject,
    ))
}

fn inference_fact_kind_priority(plan: &RecallPlan, metadata: &HashMap<String, String>) -> usize {
    if plan.answer_shape != AnswerShape::Inference {
        return 0;
    }
    let kind = metadata
        .get("anamnesis:fact-kind")
        .map(|value| value.trim().to_lowercase());
    match kind.as_deref() {
        Some("convention") => 3,
        Some("preference") => 2,
        Some("causal" | "decision" | "lesson") => 1,
        _ => 0,
    }
}

/// Route a bounded set of authoritative raw turns owned by an exact query
/// subject.
///
/// The normal graph and atomic lanes rank all memories globally. A prolific
/// subject can therefore have a semantically useful premise below the fixed
/// candidate surface even though the question names that subject explicitly.
/// This isolated lane uses the speaker provenance already written by
/// [`Memory::add`](super::Memory::add), reuses the existing batched query
/// embeddings, and never exposes a turn from a different speaker. It is active
/// only for explicit, non-hypothetical collections and remains a routing
/// surface; the local reranker and normal final selection still decide what
/// reaches the consumer.
pub(super) fn route_subject_raw_sources<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    query_embeddings: &[&[f64]],
    now: crate::graph::Timestamp,
    scope: &ScopePath,
) -> Result<Vec<crate::query::ReadoutCandidate>, Error> {
    const SOURCE_LIMIT: usize = 8;
    const SESSION_DIVERSE_LIMIT: usize = 6;

    // Raw breadth is useful for explicit factual enumeration, but it pollutes
    // hypothetical/choice questions whose reader must apply one focused
    // premise. Keep inference-modal collections and all relational/inference
    // shapes on their established atomic + graph route.
    if !uses_complex_expansion(plan)
        || plan.answer_shape != AnswerShape::Collection
        || query_has_inference_modal(&plan.query)
    {
        return Ok(Vec::new());
    }

    let normalized_query = normalized_phrase(&plan.query);
    let mut raw_nodes = Vec::new();
    let mut matched_speakers = HashSet::new();
    for node_id in storage.nodes_by_type(&KnowledgeType::Episodic) {
        let node = storage.get_node(node_id)?;
        let (speaker, _) = parse_entity_tags(&node.entity_tags);
        let Some(speaker) = speaker else {
            continue;
        };
        let normalized_speaker = normalized_phrase(&speaker);
        if normalized_speaker.len() < 3
            || matches!(
                normalized_speaker.as_str(),
                "assistant" | "bot" | "human" | "system" | "user"
            )
            || !normalized_contains_phrase(&normalized_query, &normalized_speaker)
        {
            continue;
        }
        matched_speakers.insert(speaker.clone());
        raw_nodes.push((node_id, speaker));
    }
    if raw_nodes.is_empty() {
        return Ok(Vec::new());
    }

    let mut query_terms = facet_terms(&plan.query);
    for speaker in &matched_speakers {
        query_terms.retain(|term| !facet_terms(speaker).contains(term));
    }

    let mut candidates = Vec::with_capacity(raw_nodes.len());
    let mut lexical_document_frequency = HashMap::new();
    for (node_id, speaker) in raw_nodes {
        let node = storage.get_node(node_id)?;
        if node
            .metadata
            .get("retracted")
            .is_some_and(|value| value == "true")
            || node.created_at > now
            || !crate::graph::valid_at(node.valid_from, node.valid_until, now)
            || !atomic_scope_is_visible(scope, &node.origin.scope)
        {
            continue;
        }
        let scope_weight = crate::query::scoring::scope_weight(scope, &node.origin.scope);
        let dense_score = node.embedding.as_ref().map_or(0.0, |embedding| {
            max_query_cosine(query_embeddings, embedding)
        });
        let node_terms = facet_terms(&node.content);
        let matched_terms: HashSet<_> = query_terms.intersection(&node_terms).cloned().collect();
        for term in &matched_terms {
            *lexical_document_frequency
                .entry(term.clone())
                .or_insert(0usize) += 1;
        }
        let lexical_overlap = matched_terms.len();
        if dense_score <= 0.0 && lexical_overlap == 0 {
            continue;
        }
        candidates.push((
            node_id,
            speaker,
            node.origin.session_id.clone(),
            dense_score,
            lexical_overlap,
            matched_terms,
            scope_weight,
        ));
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let eligible_count = candidates.len();
    let mut ranked: Vec<_> = candidates
        .into_iter()
        .map(
            |(
                node_id,
                speaker,
                session_id,
                dense_score,
                lexical_overlap,
                matched_terms,
                scope_weight,
            )|
             -> Result<SubjectRawCandidate, Error> {
                let lexical_idf_score = matched_terms
                    .iter()
                    .map(|term| {
                        let frequency = lexical_document_frequency
                            .get(term)
                            .copied()
                            .unwrap_or_default();
                        ((eligible_count as f64 + 1.0) / (frequency as f64 + 1.0)).ln() + 1.0
                    })
                    .sum();
                Ok(SubjectRawCandidate {
                    candidate: crate::query::ReadoutCandidate {
                        node_id,
                        score: 0.0,
                        activation: 0.0,
                        phi: dense_score,
                        embedding_cosine: dense_score,
                        salience: storage.get_salience(node_id)?,
                        impedance: 0.0,
                        scope_weight,
                        trust_weight: 1.0,
                        stress: 0.0,
                    },
                    speaker,
                    session_id,
                    lexical_overlap,
                    lexical_idf_score,
                })
            },
        )
        .collect::<Result<Vec<_>, Error>>()?;

    let mut dense_order: Vec<_> = (0..ranked.len()).collect();
    dense_order.sort_by(|left, right| {
        ranked[*right]
            .candidate
            .embedding_cosine
            .total_cmp(&ranked[*left].candidate.embedding_cosine)
            .then_with(|| {
                ranked[*left]
                    .candidate
                    .node_id
                    .cmp(&ranked[*right].candidate.node_id)
            })
    });
    let mut lexical_order: Vec<_> = (0..ranked.len())
        .filter(|index| ranked[*index].lexical_overlap > 0)
        .collect();
    lexical_order.sort_by(|left, right| {
        ranked[*right]
            .lexical_idf_score
            .total_cmp(&ranked[*left].lexical_idf_score)
            .then_with(|| {
                ranked[*right]
                    .lexical_overlap
                    .cmp(&ranked[*left].lexical_overlap)
            })
            .then_with(|| {
                ranked[*right]
                    .candidate
                    .embedding_cosine
                    .total_cmp(&ranked[*left].candidate.embedding_cosine)
            })
            .then_with(|| {
                ranked[*left]
                    .candidate
                    .node_id
                    .cmp(&ranked[*right].candidate.node_id)
            })
    });

    const RRF_K: f64 = 60.0;
    let mut fused_scores = vec![0.0; ranked.len()];
    for order in [&dense_order, &lexical_order] {
        for (position, &index) in order.iter().take(128).enumerate() {
            fused_scores[index] += 1.0 / (RRF_K + position as f64 + 1.0);
        }
    }
    for (candidate, score) in ranked.iter_mut().zip(fused_scores) {
        candidate.candidate.score = score;
    }
    ranked.sort_by(|left, right| {
        right
            .candidate
            .score
            .total_cmp(&left.candidate.score)
            .then_with(|| {
                right
                    .candidate
                    .embedding_cosine
                    .total_cmp(&left.candidate.embedding_cosine)
            })
            .then_with(|| left.candidate.node_id.cmp(&right.candidate.node_id))
    });

    let mut selected = Vec::with_capacity(SOURCE_LIMIT);
    let mut selected_ids = HashSet::new();
    let mut represented_speakers = HashSet::new();
    for candidate in &ranked {
        if selected.len() >= SOURCE_LIMIT {
            break;
        }
        if represented_speakers.insert(candidate.speaker.clone())
            && selected_ids.insert(candidate.candidate.node_id)
        {
            selected.push(candidate);
        }
    }
    let mut represented_sessions: HashSet<_> = selected
        .iter()
        .map(|candidate| candidate.session_id.clone())
        .collect();
    for candidate in &ranked {
        if selected.len() >= SESSION_DIVERSE_LIMIT {
            break;
        }
        if represented_sessions.insert(candidate.session_id.clone())
            && selected_ids.insert(candidate.candidate.node_id)
        {
            selected.push(candidate);
        }
    }
    for candidate in &ranked {
        if selected.len() >= SOURCE_LIMIT {
            break;
        }
        if selected_ids.insert(candidate.candidate.node_id) {
            selected.push(candidate);
        }
    }

    let max_score = selected
        .iter()
        .map(|candidate| candidate.candidate.score)
        .max_by(f64::total_cmp)
        .unwrap_or(1.0)
        .max(f64::EPSILON);
    Ok(selected
        .into_iter()
        .map(|candidate| {
            let mut routed = candidate.candidate.clone();
            routed.activation = (routed.score / max_score).clamp(f64::EPSILON, 1.0);
            routed.impedance = (-routed.activation.ln()).max(0.0);
            routed
        })
        .collect())
}

fn uses_complex_expansion(plan: &RecallPlan) -> bool {
    matches!(
        plan.recall_intent,
        RecallIntent::Enumeration | RecallIntent::Relational
    ) && matches!(
        plan.answer_shape,
        AnswerShape::Collection | AnswerShape::Relationship | AnswerShape::Inference
    )
}

pub(super) fn uses_dense_query_expansion(plan: &RecallPlan) -> bool {
    plan.recall_intent != RecallIntent::Temporal && uses_complex_expansion(plan)
}

fn uses_atomic_fact_expansion(plan: &RecallPlan) -> bool {
    if plan.recall_intent == RecallIntent::Temporal && plan.answer_shape != AnswerShape::Frequency {
        return false;
    }
    uses_complex_expansion(plan)
        || matches!(
            plan.answer_shape,
            AnswerShape::Count | AnswerShape::Frequency
        )
}

fn atomic_scope_is_visible(query_scope: &ScopePath, record_scope: &ScopePath) -> bool {
    query_scope.is_universal() || record_scope.is_universal() || query_scope == record_scope
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

const ATOMIC_CHAIN_TRACE_PREFIX: &str = "atomic_relation_paths:v1:";

pub(super) fn encode_atomic_relation_paths(paths: &[AtomicRelationPath]) -> Option<String> {
    let encoded = paths
        .iter()
        .take(ATOMIC_CHAIN_MAX_PATHS)
        .filter(|path| atomic_relation_path_is_well_formed(path))
        .map(|path| {
            let facts = path
                .fact_ids
                .iter()
                .map(|fact_id| fact_id.0.to_string())
                .collect::<Vec<_>>()
                .join(".");
            let hops = path
                .hops
                .iter()
                .filter_map(|hop| {
                    atomic_relation_kind_code(hop.kind).map(|kind| {
                        format!(
                            "{}.{}.{}.{}",
                            hop.relation_id.0, hop.from_fact_id.0, hop.to_fact_id.0, kind
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join(",");
            let sources = path
                .source_groups
                .iter()
                .map(|group| {
                    group
                        .iter()
                        .map(|source_id| source_id.0.to_string())
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{facts}/{hops}/{sources}")
        })
        .collect::<Vec<_>>();
    (!encoded.is_empty()).then(|| format!("{ATOMIC_CHAIN_TRACE_PREFIX}{}", encoded.join(";")))
}

pub(super) fn parse_atomic_relation_paths(strategies: &[String]) -> Vec<AtomicRelationPath> {
    let mut paths = Vec::new();
    for strategy in strategies {
        let Some(encoded_paths) = strategy.strip_prefix(ATOMIC_CHAIN_TRACE_PREFIX) else {
            continue;
        };
        if encoded_paths.len() > ATOMIC_CHAIN_MAX_TRACE_BYTES {
            continue;
        }
        for encoded_path in encoded_paths.split(';') {
            if paths.len() >= ATOMIC_CHAIN_MAX_PATHS {
                return paths;
            }
            let Some(path) = parse_atomic_relation_path(encoded_path) else {
                continue;
            };
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

pub(super) fn validated_atomic_relation_paths<S: StorageAdapter>(
    storage: &S,
    strategies: &[String],
    as_of: crate::graph::Timestamp,
    query_scope: &ScopePath,
) -> Result<Vec<AtomicRelationPath>, Error> {
    let live_fact_ids: HashSet<_> = storage.all_atomic_fact_ids().into_iter().collect();
    let live_relation_ids: HashSet<_> =
        storage.all_atomic_fact_relation_ids().into_iter().collect();
    let mut validated = Vec::new();
    for path in parse_atomic_relation_paths(strategies) {
        if path
            .fact_ids
            .iter()
            .any(|fact_id| !live_fact_ids.contains(fact_id))
            || path
                .hops
                .iter()
                .any(|hop| !live_relation_ids.contains(&hop.relation_id))
        {
            continue;
        }
        let mut concrete_scope = (!query_scope.is_universal()).then(|| query_scope.clone());
        let mut eligible = true;
        for (index, fact_id) in path.fact_ids.iter().copied().enumerate() {
            if index > 0 {
                let hop = path.hops[index - 1];
                let relation = storage.get_atomic_fact_relation(hop.relation_id)?;
                let previous_fact_id = path.fact_ids[index - 1];
                let joins_path = (hop.from_fact_id == previous_fact_id
                    && hop.to_fact_id == fact_id)
                    || (hop.to_fact_id == previous_fact_id && hop.from_fact_id == fact_id);
                if relation.from_fact_id != hop.from_fact_id
                    || relation.to_fact_id != hop.to_fact_id
                    || relation.kind != hop.kind
                    || !joins_path
                    || relation
                        .metadata
                        .get("retracted")
                        .is_some_and(|value| value == "true")
                    || relation.reviewed_at > as_of
                    || !crate::graph::valid_at(relation.valid_from, relation.valid_until, as_of)
                    || !matches!(
                        relation.kind,
                        AtomicFactRelationKind::Reason
                            | AtomicFactRelationKind::Causal
                            | AtomicFactRelationKind::Supports
                    )
                {
                    eligible = false;
                    break;
                }
                let Some(next_scope) = extend_chain_scope(concrete_scope, &relation.scope) else {
                    eligible = false;
                    break;
                };
                concrete_scope = next_scope;
            }

            let fact = storage.get_atomic_fact(fact_id)?;
            let Some((next_scope, live_sources)) =
                eligible_chain_fact_sources(storage, fact, as_of, concrete_scope)?
            else {
                eligible = false;
                break;
            };
            if bounded_chain_source_group(fact, &live_sources) != path.source_groups[index] {
                eligible = false;
                break;
            }
            concrete_scope = next_scope;
        }
        if eligible {
            validated.push(path);
        }
    }
    Ok(validated)
}

fn atomic_relation_kind_code(kind: AtomicFactRelationKind) -> Option<char> {
    match kind {
        AtomicFactRelationKind::Reason => Some('r'),
        AtomicFactRelationKind::Causal => Some('c'),
        AtomicFactRelationKind::Supports => Some('s'),
        AtomicFactRelationKind::Contradicts => None,
    }
}

fn parse_atomic_relation_kind(value: &str) -> Option<AtomicFactRelationKind> {
    match value {
        "r" => Some(AtomicFactRelationKind::Reason),
        "c" => Some(AtomicFactRelationKind::Causal),
        "s" => Some(AtomicFactRelationKind::Supports),
        _ => None,
    }
}

fn parse_atomic_relation_path(encoded: &str) -> Option<AtomicRelationPath> {
    let mut sections = encoded.split('/');
    let facts = sections.next()?;
    let hops = sections.next()?;
    let sources = sections.next()?;
    if sections.next().is_some() {
        return None;
    }
    let fact_ids = facts
        .split('.')
        .map(|value| value.parse::<u64>().ok().map(AtomicFactId))
        .collect::<Option<Vec<_>>>()?;
    let hops = hops
        .split(',')
        .map(|encoded_hop| {
            let mut fields = encoded_hop.split('.');
            let relation_id = AtomicFactRelationId(fields.next()?.parse::<u64>().ok()?);
            let from_fact_id = AtomicFactId(fields.next()?.parse::<u64>().ok()?);
            let to_fact_id = AtomicFactId(fields.next()?.parse::<u64>().ok()?);
            let kind = parse_atomic_relation_kind(fields.next()?)?;
            (fields.next().is_none()).then_some(AtomicRelationHop {
                relation_id,
                from_fact_id,
                to_fact_id,
                kind,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let source_groups = sources
        .split(',')
        .map(|encoded_group| {
            let group = encoded_group
                .split('.')
                .map(|value| value.parse::<u64>().ok().map(NodeId))
                .collect::<Option<Vec<_>>>()?;
            (!group.is_empty()).then_some(group)
        })
        .collect::<Option<Vec<_>>>()?;
    let path = AtomicRelationPath {
        fact_ids,
        hops,
        source_groups,
    };
    atomic_relation_path_is_well_formed(&path).then_some(path)
}

fn atomic_relation_path_is_well_formed(path: &AtomicRelationPath) -> bool {
    if !(2..=ATOMIC_CHAIN_MAX_FACTS_PER_PATH).contains(&path.fact_ids.len())
        || path.hops.len() + 1 != path.fact_ids.len()
        || path.source_groups.len() != path.fact_ids.len()
        || path
            .source_groups
            .iter()
            .any(|group| group.is_empty() || group.len() > ATOMIC_CHAIN_MAX_SOURCES_PER_FACT)
    {
        return false;
    }
    let unique_facts: HashSet<_> = path.fact_ids.iter().copied().collect();
    if unique_facts.len() != path.fact_ids.len() {
        return false;
    }
    let unique_relations: HashSet<_> = path.hops.iter().map(|hop| hop.relation_id).collect();
    if unique_relations.len() != path.hops.len() {
        return false;
    }
    path.hops.iter().enumerate().all(|(index, hop)| {
        let left = path.fact_ids[index];
        let right = path.fact_ids[index + 1];
        let joins_path = (hop.from_fact_id == left && hop.to_fact_id == right)
            || (hop.to_fact_id == left && hop.from_fact_id == right);
        let unique_sources: HashSet<_> = path.source_groups[index].iter().copied().collect();
        joins_path
            && unique_sources.len() == path.source_groups[index].len()
            && atomic_relation_kind_code(hop.kind).is_some()
    }) && path.source_groups.last().is_some_and(|group| {
        let unique_sources: HashSet<_> = group.iter().copied().collect();
        unique_sources.len() == group.len()
    })
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

const TEMPORAL_FACT_RESERVE_SOURCE_LIMIT: usize = 2;

fn atomic_fact_time_overlaps(
    fact: &AtomicFact,
    query_ranges: &[crate::query::temporal::TimeRange],
) -> bool {
    match (fact.valid_from, fact.valid_until) {
        // Validity intervals are half-open while parsed query ranges are
        // inclusive. Keep that distinction at the boundary instead of
        // widening either representation.
        (Some(start), Some(end)) if start < end => query_ranges
            .iter()
            .any(|range| start.0 <= range.end && range.start < end.0),
        (Some(start), None) => query_ranges.iter().any(|range| start.0 <= range.end),
        (None, Some(end)) => query_ranges.iter().any(|range| range.start < end.0),
        (Some(_), Some(_)) => false,
        (None, None) => {
            fact.observed_at.0 != 0
                && query_ranges.iter().any(|range| {
                    fact.observed_at.0 >= range.start && fact.observed_at.0 <= range.end
                })
        }
    }
}

/// Route a small source-grounded reserve for factual questions constrained to
/// a resolvable date or range.
///
/// This is deliberately separate from broad atomic expansion. A fact must
/// name an exact query subject, overlap the requested interval, and retain a
/// byte-exact binding to one current raw source. The reserve never returns
/// sidecar text and cannot widen other temporal or direct query shapes.
fn route_temporal_fact_reserve<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    query_embeddings: &[&[f64]],
    now: crate::graph::Timestamp,
    scope: &ScopePath,
) -> Result<Vec<RoutedAtomicSource>, Error> {
    let query_ranges = crate::query::temporal::parse_time_cues(&plan.query, now.0);
    if query_ranges.is_empty() {
        return Ok(Vec::new());
    }

    let query_terms = facet_terms(&plan.query);
    let mut routed_position_by_source: HashMap<NodeId, usize> = HashMap::new();
    let mut routed: Vec<RoutedAtomicSource> = Vec::new();
    for fact_id in storage.all_atomic_fact_ids() {
        let fact = storage.get_atomic_fact(fact_id)?;
        let Some(metadata) = atomic_routing_metadata(fact) else {
            continue;
        };
        if atomic_subject_matches(&plan.query, &fact.metadata) == 0
            || !atomic_fact_time_overlaps(fact, &query_ranges)
            || fact
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
            || fact.observed_at > now
            || !atomic_scope_is_visible(scope, &fact.scope)
        {
            continue;
        }

        // Only the source that owns the reviewed byte span may enter this
        // lane. Other provenance rows on the same fact remain available to
        // ordinary retrieval but cannot consume this narrow reserve.
        let source = match storage.get_node(metadata.evidence_source) {
            Ok(source) => source,
            Err(Error::NodeNotFound(_)) => continue,
            Err(error) => return Err(error),
        };
        if source.node_type != KnowledgeType::Episodic
            || source.origin.session_id != fact.source_session_id
            || source.origin.scope != fact.scope
            || !storage.atomic_fact_source_is_current(fact, source)?
            || source.created_at > now
            || source
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
            || !crate::graph::valid_at(source.valid_from, source.valid_until, now)
            || !atomic_scope_is_visible(scope, &source.origin.scope)
        {
            continue;
        }
        let Some(evidence_span) = source
            .content
            .get(metadata.evidence_start..metadata.evidence_end)
        else {
            continue;
        };
        let object_is_grounded = if metadata.requires_exact_object {
            evidence_span.contains(metadata.evidence_object)
        } else {
            normalized_phrase(evidence_span).contains(&normalized_phrase(metadata.evidence_object))
        };
        if !object_is_grounded {
            continue;
        }

        let mut fact_terms = facet_terms(&fact.content);
        for value in [
            metadata.subject,
            metadata.relation,
            metadata.object,
            metadata.evidence_object,
        ] {
            fact_terms.extend(facet_terms(value));
        }
        let lexical_overlap = query_terms.intersection(&fact_terms).count();
        let fact_dense = max_query_cosine(query_embeddings, &fact.embedding).max(0.0);
        let embedding_cosine = source.embedding.as_ref().map_or(0.0, |embedding| {
            max_query_cosine(query_embeddings, embedding)
        });
        let score = 1.0 + lexical_overlap as f64 + fact_dense + embedding_cosine.max(0.0) * 0.5;

        if let Some(position) = routed_position_by_source
            .get(&metadata.evidence_source)
            .copied()
        {
            let existing = &mut routed[position];
            if !existing.fact_ids.contains(&fact_id) {
                existing.fact_ids.push(fact_id);
            }
            if score.total_cmp(&existing.candidate.score).is_gt() {
                existing.candidate.score = score;
                existing.candidate.phi = embedding_cosine;
                existing.candidate.embedding_cosine = embedding_cosine;
            }
            continue;
        }

        routed.push(RoutedAtomicSource {
            candidate: crate::query::ReadoutCandidate {
                node_id: metadata.evidence_source,
                score,
                activation: 1.0,
                phi: embedding_cosine,
                embedding_cosine,
                salience: storage.get_salience(metadata.evidence_source)?,
                impedance: 0.0,
                scope_weight: crate::query::scoring::scope_weight(scope, &source.origin.scope),
                trust_weight: 1.0,
                stress: 0.0,
            },
            kind_priority: 0,
            fact_ids: vec![fact_id],
            origin: AtomicRouteOrigin::Direct,
        });
        routed_position_by_source.insert(metadata.evidence_source, routed.len() - 1);
    }

    routed.sort_by(|left, right| {
        right
            .candidate
            .score
            .total_cmp(&left.candidate.score)
            .then_with(|| left.candidate.node_id.cmp(&right.candidate.node_id))
    });
    routed.truncate(TEMPORAL_FACT_RESERVE_SOURCE_LIMIT);
    let max_score = routed
        .first()
        .map(|source| source.candidate.score)
        .unwrap_or(1.0)
        .max(f64::EPSILON);
    for source in &mut routed {
        source.fact_ids.sort_unstable();
        source.candidate.activation = (source.candidate.score / max_score).clamp(f64::EPSILON, 1.0);
        source.candidate.impedance = (-source.candidate.activation.ln()).max(0.0);
    }
    Ok(routed)
}

pub(super) fn route_atomic_fact_sources<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    query_embeddings: &[&[f64]],
    now: crate::graph::Timestamp,
    scope: &ScopePath,
) -> Result<Vec<RoutedAtomicSource>, Error> {
    if plan.recall_intent == RecallIntent::Temporal && plan.answer_shape == AnswerShape::Fact {
        return route_temporal_fact_reserve(storage, plan, query_embeddings, now, scope);
    }
    if !uses_atomic_fact_expansion(plan) {
        return Ok(Vec::new());
    }
    let fact_limit = match plan.answer_shape {
        AnswerShape::Collection => 32,
        // Relationship and manner questions can connect an entity to actions
        // whose wording never repeats the query predicate. Ranking already
        // scans the complete sidecar, while raw-source admission stays capped
        // at twenty below, so a wider fact shortlist improves semantic breadth
        // without widening the production reranker surface.
        AnswerShape::Relationship => 32,
        AnswerShape::Inference => 32,
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
            || fact.observed_at > now
            || !crate::graph::valid_at(fact.valid_from, fact.valid_until, now)
            || !atomic_scope_is_visible(scope, &fact.scope)
        {
            continue;
        }
        eligible_fact_count += 1;
        let dense_score = max_query_cosine(query_embeddings, &fact.embedding);
        let mut fact_terms = facet_terms(&fact.content);
        fact_terms.extend(atomic_routing_metadata_terms(
            storage,
            fact,
            &query_terms,
            now,
            scope,
        )?);
        let matched_terms: HashSet<_> = query_terms.intersection(&fact_terms).cloned().collect();
        for term in &matched_terms {
            *lexical_document_frequency
                .entry(term.clone())
                .or_insert(0usize) += 1;
        }
        let lexical_overlap = matched_terms.len();
        let entity_matches = atomic_entity_matches(&plan.query, &fact.entity_tags, &fact.metadata);
        let subject_matches = atomic_subject_matches(&plan.query, &fact.metadata);
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
                subject_matches,
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

    const LANE_DEPTH: usize = 128;
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
            .subject_matches
            .cmp(&left.subject_matches)
            .then_with(|| right.entity_matches.cmp(&left.entity_matches))
            .then_with(|| right.dense_score.total_cmp(&left.dense_score))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    // Entity tags can include an interlocutor, place, work, or organization.
    // Keep a separate lane for facts whose canonical subject itself appears in
    // the query. This is deliberately still predicate-ranked: it prevents a
    // prolific person's unrelated memories from consuming the complete lane,
    // while ensuring that relevant facts are not crowded out by secondary tag
    // matches from thousands of sidecar rows.
    let mut subject_owned: Vec<_> = facts
        .iter()
        .filter(|fact| fact.subject_matches > 0)
        .collect();
    subject_owned.sort_by(|left, right| {
        right
            .dense_score
            .total_cmp(&left.dense_score)
            .then_with(|| right.lexical_idf_score.total_cmp(&left.lexical_idf_score))
            .then_with(|| right.kind_priority.cmp(&left.kind_priority))
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
            .then_with(|| right.subject_matches.cmp(&left.subject_matches))
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
        subject_owned
            .iter()
            .take(LANE_DEPTH)
            .map(|fact| fact.fact_id),
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
    let subject_fact_quota = match plan.answer_shape {
        AnswerShape::Collection | AnswerShape::Relationship => 12,
        AnswerShape::Inference => 10,
        _ => 0,
    };
    let subject_fact_rank: HashMap<_, _> = subject_owned
        .iter()
        .take(subject_fact_quota)
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
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| {
            match (
                subject_fact_rank.get(left_id),
                subject_fact_rank.get(right_id),
            ) {
                (Some(left_rank), Some(right_rank)) => left_rank.cmp(right_rank),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        })
        .then_with(|| right_score.total_cmp(left_score))
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
    // cross-session queries bridge events and evidence-backed inference can
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
            // Source deletion can race or outlive a reviewed sidecar record.
            // Treat that fact as stale provenance instead of failing the whole
            // recall; no sidecar text is ever returned on its own. Endpoint
            // identity is revalidated as well because graph node IDs may be
            // reused after deletion.
            let source = match storage.get_node(source_id) {
                Ok(source) => source,
                Err(Error::NodeNotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            if source.node_type != KnowledgeType::Episodic
                || source.origin.session_id != fact.source_session_id
                || source.origin.scope != fact.scope
                || !storage.atomic_fact_source_is_current(fact, source)?
                || source.created_at > now
                || source
                    .metadata
                    .get("retracted")
                    .is_some_and(|value| value == "true")
                || !crate::graph::valid_at(source.valid_from, source.valid_until, now)
                || !atomic_scope_is_visible(scope, &source.origin.scope)
            {
                continue;
            }
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
            let embedding_cosine = source.embedding.as_ref().map_or(0.0, |embedding| {
                max_query_cosine(query_embeddings, embedding)
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
                origin: AtomicRouteOrigin::Direct,
            });
            routed_position_by_source.insert(source_id, routed.len() - 1);
        }
    }
    Ok(routed)
}

#[derive(Debug, Clone)]
struct AtomicChainStep {
    fact_id: AtomicFactId,
    depth: usize,
    base_score: f64,
    base_activation: f64,
    kind_priority: usize,
    concrete_scope: Option<ScopePath>,
    path: AtomicRelationPath,
}

/// Follow reviewed typed relations from the highest-ranked direct atomic facts.
///
/// The traversal is deliberately small and deterministic. Relation and fact
/// text never enters the returned evidence: every routed candidate is a live
/// raw Episodic source cited by an eligible endpoint fact. `Contradicts` is a
/// stored constraint and is never a positive traversal bridge.
pub(super) fn expand_atomic_fact_relation_sources<S: StorageAdapter>(
    storage: &S,
    plan: &RecallPlan,
    direct_sources: &[RoutedAtomicSource],
    query_embeddings: &[&[f64]],
    now: crate::graph::Timestamp,
    query_scope: &ScopePath,
) -> Result<AtomicChainExpansion, Error> {
    const SEED_LIMIT: usize = 8;
    const MAX_DEPTH: usize = 2;
    const RELATION_VISIT_LIMIT: usize = 32;
    const RELATION_SCAN_LIMIT: usize = 128;
    const EXPANDED_FACT_LIMIT: usize = 8;
    const ROUTED_SOURCE_LIMIT: usize = 8;

    if plan.recall_intent == RecallIntent::Temporal
        || !matches!(
            plan.answer_shape,
            AnswerShape::Relationship | AnswerShape::Inference
        )
    {
        return Ok(AtomicChainExpansion::default());
    }

    let initial_scope = (!query_scope.is_universal()).then(|| query_scope.clone());
    let mut queue = VecDeque::new();
    let mut seeded_facts = HashSet::new();
    let mut direct_source_ids = HashSet::new();
    for source in direct_sources {
        direct_source_ids.insert(source.candidate.node_id);
        if !matches!(source.origin, AtomicRouteOrigin::Direct) {
            continue;
        }
        for fact_id in &source.fact_ids {
            if seeded_facts.len() >= SEED_LIMIT {
                break;
            }
            if !seeded_facts.insert(*fact_id) {
                continue;
            }
            let fact = storage.get_atomic_fact(*fact_id)?;
            let Some((concrete_scope, live_sources)) =
                eligible_chain_fact_sources(storage, fact, now, initial_scope.clone())?
            else {
                continue;
            };
            direct_source_ids.extend(live_sources.iter().copied());
            queue.push_back(AtomicChainStep {
                fact_id: *fact_id,
                depth: 0,
                base_score: source.candidate.score,
                base_activation: source.candidate.activation,
                kind_priority: source.kind_priority,
                concrete_scope,
                path: AtomicRelationPath {
                    fact_ids: vec![*fact_id],
                    hops: Vec::new(),
                    source_groups: vec![bounded_chain_source_group(fact, &live_sources)],
                },
            });
        }
        if seeded_facts.len() >= SEED_LIMIT {
            break;
        }
    }
    if queue.is_empty() {
        return Ok(AtomicChainExpansion::default());
    }

    let mut diagnostics = AtomicChainDiagnostics::default();
    let mut visited_relations = HashSet::new();
    let mut scanned_relations = 0_usize;
    let mut visited_facts = seeded_facts;
    let mut routed_position_by_source: HashMap<NodeId, usize> = HashMap::new();
    let mut routed: Vec<RoutedAtomicSource> = Vec::new();
    let mut paths = Vec::new();
    let mut recorded_fact_pairs = HashSet::new();

    'traversal: while let Some(step) = queue.pop_front() {
        if step.depth >= MAX_DEPTH {
            continue;
        }
        if scanned_relations >= RELATION_SCAN_LIMIT {
            diagnostics.truncated = true;
            break;
        }
        // Adjacency slices are ID-ordered. Inspect only a bounded recent tail
        // from each direction, then merge it in descending order. This keeps a
        // high-degree fact from allocating or sorting its entire history and
        // gives current reviewed links precedence over stale low-ID records.
        let mut incident_relations = storage
            .atomic_fact_relations_from(step.fact_id)
            .iter()
            .rev()
            .take(RELATION_SCAN_LIMIT)
            .chain(
                storage
                    .atomic_fact_relations_to(step.fact_id)
                    .iter()
                    .rev()
                    .take(RELATION_SCAN_LIMIT),
            )
            .copied()
            .collect::<Vec<_>>();
        incident_relations.sort_by(|left, right| right.cmp(left));
        incident_relations.dedup();
        incident_relations.retain(|relation_id| !visited_relations.contains(relation_id));
        incident_relations.truncate(RELATION_SCAN_LIMIT.saturating_sub(scanned_relations));
        for relation_id in incident_relations {
            if !visited_relations.insert(relation_id) {
                continue;
            }
            if scanned_relations >= RELATION_SCAN_LIMIT {
                diagnostics.truncated = true;
                break 'traversal;
            }
            scanned_relations += 1;
            let relation = storage.get_atomic_fact_relation(relation_id)?;
            if relation
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
                || relation.reviewed_at > now
                || !crate::graph::valid_at(relation.valid_from, relation.valid_until, now)
            {
                continue;
            }
            if matches!(relation.kind, AtomicFactRelationKind::Contradicts) {
                diagnostics.contradictions_excluded += 1;
                continue;
            }
            if !matches!(
                relation.kind,
                AtomicFactRelationKind::Reason
                    | AtomicFactRelationKind::Causal
                    | AtomicFactRelationKind::Supports
            ) {
                continue;
            }
            let Some(relation_scope) =
                extend_chain_scope(step.concrete_scope.clone(), &relation.scope)
            else {
                continue;
            };
            let endpoint = if relation.from_fact_id == step.fact_id {
                relation.to_fact_id
            } else if relation.to_fact_id == step.fact_id {
                relation.from_fact_id
            } else {
                return Err(Error::StorageError(format!(
                    "atomic fact relation {} is absent from its adjacency endpoint {}",
                    relation.id.0, step.fact_id.0
                )));
            };
            if step.path.fact_ids.contains(&endpoint) {
                continue;
            }
            let fact_pair = if step.fact_id < endpoint {
                (step.fact_id, endpoint)
            } else {
                (endpoint, step.fact_id)
            };
            if recorded_fact_pairs.contains(&fact_pair) {
                continue;
            }
            let endpoint_was_visited = visited_facts.contains(&endpoint);
            if !endpoint_was_visited && diagnostics.expanded_facts >= EXPANDED_FACT_LIMIT {
                diagnostics.truncated = true;
                break 'traversal;
            }
            let endpoint_fact = storage.get_atomic_fact(endpoint)?;
            let Some((endpoint_scope, endpoint_sources)) =
                eligible_chain_fact_sources(storage, endpoint_fact, now, relation_scope)?
            else {
                continue;
            };
            if diagnostics.visited_relations >= RELATION_VISIT_LIMIT {
                diagnostics.truncated = true;
                break 'traversal;
            }
            if paths.len() >= ATOMIC_CHAIN_MAX_PATHS {
                diagnostics.truncated = true;
                break 'traversal;
            }
            diagnostics.visited_relations += 1;

            let mut path = step.path.clone();
            path.fact_ids.push(endpoint);
            path.hops.push(AtomicRelationHop {
                relation_id,
                from_fact_id: relation.from_fact_id,
                to_fact_id: relation.to_fact_id,
                kind: relation.kind,
            });
            path.source_groups
                .push(bounded_chain_source_group(endpoint_fact, &endpoint_sources));
            paths.push(path.clone());
            recorded_fact_pairs.insert(fact_pair);
            if endpoint_was_visited {
                continue;
            }

            visited_facts.insert(endpoint);
            diagnostics.expanded_facts += 1;
            let depth = step.depth + 1;
            let depth_scale = 0.5_f64.powi(depth as i32);
            let score = step.base_score * depth_scale;
            let activation = (step.base_activation * depth_scale).clamp(f64::EPSILON, 1.0);
            let endpoint_kind_priority = step
                .kind_priority
                .max(inference_fact_kind_priority(plan, &endpoint_fact.metadata));

            for source_id in &endpoint_sources {
                if direct_source_ids.contains(source_id) {
                    continue;
                }
                if let Some(position) = routed_position_by_source.get(source_id).copied() {
                    let existing = &mut routed[position];
                    if !existing.fact_ids.contains(&endpoint) {
                        existing.fact_ids.push(endpoint);
                    }
                    existing.kind_priority = existing.kind_priority.max(endpoint_kind_priority);
                    continue;
                }
                if routed.len() >= ROUTED_SOURCE_LIMIT {
                    diagnostics.truncated = true;
                    continue;
                }
                let source = storage.get_node(*source_id)?;
                let embedding_cosine = source.embedding.as_ref().map_or(0.0, |embedding| {
                    max_query_cosine(query_embeddings, embedding)
                });
                routed.push(RoutedAtomicSource {
                    candidate: crate::query::ReadoutCandidate {
                        node_id: *source_id,
                        score,
                        activation,
                        phi: embedding_cosine,
                        embedding_cosine,
                        salience: storage.get_salience(*source_id)?,
                        impedance: (-activation.ln()).max(0.0),
                        scope_weight: crate::query::scoring::scope_weight(
                            query_scope,
                            &source.origin.scope,
                        ),
                        trust_weight: 1.0,
                        stress: 0.0,
                    },
                    kind_priority: endpoint_kind_priority,
                    fact_ids: vec![endpoint],
                    origin: AtomicRouteOrigin::Chain { depth },
                });
                routed_position_by_source.insert(*source_id, routed.len() - 1);
            }
            queue.push_back(AtomicChainStep {
                fact_id: endpoint,
                depth,
                base_score: step.base_score,
                base_activation: step.base_activation,
                kind_priority: endpoint_kind_priority,
                concrete_scope: endpoint_scope,
                path,
            });
        }
    }
    diagnostics.routed_sources = routed.len();
    Ok(AtomicChainExpansion {
        sources: routed,
        paths,
        diagnostics,
    })
}

fn extend_chain_scope(
    concrete_scope: Option<ScopePath>,
    record_scope: &ScopePath,
) -> Option<Option<ScopePath>> {
    if record_scope.is_universal() {
        return Some(concrete_scope);
    }
    match concrete_scope {
        Some(scope) if scope == *record_scope => Some(Some(scope)),
        Some(_) => None,
        None => Some(Some(record_scope.clone())),
    }
}

type EligibleChainFactSources = Option<(Option<ScopePath>, Vec<NodeId>)>;

fn bounded_chain_source_group(fact: &AtomicFact, live_sources: &[NodeId]) -> Vec<NodeId> {
    let evidence_source = fact
        .metadata
        .get("anamnesis:evidence-source-node-id")
        .and_then(|value| value.parse::<u64>().ok())
        .map(NodeId);
    let mut sources = live_sources.to_vec();
    sources.sort_unstable();
    sources.dedup();
    sources.sort_by_key(|source_id| usize::from(Some(*source_id) != evidence_source));
    sources.truncate(ATOMIC_CHAIN_MAX_SOURCES_PER_FACT);
    sources
}

fn eligible_chain_fact_sources<S: StorageAdapter>(
    storage: &S,
    fact: &AtomicFact,
    now: crate::graph::Timestamp,
    concrete_scope: Option<ScopePath>,
) -> Result<EligibleChainFactSources, Error> {
    if fact
        .metadata
        .get("retracted")
        .is_some_and(|value| value == "true")
        || fact.observed_at > now
        || !crate::graph::valid_at(fact.valid_from, fact.valid_until, now)
    {
        return Ok(None);
    }
    let Some(concrete_scope) = extend_chain_scope(concrete_scope, &fact.scope) else {
        return Ok(None);
    };
    let mut sources = Vec::new();
    for source_id in &fact.source_node_ids {
        let source = match storage.get_node(*source_id) {
            Ok(source) => source,
            Err(Error::NodeNotFound(_)) => continue,
            Err(error) => return Err(error),
        };
        if source.node_type != KnowledgeType::Episodic
            || source.origin.session_id != fact.source_session_id
            || source.origin.scope != fact.scope
            || !storage.atomic_fact_source_is_current(fact, source)?
            || source.created_at > now
            || source
                .metadata
                .get("retracted")
                .is_some_and(|value| value == "true")
            || !crate::graph::valid_at(source.valid_from, source.valid_until, now)
            || extend_chain_scope(concrete_scope.clone(), &source.origin.scope).is_none()
        {
            continue;
        }
        sources.push(*source_id);
    }
    if sources.is_empty() {
        Ok(None)
    } else {
        Ok(Some((concrete_scope, sources)))
    }
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

    // A routed fact often identifies the exact raw answer source, or the right
    // conversation while citing a premise adjacent to the answer. Admit at
    // most two exact sources/session-window neighbors from the deeper trace.
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

    // Begin with the authoritative prefix and admit a deeper candidate only when it
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
    routed_atomic_markers: &[AtomicSourceMarker],
) -> Result<Vec<EvidenceDocument>, Error> {
    let mut routed_atomic_sources: Vec<(NodeId, usize)> = Vec::new();
    for marker in routed_atomic_markers {
        if let Some((_, priority)) = routed_atomic_sources
            .iter_mut()
            .find(|(node_id, _)| *node_id == marker.source_node_id)
        {
            *priority = (*priority).max(marker.kind_priority);
        } else {
            routed_atomic_sources.push((marker.source_node_id, marker.kind_priority));
        }
    }
    let ranking =
        coverage_preselected_ranking(storage, plan, ranking, limit, &routed_atomic_sources)?;
    let mut documents = if plan.answer_shape == AnswerShape::Inference {
        compile_inference_documents(storage, &ranking, limit)?
    } else if plan.answer_shape == AnswerShape::Frequency
        || matches!(
            plan.recall_intent,
            RecallIntent::Enumeration | RecallIntent::Relational
        )
    {
        compile_evidence_documents(storage, &ranking, limit)?
    } else {
        ranking
            .iter()
            .take(limit)
            .map(|candidate| {
                let node = storage.get_node(candidate.node_id)?;
                Ok(EvidenceDocument::from_raw(
                    candidate.node_id,
                    canonical_sources(storage, candidate.node_id)?,
                    node.content.clone(),
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?
    };
    if matches!(
        plan.answer_shape,
        AnswerShape::Collection | AnswerShape::Relationship | AnswerShape::Inference
    ) {
        apply_atomic_rerank_cues(storage, &mut documents, routed_atomic_markers)?;
    }
    apply_bounded_same_session_reply_context(storage, plan, &mut documents)?;
    Ok(documents)
}

fn apply_atomic_rerank_cues<S: StorageAdapter>(
    storage: &S,
    documents: &mut [EvidenceDocument],
    routed_atomic_markers: &[AtomicSourceMarker],
) -> Result<(), Error> {
    const MAX_CUES_PER_DOCUMENT: usize = 2;

    for document in documents {
        let mut seen_facts = HashSet::new();
        let mut seen_cues = HashSet::new();
        let mut cues = Vec::new();
        for marker in routed_atomic_markers {
            let Some(fact_id) = marker.fact_id else {
                continue;
            };
            if !seen_facts.insert(fact_id) {
                continue;
            }
            let fact = storage.get_atomic_fact(fact_id)?;
            let Some((evidence_source, evidence_span)) =
                grounded_atomic_fact(storage, fact, marker.source_node_id)?
            else {
                continue;
            };
            if !document.source_node_ids.contains(&evidence_source) {
                continue;
            }
            let content = fact.content.trim();
            let cue = format!("Fact: {content}\nExact source span: {evidence_span}");
            if seen_cues.insert(cue.clone()) {
                cues.push(cue);
            }
            if cues.len() >= MAX_CUES_PER_DOCUMENT {
                break;
            }
        }
        document.rerank_text = if cues.is_empty() {
            document.text.clone()
        } else {
            format!(
                "Grounded retrieval cues:\n{}\nRaw source evidence:\n{}",
                cues.join("\n"),
                document.text
            )
        };
    }
    Ok(())
}

fn grounded_atomic_fact<S: StorageAdapter>(
    storage: &S,
    fact: &AtomicFact,
    routed_source: NodeId,
) -> Result<Option<(NodeId, String)>, Error> {
    if !fact.source_node_ids.contains(&routed_source) || fact.content.trim().is_empty() {
        return Ok(None);
    }
    let Some(evidence_source) = fact
        .metadata
        .get("anamnesis:evidence-source-node-id")
        .and_then(|value| value.parse::<u64>().ok())
        .map(NodeId)
    else {
        return Ok(None);
    };
    let Some(start) = fact
        .metadata
        .get("anamnesis:evidence-span-start")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Ok(None);
    };
    let Some(end) = fact
        .metadata
        .get("anamnesis:evidence-span-end")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Ok(None);
    };
    let requires_exact_object = fact.metadata.contains_key("anamnesis:evidence-object");
    let Some(object) = fact
        .metadata
        .get("anamnesis:evidence-object")
        .or_else(|| fact.metadata.get("anamnesis:ground-object"))
    else {
        return Ok(None);
    };
    if !fact.source_node_ids.contains(&evidence_source) {
        return Ok(None);
    }
    let source = storage.get_node(evidence_source)?;
    let Some(evidence_span) = source.content.get(start..end) else {
        return Ok(None);
    };
    let object_is_grounded = if requires_exact_object {
        evidence_span.contains(object)
    } else {
        normalized_phrase(evidence_span).contains(&normalized_phrase(object))
    };
    Ok(object_is_grounded.then(|| (evidence_source, evidence_span.to_owned())))
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
    use crate::storage::{
        AtomicFact, AtomicFactRelation, AtomicFactRelationId, AtomicFactRelationKind, SqliteStorage,
    };
    use std::collections::VecDeque;

    #[test]
    fn reader_contract_classifies_reflection_and_output_form() {
        let direct =
            RecallPlan::infer("What value is configured for the worker?").reader_contract();
        assert_eq!(direct.reflection, ReflectionRecommendation::Optional);
        assert_eq!(direct.answer_form, ReaderAnswerForm::Direct);

        let temporal =
            RecallPlan::infer("Which task ran during the previous week?").reader_contract();
        assert_eq!(temporal.reflection, ReflectionRecommendation::Recommended);
        assert!(temporal.reflection_recommended());

        let alternatives =
            RecallPlan::infer("Would the operator choose the first or second route?")
                .reader_contract();
        assert_eq!(alternatives.answer_form, ReaderAnswerForm::Alternatives);

        let binary = RecallPlan::infer("Does the worker use the retry queue?").reader_contract();
        assert_eq!(binary.answer_form, ReaderAnswerForm::Binary);
    }

    #[test]
    fn reader_contract_emits_stage_and_shape_specific_rules() {
        let contract =
            RecallPlan::infer("How often did the worker renew its lease?").reader_contract();
        let reflection = contract.instruction(RecallReaderStage::Reflection);
        assert!(reflection.contains("every slot"));
        assert!(reflection.contains("source id"));
        assert!(reflection.contains("supported cadence"));

        let verification = contract.instruction(RecallReaderStage::Verification);
        assert!(verification.contains("Treat the draft as untrusted"));
        assert!(verification.contains("verified temporal arithmetic"));
        assert!(verification.contains("do not replace it with an abstention"));
        assert!(verification.contains("shortest verified complete answer"));

        let inference = RecallPlan::infer("Might the worker use the retry queue?")
            .reader_contract()
            .instruction(RecallReaderStage::Reflection);
        assert!(inference.contains("best-supported plausible conclusion"));
        assert!(inference.contains("without requiring the source"));

        let count =
            RecallPlan::infer("How many deployments did the worker complete?").reader_contract();
        assert!(
            count
                .instruction(RecallReaderStage::Reflection)
                .contains("source-cited event ledger")
        );
        assert!(
            count
                .instruction(RecallReaderStage::Verification)
                .contains("Recompute the count")
        );
        assert!(
            count
                .instruction(RecallReaderStage::Verification)
                .contains("rescan the whole delivered evidence")
        );
    }

    #[test]
    fn duration_reader_contract_requires_a_grounded_event_chain() {
        for query in [
            "How long did the vehicle restoration take?",
            "What was the elapsed time of the migration?",
            "What was the duration of the incident?",
            "복원 작업은 얼마 동안 진행됐어?",
        ] {
            let plan = RecallPlan::infer(query);
            assert_eq!(plan.answer_shape, AnswerShape::Temporal, "query: {query}");
            assert_eq!(plan.recall_intent, RecallIntent::Temporal, "query: {query}");
            assert!(plan.reader_contract().reflection_recommended());
        }

        let contract =
            RecallPlan::infer("How long did the vehicle restoration take?").reader_contract();
        let reflection = contract.instruction(RecallReaderStage::Reflection);
        for required_slot in [
            "entity identity",
            "start or projection",
            "intervening progress",
            "completion or end",
            "elapsed duration",
        ] {
            assert!(
                reflection.contains(required_slot),
                "missing duration slot: {required_slot}"
            );
        }
        assert!(reflection.contains("exact source ids"));
        assert!(reflection.contains("same speaker"));
        assert!(reflection.contains("same-session linkage"));
        assert!(reflection.contains("cross-session event continuity"));
        assert!(reflection.contains("lexical similarity alone is insufficient"));
        assert!(reflection.contains("projection as a forecast"));
        assert!(reflection.contains("whole delivered evidence"));

        let answer = contract.instruction(RecallReaderStage::Answer);
        assert!(answer.contains("source-grounded chronological event chain"));
        assert!(answer.contains("explicit source-stated duration"));
        assert!(answer.contains("grounded start and completion or end timestamps"));
        assert!(answer.contains("never use retrieval time"));

        let verification = contract.instruction(RecallReaderStage::Verification);
        assert!(verification.contains("Rebuild the source-cited chronological event chain"));
        assert!(verification.contains("before accepting the draft or an abstention"));
        assert!(verification.contains("same-speaker ownership"));
        assert!(verification.contains("recompute elapsed time"));

        let guidance = contract.context_guidance();
        assert!(guidance.contains("source-grounded chronological event chain"));
        assert!(guidance.contains("same-speaker ownership"));
        assert!(guidance.contains("cross-session event continuity"));
        assert!(guidance.contains("never use retrieval time"));
    }

    #[test]
    fn grounded_collection_reconciliation_deduplicates_and_checks_membership() {
        let contract =
            RecallPlan::infer("List every completed deployment target.").reader_contract();
        let draft = GroundedAnswerDraft::new(
            "north region, south region",
            vec![
                GroundedAnswerItem::new("North region", vec![NodeId(7)]),
                GroundedAnswerItem::new("north region", vec![NodeId(7)]),
                GroundedAnswerItem::new("South regions", vec![NodeId(9)]),
            ],
            vec![NodeId(7), NodeId(9)],
            false,
        );
        let attributions = vec![
            RecallSourceAttribution::new(
                NodeId(7),
                Some("operator".to_owned()),
                "operator: completed the north region",
                "session-a",
                NodeId(20),
                0,
            ),
            RecallSourceAttribution::new(
                NodeId(9),
                Some("operator".to_owned()),
                "operator: completed the south region",
                "session-a",
                NodeId(20),
                1,
            ),
        ];
        assert_eq!(
            contract.reconcile_grounded_draft_with_attributions(
                &draft,
                "North region",
                &[NodeId(7), NodeId(9)],
                &attributions,
            ),
            Some("North region, South regions".to_owned())
        );
        assert!(
            contract
                .reconcile_grounded_draft_with_attributions(
                    &draft,
                    "North region",
                    &[NodeId(7)],
                    &attributions,
                )
                .is_none()
        );
    }

    #[test]
    fn grounded_reconciliation_leaves_polarity_and_alternative_meaning_to_the_reader() {
        let binary = RecallPlan::infer("Does the worker use the retry queue?").reader_contract();
        let binary_draft =
            GroundedAnswerDraft::new("Yes; retry queue", Vec::new(), vec![NodeId(11)], false);
        let binary_attribution = [RecallSourceAttribution::new(
            NodeId(11),
            Some("worker".to_owned()),
            "worker: uses the retry queue",
            "session-a",
            NodeId(11),
            0,
        )];
        assert!(
            binary
                .reconcile_grounded_draft_with_attributions(
                    &binary_draft,
                    "No; primary queue",
                    &[NodeId(11)],
                    &binary_attribution,
                )
                .is_none()
        );

        let alternatives =
            RecallPlan::infer("Would the operator choose the first or second route?")
                .reader_contract();
        let alternative_draft =
            GroundedAnswerDraft::new("the second route", Vec::new(), vec![NodeId(13)], false);
        let alternative_attribution = [RecallSourceAttribution::new(
            NodeId(13),
            Some("operator".to_owned()),
            "operator: chose the second route",
            "session-a",
            NodeId(13),
            0,
        )];
        assert!(
            alternatives
                .reconcile_grounded_draft_with_attributions(
                    &alternative_draft,
                    "Yes; lower latency",
                    &[NodeId(13)],
                    &alternative_attribution,
                )
                .is_none()
        );
    }

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

    fn test_readout_candidate(node_id: NodeId, score: f64) -> ReadoutCandidate {
        ReadoutCandidate {
            node_id,
            score,
            activation: 1.0,
            phi: 0.0,
            embedding_cosine: 0.0,
            salience: 0.5,
            impedance: 1.0,
            scope_weight: 1.0,
            trust_weight: 1.0,
            stress: 0.0,
        }
    }

    fn add_test_turn(storage: &mut SqliteStorage, session: &str, content: &str) -> NodeId {
        let node_id = storage.next_node_id();
        storage
            .set_node(fixture_node(
                node_id,
                KnowledgeType::Episodic,
                content.to_owned(),
                session.to_owned(),
            ))
            .expect("test dialogue turn");
        node_id
    }

    fn connect_test_turns(storage: &mut SqliteStorage, source: NodeId, target: NodeId) {
        let edge_id = storage.next_edge_id();
        storage
            .set_edge(Edge::seeded(
                edge_id,
                source,
                target,
                EdgeType::Temporal,
                1.0,
                EdgeSource::Manual,
                Timestamp(1),
                Timestamp(1),
                HashMap::new(),
            ))
            .expect("test temporal edge");
    }

    #[test]
    fn relational_rerank_surface_keeps_a_native_reply_with_a_non_candidate_question() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let question = add_test_turn(
            &mut storage,
            "design-session",
            "Reviewer: What motivated the cobalt pattern?",
        );
        let unrelated = add_test_turn(
            &mut storage,
            "other-session",
            "Operator: The staging queue completed normally.",
        );
        let answer = add_test_turn(
            &mut storage,
            "design-session",
            "Operator: I chose it to catch attention and make people smile.",
        );
        connect_test_turns(&mut storage, question, answer);

        let ranking = vec![
            test_readout_candidate(unrelated, 2.0),
            test_readout_candidate(answer, 1.0),
        ];
        let baseline = compile_evidence_documents(&storage, &ranking, ranking.len())
            .expect("baseline evidence documents");
        let documents = compile_rerank_documents(
            &storage,
            &RecallPlan::infer("Why did the operator choose the cobalt pattern?"),
            &ranking,
            ranking.len(),
            &[],
        )
        .expect("relational rerank documents");

        assert_eq!(documents.len(), baseline.len());
        for (document, native) in documents.iter().zip(&baseline) {
            assert_eq!(document.node_id, native.node_id);
            assert_eq!(document.source_node_ids, native.source_node_ids);
            assert_eq!(document.text, native.text);
        }
        let answer_document = documents
            .iter()
            .find(|document| document.node_id == answer)
            .expect("native answer document");
        assert!(!answer_document.text.contains("What motivated"));
        assert!(answer_document.rerank_text().contains("What motivated"));
        assert!(answer_document.rerank_text().contains("I chose it"));
    }

    #[test]
    fn reply_context_is_same_session_query_focused_and_globally_bounded() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut node_ids = Vec::new();

        let cross_session_question = add_test_turn(
            &mut storage,
            "cross-question",
            "Reviewer: Why was the cobalt rollout delayed?",
        );
        let cross_session_answer = add_test_turn(
            &mut storage,
            "cross-answer",
            "Operator: The cobalt rollout was delayed by approval.",
        );
        connect_test_turns(&mut storage, cross_session_question, cross_session_answer);
        node_ids.extend([cross_session_question, cross_session_answer]);

        let unrelated_question = add_test_turn(
            &mut storage,
            "unrelated-session",
            "Reviewer: What color is the garden fence?",
        );
        let unrelated_answer = add_test_turn(
            &mut storage,
            "unrelated-session",
            "Operator: The garden fence is green.",
        );
        connect_test_turns(&mut storage, unrelated_question, unrelated_answer);
        node_ids.extend([unrelated_question, unrelated_answer]);

        let mut first_answer = None;
        for index in 0..4 {
            let session = format!("rollout-session-{index}");
            let question = add_test_turn(
                &mut storage,
                &session,
                &format!("Reviewer: Why did cobalt rollout {index} stop?"),
            );
            let answer = add_test_turn(
                &mut storage,
                &session,
                &format!("Operator: Cobalt rollout {index} stopped for approval."),
            );
            connect_test_turns(&mut storage, question, answer);
            first_answer.get_or_insert(answer);
            node_ids.extend([question, answer]);
        }

        let tail = add_test_turn(
            &mut storage,
            "rollout-session-0",
            "Operator: This unrelated tail must remain ordinary evidence.",
        );
        connect_test_turns(
            &mut storage,
            first_answer.expect("first valid answer"),
            tail,
        );
        node_ids.push(tail);

        let ranking: Vec<_> = node_ids
            .iter()
            .enumerate()
            .map(|(index, node_id)| test_readout_candidate(*node_id, 100.0 - index as f64))
            .collect();
        let baseline = compile_evidence_documents(&storage, &ranking, ranking.len())
            .expect("baseline evidence documents");
        let documents = compile_rerank_documents(
            &storage,
            &RecallPlan::infer("Why was the cobalt rollout delayed?"),
            &ranking,
            ranking.len(),
            &[],
        )
        .expect("bounded reply-context documents");

        assert_eq!(documents.len(), baseline.len());
        assert_eq!(
            documents
                .iter()
                .map(|document| (
                    document.node_id,
                    document.source_node_ids.clone(),
                    document.text.clone()
                ))
                .collect::<Vec<_>>(),
            baseline
                .iter()
                .map(|document| (
                    document.node_id,
                    document.source_node_ids.clone(),
                    document.text.clone()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            documents
                .iter()
                .filter(|document| {
                    document
                        .rerank_text()
                        .contains("Immediate same-session question:")
                })
                .count(),
            MAX_SAME_SESSION_REPLY_BRIDGES
        );
        for unbridged in [cross_session_answer, unrelated_answer, tail] {
            let document = documents
                .iter()
                .find(|document| document.node_id == unbridged)
                .expect("unbridged native document");
            assert_eq!(document.rerank_text(), document.text);
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

    #[allow(clippy::too_many_arguments)]
    fn seed_structured_atomic_fact(
        storage: &mut SqliteStorage,
        fact_id: AtomicFactId,
        source_node_id: NodeId,
        content: &str,
        subject: &str,
        relation: &str,
        object: &str,
        evidence_object: &str,
        evidence_span: &str,
    ) {
        let (source_session_id, scope, observed_at, start) = {
            let source = storage
                .get_node(source_node_id)
                .expect("structured atomic fact source");
            (
                source.origin.session_id.clone(),
                source.origin.scope.clone(),
                source.created_at,
                source
                    .content
                    .find(evidence_span)
                    .expect("structured evidence span"),
            )
        };
        let metadata = HashMap::from([
            ("anamnesis:ground-subject".to_owned(), subject.to_owned()),
            ("anamnesis:ground-relation".to_owned(), relation.to_owned()),
            ("anamnesis:ground-object".to_owned(), object.to_owned()),
            (
                "anamnesis:evidence-object".to_owned(),
                evidence_object.to_owned(),
            ),
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
        ]);
        storage
            .set_atomic_fact(AtomicFact {
                id: fact_id,
                content: content.to_owned(),
                embedding: vec![0.0, 1.0],
                source_node_ids: vec![source_node_id],
                entity_tags: Vec::new(),
                source_session_id,
                scope,
                observed_at,
                valid_from: None,
                valid_until: None,
                metadata,
            })
            .expect("structured atomic fact");
    }

    fn seed_temporal_structured_fact(
        storage: &mut SqliteStorage,
        subject: &str,
        activity: &str,
        observed_at: Timestamp,
    ) -> (NodeId, AtomicFactId) {
        let source_id = storage.next_node_id();
        let source_content = format!("{subject} was pursuing {activity}.");
        let mut source = fixture_node(
            source_id,
            KnowledgeType::Episodic,
            source_content.clone(),
            format!("{subject}-{activity}-session"),
        );
        source.created_at = observed_at;
        source.updated_at = observed_at;
        source.accessed_at = observed_at;
        source.embedding = Some(vec![0.0, 1.0]);
        storage.set_node(source).expect("temporal raw source");

        let fact_id = storage.next_atomic_fact_id().expect("temporal fact id");
        seed_structured_atomic_fact(
            storage,
            fact_id,
            source_id,
            &format!("{subject} pursued {activity}"),
            subject,
            "pursued",
            activity,
            activity,
            &source_content,
        );
        (source_id, fact_id)
    }

    fn seed_reviewed_atomic_relation(
        storage: &mut SqliteStorage,
        from_fact_id: AtomicFactId,
        to_fact_id: AtomicFactId,
        kind: AtomicFactRelationKind,
        key: &str,
    ) -> AtomicFactRelationId {
        let id = storage.next_atomic_fact_relation_id().expect("relation id");
        storage
            .set_atomic_fact_relation(AtomicFactRelation {
                id,
                from_fact_id,
                to_fact_id,
                kind,
                reviewed_by: "reviewer".to_owned(),
                review_profile: "policy-v1".to_owned(),
                reviewed_at: Timestamp(2),
                idempotency_key: key.to_owned(),
                scope: ScopePath::universal(),
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            })
            .expect("reviewed relation");
        id
    }

    fn atomic_chain_fixture(contents: &[&str]) -> (SqliteStorage, Vec<NodeId>, Vec<AtomicFactId>) {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut source_ids = Vec::with_capacity(contents.len());
        let mut fact_ids = Vec::with_capacity(contents.len());
        for (index, content) in contents.iter().enumerate() {
            let source_id = storage.next_node_id();
            let mut source = fixture_node(
                source_id,
                KnowledgeType::Episodic,
                (*content).to_owned(),
                format!("chain-session-{index}"),
            );
            source.embedding = Some(vec![1.0, 0.0]);
            storage.set_node(source).expect("chain raw source");
            let fact_id = storage.next_atomic_fact_id().expect("chain fact id");
            seed_legacy_atomic_fact(&mut storage, fact_id, source_id);
            source_ids.push(source_id);
            fact_ids.push(fact_id);
        }
        (storage, source_ids, fact_ids)
    }

    fn direct_atomic_route(source_id: NodeId, fact_id: AtomicFactId) -> RoutedAtomicSource {
        RoutedAtomicSource {
            candidate: ReadoutCandidate {
                node_id: source_id,
                score: 1.0,
                activation: 1.0,
                phi: 1.0,
                embedding_cosine: 1.0,
                salience: 0.5,
                impedance: 0.0,
                scope_weight: 1.0,
                trust_weight: 1.0,
                stress: 0.0,
            },
            kind_priority: 0,
            fact_ids: vec![fact_id],
            origin: AtomicRouteOrigin::Direct,
        }
    }

    fn expand_chain_from_seed(
        storage: &SqliteStorage,
        source_id: NodeId,
        fact_id: AtomicFactId,
        now: Timestamp,
        scope: &ScopePath,
    ) -> (Vec<RoutedAtomicSource>, AtomicChainDiagnostics) {
        let query_embedding = [1.0, 0.0];
        let expansion = expand_atomic_fact_relation_sources(
            storage,
            &RecallPlan::infer("How are the recorded events and their causes related?"),
            &[direct_atomic_route(source_id, fact_id)],
            &[query_embedding.as_slice()],
            now,
            scope,
        )
        .expect("bounded relation chain expansion");
        (expansion.sources, expansion.diagnostics)
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
            RecallPlan::infer("What deployment tools has Nimbus developed?").answer_shape,
            AnswerShape::Collection
        );
        assert_eq!(
            RecallPlan::infer("What operational incidents does Nimbus face?").answer_shape,
            AnswerShape::Collection
        );
        assert_eq!(
            RecallPlan::infer("What kind of cache does Nimbus use?").answer_shape,
            AnswerShape::Fact
        );
        assert_eq!(
            RecallPlan::infer("Which storage engine does the service use?").answer_shape,
            AnswerShape::Fact
        );
        assert_eq!(
            RecallPlan::infer("Would the release team postpone the rollout?").answer_shape,
            AnswerShape::Inference
        );
        assert_eq!(
            RecallPlan::infer("What might Alice do next?").answer_shape,
            AnswerShape::Inference
        );
        assert_eq!(
            RecallPlan::infer("Which deployment mode does the team prefer more than others?")
                .answer_shape,
            AnswerShape::Inference
        );
        assert_eq!(
            RecallPlan::infer("Which region could the service potentially run in?").answer_shape,
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
        let frequency = RecallPlan::infer("How often does the worker refresh its lease?");
        assert_eq!(frequency.answer_shape, AnswerShape::Frequency);
        assert_eq!(frequency.recall_intent, RecallIntent::Temporal);
        assert_eq!(
            RecallPlan::infer("Why did the service move its queue?").answer_shape,
            AnswerShape::Relationship
        );
        let shared_reason = RecallPlan::infer("Why do the API and worker share a queue?");
        assert_eq!(shared_reason.answer_shape, AnswerShape::Relationship);
        assert_eq!(shared_reason.recall_intent, RecallIntent::Relational);
        assert_eq!(adaptive_delivery_limit(&shared_reason, 20), 20);
        let manner = RecallPlan::infer("How did the team roll out the schema migration?");
        assert_eq!(manner.answer_shape, AnswerShape::Relationship);
        assert_eq!(manner.recall_intent, RecallIntent::Relational);
        let origin = RecallPlan::infer("Where did the backup move from 4 years ago?");
        assert_eq!(origin.answer_shape, AnswerShape::Relationship);
        assert_eq!(origin.recall_intent, RecallIntent::Temporal);
        let completed = RecallPlan::infer("What has the release team done with the migration?");
        assert_eq!(completed.answer_shape, AnswerShape::Collection);
        assert_eq!(completed.recall_intent, RecallIntent::Enumeration);
        let yes_no = RecallPlan::infer("Does the worker use the retry queue?");
        assert_eq!(yes_no.answer_shape, AnswerShape::Inference);
        assert_eq!(yes_no.recall_intent, RecallIntent::Relational);
        let candidate =
            RecallPlan::infer("What rollout strategy could the team use to reduce downtime?");
        assert_eq!(candidate.answer_shape, AnswerShape::Inference);
        assert_eq!(candidate.recall_intent, RecallIntent::Relational);
        let candidate_guidance = product_reader_guidance(&RecallPlan::infer(
            "Which option would best satisfy the stated constraints?",
        ));
        let consequence_guidance =
            product_reader_guidance(&RecallPlan::infer("What might happen next?"));
        assert_eq!(candidate_guidance, consequence_guidance);
        assert!(candidate_guidance.contains("one concise"));
        let inference_guidance =
            product_reader_guidance(&RecallPlan::infer("What might Alice do next?"));
        assert!(inference_guidance.contains("widely known background knowledge"));
        assert!(inference_guidance.contains("never invent personal facts"));
        assert!(inference_guidance.contains("source-grounded premise is absent"));
        assert!(inference_guidance.contains("yes/no or likely question"));
        assert!(facet_terms("Which option is preferred?").contains("prefer"));
        assert!(facet_terms("This option is a favorite.").contains("prefer"));
        assert!(
            product_reader_guidance(&RecallPlan::infer("Where does Alice live?"))
                .contains("requested attribute and granularity")
        );
        assert!(
            product_reader_guidance(&RecallPlan::infer(
                "Which activity was Alice pursuing on March 16, 2022?"
            ))
            .contains("resolved event times")
        );
        assert!(
            product_reader_guidance(&RecallPlan::infer("How long did the restoration take?"))
                .contains("source-grounded chronological event chain")
        );
        assert!(
            product_reader_guidance(&RecallPlan::infer(
                "Which regions might the service use given its latency constraints?"
            ))
            .contains("every distinct plausible item")
        );
        assert!(
            product_reader_guidance(&RecallPlan::infer(
                "Where did the backup move from 4 years ago?"
            ))
            .contains("resolves its referenced entity")
        );
        assert!(
            product_reader_guidance(&RecallPlan::infer("Which projects did Alice complete?"))
                .contains("every distinct item")
        );
        let configured_policy =
            RecallPlan::infer("What cache policy is configured for the worker?");
        assert_eq!(configured_policy.answer_shape, AnswerShape::Fact);
        assert_eq!(configured_policy.recall_intent, RecallIntent::Direct);
        assert_eq!(
            RecallPlan::infer("Which regions might the service use given its latency constraints?")
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
        let query = "When did John visit the greenhouse?";
        assert!(temporal_evidence_matches(
            query,
            "John visited the greenhouse last week."
        ));
        assert!(!temporal_evidence_matches(
            query,
            "John won an intense basketball game last week."
        ));
        assert!(temporal_evidence_matches(
            "Which release did Nimbus publish in January 2023?",
            "Nimbus: Two weeks ago I published release 4.2."
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
        ] {
            assert!(
                !uses_atomic_fact_expansion(&RecallPlan::infer(query)),
                "{query:?} must preserve the conservative production path"
            );
        }
        for query in ["Why did Alice move?", "What device could Alice give Bob?"] {
            assert!(
                uses_atomic_fact_expansion(&RecallPlan::infer(query)),
                "{query:?} should follow the typed complex-query policy"
            );
        }
        for query in [
            "What projects did Alice complete before 2020?",
            "Why did Alice move after 2020?",
            "What device could Alice give Bob before 2020?",
        ] {
            assert!(
                !uses_atomic_fact_expansion(&RecallPlan::infer(query)),
                "{query:?} has a temporal constraint and must preserve temporal recall"
            );
        }
    }

    #[test]
    fn atomic_routing_uses_the_strongest_batched_query_surface() {
        let primary = [1.0, 0.0];
        let relation = [0.0, 1.0];
        let candidate = [0.0, 1.0];

        assert_eq!(
            max_query_cosine(&[primary.as_slice(), relation.as_slice()], &candidate),
            1.0
        );
        assert_eq!(max_query_cosine(&[], &candidate), 0.0);
    }

    #[test]
    fn atomic_subject_match_distinguishes_fact_ownership_from_incidental_tags() {
        let metadata =
            HashMap::from([("anamnesis:ground-subject".to_owned(), "Nimbus".to_owned())]);

        assert_eq!(
            atomic_subject_matches("Which service would Nimbus use?", &metadata),
            1
        );
        assert_eq!(
            atomic_subject_matches("Which service would Atlas use?", &metadata),
            0
        );
        assert_eq!(
            atomic_subject_matches("Which service does someone sometimes use?", &metadata),
            0,
            "a subject must match token boundaries, not a name substring"
        );
    }

    #[test]
    fn inference_modal_recovers_the_hybrid_collection_signal() {
        assert!(query_has_inference_modal(
            "What personality traits might Alice have?"
        ));
        assert!(!query_has_inference_modal(
            "What personal health incidents did Alice have?"
        ));
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
    fn inference_claim_reserve_prefers_the_strongest_routed_grounded_fact() {
        let (mut storage, readout, _) = ranked_fixture();
        let mut ranking: Vec<_> = readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect();
        let lower_priority_source = storage.next_node_id();
        storage
            .set_node(fixture_node(
                lower_priority_source,
                KnowledgeType::Episodic,
                "Alice is adventurous".to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("lower-priority source");
        let strongest_source = storage.next_node_id();
        storage
            .set_node(fixture_node(
                strongest_source,
                KnowledgeType::Episodic,
                "Alice is thoughtful".to_owned(),
                "claim-session".to_owned(),
            ))
            .expect("strongest source");

        // The consumer reranker prefers the source at row 20, while the
        // query-relative atomic router puts the more specific source first.
        ranking[20].node_id = lower_priority_source;
        ranking[30].node_id = strongest_source;
        seed_grounded_atomic_fact(
            &mut storage,
            AtomicFactId(1),
            strongest_source,
            "Alice is thoughtful",
            "thoughtful",
        );
        seed_grounded_atomic_fact(
            &mut storage,
            AtomicFactId(2),
            lower_priority_source,
            "Alice is adventurous",
            "adventurous",
        );
        let markers = [
            AtomicSourceMarker {
                source_node_id: strongest_source,
                kind_priority: 0,
                fact_id: Some(AtomicFactId(1)),
            },
            AtomicSourceMarker {
                source_node_id: lower_priority_source,
                kind_priority: 0,
                fact_id: Some(AtomicFactId(2)),
            },
        ];

        let plan = RecallPlan::infer("What kind of person might Alice be?");
        assert_eq!(plan.answer_shape, AnswerShape::Inference);
        let selected = compile_ranking(
            &storage,
            &plan,
            &ranking,
            EvidenceSelection::Auto,
            20,
            &markers,
        )
        .expect("inference claim-slot selection");

        assert!(
            selected
                .iter()
                .any(|candidate| candidate.node_id == strongest_source),
            "the one grounded inference reserve must follow atomic route order"
        );
        assert!(
            selected
                .iter()
                .all(|candidate| candidate.node_id != lower_priority_source),
            "the lower atomic route must not consume the single reserve"
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
    fn rerank_surface_adds_only_byte_grounded_atomic_cues() {
        let (mut storage, ranking, _) = ranked_fixture();
        let source_id = ranking[0].node_id;
        let mut source = storage
            .get_node(source_id)
            .expect("grounded cue source")
            .clone();
        source.content = "Alice completed the cobalt launch project".to_owned();
        source.name = source.content.clone();
        storage.set_node(source).expect("updated cue source");
        seed_grounded_atomic_fact(
            &mut storage,
            AtomicFactId(1),
            source_id,
            "completed the cobalt launch project",
            "cobalt launch project",
        );
        let marker = AtomicSourceMarker {
            source_node_id: source_id,
            kind_priority: 0,
            fact_id: Some(AtomicFactId(1)),
        };

        let documents = compile_rerank_documents(
            &storage,
            &RecallPlan::infer("How did Alice complete the cobalt project?"),
            &ranking,
            50,
            &[marker],
        )
        .expect("grounded rerank documents");
        let document = documents
            .iter()
            .find(|document| document.source_node_ids.contains(&source_id))
            .expect("grounded source document");

        assert_eq!(document.text, "Alice completed the cobalt launch project");
        assert!(
            document
                .rerank_text()
                .contains("Fact: Alice completed cobalt launch project")
        );
        assert!(
            document
                .rerank_text()
                .contains("Exact source span: completed the cobalt launch project")
        );

        let mut invalid_fact = storage
            .get_atomic_fact(AtomicFactId(1))
            .expect("grounded fact")
            .clone();
        invalid_fact.metadata.insert(
            "anamnesis:evidence-span-end".to_owned(),
            usize::MAX.to_string(),
        );
        storage
            .set_atomic_fact(invalid_fact)
            .expect("invalidated grounded fact");
        let invalid_documents = compile_rerank_documents(
            &storage,
            &RecallPlan::infer("How did Alice complete the cobalt project?"),
            &ranking,
            50,
            &[marker],
        )
        .expect("invalid grounding falls back to raw");
        let invalid_document = invalid_documents
            .iter()
            .find(|document| document.source_node_ids.contains(&source_id))
            .expect("raw fallback source document");
        assert_eq!(invalid_document.rerank_text(), invalid_document.text);
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
    fn direct_and_temporal_question_shapes_preserve_the_prefix() {
        let (storage, ranking, rare_node_id) = ranked_fixture();
        for query in [
            "Where is the rarecomet project?",
            "When did the rarecomet project start?",
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
    fn atomic_source_can_route_itself_and_a_bounded_neighbor_from_the_deep_trace() {
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
                .any(|candidate| candidate.node_id == routed_source),
            "the exact raw source selected by the isolated fact lane must reach the reranker"
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

    #[test]
    fn atomic_entity_matching_uses_canonical_subject_without_double_counting() {
        let query = "Which regions has Nimbus deployed to?";
        let mut metadata = HashMap::new();
        metadata.insert("anamnesis:ground-subject".to_owned(), "Nimbus".to_owned());

        assert_eq!(
            atomic_entity_matches(query, &["eu-west".to_owned()], &metadata),
            1,
            "the canonical subject remains routable when the extractor omits it from entity tags"
        );
        assert_eq!(
            atomic_entity_matches(
                query,
                &["Nimbus".to_owned(), "eu-west".to_owned()],
                &metadata,
            ),
            1,
            "the canonical subject must not be counted twice"
        );
    }

    #[test]
    fn subject_raw_routing_is_exact_speaker_scoped_and_complex_only() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let (alice_project, bob_project) = {
            let mut add_raw = |speaker: &str, session: &str, content: &str, embedding: Vec<f64>| {
                let id = storage.next_node_id();
                let mut node = fixture_node(
                    id,
                    KnowledgeType::Episodic,
                    content.to_owned(),
                    session.to_owned(),
                );
                node.embedding = Some(embedding);
                node.entity_tags = vec![format!("speaker-{}", speaker.to_lowercase())];
                storage.set_node(node).expect("raw source");
                id
            };
            let alice_project = add_raw(
                "Alice",
                "alice-session",
                "Alice built the cobalt project",
                vec![1.0, 0.0],
            );
            let _alice_holiday = add_raw(
                "Alice",
                "alice-holiday",
                "Alice visited the coast",
                vec![0.0, 1.0],
            );
            let bob_project = add_raw(
                "Bob",
                "bob-session",
                "Bob built the cobalt project",
                vec![1.0, 0.0],
            );
            (alice_project, bob_project)
        };

        let query_embedding = vec![1.0, 0.0];
        let routed = route_subject_raw_sources(
            &storage,
            &RecallPlan::infer("What projects has Alice built?"),
            &[query_embedding.as_slice()],
            Timestamp(2),
            &ScopePath::universal(),
        )
        .expect("subject raw route");
        assert!(
            routed
                .iter()
                .any(|candidate| candidate.node_id == alice_project)
        );
        assert!(
            routed
                .iter()
                .all(|candidate| candidate.node_id != bob_project),
            "semantic similarity must never leak a different speaker into the isolated lane"
        );

        let direct = route_subject_raw_sources(
            &storage,
            &RecallPlan::infer("Where does Alice live?"),
            &[query_embedding.as_slice()],
            Timestamp(2),
            &ScopePath::universal(),
        )
        .expect("direct route");
        assert!(
            direct.is_empty(),
            "one-fact queries must retain the proven production route"
        );

        let hypothetical = route_subject_raw_sources(
            &storage,
            &RecallPlan::infer("What projects might Alice build next?"),
            &[query_embedding.as_slice()],
            Timestamp(2),
            &ScopePath::universal(),
        )
        .expect("hypothetical route");
        assert!(
            hypothetical.is_empty(),
            "hypothetical collections must not receive broad raw context"
        );

        let temporal = route_subject_raw_sources(
            &storage,
            &RecallPlan::infer("What projects had Alice built before 2020?"),
            &[query_embedding.as_slice()],
            Timestamp(2),
            &ScopePath::universal(),
        )
        .expect("temporal collection route");
        assert!(
            temporal.is_empty(),
            "temporal collections must not receive subject-raw expansion"
        );
    }

    #[test]
    fn structured_atomic_metadata_routes_only_to_its_grounded_raw_source() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let source_id = storage.next_node_id();
        let source_content =
            "Gina said her online store showcases work by a local artist.".to_owned();
        storage
            .set_node(fixture_node(
                source_id,
                KnowledgeType::Episodic,
                source_content.clone(),
                "gina-store-session".to_owned(),
            ))
            .expect("grounded raw source");
        let fact_id = storage.next_atomic_fact_id().expect("fact id");
        seed_structured_atomic_fact(
            &mut storage,
            fact_id,
            source_id,
            "The speaker described a creative arrangement",
            "Gina",
            "showcases work by",
            "independent creator shop",
            "local artist",
            &source_content,
        );

        let query_embedding = [1.0, 0.0];
        for query in [
            "What relationship involved an independent creator shop?",
            "What relationship involved a local artist?",
        ] {
            let routed = route_atomic_fact_sources(
                &storage,
                &RecallPlan::infer_with_answer_shape(query, AnswerShape::Relationship),
                &[query_embedding.as_slice()],
                Timestamp(5),
                &ScopePath::universal(),
            )
            .expect("structured metadata route");
            assert_eq!(routed.len(), 1, "query: {query}");
            assert_eq!(routed[0].candidate.node_id, source_id, "query: {query}");
            assert_eq!(routed[0].fact_ids, vec![fact_id], "query: {query}");
        }

        let direct = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape("Where is the local artist?", AnswerShape::Fact),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("direct route remains isolated");
        assert!(direct.is_empty());

        let temporal = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "What relationship involved a local artist before 2020?",
                AnswerShape::Relationship,
            ),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("temporal route remains isolated");
        assert!(temporal.is_empty());
    }

    #[test]
    fn temporal_fact_reserve_admits_observed_or_event_interval_overlap() {
        const MAY_8_2023: u64 = 1_683_504_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let (observed_source, _) = seed_temporal_structured_fact(
            &mut storage,
            "Nimbus",
            "trail running",
            Timestamp(MAY_8_2023 + 3_600_000),
        );
        let (event_source, event_fact_id) = seed_temporal_structured_fact(
            &mut storage,
            "Nimbus",
            "route planning",
            Timestamp(MAY_8_2023 + DAY_MS * 2),
        );
        let mut event_fact = storage
            .get_atomic_fact(event_fact_id)
            .expect("event fact")
            .clone();
        event_fact.valid_from = Some(Timestamp(MAY_8_2023));
        event_fact.valid_until = Some(Timestamp(MAY_8_2023 + DAY_MS));
        storage
            .set_atomic_fact(event_fact)
            .expect("event interval stores");

        let plan = RecallPlan::infer_with_answer_shape(
            "What activity was Nimbus pursuing on 2023-05-08?",
            AnswerShape::Fact,
        );
        assert_eq!(plan.recall_intent, RecallIntent::Temporal);
        let query_embedding = [0.0, 1.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &plan,
            &[query_embedding.as_slice()],
            Timestamp(MAY_8_2023 + DAY_MS * 4),
            &ScopePath::universal(),
        )
        .expect("temporal fact reserve");
        let routed_ids: HashSet<_> = routed
            .iter()
            .map(|source| source.candidate.node_id)
            .collect();
        assert_eq!(
            routed_ids,
            HashSet::from([observed_source, event_source]),
            "either an observation point or explicit fact interval may satisfy the date"
        );
    }

    #[test]
    fn temporal_fact_reserve_excludes_out_of_range_facts() {
        const MAY_8_2023: u64 = 1_683_504_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        seed_temporal_structured_fact(
            &mut storage,
            "Nimbus",
            "trail running",
            Timestamp(MAY_8_2023 + DAY_MS),
        );

        let query_embedding = [0.0, 1.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "What activity was Nimbus pursuing on 2023-05-08?",
                AnswerShape::Fact,
            ),
            &[query_embedding.as_slice()],
            Timestamp(MAY_8_2023 + DAY_MS * 4),
            &ScopePath::universal(),
        )
        .expect("out-of-range temporal fact route");
        assert!(routed.is_empty());
    }

    #[test]
    fn temporal_fact_reserve_excludes_a_different_exact_subject() {
        const MAY_8_2023: u64 = 1_683_504_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        seed_temporal_structured_fact(
            &mut storage,
            "Atlas",
            "trail running",
            Timestamp(MAY_8_2023 + 3_600_000),
        );

        let query_embedding = [0.0, 1.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "What activity was Nimbus pursuing on 2023-05-08?",
                AnswerShape::Fact,
            ),
            &[query_embedding.as_slice()],
            Timestamp(MAY_8_2023 + DAY_MS * 4),
            &ScopePath::universal(),
        )
        .expect("wrong-subject temporal fact route");
        assert!(routed.is_empty());
    }

    #[test]
    fn temporal_fact_reserve_is_bounded_to_two_raw_sources_per_route() {
        const MAY_8_2023: u64 = 1_683_504_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        for index in 0..6 {
            seed_temporal_structured_fact(
                &mut storage,
                "Nimbus",
                &format!("activity {index}"),
                Timestamp(MAY_8_2023 + index * 1_000),
            );
        }

        let query_embedding = [0.0, 1.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "What activity was Nimbus pursuing on 2023-05-08?",
                AnswerShape::Fact,
            ),
            &[query_embedding.as_slice()],
            Timestamp(MAY_8_2023 + DAY_MS * 4),
            &ScopePath::universal(),
        )
        .expect("bounded temporal fact route");
        assert_eq!(routed.len(), TEMPORAL_FACT_RESERVE_SOURCE_LIMIT);
        assert_eq!(
            routed
                .iter()
                .map(|source| source.candidate.node_id)
                .collect::<HashSet<_>>()
                .len(),
            TEMPORAL_FACT_RESERVE_SOURCE_LIMIT
        );
    }

    #[test]
    fn temporal_fact_reserve_does_not_widen_direct_or_other_temporal_shapes() {
        const MAY_8_2023: u64 = 1_683_504_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let (source_id, _) = seed_temporal_structured_fact(
            &mut storage,
            "Nimbus",
            "trail running",
            Timestamp(MAY_8_2023 + 3_600_000),
        );
        let query_embedding = [0.0, 1.0];
        let now = Timestamp(MAY_8_2023 + DAY_MS * 4);

        let direct = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "What activity is Nimbus pursuing?",
                AnswerShape::Fact,
            ),
            &[query_embedding.as_slice()],
            now,
            &ScopePath::universal(),
        )
        .expect("direct route");
        assert!(direct.is_empty());

        let temporal_answer = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "When did Nimbus pursue trail running?",
                AnswerShape::Temporal,
            ),
            &[query_embedding.as_slice()],
            now,
            &ScopePath::universal(),
        )
        .expect("temporal answer route");
        assert!(temporal_answer.is_empty());

        let frequency = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "How often did Nimbus pursue trail running?",
                AnswerShape::Frequency,
            ),
            &[query_embedding.as_slice()],
            now,
            &ScopePath::universal(),
        )
        .expect("existing frequency route");
        assert!(
            frequency
                .iter()
                .any(|source| source.candidate.node_id == source_id),
            "the existing temporal-frequency atomic lane must remain enabled"
        );
    }

    #[test]
    fn atomic_metadata_cannot_route_unapproved_retracted_or_stale_records() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");

        let unrelated_source_id = storage.next_node_id();
        storage
            .set_node(fixture_node(
                unrelated_source_id,
                KnowledgeType::Episodic,
                "an opaque source fragment".to_owned(),
                "unrelated-session".to_owned(),
            ))
            .expect("unrelated raw source");
        let unrelated_fact_id = storage.next_atomic_fact_id().expect("unrelated fact id");
        let unrelated_source = storage
            .get_node(unrelated_source_id)
            .expect("unrelated source exists");
        storage
            .set_atomic_fact(AtomicFact {
                id: unrelated_fact_id,
                content: "opaque arrangement summary".to_owned(),
                embedding: vec![0.0, 1.0],
                source_node_ids: vec![unrelated_source_id],
                entity_tags: Vec::new(),
                source_session_id: unrelated_source.origin.session_id.clone(),
                scope: unrelated_source.origin.scope.clone(),
                observed_at: unrelated_source.created_at,
                valid_from: None,
                valid_until: None,
                metadata: HashMap::from([(
                    "consumer:unrelated-note".to_owned(),
                    "local artist".to_owned(),
                )]),
            })
            .expect("unrelated fact");

        let retracted_source_id = storage.next_node_id();
        let retracted_content = "The gallery works with a local artist.".to_owned();
        storage
            .set_node(fixture_node(
                retracted_source_id,
                KnowledgeType::Episodic,
                retracted_content.clone(),
                "retracted-session".to_owned(),
            ))
            .expect("retracted raw source");
        let retracted_fact_id = storage.next_atomic_fact_id().expect("retracted fact id");
        seed_structured_atomic_fact(
            &mut storage,
            retracted_fact_id,
            retracted_source_id,
            "opaque arrangement summary",
            "The gallery",
            "works with",
            "independent maker",
            "local artist",
            &retracted_content,
        );
        let mut retracted = storage
            .get_atomic_fact(retracted_fact_id)
            .expect("retracted fact exists")
            .clone();
        retracted
            .metadata
            .insert("retracted".to_owned(), "true".to_owned());
        storage
            .set_atomic_fact(retracted)
            .expect("retracted fact stores");

        let stale_source_id = storage.next_node_id();
        let stale_content = "The workshop features a local artist.".to_owned();
        storage
            .set_node(fixture_node(
                stale_source_id,
                KnowledgeType::Episodic,
                stale_content.clone(),
                "stale-session".to_owned(),
            ))
            .expect("stale raw source");
        let stale_fact_id = storage.next_atomic_fact_id().expect("stale fact id");
        seed_structured_atomic_fact(
            &mut storage,
            stale_fact_id,
            stale_source_id,
            "opaque arrangement summary",
            "The workshop",
            "features",
            "independent maker",
            "local artist",
            &stale_content,
        );
        let mut changed_source = storage
            .get_node(stale_source_id)
            .expect("stale source exists")
            .clone();
        changed_source.content.push_str(" The source was revised.");
        storage
            .set_node(changed_source)
            .expect("changed source stores");

        let query_embedding = [1.0, 0.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer_with_answer_shape(
                "What relationship involved a local artist?",
                AnswerShape::Relationship,
            ),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("ineligible metadata route");
        assert!(routed.is_empty());
    }

    #[test]
    fn direct_atomic_routing_rejects_future_facts_and_sources() {
        let (mut storage, source_ids, fact_ids) =
            atomic_chain_fixture(&["future fact evidence", "future source evidence"]);
        let mut future_fact = storage
            .get_atomic_fact(fact_ids[0])
            .expect("future fact exists")
            .clone();
        future_fact.observed_at = Timestamp(10);
        storage
            .set_atomic_fact(future_fact)
            .expect("future fact stores");
        let mut future_source = storage
            .get_node(source_ids[1])
            .expect("future source exists")
            .clone();
        future_source.created_at = Timestamp(10);
        storage
            .set_node(future_source)
            .expect("future source stores");

        let query_embedding = [1.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer("Why is the future evidence related?"),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("future-safe direct route");
        assert!(routed.is_empty());
    }

    #[test]
    fn subject_raw_routing_rejects_future_and_disjoint_scope_sources() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let scope_a = ScopePath::new("workspace/a").expect("scope a");
        let scope_b = ScopePath::new("workspace/b").expect("scope b");

        let scoped_id = storage.next_node_id();
        let mut scoped = fixture_node(
            scoped_id,
            KnowledgeType::Episodic,
            "Alice built the scoped project".to_owned(),
            "scoped-session".to_owned(),
        );
        scoped.origin.scope = scope_a;
        scoped.entity_tags = vec!["speaker-alice".to_owned()];
        scoped.embedding = Some(vec![1.0, 0.0]);
        storage.set_node(scoped).expect("scoped source stores");

        let future_id = storage.next_node_id();
        let mut future = fixture_node(
            future_id,
            KnowledgeType::Episodic,
            "Alice built the future project".to_owned(),
            "future-session".to_owned(),
        );
        future.created_at = Timestamp(10);
        future.entity_tags = vec!["speaker-alice".to_owned()];
        future.embedding = Some(vec![1.0, 0.0]);
        storage.set_node(future).expect("future source stores");

        let query_embedding = [1.0, 0.0];
        let routed = route_subject_raw_sources(
            &storage,
            &RecallPlan::infer("What projects has Alice built?"),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &scope_b,
        )
        .expect("subject-raw boundary route");
        assert!(routed.is_empty());
    }

    #[test]
    fn direct_atomic_routing_rejects_disjoint_concrete_scope() {
        let (mut storage, source_ids, fact_ids) = atomic_chain_fixture(&["scoped evidence"]);
        let scope_a = ScopePath::new("workspace/a").expect("scope a");
        let scope_b = ScopePath::new("workspace/b").expect("scope b");
        let mut source = storage
            .get_node(source_ids[0])
            .expect("scoped source exists")
            .clone();
        source.origin.scope = scope_a.clone();
        storage.set_node(source).expect("scoped source stores");
        let mut fact = storage
            .get_atomic_fact(fact_ids[0])
            .expect("scoped fact exists")
            .clone();
        fact.scope = scope_a;
        storage
            .delete_atomic_fact(fact_ids[0])
            .expect("old scoped fact binding deletes");
        storage.set_atomic_fact(fact).expect("scoped fact stores");

        let query_embedding = [1.0];
        let plan = RecallPlan::infer("Why is the scoped evidence relevant?");
        let disjoint = route_atomic_fact_sources(
            &storage,
            &plan,
            &[query_embedding.as_slice()],
            Timestamp(5),
            &scope_b,
        )
        .expect("disjoint-scope route");
        assert!(disjoint.is_empty());
        let universal = route_atomic_fact_sources(
            &storage,
            &plan,
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("universal-scope route");
        assert_eq!(universal.len(), 1);
    }

    #[test]
    fn direct_atomic_routing_revalidates_reused_source_identity() {
        let (mut storage, source_ids, fact_ids) = atomic_chain_fixture(&["original evidence"]);
        let stale_source_id = source_ids[0];
        let replacement = storage
            .get_node(stale_source_id)
            .expect("original source exists")
            .clone();
        storage
            .delete_node(stale_source_id)
            .expect("original source deletes");
        let replacement_id = storage.next_node_id();
        assert_eq!(replacement_id, stale_source_id);
        storage
            .set_node(replacement)
            .expect("replacement source stores");
        let fact = storage
            .get_atomic_fact(fact_ids[0])
            .expect("fact survives raw-source deletion")
            .clone();
        storage
            .set_atomic_fact(fact)
            .expect("idempotent fact update preserves source incarnation");

        let query_embedding = [1.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer("Why is the original evidence relevant?"),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("source-identity-safe route");
        assert!(routed.is_empty());
    }

    #[test]
    fn duplicate_direct_atomic_source_revalidates_each_fact_identity() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let source_id = storage.next_node_id();
        let mut source = fixture_node(
            source_id,
            KnowledgeType::Episodic,
            "the records are related".to_owned(),
            "current-session".to_owned(),
        );
        source.embedding = Some(vec![1.0, 0.0]);
        storage.set_node(source).expect("source stores");

        let valid_fact_id = storage.next_atomic_fact_id().expect("valid fact id");
        storage
            .set_atomic_fact(AtomicFact {
                id: valid_fact_id,
                content: "the records are related".to_owned(),
                embedding: vec![1.0, 0.0],
                source_node_ids: vec![source_id],
                entity_tags: Vec::new(),
                source_session_id: "current-session".to_owned(),
                scope: ScopePath::universal(),
                observed_at: Timestamp(1),
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            })
            .expect("valid fact stores");
        let stale_fact_id = storage.next_atomic_fact_id().expect("stale fact id");
        storage
            .set_atomic_fact(AtomicFact {
                id: stale_fact_id,
                content: "the records are related".to_owned(),
                embedding: vec![0.5, 0.5],
                source_node_ids: vec![source_id],
                entity_tags: Vec::new(),
                source_session_id: "deleted-session".to_owned(),
                scope: ScopePath::universal(),
                observed_at: Timestamp(1),
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            })
            .expect("stale fact stores");

        let query_embedding = [1.0, 0.0];
        let routed = route_atomic_fact_sources(
            &storage,
            &RecallPlan::infer("Why are the records related?"),
            &[query_embedding.as_slice()],
            Timestamp(5),
            &ScopePath::universal(),
        )
        .expect("identity-safe duplicate route");
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].fact_ids, vec![valid_fact_id]);
    }

    #[test]
    fn reviewed_atomic_relations_route_one_and_two_hop_raw_sources() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut source_ids = Vec::new();
        let mut fact_ids = Vec::new();
        for (session, content) in [
            ("seed-session", "the launch was delayed"),
            ("reason-session", "a supplier revised the contract"),
            (
                "cause-session",
                "a port closure moved the supplier schedule",
            ),
        ] {
            let source_id = storage.next_node_id();
            let mut source = fixture_node(
                source_id,
                KnowledgeType::Episodic,
                content.to_owned(),
                session.to_owned(),
            );
            source.embedding = Some(vec![1.0, 0.0]);
            storage.set_node(source).expect("raw source");
            let fact_id = storage.next_atomic_fact_id().expect("fact id");
            seed_legacy_atomic_fact(&mut storage, fact_id, source_id);
            source_ids.push(source_id);
            fact_ids.push(fact_id);
        }
        let reason_relation_id = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[1],
            fact_ids[0],
            AtomicFactRelationKind::Reason,
            "reason-link",
        );
        let causal_relation_id = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[2],
            fact_ids[1],
            AtomicFactRelationKind::Causal,
            "cause-link",
        );

        let plan = RecallPlan::infer("How are the launch delay and its causes related?");
        assert!(matches!(
            plan.answer_shape,
            AnswerShape::Relationship | AnswerShape::Inference
        ));
        let query_embedding = vec![1.0, 0.0];
        let expansion = expand_atomic_fact_relation_sources(
            &storage,
            &plan,
            &[direct_atomic_route(source_ids[0], fact_ids[0])],
            &[query_embedding.as_slice()],
            Timestamp(10),
            &ScopePath::universal(),
        )
        .expect("bounded chain route");
        assert_eq!(expansion.paths.len(), 2);
        assert_eq!(
            expansion.paths[0],
            AtomicRelationPath {
                fact_ids: vec![fact_ids[0], fact_ids[1]],
                hops: vec![AtomicRelationHop {
                    relation_id: reason_relation_id,
                    from_fact_id: fact_ids[1],
                    to_fact_id: fact_ids[0],
                    kind: AtomicFactRelationKind::Reason,
                }],
                source_groups: vec![vec![source_ids[0]], vec![source_ids[1]]],
            },
            "an inbound traversal must preserve the relation's canonical orientation"
        );
        assert_eq!(
            expansion.paths[1].hops[1],
            AtomicRelationHop {
                relation_id: causal_relation_id,
                from_fact_id: fact_ids[2],
                to_fact_id: fact_ids[1],
                kind: AtomicFactRelationKind::Causal,
            }
        );
        let routed = expansion.sources;
        let diagnostics = expansion.diagnostics;

        assert_eq!(
            routed
                .iter()
                .map(|source| source.candidate.node_id)
                .collect::<Vec<_>>(),
            [source_ids[1], source_ids[2]]
        );
        assert!(matches!(
            routed[0].origin,
            AtomicRouteOrigin::Chain { depth: 1 }
        ));
        assert!(matches!(
            routed[1].origin,
            AtomicRouteOrigin::Chain { depth: 2 }
        ));
        assert_eq!(diagnostics.visited_relations, 2);
        assert_eq!(diagnostics.expanded_facts, 2);
        assert_eq!(diagnostics.routed_sources, 2);
        assert_eq!(diagnostics.contradictions_excluded, 0);
        assert!(!diagnostics.truncated);
    }

    #[test]
    fn relation_identity_survives_when_both_endpoints_are_direct_candidates() {
        let (mut storage, source_ids, fact_ids) =
            atomic_chain_fixture(&["the outcome", "the reason"]);
        let relation_id = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[1],
            fact_ids[0],
            AtomicFactRelationKind::Reason,
            "direct-endpoint-link",
        );
        let query_embedding = [1.0, 0.0];
        let expansion = expand_atomic_fact_relation_sources(
            &storage,
            &RecallPlan::infer("Why did the outcome happen?"),
            &[
                direct_atomic_route(source_ids[0], fact_ids[0]),
                direct_atomic_route(source_ids[1], fact_ids[1]),
            ],
            &[query_embedding.as_slice()],
            Timestamp(10),
            &ScopePath::universal(),
        )
        .expect("direct endpoint relation expansion");

        assert!(expansion.sources.is_empty());
        assert_eq!(expansion.paths.len(), 1);
        assert_eq!(
            expansion.paths[0].hops,
            vec![AtomicRelationHop {
                relation_id,
                from_fact_id: fact_ids[1],
                to_fact_id: fact_ids[0],
                kind: AtomicFactRelationKind::Reason,
            }]
        );
        assert_eq!(expansion.diagnostics.visited_relations, 1);
        assert_eq!(expansion.diagnostics.expanded_facts, 0);
    }

    #[test]
    fn atomic_relation_path_codec_preserves_inbound_orientation_and_rejects_malformed_rows() {
        let path = AtomicRelationPath {
            fact_ids: vec![AtomicFactId(20), AtomicFactId(10)],
            hops: vec![AtomicRelationHop {
                relation_id: AtomicFactRelationId(7),
                from_fact_id: AtomicFactId(10),
                to_fact_id: AtomicFactId(20),
                kind: AtomicFactRelationKind::Reason,
            }],
            source_groups: vec![vec![NodeId(200)], vec![NodeId(100), NodeId(101)]],
        };
        let encoded = encode_atomic_relation_paths(std::slice::from_ref(&path))
            .expect("well-formed path encodes");
        assert_eq!(parse_atomic_relation_paths(&[encoded]), vec![path]);

        assert!(parse_atomic_relation_paths(&[ATOMIC_CHAIN_TRACE_PREFIX.to_owned()]).is_empty());
        assert!(
            parse_atomic_relation_paths(&[format!(
                "{ATOMIC_CHAIN_TRACE_PREFIX}20.10/7.10.20.x/200,100"
            )])
            .is_empty()
        );
        assert!(
            parse_atomic_relation_paths(&[format!(
                "{ATOMIC_CHAIN_TRACE_PREFIX}20.10/7.10.20.r/200,100.101.102"
            )])
            .is_empty(),
            "source groups over the production bound must fail closed"
        );
    }

    #[test]
    fn atomic_relation_paths_are_revalidated_at_repackage_time() {
        let (mut storage, source_ids, fact_ids) =
            atomic_chain_fixture(&["the outcome", "the reviewed reason"]);
        let relation_id = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[1],
            fact_ids[0],
            AtomicFactRelationKind::Reason,
            "revalidation-link",
        );
        let query_embedding = [1.0, 0.0];
        let expansion = expand_atomic_fact_relation_sources(
            &storage,
            &RecallPlan::infer("Why did the outcome happen?"),
            &[direct_atomic_route(source_ids[0], fact_ids[0])],
            &[query_embedding.as_slice()],
            Timestamp(10),
            &ScopePath::universal(),
        )
        .expect("initial relation expansion");
        let encoded = encode_atomic_relation_paths(&expansion.paths).expect("path trace");
        assert_eq!(
            validated_atomic_relation_paths(
                &storage,
                std::slice::from_ref(&encoded),
                Timestamp(10),
                &ScopePath::universal(),
            )
            .expect("live path validation")
            .len(),
            1
        );

        let mut relation = storage
            .get_atomic_fact_relation(relation_id)
            .expect("stored relation")
            .clone();
        relation.reviewed_at = Timestamp(11);
        storage
            .set_atomic_fact_relation(relation)
            .expect("future review stores");
        assert!(
            validated_atomic_relation_paths(
                &storage,
                &[encoded],
                Timestamp(10),
                &ScopePath::universal(),
            )
            .expect("stale path validation")
            .is_empty(),
            "a trace cannot carry a relation backward across its review time"
        );
    }

    #[test]
    fn atomic_chain_selection_admits_a_complete_group_or_keeps_the_baseline() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut ranking = Vec::new();
        for index in 0..8_u64 {
            let node_id = storage.next_node_id();
            storage
                .set_node(fixture_node(
                    node_id,
                    KnowledgeType::Episodic,
                    format!("chain evidence {index}"),
                    format!("chain-selection-session-{index}"),
                ))
                .expect("chain selection source");
            ranking.push(RerankedCandidate {
                node_id,
                score: 8.0 - index as f64,
            });
        }
        let path = AtomicRelationPath {
            fact_ids: vec![AtomicFactId(1), AtomicFactId(2), AtomicFactId(3)],
            hops: vec![
                AtomicRelationHop {
                    relation_id: AtomicFactRelationId(1),
                    from_fact_id: AtomicFactId(1),
                    to_fact_id: AtomicFactId(2),
                    kind: AtomicFactRelationKind::Supports,
                },
                AtomicRelationHop {
                    relation_id: AtomicFactRelationId(2),
                    from_fact_id: AtomicFactId(2),
                    to_fact_id: AtomicFactId(3),
                    kind: AtomicFactRelationKind::Causal,
                },
            ],
            source_groups: vec![
                vec![ranking[0].node_id],
                vec![ranking[6].node_id],
                vec![ranking[7].node_id],
            ],
        };
        let plan = RecallPlan::infer("What is the relationship between these events and causes?");
        let selected = compile_ranking_with_atomic_chains(
            &storage,
            &plan,
            &ranking,
            EvidenceSelection::Auto,
            6,
            &[],
            std::slice::from_ref(&path),
        )
        .expect("complete atomic chain selection");
        assert_eq!(selected.len(), 6);
        assert_eq!(selected[..4], ranking[..4], "the reranker head stays fixed");
        assert!(selected.contains(&ranking[6]));
        assert!(selected.contains(&ranking[7]));

        let impossible = AtomicRelationPath {
            source_groups: vec![
                vec![ranking[0].node_id],
                vec![ranking[5].node_id, ranking[6].node_id],
                vec![ranking[7].node_id],
            ],
            ..path
        };
        let unchanged = compile_ranking_with_atomic_chains(
            &storage,
            &plan,
            &ranking,
            EvidenceSelection::Auto,
            5,
            &[],
            &[impossible],
        )
        .expect("incomplete chain falls back");
        assert_eq!(unchanged, ranking[..5]);
    }

    #[test]
    fn atomic_chain_grouping_does_not_change_direct_or_temporal_selection() {
        let (storage, readout, _) = ranked_fixture();
        let ranking = readout
            .iter()
            .map(|candidate| RerankedCandidate {
                node_id: candidate.node_id,
                score: candidate.score,
            })
            .collect::<Vec<_>>();
        let path = AtomicRelationPath {
            fact_ids: vec![AtomicFactId(1), AtomicFactId(2)],
            hops: vec![AtomicRelationHop {
                relation_id: AtomicFactRelationId(1),
                from_fact_id: AtomicFactId(1),
                to_fact_id: AtomicFactId(2),
                kind: AtomicFactRelationKind::Supports,
            }],
            source_groups: vec![vec![ranking[20].node_id], vec![ranking[21].node_id]],
        };
        for plan in [
            RecallPlan::infer("Where does Alice live?"),
            RecallPlan::infer("When did Alice move?"),
        ] {
            let baseline =
                compile_ranking(&storage, &plan, &ranking, EvidenceSelection::Auto, 20, &[])
                    .expect("baseline selection");
            let with_chain = compile_ranking_with_atomic_chains(
                &storage,
                &plan,
                &ranking,
                EvidenceSelection::Auto,
                20,
                &[],
                std::slice::from_ref(&path),
            )
            .expect("non-relational chain selection");
            assert_eq!(with_chain, baseline);
        }
    }

    #[test]
    fn reviewed_atomic_relation_traversal_stops_before_a_third_hop() {
        let (mut storage, source_ids, fact_ids) = atomic_chain_fixture(&[
            "the launch was delayed",
            "a supplier revised the contract",
            "a port closure moved the supplier schedule",
            "a storm caused the port closure",
        ]);
        for index in 0..3 {
            seed_reviewed_atomic_relation(
                &mut storage,
                fact_ids[index],
                fact_ids[index + 1],
                AtomicFactRelationKind::Causal,
                &format!("depth-link-{index}"),
            );
        }

        let (routed, diagnostics) = expand_chain_from_seed(
            &storage,
            source_ids[0],
            fact_ids[0],
            Timestamp(10),
            &ScopePath::universal(),
        );
        assert_eq!(
            routed
                .iter()
                .map(|source| source.candidate.node_id)
                .collect::<Vec<_>>(),
            [source_ids[1], source_ids[2]]
        );
        assert!(
            routed
                .iter()
                .all(|source| source.candidate.node_id != source_ids[3]),
            "the depth-three endpoint must not enter the production evidence lane"
        );
        assert_eq!(diagnostics.visited_relations, 2);
        assert_eq!(diagnostics.expanded_facts, 2);
        assert_eq!(diagnostics.routed_sources, 2);
        assert!(!diagnostics.truncated);
    }

    #[test]
    fn reviewed_atomic_relation_cycles_terminate_without_duplicate_sources() {
        let (mut storage, source_ids, fact_ids) =
            atomic_chain_fixture(&["the rollout started", "the approval enabled the rollout"]);
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[1],
            AtomicFactRelationKind::Supports,
            "cycle-forward",
        );
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[1],
            fact_ids[0],
            AtomicFactRelationKind::Reason,
            "cycle-backward",
        );

        let (routed, diagnostics) = expand_chain_from_seed(
            &storage,
            source_ids[0],
            fact_ids[0],
            Timestamp(10),
            &ScopePath::universal(),
        );
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].candidate.node_id, source_ids[1]);
        assert_eq!(routed[0].fact_ids, [fact_ids[1]]);
        assert_eq!(diagnostics.visited_relations, 1);
        assert_eq!(diagnostics.expanded_facts, 1);
        assert_eq!(diagnostics.routed_sources, 1);
        assert!(!diagnostics.truncated);
    }

    #[test]
    fn reviewed_atomic_relation_traversal_rejects_future_records() {
        let (mut storage, source_ids, fact_ids) = atomic_chain_fixture(&[
            "the seed event",
            "the future-reviewed endpoint",
            "the future-observed fact",
            "the future source",
        ]);

        let mut future_fact = storage
            .get_atomic_fact(fact_ids[2])
            .expect("future fact exists")
            .clone();
        future_fact.observed_at = Timestamp(11);
        storage
            .set_atomic_fact(future_fact)
            .expect("future observation stores");

        let mut future_source = storage
            .get_node(source_ids[3])
            .expect("future source exists")
            .clone();
        future_source.created_at = Timestamp(11);
        future_source.updated_at = Timestamp(11);
        storage
            .set_node(future_source)
            .expect("future source stores");

        let future_review_relation = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[1],
            AtomicFactRelationKind::Supports,
            "future-review",
        );
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[2],
            AtomicFactRelationKind::Supports,
            "future-fact",
        );
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[3],
            AtomicFactRelationKind::Supports,
            "future-source",
        );
        let mut relation = storage
            .get_atomic_fact_relation(future_review_relation)
            .expect("future-reviewed relation exists")
            .clone();
        relation.reviewed_at = Timestamp(11);
        storage
            .set_atomic_fact_relation(relation)
            .expect("future review stores");

        let (routed, diagnostics) = expand_chain_from_seed(
            &storage,
            source_ids[0],
            fact_ids[0],
            Timestamp(10),
            &ScopePath::universal(),
        );
        assert!(routed.is_empty());
        assert_eq!(diagnostics.visited_relations, 0);
        assert_eq!(diagnostics.expanded_facts, 0);
        assert_eq!(diagnostics.routed_sources, 0);
    }

    #[test]
    fn reviewed_atomic_relation_validity_is_half_open_at_every_layer() {
        let (mut storage, source_ids, fact_ids) = atomic_chain_fixture(&[
            "the seed event",
            "the relation-expired endpoint",
            "the fact-expired endpoint",
            "the source-expired endpoint",
            "the lower-bound-active endpoint",
        ]);

        let mut expired_fact = storage
            .get_atomic_fact(fact_ids[2])
            .expect("expiring fact exists")
            .clone();
        expired_fact.valid_from = Some(Timestamp(1));
        expired_fact.valid_until = Some(Timestamp(10));
        storage
            .set_atomic_fact(expired_fact)
            .expect("expiring fact stores");

        let mut expired_source = storage
            .get_node(source_ids[3])
            .expect("expiring source exists")
            .clone();
        expired_source.valid_from = Some(Timestamp(1));
        expired_source.valid_until = Some(Timestamp(10));
        storage
            .set_node(expired_source)
            .expect("expiring source stores");
        let expired_source_fact = storage
            .get_atomic_fact(fact_ids[3])
            .expect("source-expiring fact exists")
            .clone();
        storage
            .delete_atomic_fact(fact_ids[3])
            .expect("old source-expiring binding deletes");
        storage
            .set_atomic_fact(expired_source_fact)
            .expect("source-expiring fact rebinds");

        let mut active_fact = storage
            .get_atomic_fact(fact_ids[4])
            .expect("active fact exists")
            .clone();
        active_fact.valid_from = Some(Timestamp(10));
        active_fact.valid_until = Some(Timestamp(11));
        storage
            .set_atomic_fact(active_fact)
            .expect("lower-bound fact stores");
        let mut active_source = storage
            .get_node(source_ids[4])
            .expect("active source exists")
            .clone();
        active_source.valid_from = Some(Timestamp(10));
        active_source.valid_until = Some(Timestamp(11));
        storage
            .set_node(active_source)
            .expect("lower-bound source stores");
        let active_fact = storage
            .get_atomic_fact(fact_ids[4])
            .expect("lower-bound fact exists")
            .clone();
        storage
            .delete_atomic_fact(fact_ids[4])
            .expect("old lower-bound binding deletes");
        storage
            .set_atomic_fact(active_fact)
            .expect("lower-bound fact rebinds");

        let expired_relation_id = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[1],
            AtomicFactRelationKind::Supports,
            "expired-relation",
        );
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[2],
            AtomicFactRelationKind::Supports,
            "expired-fact",
        );
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[3],
            AtomicFactRelationKind::Supports,
            "expired-source",
        );
        let active_relation_id = seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[4],
            AtomicFactRelationKind::Supports,
            "active-lower-bound",
        );
        let mut expired_relation = storage
            .get_atomic_fact_relation(expired_relation_id)
            .expect("expiring relation exists")
            .clone();
        expired_relation.valid_from = Some(Timestamp(1));
        expired_relation.valid_until = Some(Timestamp(10));
        storage
            .set_atomic_fact_relation(expired_relation)
            .expect("expiring relation stores");
        let mut active_relation = storage
            .get_atomic_fact_relation(active_relation_id)
            .expect("active relation exists")
            .clone();
        active_relation.valid_from = Some(Timestamp(10));
        active_relation.valid_until = Some(Timestamp(11));
        storage
            .set_atomic_fact_relation(active_relation)
            .expect("lower-bound relation stores");

        let (routed, diagnostics) = expand_chain_from_seed(
            &storage,
            source_ids[0],
            fact_ids[0],
            Timestamp(10),
            &ScopePath::universal(),
        );
        assert_eq!(
            routed
                .iter()
                .map(|source| source.candidate.node_id)
                .collect::<Vec<_>>(),
            [source_ids[4]],
            "valid_until is exclusive while valid_from is inclusive"
        );
        assert_eq!(diagnostics.visited_relations, 1);
        assert_eq!(diagnostics.expanded_facts, 1);
        assert_eq!(diagnostics.routed_sources, 1);
    }

    #[test]
    fn reviewed_atomic_relation_traversal_rejects_disjoint_concrete_scopes() {
        let (mut storage, source_ids, fact_ids) =
            atomic_chain_fixture(&["the scoped seed", "the other scoped endpoint"]);
        let scope_a = ScopePath::new("workspace/a").expect("scope a");
        let scope_b = ScopePath::new("workspace/b").expect("scope b");

        for (source_id, fact_id, scope) in [
            (source_ids[0], fact_ids[0], scope_a.clone()),
            (source_ids[1], fact_ids[1], scope_b),
        ] {
            let mut source = storage
                .get_node(source_id)
                .expect("scoped source exists")
                .clone();
            source.origin.scope = scope.clone();
            storage.set_node(source).expect("scoped source stores");
            let mut fact = storage
                .get_atomic_fact(fact_id)
                .expect("scoped fact exists")
                .clone();
            fact.scope = scope;
            storage.set_atomic_fact(fact).expect("scoped fact stores");
        }
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[1],
            AtomicFactRelationKind::Supports,
            "cross-scope-link",
        );

        let (routed, diagnostics) = expand_chain_from_seed(
            &storage,
            source_ids[0],
            fact_ids[0],
            Timestamp(10),
            &scope_a,
        );
        assert!(routed.is_empty());
        assert_eq!(diagnostics.visited_relations, 0);
        assert_eq!(diagnostics.expanded_facts, 0);
        assert_eq!(diagnostics.routed_sources, 0);
    }

    #[test]
    fn relation_absence_leaves_direct_atomic_routing_unchanged() {
        let (storage, source_ids, fact_ids) = atomic_chain_fixture(&["the direct seed event"]);
        let plan = RecallPlan::infer("How are the direct seed event and its causes related?");
        let query_embedding = [1.0];
        let direct = route_atomic_fact_sources(
            &storage,
            &plan,
            &[query_embedding.as_slice()],
            Timestamp(10),
            &ScopePath::universal(),
        )
        .expect("ordinary atomic routing");
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].candidate.node_id, source_ids[0]);
        assert_eq!(direct[0].fact_ids, [fact_ids[0]]);
        assert_eq!(direct[0].origin, AtomicRouteOrigin::Direct);

        let expansion = expand_atomic_fact_relation_sources(
            &storage,
            &plan,
            &direct,
            &[query_embedding.as_slice()],
            Timestamp(10),
            &ScopePath::universal(),
        )
        .expect("empty relation lane");
        let expanded = expansion.sources;
        let diagnostics = expansion.diagnostics;
        assert!(expanded.is_empty());
        assert_eq!(diagnostics.visited_relations, 0);
        assert_eq!(diagnostics.expanded_facts, 0);
        assert_eq!(diagnostics.routed_sources, 0);
        assert!(!diagnostics.truncated);
        assert_eq!(direct[0].candidate.node_id, source_ids[0]);
        assert_eq!(direct[0].fact_ids, [fact_ids[0]]);
        assert_eq!(direct[0].origin, AtomicRouteOrigin::Direct);
    }

    #[test]
    fn contradiction_is_never_an_atomic_relevance_bridge() {
        let mut storage = SqliteStorage::in_memory().expect("in-memory storage");
        let mut source_ids = Vec::new();
        let mut fact_ids = Vec::new();
        for (session, content) in [
            ("claim-session", "the rollout is approved"),
            ("counter-session", "the rollout is rejected"),
        ] {
            let source_id = storage.next_node_id();
            storage
                .set_node(fixture_node(
                    source_id,
                    KnowledgeType::Episodic,
                    content.to_owned(),
                    session.to_owned(),
                ))
                .expect("raw source");
            let fact_id = storage.next_atomic_fact_id().expect("fact id");
            seed_legacy_atomic_fact(&mut storage, fact_id, source_id);
            source_ids.push(source_id);
            fact_ids.push(fact_id);
        }
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[1],
            AtomicFactRelationKind::Contradicts,
            "constraint-link",
        );
        let query_embedding = vec![1.0];
        let expansion = expand_atomic_fact_relation_sources(
            &storage,
            &RecallPlan::infer("What is the relationship between the rollout decisions?"),
            &[direct_atomic_route(source_ids[0], fact_ids[0])],
            &[query_embedding.as_slice()],
            Timestamp(10),
            &ScopePath::universal(),
        )
        .expect("constraint-safe route");
        let routed = expansion.sources;
        let diagnostics = expansion.diagnostics;

        assert!(routed.is_empty());
        assert_eq!(diagnostics.visited_relations, 0);
        assert_eq!(diagnostics.contradictions_excluded, 1);
        assert_eq!(diagnostics.expanded_facts, 0);
    }

    #[test]
    fn recent_live_relation_is_not_starved_by_large_stale_adjacency() {
        let (mut storage, source_ids, fact_ids) =
            atomic_chain_fixture(&["the seed event", "the reviewed explanation"]);
        for index in 0..160 {
            seed_reviewed_atomic_relation(
                &mut storage,
                fact_ids[0],
                fact_ids[1],
                AtomicFactRelationKind::Contradicts,
                &format!("older-constraint-{index}"),
            );
        }
        seed_reviewed_atomic_relation(
            &mut storage,
            fact_ids[0],
            fact_ids[1],
            AtomicFactRelationKind::Supports,
            "current-positive-link",
        );

        let (routed, diagnostics) = expand_chain_from_seed(
            &storage,
            source_ids[0],
            fact_ids[0],
            Timestamp(10),
            &ScopePath::universal(),
        );
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].candidate.node_id, source_ids[1]);
        assert_eq!(diagnostics.visited_relations, 1);
        assert!(diagnostics.contradictions_excluded > 0);
        assert!(diagnostics.truncated);
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
