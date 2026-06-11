use anamnesis::query::{PackagingMode, SearchInput, SearchResult, SearchTrace};

#[test]
fn search_input_can_be_constructed() {
    let input = SearchInput {
        text: "auth".into(),
        ..Default::default()
    };
    assert_eq!(input.text, "auth");
}

#[test]
fn search_input_default() {
    let input = SearchInput::default();
    assert_eq!(input.limit, 10);
    assert!(input.text.is_empty());
    assert!(input.entity_tags.is_empty());
    assert!(input.seed_limit.is_none());
}

#[test]
fn packaging_mode_variants() {
    let modes = [
        PackagingMode::Balanced,
        PackagingMode::KnowledgeOnly,
        PackagingMode::KnowledgeWithProvenance,
        PackagingMode::PersonaWeighted,
        PackagingMode::Timeline,
    ];
    assert_eq!(modes.len(), 5);
}

#[test]
fn search_trace_default() {
    let trace = SearchTrace::default();
    assert!(trace.strategies_used.is_empty());
    assert_eq!(trace.seed_count, 0);
    assert_eq!(trace.iterations, 0);
    assert_eq!(trace.excluded_edge_count, 0);
    assert!(!trace.truncated);
    assert!(trace.packaging_mode.is_none());
    // The query-local readout energy decomposition defaults to all-zero terms
    // (E = 0) for an empty trace; it is never stored.
    assert_eq!(trace.energy.field_alignment, 0.0);
    assert_eq!(trace.energy.conductive_support, 0.0);
    assert_eq!(trace.energy.impedance_regularization, 0.0);
    assert_eq!(trace.energy.frustration_penalty, 0.0);
    assert_eq!(trace.energy.total(), 0.0);
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
