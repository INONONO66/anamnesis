//! Argument-parsing tests for the `real_memory` bench CLI.
//!
//! The bench target itself is `harness = false`, so `#[test]` functions inside
//! it never execute; the CLI module is included here by path so its parser
//! runs under the normal integration-test harness.

#![allow(dead_code)]

#[path = "../benches/eval_common/mod.rs"]
mod eval_common;

#[path = "../benches/eval/real_memory_cli.rs"]
mod real_memory_cli;

use real_memory_cli::parse_args;

fn args(items: &[&str]) -> impl Iterator<Item = String> + use<> {
    items
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .into_iter()
}

#[test]
fn seed_limit_parses_correctly() {
    let parsed = parse_args(args(&["--dataset", "locomo", "--seed-limit", "40"]))
        .expect("parse succeeds")
        .expect("args present");
    assert_eq!(parsed.seed_limit, Some(40));
}

#[test]
fn seed_limit_defaults_to_none() {
    let parsed = parse_args(args(&["--dataset", "locomo"]))
        .expect("parse succeeds")
        .expect("args present");
    assert_eq!(parsed.seed_limit, None);
}

#[test]
fn seed_limit_zero_is_rejected() {
    let err = parse_args(args(&["--dataset", "locomo", "--seed-limit", "0"]))
        .expect_err("--seed-limit 0 must be rejected");
    assert!(err.to_string().contains("--seed-limit"));
}
