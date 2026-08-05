use std::collections::{HashMap, HashSet};

use anamnesis::engine::StorageAdapter;
use anamnesis::graph::{KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::memory::{AtomicFactInput, AtomicFactRelationInput};
use anamnesis::storage::{AtomicFact, AtomicFactId, AtomicFactRelationId, AtomicFactRelationKind};
use sha2::{Digest, Sha256};

use crate::capture::{META_CAPTURE, META_TURN_KEY};
use crate::extract::{
    audit::{
        ExtractionAuditCandidateRow, ExtractionAuditResult, ExtractionAuditSource,
        ExtractionAuditSourceAvailability, resolve_reviewer,
    },
    profile, scan,
    types::{
        AuditSupport, ContaminationCategory, ExtractionScanResult, ExtractionSource,
        ExtractorProfileComponents, RelationVerdict, ValidatedExtraction,
    },
    validate,
};
use crate::memory::{ExtractionProfileStatus, MemoryRegistry, NamespaceHandles, PolicyStoreState};
use crate::proto::{ExtractionErrorKind, Response, StageExtractionResult};

pub(super) fn dispatch_scan(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    profile: ExtractorProfileComponents,
    min_turns: u32,
    max_turns: u32,
) -> Response {
    let profile_id = match profile::profile_id(&profile) {
        Ok(profile_id) => profile_id,
        Err(error) => return Response::internal(error),
    };

    // Phase 1: resolve both namespace handles under the global lock, then drop
    // it before opening the policy store or inspecting the graph.
    let handles = {
        let mut registry = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match registry.namespace_handles(namespace.as_deref()) {
            Ok(handles) => handles,
            Err(error) => return extraction_error_response(error),
        }
    };

    let result = scan_namespace(&handles, &profile_id, &profile, min_turns, max_turns);
    match result {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(text) => Response::ok(text),
            Err(error) => Response::internal(error),
        },
        Err(error) => extraction_error_response(error),
    }
}
pub(super) fn dispatch_stage(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    profile: ExtractorProfileComponents,
    llm_duration_ms: u64,
    sources: Vec<ExtractionSource>,
    extraction: ValidatedExtraction,
) -> Response {
    let profile_id = match profile::profile_id(&profile) {
        Ok(profile_id) => profile_id,
        Err(error) => return Response::internal(error),
    };
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };

    let result = stage_namespace(
        &handles,
        &profile_id,
        &profile,
        llm_duration_ms,
        sources,
        extraction,
    );
    match result {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(text) => Response::ok(text),
            Err(error) => Response::internal(error),
        },
        Err(error) => extraction_error_response(error),
    }
}

pub(super) fn dispatch_record_failure(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    profile: ExtractorProfileComponents,
    turn_count: u32,
    llm_invoked: bool,
    error_kind: ExtractionErrorKind,
    duration_ms: u64,
) -> Response {
    let profile_id = match profile::profile_id(&profile) {
        Ok(profile_id) => profile_id,
        Err(error) => return Response::internal(error),
    };
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };

    let result =
        MemoryRegistry::policy_store(&handles.policy).and_then(|mut policy| match &mut *policy {
            PolicyStoreState::Ready(store) => store.record_extraction_failure(
                &profile_id,
                turn_count,
                llm_invoked,
                error_kind,
                duration_ms,
            ),
            PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Disabled { .. } => {
                Err(anamnesis::Error::StorageError(
                    "policy store was not ready after initialization".to_owned(),
                ))
            }
        });
    match result {
        Ok(_) => Response::ok("recorded extraction failure"),
        Err(error) => extraction_error_response(error),
    }
}

pub(super) fn dispatch_audit_list(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    limit: Option<u32>,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };
    let result = {
        let memory = handles
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut audit =
            match MemoryRegistry::policy_store(&handles.policy).and_then(|policy| match &*policy {
                PolicyStoreState::Ready(store) => store.list_extraction_audit(limit.unwrap_or(100)),
                PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Disabled { .. } => {
                    Err(anamnesis::Error::StorageError(
                        "policy store was not ready after initialization".to_owned(),
                    ))
                }
            }) {
                Ok(audit) => audit,
                Err(error) => return Response::internal(error),
            };
        match enrich_audit_sources(&memory, &mut audit) {
            Ok(()) => serde_json::to_string(&audit)
                .map_err(|error| anamnesis::Error::InvalidInput(error.to_string())),
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(text) => Response::ok(text),
        Err(error) => Response::internal(error),
    }
}

pub(super) fn dispatch_update_candidate_audit(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    candidate_id: u64,
    support: AuditSupport,
    contamination: Option<ContaminationCategory>,
    reviewer: String,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };
    let reviewer = resolve_reviewer(Some(&reviewer));
    let result = {
        let memory = handles
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut policy = match MemoryRegistry::policy_store(&handles.policy) {
            Ok(policy) => policy,
            Err(error) => return Response::internal(error),
        };
        let PolicyStoreState::Ready(store) = &mut *policy else {
            return Response::internal("policy store was not ready after initialization");
        };
        let candidate = match store.extraction_audit_candidate(candidate_id) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => {
                return Response::invalid_params("extraction audit candidate was not found");
            }
            Err(error) => return Response::internal(error),
        };
        match candidate_sources_available(&memory, &candidate) {
            Ok(true) => {}
            Ok(false) => {
                return Response::invalid_params(
                    "extraction audit candidate sources are unavailable or mismatched",
                );
            }
            Err(error) => return Response::internal(error),
        }
        store.update_extraction_candidate_audit(
            candidate_id,
            support,
            contamination,
            &reviewer,
            Timestamp::now().0,
        )
    };
    match result {
        Ok(()) => Response::ok("updated extraction candidate audit"),
        Err(error) => Response::internal(error),
    }
}

pub(super) fn dispatch_promote_candidate(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    candidate_id: u64,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };
    let result = {
        let mut memory = handles
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut policy = match MemoryRegistry::policy_store(&handles.policy) {
            Ok(policy) => policy,
            Err(error) => return Response::internal(error),
        };
        let PolicyStoreState::Ready(store) = &mut *policy else {
            return Response::internal("policy store was not ready after initialization");
        };
        let mut candidate = match store.extraction_audit_candidate(candidate_id) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => {
                return Response::invalid_params("extraction audit candidate was not found");
            }
            Err(error) => return Response::internal(error),
        };
        if candidate.support != Some(AuditSupport::Supported)
            || candidate.contamination.is_some()
            || candidate.reviewed_at.is_none()
            || candidate
                .reviewed_by
                .as_deref()
                .is_none_or(|reviewer| reviewer.trim().is_empty())
        {
            return Response::invalid_params(
                "candidate promotion requires a reviewed supported verdict with no contamination",
            );
        }
        match candidate_sources_available(&memory, &candidate) {
            Ok(true) => {}
            Ok(false) => {
                return Response::invalid_params(
                    "extraction audit candidate sources are unavailable or mismatched",
                );
            }
            Err(error) => return Response::internal(error),
        }
        let sources = match candidate_audit_sources(&memory, &candidate, &mut None) {
            Ok(sources) => sources,
            Err(error) => return Response::internal(error),
        };
        candidate.evidence_span = live_evidence_span_from_sources(&candidate, &sources);
        promote_reviewed_candidate(&mut memory, &candidate).and_then(|promoted| {
            let flush_result = memory.engine_mut().graph_mut().storage_mut().flush();
            let promoted =
                finalize_candidate_flush(candidate.id, promoted, flush_result, |fact_id| {
                    memory.delete_atomic_fact(fact_id)
                })?;
            let commit_result =
                store.mark_extraction_candidate_committed(candidate.id, promoted.0.0);
            finalize_candidate_promotion(candidate.id, promoted, commit_result, |fact_id| {
                memory.delete_atomic_fact(fact_id)
            })
        })
    };
    match result {
        Ok((fact_id, already_materialized)) => match serde_json::to_string(&serde_json::json!({
            "atomic_fact_id": fact_id.0,
            "node_id": fact_id.0,
            "already_materialized": already_materialized,
        })) {
            Ok(text) => Response::ok(text),
            Err(error) => Response::internal(error),
        },
        Err(error) => Response::internal(error),
    }
}

pub(super) fn dispatch_promote_relation(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    relation_id: u64,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };
    let result = {
        let mut memory = handles
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut policy = match MemoryRegistry::policy_store(&handles.policy) {
            Ok(policy) => policy,
            Err(error) => return Response::internal(error),
        };
        let PolicyStoreState::Ready(store) = &mut *policy else {
            return Response::internal("policy store was not ready after initialization");
        };
        let relation = match store.extraction_audit_relation(relation_id) {
            Ok(Some(relation)) => relation,
            Ok(None) => {
                return Response::invalid_params("extraction audit relation was not found");
            }
            Err(error) => return Response::internal(error),
        };
        if relation.verdict != Some(crate::extract::types::RelationVerdict::Correct)
            || relation.reviewed_at.is_none()
            || relation
                .reviewed_by
                .as_deref()
                .is_none_or(|reviewer| reviewer.trim().is_empty())
        {
            return Response::invalid_params(
                "relation promotion requires a reviewed correct verdict",
            );
        }
        let from_candidate = match store.extraction_audit_candidate(relation.candidate_from) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => {
                return Response::invalid_params("relation source candidate was not found");
            }
            Err(error) => return Response::internal(error),
        };
        let to_candidate = match store.extraction_audit_candidate(relation.candidate_to) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => {
                return Response::invalid_params("relation target candidate was not found");
            }
            Err(error) => return Response::internal(error),
        };
        for candidate in [&from_candidate, &to_candidate] {
            if candidate.support != Some(AuditSupport::Supported)
                || candidate.contamination.is_some()
                || candidate.reviewed_at.is_none()
                || candidate
                    .reviewed_by
                    .as_deref()
                    .is_none_or(|reviewer| reviewer.trim().is_empty())
            {
                return Response::invalid_params(
                    "relation endpoints must remain reviewed, supported, and uncontaminated",
                );
            }
            match candidate_sources_available(&memory, candidate) {
                Ok(true) => {}
                Ok(false) => {
                    return Response::invalid_params(
                        "relation endpoint sources are unavailable or mismatched",
                    );
                }
                Err(error) => return Response::internal(error),
            }
        }
        let from_fact = match find_promoted_candidate(&memory, &from_candidate.idempotency_key) {
            Ok(Some(fact_id)) => fact_id,
            Ok(None) => {
                return Response::invalid_params("relation source candidate is not promoted");
            }
            Err(error) => return Response::internal(error),
        };
        if from_candidate.committed_node_id != Some(from_fact.0) {
            return Response::invalid_params(
                "relation source candidate commit does not match its promoted fact",
            );
        }
        let to_fact = match find_promoted_candidate(&memory, &to_candidate.idempotency_key) {
            Ok(Some(fact_id)) => fact_id,
            Ok(None) => {
                return Response::invalid_params("relation target candidate is not promoted");
            }
            Err(error) => return Response::internal(error),
        };
        if to_candidate.committed_node_id != Some(to_fact.0) {
            return Response::invalid_params(
                "relation target candidate commit does not match its promoted fact",
            );
        }
        promote_reviewed_relation(&mut memory, &relation, from_fact, to_fact).and_then(|promoted| {
            let flush_result = memory.engine_mut().graph_mut().storage_mut().flush();
            let promoted =
                finalize_relation_flush(relation.id, promoted, flush_result, |relation_id| {
                    memory.delete_atomic_fact_relation(relation_id)
                })?;
            let commit_result = store.mark_extraction_relation_committed(relation.id, promoted.0.0);
            finalize_relation_promotion(relation.id, promoted, commit_result, |relation_id| {
                memory.delete_atomic_fact_relation(relation_id)
            })
        })
    };
    match result {
        Ok((relation_id, already_materialized)) => match serde_json::to_string(&serde_json::json!({
            "atomic_relation_id": relation_id.0,
            "edge_id": relation_id.0,
            "already_materialized": already_materialized,
        })) {
            Ok(text) => Response::ok(text),
            Err(error) => Response::internal(error),
        },
        Err(error) => Response::internal(error),
    }
}

fn promote_reviewed_candidate(
    memory: &mut anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    candidate: &ExtractionAuditCandidateRow,
) -> Result<(AtomicFactId, bool), anamnesis::Error> {
    let existing = find_promoted_candidate(memory, &candidate.idempotency_key)?;
    let source_ids: Vec<_> = candidate
        .source_node_ids
        .iter()
        .copied()
        .map(NodeId)
        .collect();
    if source_ids.is_empty() {
        return Err(anamnesis::Error::InvalidInput(
            "reviewed extraction candidate requires at least one raw source".to_owned(),
        ));
    }

    let (fact_id, already_materialized) = match existing {
        Some(fact_id) => {
            let fact = memory.engine().graph().storage().get_atomic_fact(fact_id)?;
            if !reviewed_candidate_matches_fact(memory, candidate, &source_ids, fact)? {
                return Err(anamnesis::Error::InvalidInput(format!(
                    "extraction candidate idempotency key {:?} conflicts with the promoted fact",
                    candidate.idempotency_key
                )));
            }
            (fact_id, true)
        }
        None => {
            let fact_id = memory.add_atomic_fact(
                AtomicFactInput::new(&candidate.content, source_ids)
                    .with_embedding_surface(candidate_routing_surface(candidate))
                    .with_entity_tags(candidate.entity_tags.clone())
                    .with_validity(
                        candidate.valid_from_ms.map(Timestamp),
                        candidate.valid_until_ms.map(Timestamp),
                    )
                    .with_metadata(reviewed_candidate_metadata(candidate).into_iter().collect()),
            )?;
            (fact_id, false)
        }
    };

    Ok((fact_id, already_materialized))
}

const EXTRACTION_IDEMPOTENCY_META_KEY: &str = "anamnesis:extraction_idempotency_key";
const SOURCE_INCARNATION_META_PREFIX: &str = "anamnesis:source-incarnation:";

fn reviewed_candidate_metadata(candidate: &ExtractionAuditCandidateRow) -> HashMap<String, String> {
    let mut metadata = HashMap::from([
        (
            EXTRACTION_IDEMPOTENCY_META_KEY.to_owned(),
            candidate.idempotency_key.clone(),
        ),
        (
            "anamnesis:extraction_candidate_id".to_owned(),
            candidate.id.to_string(),
        ),
        (
            "anamnesis:extraction_profile_id".to_owned(),
            candidate.profile_id.clone(),
        ),
        (
            "anamnesis:source_session_id".to_owned(),
            candidate.source_session_id.clone(),
        ),
        (
            "anamnesis:fact-kind".to_owned(),
            candidate_kind_label(&candidate.kind).to_owned(),
        ),
    ]);
    for (key, value) in [
        ("anamnesis:ground-subject", candidate.subject.as_ref()),
        ("anamnesis:ground-relation", candidate.relation.as_ref()),
        ("anamnesis:ground-object", candidate.object.as_ref()),
        (
            "anamnesis:evidence-object",
            candidate.evidence_object.as_ref(),
        ),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(node_id) = candidate.evidence_source_node_id {
        metadata.insert(
            "anamnesis:evidence-source-node-id".to_owned(),
            node_id.to_string(),
        );
    }
    for (key, value) in [
        (
            "anamnesis:evidence-span-start",
            candidate.evidence_span_start,
        ),
        ("anamnesis:evidence-span-end", candidate.evidence_span_end),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), value.to_string());
        }
    }
    if let Some(value) = candidate.evidence_span_sha256.as_ref() {
        metadata.insert("anamnesis:evidence-span-sha256".to_owned(), value.clone());
    }
    metadata
}

fn reviewed_candidate_matches_fact(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    candidate: &ExtractionAuditCandidateRow,
    source_ids: &[NodeId],
    fact: &AtomicFact,
) -> Result<bool, anamnesis::Error> {
    let mut entity_tags = candidate
        .entity_tags
        .iter()
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .filter(|tag| {
            let normalized = tag.to_ascii_lowercase();
            !normalized.starts_with("speaker-")
                && !normalized.starts_with("session-")
                && normalized != "anamnesis:derived"
        })
        .collect::<Vec<_>>();
    entity_tags.sort();
    entity_tags.dedup();

    let mut observed_at = Timestamp(0);
    let storage = memory.engine().graph().storage();
    let mut source_incarnations_current = true;
    for source_id in source_ids {
        let source = storage.get_node(*source_id)?;
        observed_at = observed_at.max(source.created_at);
        source_incarnations_current &= storage.atomic_fact_source_is_current(fact, source)?;
    }

    let expected_metadata = reviewed_candidate_metadata(candidate);
    let persisted_metadata = fact
        .metadata
        .iter()
        .filter(|(key, _)| !key.starts_with(SOURCE_INCARNATION_META_PREFIX))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<HashMap<_, _>>();
    Ok(fact.content == candidate.content.trim()
        && fact.source_node_ids == source_ids
        && fact.entity_tags == entity_tags
        && fact.source_session_id == candidate.source_session_id
        && fact.scope.as_str() == candidate.source_scope
        && fact.observed_at == observed_at
        && fact.valid_from == candidate.valid_from_ms.map(Timestamp)
        && fact.valid_until == candidate.valid_until_ms.map(Timestamp)
        && persisted_metadata == expected_metadata
        && source_incarnations_current
        && !fact.embedding.is_empty()
        && fact.embedding.iter().all(|value| value.is_finite()))
}

fn finalize_candidate_flush<F>(
    audit_candidate_id: u64,
    promoted: (AtomicFactId, bool),
    flush_result: Result<(), anamnesis::Error>,
    compensate: F,
) -> Result<(AtomicFactId, bool), anamnesis::Error>
where
    F: FnOnce(AtomicFactId) -> Result<(), anamnesis::Error>,
{
    match flush_result {
        Ok(()) => Ok(promoted),
        Err(flush_error) if promoted.1 => Err(flush_error),
        Err(flush_error) => match compensate(promoted.0) {
            Ok(()) => Err(flush_error),
            Err(compensation_error) => Err(anamnesis::Error::StorageError(format!(
                "failed to flush promoted extraction candidate {audit_candidate_id}: \
                 {flush_error}; failed to delete newly created atomic fact {}: \
                 {compensation_error}",
                promoted.0.0
            ))),
        },
    }
}

fn finalize_candidate_promotion<F>(
    audit_candidate_id: u64,
    promoted: (AtomicFactId, bool),
    commit_result: Result<(), anamnesis::Error>,
    compensate: F,
) -> Result<(AtomicFactId, bool), anamnesis::Error>
where
    F: FnOnce(AtomicFactId) -> Result<(), anamnesis::Error>,
{
    match commit_result {
        Ok(()) => Ok(promoted),
        Err(commit_error) if promoted.1 => Err(commit_error),
        Err(commit_error) => match compensate(promoted.0) {
            Ok(()) => Err(commit_error),
            Err(compensation_error) => Err(anamnesis::Error::StorageError(format!(
                "failed to mark extraction candidate {audit_candidate_id} committed: \
                 {commit_error}; failed to delete newly created atomic fact {}: \
                 {compensation_error}",
                promoted.0.0
            ))),
        },
    }
}

fn candidate_routing_surface(candidate: &ExtractionAuditCandidateRow) -> String {
    match candidate
        .evidence_object
        .as_deref()
        .or(candidate.object.as_deref())
        .map(str::trim)
        .filter(|evidence_object| !evidence_object.is_empty())
    {
        Some(evidence_object) => {
            format!("{}\nEvidence object: {evidence_object}", candidate.content)
        }
        None => candidate.content.clone(),
    }
}

fn find_promoted_candidate(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    idempotency_key: &str,
) -> Result<Option<AtomicFactId>, anamnesis::Error> {
    let storage = memory.engine().graph().storage();
    storage
        .atomic_fact_by_metadata(EXTRACTION_IDEMPOTENCY_META_KEY, idempotency_key)
        .map(|fact| fact.map(|fact| fact.id))
}

fn promote_reviewed_relation(
    memory: &mut anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    relation: &crate::extract::audit::ExtractionAuditRelationRow,
    from: AtomicFactId,
    to: AtomicFactId,
) -> Result<(AtomicFactRelationId, bool), anamnesis::Error> {
    let reviewer = relation
        .reviewed_by
        .as_deref()
        .map(str::trim)
        .filter(|reviewer| !reviewer.is_empty())
        .ok_or_else(|| {
            anamnesis::Error::InvalidInput(
                "reviewed relation requires a non-empty reviewer".to_owned(),
            )
        })?;
    let reviewed_at = relation.reviewed_at.map(Timestamp).ok_or_else(|| {
        anamnesis::Error::InvalidInput("reviewed relation requires a review time".to_owned())
    })?;
    let already_materialized = find_promoted_relation(memory, &relation.idempotency_key)?.is_some();
    let relation_id = memory.add_atomic_fact_relation(
        AtomicFactRelationInput::new(
            from,
            to,
            relation_kind(&relation.relation_type),
            reviewer,
            &relation.profile_id,
            reviewed_at,
            &relation.idempotency_key,
        )
        .with_metadata(vec![(
            "anamnesis:extraction_relation_id".to_owned(),
            relation.id.to_string(),
        )]),
    )?;
    Ok((relation_id, already_materialized))
}

fn finalize_relation_flush<F>(
    audit_relation_id: u64,
    promoted: (AtomicFactRelationId, bool),
    flush_result: Result<(), anamnesis::Error>,
    compensate: F,
) -> Result<(AtomicFactRelationId, bool), anamnesis::Error>
where
    F: FnOnce(AtomicFactRelationId) -> Result<(), anamnesis::Error>,
{
    match flush_result {
        Ok(()) => Ok(promoted),
        Err(flush_error) if promoted.1 => Err(flush_error),
        Err(flush_error) => match compensate(promoted.0) {
            Ok(()) => Err(flush_error),
            Err(compensation_error) => Err(anamnesis::Error::StorageError(format!(
                "failed to flush promoted extraction relation {audit_relation_id}: \
                 {flush_error}; failed to delete newly created atomic fact relation {}: \
                 {compensation_error}",
                promoted.0.0
            ))),
        },
    }
}

fn finalize_relation_promotion<F>(
    audit_relation_id: u64,
    promoted: (AtomicFactRelationId, bool),
    commit_result: Result<(), anamnesis::Error>,
    compensate: F,
) -> Result<(AtomicFactRelationId, bool), anamnesis::Error>
where
    F: FnOnce(AtomicFactRelationId) -> Result<(), anamnesis::Error>,
{
    match commit_result {
        Ok(()) => Ok(promoted),
        Err(commit_error) if promoted.1 => Err(commit_error),
        Err(commit_error) => match compensate(promoted.0) {
            Ok(()) => Err(commit_error),
            Err(compensation_error) => Err(anamnesis::Error::StorageError(format!(
                "failed to mark extraction relation {audit_relation_id} committed: \
                 {commit_error}; failed to delete newly created atomic fact relation {}: \
                 {compensation_error}",
                promoted.0.0
            ))),
        },
    }
}

fn find_promoted_relation<'a>(
    memory: &'a anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    idempotency_key: &str,
) -> Result<Option<&'a anamnesis::storage::AtomicFactRelation>, anamnesis::Error> {
    memory
        .engine()
        .graph()
        .storage()
        .atomic_fact_relation_by_idempotency_key(idempotency_key)
}

fn relation_kind(kind: &crate::extract::types::RelationKind) -> AtomicFactRelationKind {
    use crate::extract::types::RelationKind;
    match kind {
        RelationKind::Reason => AtomicFactRelationKind::Reason,
        RelationKind::Causal => AtomicFactRelationKind::Causal,
        RelationKind::Contradicts => AtomicFactRelationKind::Contradicts,
        RelationKind::Supports => AtomicFactRelationKind::Supports,
    }
}

fn candidate_kind_label(kind: &crate::extract::types::CandidateKind) -> &'static str {
    use crate::extract::types::CandidateKind;
    match kind {
        CandidateKind::Fact => "fact",
        CandidateKind::Entity => "entity",
        CandidateKind::Event => "event",
        CandidateKind::Preference => "preference",
        CandidateKind::Decision => "decision",
        CandidateKind::Causal => "causal",
        CandidateKind::Lesson => "lesson",
        CandidateKind::Convention => "convention",
        CandidateKind::Gotcha => "gotcha",
    }
}

pub(super) fn dispatch_update_relation_audit(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    relation_id: u64,
    verdict: RelationVerdict,
    reviewer: String,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return extraction_error_response(error),
    };
    let reviewer = resolve_reviewer(Some(&reviewer));
    let result = {
        let memory = handles
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut policy = match MemoryRegistry::policy_store(&handles.policy) {
            Ok(policy) => policy,
            Err(error) => return Response::internal(error),
        };
        let PolicyStoreState::Ready(store) = &mut *policy else {
            return Response::internal("policy store was not ready after initialization");
        };
        let relation = match store.extraction_audit_relation(relation_id) {
            Ok(Some(relation)) => relation,
            Ok(None) => {
                return Response::invalid_params("extraction audit relation was not found");
            }
            Err(error) => return Response::internal(error),
        };
        for candidate_id in [relation.candidate_from, relation.candidate_to] {
            let candidate = match store.extraction_audit_candidate(candidate_id) {
                Ok(Some(candidate)) => candidate,
                Ok(None) => {
                    return Response::invalid_params(
                        "extraction audit relation endpoint was not found",
                    );
                }
                Err(error) => return Response::internal(error),
            };
            match candidate_sources_available(&memory, &candidate) {
                Ok(true) => {}
                Ok(false) => {
                    return Response::invalid_params(
                        "relation endpoint sources are unavailable or mismatched",
                    );
                }
                Err(error) => return Response::internal(error),
            }
        }
        store.update_extraction_relation_audit(relation_id, verdict, &reviewer, Timestamp::now().0)
    };
    match result {
        Ok(()) => Response::ok("updated extraction relation audit"),
        Err(error) => Response::internal(error),
    }
}

fn enrich_audit_sources(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    result: &mut ExtractionAuditResult,
) -> Result<(), anamnesis::Error> {
    let mut capture_turn_key_index = None;
    for candidate in &mut result.candidates {
        candidate.sources =
            candidate_audit_sources(memory, candidate, &mut capture_turn_key_index)?;
        candidate.evidence_span = live_evidence_span(candidate);
    }
    Ok(())
}

fn candidate_sources_available(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    candidate: &ExtractionAuditCandidateRow,
) -> Result<bool, anamnesis::Error> {
    let sources = candidate_audit_sources(memory, candidate, &mut None)?;
    let grounding_fields_present = [
        candidate.subject.is_some(),
        candidate.relation.is_some(),
        candidate.object.is_some(),
        candidate.evidence_source_node_id.is_some(),
        candidate.evidence_span_start.is_some(),
        candidate.evidence_span_end.is_some(),
        candidate.evidence_span_sha256.is_some(),
    ];
    let grounding_available = if grounding_fields_present.iter().any(|present| *present) {
        grounding_fields_present.iter().all(|present| *present)
            && live_evidence_span_from_sources(candidate, &sources).is_some()
    } else {
        true
    };
    Ok(grounding_available
        && !candidate.source_turn_keys.is_empty()
        && candidate.source_node_ids.len() == candidate.source_turn_keys.len()
        && candidate.source_content_hashes.len() == candidate.source_turn_keys.len()
        && candidate.source_incarnations.len() == candidate.source_turn_keys.len()
        && sources.len() == candidate.source_turn_keys.len()
        && sources
            .iter()
            .all(|source| source.availability == ExtractionAuditSourceAvailability::Available))
}

fn live_evidence_span(candidate: &ExtractionAuditCandidateRow) -> Option<String> {
    live_evidence_span_from_sources(candidate, &candidate.sources)
}

fn live_evidence_span_from_sources(
    candidate: &ExtractionAuditCandidateRow,
    sources: &[ExtractionAuditSource],
) -> Option<String> {
    let source_node_id = candidate.evidence_source_node_id?;
    let start = usize::try_from(candidate.evidence_span_start?).ok()?;
    let end = usize::try_from(candidate.evidence_span_end?).ok()?;
    let expected_hash = candidate.evidence_span_sha256.as_deref()?;
    let content = sources
        .iter()
        .find(|source| {
            source.node_id == source_node_id
                && source.availability == ExtractionAuditSourceAvailability::Available
        })?
        .content
        .as_deref()?;
    let span = content.get(start..end)?;
    let actual_hash = format!("{:x}", Sha256::digest(span.as_bytes()));
    (actual_hash == expected_hash).then(|| span.to_owned())
}

fn candidate_audit_sources(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    candidate: &ExtractionAuditCandidateRow,
    capture_turn_key_index: &mut Option<HashMap<String, Vec<anamnesis::graph::NodeId>>>,
) -> Result<Vec<ExtractionAuditSource>, anamnesis::Error> {
    let source_count = candidate.source_turn_keys.len();
    if source_count == 0
        || candidate.source_node_ids.len() != source_count
        || candidate.source_content_hashes.len() != source_count
    {
        return Ok(Vec::new());
    }

    candidate
        .source_node_ids
        .iter()
        .zip(&candidate.source_turn_keys)
        .zip(&candidate.source_content_hashes)
        .enumerate()
        .map(|(index, ((&node_id, turn_key), content_hash))| {
            resolve_audit_source(
                memory,
                capture_turn_key_index,
                AuditSourceBinding {
                    node_id,
                    turn_key,
                    session_id: &candidate.source_session_id,
                    scope: &candidate.source_scope,
                    content_hash,
                    expected_incarnation: candidate
                        .source_incarnations
                        .get(index)
                        .map(String::as_str),
                },
            )
        })
        .collect()
}

#[derive(Clone, Copy)]
struct AuditSourceBinding<'a> {
    node_id: u64,
    turn_key: &'a str,
    session_id: &'a str,
    scope: &'a str,
    content_hash: &'a str,
    expected_incarnation: Option<&'a str>,
}

fn resolve_audit_source(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    capture_turn_key_index: &mut Option<HashMap<String, Vec<anamnesis::graph::NodeId>>>,
    binding: AuditSourceBinding<'_>,
) -> Result<ExtractionAuditSource, anamnesis::Error> {
    let unavailable = || ExtractionAuditSource {
        node_id: binding.node_id,
        turn_key: binding.turn_key.to_owned(),
        session_id: binding.session_id.to_owned(),
        scope: binding.scope.to_owned(),
        content_hash: binding.content_hash.to_owned(),
        content: None,
        availability: ExtractionAuditSourceAvailability::SourceUnavailable,
    };
    let graph = memory.engine().graph();
    let hinted = graph
        .get_node(anamnesis::graph::NodeId(binding.node_id))
        .ok();
    let has_hint = hinted.is_some();
    if let Some(node) = hinted.filter(|node| {
        is_capture_node(node)
            && node
                .metadata
                .get(META_TURN_KEY)
                .is_some_and(|candidate_key| candidate_key == binding.turn_key)
    }) {
        return audit_source_from_live_node(
            graph.storage(),
            node,
            binding.turn_key,
            binding.session_id,
            binding.scope,
            binding.content_hash,
            binding.expected_incarnation,
        );
    }

    let capture_turn_key_index = capture_turn_key_index.get_or_insert_with(|| {
        graph
            .all_node_ids()
            .into_iter()
            .filter_map(|id| {
                let node = graph.get_node(id).ok()?;
                is_capture_node(node)
                    .then_some(())
                    .and_then(|()| node.metadata.get(META_TURN_KEY))
                    .map(|turn_key| (turn_key.clone(), id))
            })
            .fold(HashMap::new(), |mut index, (turn_key, id)| {
                index.entry(turn_key).or_default().push(id);
                index
            })
    });
    let matches = capture_turn_key_index.get(binding.turn_key);
    let resolved = matches
        .filter(|matches| matches.len() == 1)
        .and_then(|matches| graph.get_node(matches[0]).ok());
    let Some(node) = resolved else {
        return Ok(if !has_hint && matches.is_none_or(Vec::is_empty) {
            unavailable()
        } else {
            ExtractionAuditSource {
                availability: ExtractionAuditSourceAvailability::SourceMismatch,
                ..unavailable()
            }
        });
    };
    audit_source_from_live_node(
        graph.storage(),
        node,
        binding.turn_key,
        binding.session_id,
        binding.scope,
        binding.content_hash,
        binding.expected_incarnation,
    )
}

fn audit_source_from_live_node(
    storage: &anamnesis::storage::SqliteStorage,
    node: &Node,
    turn_key: &str,
    session_id: &str,
    scope: &str,
    content_hash: &str,
    expected_incarnation: Option<&str>,
) -> Result<ExtractionAuditSource, anamnesis::Error> {
    let live_hash = format!("{:x}", Sha256::digest(node.content.as_bytes()));
    let live_incarnation = storage.atomic_source_incarnation(node)?;
    let exact = node.origin.session_id == session_id
        && node.origin.scope.as_str() == scope
        && live_hash == content_hash
        && expected_incarnation == Some(live_incarnation.as_str());
    Ok(ExtractionAuditSource {
        node_id: node.id.0,
        turn_key: turn_key.to_owned(),
        session_id: session_id.to_owned(),
        scope: scope.to_owned(),
        content_hash: content_hash.to_owned(),
        content: exact.then(|| node.content.clone()),
        availability: if exact {
            ExtractionAuditSourceAvailability::Available
        } else {
            ExtractionAuditSourceAvailability::SourceMismatch
        },
    })
}
fn extraction_error_response(error: anamnesis::Error) -> Response {
    match error {
        anamnesis::Error::InvalidInput(message) => Response::invalid_params(message),
        error => Response::internal(error),
    }
}

fn resolve_handles(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<&str>,
) -> Result<NamespaceHandles, anamnesis::Error> {
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.namespace_handles(namespace)
}

fn stage_namespace(
    handles: &NamespaceHandles,
    profile_id: &str,
    profile: &ExtractorProfileComponents,
    llm_duration_ms: u64,
    sources: Vec<ExtractionSource>,
    extraction: ValidatedExtraction,
) -> Result<StageExtractionResult, anamnesis::Error> {
    let memory = handles
        .memory
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let source_incarnations = validate_stage_snapshot(&memory, &sources)?;
    let canonical =
        reconstruct_and_validate(&sources, profile_id, profile.schema_version, &extraction)?;
    if canonical != extraction {
        return Err(anamnesis::Error::InvalidInput(
            "extraction payload does not match its canonical validation".to_owned(),
        ));
    }

    let mut policy = MemoryRegistry::policy_store(&handles.policy)?;
    match &mut *policy {
        PolicyStoreState::Ready(store) => store.stage_extraction(
            profile_id,
            profile,
            llm_duration_ms,
            &sources,
            &source_incarnations,
            &canonical,
        ),
        PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Disabled { .. } => {
            Err(anamnesis::Error::StorageError(
                "policy store was not ready after initialization".to_owned(),
            ))
        }
    }
}

fn validate_stage_snapshot(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    sources: &[ExtractionSource],
) -> Result<HashMap<u64, String>, anamnesis::Error> {
    let mut source_incarnations = HashMap::with_capacity(sources.len());
    let graph = memory.engine().graph();
    for source in sources {
        if source_incarnations.contains_key(&source.node_id) {
            return Err(anamnesis::Error::InvalidInput(
                "extraction sources must not reuse a node id".to_owned(),
            ));
        }
        let node = graph.get_node(anamnesis::graph::NodeId(source.node_id))?;
        if !is_capture_node(node) {
            return Err(anamnesis::Error::InvalidInput(
                "extraction source node is not a captured episodic node".to_owned(),
            ));
        }
        let Some(turn_key) = node.metadata.get(META_TURN_KEY) else {
            return Err(anamnesis::Error::InvalidInput(
                "extraction source node has no turn key".to_owned(),
            ));
        };
        let authoritative = ExtractionSource {
            node_id: source.node_id,
            turn_key: turn_key.clone(),
            session_id: node.origin.session_id.clone(),
            scope: node.origin.scope.as_str().to_owned(),
            content: node.content.clone(),
            content_hash: format!("{:x}", Sha256::digest(node.content.as_bytes())),
            at_ms: node.created_at.0,
        };
        if &authoritative != source {
            return Err(anamnesis::Error::InvalidInput(
                "extraction source snapshot no longer matches memory".to_owned(),
            ));
        }
        source_incarnations.insert(
            source.node_id,
            graph.storage().atomic_source_incarnation(node)?,
        );
    }
    Ok(source_incarnations)
}

fn reconstruct_and_validate(
    sources: &[ExtractionSource],
    profile_id: &str,
    schema_version: u32,
    extraction: &ValidatedExtraction,
) -> Result<ValidatedExtraction, anamnesis::Error> {
    let items: Vec<_> = extraction
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "item_local_id": item.item_local_id,
                "content": item.content,
                "subject": item.subject,
                "relation": item.relation,
                "object": item.object,
                "evidence_object": item.evidence_object,
                "evidence_span": item.evidence_span,
                "kind": item.kind,
                "confidence": item.confidence,
                "entity_tags": item.entity_tags,
                "valid_from_ms": item.valid_from_ms,
                "valid_until_ms": item.valid_until_ms,
                "source_node_ids": item.sources.iter().map(|source| source.node_id).collect::<Vec<_>>(),
            })
        })
        .collect();
    let relations: Vec<_> = extraction
        .relations
        .iter()
        .map(|relation| {
            serde_json::json!({
                "from_item_local_id": relation.from_item_local_id,
                "to_item_local_id": relation.to_item_local_id,
                "relation_type": relation.relation_type,
            })
        })
        .collect();
    let payload =
        serde_json::to_vec(&serde_json::json!({ "items": items, "relations": relations }))
            .map_err(|error| anamnesis::Error::InvalidInput(error.to_string()))?;
    validate::validate_output_for_schema(&payload, sources, profile_id, schema_version)
        .map_err(|error| anamnesis::Error::InvalidInput(error.to_string()))
}

fn scan_namespace(
    handles: &NamespaceHandles,
    profile_id: &str,
    profile: &ExtractorProfileComponents,
    min_turns: u32,
    max_turns: u32,
) -> Result<ExtractionScanResult, anamnesis::Error> {
    // Policy initialization and queries occur while holding Memory, preserving
    // the namespace lock order. The global registry lock was dropped in phase 1.
    let memory = handles
        .memory
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let processed_turn_keys = processed_turn_keys(handles, profile_id, profile)?;
    let sources = capture_sources(&memory)?;

    scan::scan(sources, &processed_turn_keys, profile, min_turns, max_turns)
        .map_err(|error| anamnesis::Error::StorageError(error.to_string()))
}

fn processed_turn_keys(
    handles: &NamespaceHandles,
    profile_id: &str,
    profile: &ExtractorProfileComponents,
) -> Result<HashSet<String>, anamnesis::Error> {
    let policy = MemoryRegistry::policy_store(&handles.policy)?;
    match &*policy {
        PolicyStoreState::Ready(store) => {
            match store.ensure_extraction_shadow_profile(profile_id, profile, Timestamp::now().0)? {
                ExtractionProfileStatus::Shadow => store.processed_extraction_turn_keys(profile_id),
                ExtractionProfileStatus::Revoked => Err(anamnesis::Error::InvalidInput(
                    "revoked extraction profile cannot be used".to_owned(),
                )),
                status => Err(anamnesis::Error::InvalidInput(format!(
                    "unsupported extraction profile status for shadow scans: {status:?}"
                ))),
            }
        }
        PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Disabled { .. } => {
            Err(anamnesis::Error::StorageError(
                "policy store was not ready after initialization".to_owned(),
            ))
        }
    }
}

fn capture_sources(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
) -> Result<Vec<ExtractionSource>, anamnesis::Error> {
    let graph = memory.engine().graph();
    graph
        .all_node_ids()
        .into_iter()
        .map(|node_id| {
            let node = graph.get_node(node_id)?;
            if !is_capture_node(node) {
                return Ok(None);
            }
            let Some(turn_key) = node.metadata.get(META_TURN_KEY) else {
                return Ok(None);
            };
            Ok(Some(ExtractionSource {
                node_id: node_id.0,
                turn_key: turn_key.clone(),
                session_id: node.origin.session_id.clone(),
                scope: node.origin.scope.as_str().to_owned(),
                content: node.content.clone(),
                content_hash: format!("{:x}", Sha256::digest(node.content.as_bytes())),
                at_ms: node.created_at.0,
            }))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|sources| sources.into_iter().flatten().collect())
}
fn is_capture_node(node: &Node) -> bool {
    matches!(&node.node_type, KnowledgeType::Episodic)
        && node
            .metadata
            .get(META_CAPTURE)
            .is_some_and(|value| value == "true")
        && node.metadata.contains_key(META_TURN_KEY)
}

#[cfg(test)]
mod tests {
    use super::{
        finalize_candidate_flush, finalize_candidate_promotion, finalize_relation_flush,
        finalize_relation_promotion,
    };
    use anamnesis::storage::{AtomicFactId, AtomicFactRelationId};

    #[test]
    fn candidate_flush_failure_deletes_new_fact() {
        let mut deleted = None;
        let result = finalize_candidate_flush(
            7,
            (AtomicFactId(19), false),
            Err(anamnesis::Error::StorageError("flush failed".to_owned())),
            |fact_id| {
                deleted = Some(fact_id);
                Ok(())
            },
        )
        .expect_err("failed flush must fail promotion");

        assert_eq!(deleted, Some(AtomicFactId(19)));
        assert!(result.to_string().contains("flush failed"));
    }

    #[test]
    fn candidate_commit_failure_deletes_new_fact() {
        let mut deleted = None;
        let result = finalize_candidate_promotion(
            7,
            (AtomicFactId(19), false),
            Err(anamnesis::Error::StorageError(
                "policy commit conflict".to_owned(),
            )),
            |fact_id| {
                deleted = Some(fact_id);
                Ok(())
            },
        )
        .expect_err("failed policy commit must fail promotion");

        assert_eq!(deleted, Some(AtomicFactId(19)));
        assert!(result.to_string().contains("policy commit conflict"));
    }

    #[test]
    fn relation_flush_failure_deletes_new_relation() {
        let mut deleted = None;
        let result = finalize_relation_flush(
            11,
            (AtomicFactRelationId(23), false),
            Err(anamnesis::Error::StorageError("flush failed".to_owned())),
            |relation_id| {
                deleted = Some(relation_id);
                Ok(())
            },
        )
        .expect_err("failed flush must fail promotion");

        assert_eq!(deleted, Some(AtomicFactRelationId(23)));
        assert!(result.to_string().contains("flush failed"));
    }

    #[test]
    fn relation_promotion_reports_commit_and_compensation_failures() {
        let result = finalize_relation_promotion(
            11,
            (AtomicFactRelationId(23), false),
            Err(anamnesis::Error::StorageError(
                "policy commit conflict".to_owned(),
            )),
            |_| {
                Err(anamnesis::Error::StorageError(
                    "compensation delete failed".to_owned(),
                ))
            },
        )
        .expect_err("both failures must be returned");

        let message = result.to_string();
        assert!(message.contains("policy commit conflict"));
        assert!(message.contains("compensation delete failed"));
        assert!(message.contains("atomic fact relation 23"));
    }
}
