#!/bin/sh
# Test fixture intentionally uses a shebang and is intended to be executable.
# Invocation: fake-extractor.sh <valid|nonzero|large-stdout|large-stderr|timeout> [pidfile]

set -eu

mode=${1:?missing mode}

case "$mode" in
    valid)
        IFS= read -r input
        printf '{"received":%s}\n' "$input"
        ;;
    nonzero)
        printf '%s' 'secret-stderr-marker-do-not-log' >&2
        exit 7
        ;;
    large-stdout)
        dd if=/dev/zero bs=1048577 count=1 2>/dev/null | tr '\000' x
        ;;
    large-stderr)
        dd if=/dev/zero bs=1048577 count=1 2>/dev/null | tr '\000' x >&2
        ;;
    timeout)
        pidfile=${2:?missing pidfile}
        (
            trap '' TERM
            while :; do sleep 1; done
        ) &
        printf '%s\n%s\n' "$$" "$!" > "$pidfile"
        while :; do sleep 1; done
        ;;
    *)
        printf '%s\n' "unknown mode: $mode" >&2
        exit 64
        ;;
esac
