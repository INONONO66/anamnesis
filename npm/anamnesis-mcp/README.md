# anamnesis-mcp

MCP **stdio** server (and one-shot CLI) for [anamnesis](../anamnesis) — associative
cognitive memory for LLM agents.

## Tools

| Tool | When the agent should call it |
|------|-------------------------------|
| `recall` | **Before answering** — surfaces prior decisions/lessons. Reading auto-reinforces what it returns. |
| `remember` | **After any decision or lesson worth keeping** — stores one distilled insight. |
| `ingest_conversation` | Hand off a full transcript (ordered turns) for the windowing recipe. |

## Install (Claude Desktop)

```jsonc
// claude_desktop_config.json
{
  "mcpServers": {
    "anamnesis": {
      "command": "npx",
      "args": ["-y", "anamnesis-mcp", "serve"],
      "env": { "ANAMNESIS_DB": "/Users/you/.anamnesis/memory.db" }
    }
  }
}
```

First run downloads the embedding model (~400 MB) to a per-user cache. Pre-warm
interactively with `npx anamnesis-mcp prewarm`.

The npm package is a small launcher. During install it downloads the matching
native `anamnesis-mcp` binary from the GitHub Release for the package version.
Set `ANAMNESIS_MCP_BINARY` to use a locally built binary instead, or
`ANAMNESIS_MCP_SKIP_DOWNLOAD=1` when packaging without downloading.

## Use with other MCP clients

Any MCP-compatible client can launch this the same way — generic stdio config:

```json
{
  "mcpServers": {
    "anamnesis": {
      "command": "npx",
      "args": ["-p", "anamnesis-mcp", "anamnesis", "serve"],
      "env": {
        "ANAMNESIS_DB": "/absolute/path/to/memory.db",
        "ANAMNESIS_NAMESPACE": "default"
      }
    }
  }
}
```

Omit `ANAMNESIS_DB` and the server auto-scopes by walking up from the client's
launch **cwd** for a `.anamnesis/` directory (git-style), falling back to the
global `~/.anamnesis/memory.db` (see Configuration below).

**Cursor** (`.cursor/mcp.json` or `~/.cursor/mcp.json`, verified against
[Cursor's MCP docs](https://cursor.com/docs/mcp)): same `mcpServers` object, add
`"type": "stdio"`.

**Windsurf** (`~/.codeium/windsurf/mcp_config.json`, verified against
[Windsurf's Cascade MCP docs](https://docs.windsurf.com/plugins/cascade/mcp)):
same `mcpServers` / `command` / `args` / `env` shape, no `type` field.

**OpenCode** (`opencode.json`, verified against
[OpenCode's MCP servers docs](https://opencode.ai/docs/mcp-servers/)): config key
is `mcp` (not `mcpServers`), `"type": "local"` required, `command` is a single
array combining executable + args, env key is `environment`:

```json
{
  "mcp": {
    "anamnesis": {
      "type": "local",
      "command": ["npx", "-p", "anamnesis-mcp", "anamnesis", "serve"],
      "environment": { "ANAMNESIS_NAMESPACE": "default" }
    }
  }
}
```

Other clients (OpenClaw, Antigravity, …) are not verified against an official
config schema — use the generic stdio config above and consult the client's own
MCP docs for the exact wrapper key.

## Make memory reliable (the important part)

This is a general MCP server — there are no hooks, so the agent must *choose* to
call the tools. Paste this into your system/project instructions so it does:

> Before answering anything non-trivial, call `recall` with the key terms of the
> question. After you decide something, reach a conclusion, or learn a lesson the
> user would want kept, call `remember` with a one-sentence statement of it.

## Configuration

| Env var | Default | Meaning |
|---------|---------|---------|
| `ANAMNESIS_DB` | `<data_dir>/anamnesis/memory.db` | SQLite file for the default namespace. |
| `ANAMNESIS_NAMESPACE` | `default` | Namespace when a call omits one. |
| `ANAMNESIS_REINFORCE` | `true` | Auto-commit (reinforce) recalled results. Set `false` for receipt mode. |
| `FASTEMBED_CACHE_DIR` | `<cache_dir>/anamnesis/models` | Where the bge model is cached. |

Namespaces are isolated by separate SQLite files (`<db_dir>/<namespace>.db`).
