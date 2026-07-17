#!/bin/sh
set -eu

HERE=$(CDPATH= cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd "$HERE/../.." && pwd)

exec python3 - "$REPO_ROOT" <<'PY'
from pathlib import Path
import re
import sys

root = Path(sys.argv[1])
workflow_dir = root / ".github" / "workflows"
workflows = sorted({*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")})
approved = {
    "actions/checkout": ("9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0", "v7.0.0"),
    "actions/upload-artifact": ("043fb46d1a93c77aae656e7c1c64a875d1fc6a0a", "v7.0.1"),
    "actions/download-artifact": ("3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c", "v8.0.1"),
    "actions/setup-node": ("820762786026740c76f36085b0efc47a31fe5020", "v7.0.0"),
}
uses_pattern = re.compile(
    r"^\s*-\s+uses:\s+(actions/[A-Za-z0-9_.-]+)@([0-9a-f]{40})\s+#\s+(v[0-9]+\.[0-9]+\.[0-9]+)\s*$"
)
errors = []
seen = set()

for workflow in workflows:
    for number, line in enumerate(workflow.read_text().splitlines(), start=1):
        if "actions/" not in line:
            continue
        match = uses_pattern.fullmatch(line)
        location = f"{workflow.relative_to(root)}:{number}"
        if match is None:
            errors.append(
                f"{location}: first-party actions must use canonical unquoted "
                "uses: actions/name@<40-hex> # vMAJOR.MINOR.PATCH syntax"
            )
            continue
        action, ref, comment = match.groups()
        if action not in approved:
            errors.append(f"{location}: unapproved first-party action {action}")
            continue
        expected_sha, expected_version = approved[action]
        seen.add(action)
        if ref != expected_sha:
            errors.append(f"{location}: {action} must use {expected_sha}")
        if comment != expected_version:
            errors.append(f"{location}: {action} must have comment # {expected_version}")

for action in approved:
    if action not in seen:
        errors.append(f"expected first-party action is absent: {action}")

dependabot = root / ".github" / "dependabot.yml"
expected_dependabot = """version: 2
updates:
  - package-ecosystem: github-actions
    directory: /
    schedule:
      interval: monthly
    open-pull-requests-limit: 5
"""
if not dependabot.is_file():
    errors.append(".github/dependabot.yml is missing")
elif dependabot.read_text() != expected_dependabot:
    errors.append(
        ".github/dependabot.yml must exactly match the reviewed root monthly "
        "github-actions configuration"
    )

if errors:
    print("Action pin verification failed:", file=sys.stderr)
    print("\n".join(f"- {error}" for error in errors), file=sys.stderr)
    sys.exit(1)
PY
