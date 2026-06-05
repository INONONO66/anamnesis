//! Locked T16 `scope_weight` value table.
//!
//! - Exact: 1.0
//! - Universal: 0.95
//! - Ancestor: 0.85
//! - Descendant: 0.85
//! - Sibling: 0.50
//! - Unrelated: 0.05 base, shared-entity bonus capped at +0.20 (max 0.25)

use anamnesis::graph::ScopePath;
use anamnesis::query::scope_weight;

const EPS: f64 = 1e-10;

fn proj(s: &str) -> ScopePath {
    ScopePath::new(s).expect("valid scope")
}

// ===== 6 Relation -> Weight Cases =====

#[test]
fn exact_weight_is_one() {
    let w = scope_weight(&proj("personal/foo"), &proj("personal/foo"), 0);
    assert!((w - 1.0).abs() < EPS, "expected Exact 1.0, got {w}");
}

#[test]
fn universal_weight_is_zero_point_nine_five() {
    let w_query_universal = scope_weight(&ScopePath::universal(), &proj("personal"), 0);
    let w_node_universal = scope_weight(&proj("personal"), &ScopePath::universal(), 0);
    assert!(
        (w_query_universal - 0.95).abs() < EPS,
        "expected Universal 0.95 (universal query), got {w_query_universal}"
    );
    assert!(
        (w_node_universal - 0.95).abs() < EPS,
        "expected Universal 0.95 (universal node), got {w_node_universal}"
    );
}

#[test]
fn ancestor_weight_is_zero_point_eight_five() {
    let w = scope_weight(&proj("personal"), &proj("personal/foo"), 0);
    assert!((w - 0.85).abs() < EPS, "expected Ancestor 0.85, got {w}");
}

#[test]
fn descendant_weight_is_zero_point_eight_five() {
    let w = scope_weight(&proj("personal/foo"), &proj("personal"), 0);
    assert!((w - 0.85).abs() < EPS, "expected Descendant 0.85, got {w}");
}

#[test]
fn sibling_weight_is_zero_point_five() {
    let w = scope_weight(&proj("personal/foo"), &proj("personal/bar"), 0);
    assert!((w - 0.50).abs() < EPS, "expected Sibling 0.50, got {w}");
}

#[test]
fn unrelated_base_weight_is_zero_point_zero_five() {
    let w = scope_weight(&proj("work"), &proj("personal"), 0);
    assert!((w - 0.05).abs() < EPS, "expected Unrelated 0.05, got {w}");
}

// ===== Shared-Entity Bonus Cap =====

#[test]
fn entity_overlap_bonus_capped_at_020() {
    let q = proj("work");
    let n = proj("personal");

    let zero = scope_weight(&q, &n, 0);
    let one = scope_weight(&q, &n, 1);
    let two = scope_weight(&q, &n, 2);
    let many = scope_weight(&q, &n, 1000);

    assert!((zero - 0.05).abs() < EPS, "base must be 0.05, got {zero}");
    assert!(
        one > zero,
        "1 shared entity must increase weight above base 0.05, got {one}"
    );
    assert!(
        two >= one,
        "bonus must be monotonic non-decreasing: 1->{one}, 2->{two}"
    );
    assert!(
        many <= 0.05 + 0.20 + EPS,
        "weight must be capped at base + 0.20 = 0.25, got {many}"
    );
    assert!(
        many >= two - EPS,
        "1000 shared entities must not decrease below 2 entities; 2->{two}, 1000->{many}"
    );
    assert!(
        (many - 0.25).abs() < EPS,
        "with bonus cap saturated, weight must equal 0.25, got {many}"
    );
}

// ===== Weight Clamping =====

#[test]
fn weight_clamped_to_one() {
    let pairs = [
        (proj("p"), proj("p"), 0),
        (proj("p"), proj("p"), 1_000_000),
        (proj("p"), ScopePath::universal(), 0),
        (proj("p"), ScopePath::universal(), 1_000_000),
        (ScopePath::universal(), proj("p"), 1_000_000),
        (proj("p"), proj("p/q"), 0),
        (proj("p/q"), proj("p"), 0),
        (proj("p/x"), proj("p/y"), 0),
        (proj("p/x"), proj("p/y"), 1_000_000),
        (proj("a"), proj("b"), 0),
        (proj("a"), proj("b"), 1_000_000),
    ];
    for (q, n, ec) in &pairs {
        let w = scope_weight(q, n, *ec);
        assert!(
            (0.0 - EPS..=1.0 + EPS).contains(&w),
            "scope_weight out of [0, 1]: {w} for ({:?}, {:?}, {})",
            q.as_str(),
            n.as_str(),
            ec
        );
    }
}
