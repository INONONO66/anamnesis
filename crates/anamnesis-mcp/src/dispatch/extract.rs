use std::collections::HashSet;

use anamnesis::graph::Timestamp;
use sha2::{Digest, Sha256};

use crate::capture::META_TURN_KEY;
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
            Err(error) => return Response::internal(error),
        }
    };

    let result = scan_namespace(&handles, &profile_id, &profile, min_turns, max_turns);
    match result {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(text) => Response::ok(text),
            Err(error) => Response::internal(error),
        },
        Err(error) => Response::internal(error),
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
        Err(error) => return Response::internal(error),
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
        Err(error) => Response::internal(error),
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
        Err(error) => return Response::internal(error),
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
        Err(error) => Response::internal(error),
    }
}

pub(super) fn dispatch_audit_list(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    limit: Option<u32>,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return Response::internal(error),
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
        enrich_audit_sources(&memory, &mut audit);
        serde_json::to_string(&audit)
            .map_err(|error| anamnesis::Error::InvalidInput(error.to_string()))
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
        Err(error) => return Response::internal(error),
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
        let audit = match store.list_extraction_audit(u32::MAX) {
            Ok(audit) => audit,
            Err(error) => return Response::internal(error),
        };
        let Some(candidate) = audit
            .candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
        else {
            return Response::invalid_params("extraction audit candidate was not found");
        };
        if !candidate_sources_available(&memory, candidate) {
            return Response::invalid_params(
                "extraction audit candidate sources are unavailable or mismatched",
            );
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

pub(super) fn dispatch_update_relation_audit(
    registry: &std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    namespace: Option<String>,
    relation_id: u64,
    verdict: RelationVerdict,
    reviewer: String,
) -> Response {
    let handles = match resolve_handles(registry, namespace.as_deref()) {
        Ok(handles) => handles,
        Err(error) => return Response::internal(error),
    };
    let reviewer = resolve_reviewer(Some(&reviewer));
    let result =
        MemoryRegistry::policy_store(&handles.policy).and_then(|mut policy| match &mut *policy {
            PolicyStoreState::Ready(store) => store.update_extraction_relation_audit(
                relation_id,
                verdict,
                &reviewer,
                Timestamp::now().0,
            ),
            PolicyStoreState::Uninitialized { .. } | PolicyStoreState::Disabled { .. } => {
                Err(anamnesis::Error::StorageError(
                    "policy store was not ready after initialization".to_owned(),
                ))
            }
        });
    match result {
        Ok(()) => Response::ok("updated extraction relation audit"),
        Err(error) => Response::internal(error),
    }
}

fn enrich_audit_sources(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    result: &mut ExtractionAuditResult,
) {
    for candidate in &mut result.candidates {
        candidate.sources = candidate_audit_sources(memory, candidate);
    }
}

fn candidate_sources_available(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    candidate: &ExtractionAuditCandidateRow,
) -> bool {
    let sources = candidate_audit_sources(memory, candidate);
    sources.len() == candidate.source_turn_keys.len()
        && sources
            .iter()
            .all(|source| source.availability == ExtractionAuditSourceAvailability::Available)
}

fn candidate_audit_sources(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    candidate: &ExtractionAuditCandidateRow,
) -> Vec<ExtractionAuditSource> {
    let source_count = candidate.source_turn_keys.len();
    if candidate.source_node_ids.len() != source_count
        || candidate.source_content_hashes.len() != source_count
    {
        return Vec::new();
    }

    candidate
        .source_node_ids
        .iter()
        .zip(&candidate.source_turn_keys)
        .zip(&candidate.source_content_hashes)
        .map(|((&node_id, turn_key), content_hash)| {
            resolve_audit_source(
                memory,
                node_id,
                turn_key,
                &candidate.source_session_id,
                &candidate.source_scope,
                content_hash,
            )
        })
        .collect()
}

fn resolve_audit_source(
    memory: &anamnesis::Memory<anamnesis::storage::SqliteStorage>,
    node_id: u64,
    turn_key: &str,
    session_id: &str,
    scope: &str,
    content_hash: &str,
) -> ExtractionAuditSource {
    let unavailable = || ExtractionAuditSource {
        node_id,
        turn_key: turn_key.to_owned(),
        session_id: session_id.to_owned(),
        scope: scope.to_owned(),
        content_hash: content_hash.to_owned(),
        content: None,
        availability: ExtractionAuditSourceAvailability::SourceUnavailable,
    };
    let graph = memory.engine().graph();
    let Ok(node) = graph.get_node(anamnesis::graph::NodeId(node_id)) else {
        return unavailable();
    };

    let authoritative = graph.all_node_ids().into_iter().any(|id| {
        id.0 == node_id
            && graph
                .get_node(id)
                .ok()
                .and_then(|candidate| candidate.metadata.get(META_TURN_KEY))
                .is_some_and(|candidate_key| candidate_key == turn_key)
    });
    let live_hash = format!("{:x}", Sha256::digest(node.content.as_bytes()));
    let exact = authoritative
        && node.origin.session_id == session_id
        && node.origin.scope.as_str() == scope
        && live_hash == content_hash;
    ExtractionAuditSource {
        node_id,
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
    validate_stage_snapshot(&memory, &sources)?;
    let canonical = reconstruct_and_validate(&sources, profile_id, &extraction)?;
    if canonical != extraction {
        return Err(anamnesis::Error::InvalidInput(
            "extraction payload does not match its canonical validation".to_owned(),
        ));
    }

    let mut policy = MemoryRegistry::policy_store(&handles.policy)?;
    match &mut *policy {
        PolicyStoreState::Ready(store) => {
            store.stage_extraction(profile_id, profile, llm_duration_ms, &sources, &canonical)
        }
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
) -> Result<(), anamnesis::Error> {
    let mut source_ids = HashSet::with_capacity(sources.len());
    let graph = memory.engine().graph();
    for source in sources {
        if !source_ids.insert(source.node_id) {
            return Err(anamnesis::Error::InvalidInput(
                "extraction sources must not reuse a node id".to_owned(),
            ));
        }
        let node = graph.get_node(anamnesis::graph::NodeId(source.node_id))?;
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
    }
    Ok(())
}

fn reconstruct_and_validate(
    sources: &[ExtractionSource],
    profile_id: &str,
    extraction: &ValidatedExtraction,
) -> Result<ValidatedExtraction, anamnesis::Error> {
    let items: Vec<_> = extraction
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "item_local_id": item.item_local_id,
                "content": item.content,
                "kind": item.kind,
                "confidence": item.confidence,
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
    validate::validate_output(&payload, sources, profile_id)
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
    let sources = capture_sources(&memory);

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
                ExtractionProfileStatus::Approved => Err(anamnesis::Error::InvalidInput(
                    "approved extraction profiles cannot be used for shadow scans".to_owned(),
                )),
                ExtractionProfileStatus::Revoked => Err(anamnesis::Error::InvalidInput(
                    "revoked extraction profile cannot be used".to_owned(),
                )),
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
) -> Vec<ExtractionSource> {
    let graph = memory.engine().graph();
    graph
        .all_node_ids()
        .into_iter()
        .filter_map(|node_id| {
            let node = graph.get_node(node_id).ok()?;
            let turn_key = node.metadata.get(META_TURN_KEY)?.clone();
            Some(ExtractionSource {
                node_id: node_id.0,
                turn_key,
                session_id: node.origin.session_id.clone(),
                scope: node.origin.scope.as_str().to_owned(),
                content: node.content.clone(),
                content_hash: format!("{:x}", Sha256::digest(node.content.as_bytes())),
                at_ms: node.created_at.0,
            })
        })
        .collect()
}
