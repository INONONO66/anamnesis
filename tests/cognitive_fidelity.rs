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
fn read_only_retrieval_does_not_mutate_reservoirs() {
    use anamnesis::graph::{EdgeType, KnowledgeType};
    use fidelity_common::scenario::{activation_from, ingest, scenario_engine};

    let mut e = scenario_engine();
    let s = ingest(&mut e, "s", KnowledgeType::Semantic);
    let t = ingest(&mut e, "t", KnowledgeType::Semantic);
    e.link(s, t, EdgeType::Semantic).unwrap();

    let before = e.retained_action(t).unwrap();
    // The read-only paradigms (forgetting/fan/priming) drive recall through this
    // path; it takes &Engine and must leave the reservoir untouched.
    let _ = activation_from(&e, s, t);
    let _ = activation_from(&e, s, t);
    let after = e.retained_action(t).unwrap();

    assert_eq!(before, after, "read-only RWR must not mutate the reservoir");
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
