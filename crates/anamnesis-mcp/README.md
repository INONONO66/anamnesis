# anamnesis-mcp

MCP **stdio** server (and one-shot CLI) for [anamnesis](../anamnesis) — associative
cognitive memory for LLM agents.

## Tools

| Tool | When the agent should call it |
|------|-------------------------------|
| `recall` | **Before answering** — surfaces prior decisions/lessons as a readable context block plus a compact `{node_id, score}` list. Reading auto-reinforces what it returns. |
| `remember` | **After any decision or lesson worth keeping** — stores one distilled insight. |
| `ingest_conversation` | Hand off a full transcript (ordered turns) for the windowing recipe. |
| `relate` | Link two recalled nodes with a typed reasoning relation (`causes`, `contradicts`, `supports`, `refutes`, `reason`, `rejected-alternative`, `belongs-to`, `related`, or `custom:<label>`). Pass `node_id`s from a prior `recall`. |

## CLI subcommands

`anamnesis-mcp <cmd>` also runs one-shot commands (cold model load) and exits:
`recall` / `remember` / `prewarm`, plus:

| Command | What it does |
|---------|--------------|
| `doctor` | Print a setup checklist — resolved DB path, lock availability, model cache dir, config. Does **not** load the embedding model. |
| `stats` | Open the registry and print graph health/size stats (`Memory::stats`) for the default namespace. Loads the model. |

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
| `ANAMNESIS_DB` | `~/.anamnesis/memory.db` | SQLite file for the default namespace. |
| `ANAMNESIS_NAMESPACE` | `default` | Namespace when a call omits one. |
| `ANAMNESIS_REINFORCE` | `true` | Auto-commit (reinforce) recalled results. Set `false` for receipt mode. |
| `FASTEMBED_CACHE_DIR` | `~/.anamnesis/models` | Where the bge model is cached (~400 MB). |

Everything anamnesis lives under `~/.anamnesis/` by default — the global memory DB
plus the model cache. **Scope is auto-selected**, the way git finds `.git`: walking
up from the launch directory, the nearest ancestor containing a `.anamnesis/`
directory wins (project scope → `<project>/.anamnesis/memory.db`); with none, the
global `~/.anamnesis/memory.db` is used. Opt a project in with `mkdir .anamnesis`
at its root.

Auto-detection needs the client to launch the server with the project as its
working directory (Claude Code / Cursor do; Claude Desktop has no project CWD, so
it is always global). `ANAMNESIS_DB` overrides everything; a `namespace` per call
isolates logically within one store (sibling `<db_dir>/<namespace>.db`).
