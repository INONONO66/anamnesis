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
BATCH_KEY_RE = re.compile(r"^(\d+):(session_\d+):(\d+)$")
DEFAULT_MODEL = "qwen3.6:35b-a3b"
SOURCE_SURFACE_VERSION = "locomo-caption-v2"
BATCH_TURNS = 10


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


def normalized_scalar(value: Any) -> str | None:
    if isinstance(value, str):
        normalized = value.strip()
    elif isinstance(value, bool):
        normalized = "true" if value else "false"
    elif isinstance(value, (int, float)):
        normalized = str(value)
    else:
        return None
    return normalized or None


def product_turn_surface(turn: dict[str, Any]) -> tuple[str, str]:
    speaker = normalized_scalar(turn.get("speaker")) or "unknown"
    content = normalized_scalar(turn.get("text")) or ""
    caption = normalized_scalar(turn.get("blip_caption"))
    if caption:
        if content:
            content += "\n"
        content += f"{speaker} shared {caption}."
    return speaker, content


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
    """Mirror the product worker's deterministic validation-failure isolation."""
    try:
        return [
            (
                part_key,
                preview_batch(binary, sources, timeout, transient_retries),
            )
        ]
    except RuntimeError as error:
        detail = str(error)
        validation_failure = (
            "schema-reject" in detail
            or "extraction output validation failed:" in detail
        )
        if not validation_failure:
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


def bounded_record_id(value: str) -> str:
    if len(value) <= 64:
        return value
    digest = hashlib.sha256(value.encode()).hexdigest()[:16]
    return f"{value[:47]}-{digest}"


def normalize_record_ids(state: dict[str, Any]) -> None:
    id_map: dict[str, str] = {}
    bounded_ids: set[str] = set()
    for record in state["records"]:
        old_id = record["id"]
        new_id = bounded_record_id(old_id)
        if new_id in bounded_ids:
            raise RuntimeError(f"bounded artifact record id collision: {new_id!r}")
        bounded_ids.add(new_id)
        id_map[old_id] = new_id
        record["id"] = new_id
    for relation in state["relations"]:
        try:
            relation["from"] = id_map[relation["from"]]
            relation["to"] = id_map[relation["to"]]
        except KeyError as error:
            raise RuntimeError("artifact relation references an unknown record") from error


def rebuild_batches(state: dict[str, Any], batch_keys: list[str]) -> None:
    completed = set(state["completed_batches"])
    batch_record_ids = state.setdefault("batch_record_ids", {})
    if not isinstance(batch_record_ids, dict):
        raise RuntimeError("checkpoint batch_record_ids must be an object")
    for batch_key in batch_keys:
        matched = BATCH_KEY_RE.fullmatch(batch_key)
        if matched is None:
            raise RuntimeError(f"invalid rebuild batch key: {batch_key!r}")
        if batch_key not in completed:
            raise RuntimeError(f"rebuild batch is not completed: {batch_key!r}")
        sample_index, session_id, chunk_index = matched.groups()
        record_prefix = f"locomo-{sample_index}-{session_id}-c{chunk_index}-"
        recorded_ids = batch_record_ids.get(batch_key)
        if recorded_ids is not None:
            if not isinstance(recorded_ids, list) or not all(
                isinstance(record_id, str) for record_id in recorded_ids
            ):
                raise RuntimeError(
                    f"rebuild batch {batch_key!r} has an invalid record-id ledger"
                )
            removed_ids = set(recorded_ids)
            existing_ids = {record["id"] for record in state["records"]}
            missing_ids = removed_ids.difference(existing_ids)
            if missing_ids:
                raise RuntimeError(
                    f"rebuild batch {batch_key!r} is missing recorded ids: "
                    f"{sorted(missing_ids)!r}"
                )
        else:
            # Legacy checkpoints predate the per-batch ledger. Prefix matching
            # is safe only when it proves that at least one record belongs to
            # the completed batch; bounded ids may have truncated the prefix.
            removed_ids = {
                record["id"]
                for record in state["records"]
                if record["id"].startswith(record_prefix)
            }
            if not removed_ids:
                raise RuntimeError(
                    f"rebuild batch {batch_key!r} matched no records; "
                    "record ids may have been truncated"
                )
        state["records"] = [
            record for record in state["records"] if record["id"] not in removed_ids
        ]
        state["relations"] = [
            relation
            for relation in state["relations"]
            if relation["from"] not in removed_ids and relation["to"] not in removed_ids
        ]
        state["skipped_sources"] = [
            source
            for source in state["skipped_sources"]
            if source.get("batch_key") != batch_key
        ]
        completed.remove(batch_key)
        batch_record_ids.pop(batch_key, None)
        print(
            f"rebuilding {batch_key}: removed_records={len(removed_ids)}",
            flush=True,
        )
    state["completed_batches"] = sorted(completed)


def product_source_surfaces(
    samples: list[Any],
) -> dict[tuple[str, str], str]:
    surfaces: dict[tuple[str, str], str] = {}
    for sample_index, sample in enumerate(samples):
        if not isinstance(sample, dict):
            continue
        for session_id in (key for key in sample if SESSION_RE.match(key)):
            turns = sample.get(session_id)
            if not isinstance(turns, list):
                continue
            source_session_id = f"locomo-{sample_index}-{session_id}"
            for turn in turns:
                if not isinstance(turn, dict):
                    continue
                turn_id = normalized_scalar(turn.get("dia_id"))
                speaker, content = product_turn_surface(turn)
                if turn_id is None or not content.strip():
                    continue
                key = (source_session_id, turn_id)
                if key in surfaces:
                    raise RuntimeError(
                        f"duplicate product source turn {source_session_id!r}/{turn_id!r}"
                    )
                surfaces[key] = f"{speaker}: {content}"
    return surfaces


def validate_final_records(
    records: list[dict[str, Any]],
    source_surfaces: dict[tuple[str, str], str],
) -> None:
    for record in records:
        record_id = record.get("id")
        if not isinstance(record_id, str) or not record_id or len(record_id) > 64:
            raise RuntimeError("artifact record requires a non-empty id of at most 64 characters")
        source_session_id = record.get("source_session_id")
        if not isinstance(source_session_id, str) or not source_session_id:
            raise RuntimeError(
                f"artifact record {record_id!r} requires a source session id"
            )
        required_text = (
            "content",
            "subject",
            "relation",
            "object",
            "evidence_object",
            "evidence_span",
            "evidence_source_turn_id",
        )
        values: dict[str, str] = {}
        for field in required_text:
            value = record.get(field)
            if not isinstance(value, str) or not value.strip():
                raise RuntimeError(
                    f"artifact record {record_id!r} is missing non-empty {field}"
                )
            values[field] = value
        expected_content = " ".join(
            values[field].strip() for field in ("subject", "relation", "object")
        )
        if values["content"] != expected_content:
            raise RuntimeError(
                f"artifact record {record_id!r} content does not match grounded S/R/O"
            )
        if values["evidence_object"] not in values["evidence_span"]:
            raise RuntimeError(
                f"artifact record {record_id!r} evidence object is not verbatim "
                "in its evidence span"
            )
        if "_" in values["relation"]:
            raise RuntimeError(
                f"artifact record {record_id!r} relation is not natural language"
            )
        first_person = {
            "i",
            "me",
            "my",
            "mine",
            "myself",
            "we",
            "us",
            "our",
            "ours",
            "ourselves",
        }
        unresolved_subject = first_person | {
            "you",
            "your",
            "yours",
            "yourself",
            "yourselves",
            "he",
            "him",
            "his",
            "himself",
            "she",
            "her",
            "hers",
            "herself",
            "they",
            "them",
            "their",
            "theirs",
            "themselves",
            "it",
            "its",
            "itself",
        }
        subject_tokens = {
            token.lower()
            for token in re.split(r"[^A-Za-z0-9]+", values["subject"])
            if token
        }
        object_tokens = {
            token.lower()
            for token in re.split(r"[^A-Za-z0-9]+", values["object"])
            if token
        }
        if (
            subject_tokens.intersection(unresolved_subject)
            or object_tokens.intersection(first_person)
        ):
            raise RuntimeError(
                f"artifact record {record_id!r} retains an unresolved canonical pronoun"
            )
        source_turn_ids = record.get("source_turn_ids")
        if (
            not isinstance(source_turn_ids, list)
            or not source_turn_ids
            or values["evidence_source_turn_id"] not in source_turn_ids
        ):
            raise RuntimeError(
                f"artifact record {record_id!r} has an invalid evidence source"
            )
        surface = source_surfaces.get(
            (source_session_id, values["evidence_source_turn_id"])
        )
        if surface is None:
            raise RuntimeError(
                f"artifact record {record_id!r} cites an unknown source turn"
            )
        if values["evidence_span"] not in surface:
            raise RuntimeError(
                f"artifact record {record_id!r} evidence span is not verbatim "
                "in the declared LoCoMo source surface"
            )
        fields = [record_id, *values.values()]
        fields.extend(str(tag) for tag in record.get("entity_tags", []))
        for value in fields:
            if any(ord(char) < 32 and char not in "\n\t" for char in value):
                raise RuntimeError(
                    f"artifact record {record_id!r} contains a control character"
                )


def batch_shard_index(batch_key: str, shard_count: int) -> int:
    digest = hashlib.sha256(batch_key.encode()).digest()
    return int.from_bytes(digest[:8], "big") % shard_count


def expected_batch_keys(samples: list[Any]) -> set[str]:
    keys: set[str] = set()
    for sample_index, sample in enumerate(samples):
        if not isinstance(sample, dict):
            raise RuntimeError(f"sample {sample_index} is not an object")
        session_keys = sorted(
            (key for key in sample if SESSION_RE.match(key)),
            key=lambda key: int(SESSION_RE.match(key).group(1)),  # type: ignore[union-attr]
        )
        for raw_session_id in session_keys:
            raw_turns = sample[raw_session_id]
            if not isinstance(raw_turns, list):
                raise RuntimeError(
                    f"{sample_index}:{raw_session_id} contains a non-array session"
                )
            usable_turns = sum(
                1
                for turn in raw_turns
                if isinstance(turn, dict) and product_turn_surface(turn)[1].strip()
            )
            for chunk_index in range(0, usable_turns, BATCH_TURNS):
                keys.add(
                    f"{sample_index}:{raw_session_id}:{chunk_index // BATCH_TURNS}"
                )
    return keys


def prompt_profile_version(profile: dict[str, Any]) -> str:
    return (
        f"extract-v{profile['prompt_version']}-schema{profile['schema_version']}"
        f"-norm{profile['normalization_version']}"
        f"-relations{profile['relation_policy_version']}"
        f"-source-{SOURCE_SURFACE_VERSION}"
    )


def merge_shard_states(
    state_paths: list[Path],
    output: Path,
    dataset_bytes: bytes,
    samples: list[Any],
) -> int:
    if not state_paths:
        raise RuntimeError("at least one --merge-shard-state is required")
    fingerprint = fnv1a64(dataset_bytes)
    states = [json.loads(path.read_text()) for path in state_paths]
    first = states[0]
    profile = first.get("profile")
    extractor_digest = first.get("extractor_digest")
    shard_count = first.get("batch_shard_count")
    if not isinstance(profile, dict) or not extractor_digest:
        raise RuntimeError("shard state is missing extractor identity")
    if not isinstance(shard_count, int) or shard_count <= 1:
        raise RuntimeError("shard state must declare batch_shard_count greater than one")

    shard_indices: set[int] = set()
    completed_batches: set[str] = set()
    records_by_id: dict[str, dict[str, Any]] = {}
    relations_by_key: dict[tuple[str, str, str], dict[str, Any]] = {}
    for path, state in zip(state_paths, states):
        if state.get("dataset_fnv1a64") != fingerprint:
            raise RuntimeError(f"shard dataset fingerprint differs: {path}")
        if state.get("source_surface_version") != SOURCE_SURFACE_VERSION:
            raise RuntimeError(f"shard source surface differs: {path}")
        if state.get("profile") != profile or state.get("extractor_digest") != extractor_digest:
            raise RuntimeError(f"shard extractor profile differs: {path}")
        if state.get("batch_shard_count") != shard_count:
            raise RuntimeError(f"shard count differs: {path}")
        shard_index = state.get("batch_shard_index")
        if not isinstance(shard_index, int) or not 0 <= shard_index < shard_count:
            raise RuntimeError(f"shard index is invalid: {path}")
        if shard_index in shard_indices:
            raise RuntimeError(f"duplicate shard index {shard_index}")
        shard_indices.add(shard_index)
        state_batches = state.get("completed_batches")
        if not isinstance(state_batches, list) or not all(
            isinstance(batch_key, str) for batch_key in state_batches
        ):
            raise RuntimeError(f"shard completed batch ledger is invalid: {path}")
        misplaced = [
            batch_key
            for batch_key in state_batches
            if batch_shard_index(batch_key, shard_count) != shard_index
        ]
        if misplaced:
            raise RuntimeError(
                f"shard contains batches assigned elsewhere: {misplaced[:8]!r}"
            )
        duplicated = completed_batches.intersection(state_batches)
        if duplicated:
            raise RuntimeError(
                f"shards repeat completed batches: {sorted(duplicated)[:8]!r}"
            )
        completed_batches.update(state_batches)
        for record in state.get("records", []):
            record_id = record.get("id")
            if not isinstance(record_id, str):
                raise RuntimeError(f"shard record has no string id: {path}")
            prior = records_by_id.get(record_id)
            if prior is not None and prior != record:
                raise RuntimeError(f"shards disagree about record {record_id!r}")
            records_by_id[record_id] = record
        for relation in state.get("relations", []):
            key = (relation.get("from"), relation.get("to"), relation.get("kind"))
            if not all(isinstance(value, str) for value in key):
                raise RuntimeError(f"shard relation is invalid: {path}")
            relations_by_key[key] = relation

    if shard_indices != set(range(shard_count)):
        raise RuntimeError(
            f"shard indices are incomplete: got {sorted(shard_indices)!r}, "
            f"expected 0..{shard_count - 1}"
        )
    expected = expected_batch_keys(samples)
    if completed_batches != expected:
        missing = sorted(expected.difference(completed_batches))
        extra = sorted(completed_batches.difference(expected))
        raise RuntimeError(
            f"shard batch coverage differs: missing={missing[:8]!r} extra={extra[:8]!r}"
        )

    records = sorted(records_by_id.values(), key=lambda record: record["id"])
    relations = sorted(
        relations_by_key.values(),
        key=lambda relation: (relation["from"], relation["to"], relation["kind"]),
    )
    record_ids = set(records_by_id)
    for relation in relations:
        if relation["from"] not in record_ids or relation["to"] not in record_ids:
            raise RuntimeError("merged relation references an unknown record")
    validate_final_records(records, product_source_surfaces(samples))
    artifact = {
        "schema_version": 3,
        "dataset_fnv1a64": fingerprint,
        "extractor_model": profile["model_id"],
        "extractor_digest": extractor_digest,
        "prompt_version": prompt_profile_version(profile),
        "records": records,
        "relations": relations,
    }
    checkpoint(output, artifact)
    print(
        f"wrote {output}: records={len(records)} relations={len(relations)} "
        f"merged_shards={shard_count}"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/anamnesis"))
    parser.add_argument("--ollama-base-url", default="http://127.0.0.1:11434")
    parser.add_argument("--extractor-model", default=DEFAULT_MODEL)
    parser.add_argument("--extractor-digest")
    parser.add_argument("--timeout-secs", type=int, default=300)
    parser.add_argument("--transient-retries", type=int, default=2)
    parser.add_argument("--max-batches", type=int)
    parser.add_argument("--batch-shard-count", type=int, default=1)
    parser.add_argument("--batch-shard-index", type=int, default=0)
    parser.add_argument("--merge-shard-state", action="append", type=Path, default=[])
    parser.add_argument(
        "--rebuild-batch",
        action="append",
        default=[],
        help="remove and regenerate one completed sample:session_N:chunk batch",
    )
    args = parser.parse_args()
    if args.timeout_secs <= 0 or args.transient_retries < 0:
        parser.error("timeout must be positive and transient retries must be non-negative")
    if args.batch_shard_count <= 0 or not 0 <= args.batch_shard_index < args.batch_shard_count:
        parser.error("batch shard index must be within a positive shard count")
    if args.merge_shard_state and (
        args.max_batches is not None
        or args.rebuild_batch
        or args.batch_shard_count != 1
        or args.batch_shard_index != 0
    ):
        parser.error(
            "--merge-shard-state cannot be combined with generation, rebuild, or shard flags"
        )
    if args.extractor_digest is not None and re.fullmatch(
        r"[0-9a-f]{64}", args.extractor_digest
    ) is None:
        parser.error("--extractor-digest must be 64 lowercase hexadecimal characters")

    dataset_bytes = args.dataset.read_bytes()
    samples = json.loads(dataset_bytes)
    if not isinstance(samples, list):
        raise RuntimeError("LoCoMo dataset root must be an array")
    if args.merge_shard_state:
        return merge_shard_states(
            args.merge_shard_state,
            args.output,
            dataset_bytes,
            samples,
        )
    state_path = args.output.with_suffix(args.output.suffix + ".state.json")
    state: dict[str, Any]
    if state_path.exists():
        state = json.loads(state_path.read_text())
        if state.get("dataset_fnv1a64") != fnv1a64(dataset_bytes):
            raise RuntimeError("checkpoint dataset fingerprint differs")
        if state.get("source_surface_version") != SOURCE_SURFACE_VERSION:
            raise RuntimeError(
                "checkpoint source surface differs; choose a new output path"
            )
        checkpoint_shard_count = state.get("batch_shard_count")
        checkpoint_shard_index = state.get("batch_shard_index")
        if checkpoint_shard_count is not None and (
            checkpoint_shard_count != args.batch_shard_count
            or checkpoint_shard_index != args.batch_shard_index
        ):
            raise RuntimeError("checkpoint batch shard differs")
    else:
        state = {
            "dataset_fnv1a64": fnv1a64(dataset_bytes),
            "source_surface_version": SOURCE_SURFACE_VERSION,
            "completed_batches": [],
            "batch_record_ids": {},
            "records": [],
            "relations": [],
            "skipped_sources": [],
            "profile": None,
            "extractor_digest": args.extractor_digest,
        }

    state["batch_shard_count"] = args.batch_shard_count
    state["batch_shard_index"] = args.batch_shard_index

    state.setdefault("skipped_sources", [])
    state.setdefault("batch_record_ids", {})
    state_profile = state.get("profile")
    if state_profile is not None and state_profile.get("model_id") != args.extractor_model:
        raise RuntimeError("checkpoint extractor model differs")
    checkpoint_digest = state.get("extractor_digest")
    if (
        checkpoint_digest is not None
        and args.extractor_digest is not None
        and checkpoint_digest != args.extractor_digest
    ):
        raise RuntimeError("checkpoint extractor digest differs")
    if checkpoint_digest is None and args.extractor_digest is not None:
        state["extractor_digest"] = args.extractor_digest
    if args.rebuild_batch:
        rebuild_batches(state, args.rebuild_batch)
        checkpoint(state_path, state)
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
            raw_turns = sample[raw_session_id]
            if not isinstance(raw_turns, list) or not raw_turns:
                continue
            turns: list[tuple[dict[str, Any], str, str]] = []
            for turn in raw_turns:
                if not isinstance(turn, dict):
                    raise RuntimeError(
                        f"{sample_index}:{raw_session_id} contains a non-object turn"
                    )
                speaker, turn_content = product_turn_surface(turn)
                if turn_content.strip():
                    turns.append((turn, speaker, turn_content))
            if not turns:
                continue
            session_number = int(SESSION_RE.match(raw_session_id).group(1))  # type: ignore[union-attr]
            start_ms = session_timestamp_ms(
                sample.get(f"{raw_session_id}_date_time"),
                sample_index,
                session_number,
            )
            for chunk_index, offset in enumerate(
                range(0, len(turns), BATCH_TURNS)
            ):
                batch_key = f"{sample_index}:{raw_session_id}:{chunk_index}"
                if (
                    batch_shard_index(batch_key, args.batch_shard_count)
                    != args.batch_shard_index
                ):
                    continue
                if batch_key in completed_batches:
                    continue
                chunk = turns[offset : offset + BATCH_TURNS]
                sources = []
                turn_ids: dict[int, str] = {}
                for local_index, (turn, speaker, turn_content) in enumerate(
                    chunk, start=1
                ):
                    raw_turn_id = normalized_scalar(turn.get("dia_id"))
                    if raw_turn_id is None:
                        raise RuntimeError(f"{batch_key} contains a turn without dia_id")
                    content = f"{speaker}: {turn_content}"
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
                generated_record_ids: list[str] = []
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
                    if profile.get("model_id") != args.extractor_model:
                        raise RuntimeError(
                            "artifact extractor must be "
                            f"{args.extractor_model}, got {profile.get('model_id')}"
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
                        evidence_node_id = item.get("evidence_source_node_id")
                        if (
                            not isinstance(evidence_node_id, int)
                            or isinstance(evidence_node_id, bool)
                            or evidence_node_id not in turn_ids
                        ):
                            raise RuntimeError(
                                f"{batch_key} item {local_id!r} has no valid "
                                "evidence_source_node_id"
                            )
                        record_id = bounded_record_id(
                            f"locomo-{sample_index}-{raw_session_id}-c{chunk_index}"
                            f"{part_component}-{local_id}"
                        )
                        id_map[local_id] = record_id
                        generated_record_ids.append(record_id)
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
                                "subject": item.get("subject"),
                                "relation": item.get("relation"),
                                "object": item.get("object"),
                                "evidence_object": item.get("evidence_object"),
                                "evidence_span": item.get("evidence_span"),
                                "evidence_source_turn_id": turn_ids[evidence_node_id],
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
                state["batch_record_ids"][batch_key] = generated_record_ids
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
    extractor_digest = state.get("extractor_digest") or args.extractor_digest
    if not extractor_digest:
        if profile.get("provider_id") != "ollama":
            raise RuntimeError(
                "--extractor-digest is required for a non-Ollama extraction provider"
            )
        extractor_digest = ollama_digest(args.ollama_base_url, args.extractor_model)
    state["extractor_digest"] = extractor_digest
    normalize_record_ids(state)
    validate_final_records(state["records"], product_source_surfaces(samples))
    checkpoint(state_path, state)
    artifact = {
        "schema_version": 3,
        "dataset_fnv1a64": state["dataset_fnv1a64"],
        "extractor_model": profile["model_id"],
        "extractor_digest": extractor_digest,
        "prompt_version": prompt_profile_version(profile),
        "records": state["records"],
        "relations": state["relations"],
    }
    if args.batch_shard_count > 1:
        print(
            f"completed shard {args.batch_shard_index}/{args.batch_shard_count}: "
            f"records={len(artifact['records'])} relations={len(artifact['relations'])}; "
            "merge shard state files to create the final artifact"
        )
        return 0
    checkpoint(args.output, artifact)
    print(
        f"wrote {args.output}: records={len(artifact['records'])} "
        f"relations={len(artifact['relations'])}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
