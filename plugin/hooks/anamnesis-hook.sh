#!/usr/bin/env sh
# anamnesis Claude Code hook guard.
#
# Why this wrapper exists: the hooks invoke `anamnesis-mcp hook <event>`, but the
# binary is installed out-of-band (`cargo install`) and may be MISSING or an OLD
# build without the `hook` subcommand. In that case clap exits 2 — and a
# `UserPromptSubmit` hook that exits 2 ERASES the user's prompt. This guard
# neutralizes that: it no-ops when the binary is absent and forces exit 0 so a
# non-zero exit from a wrong/old binary can never block or erase a prompt. The
# real hook logic (parse, gated read-only recall, fail-open) lives in the Rust
# binary (`crates/anamnesis-mcp/src/hook.rs`); this stays a 3-line safety shim.
#
# Usage (from hooks.json): anamnesis-hook.sh <event>   e.g. user-prompt | session-start
# stdin (the Claude Code hook JSON) and stdout (the hook output JSON) pass through.

command -v anamnesis-mcp >/dev/null 2>&1 || exit 0
anamnesis-mcp hook "$@" 2>/dev/null
exit 0
