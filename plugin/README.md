# anamnesis agent plugin

This plugin wires anamnesis into Claude Code as **activation-gated recall injection**. On
`SessionStart` it seeds the turn with a few high-salience project memories; on every
`UserPromptSubmit` it runs the same **read-only**, **top-`k`-capped** canonical reranked-recall
pipeline used by the MCP tool and direct `Memory` consumers. It injects the result **only when the selected
top node's cognitive activation clears a need-odds threshold `τ`** — an off-topic prompt injects
nothing. Injection never reinforces anything: hook recall is
strictly read-only, so it cannot drive a recommender-style feedback loop. **Reinforcement is
agent-driven** — the injected block carries a one-line nudge asking the agent to call the
`recall`/`relate` MCP tools when it actually uses a memory, and that deliberate call is the only
"used" signal that lifts a memory's base-level activation. Both hooks are **fail-open**: any
error, timeout, or below-`τ` result injects nothing and exits 0, so a prompt is never blocked or
erased. See `docs/adr/0011-activation-gated-triggering.md` for the rationale.

## How it works

Each hook runs `hooks/anamnesis-hook.sh <event>` (a guard wrapper), which calls
`anamnesis hook <event>`. The binary reads the Claude Code hook JSON on stdin, talks to the
warm shared anamnesis daemon over a Unix socket (auto-spawned on first call, reused thereafter),
and prints the hook output JSON on stdout:

| Event | Subcommand | Behavior |
|--|--|--|
| `SessionStart` | `anamnesis hook session-start` | Ungated read-only recall seeded by the project cue (cwd basename), up to `ANAMNESIS_HOOK_SEED_K` memories. |
| `UserPromptSubmit` | `anamnesis hook user-prompt` | Activation-**gated** read-only recall on the prompt (`τ` floor, top-`k` cap); below `τ` injects nothing. |

### The guard wrapper (why hooks don't call the binary directly)

`anamnesis` is resolved at run time — the binary bundled in the plugin first, else a PATH
`anamnesis` (npm/cargo) — so on a given machine it may be **missing** or an **older build without the `hook` subcommand**. A non-zero hook exit can interrupt host prompt handling, so
`hooks.json` points at `hooks/anamnesis-hook.sh`, a three-line shim that no-ops when the binary is
absent and **always exits 0**. All product
logic stays in the Rust binary (`crates/anamnesis-mcp/src/hook.rs`); the shim only neutralizes the
process-status boundary.

## Install

The plugin is **self-contained for everyone**: it declares *both* the hooks *and* the agent MCP
server, and its wrappers **fetch the matching `anamnesis` binary from the GitHub Release on first
use** (`bin/ensure-anamnesis.sh`). So a plain plugin install + reload gives you everything —
proactive recall (hooks) and the agent MCP server — with **no separate `claude mcp add`,
no `npm`/`cargo`, no global binary**. See the root
[authoritative MCP tool inventory](../README.md#mcp-tool-inventory).

```
/plugin marketplace add INONONO66/anamnesis     # git repo (or `./plugin` for a local checkout)
/plugin install anamnesis@anamnesis-plugins
/reload-plugins
```

On first use the SessionStart hook kicks off a **background** fetch of the platform binary into the
plugin's cached `bin/`, and the MCP server's launcher reuses that same in-flight download (rather
than racing a second one) — a one-time, few-second fetch; later sessions are instant. This needs a
**published GitHub Release `v<plugin-version>`** carrying the `anamnesis-<platform>` assets (built by
the release CI). The `hook` subcommand requires the binary **`>= 0.8.0`**.

> **First-run note (slow networks).** Claude Code's MCP startup timeout is **30 s**. On a slow link
> the ~24 MB binary may not land within that window, so the **first** session can show a one-time
> `MCP client for anamnesis failed to start` warning. The download still completes in the background —
> just `/reload-plugins` (or open the next session) and the MCP server attaches instantly. To avoid
> the warning entirely, raise the limit once with `MCP_TIMEOUT=120000 claude`. The hooks (proactive
> recall) are unaffected — they never block on the fetch.

(`./plugin` is the dir with `.claude-plugin/marketplace.json`; `anamnesis-plugins` is the marketplace
`name`. `source: "./"` resolves against a local-dir or git marketplace.)

### Local development — pre-bundle to skip the fetch

For local Anamnesis development, place a freshly built binary in `plugin/bin/` so the wrappers use it
directly (no download — `ensure-anamnesis.sh` sees it present):

```sh
cargo build --release -p anamnesis-mcp && cp target/release/anamnesis plugin/bin/
```

`plugin/bin/anamnesis` is **gitignored** (never committed); the shipped plugin contains only the
wrappers + `VERSION` and fetches the binary on first use.

### Binary resolution & PATH fallback

The wrappers resolve the binary in order: **bundled/fetched** (`plugin/bin/anamnesis`, next to the
wrapper) → **PATH** `anamnesis` (`npm install -g anamnesis-mcp` / `cargo install`) → `~/.cargo/bin`.
The first-use fetch makes PATH unnecessary for most users; PATH only matters if the fetch can't run
(offline / unsupported platform) and you installed the binary yourself. If recall silently injects
nothing and no binary was fetched, check `which anamnesis` from the shell Claude Code launched from.

### Distribution channels

- **Plugin installation:** `ensure-anamnesis.sh` fetches the binary from the Release
  on first use, so `/plugin install` is all an end user needs.
- **npm (`anamnesis-mcp`):** a thin wrapper whose `postinstall` downloads the same release binary and
  exposes the `anamnesis` command — for the CLI/MCP without the plugin.

Both pull the same `anamnesis-<platform>` asset from Release `v<version>`; the binary is never
committed to git. The guard makes a missing/old binary *safe* (no-op), so a version mismatch never
breaks a prompt. Requires the binary **`>= 0.8.0`**.

### Versioning

The plugin's `version` (in `.claude-plugin/plugin.json`) **tracks the `anamnesis-mcp` crate
version** — they are released together. Claude Code uses this version as the cache key to detect
plugin updates, so it is bumped whenever the crate is.

## Codex (OpenAI Codex CLI)

Codex exposes a compatible hook event surface, so the **same `anamnesis hook` subcommand and the
same guard wrapper drive Codex**. This repository ships a Codex plugin alongside the Claude Code one:
`plugin/.codex-plugin/plugin.json` + `plugin/hooks/codex-hooks.json`, and a Codex marketplace
manifest at `.agents/plugins/marketplace.json` (repo root) pointing at `./plugin`.

Install (uses the bundled / PATH `anamnesis` binary, exactly like the Claude Code plugin):

```sh
# add this repo as a local marketplace (or `INONONO66/anamnesis` once pushed), then install
codex plugin marketplace add /path/to/anamnesis
codex plugin add anamnesis@anamnesis-plugins
# restart Codex (or start a new session) to apply the hooks
```

Codex copies the plugin into its own cache (`~/.codex/plugins/cache/...`), so — like Claude Code —
it keeps working after you switch git branches.

Prefer no marketplace? Wire it manually in **user-level** `~/.codex/config.toml` (repo-local
`.codex/config.toml` hooks do not fire in interactive sessions):

```toml
[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "anamnesis hook user-prompt"
timeout = 5

[[hooks.SessionStart.hooks]]
type = "command"
command = "anamnesis hook session-start"
timeout = 5
```

> **Visibility note.** Claude Code injects
> `additionalContext` *silently*; Codex's TUI currently *renders* the injected recall block on
> screen as a `hook context:` message. Capture hooks (see below) fire silently in both; the extraction signal appears
> only in SessionStart context (visible in Codex's TUI). Everything else — the `τ` gate, read-only recall, agent-driven reinforcement,
> fail-open behavior, and the warm daemon — uses the same Anamnesis path, and the environment knobs below apply unchanged.

## Capture hooks (Stage 1 & 2)

Both Claude Code and Codex can automatically ingest your turn transcripts into anamnesis as raw episodic memories, then surface them back to you for distillation into project knowledge.

**Stage 1 (Capture):** The hooks attempt to persist each available turn as raw `Episodic` memory. Claude Code supplies `Stop`, `PreCompact`, and `SessionEnd`; the bundled Codex manifest supplies `Stop` and `PreCompact`. Successfully persisted turns are idempotently deduplicated by a content hash, so overlap between events does not create duplicate memories. Capture is fail-open: a daemon, parsing, or timeout failure is reported through diagnostics and does not block the host prompt, but that event's unpersisted turn may be absent from memory.

**Stage 2 (Extraction):** The daemon holds an un-extracted queue of ingested turns. When the queue crosses `ANAMNESIS_EXTRACT_THRESHOLD_N` (default 20), the next `SessionStart` hook injects a one-line nudge into the context, asking the agent to call the `extract_pending` MCP tool. That tool returns the raw turns and marks them extracted, so the agent can distill them into reasoning or project lessons using `relate` and `remember`. Extraction is agent-driven and best-effort — the nudge is advisory only, and there is no guarantee the agent will call the tool or that extraction will be immediate.

Enable or disable capture entirely with `ANAMNESIS_CAPTURE_ENABLED` (default `true`).

## Shadow extraction (opt-in)

The exact opt-in `ANAMNESIS_EXTRACT_MODE=shadow` starts a detached extraction
worker after successful `PreCompact` or `SessionEnd` capture. The default is
`off`; unrecognized values also resolve to `off`. The worker drains bounded
source batches through one configured command. Its default is the local
`ollama run qwen3.6:35b-a3b --think=false` path with structured output; an
operator may replace it with `ANAMNESIS_EXTRACT_CMD`. Exactly one command is
parsed as an argument vector and executed without a shell.

Raw captured sources remain authoritative `Episodic` memories. Provider input,
raw output, stderr, and the raw command are transient. Persisted policy records
contain a non-secret profile identity, run/failure scalars, validated
candidates and relations, source identity/hash references, and review labels.
Validated output is staged outside recall until a reviewer records support and
explicitly promotes a candidate or relation with `anamnesis extract`. See the
[operations guide](../docs/06-operations/operations.md) for worker, audit, and
promotion contracts.


## Configuration (environment variables)

All knobs are read from the environment at hook time; the defaults are calibrated priors, not
laws (ADR-0010).

| Var | Meaning | Default |
|--|--|--|
| `ANAMNESIS_HOOK_THRESHOLD` | `τ` — need-odds injection gate (top-score floor) for `UserPromptSubmit`. | `13.0` |
| `ANAMNESIS_HOOK_COSINE_GATE` | Minimum query-embedding cosine for `UserPromptSubmit` injection after scope/type filters. | `0.86` |
| `ANAMNESIS_HOOK_SEED_COSINE_GATE` | Minimum query-embedding cosine for `SessionStart` seed injection after scope/type filters. | `0.80` |
| `ANAMNESIS_HOOK_CONTEXT_TURNS` | Recent transcript turns folded into the `UserPromptSubmit` recall query. | `3` |
| `ANAMNESIS_HOOK_TOPK` | `k` — cap on injected per-turn memories. | `20` |
| `ANAMNESIS_HOOK_SEED_K` | `SessionStart` seed size. | `5` |
| `ANAMNESIS_HOOK_TIMEOUT_MS` | Per-hook fail-open timeout (ms); on elapse, inject nothing. | `4000` |
| `ANAMNESIS_CAPTURE_ENABLED` | Enable/disable capture hooks (Stage 1 & 2) entirely. | `true` |
| `ANAMNESIS_EXTRACT_THRESHOLD_N` | Queue size threshold; when crossed, `SessionStart` injects extraction nudge to call `extract_pending`. | `20` |
| `ANAMNESIS_EXTRACT_MODE` | Exact `shadow` enables reviewed extraction; `off` and unrecognized values disable it. The configured command receives raw captured content. | `off` |
| `ANAMNESIS_EXTRACT_CMD` | Extractor command argv, parsed and executed without a shell. | `ollama run qwen3.6:35b-a3b --think=false --format <schema>` |

> **`τ` is on the raw activation scale, not 0..1.** The gate compares the **top recall
> score** — the unnormalized ACT-R activation of the strongest hit — against `τ`. On a typical
> graph that score lands around **~8–16**, so `τ` must be set on that scale; a sub-1 value
> silently disables the gate and injects on every prompt. `13.0` was calibrated against a real
> 240-node graph (relevant prompts ~14–16, off-topic ~8–10). Because activation magnitude scales
> with graph density and recency, **recalibrate `τ` per-graph**: pick a relevant and an off-topic
> prompt, run `anamnesis recall <prompt>` to read the top score for each, and set `τ` between
> the two bands. Raise it toward precision (suppress more), lower it toward recall (inject more).

The cosine gates are 0..1 embedding-similarity floors layered on top of `τ`. Lower
`ANAMNESIS_HOOK_COSINE_GATE` if prompt recall is too quiet; raise it if content-free project
cues inject memories. `ANAMNESIS_HOOK_CONTEXT_TURNS` lets the hook include recent transcript
context so short follow-up prompts can still match relevant memories.

The general anamnesis knobs apply to the hook too, since it talks to the same daemon:

| Var | Meaning | Default |
|--|--|--|
| `ANAMNESIS_DB` | Path to the memory DB (selects which daemon/graph the hook reads). | `<data_dir>/anamnesis/memory.db` |
| `ANAMNESIS_NAMESPACE` | Namespace scoping recall. | `default` |
| `ANAMNESIS_DAEMON_GRACE_SECS` | How long the shared daemon stays warm after the last client disconnects. | `30` |
| `ANAMNESIS_EMBED_MODEL` | FastEmbed model for new embeddings. Supported: `multilingual-e5-small`, `multilingual-e5-base`, `multilingual-e5-large`, `bge-base-en-v1.5`. Use `bge-base-en-v1.5` for existing 768-d databases. | `multilingual-e5-small` |
| `ANAMNESIS_RERANK_MODEL` | Local cross-encoder used by the canonical reranked-recall path. | `BAAI/bge-reranker-base` |

The recall-hook `timeout` is 5 seconds as an outer backstop. The Rust hook's
`ANAMNESIS_HOOK_TIMEOUT_MS` default is 4 seconds, so it remains the first
fail-open boundary. The default production path searches at 20, preselects and
reranks 50 source-aware evidence documents, and delivers at most 20. Versioned
measurements and their environment are recorded in the
[quality-gate records](../docs/07-quality-gates/calibration-records.md).

## Use with other MCP clients

The `hook` subcommand (proactive recall) is Claude-Code/Codex-specific, but the underlying
`anamnesis serve` **stdio MCP server** exposes the root
[authoritative MCP tool inventory](../README.md#mcp-tool-inventory) to any MCP-compatible
client. No plugin, daemon socket, or hooks are required.

### Generic (any MCP-compatible client)

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

`ANAMNESIS_DB` pins the SQLite file explicitly; omit it and the server auto-scopes
by walking up from the client's launch **cwd** for a `.anamnesis/` directory (git-style),
falling back to the global `~/.anamnesis/memory.db` — see
[`crates/anamnesis-mcp/README.md`](../crates/anamnesis-mcp/README.md#configuration) for the
full env-var table and scope-resolution rules. Adapt the `mcpServers` wrapper key to whatever
your client expects (see below); the `command`/`args`/`env` triple stays the same everywhere.

### Cursor — `.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global)

Verified against [Cursor's MCP docs](https://cursor.com/docs/mcp): stdio servers take
`type`/`command`/`args`/`env` under the same `mcpServers` key as above.

```json
{
  "mcpServers": {
    "anamnesis": {
      "type": "stdio",
      "command": "npx",
      "args": ["-p", "anamnesis-mcp", "anamnesis", "serve"],
      "env": { "ANAMNESIS_NAMESPACE": "default" }
    }
  }
}
```

### Windsurf — `~/.codeium/windsurf/mcp_config.json`

Verified against [Windsurf's Cascade MCP docs](https://docs.windsurf.com/plugins/cascade/mcp):
same `mcpServers` / `command` / `args` / `env` shape, no `type` field, global-only (no
per-project config).

```json
{
  "mcpServers": {
    "anamnesis": {
      "command": "npx",
      "args": ["-p", "anamnesis-mcp", "anamnesis", "serve"],
      "env": { "ANAMNESIS_NAMESPACE": "default" }
    }
  }
}
```

### OpenCode — `opencode.json` (project) or `~/.config/opencode/opencode.json` (global)

Verified against [OpenCode's MCP servers docs](https://opencode.ai/docs/mcp-servers/):
the config key is `mcp` (not `mcpServers`), each entry needs `"type": "local"`, and
`command` is a **single array** combining the executable and its args (the env key is
`environment`, not `env`).

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "anamnesis": {
      "type": "local",
      "command": ["npx", "-p", "anamnesis-mcp", "anamnesis", "serve"],
      "enabled": true,
      "environment": { "ANAMNESIS_NAMESPACE": "default" }
    }
  }
}
```

### Other clients (OpenClaw, Antigravity, …)

Not verified against an official config schema at time of writing — use the
generic stdio config above and consult the client's own MCP documentation for the
exact wrapper key/field names (most MCP clients use a `command`/`args`/`env` triple
under some `mcpServers`-style object).

## Recall telemetry and rollout gate

Hook and tool recall records privacy-minimized eligibility telemetry only: it never stores the raw
query, transcript, or rendered context. The row includes metadata such as event kind, provenance,
scope, gate outcomes, filtered top score/cosine, and `query_chars`, and retains only the newest
**10,000** rows. `anamnesis stats --recall` reports **injection eligibility, not delivery or
quality**. A newer telemetry side-schema, or a telemetry policy open/write/query failure, disables
or degrades telemetry only; core recall and fail-open hook prompt delivery continue.

Use the [operations guide](../docs/06-operations/operations.md) to interpret
these counters and define deployment-specific rollout gates; eligibility alone
does not establish delivery or answer quality.

## Verify it works

Pipe a real `UserPromptSubmit` payload into the hook and confirm you get valid hook JSON out
(an empty stdout is the correct below-`τ` / no-memory no-op — it still exits 0):

```sh
echo '{"hook_event_name":"UserPromptSubmit","prompt":"what did we decide about the recall gate?","cwd":"'"$PWD"'"}' \
  | anamnesis hook user-prompt
```

A clearly off-topic prompt should inject nothing:

```sh
echo '{"hook_event_name":"UserPromptSubmit","prompt":"zxqv wrrn plugh","cwd":"'"$PWD"'"}' \
  | anamnesis hook user-prompt
# (no output, exit 0)
```

## Local dashboard

`anamnesis dashboard` serves a **read-only** local web UI to browse memories
and view graph stats — a thin client of the shared daemon, never opening the
DB directly. Binds `127.0.0.1:<port>` only (local, **no auth**); prints the
URL on startup and runs until interrupted.

```bash
npx -p anamnesis-mcp anamnesis dashboard [--port N] [--namespace ns]
# or, from a checkout:
cargo run -p anamnesis-mcp -- dashboard
```

`--port` defaults to `0` (pick a free port); `--namespace` defaults to the
configured namespace.
