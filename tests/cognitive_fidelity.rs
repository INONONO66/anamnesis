#[path = "../benches/fidelity_common/mod.rs"]
mod fidelity_common;

use fidelity_common::Paradigm;

fn assert_paradigm(p: impl Paradigm) {
    let r = p.measure();
    assert!(r.passed, "[{}] FAILED: {}", r.name, r.explanation);
}

#[test]
fn forgetting_is_power_law() {
    assert_paradigm(fidelity_common::paradigms::forgetting::Forgetting);
}

#[test]
fn fan_effect_decreases_activation() {
    assert_paradigm(fidelity_common::paradigms::fan::FanEffect);
}

#[test]
fn priming_is_additive() {
    assert_paradigm(fidelity_common::paradigms::priming::Priming);
}

#[test]
fn interference_surfaces_frustration() {
    assert_paradigm(fidelity_common::paradigms::interference::Interference);
}

#[test]
fn testing_effect_beats_restudy() {
    assert_paradigm(fidelity_common::paradigms::testing_effect::TestingEffect);
}

#[test]
fn measurements_are_deterministic() {
    for p in fidelity_common::all() {
        let a = p.measure();
        let b = p.measure();
        assert_eq!(
            serde_json::to_string(&a.metrics).unwrap(),
            serde_json::to_string(&b.metrics).unwrap(),
            "[{}] non-deterministic metrics",
            a.name
        );
    }
}
