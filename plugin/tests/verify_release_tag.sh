#!/usr/bin/env sh
# Verify that a stable release tag exactly matches every lockstep version surface.
# Requires only POSIX sh and Python 3.

set -u

if [ "$#" -ne 1 ]; then
    printf 'usage: %s vMAJOR.MINOR.PATCH (for example, v0.19.0)\n' "$0" >&2
    exit 2
fi

HERE=$(CDPATH= cd "$(dirname "$0")" && pwd)

version=$(python3 - "$1" <<'PY'
import re
import sys

match = re.fullmatch(
    r"v((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))",
    sys.argv[1],
)
if not match:
    print(
        f"ERROR release tag must be a stable vMAJOR.MINOR.PATCH tag; got {sys.argv[1]!r}",
        file=sys.stderr,
    )
    sys.exit(2)
print(match.group(1))
PY
) || exit $?

exec "$HERE/verify_versions.sh" "$version"
