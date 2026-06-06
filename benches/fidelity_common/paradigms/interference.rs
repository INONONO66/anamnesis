use super::super::scenario::{activation_from, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series};
use anamnesis::graph::{EdgeType, KnowledgeType};
use anamnesis::query::SearchInput;

pub struct Interference;

impl Paradigm for Interference {
    fn name(&self) -> &'static str {
        "interference"
    }

    fn measure(&self) -> ParadigmResult {
        // Interference graph: shared cue A -> B and A -> D, with Contradicts(B,D).
        let mut e = scenario_engine();
        let a = ingest(&mut e, "A", KnowledgeType::Semantic);
        let b = ingest(&mut e, "B", KnowledgeType::Decision);
        let d = ingest(&mut e, "D", KnowledgeType::Decision);
        e.link(a, b, EdgeType::Semantic).unwrap();
        e.link(a, d, EdgeType::Semantic).unwrap();
        e.link(b, d, EdgeType::Contradicts).unwrap();
        let ab_interference = activation_from(&e, a, b);

        // Control: A' -> B' only (no competitor, no contradiction).
        let mut c = scenario_engine();
        let a2 = ingest(&mut c, "A2", KnowledgeType::Semantic);
        let b2 = ingest(&mut c, "B2", KnowledgeType::Decision);
        c.link(a2, b2, EdgeType::Semantic).unwrap();
        let ab_control = activation_from(&c, a2, b2);

        // Frustration + both-sides survive, via the public search package.
        let result = e
            .search(SearchInput {
                text: "B".into(),
                limit: 10,
                seed_limit: Some(5),
                ..Default::default()
            })
            .unwrap();
        let sigma = result
            .package
            .tensions
            .iter()
            .map(|t| t.stress)
            .fold(0.0_f64, f64::max);
        // Neither contradicting side is deleted (frustration.md: contradictions are
        // surfaced as tension, never auto-deleted) — both remain fetchable by id.
        let both_present = e.retained_action(b).is_ok() && e.retained_action(d).is_ok();
        let interference = ab_interference < ab_control;

        let passed = sigma > 0.0 && both_present && interference;
        ParadigmResult {
            name: "interference",
            series: vec![
                Series {
                    name: "ab_activation".into(),
                    xs: vec![0.0, 1.0],
                    ys: vec![ab_control, ab_interference],
                },
                Series {
                    name: "sigma".into(),
                    xs: vec![0.0],
                    ys: vec![sigma],
                },
            ],
            metrics: serde_json::json!({
                "ab_interference": ab_interference, "ab_control": ab_control,
                "interference": interference, "sigma": sigma, "both_survive": both_present,
            }),
            passed,
            explanation: format!(
                "sigma={:.4}(>0:{}); both B,D survive:{}; A-B interference {:.4} < control {:.4}:{}",
                sigma,
                sigma > 0.0,
                both_present,
                ab_interference,
                ab_control,
                interference
            ),
        }
    }
}
