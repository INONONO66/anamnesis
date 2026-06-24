#!/usr/bin/env sh
# anamnesis hook guard — shared by the Claude Code and Codex plugins.
#
# Hooks invoke `anamnesis hook <event>`, but the binary is installed
# out-of-band (`cargo install`, later npm) and may be MISSING, an OLD build
# without the `hook` subcommand, or simply off the hook's PATH (GUI and Codex
# launches often have a minimal PATH). clap then exits 2 — and a UserPromptSubmit
# hook that exits 2 ERASES the user's prompt. This shim neutralizes all of that:
# it resolves the binary (bundled/fetched-on-first-use, then PATH, then ~/.cargo/bin),
# no-ops if absent, and ALWAYS exits 0 so a wrong/old/missing binary can never
# block or erase a prompt. The
# real logic (parse, gated read-only recall, fail-open) lives in the Rust binary
# (`crates/anamnesis-mcp/src/hook.rs`); this stays a tiny safety shim.
#
# Usage (from hooks.json / codex-hooks.json): anamnesis-hook.sh <event>
#   e.g. user-prompt | session-start. stdin/stdout pass through unchanged.

# Resolve the binary: bundled/fetched next to the plugin first, then PATH, then
# ~/.cargo/bin. The binary is fetched on first use (install-and-go), but that
# ~24MB download must NEVER block a hook — SessionStart's timeout is short and a
# UserPromptSubmit hook must stay instant. So SessionStart kicks the fetch off in
# the BACKGROUND (detached via nohup, so it survives this hook returning) to get
# it in flight before the MCP server's ~30s startup window; both hooks then USE
# the binary only if it is ALREADY present and no-op otherwise — neither ever
# waits on the download. (The MCP server's own launcher reuses this same in-flight
# fetch, so startup does not race a second download.)
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "$1" = "session-start" ] && [ ! -x "$HERE/../bin/anamnesis" ]; then
  nohup "$HERE/../bin/ensure-anamnesis.sh" >/dev/null 2>&1 &
fi
BIN="$HERE/../bin/anamnesis"
[ -x "$BIN" ] || BIN=
[ -n "$BIN" ] || BIN=$(command -v anamnesis 2>/dev/null) || BIN=
[ -n "$BIN" ] || BIN="${HOME}/.cargo/bin/anamnesis"
[ -x "$BIN" ] || exit 0
"$BIN" hook "$@" 2>/dev/null
exit 0
