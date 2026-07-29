#!/usr/bin/env python3
"""Convert Mem0 memory-benchmarks predict output into the strict external lane.

The converter intentionally copies retrieval text only. Ground-truth answers,
gold evidence, provider-generated answers, and judge decisions are never
written to the artifact consumed by `local_answer`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path
from typing import Any


MEMORYBENCH_ID = re.compile(r"^conv(\d+)_q(\d+)$")


def fnv1a64(data: bytes) -> str:
    value = 0xCBF29CE484222325
    for byte in data:
        value ^= byte
        value = (value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


def selected_question_ids(report_path: Path) -> set[str]:
    report = json.loads(report_path.read_text())
    questions = report.get("questions")
    if not isinstance(questions, list) or not questions:
        raise RuntimeError("selection report has no questions")
    result = {str(question["question_id"]) for question in questions}
    if len(result) != len(questions):
        raise RuntimeError("selection report has duplicate question ids")
    return result


def local_question_id(memorybench_id: str) -> str:
    match = MEMORYBENCH_ID.fullmatch(memorybench_id)
    if match is None:
        raise RuntimeError(f"unexpected MemoryBench question id {memorybench_id!r}")
    return f"locomo-{int(match.group(1))}-qa-{int(match.group(2))}"


def render_context(results: list[dict[str, Any]], top_k: int) -> tuple[str, list[dict[str, Any]]]:
    selected = results[:top_k]
    if not selected:
        return "No memories were returned by the external memory system.", []
    lines = ["## EXTERNAL MEMORY RESULTS"]
    evidence = []
    for rank, item in enumerate(selected, start=1):
        memory = str(item.get("memory", "")).strip()
        if not memory:
            raise RuntimeError(f"result rank {rank} has empty memory text")
        score = item.get("score", 0)
        created_at = item.get("created_at")
        header = f"### memory {rank} score={score}"
        if created_at:
            header += f" created_at={created_at}"
        lines.extend([header, memory])
        evidence.append(
            {
                "text": memory,
                "raw_session_id": None,
                "raw_turn_id": None,
            }
        )
    return "\n\n".join(lines), evidence


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--selection-report", type=Path, required=True)
    parser.add_argument("--system-name", required=True)
    parser.add_argument("--system-version", required=True)
    parser.add_argument("--system-config", type=Path, required=True)
    parser.add_argument("--top-k", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.top_k <= 0 or args.top_k > 512:
        parser.error("--top-k must be in 1..=512")

    selected = selected_question_ids(args.selection_report)
    records: dict[str, dict[str, Any]] = {}
    for path in sorted(args.input_dir.glob("conv*_q*.json")):
        item = json.loads(path.read_text())
        question_id = local_question_id(str(item.get("question_id", "")))
        if question_id not in selected:
            continue
        retrieval = item.get("retrieval")
        results = retrieval.get("search_results") if isinstance(retrieval, dict) else None
        if not isinstance(results, list):
            raise RuntimeError(f"{path} has no retrieval.search_results array")
        context, evidence = render_context(results, args.top_k)
        if question_id in records:
            raise RuntimeError(f"duplicate question output {question_id}")
        records[question_id] = {
            "question_id": question_id,
            "context": context,
            "evidence": evidence,
        }

    missing = selected - records.keys()
    extra = records.keys() - selected
    if missing or extra:
        raise RuntimeError(
            f"MemoryBench question set differs: missing={sorted(missing)} extra={sorted(extra)}"
        )

    config_digest = hashlib.sha256(args.system_config.read_bytes()).hexdigest()
    artifact = {
        "schema_version": 1,
        "dataset_fnv1a64": fnv1a64(args.dataset.read_bytes()),
        "system_name": args.system_name,
        "system_version": args.system_version,
        "system_config_digest": config_digest,
        "records": [records[question_id] for question_id in sorted(records)],
    }
    args.output.write_text(json.dumps(artifact, indent=2, ensure_ascii=False) + "\n")
    print(
        f"wrote {args.output}: records={len(records)} top_k={args.top_k} "
        f"config_sha256={config_digest}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
