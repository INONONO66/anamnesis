#!/usr/bin/env sh
# Exercise release-tag validation against the repository's current lockstep version.
# Requires only POSIX sh and Python 3.

set -u

HERE=$(CDPATH= cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd "$HERE/../.." && pwd)
VERIFY_RELEASE_TAG="$HERE/verify_release_tag.sh"

version=$(python3 - "$REPO_ROOT/crates/anamnesis/Cargo.toml" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
in_package = False
versions = []
for line in text.splitlines():
    if re.match(r"^\s*\[package\]\s*(?:#.*)?$", line):
        in_package = True
        continue
    if re.match(r"^\s*\[\[?[^\]]+\]\]?\s*(?:#.*)?$", line):
        in_package = False
        continue
    if in_package:
        match = re.match(r'^\s*version\s*=\s*"([^"]+)"\s*(?:#.*)?$', line)
        if match:
            versions.append(match.group(1))

if len(versions) != 1:
    print("unable to derive exactly one [package].version from crates/anamnesis/Cargo.toml", file=sys.stderr)
    sys.exit(2)
print(versions[0])
PY
) || exit $?

mismatching_version=$(python3 - "$version" <<'PY'
import sys

major, minor, patch = sys.argv[1].split(".")
print(f"{major}.{minor}.{int(patch) + 1}")
PY
) || exit $?

failures=0
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1" >&2; failures=$((failures + 1)); }
expect_pass() {
    description=$1
    shift
    if "$@" >/dev/null 2>&1; then
        pass "$description"
    else
        fail "$description"
    fi
}
expect_fail() {
    description=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$description"
    else
        pass "$description"
    fi
}
verify_workflow_preflight() {
    python3 - "$REPO_ROOT/.github/workflows/release.yml" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
preflight = re.search(r"(?ms)^  preflight:\n(.*?)(?=^  [a-zA-Z0-9_-]+:\n|\Z)", text)
build = re.search(r"(?ms)^  build:\n(.*?)(?=^  [a-zA-Z0-9_-]+:\n|\Z)", text)
publish = re.search(r"(?ms)^  publish:\n(.*?)(?=^  [a-zA-Z0-9_-]+:\n|\Z)", text)
if preflight is None or build is None or publish is None:
    sys.exit("release workflow must define preflight, build, and publish jobs")
if "RELEASE_TAG: ${{ github.ref_name }}" not in preflight.group(1):
    sys.exit("preflight must pass github.ref_name through the environment")
if 'plugin/tests/verify_release_tag.sh "$RELEASE_TAG"' not in preflight.group(1):
    sys.exit("preflight must validate the quoted release-tag environment variable")
if 'verify_release_tag.sh "${{ github.ref_name }}"' in preflight.group(1):
    sys.exit("preflight must not interpolate github.ref_name into shell source")
if not re.search(r"(?m)^    needs: preflight\s*$", build.group(1)):
    sys.exit("build must depend on preflight")
if not re.search(r"(?m)^    needs: build\s*$", publish.group(1)):
    sys.exit("publish must depend on the preflight-gated build")
PY
}

expect_pass "current stable tag passes lockstep verification" "$VERIFY_RELEASE_TAG" "v$version"
expect_pass "release workflow blocks build on tag preflight" verify_workflow_preflight
expect_fail "missing tag argument is rejected" "$VERIFY_RELEASE_TAG"
expect_fail "multiple tag arguments are rejected" "$VERIFY_RELEASE_TAG" "v$version" "extra"
expect_fail "empty tag is rejected" "$VERIFY_RELEASE_TAG" ""
expect_fail "leading whitespace is rejected" "$VERIFY_RELEASE_TAG" " v1.2.3"
expect_fail "trailing whitespace is rejected" "$VERIFY_RELEASE_TAG" "v1.2.3 "
expect_fail "non-numeric component is rejected" "$VERIFY_RELEASE_TAG" "v1.x.3"
expect_fail "missing v prefix is rejected" "$VERIFY_RELEASE_TAG" "$version"
expect_fail "missing patch is rejected" "$VERIFY_RELEASE_TAG" "v1.2"
expect_fail "extra version component is rejected" "$VERIFY_RELEASE_TAG" "v1.2.3.4"
expect_fail "suffix is rejected" "$VERIFY_RELEASE_TAG" "v1.2.3suffix"
expect_fail "leading zero major is rejected" "$VERIFY_RELEASE_TAG" "v01.2.3"
expect_fail "leading zero minor is rejected" "$VERIFY_RELEASE_TAG" "v1.02.3"
expect_fail "leading zero patch is rejected" "$VERIFY_RELEASE_TAG" "v1.2.03"
expect_fail "prerelease tag is rejected" "$VERIFY_RELEASE_TAG" "v1.2.3-rc.1"
expect_fail "build metadata is rejected" "$VERIFY_RELEASE_TAG" "v1.2.3+build.1"
expect_fail "valid tag with mismatched lockstep version is rejected" "$VERIFY_RELEASE_TAG" "v$mismatching_version"

if [ "$failures" -ne 0 ]; then
    printf '%s release-tag verification test(s) failed.\n' "$failures" >&2
    exit 1
fi

printf 'PASS release-tag verification tests.\n'
