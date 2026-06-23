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

## Install

The plugin is **self-contained**: it bundles the `anamnesis` binary (`plugin/bin/`) and declares
*both* the hooks *and* the agent MCP server, so **one install + reload gives you everything** — no
separate `claude mcp add`, no global binary on PATH.

1. Put the binary in `plugin/bin/` (gitignored — never committed). Building from source:

   ```sh
   cargo build --release -p anamnesis-mcp && cp target/release/anamnesis plugin/bin/
   ```

   A published release fetches the matching prebuilt binary into `plugin/bin/` for you, so this
   step is only for building from source. If `plugin/bin/anamnesis` is absent, the guard and
   serve wrappers fall back to a PATH `anamnesis` (`npm install -g anamnesis-mcp` /
   `cargo install`) — the plugin still works, just not self-contained.

2. Add the marketplace, install, reload:

   ```
   /plugin marketplace add ./plugin
   /plugin install anamnesis@anamnesis-plugins
   /reload-plugins
   ```

   Installing copies the plugin — *including the bundled binary* — into Claude Code's cache; reload
   activates the hooks and the MCP server. You now have proactive recall injection (hooks) **and**
   the `recall`/`remember`/`relate`/`ingest_conversation`/`stats` tools (MCP), both backed by the one
   bundled binary. (`./plugin` is the dir containing `.claude-plugin/marketplace.json`;
   `anamnesis-plugins` is the marketplace `name`. The relative `source: "./"` resolves against a
   local-dir or git marketplace; a direct **URL** to `marketplace.json` would need a git `source`.)

### PATH (only the fallback)

When the plugin bundles `plugin/bin/anamnesis` (the default), **no PATH is needed** — the guard
and serve wrappers resolve the bundled binary by their own location. PATH matters only for the
**fallback** (no bundled binary): then `anamnesis` must be on the **PATH Claude Code sees**, and
GUI launches frequently do **not** include the npm-global bin (or `~/.cargo/bin`). If recall
silently injects nothing in that mode, confirm `which anamnesis` from the shell Claude Code was
launched from and put the install dir on that PATH (or symlink the binary onto it).

### Distribution

The binary ships via **npm**: `anamnesis-mcp` is a thin wrapper whose `postinstall` downloads the
matching prebuilt binary from the GitHub Release (`v<version>`), so `npm install -g anamnesis-mcp`
needs no Rust toolchain. The plugin stays tiny (hooks + guard) and runs whatever `anamnesis` is
on PATH — npm-global or `cargo install`, either works.

The binary and the plugin install separately (the plugin is **not fully self-contained**), but the
guard wrapper makes a missing/old binary *safe* (no-op), so a version mismatch never breaks a prompt.
The `hook` subcommand requires **`anamnesis >= 0.8.0`** (the first published release that has it).

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
# add this repo as a local marketplace (or `amsminn/anamnesis` once pushed), then install
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
> anamnesis's). Everything else — the `τ` gate, read-only recall, agent-driven reinforcement,
> fail-open, the warm daemon — is identical, and the env knobs below apply unchanged. When Codex
> makes hook context silent upstream, anamnesis needs no change.

## Configuration (environment variables)

All knobs are read from the environment at hook time; the defaults are calibrated priors, not
laws (ADR-0010).

| Var | Meaning | Default |
|--|--|--|
| `ANAMNESIS_HOOK_THRESHOLD` | `τ` — need-odds injection gate (top-score floor) for `UserPromptSubmit`. | `13.0` |
| `ANAMNESIS_HOOK_TOPK` | `k` — cap on injected per-turn memories. | `5` |
| `ANAMNESIS_HOOK_SEED_K` | `SessionStart` seed size. | `5` |
| `ANAMNESIS_HOOK_TIMEOUT_MS` | Per-hook fail-open timeout (ms); on elapse, inject nothing. | `1500` |

> **`τ` is on the raw activation scale, not 0..1.** The gate compares the **top recall
> score** — the unnormalized ACT-R activation of the strongest hit — against `τ`. On a typical
> graph that score lands around **~8–16**, so `τ` must be set on that scale; a sub-1 value
> silently disables the gate and injects on every prompt. `13.0` was calibrated against a real
> 240-node graph (relevant prompts ~14–16, off-topic ~8–10). Because activation magnitude scales
> with graph density and recency, **recalibrate `τ` per-graph**: pick a relevant and an off-topic
> prompt, run `anamnesis recall <prompt>` to read the top score for each, and set `τ` between
> the two bands. Raise it toward precision (suppress more), lower it toward recall (inject more).

The general anamnesis knobs apply to the hook too, since it talks to the same daemon:

| Var | Meaning | Default |
|--|--|--|
| `ANAMNESIS_DB` | Path to the memory DB (selects which daemon/graph the hook reads). | `<data_dir>/anamnesis/memory.db` |
| `ANAMNESIS_NAMESPACE` | Namespace scoping recall. | `default` |
| `ANAMNESIS_DAEMON_GRACE_SECS` | How long the shared daemon stays warm after the last client disconnects. | `30` |

The `hooks.json` `timeout` (5–10 s) is only a backstop; the real fail-open bound is
`ANAMNESIS_HOOK_TIMEOUT_MS` (default 1500 ms), kept well under it so a hung daemon can never
stall a prompt.

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
