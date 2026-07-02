//! Tests for ingest_conversation() convenience method (T16).

use anamnesis::Engine;
use anamnesis::api::{ConversationInput, ExtractedFact};
use anamnesis::engine::SourceKind;
use anamnesis::engine::{EngineConfig, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, ScopePath, Timestamp};

fn default_origin() -> Origin {
    Origin {
        peer_id: PeerId(0),
        source_kind: SourceKind::AgentObservation,
        session_id: "s1".to_string(),
        scope: ScopePath::universal(),
        confidence: 0.9,
    }
}

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn ingest_conversation_creates_episode_and_facts() {
    let mut e = engine();
    let result = e
        .ingest_conversation(ConversationInput {
            name: "session-1".to_string(),
            summary: None,
            raw_text: "Alice said: auth uses factory pattern".to_string(),
            extracted_facts: vec![ExtractedFact {
                name: "auth uses factory pattern".to_string(),
                summary: None,
                content: "The auth module uses factory pattern".to_string(),
                embedding: None,
                confidence: Some(0.9),
                entity_tags: vec!["auth".to_string()],
            }],
            confidence: Some(0.8),
            entity_tags: vec![],
            origin: default_origin(),
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    assert!(result.episode_id.0 < u64::MAX);
    assert_eq!(result.extracted_ids.len(), 1);
}

#[test]
fn ingest_conversation_links_fact_to_episode() {
    let mut e = engine();
    let result = e
        .ingest_conversation(ConversationInput {
            name: "session-2".to_string(),
            summary: None,
            raw_text: "Raw conversation text".to_string(),
            extracted_facts: vec![ExtractedFact {
                name: "extracted fact".to_string(),
                summary: None,
                content: "A fact extracted from the conversation".to_string(),
                embedding: None,
                confidence: None,
                entity_tags: vec![],
            }],
            confidence: None,
            entity_tags: vec![],
            origin: default_origin(),
            timestamp: None,
        })
        .unwrap();
    // Verify ExtractedFrom edge: fact -> episode
    let storage = e.graph().storage();
    let has_extracted_from = storage
        .edges_from(result.extracted_ids[0])
        .iter()
        .any(|&eid| {
            storage.get_edge(eid).is_ok_and(|edge| {
                edge.target == result.episode_id && edge.edge_type == EdgeType::ExtractedFrom
            })
        });
    assert!(has_extracted_from, "ExtractedFrom edge should exist");
}
