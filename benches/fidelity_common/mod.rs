#![allow(dead_code, unused_imports)]

pub mod metrics;
pub mod paradigms;
pub mod scenario;

use serde::Serialize;

/// A named (x, y) series for plotting (e.g. delay vs retention).
#[derive(Debug, Clone, Serialize)]
pub struct Series {
    pub name: String,
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
}

/// The result of running one paradigm: plottable series + scalar metrics + verdict.
#[derive(Debug, Clone, Serialize)]
pub struct ParadigmResult {
    pub name: &'static str,
    pub series: Vec<Series>,
    pub metrics: serde_json::Value,
    pub passed: bool,
    pub explanation: String,
}

/// A cognitive-fidelity paradigm: drives the engine and computes a falsifiable verdict.
pub trait Paradigm {
    fn name(&self) -> &'static str;
    fn measure(&self) -> ParadigmResult;
}

/// All v1 paradigms.
pub fn all() -> Vec<Box<dyn Paradigm>> {
    vec![
        Box::new(paradigms::forgetting::Forgetting),
        Box::new(paradigms::fan::FanEffect),
        Box::new(paradigms::priming::Priming),
        Box::new(paradigms::interference::Interference),
        Box::new(paradigms::testing_effect::TestingEffect),
    ]
}
