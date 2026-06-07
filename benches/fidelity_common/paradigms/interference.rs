use super::super::scenario::{activation_from, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series};
use anamnesis::Engine;
use anamnesis::graph::{EdgeType, KnowledgeType};
use anamnesis::query::SearchInput;

pub struct Interference;

/// Max surfaced frustration stress for a query seeded by `text`.
fn max_stress(e: &Engine, text: &str) -> f64 {
    e.search(SearchInput {
        text: text.into(),
        limit: 10,
        seed_limit: Some(5),
        ..Default::default()
    })
    .unwrap()
    .package
    .tensions
    .iter()
    .map(|t| t.stress)
    .fold(0.0_f64, f64::max)
}

impl Paradigm for Interference {
    fn name(&self) -> &'static str {
        "interference"
    }

    fn measure(&self) -> ParadigmResult {
        // (1) Contradiction graph: shared cue A -> B, A -> D, with Contradicts(B,D).
        let mut e = scenario_engine();
        let a = ingest(&mut e, "A", KnowledgeType::Semantic);
        let b = ingest(&mut e, "B", KnowledgeType::Decision);
        let d = ingest(&mut e, "D", KnowledgeType::Decision);
        e.link(a, b, EdgeType::Semantic).unwrap();
        e.link(a, d, EdgeType::Semantic).unwrap();
        e.link(b, d, EdgeType::Contradicts).unwrap();
        let sigma_contra = max_stress(&e, "B");
        let both_survive = e.retained_action(b).is_ok() && e.retained_action(d).is_ok();

        // (2) Non-contradiction control: IDENTICAL fan (A2 -> B2, A2 -> D2) but the
        // B2-D2 edge is an ordinary Semantic link, NOT Contradicts. Frustration must
        // be contradiction-SPECIFIC: sigma_control == 0 proves the stress is driven
        // by the Contradicts edge, not by the shared-cue fan.
        let mut c = scenario_engine();
        let a2 = ingest(&mut c, "A2", KnowledgeType::Semantic);
        let b2 = ingest(&mut c, "B2", KnowledgeType::Decision);
        let d2 = ingest(&mut c, "D2", KnowledgeType::Decision);
        c.link(a2, b2, EdgeType::Semantic).unwrap();
        c.link(a2, d2, EdgeType::Semantic).unwrap();
        c.link(b2, d2, EdgeType::Semantic).unwrap();
        let sigma_control = max_stress(&c, "B2");

        // (3) Cue competition (the fan-based component of interference, honestly
        // labelled — NOT caused by the Contradicts edge, which is excluded from
        // propagation): a shared cue divides activation, so B's activation with a
        // competitor (A -> B, A -> D) is below B's activation with no competitor.
        let ab_competed = activation_from(&e, a, b);
        let mut s = scenario_engine();
        let a3 = ingest(&mut s, "A3", KnowledgeType::Semantic);
        let b3 = ingest(&mut s, "B3", KnowledgeType::Decision);
        s.link(a3, b3, EdgeType::Semantic).unwrap();
        let ab_alone = activation_from(&s, a3, b3);
        let cue_competition = ab_competed < ab_alone;

        let passed = sigma_contra > 0.0 && sigma_control == 0.0 && both_survive && cue_competition;
        ParadigmResult {
            name: "interference",
            series: vec![
                Series {
                    name: "sigma_contra_vs_control".into(),
                    xs: vec![0.0, 1.0],
                    ys: vec![sigma_control, sigma_contra],
                },
                Series {
                    name: "cue_competition".into(),
                    xs: vec![0.0, 1.0],
                    ys: vec![ab_alone, ab_competed],
                },
            ],
            metrics: serde_json::json!({
                "sigma_contra": sigma_contra, "sigma_control": sigma_control,
                "both_survive": both_survive,
                "ab_competed": ab_competed, "ab_alone": ab_alone, "cue_competition": cue_competition,
            }),
            passed,
            explanation: format!(
                "frustration is contradiction-specific (sigma_contra={:.4}>0, control={:.4}=0:{}); both B,D survive:{}; shared-cue competition lowers B activation ({:.4} < {:.4}:{})",
                sigma_contra,
                sigma_control,
                sigma_control == 0.0,
                both_survive,
                ab_competed,
                ab_alone,
                cue_competition
            ),
        }
    }
}
