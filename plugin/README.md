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

Each hook spawns `anamnesis-mcp hook <event>`, which reads the Claude Code hook JSON on stdin,
talks to the warm shared anamnesis daemon over a Unix socket (auto-spawned on first call, reused
thereafter), and prints the hook output JSON on stdout:

| Event | Subcommand | Behavior |
|--|--|--|
| `SessionStart` | `anamnesis-mcp hook session-start` | Ungated read-only recall seeded by the project cue (cwd basename), up to `ANAMNESIS_HOOK_SEED_K` memories. |
| `UserPromptSubmit` | `anamnesis-mcp hook user-prompt` | Activation-**gated** read-only recall on the prompt (`τ` floor, top-`k` cap); below `τ` injects nothing. |

## Install

1. Install the binary so `anamnesis-mcp` is on your PATH:

   ```sh
   cargo install --path crates/anamnesis-mcp
   ```

2. Add this directory as a local marketplace, then install the plugin:

   ```
   /plugin marketplace add ./plugin
   /plugin install anamnesis@anamnesis-plugins
   ```

   `./plugin` is the path to the directory containing `.claude-plugin/marketplace.json`;
   `anamnesis-plugins` is the marketplace `name`, not a path.

   This local-directory flow works because the marketplace entry uses `source: "./"`, which
   resolves against the local marketplace root (the checked-out directory). The same relative
   `source` can fail if a marketplace is added by a direct **URL** to `marketplace.json`; for
   URL-based distribution, use a git/github `source` in the marketplace entry instead.

### PATH requirement

The hooks invoke the bare name `anamnesis-mcp`, so it must be on the **PATH Claude Code sees**.
GUI launches frequently do **not** include `~/.cargo/bin`. If the hook silently injects nothing,
the most likely cause is the binary not being found: confirm with `which anamnesis-mcp` from the
same shell Claude Code was launched from, and put `~/.cargo/bin` on that PATH (or symlink the
binary into a directory already on PATH).

### Versioning

The plugin's `version` (in `.claude-plugin/plugin.json`) **tracks the `anamnesis-mcp` crate
version** — they are released together. Claude Code uses this version as the cache key to detect
plugin updates, so it is bumped whenever the crate is.

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
> prompt, run `anamnesis-mcp recall <prompt>` to read the top score for each, and set `τ` between
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
  | anamnesis-mcp hook user-prompt
```

A clearly off-topic prompt should inject nothing:

```sh
echo '{"hook_event_name":"UserPromptSubmit","prompt":"zxqv wrrn plugh","cwd":"'"$PWD"'"}' \
  | anamnesis-mcp hook user-prompt
# (no output, exit 0)
```
