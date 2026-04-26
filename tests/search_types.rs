use anamnesis::graph::Timestamp;
use anamnesis::query::{PackagingMode, SearchInput, SearchPlan, SearchResult, SearchTrace};

#[test]
fn search_input_can_be_constructed() {
    let input = SearchInput {
        text: "auth".into(),
        agent_id: None,
        project_id: None,
        now: Timestamp(0),
        query_embedding: None,
        limit: 10,
        context: None,
    };
    assert_eq!(input.text, "auth");
}

#[test]
fn search_input_default() {
    let input = SearchInput::default();
    assert_eq!(input.limit, 10);
    assert!(input.text.is_empty());
}

#[test]
fn packaging_mode_variants() {
    let modes = [
        PackagingMode::KnowledgeOnly,
        PackagingMode::KnowledgeWithProvenance,
        PackagingMode::PersonaWeighted,
        PackagingMode::Timeline,
    ];
    assert_eq!(modes.len(), 4);
}

#[test]
fn search_trace_default() {
    let trace = SearchTrace::default();
    assert!(trace.strategies_used.is_empty());
    assert_eq!(trace.seed_count, 0);
    assert_eq!(trace.spread_iterations, 0);
    assert!(trace.packaging_mode.is_none());
}

#[test]
fn search_plan_construction() {
    let plan = SearchPlan {
        use_text: true,
        use_vector: false,
        use_graph: true,
        use_temporal: false,
        use_persona_bias: true,
        packaging_mode: PackagingMode::KnowledgeOnly,
    };
    assert!(plan.use_text);
    assert!(!plan.use_vector);
}

#[test]
fn search_result_construction() {
    use anamnesis::ContextPackage;
    let result = SearchResult {
        package: ContextPackage::empty(),
        trace: SearchTrace::default(),
    };
    assert_eq!(result.package.total_fragments(), 0);
}
