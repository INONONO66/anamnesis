use super::super::scenario::{day, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series};
use anamnesis::graph::KnowledgeType;
use anamnesis::query::{Query, QueryConfig};

pub struct Commitment;

impl Commitment {
    /// `committed=true`: three committed retrievals (`touch`) at spaced days (7, 14,
    /// 21), each appending a durable access trace. `committed=false`: three read-only
    /// retrievals (`query`), which mutate nothing. Both arms study once, wait past the
    /// prior ceiling, then (do / do not) practice, then read `retained_action` at a
    /// common delayed test.
    fn arm(&self, committed: bool) -> f64 {
        let mut e = scenario_engine();
        let seed = ingest(&mut e, "seed", KnowledgeType::Semantic);
        // Retention interval 1: let the base level fall below the prior ceiling so
        // committed reinforcement has headroom to show.
        e.tick(day(7)).unwrap();
        // Practice at SPACED days so each committed touch lands on a less-active node
        // and leaves a genuinely durable trace (under activation-dependent decay,
        // repeated same-instant touches would be near-inert).
        for &d in &[7u64, 14, 21] {
            if committed {
                e.touch(seed, day(d)).unwrap(); // committed retrieval: appends a durable trace
            } else {
                // read-only retrieval at the SAME scheduled day (time-matched to the
                // committed arm); mutates nothing (`query` takes `&self`; see the
                // read_only_retrieval_does_not_mutate_reservoirs invariant test).
                let mut cfg = QueryConfig::default();
                cfg.now = Some(day(d));
                let _ = e
                    .query(&Query::Associative { seed, budget: 100 }, &cfg)
                    .unwrap();
            }
        }
        // Retention interval 2: common delayed test.
        e.tick(day(40)).unwrap();
        e.retained_action(seed).unwrap()
    }
}

impl Paradigm for Commitment {
    fn name(&self) -> &'static str {
        "commitment"
    }

    fn measure(&self) -> ParadigmResult {
        let committed = self.arm(true);
        let read_only = self.arm(false);
        let passed = committed > read_only;
        ParadigmResult {
            name: "commitment",
            series: vec![Series {
                name: "delayed_retained_action".into(),
                xs: vec![0.0, 1.0],
                ys: vec![read_only, committed],
            }],
            metrics: serde_json::json!({ "committed": committed, "read_only": read_only }),
            passed,
            explanation: format!(
                "ENGINE COMMITMENT PRINCIPLE (state = the integral of committed interactions): committed retrieval appends a durable access trace and raises later retained_action, while read-only retrieval mutates nothing — {:.4} {} {:.4}. This is NOT the human testing effect (test-vs-restudy at matched timing), which activation-dependent decay does not reproduce (see ADR-0008); it is the engine invariant that only committed interactions update state.",
                committed,
                if passed { ">" } else { "<=" },
                read_only
            ),
        }
    }
}
