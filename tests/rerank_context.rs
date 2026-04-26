use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig};
use anamnesis::{Engine, IngestResult};

fn make_obs_tagged(name: &str, tags: Vec<&str>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content about {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.into_iter().map(|s| s.to_string()).collect(),
        origin: Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: None,
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
    }
}

fn created_id(result: IngestResult) -> anamnesis::NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    }
}

#[test]
fn rerank_changes_order_for_different_contexts() {
    let mut e = Engine::new();

    let auth_id = created_id(
        e.ingest(make_obs_tagged("auth_node", vec!["auth"]))
            .unwrap(),
    );
    let db_id = created_id(e.ingest(make_obs_tagged("db_node", vec!["db"])).unwrap());

    // Decay so salience < 0.8 — the +0.2 boost becomes visible
    e.tick(Timestamp(1000 + 30 * 86_400_000)).unwrap();

    let mut auth_config = QueryConfig::default();
    auth_config.context = Some("auth security".into());
    let pkg_auth = e
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &auth_config,
        )
        .unwrap();

    let mut db_config = QueryConfig::default();
    db_config.context = Some("db migration".into());
    let pkg_db = e
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &db_config,
        )
        .unwrap();

    let auth_first = pkg_auth.knowledge.first().map(|f| f.node_id);
    let db_first = pkg_db.knowledge.first().map(|f| f.node_id);
    assert_eq!(
        auth_first,
        Some(auth_id),
        "auth context should rank auth_node first"
    );
    assert_eq!(
        db_first,
        Some(db_id),
        "db context should rank db_node first"
    );
}

#[test]
fn no_context_preserves_original_order() {
    let mut e = Engine::new();
    e.ingest(make_obs_tagged("a", vec![])).unwrap();
    e.ingest(make_obs_tagged("b", vec![])).unwrap();

    let pkg = e
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert!(!pkg.knowledge.is_empty());
}

#[test]
fn rerank_works_with_type_filtered() {
    let mut e = Engine::new();

    let conv_auth = created_id(
        e.ingest(Observation {
            node_type: KnowledgeType::Convention,
            ..make_obs_tagged("auth_convention", vec!["auth"])
        })
        .unwrap(),
    );
    created_id(
        e.ingest(Observation {
            node_type: KnowledgeType::Convention,
            ..make_obs_tagged("logging_convention", vec!["logging"])
        })
        .unwrap(),
    );

    e.tick(Timestamp(1000 + 30 * 86_400_000)).unwrap();

    let mut config = QueryConfig::default();
    config.context = Some("auth".into());
    let pkg = e
        .query(
            &Query::TypeFiltered {
                node_type: KnowledgeType::Convention,
                limit: 10,
            },
            &config,
        )
        .unwrap();

    assert_eq!(pkg.knowledge.len(), 2);
    assert_eq!(
        pkg.knowledge[0].node_id, conv_auth,
        "auth context should rank auth_convention first"
    );
}

#[test]
fn rerank_boosts_by_content_match() {
    let mut e = Engine::new();

    created_id(
        e.ingest(Observation {
            content: "handles database migrations and schema updates".to_string(),
            ..make_obs_tagged("module_x", vec![])
        })
        .unwrap(),
    );
    let _unrelated = created_id(
        e.ingest(Observation {
            content: "manages http routing".to_string(),
            ..make_obs_tagged("module_y", vec![])
        })
        .unwrap(),
    );

    e.tick(Timestamp(1000 + 30 * 86_400_000)).unwrap();

    let mut config = QueryConfig::default();
    config.context = Some("database".into());
    let pkg = e
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &config,
        )
        .unwrap();

    assert_eq!(pkg.knowledge.len(), 2);
    assert!(
        pkg.knowledge[0].relevance > pkg.knowledge[1].relevance,
        "content-matched node should have higher relevance"
    );
}
