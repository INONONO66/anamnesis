#!/usr/bin/env sh
# Verify the exact release-version surfaces without touching unrelated historical
# lockfile or changelog entries. Requires only POSIX sh and Python 3.

set -u

if [ "$#" -ne 1 ]; then
    printf 'usage: %s EXPECTED_VERSION (for example, 0.19.0)\n' "$0" >&2
    exit 2
fi

HERE=$(CDPATH= cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd "$HERE/../.." && pwd)

exec python3 - "$REPO_ROOT" "$1" <<'PY'
import json
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
expected_full = sys.argv[2]

if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", expected_full):
    print(
        f"ERROR expected version must be a full numeric version such as 0.19.0; got {expected_full!r}",
        file=sys.stderr,
    )
    sys.exit(2)

expected_cargo = expected_full.rsplit(".", 1)[0]
failures = []


def fail(surface, detail):
    failures.append(f"FAIL {surface}: {detail}")


def read_text(relative_path, surface):
    try:
        return (root / relative_path).read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        fail(surface, f"cannot read {relative_path}: {error}")
        return None


def check_single_value(surface, field, values, expected):
    if not values:
        fail(surface, f"missing {field}; expected {expected!r}")
        return
    if len(values) != 1:
        found = ", ".join(repr(value) for value in values)
        fail(surface, f"duplicate/conflicting {field} entries: {found}; expected one {expected!r}")
        return
    if values[0] != expected:
        fail(surface, f"{field} is {values[0]!r}; expected {expected!r}")


STRING_ASSIGNMENT = re.compile(
    r'''^\s*(name|version|source)\s*=\s*(?:"((?:[^"\\]|\\.)*)"|'([^']*)')\s*(?:#.*)?$'''
)
PACKAGE_HEADER = re.compile(r"^\s*\[package\]\s*(?:#.*)?$")
TABLE_HEADER = re.compile(r"^\s*\[\[?[^\]]+\]\]?\s*(?:#.*)?$")
PACKAGE_ARRAY_HEADER = re.compile(r"^\s*\[\[package\]\]\s*(?:#.*)?$")


def string_assignment(line):
    match = STRING_ASSIGNMENT.match(line)
    if not match:
        return None
    key, double_quoted, single_quoted = match.groups()
    if double_quoted is not None:
        try:
            value = json.loads(f'"{double_quoted}"')
        except json.JSONDecodeError:
            return None
    else:
        value = single_quoted
    return key, value


def package_manifest_fields(relative_path, surface):
    text = read_text(relative_path, surface)
    if text is None:
        return None

    in_package = False
    fields = {"name": [], "version": []}
    for line in text.splitlines():
        if PACKAGE_HEADER.match(line):
            in_package = True
            continue
        if TABLE_HEADER.match(line):
            in_package = False
            continue
        if in_package:
            assignment = string_assignment(line)
            if assignment and assignment[0] in fields:
                fields[assignment[0]].append(assignment[1])
    return fields


def check_manifest(relative_path, surface, package_name):
    fields = package_manifest_fields(relative_path, surface)
    if fields is None:
        return
    check_single_value(surface, "[package].name", fields["name"], package_name)
    check_single_value(surface, "[package].version", fields["version"], expected_full)


def lock_entries():
    surface = "Cargo.lock"
    text = read_text("Cargo.lock", surface)
    if text is None:
        return None

    entries = []
    current = None
    for line_number, line in enumerate(text.splitlines(), start=1):
        if PACKAGE_ARRAY_HEADER.match(line):
            if current is not None:
                entries.append(current)
            current = {"line": line_number, "fields": {"name": [], "version": [], "source": []}}
            continue
        if TABLE_HEADER.match(line):
            if current is not None:
                entries.append(current)
                current = None
            continue
        if current is not None:
            assignment = string_assignment(line)
            if assignment and assignment[0] in current["fields"]:
                current["fields"][assignment[0]].append(assignment[1])
    if current is not None:
        entries.append(current)
    return entries


def check_lock_package(entries, package_name):
    surface = f"Cargo.lock [{package_name}]"
    matching = [entry for entry in entries if package_name in entry["fields"]["name"]]
    if not matching:
        fail(surface, "missing named workspace [[package]] entry")
        return
    if len(matching) != 1:
        locations = ", ".join(str(entry["line"]) for entry in matching)
        fail(surface, f"duplicate/conflicting named package entries at lines {locations}")
        return

    fields = matching[0]["fields"]
    check_single_value(surface, "name", fields["name"], package_name)
    if fields["source"]:
        fail(surface, f"entry is not a workspace package (source is {fields['source'][0]!r})")
    check_single_value(surface, "version", fields["version"], expected_full)


class DuplicateJsonKey(ValueError):
    pass


def reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJsonKey(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def load_json(relative_path, surface):
    text = read_text(relative_path, surface)
    if text is None:
        return None
    try:
        parsed = json.loads(text, object_pairs_hook=reject_duplicate_json_keys)
    except (json.JSONDecodeError, DuplicateJsonKey) as error:
        fail(surface, f"invalid or duplicate-key JSON in {relative_path}: {error}")
        return None
    if not isinstance(parsed, dict):
        fail(surface, f"top-level JSON must be an object in {relative_path}")
        return None
    return parsed


def check_json_package(relative_path, surface, package_name):
    parsed = load_json(relative_path, surface)
    if parsed is None:
        return
    name = parsed.get("name")
    if not isinstance(name, str):
        fail(surface, f"missing string name; expected {package_name!r}")
    elif name != package_name:
        fail(surface, f"name is {name!r}; expected {package_name!r}")
    version = parsed.get("version")
    if not isinstance(version, str):
        fail(surface, f"missing string version; expected {expected_full!r}")
    elif version != expected_full:
        fail(surface, f"version is {version!r}; expected {expected_full!r}")


def check_codex_mcp():
    surface = "plugin/.codex-mcp.json"
    parsed = load_json("plugin/.codex-mcp.json", surface)
    if parsed is None:
        return
    servers = parsed.get("mcpServers")
    if not isinstance(servers, dict):
        fail(surface, "missing object mcpServers.anamnesis entry")
        return
    server = servers.get("anamnesis")
    if not isinstance(server, dict):
        fail(surface, "missing object mcpServers.anamnesis entry")
        return
    arguments = server.get("args")
    if not isinstance(arguments, list) or not all(isinstance(argument, str) for argument in arguments):
        fail(surface, "mcpServers.anamnesis.args must be an array of strings containing one package pin")
        return
    pins = [argument for argument in arguments if argument.startswith("anamnesis-mcp@")]
    if not pins:
        fail(surface, f"missing anamnesis-mcp package pin; expected anamnesis-mcp@{expected_full}")
        return
    if len(pins) != 1:
        fail(surface, f"duplicate/conflicting anamnesis-mcp package pins: {', '.join(repr(pin) for pin in pins)}")
        return
    expected_pin = f"anamnesis-mcp@{expected_full}"
    if pins[0] != expected_pin:
        fail(surface, f"package pin is {pins[0]!r}; expected {expected_pin!r}")


def check_version_file():
    surface = "plugin/bin/VERSION"
    text = read_text("plugin/bin/VERSION", surface)
    if text is None:
        return
    permitted = {expected_full, f"{expected_full}\n", f"{expected_full}\r\n"}
    if text not in permitted:
        fail(
            surface,
            f"must contain only {expected_full!r} with at most one final newline; found {text!r}",
        )


def check_readme_example():
    surface = "README.md published anamnesis-engine embed example"
    text = read_text("README.md", surface)
    if text is None:
        return
    declaration = re.compile(r'^\s*anamnesis-engine\s*=\s*(.*)$')
    matching_lines = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        match = declaration.match(line)
        if match:
            matching_lines.append((line_number, match.group(1)))
    if not matching_lines:
        fail(surface, "missing anamnesis-engine dependency declaration with the embed feature")
        return
    if len(matching_lines) != 1:
        locations = ", ".join(str(line_number) for line_number, _ in matching_lines)
        fail(surface, f"duplicate/conflicting anamnesis-engine dependency declarations at lines {locations}")
        return

    line_number, value = matching_lines[0]
    example = re.fullmatch(
        r'\{\s*version\s*=\s*"([^"]+)"\s*,\s*features\s*=\s*\[\s*"embed"\s*\]\s*\}\s*(?:#.*)?',
        value,
    )
    if not example:
        fail(
            surface,
            f"line {line_number} must be anamnesis-engine = {{ version = \"{expected_cargo}\", features = [\"embed\"] }}",
        )
        return
    actual_requirement = example.group(1)
    if actual_requirement != expected_cargo:
        fail(surface, f"line {line_number} version requirement is {actual_requirement!r}; expected {expected_cargo!r}")


check_manifest("crates/anamnesis/Cargo.toml", "crates/anamnesis/Cargo.toml", "anamnesis-engine")
check_manifest("crates/anamnesis-mcp/Cargo.toml", "crates/anamnesis-mcp/Cargo.toml", "anamnesis-mcp")

entries = lock_entries()
if entries is not None:
    check_lock_package(entries, "anamnesis-engine")
    check_lock_package(entries, "anamnesis-mcp")

check_json_package("npm/anamnesis-mcp/package.json", "npm/anamnesis-mcp/package.json", "anamnesis-mcp")
check_json_package("plugin/.claude-plugin/plugin.json", "plugin/.claude-plugin/plugin.json", "anamnesis")
check_json_package("plugin/.codex-plugin/plugin.json", "plugin/.codex-plugin/plugin.json", "anamnesis")
check_codex_mcp()
check_version_file()
check_readme_example()

if failures:
    for message in failures:
        print(message, file=sys.stderr)
    print(
        f"{len(failures)} version consistency check(s) failed; expected {expected_full} (Cargo requirement {expected_cargo}).",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"PASS versions: all 10 product surfaces match {expected_full} (Cargo requirement {expected_cargo}).")
PY
