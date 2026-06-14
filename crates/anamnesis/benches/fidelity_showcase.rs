//! Dense cognitive-fidelity SHOWCASE curves, emitted as CSV for plotting.
//!
//! This bench drives the SAME real engine the CI gate `tests/cognitive_fidelity.rs`
//! exercises (it reuses `benches/fidelity_common`), but instead of a pass/fail
//! verdict it sweeps each paradigm over a dense parameter range and writes the raw
//! engine numbers to `target/fidelity-showcase/*.csv`. `scripts/plot_showcase.py`
//! turns those CSVs into the committed charts under
//! `docs/07-quality-gates/assets/`.
//!
//! Every number below comes from `engine.retained_action` / `activation_from` on a
//! freshly built deterministic graph — nothing is fabricated. In particular the
//! recency-only counterfactual margin (spaced and clustered share their final study
//! day, so a model keyed only on time-since-last-access predicts ZERO difference) is
//! NOT written here; the plot draws that flat zero line itself.

#[path = "fidelity_common/mod.rs"]
mod fidelity_common;

use std::fs;
use std::io::Write;
use std::path::Path;

use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use fidelity_common::scenario::{self, activation_from, day, ingest, ingest_at, scenario_engine};

/// Fractional-day timestamp: T0 + d*DAY_MS (mirrors forgetting paradigm's helper).
fn day_frac(d: f64) -> Timestamp {
    Timestamp(scenario::T0 + (d * scenario::DAY_MS as f64) as u64)
}

/// (a) FORGETTING — dense single-decay curve.
///
/// For each delay we build a FRESH cohort of 50 Episodic nodes and decay it once
/// from creation to T0+delay (no cumulative ticks, no path-dependence), then read
/// the mean authoritative reservoir `retained_action`. CSV: delay_days,retained_action.
fn write_forgetting(out: &Path) {
    let delays_days: [f64; 12] = [
        0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0,
    ];
    let mut csv = String::from("delay_days,retained_action\n");
    for &d in &delays_days {
        let mut engine = scenario_engine();
        let ids: Vec<_> = (0..50)
            .map(|i| ingest(&mut engine, &format!("ep-{i}"), KnowledgeType::Episodic))
            .collect();
        engine.tick(day_frac(d)).unwrap();
        let mean = ids
            .iter()
            .map(|&id| engine.retained_action(id).unwrap())
            .sum::<f64>()
            / ids.len() as f64;
        csv.push_str(&format!("{d},{mean}\n"));
    }
    write_csv(out, "forgetting.csv", &csv);
}

/// One spacing arm: `study_days[0]` is the node's BIRTH day (its creation trace IS
/// the first study event — Pavlik & Anderson framing, no synthetic day-0 trace),
/// remaining days are committed re-presentations (`touch`). Reads `retained_action`
/// after ticking to `test_day`. Identical semantics to the gated spacing paradigm.
fn spacing_arm(study_days: &[u64], test_day: u64) -> f64 {
    let mut e = scenario_engine();
    let seed = ingest_at(&mut e, "seed", KnowledgeType::Semantic, day(study_days[0]));
    for &d in &study_days[1..] {
        e.touch(seed, day(d)).unwrap();
    }
    e.tick(day(test_day)).unwrap();
    e.retained_action(seed).unwrap()
}

/// (b) SPACING x RETENTION-INTERVAL — dense curve over test day.
///
/// Three schedules, each 3 study events, all stamped via `ingest_at` at the FIRST
/// study day so creation IS the first study (no day-0 trace):
///   spaced    [1,13,25]   distributed practice
///   clustered [23,24,25]  massed late — SAME final study day (25) as spaced, so
///                         recency is held constant between the two
///   massed    [1,2,3]     massed early
/// We tick each arm to day(test_day) and read `retained_action`. The recency-only
/// counterfactual margin is exactly 0 by construction (spaced and clustered last
/// study the same day); we do NOT write an engine number for it — the plot draws the
/// flat zero line. CSV: test_day,spaced,clustered,massed,margin_spaced_minus_clustered.
fn write_spacing(out: &Path) {
    let mut csv = String::from("test_day,spaced,clustered,massed,margin_spaced_minus_clustered\n");
    let mut test_day = 26u64;
    while test_day <= 70 {
        let spaced = spacing_arm(&[1, 13, 25], test_day);
        let clustered = spacing_arm(&[23, 24, 25], test_day);
        let massed = spacing_arm(&[1, 2, 3], test_day);
        let margin = spaced - clustered;
        csv.push_str(&format!(
            "{test_day},{spaced},{clustered},{massed},{margin}\n"
        ));
        test_day += 2;
    }
    write_csv(out, "spacing.csv", &csv);
}

/// (c) FAN — target activation vs fan size.
///
/// One hub cue with N competing out-edges; we read the first target's settled
/// query-local activation when RWR is seeded at the hub (read-only `activation_from`,
/// no touch/commit). Anderson's fan effect: activation falls as the cue's
/// associative strength is divided across more competitors. CSV: fan,activation.
fn write_fan(out: &Path) {
    let mut csv = String::from("fan,activation\n");
    for k in 1usize..=8 {
        let mut engine = scenario_engine();
        let hub = ingest(&mut engine, "hub", KnowledgeType::Semantic);
        let targets: Vec<_> = (0..k)
            .map(|i| ingest(&mut engine, &format!("t-{k}-{i}"), KnowledgeType::Semantic))
            .collect();
        for &t in &targets {
            engine.link(hub, t, EdgeType::Semantic).unwrap();
        }
        let activation = activation_from(&engine, hub, targets[0]);
        csv.push_str(&format!("{k},{activation}\n"));
    }
    write_csv(out, "fan.csv", &csv);
}

fn write_csv(out: &Path, name: &str, body: &str) {
    let path = out.join(name);
    fs::File::create(&path)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    eprintln!("wrote {}", path.display());
}

fn main() {
    let out = Path::new("target/fidelity-showcase");
    fs::create_dir_all(out).expect("create target/fidelity-showcase");
    write_forgetting(out);
    write_spacing(out);
    write_fan(out);
    eprintln!("fidelity-showcase CSVs written to {}", out.display());
}
