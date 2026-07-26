#!/usr/bin/env python3
"""Prepare the frozen local Mem0 LoCoMo comparison checkout.

The upstream memory-benchmarks commit does not build or run against its pinned
Mem0 shape without a few dependency/provider repairs. This script applies only
those audited repairs and writes a manifest of the resulting files. It does not
change Mem0 prompts, extraction policy, benchmark questions, or ranking logic.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
from pathlib import Path


MEMORY_BENCHMARKS_REPOSITORY = "https://github.com/mem0ai/memory-benchmarks.git"
MEMORY_BENCHMARKS_SHA = "4b61c5d31b9c668a12b4f5e78064248a02c82d2b"
MEM0_SHA = "b357a5a1b03c299ec8229c268e63cfac0f7c6566"


def run(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        list(args),
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout.strip()


def replace_once(path: Path, old: str, new: str, marker: str) -> None:
    text = path.read_text()
    if marker in text:
        return
    if text.count(old) != 1:
        raise RuntimeError(f"unexpected upstream shape in {path}: missing {old!r}")
    path.write_text(text.replace(old, new))


def append_requirement(path: Path, requirement: str) -> None:
    lines = path.read_text().splitlines()
    if requirement not in lines:
        lines.append(requirement)
        path.write_text("\n".join(lines) + "\n")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare_checkout(target: Path, config_source: Path) -> None:
    if not target.exists():
        run("git", "clone", MEMORY_BENCHMARKS_REPOSITORY, str(target))
    if not (target / ".git").is_dir():
        raise RuntimeError(f"{target} is not a git checkout")

    origin = run("git", "remote", "get-url", "origin", cwd=target)
    if "github.com/mem0ai/memory-benchmarks" not in origin.removesuffix(".git"):
        raise RuntimeError(f"{target} has unexpected origin {origin!r}")
    current_head = run("git", "rev-parse", "HEAD", cwd=target)
    dirty = run("git", "status", "--porcelain", cwd=target)
    if current_head != MEMORY_BENCHMARKS_SHA and dirty:
        raise RuntimeError(
            f"{target} has local changes at {current_head}; refusing to change its checkout"
        )

    run("git", "fetch", "origin", MEMORY_BENCHMARKS_SHA, cwd=target)
    run("git", "checkout", "--detach", MEMORY_BENCHMARKS_SHA, cwd=target)
    head = run("git", "rev-parse", "HEAD", cwd=target)
    if head != MEMORY_BENCHMARKS_SHA:
        raise RuntimeError(f"unexpected memory-benchmarks HEAD {head}")

    requirements = target / "docker/mem0/requirements.txt"
    replace_once(
        requirements,
        "mem0ai @ git+https://github.com/mem0ai/mem0.git@feat/v3-pipeline",
        f"mem0ai @ git+https://github.com/mem0ai/mem0.git@{MEM0_SHA}",
        f"mem0.git@{MEM0_SHA}",
    )
    append_requirement(requirements, "ollama>=0.6.0")
    append_requirement(requirements, "fastembed>=0.3.1")

    dockerfile = target / "docker/mem0/Dockerfile"
    think_patch = """\
# Freeze the local Qwen lane to non-thinking generation. The pinned adapter
# does not expose Ollama's `think` request field through YAML configuration.
RUN python - <<'PY'
from pathlib import Path

path = Path("/usr/local/lib/python3.12/site-packages/mem0/llms/ollama.py")
source = path.read_text()
needle = "        response = self.client.chat(**params)\\n"
replacement = "        params[\\\"think\\\"] = False\\n\\n" + needle
if source.count(needle) != 1:
    raise SystemExit("unexpected pinned Mem0 Ollama adapter shape")
path.write_text(source.replace(needle, replacement))
PY

"""
    replace_once(
        dockerfile,
        "# History directory\n",
        think_patch + "# History directory\n",
        'params[\\"think\\"] = False',
    )

    server = target / "docker/mem0/main.py"
    old_search_filters = """\
    if req.user_id:
        params["user_id"] = req.user_id
    if req.agent_id:
        params["agent_id"] = req.agent_id
    if req.run_id:
        params["run_id"] = req.run_id
    if req.filters:
        params["filters"] = req.filters
"""
    new_search_filters = """\
    filters = dict(req.filters or {})
    if req.user_id:
        filters["user_id"] = req.user_id
    if req.agent_id:
        filters["agent_id"] = req.agent_id
    if req.run_id:
        filters["run_id"] = req.run_id
    if filters:
        params["filters"] = filters
"""
    replace_once(
        server,
        old_search_filters,
        new_search_filters,
        "filters = dict(req.filters or {})",
    )

    client = target / "benchmarks/common/mem0_client.py"
    replace_once(
        client,
        "        timeout: float = 300.0,\n",
        "        timeout: float = 1800.0,\n",
        "        timeout: float = 1800.0,",
    )

    config_target = target / "mem0-config.yaml"
    shutil.copyfile(config_source, config_target)

    patched = [
        requirements,
        dockerfile,
        server,
        client,
        config_target,
    ]
    manifest = {
        "schema_version": 1,
        "memory_benchmarks_repository": MEMORY_BENCHMARKS_REPOSITORY,
        "memory_benchmarks_sha": MEMORY_BENCHMARKS_SHA,
        "mem0_sha": MEM0_SHA,
        "repairs": {
            "ollama_dependency": "ollama>=0.6.0",
            "bm25_dependency": "fastembed>=0.3.1",
            "ollama_think": False,
            "oss_client_timeout_seconds": 1800,
            "search_identity_constraints": "filters",
        },
        "files": {
            str(path.relative_to(target)): sha256(path)
            for path in patched
        },
    }
    (target / "anamnesis-local-reproduction.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("target", type=Path)
    parser.add_argument(
        "--config",
        type=Path,
        default=Path("docs/07-quality-gates/configs/mem0-oss-qwen36.yaml"),
    )
    parser.add_argument(
        "--build",
        action="store_true",
        help="build and start the repaired local Mem0 service",
    )
    args = parser.parse_args()

    target = args.target.expanduser().resolve()
    config = args.config.expanduser().resolve()
    if not config.is_file():
        parser.error(f"config does not exist: {config}")
    prepare_checkout(target, config)
    print(f"prepared {target} at {MEMORY_BENCHMARKS_SHA}")
    print(f"manifest: {target / 'anamnesis-local-reproduction.json'}")
    if args.build:
        subprocess.run(
            ["docker", "compose", "up", "-d", "--build"],
            cwd=target,
            check=True,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
