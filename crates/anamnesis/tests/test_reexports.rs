//! Test that the public API surface is accessible at the documented paths.
//! Root: Memory / Engine / Error.  Kernel types: anamnesis::engine::*.

#[test]
fn test_engine_import() {
    use anamnesis::Engine;
    let _engine = Engine::new();
}

#[test]
fn test_engine_namespace_types() {
    use anamnesis::engine::{
        Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, Origin, Timestamp,
    };
    let _ = (
        NodeId(0),
        EdgeId(0),
        Timestamp(0),
        KnowledgeType::Semantic,
        EdgeType::Semantic,
    );
    let _ = std::any::type_name::<(Node, Edge, Origin)>();
}

#[test]
fn test_engine_namespace_query_types() {
    use anamnesis::engine::{ContextPackage, Fragment, Query, QueryConfig, Tension, TokenBudget};
    let _ = QueryConfig::default();
    let _ = ContextPackage::empty();
    let _ = TokenBudget::default();
    let _ = Query::List {
        min_salience: 0.5,
        limit: 10,
    };
    let _ = std::any::type_name::<(Fragment, Tension)>();
}

#[test]
fn test_engine_namespace_api_types() {
    use anamnesis::engine::{EngineConfig, Observation, TickReport};
    let _ = EngineConfig::default();
    let _ = TickReport::default();
    let _ = std::any::type_name::<Observation>();
}

#[test]
fn test_engine_namespace_storage() {
    use anamnesis::engine::{SqliteStorage, StorageAdapter};
    let _ = SqliteStorage::new().unwrap();
    let _ = std::any::type_name::<dyn StorageAdapter>();
}

#[test]
fn test_external_atomic_fact_relation_construction() {
    use std::collections::HashMap;

    use anamnesis::engine::{ScopePath, Timestamp};
    use anamnesis::storage::{
        AtomicFactId, AtomicFactRelation, AtomicFactRelationId, AtomicFactRelationKind,
    };

    let relation = AtomicFactRelation::new(
        AtomicFactRelationId(7),
        AtomicFactId(2),
        AtomicFactId(3),
        AtomicFactRelationKind::Supports,
        "reviewer",
        "profile-v1",
        Timestamp(100),
        "relation-key",
        ScopePath::universal(),
    )
    .with_validity(Some(Timestamp(80)), Some(Timestamp(120)))
    .with_metadata(HashMap::from([("source".to_owned(), "adapter".to_owned())]));

    assert_eq!(relation.id, AtomicFactRelationId(7));
    assert_eq!(relation.from_fact_id, AtomicFactId(2));
    assert_eq!(relation.to_fact_id, AtomicFactId(3));
    assert_eq!(relation.reviewed_by, "reviewer");
    assert_eq!(relation.valid_from, Some(Timestamp(80)));
    assert_eq!(relation.metadata.get("source"), Some(&"adapter".to_owned()));
}

#[test]
fn test_error_import() {
    use anamnesis::Error;
    let _ = Error::NodeNotFound;
}

#[test]
fn test_memory_import() {
    use anamnesis::Memory;
    let _ = std::any::type_name::<Memory>();
}

#[test]
fn test_memory_structured_readout_types() {
    use anamnesis::memory::{FocusedEvidence, RecallReadout};

    let _ = std::any::type_name::<FocusedEvidence>();
    let _ = std::any::type_name::<RecallReadout>();
}
