use std::collections::HashSet;

use anamnesis::graph::Timestamp;

use crate::capture::META_TURN_KEY;
use crate::extract::{
    profile, scan,
    types::{ExtractionScanResult, ExtractionSource, ExtractorProfileComponents},
};
use crate::memory::{ExtractionProfileStatus, MemoryRegistry, NamespaceHandles, PolicyStoreState};
use crate::proto::Response;

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
                content_hash: String::new(),
                at_ms: node.created_at.0,
            })
        })
        .collect()
}
