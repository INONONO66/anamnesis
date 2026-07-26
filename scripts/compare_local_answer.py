#!/usr/bin/env python3
"""Paired comparison for compatible local_answer product-wire reports."""

from __future__ import annotations

import argparse
import json
import random
import statistics
import sys
from collections import defaultdict
from pathlib import Path


DEFAULT_ROUTE = "2-retrieval-baseline"
RESAMPLES = 10_000
EPSILON = 1e-12


def load_report(path: Path) -> dict:
    with path.open(encoding="utf-8") as handle:
        report = json.load(handle)
    if report.get("schema_version", 0) < 16:
        raise ValueError(f"{path}: schema v16 or newer is required")
    return report


def validate_pair(baseline: dict, candidate: dict, route: str) -> list[str]:
    fixed_fields = ("dataset_fnv1a64",)
    for field in fixed_fields:
        if baseline.get(field) != candidate.get(field):
            raise ValueError(f"reports differ on {field}")

    baseline_config = baseline["config"]
    candidate_config = candidate["config"]
    controlled_fields = (
        "dataset",
        "samples",
        "stratify",
        "question_type",
        "sample_seed",
        "skip_adversarial",
        "context_surface",
        "answer_prompt_version",
        "baseline_reader_model",
        "dataset_loader_version",
        "reader_generation",
    )
    for field in controlled_fields:
        if baseline_config.get(field) != candidate_config.get(field):
            raise ValueError(f"reports differ on controlled config field {field}")

    baseline_ids = {question["question_id"] for question in baseline["questions"]}
    candidate_ids = {question["question_id"] for question in candidate["questions"]}
    if baseline_ids != candidate_ids:
        raise ValueError("reports do not contain the same question ids")
    missing = [
        question["question_id"]
        for question in baseline["questions"]
        if route not in question.get("routes", {})
    ]
    missing.extend(
        question["question_id"]
        for question in candidate["questions"]
        if route not in question.get("routes", {})
    )
    if missing:
        raise ValueError(f"route {route!r} is missing for {len(missing)} records")
    return sorted(baseline_ids)


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = round((len(ordered) - 1) * fraction)
    return ordered[index]


def paired_question_ci(deltas: list[float], seed: int) -> list[float]:
    rng = random.Random(seed)
    means = [
        statistics.fmean(rng.choice(deltas) for _ in deltas)
        for _ in range(RESAMPLES)
    ]
    return [percentile(means, 0.025), percentile(means, 0.975)]


def paired_cluster_ci(rows: list[dict], field: str, seed: int) -> list[float]:
    clusters: dict[int, list[float]] = defaultdict(list)
    for row in rows:
        clusters[row["sample_index"]].append(row[field])
    keys = sorted(clusters)
    rng = random.Random(seed)
    means: list[float] = []
    for _ in range(RESAMPLES):
        sampled: list[float] = []
        for _ in keys:
            sampled.extend(clusters[rng.choice(keys)])
        means.append(statistics.fmean(sampled))
    return [percentile(means, 0.025), percentile(means, 0.975)]


def mean(values: list[float]) -> float:
    return statistics.fmean(values) if values else 0.0


def stage_recall(question: dict, stage: str) -> float:
    evaluation = question["retrieval_evaluation"]
    if stage == "rendered":
        return float(evaluation["rendered_recall"])
    return float(evaluation[f"{stage}_metrics"]["recall_at_k"])


def score(question: dict, route: str, field: str) -> float:
    value = question["routes"][route].get(field)
    if value is None:
        raise ValueError(
            f"{question['question_id']}: route {route!r} has no {field}"
        )
    return float(value)


def build_comparison(
    baseline: dict, candidate: dict, route: str, seed: int
) -> dict:
    question_ids = validate_pair(baseline, candidate, route)
    baseline_by_id = {
        question["question_id"]: question for question in baseline["questions"]
    }
    candidate_by_id = {
        question["question_id"]: question for question in candidate["questions"]
    }
    rows: list[dict] = []
    for question_id in question_ids:
        before = baseline_by_id[question_id]
        after = candidate_by_id[question_id]
        row = {
            "question_id": question_id,
            "sample_index": before["sample_index"],
            "question_type": before["question_type"],
        }
        for stage in ("candidate", "reranker", "delivered", "rendered"):
            row[f"{stage}_recall_before"] = stage_recall(before, stage)
            row[f"{stage}_recall_after"] = stage_recall(after, stage)
            row[f"{stage}_recall_delta"] = (
                row[f"{stage}_recall_after"] - row[f"{stage}_recall_before"]
            )
        row["raw_f1_before"] = score(before, route, "locomo_official_f1")
        row["raw_f1_after"] = score(after, route, "locomo_official_f1")
        row["raw_f1_delta"] = row["raw_f1_after"] - row["raw_f1_before"]
        row["surface_f1_before"] = score(
            before, route, "locomo_reader_surface_f1"
        )
        row["surface_f1_after"] = score(
            after, route, "locomo_reader_surface_f1"
        )
        row["surface_f1_delta"] = (
            row["surface_f1_after"] - row["surface_f1_before"]
        )
        rows.append(row)

    raw_deltas = [row["raw_f1_delta"] for row in rows]
    surface_deltas = [row["surface_f1_delta"] for row in rows]
    rendered_improved = [
        row for row in rows if row["rendered_recall_delta"] > EPSILON
    ]
    rendered_regressed = [
        row for row in rows if row["rendered_recall_delta"] < -EPSILON
    ]
    rendered_tied = [
        row
        for row in rows
        if abs(row["rendered_recall_delta"]) <= EPSILON
    ]

    def transmission(group: list[dict]) -> dict:
        return {
            "questions": len(group),
            "mean_raw_f1_delta": mean(
                [row["raw_f1_delta"] for row in group]
            ),
            "answer_wins": sum(
                row["raw_f1_delta"] > EPSILON for row in group
            ),
            "answer_ties": sum(
                abs(row["raw_f1_delta"]) <= EPSILON for row in group
            ),
            "answer_losses": sum(
                row["raw_f1_delta"] < -EPSILON for row in group
            ),
        }

    return {
        "route": route,
        "questions": len(rows),
        "baseline_run_id": baseline["run_id"],
        "candidate_run_id": candidate["run_id"],
        "context_surface": baseline["config"]["context_surface"],
        "retrieval_cutoffs": {
            "baseline": {
                "candidate_k": baseline["config"]["consumer_candidate_k"],
                "final_k": baseline["config"]["top_k"],
            },
            "candidate": {
                "candidate_k": candidate["config"]["consumer_candidate_k"],
                "final_k": candidate["config"]["top_k"],
            },
        },
        "raw_official_f1": {
            "baseline": mean([row["raw_f1_before"] for row in rows]),
            "candidate": mean([row["raw_f1_after"] for row in rows]),
            "paired_delta": mean(raw_deltas),
            "question_bootstrap_ci95": paired_question_ci(raw_deltas, seed),
            "conversation_cluster_bootstrap_ci95": paired_cluster_ci(
                rows, "raw_f1_delta", seed ^ 0x9E3779B9
            ),
            "wins": sum(delta > EPSILON for delta in raw_deltas),
            "ties": sum(abs(delta) <= EPSILON for delta in raw_deltas),
            "losses": sum(delta < -EPSILON for delta in raw_deltas),
        },
        "reader_surface_f1": {
            "baseline": mean([row["surface_f1_before"] for row in rows]),
            "candidate": mean([row["surface_f1_after"] for row in rows]),
            "paired_delta": mean(surface_deltas),
        },
        "retrieval_stage_recall": {
            stage: {
                "baseline": mean(
                    [row[f"{stage}_recall_before"] for row in rows]
                ),
                "candidate": mean(
                    [row[f"{stage}_recall_after"] for row in rows]
                ),
                "paired_delta": mean(
                    [row[f"{stage}_recall_delta"] for row in rows]
                ),
            }
            for stage in ("candidate", "reranker", "delivered", "rendered")
        },
        "rendered_recall_to_answer_f1": {
            "recall_improved": transmission(rendered_improved),
            "recall_tied": transmission(rendered_tied),
            "recall_regressed": transmission(rendered_regressed),
        },
    }


def build_retrieval_comparison(
    baseline: dict, candidate: dict, variant: str, seed: int
) -> dict:
    for field in (
        "dataset_fnv1a64",
        "dataset_loader_version",
    ):
        before = (
            baseline.get(field)
            if field == "dataset_fnv1a64"
            else baseline["config"].get(field)
        )
        after = (
            candidate.get(field)
            if field == "dataset_fnv1a64"
            else candidate["config"].get(field)
        )
        if before != after:
            raise ValueError(f"reports differ on {field}")
    for field in (
        "dataset",
        "samples",
        "stratify",
        "question_type",
        "sample_seed",
        "skip_adversarial",
        "context_surface",
        "embedding_model",
        "consumer_cross_encoder",
        "consumer_candidate_k",
        "first_stage_seed_limit",
    ):
        if baseline["config"].get(field) != candidate["config"].get(field):
            raise ValueError(f"reports differ on controlled config field {field}")

    baseline_by_id = {
        question["question_id"]: question for question in baseline["questions"]
    }
    candidate_by_id = {
        question["question_id"]: question for question in candidate["questions"]
    }
    if set(baseline_by_id) != set(candidate_by_id):
        raise ValueError("reports do not contain the same question ids")
    rows: list[dict] = []
    for question_id in sorted(baseline_by_id):
        before_question = baseline_by_id[question_id]
        after_question = candidate_by_id[question_id]
        before_evaluation = before_question.get("retrieval_evaluation")
        after_evaluation = after_question.get("retrieval_evaluation")
        if before_evaluation is None or after_evaluation is None:
            raise ValueError(f"{question_id}: retrieval evaluation is incomplete")
        before = before_evaluation["selection_variants"].get(variant)
        after = after_evaluation["selection_variants"].get(variant)
        if before is None or after is None:
            raise ValueError(f"{question_id}: selection variant {variant!r} is missing")
        rows.append(
            {
                "question_id": question_id,
                "sample_index": before_question["sample_index"],
                "question_type": before_question["question_type"],
                "selected_recall_delta": float(
                    after["selected_metrics"]["recall_at_k"]
                )
                - float(before["selected_metrics"]["recall_at_k"]),
                "delivered_recall_delta": float(
                    after["delivered_metrics"]["recall_at_k"]
                )
                - float(before["delivered_metrics"]["recall_at_k"]),
                "rendered_recall_before": float(before["rendered_recall"]),
                "rendered_recall_after": float(after["rendered_recall"]),
                "rendered_recall_delta": float(after["rendered_recall"])
                - float(before["rendered_recall"]),
                "rendered_hit_before": bool(before["rendered_hit"]),
                "rendered_hit_after": bool(after["rendered_hit"]),
            }
        )
    rendered_deltas = [row["rendered_recall_delta"] for row in rows]
    by_type: dict[str, list[float]] = defaultdict(list)
    for row in rows:
        by_type[row["question_type"]].append(row["rendered_recall_delta"])
    return {
        "selection_variant": variant,
        "questions": len(rows),
        "baseline_run_id": baseline["run_id"],
        "candidate_run_id": candidate["run_id"],
        "rendered_recall": {
            "baseline": mean(
                [row["rendered_recall_before"] for row in rows]
            ),
            "candidate": mean(
                [row["rendered_recall_after"] for row in rows]
            ),
            "paired_delta": mean(rendered_deltas),
            "question_bootstrap_ci95": paired_question_ci(
                rendered_deltas, seed
            ),
            "conversation_cluster_bootstrap_ci95": paired_cluster_ci(
                rows, "rendered_recall_delta", seed ^ 0x9E3779B9
            ),
            "wins": sum(delta > EPSILON for delta in rendered_deltas),
            "ties": sum(abs(delta) <= EPSILON for delta in rendered_deltas),
            "losses": sum(delta < -EPSILON for delta in rendered_deltas),
        },
        "selected_recall_paired_delta": mean(
            [row["selected_recall_delta"] for row in rows]
        ),
        "delivered_recall_paired_delta": mean(
            [row["delivered_recall_delta"] for row in rows]
        ),
        "rendered_hit": {
            "baseline": sum(row["rendered_hit_before"] for row in rows)
            / len(rows),
            "candidate": sum(row["rendered_hit_after"] for row in rows)
            / len(rows),
        },
        "rendered_recall_paired_delta_by_type": {
            question_type: mean(deltas)
            for question_type, deltas in sorted(by_type.items())
        },
    }


def build_primary_retrieval_comparison(
    baseline: dict, candidate: dict, seed: int
) -> dict:
    for field in ("dataset_fnv1a64",):
        if baseline.get(field) != candidate.get(field):
            raise ValueError(f"reports differ on {field}")
    for field in (
        "dataset",
        "samples",
        "stratify",
        "question_type",
        "sample_seed",
        "skip_adversarial",
        "context_surface",
        "embedding_model",
        "consumer_cross_encoder",
        "consumer_candidate_k",
        "first_stage_seed_limit",
        "top_k",
    ):
        if baseline["config"].get(field) != candidate["config"].get(field):
            raise ValueError(f"reports differ on controlled config field {field}")

    baseline_by_id = {
        question["question_id"]: question for question in baseline["questions"]
    }
    candidate_by_id = {
        question["question_id"]: question for question in candidate["questions"]
    }
    if set(baseline_by_id) != set(candidate_by_id):
        raise ValueError("reports do not contain the same question ids")

    rows: list[dict] = []
    for question_id in sorted(baseline_by_id):
        before = baseline_by_id[question_id]
        after = candidate_by_id[question_id]
        if (
            before.get("retrieval_evaluation") is None
            or after.get("retrieval_evaluation") is None
        ):
            raise ValueError(f"{question_id}: retrieval evaluation is incomplete")
        row = {
            "question_id": question_id,
            "sample_index": before["sample_index"],
            "question_type": before["question_type"],
            "rendered_hit_before": bool(
                before["retrieval_evaluation"]["rendered_hit"]
            ),
            "rendered_hit_after": bool(
                after["retrieval_evaluation"]["rendered_hit"]
            ),
        }
        for stage in ("candidate", "reranker", "delivered", "rendered"):
            before_recall = stage_recall(before, stage)
            after_recall = stage_recall(after, stage)
            row[f"{stage}_recall_before"] = before_recall
            row[f"{stage}_recall_after"] = after_recall
            row[f"{stage}_recall_delta"] = after_recall - before_recall
        rows.append(row)

    rendered_deltas = [row["rendered_recall_delta"] for row in rows]
    by_type: dict[str, list[float]] = defaultdict(list)
    for row in rows:
        by_type[row["question_type"]].append(row["rendered_recall_delta"])

    return {
        "questions": len(rows),
        "baseline_run_id": baseline["run_id"],
        "candidate_run_id": candidate["run_id"],
        "retrieval_cutoffs": {
            "candidate_k": baseline["config"]["consumer_candidate_k"],
            "final_k": baseline["config"]["top_k"],
        },
        "retrieval_stage_recall": {
            stage: {
                "baseline": mean(
                    [row[f"{stage}_recall_before"] for row in rows]
                ),
                "candidate": mean(
                    [row[f"{stage}_recall_after"] for row in rows]
                ),
                "paired_delta": mean(
                    [row[f"{stage}_recall_delta"] for row in rows]
                ),
            }
            for stage in ("candidate", "reranker", "delivered", "rendered")
        },
        "rendered_recall": {
            "question_bootstrap_ci95": paired_question_ci(
                rendered_deltas, seed
            ),
            "conversation_cluster_bootstrap_ci95": paired_cluster_ci(
                rows, "rendered_recall_delta", seed ^ 0x9E3779B9
            ),
            "wins": sum(delta > EPSILON for delta in rendered_deltas),
            "ties": sum(abs(delta) <= EPSILON for delta in rendered_deltas),
            "losses": sum(delta < -EPSILON for delta in rendered_deltas),
        },
        "rendered_hit": {
            "baseline": mean(
                [float(row["rendered_hit_before"]) for row in rows]
            ),
            "candidate": mean(
                [float(row["rendered_hit_after"]) for row in rows]
            ),
        },
        "rendered_recall_paired_delta_by_type": {
            question_type: mean(deltas)
            for question_type, deltas in sorted(by_type.items())
        },
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--route", default=DEFAULT_ROUTE)
    parser.add_argument(
        "--selection-variant",
        help="compare one reader-free fixed-ranking variant, e.g. top-20",
    )
    parser.add_argument(
        "--retrieval-primary",
        action="store_true",
        help="compare the primary reader-free retrieval surfaces",
    )
    parser.add_argument("--seed", type=int, default=42)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.selection_variant and args.retrieval_primary:
        print(
            "compare_local_answer: --selection-variant and --retrieval-primary "
            "are mutually exclusive",
            file=sys.stderr,
        )
        return 2
    try:
        baseline = load_report(args.baseline)
        candidate = load_report(args.candidate)
        comparison = (
            build_primary_retrieval_comparison(
                baseline, candidate, args.seed
            )
            if args.retrieval_primary
            else build_retrieval_comparison(
                baseline, candidate, args.selection_variant, args.seed
            )
            if args.selection_variant
            else build_comparison(
                baseline, candidate, args.route, args.seed
            )
        )
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"compare_local_answer: {error}", file=sys.stderr)
        return 2
    json.dump(comparison, sys.stdout, indent=2, sort_keys=True)
    print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
