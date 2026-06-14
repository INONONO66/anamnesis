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
