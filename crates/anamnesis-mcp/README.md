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
| `get` / `list` | Read one node, or list nodes by salience/type/tag/scope/metadata filter, as JSON. Both now include per-memory **provenance** — `peer_id`, `session_id`, `scope`, `confidence` (projected from `Origin`) — so a consumer can see which agent/session/scope produced each memory. This is the attribution foundation for multi-agent/team memory; multi-writer merge and trust-weighting remain roadmap. |
| `update` / `forget` / `supersede` | Edit a node's content, soft/hard-delete it, or mark one node as superseding another. |

## Local dashboard

`anamnesis dashboard` serves a **read-only** local web UI for exploring your
memory graph as an interactive **3D force-directed galaxy** (vendored
`3d-force-graph` + three.js, offline/no-CDN) — a thin daemon client, never
opening the DB directly. Nodes are colored by knowledge type and sized by
salience, with `UnrealBloomPass` bloom glow on the brightest ones. Retrieval
seeds a starting neighborhood; **search-to-focus** re-centers the camera on a
match, **click-to-expand** pulls in a node's k-hop neighbors on demand, a
**detail panel** shows a selected node's content, provenance, and validity
(with a `forget` action), and a category **filter sidebar** toggles knowledge
types on/off. Binds `127.0.0.1:<port>` only (local, **no auth**); prints the
URL on startup and runs until interrupted. `/api/graph` nodes now carry
`cluster` (Leiden community id) and `doi` (degree-of-interest) computed
server-side; the dashboard has a **Type/Cluster color toggle** (color nodes by
community) and a **Focus mode** (fades low-DOI context, highlights the
relevant core). The scene renders as a cinematic deep-space **galaxy** (nebula
haze + starfield + overview HUD) that stays dense-but-legible at hundreds of
nodes; **labels appear only as you zoom into a cluster** (not all at once), and
the render loop pauses when idle so a large graph stays smooth.

```bash
npx -p anamnesis-mcp anamnesis dashboard [--port N] [--namespace ns]
# or, from a checkout:
cargo run -p anamnesis-mcp -- dashboard
```

`--port` defaults to `0` (pick a free port); `--namespace` defaults to the
configured namespace (overridable per-request via `?namespace=`).

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

Full worked examples for each client (plus a pointer for unverified clients like
OpenClaw/Antigravity) live in [`plugin/README.md`](../../plugin/README.md#use-with-other-mcp-clients).

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
