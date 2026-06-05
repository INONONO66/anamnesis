//! Test that all public types are accessible from crate root

#[test]
fn test_engine_import() {
    use anamnesis::Engine;
    let _engine = Engine::new();
}

#[test]
fn test_core_types_import() {
    use anamnesis::{Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, Origin, Timestamp};
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
fn test_query_types_import() {
    use anamnesis::{ContextPackage, Fragment, Query, QueryConfig, Tension, TokenBudget};
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
fn test_api_types_import() {
    use anamnesis::{
        EngineConfig, Observation, ReflectReport, SessionSummary, TickReport,
    };
    let _ = EngineConfig::default();
    let _ = TickReport::default();
    let _ = ReflectReport::default();
    let _ = std::any::type_name::<(Observation, SessionSummary)>();
}

#[test]
fn test_storage_import() {
    use anamnesis::{SqliteStorage, StorageAdapter};
    let _ = SqliteStorage::new().unwrap();
    let _ = std::any::type_name::<dyn StorageAdapter>();
}

#[test]
fn test_error_import() {
    use anamnesis::Error;
    let _ = Error::NodeNotFound;
}
