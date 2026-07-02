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
fn test_error_import() {
    use anamnesis::Error;
    let _ = Error::NodeNotFound;
}

#[test]
fn test_memory_import() {
    use anamnesis::Memory;
    let _ = std::any::type_name::<Memory>();
}
