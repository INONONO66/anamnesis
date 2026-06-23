#!/usr/bin/env sh
# anamnesis MCP server launcher for the plugin's bundled `mcpServers` entry.
#
# Resolves the binary in order: the one bundled next to this script (the
# self-contained plugin), then PATH (`npm i -g anamnesis-mcp` / `cargo install`),
# then ~/.cargo/bin. `exec` replaces this shell with the binary so the MCP
# server's stdio (JSON-RPC) passes straight through untouched.
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN="$HERE/anamnesis-mcp"
[ -x "$BIN" ] || BIN=$(command -v anamnesis-mcp 2>/dev/null) || BIN=
[ -n "$BIN" ] || BIN="${HOME}/.cargo/bin/anamnesis-mcp"
exec "$BIN" serve
