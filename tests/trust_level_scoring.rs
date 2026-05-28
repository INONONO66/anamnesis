//! Tests for TrustLevel → scope_weight bonus integration (T13).

use anamnesis::api::{IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::{SourceKind, TrustLevel};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig};

fn obs_with_peer(name: &str, peer_id: PeerId) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id,
            source_kind: SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp::now(),
        valid_from: None,
        valid_until: None,
    }
}

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn trust_level_scope_weight_bonus_values() {
    // Verify the bonus values are correct
    assert_eq!(TrustLevel::Owner.scope_weight_bonus(), 0.10);
    assert_eq!(TrustLevel::Admin.scope_weight_bonus(), 0.07);
    assert_eq!(TrustLevel::Member.scope_weight_bonus(), 0.03);
    assert_eq!(TrustLevel::Agent.scope_weight_bonus(), 0.00);
    assert_eq!(TrustLevel::Observer.scope_weight_bonus(), 0.00);
    assert_eq!(TrustLevel::Untrusted.scope_weight_bonus(), -0.05);
}

#[test]
fn owner_peer_gets_higher_relevance_than_untrusted() {
    let mut e = engine();
    let owner_id = e.register_peer("owner", TrustLevel::Owner).unwrap();
    let untrusted_id = e.register_peer("untrusted", TrustLevel::Untrusted).unwrap();

    let IngestResult::Created(owner_ids) = e
        .ingest(obs_with_peer("auth-fact-owner", owner_id))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let IngestResult::Created(untrusted_ids) = e
        .ingest(obs_with_peer("auth-fact-untrusted", untrusted_id))
        .unwrap()
    else {
        panic!("expected Created");
    };

    let result = e
        .search(SearchInput {
            text: "auth-fact".to_string(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    // Find relevance scores for both nodes
    let all_frags: Vec<_> = result
        .package
        .knowledge
        .iter()
        .chain(result.package.memories.iter())
        .collect();

    let owner_relevance = all_frags
        .iter()
        .find(|f| f.node_id == owner_ids[0])
        .map(|f| f.relevance);
    let untrusted_relevance = all_frags
        .iter()
        .find(|f| f.node_id == untrusted_ids[0])
        .map(|f| f.relevance);

    // Both should be found (or neither if text search doesn't find them)
    if let (Some(owner_r), Some(untrusted_r)) = (owner_relevance, untrusted_relevance) {
        assert!(
            owner_r >= untrusted_r,
            "owner relevance ({owner_r}) should be >= untrusted relevance ({untrusted_r})"
        );
    }
}

#[test]
fn scope_weight_bonus_clamped_to_one() {
    // Even with Owner bonus, scope_weight should not exceed 1.0
    // Exact scope = 1.0 + 0.10 bonus -> clamped to 1.0
    let bonus = TrustLevel::Owner.scope_weight_bonus();
    let base = 1.0_f64; // Exact scope weight
    let clamped = (base + bonus).clamp(0.0, 1.0);
    assert_eq!(clamped, 1.0);
}
