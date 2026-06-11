#[path = "../benches/eval_common/locomo_pipeline.rs"]
mod locomo_pipeline;

use serde_json::json;

use locomo_pipeline::{
    answer_needles, contains_any_needle, load_locomo, normalize_for_match, parse_locomo_samples,
};

#[test]
fn parse_locomo_preserves_speakers_and_numeric_session_order() {
    let samples = vec![json!({
        "session_10": [
            {"speaker": "Alice", "text": "Late session turn"}
        ],
        "session_2": [
            {"speaker": "Bob", "text": "Earlier numeric session turn"}
        ],
        "qa": []
    })];

    let loaded = parse_locomo_samples(&samples, Some(1)).expect("valid synthetic LoCoMo data");

    assert_eq!(loaded.sessions[0].session_id, "locomo-0-session_2");
    assert_eq!(loaded.sessions[1].session_id, "locomo-0-session_10");
    assert_eq!(loaded.sessions[0].turns[0].speaker, "Bob");
    assert_eq!(loaded.sessions[1].turns[0].speaker, "Alice");
    assert!(loaded.speakers.contains("Alice"));
    assert!(loaded.speakers.contains("Bob"));
}

#[test]
fn answer_needles_extract_mixed_json_answers() {
    let value = json!({
        "date": "7 May 2023",
        "year": 2022,
        "topics": ["Adoption agencies", {"field": "Counseling certification"}],
        "empty": ""
    });

    let needles = answer_needles(&value);

    assert!(needles.contains(&"7 may 2023".to_string()));
    assert!(needles.contains(&"2022".to_string()));
    assert!(needles.contains(&"adoption agencies".to_string()));
    assert!(needles.contains(&"counseling certification".to_string()));
    assert!(!needles.iter().any(String::is_empty));
}

#[test]
fn normalize_for_match_casefolds_and_collapses_whitespace() {
    assert_eq!(
        normalize_for_match("  Adoption\n\tAgencies  "),
        "adoption agencies"
    );
}

#[test]
fn load_locomo_reads_dataset_path_and_needle_matching_checks_all_text() {
    let root = std::env::temp_dir().join(format!(
        "anamnesis-locomo-pipeline-test-{}",
        std::process::id()
    ));
    let locomo_dir = root.join("locomo");
    std::fs::create_dir_all(&locomo_dir).expect("create synthetic dataset dir");
    std::fs::write(
        locomo_dir.join("locomo10.json"),
        json!([{
            "session_1": [
                {"speaker": "Alice", "text": "Alice researched adoption agencies."}
            ],
            "qa": [{
                "question": "What did Alice research?",
                "answer": "Adoption agencies",
                "category": 1
            }]
        }])
        .to_string(),
    )
    .expect("write synthetic dataset");

    let loaded = load_locomo(&root, Some(1)).expect("load synthetic dataset");
    let needles = &loaded.questions[0].answer_needles;

    assert!(contains_any_needle(
        &loaded.sessions[0].turns[0].text,
        needles
    ));

    std::fs::remove_dir_all(root).expect("cleanup synthetic dataset");
}
