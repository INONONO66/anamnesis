use std::collections::HashSet;

use anamnesis::graph::Timestamp;
use sha2::{Digest, Sha256};

use crate::capture::META_TURN_KEY;
use crate::extract::{
    profile, scan,
    types::{
        ExtractionScanResult, ExtractionSource, ExtractorProfileComponents, ValidatedExtraction,
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
