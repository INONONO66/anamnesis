# anamnesis — Claude Code plugin

This plugin wires anamnesis into Claude Code as **activation-gated recall injection**. On
`SessionStart` it seeds the turn with a few high-salience project memories; on every
`UserPromptSubmit` it runs a **read-only**, **top-`k`-capped** spreading-activation recall over
your prompt and injects the result **only when the top activation clears a need-odds threshold
`τ`** — an off-topic prompt injects nothing. Injection never reinforces anything: hook recall is
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
`anamnesis` (npm/cargo) — so on a given machine it may be **missing** or an **older build without the `hook` subcommand**. In that case `clap` exits `2` —
and a `UserPromptSubmit` hook that exits `2` **erases the user's prompt**. To make that impossible,
`hooks.json` points at `hooks/anamnesis-hook.sh`, a three-line shim that no-ops when the binary is
absent and **always exits 0**, so a wrong/old binary can never block or erase a prompt. All real
logic stays in the Rust binary (`crates/anamnesis-mcp/src/hook.rs`); the shim only neutralizes the
exit-2 footgun.

## Install (install-and-go — nothing else)

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

While hacking on anamnesis, drop a freshly-built binary into `plugin/bin/` so the wrappers use it
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

- **Plugin (recommended, install-and-go):** `ensure-anamnesis.sh` fetches the binary from the Release
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

Codex adopted Claude Code's hook contract, so the **same `anamnesis hook` subcommand and the
same guard wrapper drive Codex**. This repo ships a Codex plugin alongside the Claude Code one:
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
timeout = 10
```

> **Visibility caveat — the one real difference from Claude Code.** Claude Code injects
> `additionalContext` *silently*; Codex's TUI currently *renders* the injected recall block on
> screen as a `hook context:` message (open Codex issues #16933 / #16486 — Codex's behavior, not
> anamnesis's). Capture hooks (see below) fire silently in both; the extraction signal appears
> only in SessionStart context (visible in Codex's TUI). Everything else — the `τ` gate, read-only recall, agent-driven reinforcement,
> fail-open, the warm daemon — is identical, and the env knobs below apply unchanged. When Codex
> makes hook context silent upstream, anamnesis needs no change.

## Capture hooks (Stage 1 & 2)

Both Claude Code and Codex can automatically ingest your turn transcripts into anamnesis as raw episodic memories, then surface them back to you for distillation into project knowledge.

**Stage 1 (Capture):** The hooks fire on each turn-end event and stream the transcript to anamnesis as raw `Episodic` memories. Claude Code fires three events: `Stop` (mid-turn), `PreCompact` (post-turn), and `SessionEnd` (session close). Codex fires two: `Stop` and `PreCompact` (it lacks `SessionEnd` in its binary; its strict schema parser would reject it, as noted in #79). Each turn is idempotently deduped by a content hash, so overlap between multi-hook firing is harmless. Capture is a fire-and-forget, read-only pipe — it cannot fail and never blocks a prompt.

**Stage 2 (Extraction):** The daemon holds an un-extracted queue of ingested turns. When the queue crosses `ANAMNESIS_EXTRACT_THRESHOLD_N` (default 20), the next `SessionStart` hook injects a one-line nudge into the context, asking the agent to call the `extract_pending` MCP tool. That tool returns the raw turns and marks them extracted, so the agent can distill them into reasoning or project lessons using `relate` and `remember`. Extraction is agent-driven and best-effort — the nudge is advisory only, and there is no guarantee the agent will call the tool or that extraction will be immediate.

Enable or disable capture entirely with `ANAMNESIS_CAPTURE_ENABLED` (default `true`).
## R2 shadow extraction (opt-in)

R2 can send a bounded batch of captured raw turns to exactly one configured external extractor
only with the explicit opt-in `ANAMNESIS_EXTRACT_MODE=shadow`; its default is `off`. `auto`,
boolean-like, and other unrecognized values also degrade to `off`. `ANAMNESIS_EXTRACT_CMD`
configures that one provider command (default argv `claude -p`), parsed into a program plus
arguments and executed directly, **never through a shell and with no fallback command**. Stage-1
raw capture remains in the graph as `Episodic` memories. Provider stdin/the raw source batch,
raw stdout/stderr, and the raw command are transient and are not persisted or logged by R2
policy or error records. The policy side schema persists only profile hash/components, run and
failure scalars, validated candidates/relations, source identity/hash ledger, and audit labels.
R2 performs no automatic pruning or cleanup; those rows persist until an operator takes a
database lifecycle action. R2 stages candidates only for audit, does not change the graph, and
keeps candidates out of recall until R3. See
[operations](../docs/06-operations/operations.md#r2-shadow-extraction-opt-in) for batching and
audit details.


## Configuration (environment variables)

All knobs are read from the environment at hook time; the defaults are calibrated priors, not
laws (ADR-0010).

| Var | Meaning | Default |
|--|--|--|
| `ANAMNESIS_HOOK_THRESHOLD` | `τ` — need-odds injection gate (top-score floor) for `UserPromptSubmit`. | `13.0` |
| `ANAMNESIS_HOOK_COSINE_GATE` | Minimum query-embedding cosine for `UserPromptSubmit` injection after scope/type filters. | `0.86` |
| `ANAMNESIS_HOOK_SEED_COSINE_GATE` | Minimum query-embedding cosine for `SessionStart` seed injection after scope/type filters. | `0.80` |
| `ANAMNESIS_HOOK_CONTEXT_TURNS` | Recent transcript turns folded into the `UserPromptSubmit` recall query. | `3` |
| `ANAMNESIS_HOOK_TOPK` | `k` — cap on injected per-turn memories. | `3` |
| `ANAMNESIS_HOOK_SEED_K` | `SessionStart` seed size. | `5` |
| `ANAMNESIS_HOOK_TIMEOUT_MS` | Per-hook fail-open timeout (ms); on elapse, inject nothing. | `1500` |
| `ANAMNESIS_CAPTURE_ENABLED` | Enable/disable capture hooks (Stage 1 & 2) entirely. | `true` |
| `ANAMNESIS_EXTRACT_THRESHOLD_N` | Queue size threshold; when crossed, `SessionStart` injects extraction nudge to call `extract_pending`. | `20` |
| `ANAMNESIS_EXTRACT_MODE` | R2 extraction mode: exact `shadow` permits raw captured content to be sent to the configured external extractor; `off` (and invalid values, including `auto`) disables it. | `off` |
| `ANAMNESIS_EXTRACT_CMD` | Extractor command argv. Defaults to `claude -p`; parsed as argv and executed without a shell. | `claude -p` |

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

The `hooks.json` `timeout` (5–10 s) is only a backstop; the real fail-open bound is
`ANAMNESIS_HOOK_TIMEOUT_MS` (default 1500 ms), kept well under it so a hung daemon can never
stall a prompt.

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

Plugin reactivation is blocked until the operational evidence gate is complete. Follow the
[recall telemetry rollout gate](../docs/06-operations/operations.md#recall-telemetry-rollout-gate);
this document does not claim that live deployment observations have been collected.

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
