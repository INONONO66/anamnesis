use anamnesis::decompose_query;

#[test]
fn english_who_is_pattern() {
    let r = decompose_query("who is the CEO of Hashed?");
    assert!(!r.is_empty());
    assert!(r.iter().any(|s| s.contains("CEO") || s.contains("Hashed")));
}

#[test]
fn english_what_is_pattern() {
    let r = decompose_query("what is the factory pattern?");
    assert_eq!(r, vec!["the factory pattern"]);
}

#[test]
fn english_of_pattern() {
    let r = decompose_query("CEO of Hashed");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0], "CEO");
    assert_eq!(r[1], "Hashed");
}

#[test]
fn english_how_does_pattern() {
    let r = decompose_query("how does spreading activation work?");
    assert_eq!(r, vec!["spreading activation"]);
}

#[test]
fn korean_eui_pattern() {
    let r = decompose_query("Hashed의 CEO는 누구야?");
    assert!(r.iter().any(|s| s.contains("Hashed")));
    assert!(r.iter().any(|s| s.contains("CEO")));
}

#[test]
fn korean_nugu_pattern() {
    let r = decompose_query("Alice가 누구야?");
    assert_eq!(r, vec!["Alice"]);
}

#[test]
fn korean_mwo_pattern() {
    let r = decompose_query("팩토리 패턴은 뭐야?");
    assert_eq!(r, vec!["팩토리 패턴"]);
}

#[test]
fn no_match_returns_original() {
    let r = decompose_query("foo bar baz");
    assert_eq!(r, vec!["foo bar baz"]);
}

#[test]
fn case_insensitive_english() {
    let r = decompose_query("Who Is Alice?");
    assert_eq!(r, vec!["Alice"]);
}
