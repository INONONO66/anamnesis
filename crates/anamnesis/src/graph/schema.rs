use super::{EdgeType, KnowledgeType};
pub use anamnesis_core::{EdgeKind, EntityKind, MemoryKind, NodeKind};

impl KnowledgeType {
    pub fn node_kind(&self) -> NodeKind {
        match self {
            KnowledgeType::Entity => NodeKind::Entity,
            KnowledgeType::Episodic => NodeKind::Fragment,
            _ => NodeKind::Memory,
        }
    }

    pub fn memory_kind(&self) -> Option<MemoryKind> {
        match self {
            KnowledgeType::IdentityCore
            | KnowledgeType::IdentityLearned
            | KnowledgeType::Semantic
            | KnowledgeType::Convention
            | KnowledgeType::Gotcha
            | KnowledgeType::Custom(_) => Some(MemoryKind::Fact),
            KnowledgeType::IdentityState => Some(MemoryKind::State),
            KnowledgeType::Procedural => Some(MemoryKind::Procedure),
            KnowledgeType::Decision => Some(MemoryKind::Decision),
            KnowledgeType::Hypothesis => Some(MemoryKind::Hypothesis),
            KnowledgeType::Evidence => Some(MemoryKind::Evidence),
            KnowledgeType::DebugSession => Some(MemoryKind::Problem),
            KnowledgeType::Event => Some(MemoryKind::Event),
            KnowledgeType::Entity | KnowledgeType::Episodic => None,
        }
    }
}

impl EdgeType {
    pub fn edge_kind(&self) -> EdgeKind {
        match self {
            EdgeType::Temporal => EdgeKind::Sequence,
            EdgeType::BelongsTo => EdgeKind::Contains,
            EdgeType::ExtractedFrom | EdgeType::ConsolidatedFrom => EdgeKind::Derives,
            EdgeType::Entity => EdgeKind::References,
            EdgeType::Contradicts | EdgeType::Supersedes | EdgeType::Refutes => EdgeKind::Contrasts,
            EdgeType::RejectedAlternative => EdgeKind::Resolves,
            EdgeType::Semantic
            | EdgeType::Causal
            | EdgeType::Reason
            | EdgeType::ReinforcedBy
            | EdgeType::Supports
            | EdgeType::Custom(_) => EdgeKind::Associates,
        }
    }

    pub fn relation_kind(&self) -> &str {
        match self {
            EdgeType::Semantic => "semantic",
            EdgeType::Causal => "causal",
            EdgeType::Temporal => "temporal",
            EdgeType::Reason => "reason",
            EdgeType::ReinforcedBy => "reinforced_by",
            EdgeType::ConsolidatedFrom => "consolidated_from",
            EdgeType::ExtractedFrom => "extracted_from",
            EdgeType::Entity => "entity",
            EdgeType::Supersedes => "supersedes",
            EdgeType::RejectedAlternative => "rejected_alternative",
            EdgeType::Supports => "supports",
            EdgeType::Refutes => "refutes",
            EdgeType::BelongsTo => "belongs_to",
            EdgeType::Contradicts => "contradicts",
            EdgeType::Custom(label) => label.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_knowledge_projects_to_minimal_node_kind() {
        assert_eq!(KnowledgeType::Entity.node_kind(), NodeKind::Entity);
        assert_eq!(KnowledgeType::Episodic.node_kind(), NodeKind::Fragment);
        assert_eq!(KnowledgeType::Decision.node_kind(), NodeKind::Memory);
    }

    #[test]
    fn legacy_knowledge_projects_to_memory_kind_when_applicable() {
        assert_eq!(
            KnowledgeType::IdentityState.memory_kind(),
            Some(MemoryKind::State)
        );
        assert_eq!(
            KnowledgeType::Procedural.memory_kind(),
            Some(MemoryKind::Procedure)
        );
        assert_eq!(KnowledgeType::Entity.memory_kind(), None);
        assert_eq!(KnowledgeType::Episodic.memory_kind(), None);
    }

    #[test]
    fn legacy_edges_project_to_minimal_edge_kind() {
        assert_eq!(EdgeType::Temporal.edge_kind(), EdgeKind::Sequence);
        assert_eq!(EdgeType::ExtractedFrom.edge_kind(), EdgeKind::Derives);
        assert_eq!(EdgeType::Entity.edge_kind(), EdgeKind::References);
        assert_eq!(EdgeType::Contradicts.edge_kind(), EdgeKind::Contrasts);
        assert_eq!(
            EdgeType::RejectedAlternative.edge_kind(),
            EdgeKind::Resolves
        );
    }

    #[test]
    fn legacy_edge_relation_kind_preserves_detail() {
        assert_eq!(EdgeType::Temporal.relation_kind(), "temporal");
        assert_eq!(
            EdgeType::Custom("reviewed".to_string()).relation_kind(),
            "reviewed"
        );
    }
}
