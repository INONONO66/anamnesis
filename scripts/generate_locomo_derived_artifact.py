#!/usr/bin/env python3
"""Generate a frozen, reference-blind LoCoMo extraction artifact.

The script reads conversation turns only and sends batches through the
`anamnesis extract-preview` product prompt/provider/validator path. It never
reads QA answers or evidence annotations. A sidecar checkpoint makes the local
Qwen run resumable; the final artifact contains only the strict benchmark wire.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import tempfile
import urllib.request
from pathlib import Path
from typing import Any


SESSION_RE = re.compile(r"^session_(\d+)$")
MODEL = "qwen3.6:35b-a3b"


def fnv1a64(data: bytes) -> str:
    value = 0xCBF29CE484222325
    for byte in data:
        value ^= byte
        value = (value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


def ollama_digest(base_url: str, model: str) -> str:
    with urllib.request.urlopen(f"{base_url.rstrip('/')}/api/tags", timeout=30) as response:
        payload = json.load(response)
    for item in payload.get("models", []):
        if item.get("name") == model or item.get("model") == model:
            digest = item.get("digest")
            if isinstance(digest, str) and digest:
                return digest
    raise RuntimeError(f"{model} is not installed in Ollama")


def session_timestamp_ms(value: str | None, sample_index: int, session_index: int) -> int:
    if value:
        try:
            parsed = dt.datetime.strptime(value, "%I:%M %p on %d %B, %Y")
            return int(parsed.replace(tzinfo=dt.timezone.utc).timestamp() * 1000)
        except ValueError:
            pass
    fallback = dt.datetime(2020, 1, 1, tzinfo=dt.timezone.utc)
    return int(fallback.timestamp() * 1000) + sample_index * 40 * 86_400_000 + session_index * 86_400_000


def preview_batch(
    binary: Path,
    sources: list[dict[str, Any]],
    timeout: int,
    transient_retries: int,
) -> dict[str, Any]:
    with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as handle:
        json.dump({"sources": sources}, handle, ensure_ascii=False)
        handle.flush()
        for attempt in range(transient_retries + 1):
            try:
                completed = subprocess.run(
                    [str(binary), "extract-preview", handle.name],
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=timeout,
                )
            except subprocess.TimeoutExpired as error:
                if attempt < transient_retries:
                    print(
                        f"transient outer timeout; retry {attempt + 1}/{transient_retries}",
                        flush=True,
                    )
                    continue
                raise RuntimeError("extract-preview exceeded outer timeout") from error
            if completed.returncode == 0:
                return json.loads(completed.stdout)
            detail = completed.stderr.strip() or "no stderr"
            transient = (
                "provider timed out" in detail
                or "could not request the local extraction provider" in detail
            )
            if transient and attempt < transient_retries:
                print(
                    f"transient provider failure; retry {attempt + 1}/{transient_retries}",
                    flush=True,
                )
                continue
            raise RuntimeError(f"extract-preview failed: {detail}")
    raise RuntimeError("extract-preview retry loop ended unexpectedly")


def preview_parts(
    binary: Path,
    sources: list[dict[str, Any]],
    timeout: int,
    transient_retries: int,
    part_key: str = "",
) -> list[tuple[str, dict[str, Any] | None]]:
    try:
        return [
            (
                part_key,
                preview_batch(binary, sources, timeout, transient_retries),
            )
        ]
    except RuntimeError as error:
        if "schema-reject" not in str(error):
            raise
        if len(sources) == 1:
            source = sources[0]
            print(
                "validation rejected one source; recording fail-closed omission "
                f"node_id={source['node_id']} turn_key={source['turn_key']}",
                flush=True,
            )
            return [(part_key, None)]
        midpoint = len(sources) // 2
        print(
            f"validation rejected {len(sources)} sources; "
            f"retrying deterministic halves {len(sources[:midpoint])}+{len(sources[midpoint:])}",
            flush=True,
        )
        return preview_parts(
            binary,
            sources[:midpoint],
            timeout,
            transient_retries,
            part_key + "a",
        ) + preview_parts(
            binary,
            sources[midpoint:],
            timeout,
            transient_retries,
            part_key + "b",
        )


def checkpoint(path: Path, state: dict[str, Any]) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(state, indent=2, ensure_ascii=False) + "\n")
    temporary.replace(path)


def validate_final_records(records: list[dict[str, Any]]) -> None:
    for record in records:
        fields = [str(record["id"]), str(record["content"])]
        fields.extend(str(tag) for tag in record.get("entity_tags", []))
        for value in fields:
            if any(ord(char) < 32 and char not in "\n\t" for char in value):
                raise RuntimeError(
                    f"artifact record {record['id']!r} contains a control character"
                )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/anamnesis"))
    parser.add_argument("--ollama-base-url", default="http://127.0.0.1:11434")
    parser.add_argument("--timeout-secs", type=int, default=300)
    parser.add_argument("--transient-retries", type=int, default=2)
    parser.add_argument("--max-batches", type=int)
    args = parser.parse_args()
    if args.timeout_secs <= 0 or args.transient_retries < 0:
        parser.error("timeout must be positive and transient retries must be non-negative")

    dataset_bytes = args.dataset.read_bytes()
    samples = json.loads(dataset_bytes)
    if not isinstance(samples, list):
        raise RuntimeError("LoCoMo dataset root must be an array")
    state_path = args.output.with_suffix(args.output.suffix + ".state.json")
    state: dict[str, Any]
    if state_path.exists():
        state = json.loads(state_path.read_text())
        if state.get("dataset_fnv1a64") != fnv1a64(dataset_bytes):
            raise RuntimeError("checkpoint dataset fingerprint differs")
    else:
        state = {
            "dataset_fnv1a64": fnv1a64(dataset_bytes),
            "completed_batches": [],
            "records": [],
            "relations": [],
            "skipped_sources": [],
            "profile": None,
        }

    state.setdefault("skipped_sources", [])
    completed_batches = set(state["completed_batches"])
    processed_now = 0
    for sample_index, sample in enumerate(samples):
        if not isinstance(sample, dict):
            raise RuntimeError(f"sample {sample_index} is not an object")
        session_keys = sorted(
            (key for key in sample if SESSION_RE.match(key)),
            key=lambda key: int(SESSION_RE.match(key).group(1)),  # type: ignore[union-attr]
        )
        for raw_session_id in session_keys:
            turns = sample[raw_session_id]
            if not isinstance(turns, list) or not turns:
                continue
            session_number = int(SESSION_RE.match(raw_session_id).group(1))  # type: ignore[union-attr]
            start_ms = session_timestamp_ms(
                sample.get(f"{raw_session_id}_date_time"),
                sample_index,
                session_number,
            )
            for chunk_index, offset in enumerate(range(0, len(turns), 20)):
                batch_key = f"{sample_index}:{raw_session_id}:{chunk_index}"
                if batch_key in completed_batches:
                    continue
                chunk = turns[offset : offset + 20]
                sources = []
                turn_ids: dict[int, str] = {}
                for local_index, turn in enumerate(chunk, start=1):
                    if not isinstance(turn, dict):
                        raise RuntimeError(f"{batch_key} contains a non-object turn")
                    raw_turn_id = str(turn["dia_id"])
                    speaker = str(turn.get("speaker", "speaker"))
                    content = f"{speaker}: {turn['text']}"
                    turn_ids[local_index] = raw_turn_id
                    sources.append(
                        {
                            "node_id": local_index,
                            "turn_key": f"{raw_turn_id} ({speaker})",
                            "session_id": f"locomo-{sample_index}-{raw_session_id}",
                            "scope": "universal",
                            "content": content,
                            "content_hash": hashlib.sha256(content.encode()).hexdigest(),
                            "at_ms": start_ms + (offset + local_index - 1) * 60_000,
                        }
                    )

                previews = preview_parts(
                    args.binary,
                    sources,
                    args.timeout_secs,
                    args.transient_retries,
                )
                for part_key, preview in previews:
                    if preview is None:
                        part_sources = sources
                        for direction in part_key:
                            midpoint = len(part_sources) // 2
                            part_sources = (
                                part_sources[:midpoint]
                                if direction == "a"
                                else part_sources[midpoint:]
                            )
                        if len(part_sources) != 1:
                            raise RuntimeError(
                                f"invalid skipped extraction partition {part_key!r}"
                            )
                        source = part_sources[0]
                        state["skipped_sources"].append(
                            {
                                "batch_key": batch_key,
                                "part_key": part_key,
                                "source_turn_id": turn_ids[int(source["node_id"])],
                                "content_hash": source["content_hash"],
                                "reason": "schema-reject",
                            }
                        )
                        continue
                    profile = preview["profile"]
                    if profile.get("model_id") != MODEL:
                        raise RuntimeError(
                            f"artifact extractor must be {MODEL}, got {profile.get('model_id')}"
                        )
                    if state["profile"] is None:
                        state["profile"] = profile
                    elif state["profile"] != profile:
                        raise RuntimeError("extractor profile changed during artifact generation")

                    id_map: dict[str, str] = {}
                    extraction = preview["extraction"]
                    part_component = f"-p{part_key}" if part_key else ""
                    for item in extraction["items"]:
                        local_id = str(item["item_local_id"])
                        record_id = (
                            f"locomo-{sample_index}-{raw_session_id}-c{chunk_index}"
                            f"{part_component}-{local_id}"
                        )
                        id_map[local_id] = record_id
                        state["records"].append(
                            {
                                "id": record_id,
                                "source_session_id": f"locomo-{sample_index}-{raw_session_id}",
                                "kind": item["kind"],
                                "content": item["content"],
                                "source_turn_ids": [
                                    turn_ids[int(source["node_id"])] for source in item["sources"]
                                ],
                                "entity_tags": item.get("entity_tags", []),
                                "valid_from_ms": item.get("valid_from_ms"),
                                "valid_until_ms": item.get("valid_until_ms"),
                            }
                        )
                    for relation in extraction["relations"]:
                        state["relations"].append(
                            {
                                "from": id_map[relation["from_item_local_id"]],
                                "to": id_map[relation["to_item_local_id"]],
                                "kind": relation["relation_type"],
                            }
                        )

                completed_batches.add(batch_key)
                state["completed_batches"] = sorted(completed_batches)
                checkpoint(state_path, state)
                processed_now += 1
                print(
                    f"{batch_key}: records={len(state['records'])} "
                    f"relations={len(state['relations'])}",
                    flush=True,
                )
                if args.max_batches is not None and processed_now >= args.max_batches:
                    return 0

    profile = state.get("profile")
    if not profile:
        raise RuntimeError("dataset produced no extraction batches")
    validate_final_records(state["records"])
    artifact = {
        "schema_version": 1,
        "dataset_fnv1a64": state["dataset_fnv1a64"],
        "extractor_model": profile["model_id"],
        "extractor_digest": ollama_digest(args.ollama_base_url, MODEL),
        "prompt_version": (
            f"extract-v{profile['prompt_version']}-schema{profile['schema_version']}"
            f"-norm{profile['normalization_version']}-relations{profile['relation_policy_version']}"
        ),
        "records": state["records"],
        "relations": state["relations"],
    }
    checkpoint(args.output, artifact)
    print(
        f"wrote {args.output}: records={len(artifact['records'])} "
        f"relations={len(artifact['relations'])}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
