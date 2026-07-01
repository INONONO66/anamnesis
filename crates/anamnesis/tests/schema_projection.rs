use anamnesis::graph::{EdgeType, KnowledgeType};
use anamnesis::schema::{EdgeKind, MemoryKind, NodeKind};

#[test]
fn legacy_types_project_into_core_schema_when_used_by_consumers() {
    let semantic = KnowledgeType::Semantic;
    let event = KnowledgeType::Event;
    let source_edge = EdgeType::ExtractedFrom;

    assert_eq!(semantic.node_kind(), NodeKind::Memory);
    assert_eq!(semantic.memory_kind(), Some(MemoryKind::Fact));
    assert_eq!(event.memory_kind(), Some(MemoryKind::Event));
    assert_eq!(source_edge.edge_kind(), EdgeKind::Derives);
}

#[test]
fn custom_edge_relation_keeps_consumer_label_when_projected() {
    let reviewed = EdgeType::Custom("reviewed".to_string());

    assert_eq!(reviewed.edge_kind(), EdgeKind::Associates);
    assert_eq!(reviewed.relation_kind(), "reviewed");
}
