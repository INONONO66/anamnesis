#!/usr/bin/env sh
# anamnesis hook guard — shared by the Claude Code and Codex plugins.
#
# Hooks invoke `anamnesis hook <event>`, but the binary is installed
# out-of-band (`cargo install`, later npm) and may be MISSING, an OLD build
# without the `hook` subcommand, or simply off the hook's PATH (GUI and Codex
# launches often have a minimal PATH). clap then exits 2 — and a UserPromptSubmit
# hook that exits 2 ERASES the user's prompt. This shim neutralizes all of that:
# it resolves the binary (bundled next to the plugin, then PATH, then ~/.cargo/bin),
# no-ops if absent, and ALWAYS exits 0 so a wrong/old/missing binary can never
# block or erase a prompt. The
# real logic (parse, gated read-only recall, fail-open) lives in the Rust binary
# (`crates/anamnesis-mcp/src/hook.rs`); this stays a tiny safety shim.
#
# Usage (from hooks.json / codex-hooks.json): anamnesis-hook.sh <event>
#   e.g. user-prompt | session-start. stdin/stdout pass through unchanged.

# Resolve the binary: bundled next to this script (self-contained plugin) first,
# then PATH (npm/cargo), then ~/.cargo/bin.
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN="$HERE/../bin/anamnesis"
[ -x "$BIN" ] || BIN=$(command -v anamnesis 2>/dev/null) || BIN=
[ -n "$BIN" ] || BIN="${HOME}/.cargo/bin/anamnesis"
[ -x "$BIN" ] || exit 0
"$BIN" hook "$@" 2>/dev/null
exit 0
