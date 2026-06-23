#!/usr/bin/env sh
# anamnesis MCP server launcher for the plugin's bundled `mcpServers` entry.
#
# Resolves the binary in order: bundled/fetched next to this script (via
# `ensure-anamnesis.sh`, which downloads it from the GitHub Release on first use
# so installing the plugin is enough), then PATH (`npm i -g anamnesis-mcp` /
# `cargo install`), then ~/.cargo/bin. `exec` replaces this shell with the binary
# so the MCP server's stdio (JSON-RPC) passes straight through untouched.
#
# MCP startup is not prompt-blocking, so a one-time first-run download here is
# acceptable; the hooks (which must never block a prompt) only USE the binary.
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN=$("$HERE/ensure-anamnesis.sh" 2>/dev/null) || BIN=
[ -n "$BIN" ] || BIN=$(command -v anamnesis 2>/dev/null) || BIN=
[ -n "$BIN" ] || BIN="${HOME}/.cargo/bin/anamnesis"
exec "$BIN" serve
