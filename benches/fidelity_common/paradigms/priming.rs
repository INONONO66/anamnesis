use super::super::scenario::{activation_from, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series};
use anamnesis::graph::{EdgeType, KnowledgeType};

pub struct Priming;

impl Paradigm for Priming {
    fn name(&self) -> &'static str {
        "priming"
    }

    fn measure(&self) -> ParadigmResult {
        // Build ONE graph seeded at S.
        //   priming:   S -> Tr (related)            vs   Ur (unrelated, no path)
        //   additivity: S -> M1, M2, M3             (symmetric intermediaries)
        //               M1 -> T2, M2 -> T2          (target reached by TWO paths)
        //               M3 -> T1                    (target reached by ONE path)
        //
        // additive_rwr conserves seed mass (multiple SEED cues are L1-normalized, so
        // seeding two cues does NOT double a target — that is correct probability
        // conservation, not a "sum"). The real additive-flow invariant is that
        // contributions from MULTIPLE INCOMING PATHS are SUMMED (never max-pooled) at
        // a node. So a target fed by two equal paths must out-activate one fed by a
        // single equal path: a(T2) > a(T1). A max-pooling model would give a(T2)≈a(T1).
        let mut e = scenario_engine();
        let s = ingest(&mut e, "S", KnowledgeType::Semantic);
        let tr = ingest(&mut e, "Tr", KnowledgeType::Semantic);
        let ur = ingest(&mut e, "Ur", KnowledgeType::Semantic); // unrelated: no edge
        let m1 = ingest(&mut e, "M1", KnowledgeType::Semantic);
        let m2 = ingest(&mut e, "M2", KnowledgeType::Semantic);
        let m3 = ingest(&mut e, "M3", KnowledgeType::Semantic);
        let t2 = ingest(&mut e, "T2", KnowledgeType::Semantic); // two incoming paths
        let t1 = ingest(&mut e, "T1", KnowledgeType::Semantic); // one incoming path
        for (a, b) in [
            (s, tr),
            (s, m1),
            (s, m2),
            (s, m3),
            (m1, t2),
            (m2, t2),
            (m3, t1),
        ] {
            e.link(a, b, EdgeType::Semantic).unwrap();
        }

        let a_related = activation_from(&e, s, tr);
        let a_unrelated = activation_from(&e, s, ur);
        let priming = a_related - a_unrelated;

        let a_two_paths = activation_from(&e, s, t2);
        let a_one_path = activation_from(&e, s, t1);
        let summed = a_two_paths > a_one_path + 1e-9;

        let passed = priming > 0.0 && summed;
        ParadigmResult {
            name: "priming",
            series: vec![
                Series {
                    name: "priming".into(),
                    xs: vec![0.0, 1.0],
                    ys: vec![a_unrelated, a_related],
                },
                Series {
                    name: "path_summation".into(),
                    xs: vec![1.0, 2.0],
                    ys: vec![a_one_path, a_two_paths],
                },
            ],
            metrics: serde_json::json!({
                "a_related": a_related, "a_unrelated": a_unrelated, "priming": priming,
                "a_two_paths": a_two_paths, "a_one_path": a_one_path, "summed": summed,
            }),
            passed,
            explanation: format!(
                "priming={:.4} (related>unrelated:{}); two converging paths out-activate one (a_T2={:.4} > a_T1={:.4}:{}) — contributions sum, not max",
                priming,
                priming > 0.0,
                a_two_paths,
                a_one_path,
                summed
            ),
        }
    }
}
